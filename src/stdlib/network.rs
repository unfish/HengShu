use super::codec::{as_bytes, bytes};
use super::error;
use super::http::{http_request, min_timeout};
use crate::contracts::{decimal_to_usize, timeout_duration};
use crate::resource::DEFAULT_MAX_BUFFER_BYTES;
use crate::security::PermissionSet;
use crate::value::{SocketValue, Value};
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use url::Url;

pub(super) fn network_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
    execution_budget: Option<Duration>,
) -> Result<Value, String> {
    match name {
        "解析URL" => parse_url(&args[0]),
        "拼接URL" => Url::parse(&args[0].to_string())
            .and_then(|base| base.join(&args[1].to_string()))
            .map(|url| Value::Text(url.to_string()))
            .map_err(|error_value| error(error_value.to_string())),
        "请求" => http_request(args, named, base_dir, permissions, execution_budget),
        "连接TCP" => connect_tcp(&args[0], &named, execution_budget),
        "监听TCP" => TcpListener::bind(args[0].to_string())
            .map(|listener| {
                Value::Socket(Rc::new(RefCell::new(SocketValue::TcpListener(listener))))
            })
            .map_err(|error_value| error(error_value.to_string())),
        "绑定UDP" => UdpSocket::bind(args[0].to_string())
            .map(|socket| Value::Socket(Rc::new(RefCell::new(SocketValue::Udp(socket)))))
            .map_err(|error_value| error(error_value.to_string())),
        _ => Err(error(format!("网络模块不支持“{name}”"))),
    }
}

fn parse_url(value: &Value) -> Result<Value, String> {
    let url =
        Url::parse(&value.to_string()).map_err(|error_value| error(error_value.to_string()))?;
    Ok(Value::record(BTreeMap::from([
        ("协议".into(), Value::Text(url.scheme().into())),
        (
            "主机".into(),
            url.host_str()
                .map(|host| Value::Text(host.into()))
                .unwrap_or(Value::Null),
        ),
        (
            "端口".into(),
            url.port()
                .map(|port| Value::Number(Decimal::from(port)))
                .unwrap_or(Value::Null),
        ),
        ("路径".into(), Value::Text(url.path().into())),
        (
            "查询".into(),
            Value::record(
                url.query_pairs()
                    .map(|(key, value)| (key.into_owned(), Value::Text(value.into_owned())))
                    .collect(),
            ),
        ),
    ])))
}

fn connect_tcp(
    value: &Value,
    named: &BTreeMap<String, Value>,
    execution_budget: Option<Duration>,
) -> Result<Value, String> {
    let address = value.to_string();
    let timeout = named
        .get("最长等待")
        .map(|value| timeout_duration(value, "TCP 连接最长等待"))
        .transpose()?
        .unwrap_or_else(|| Duration::from_millis(30_000));
    let timeout = min_timeout(Some(timeout), execution_budget)
        .ok_or_else(|| error("TCP 连接超过程序执行时间限制"))?;
    let address = address
        .to_socket_addrs()
        .map_err(|error_value| error(error_value.to_string()))?
        .next()
        .ok_or_else(|| error("无法解析 TCP 地址"))?;
    let stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error_value| error(error_value.to_string()))?;
    Ok(Value::Socket(Rc::new(RefCell::new(
        SocketValue::TcpStream(stream),
    ))))
}

pub(super) fn socket_method(
    socket: &Rc<RefCell<SocketValue>>,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    execution_budget: Option<Duration>,
) -> Result<Value, String> {
    match (&mut *socket.borrow_mut(), name) {
        (SocketValue::TcpStream(stream), "发送") => {
            stream
                .set_write_timeout(execution_budget)
                .map_err(|error_value| error(error_value.to_string()))?;
            stream
                .write_all(&as_bytes(&args[0])?)
                .map_err(|error_value| error(error_value.to_string()))?;
            Ok(Value::Null)
        }
        (SocketValue::TcpStream(stream), "接收") => {
            stream
                .set_read_timeout(execution_budget)
                .map_err(|error_value| error(error_value.to_string()))?;
            let size = named
                .get("最多字节")
                .or_else(|| args.first())
                .map(Value::as_decimal)
                .transpose()?
                .map(|value| decimal_to_usize(value, "套接字接收字节数"))
                .transpose()?
                .unwrap_or(65_536);
            if size > DEFAULT_MAX_BUFFER_BYTES {
                return Err(error("套接字接收大小超过缓冲上限"));
            }
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(size)
                .map_err(|_| error("套接字接收大小超出可分配范围"))?;
            buffer.resize(size, 0);
            let length = stream
                .read(&mut buffer)
                .map_err(|error_value| error(error_value.to_string()))?;
            buffer.truncate(length);
            Ok(bytes(buffer))
        }
        (SocketValue::TcpListener(listener), "接受") => {
            let (stream, address) = accept_with_budget(listener, execution_budget)?;
            Ok(Value::record(BTreeMap::from([
                (
                    "连接".into(),
                    Value::Socket(Rc::new(RefCell::new(SocketValue::TcpStream(stream)))),
                ),
                ("地址".into(), Value::Text(address.to_string())),
            ])))
        }
        (SocketValue::Udp(socket), "发送到") => {
            socket
                .set_write_timeout(execution_budget)
                .map_err(|error_value| error(error_value.to_string()))?;
            socket
                .send_to(&as_bytes(&args[0])?, args[1].to_string())
                .map(|count| Value::Number(Decimal::from(count)))
                .map_err(|error_value| error(error_value.to_string()))
        }
        (SocketValue::Udp(socket), "接收") => {
            socket
                .set_read_timeout(execution_budget)
                .map_err(|error_value| error(error_value.to_string()))?;
            let mut buffer = vec![0; 65_536];
            let (length, address) = socket
                .recv_from(&mut buffer)
                .map_err(|error_value| error(error_value.to_string()))?;
            buffer.truncate(length);
            Ok(Value::record(BTreeMap::from([
                ("字节".into(), bytes(buffer)),
                ("地址".into(), Value::Text(address.to_string())),
            ])))
        }
        _ => Err(error(format!("网络套接字不支持“{name}”"))),
    }
}

fn accept_with_budget(
    listener: &TcpListener,
    execution_budget: Option<Duration>,
) -> Result<(TcpStream, std::net::SocketAddr), String> {
    let Some(budget) = execution_budget else {
        return listener
            .accept()
            .map_err(|error_value| error(error_value.to_string()));
    };
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .ok_or_else(|| error("TCP 接受连接的时间预算无效"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error_value| error(error_value.to_string()))?;
    let result = loop {
        match listener.accept() {
            Ok(value) => break Ok(value),
            Err(error_value) if error_value.kind() == std::io::ErrorKind::WouldBlock => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    break Err(error("TCP 接受连接超过程序执行时间限制"));
                }
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(5)),
                );
            }
            Err(error_value) => break Err(error(error_value.to_string())),
        }
    };
    let reset = listener
        .set_nonblocking(false)
        .map_err(|error_value| error(error_value.to_string()));
    match (result, reset) {
        (Err(error_value), _) | (Ok(_), Err(error_value)) => Err(error_value),
        (Ok(value), Ok(())) => Ok(value),
    }
}
