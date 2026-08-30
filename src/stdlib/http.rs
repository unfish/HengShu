use super::codec::{as_bytes, bytes, from_json, to_json};
use super::error;
use super::path::resolve;
use crate::contracts::{decimal_to_u32, decimal_to_usize, timeout_duration};
use crate::resource::{read_file_limited, read_to_end_limited};
use crate::security::PermissionSet;
use crate::value::{HttpBody, HttpResponseValue, StreamValue, Value};
use encoding_rs::Encoding;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use url::Url;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) fn response_attr(
    response: &HttpResponseValue,
    name: &str,
) -> Option<Result<Value, String>> {
    Some(match name {
        "状态码" => Ok(Value::Number(response.status.into())),
        "成功" => Ok(Value::Bool((200..300).contains(&response.status))),
        "响应头" => Ok(Value::record(
            response
                .headers
                .iter()
                .map(|(key, value)| (key.clone(), Value::Text(value.clone())))
                .collect(),
        )),
        "设置Cookie" => Ok(Value::list(
            response
                .set_cookies
                .iter()
                .cloned()
                .map(Value::Text)
                .collect(),
        )),
        "字节正文" => body_bytes(response).map(bytes),
        "文本正文" => body_bytes(response)
            .and_then(|body| decode_response_text(response, &body))
            .map(Value::Text),
        _ => return None,
    })
}

pub(super) fn response_method(
    response: &HttpResponseValue,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Option<Result<Value, String>> {
    Some(match name {
        "转JSON" | "JSON" => body_bytes(response).and_then(|body| {
            serde_json::from_slice(&body)
                .map(|value| from_json(&value))
                .map_err(|error_value| error(error_value.to_string()))
        }),
        "保存到" => {
            let path = resolve(base_dir, &args[0]);
            if let Err(permission_error) = permissions.check_file(&path) {
                return Some(Err(permission_error));
            }
            body_bytes(response).and_then(|body| {
                fs::write(path, body)
                    .map(|_| Value::Null)
                    .map_err(|error_value| error(error_value.to_string()))
            })
        }
        "转流" => {
            let size = match named
                .get("块大小")
                .or_else(|| args.first())
                .map(Value::as_decimal)
                .transpose()
            {
                Ok(Some(value)) => match decimal_to_usize(value, "HTTP 流块大小") {
                    Ok(size) => size.max(1),
                    Err(error_value) => return Some(Err(error_value)),
                },
                Ok(None) => 65_536,
                Err(error_value) => return Some(Err(error_value)),
            };
            if size > permissions.buffer_limit() {
                return Some(Err(error("HTTP 流块大小超过缓冲上限")));
            }
            Ok(Value::Stream(Rc::new(RefCell::new(StreamValue {
                source: crate::stream::reader_chunks(
                    take_body_reader(response),
                    size,
                    permissions.buffer_limit(),
                ),
                operations: Vec::new(),
            }))))
        }
        _ => return None,
    })
}

pub(super) fn http_request(
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
    execution_budget: Option<Duration>,
) -> Result<Value, String> {
    let method = args[0].to_string().to_uppercase();
    let mut url =
        Url::parse(&args[1].to_string()).map_err(|error_value| error(error_value.to_string()))?;
    if let Some(Value::Record(query)) = named.get("查询参数") {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query.borrow().iter() {
            pairs.append_pair(key, &value.to_string());
        }
    }
    let timeout = named
        .get("最长等待")
        .map(|value| timeout_duration(value, "网络请求最长等待"))
        .transpose()?
        .unwrap_or(DEFAULT_TIMEOUT);
    let max_redirects = named
        .get("最大重定向")
        .map(Value::as_decimal)
        .transpose()?
        .map(|value| decimal_to_u32(value, "最大重定向次数"))
        .transpose()?
        .unwrap_or(5);
    let proxy = named.get("代理").map(ToString::to_string);
    if let Some(proxy) = &proxy {
        permissions.check_network(proxy)?;
    }
    let deadline = execution_budget.and_then(|budget| Instant::now().checked_add(budget));
    let retries = named
        .get("重试次数")
        .map(Value::as_decimal)
        .transpose()?
        .map(|value| decimal_to_u32(value, "HTTP 重试次数"))
        .transpose()?
        .unwrap_or(0);
    let retry_delay = named
        .get("再次尝试前等待")
        .map(|value| timeout_duration(value, "再次尝试前等待"))
        .transpose()?
        .unwrap_or_else(|| Duration::from_millis(100));
    let mut method = method;
    let mut redirect_count = 0_u32;
    let response = 'redirects: loop {
        permissions.check_network(url.as_str())?;
        let mut response = None;
        let mut last_error = None;
        for attempt in 0..=retries {
            let request_timeout = min_timeout(Some(timeout), remaining_budget(deadline)?)
                .ok_or_else(|| error("网络请求超过程序执行时间限制"))?;
            let mut agent_builder = ureq::AgentBuilder::new()
                .timeout(request_timeout)
                .redirects(0);
            if let Some(proxy) = &proxy {
                agent_builder = agent_builder.proxy(
                    ureq::Proxy::new(proxy)
                        .map_err(|error_value| error(error_value.to_string()))?,
                );
            }
            let agent = agent_builder.build();
            let mut request = agent.request(&method, url.as_str());
            if let Some(Value::Record(headers)) = named.get("请求头") {
                for (key, value) in headers.borrow().iter() {
                    request = request.set(key, &value.to_string());
                }
            }
            if cancelled(&named, base_dir) {
                return Err(error("网络请求已取消"));
            }
            let result = send_http_request(
                request,
                &method,
                &url,
                &named,
                base_dir,
                permissions.buffer_limit(),
            )?;
            match result {
                Ok(value) | Err(ureq::Error::Status(_, value)) => {
                    response = Some(value);
                    break;
                }
                Err(error_value) => {
                    last_error = Some(error_value.to_string());
                    if attempt < retries {
                        let delay = min_timeout(Some(retry_delay), remaining_budget(deadline)?)
                            .ok_or_else(|| error("网络请求超过程序执行时间限制"))?;
                        std::thread::sleep(delay);
                    }
                }
            }
        }
        let response =
            response.ok_or_else(|| error(last_error.unwrap_or_else(|| "网络请求失败".into())))?;
        if (300..400).contains(&response.status()) {
            let Some(location) = response.header("Location") else {
                break 'redirects response;
            };
            if redirect_count >= max_redirects {
                return Err(error("网络请求重定向次数超过限制"));
            }
            let next = url
                .join(location)
                .map_err(|error_value| error(error_value.to_string()))?;
            permissions.check_network(next.as_str())?;
            let status = response.status();
            if status == 303 || ((status == 301 || status == 302) && method == "POST") {
                method = "GET".into();
            }
            url = next;
            redirect_count += 1;
            continue 'redirects;
        }
        break 'redirects response;
    };
    let status = response.status();
    let set_cookies = response
        .all("Set-Cookie")
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if set_cookies.len() > permissions.collection_limit() {
        return Err(error("HTTP 设置Cookie数量超过集合项数限制"));
    }
    let headers = response
        .headers_names()
        .into_iter()
        .filter_map(|name| {
            response
                .header(&name)
                .map(|value| (name, value.to_string()))
        })
        .collect();
    if cancelled(&named, base_dir) {
        return Err(error("网络请求已取消"));
    }
    let reader: Box<dyn Read> = Box::new(response.into_reader());
    let reader = match named.get("取消文件") {
        Some(path) => Box::new(CancellableReader {
            inner: reader,
            cancel_path: resolve(base_dir, path),
        }) as Box<dyn Read>,
        None => reader,
    };
    Ok(Value::HttpResponse(Rc::new(HttpResponseValue {
        status,
        headers,
        set_cookies,
        body: RefCell::new(HttpBody::Reader(reader)),
        buffer_limit: permissions.buffer_limit(),
    })))
}

fn decode_response_text(response: &HttpResponseValue, body: &[u8]) -> Result<String, String> {
    let label = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, value)| charset_label(value))
        .or_else(|| html_charset_label(body));
    if let Some(label) = label {
        let encoding = Encoding::for_label(label.as_bytes())
            .ok_or_else(|| error(format!("网页声明了不支持的字符编码“{label}”")))?;
        let (text, _, had_errors) = encoding.decode(body);
        if had_errors {
            return Err(error(format!(
                "网页正文不能完整按“{}”解码",
                encoding.name()
            )));
        }
        return Ok(text.into_owned());
    }
    String::from_utf8(body.to_vec()).map_err(|_| {
        error("网页正文不是 UTF-8，响应也没有声明字符编码；可读取“字节正文”后用 编码 转文本 明确指定编码",
        )
    })
}

fn charset_label(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        name.trim().eq_ignore_ascii_case("charset").then(|| {
            value
                .trim()
                .trim_matches(|character| character == '\'' || character == '"')
                .to_ascii_lowercase()
        })
    })
}

fn html_charset_label(body: &[u8]) -> Option<String> {
    let prefix = &body[..body.len().min(4_096)];
    let ascii = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    let (_, remainder) = ascii.split_once("charset")?;
    let remainder = remainder
        .trim_start_matches(|character: char| character.is_ascii_whitespace() || character == '=')
        .trim_start_matches(['\'', '"']);
    let label = remainder
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        .collect::<String>();
    (!label.is_empty()).then_some(label)
}

pub(super) fn min_timeout(
    local: Option<Duration>,
    execution_budget: Option<Duration>,
) -> Option<Duration> {
    match (local, execution_budget) {
        (Some(local), Some(budget)) => Some(local.min(budget)),
        (Some(local), None) => Some(local),
        (None, budget) => budget,
    }
}

fn remaining_budget(deadline: Option<Instant>) -> Result<Option<Duration>, String> {
    let Some(deadline) = deadline else {
        return Ok(None);
    };
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .map(Some)
        .ok_or_else(|| error("网络请求超过程序执行时间限制"))
}

fn send_http_request(
    mut request: ureq::Request,
    method: &str,
    url: &Url,
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    buffer_limit: usize,
) -> Result<Result<ureq::Response, ureq::Error>, String> {
    if method == "GET" || method == "HEAD" {
        return Ok(request.call());
    }
    Ok(if let Some(value) = named.get("JSON") {
        request.send_json(to_json(value)?)
    } else if let Some(Value::Record(form)) = named.get("表单") {
        let values = form
            .borrow()
            .iter()
            .map(|(key, value)| (key.clone(), value.to_string()))
            .collect::<Vec<_>>();
        let fields = values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        request.send_form(&fields)
    } else if let Some(Value::Record(files)) = named.get("文件上传") {
        let boundary = format!("hengshu-{}", uuid_boundary(url));
        let mut body = Vec::new();
        for (field, path) in files.borrow().iter() {
            let file_path = resolve(base_dir, path);
            let content = read_file_limited(&file_path, buffer_limit)
                .map_err(|error_value| error(format!("读取上传文件失败：{error_value}")))?;
            let filename = file_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("上传文件");
            append_body(
                &mut body,
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
                buffer_limit,)?;
            append_body(&mut body, &content, buffer_limit)?;
            append_body(&mut body, b"\r\n", buffer_limit)?;
        }
        append_body(
            &mut body,
            format!("--{boundary}--\r\n").as_bytes(),
            buffer_limit,
        )?;
        request = request.set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        );
        request.send_bytes(&body)
    } else if let Some(value) = named.get("正文") {
        request.send_bytes(&as_bytes(value)?)
    } else {
        request.call()
    })
}

pub(super) struct CancellableReader {
    pub(super) inner: Box<dyn Read>,
    pub(super) cancel_path: PathBuf,
}

impl Read for CancellableReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "网络请求已取消",
            ));
        }
        self.inner.read(buffer)
    }
}

pub(super) fn body_bytes(response: &HttpResponseValue) -> Result<Vec<u8>, String> {
    let mut body = response.body.borrow_mut();
    match &mut *body {
        HttpBody::Buffered(bytes) => Ok(bytes.clone()),
        HttpBody::Reader(reader) => {
            let Some(bytes) = read_body_with_limit(reader.as_mut(), response.buffer_limit)
                .map_err(|error_value| error(error_value.to_string()))?
            else {
                return Err(error(format!(
                    "HTTP 响应正文超过 {} MiB 缓冲上限，请使用转流读取",
                    response.buffer_limit / 1024 / 1024
                )));
            };
            *body = HttpBody::Buffered(bytes.clone());
            Ok(bytes)
        }
    }
}

fn read_body_with_limit(reader: &mut dyn Read, limit: usize) -> std::io::Result<Option<Vec<u8>>> {
    match read_to_end_limited(reader, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Ok(None),
        Err(error) => Err(error),
    }
}

fn append_body(body: &mut Vec<u8>, part: &[u8], limit: usize) -> Result<(), String> {
    let next = body
        .len()
        .checked_add(part.len())
        .ok_or_else(|| error("HTTP 请求正文大小溢出"))?;
    if next > limit {
        return Err(error("HTTP 请求正文超过缓冲上限"));
    }
    body.try_reserve(part.len())
        .map_err(|_| error("无法为 HTTP 请求正文分配内存"))?;
    body.extend_from_slice(part);
    Ok(())
}

fn take_body_reader(response: &HttpResponseValue) -> Box<dyn Read> {
    let mut body = response.body.borrow_mut();
    match std::mem::replace(&mut *body, HttpBody::Buffered(Vec::new())) {
        HttpBody::Reader(reader) => reader,
        HttpBody::Buffered(bytes) => Box::new(std::io::Cursor::new(bytes)),
    }
}

fn uuid_boundary(url: &Url) -> String {
    let digest = Sha256::digest(url.as_str().as_bytes());
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cancelled(named: &BTreeMap<String, Value>, base_dir: &Path) -> bool {
    named
        .get("取消文件")
        .is_some_and(|path| resolve(base_dir, path).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_body_limit_detects_oversized_readers() {
        let mut exact = std::io::Cursor::new(b"1234".to_vec());
        assert_eq!(
            read_body_with_limit(&mut exact, 4).unwrap(),
            Some(b"1234".to_vec())
        );

        let mut oversized = std::io::Cursor::new(b"12345".to_vec());
        assert_eq!(read_body_with_limit(&mut oversized, 4).unwrap(), None);
    }

    #[test]
    fn multipart_builder_obeys_buffer_limit() {
        let mut body = Vec::new();
        append_body(&mut body, b"1234", 4).unwrap();
        assert!(append_body(&mut body, b"5", 4)
            .unwrap_err()
            .contains("缓冲上限"));
    }

    #[test]
    fn response_text_decodes_declared_gb18030_from_header_or_html_meta() {
        let source = "<meta charset=\"gb18030\"><h1>中文图书</h1>";
        let (encoded, _, had_errors) = encoding_rs::GBK.encode(source);
        assert!(!had_errors);
        let response = |headers| HttpResponseValue {
            status: 200,
            headers,
            set_cookies: Vec::new(),
            body: RefCell::new(HttpBody::Buffered(Vec::new())),
            buffer_limit: 1_024,
        };
        assert_eq!(
            decode_response_text(
                &response(BTreeMap::from([(
                    "Content-Type".into(),
                    "text/html; charset=GB18030".into(),
                )])),
                &encoded,
            )
            .unwrap(),
            source
        );
        assert_eq!(
            decode_response_text(&response(BTreeMap::new()), &encoded).unwrap(),
            source
        );
    }

    #[test]
    fn response_text_reports_unknown_or_undeclared_non_utf8_encoding() {
        let response = |headers| HttpResponseValue {
            status: 200,
            headers,
            set_cookies: Vec::new(),
            body: RefCell::new(HttpBody::Buffered(Vec::new())),
            buffer_limit: 1_024,
        };
        let unknown = decode_response_text(
            &response(BTreeMap::from([(
                "content-type".into(),
                "text/html; charset=made-up".into(),
            )])),
            b"text",
        )
        .unwrap_err();
        assert!(unknown.contains("不支持的字符编码"));
        let undeclared =
            decode_response_text(&response(BTreeMap::new()), &[0x81, 0x81]).unwrap_err();
        assert!(undeclared.contains("字节正文"));
    }
    use std::io::Write;
    use std::net::TcpListener;

    use std::time::Duration as StdDuration;

    #[test]
    fn http_multipart_and_stream_keep_response_reader_lazy() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let directory = tempfile::tempdir().unwrap();
        let upload = directory.path().join("上传.bin");
        fs::write(&upload, b"payload").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let length = stream.read(&mut buffer).unwrap_or(0);
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..length]);
                let header_end = request.windows(4).position(|part| part == b"\r\n\r\n");
                if let Some(header_end) = header_end {
                    let header = String::from_utf8_lossy(&request[..header_end]);
                    let body_length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(str::trim)
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + body_length {
                        break;
                    }
                }
            }
            let response = b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
            stream.write_all(response).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        let files = Value::record(BTreeMap::from([(
            "文件".into(),
            Value::Path(Rc::new(upload)),
        )]));
        let mut permissions = PermissionSet::for_source(directory.path());
        permissions.unrestricted = true;
        let response = http_request(
            vec![
                Value::Text("POST".into()),
                Value::Text(format!("http://{address}/upload")),
            ],
            BTreeMap::from([("文件上传".into(), files)]),
            directory.path(),
            &permissions,
            None,
        )
        .unwrap();
        let Value::HttpResponse(response) = response else {
            panic!()
        };
        assert!(matches!(*response.body.borrow(), HttpBody::Reader(_)));
        assert_eq!(body_bytes(&response).unwrap(), b"hello");
        let request = server.join().unwrap();
        assert!(request.contains("multipart/form-data"));
        assert!(request.contains("payload"));
    }

    #[test]
    fn http_cancellation_file_is_checked_before_network() {
        let directory = tempfile::tempdir().unwrap();
        let cancel = directory.path().join("取消");
        fs::write(&cancel, b"stop").unwrap();
        let mut permissions = PermissionSet::for_source(directory.path());
        permissions.unrestricted = true;
        let result = http_request(
            vec![
                Value::Text("GET".into()),
                Value::Text("http://127.0.0.1:1".into()),
            ],
            BTreeMap::from([("取消文件".into(), Value::Path(Rc::new(cancel)))]),
            directory.path(),
            &permissions,
            None,
        );
        assert!(result.unwrap_err().contains("已取消"));
    }

    #[test]
    fn http_response_preserves_every_set_cookie_header() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(
                    b"HTTP/1.0 200 OK\r\nSet-Cookie: session=abc; Path=/\r\nSet-Cookie: region=sh; Path=/\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let mut permissions = PermissionSet::for_source(directory.path());
        permissions.network_hosts = vec!["127.0.0.1".into()];
        let response = http_request(
            vec![
                Value::Text("GET".into()),
                Value::Text(format!("http://{address}/cookies")),
            ],
            BTreeMap::new(),
            directory.path(),
            &permissions,
            None,
        )
        .unwrap();
        server.join().unwrap();
        let Value::HttpResponse(response) = response else {
            panic!("需要网络响应")
        };
        let cookies = response_attr(&response, "设置Cookie")
            .unwrap()
            .unwrap()
            .iter_values()
            .unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].to_string(), "session=abc; Path=/");
        assert_eq!(cookies[1].to_string(), "region=sh; Path=/");
    }

    #[test]
    fn http_set_cookie_count_obeys_collection_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(
                    b"HTTP/1.0 200 OK\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let mut permissions = PermissionSet::for_source(directory.path());
        permissions.network_hosts = vec!["127.0.0.1".into()];
        permissions.max_collection_items = Some(1);
        let error = http_request(
            vec![
                Value::Text("GET".into()),
                Value::Text(format!("http://{address}/cookies")),
            ],
            BTreeMap::new(),
            directory.path(),
            &permissions,
            None,
        )
        .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("设置Cookie数量超过集合项数限制"));
    }

    #[test]
    fn http_proxy_requires_its_own_network_permission() {
        let mut permissions = PermissionSet::restricted_for_source(Path::new("."));
        permissions.network_hosts = vec!["example.com".into()];
        let error = http_request(
            vec![
                Value::Text("GET".into()),
                Value::Text("https://example.com".into()),
            ],
            BTreeMap::from([("代理".into(), Value::Text("http://127.0.0.1:8080".into()))]),
            Path::new("."),
            &permissions,
            None,
        )
        .unwrap_err();
        assert!(error.contains("127.0.0.1"));
        assert!(error.contains("没有访问网络主机"));
    }

    #[test]
    fn http_body_reader_checks_cancellation_between_chunks() {
        let directory = tempfile::tempdir().unwrap();
        let cancel = directory.path().join("取消");
        let mut reader = CancellableReader {
            inner: Box::new(std::io::Cursor::new(b"response".to_vec())),
            cancel_path: cancel.clone(),
        };
        let mut first = [0; 4];
        assert_eq!(reader.read(&mut first).unwrap(), 4);
        fs::write(cancel, b"stop").unwrap();
        let error = reader.read(&mut first).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(error.to_string().contains("已取消"));
    }

    #[test]
    fn http_redirects_recheck_network_permissions() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.0 302 Found\r\nLocation: http://localhost:{}/final\r\nContent-Length: 0\r\n\r\n",
                address.port()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let mut permissions = PermissionSet::restricted_for_source(directory.path());
        permissions.network_hosts = vec!["127.0.0.1".into()];
        let error = http_request(
            vec![
                Value::Text("GET".into()),
                Value::Text(format!("http://{address}/start")),
            ],
            BTreeMap::new(),
            directory.path(),
            &permissions,
            None,
        )
        .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("localhost"));
        assert!(error.contains("没有访问网络主机"));
    }

    #[cfg(unix)]
    #[test]
    fn http_requests_obey_execution_budget_before_local_timeout() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            std::thread::sleep(StdDuration::from_millis(150));
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        });
        let directory = tempfile::tempdir().unwrap();
        let mut permissions = PermissionSet::for_source(directory.path());
        permissions.network_hosts = vec!["127.0.0.1".into()];
        let started = std::time::Instant::now();
        let result = http_request(
            vec![
                Value::Text("GET".into()),
                Value::Text(format!("http://{address}/slow")),
            ],
            BTreeMap::from([(
                "最长等待".into(),
                Value::Duration(chrono::Duration::seconds(5)),
            )]),
            directory.path(),
            &permissions,
            Some(StdDuration::from_millis(20)),
        );
        assert!(result.is_err());
        assert!(started.elapsed() < StdDuration::from_millis(100));
        server.join().unwrap();
    }
}
