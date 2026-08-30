use super::codec::{as_bytes, bytes, decode, encode, to_json};
use super::error;
use super::path::{normalize, resolve};
use crate::contracts::{decimal_to_u64, decimal_to_usize};
use crate::resource::read_file_limited;
use crate::value::{StreamValue, Value};
use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, TimeZone};
use rust_decimal::Decimal;
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};
use walkdir::WalkDir;

pub(super) fn file_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    buffer_limit: usize,
) -> Result<Value, String> {
    let path = || resolve(base_dir, &args[0]);
    match name {
        "读取文本" => {
            let data = read_file_limited(&path(), buffer_limit)
                .map_err(|e| file_error("读取", &path(), &e))?;
            decode(&data, named_text(&named, "编码", "UTF-8")).map(Value::Text)
        }
        "读取字节" => read_file_limited(&path(), buffer_limit)
            .map(bytes)
            .map_err(|e| file_error("读取", &path(), &e)),
        "写入文本" => {
            let data = encode(&args[1].to_string(), named_text(&named, "编码", "UTF-8"))?;
            ensure_parent(&path())?;
            fs::write(path(), data)
                .map(|_| Value::Null)
                .map_err(|e| file_error("写入", &path(), &e))
        }
        "写入字节" => {
            ensure_parent(&path())?;
            fs::write(path(), as_bytes(&args[1])?)
                .map(|_| Value::Null)
                .map_err(|e| file_error("写入", &path(), &e))
        }
        "追加文本" => {
            use std::fs::OpenOptions;
            ensure_parent(&path())?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path())
                .map_err(|e| file_error("追加", &path(), &e))?;
            file.write_all(&encode(
                &args[1].to_string(),
                named_text(&named, "编码", "UTF-8"),
            )?)
            .map(|_| Value::Null)
            .map_err(|e| file_error("追加", &path(), &e))
        }
        "存在" => Ok(Value::Bool(path().exists())),
        "是文件" => Ok(Value::Bool(path().is_file())),
        "是目录" => Ok(Value::Bool(path().is_dir())),
        "创建目录" => fs::create_dir_all(path())
            .map(|_| Value::Null)
            .map_err(|e| file_error("创建目录", &path(), &e)),
        "复制" => {
            let target = resolve(base_dir, &args[1]);
            ensure_parent(&target)?;
            fs::copy(path(), &target)
                .map(|size| Value::Number(Decimal::from(size)))
                .map_err(|e| file_error("复制到", &target, &e))
        }
        "移动" => {
            let target = resolve(base_dir, &args[1]);
            ensure_parent(&target)?;
            fs::rename(path(), &target)
                .map(|_| Value::Null)
                .map_err(|e| file_error("移动到", &target, &e))
        }
        "删除" => {
            let target = path();
            reject_dangerous_delete(&target)?;
            if target.is_dir() {
                if named.get("递归").is_some_and(Value::truth) {
                    fs::remove_dir_all(target)
                } else {
                    fs::remove_dir(target)
                }
            } else {
                fs::remove_file(target)
            }
            .map(|_| Value::Null)
            .map_err(|e| error(e.to_string()))
        }
        "遍历" => {
            let entries = fs::read_dir(path())
                .map_err(|e| error(e.to_string()))?
                .map(|entry| {
                    entry
                        .map(|entry| Value::Path(Rc::new(entry.path())))
                        .map_err(|e| error(e.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::list(entries))
        }
        "递归遍历" => {
            let entries = WalkDir::new(path())
                .into_iter()
                .map(|entry| {
                    entry
                        .map(|entry| Value::Path(Rc::new(entry.path().to_path_buf())))
                        .map_err(|e| error(e.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::list(entries))
        }
        "元数据" => {
            let metadata = fs::metadata(path()).map_err(|e| error(e.to_string()))?;
            Ok(Value::record(BTreeMap::from([
                ("大小".into(), Value::Number(Decimal::from(metadata.len()))),
                ("是文件".into(), Value::Bool(metadata.is_file())),
                ("是目录".into(), Value::Bool(metadata.is_dir())),
                (
                    "只读".into(),
                    Value::Bool(metadata.permissions().readonly()),
                ),
            ])))
        }
        "逐行读取" => {
            let file = fs::File::open(path()).map_err(|e| error(e.to_string()))?;
            Ok(Value::Stream(Rc::new(RefCell::new(StreamValue {
                source: crate::stream::lines(BufReader::new(file), buffer_limit),
                operations: Vec::new(),
            }))))
        }
        _ => Err(error(format!("文件系统不支持“{name}”"))),
    }
}

pub(super) fn datetime_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
) -> Result<Value, String> {
    match name {
        "现在" => Ok(Value::DateTime(Local::now().fixed_offset())),
        "今天" => Ok(Value::Text(Local::now().date_naive().to_string())),
        "解析" => {
            let text = args[0].to_string();
            if let Ok(value) = DateTime::parse_from_rfc3339(&text) {
                return Ok(Value::DateTime(value));
            }
            let format = named_text(&named, "格式", "%Y-%m-%d %H:%M:%S");
            let naive =
                NaiveDateTime::parse_from_str(&text, &format).map_err(|e| error(e.to_string()))?;
            let offset = parse_offset(named_text(&named, "时区", "+08:00"))?;
            offset
                .from_local_datetime(&naive)
                .single()
                .map(Value::DateTime)
                .ok_or_else(|| error("日期时间在指定时区中不唯一"))
        }
        "格式化" => match &args[0] {
            Value::DateTime(value) => Ok(Value::Text(
                value
                    .format(
                        &args
                            .get(1)
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".into()),
                    )
                    .to_string(),
            )),
            _ => Err(error("格式化需要日期时间值")),
        },
        "增加" => match (&args[0], &args[1]) {
            (Value::DateTime(value), Value::Duration(duration)) => {
                Ok(Value::DateTime(*value + *duration))
            }
            _ => Err(error("增加需要日期时间和持续时间")),
        },
        "相差" => match (&args[0], &args[1]) {
            (Value::DateTime(left), Value::DateTime(right)) => Ok(Value::Duration(*left - *right)),
            _ => Err(error("相差需要两个日期时间")),
        },
        _ => Err(error(format!("日期时间模块不支持“{name}”"))),
    }
}

pub(super) fn environment_call(
    name: &str,
    args: Vec<Value>,
    program_args: &[String],
    base_dir: &Path,
) -> Result<Value, String> {
    match name {
        "读取" => Ok(crate::dotenv::value(base_dir, &args[0].to_string())
            .map(Value::Text)
            .unwrap_or(Value::Null)),
        "设置" => {
            std::env::set_var(args[0].to_string(), args[1].to_string());
            Ok(Value::Null)
        }
        "全部" => Ok(Value::record(
            crate::dotenv::values(base_dir)
                .into_iter()
                .map(|(key, value)| (key, Value::Text(value)))
                .collect(),
        )),
        "当前目录" => std::env::current_dir()
            .map(|path| Value::Path(Rc::new(path)))
            .map_err(|e| error(e.to_string())),
        "切换目录" => std::env::set_current_dir(resolve(base_dir, &args[0]))
            .map(|_| Value::Null)
            .map_err(|e| error(e.to_string())),
        "程序参数" => Ok(Value::list(
            program_args.iter().cloned().map(Value::Text).collect(),
        )),
        "平台" => Ok(Value::record(BTreeMap::from([
            ("操作系统".into(), Value::Text(std::env::consts::OS.into())),
            ("架构".into(), Value::Text(std::env::consts::ARCH.into())),
            ("系列".into(), Value::Text(std::env::consts::FAMILY.into())),
        ]))),
        _ => Err(error(format!("环境模块不支持“{name}”"))),
    }
}

pub(super) fn logging_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
) -> Result<Value, String> {
    for value in &args {
        if let Value::Table(table) = value {
            let endpoint = if named.contains_key("文件") {
                crate::privacy::Endpoint::LocalFile
            } else {
                crate::privacy::Endpoint::Log
            };
            crate::privacy::guard_endpoint(endpoint, &table.borrow().sensitivity())?;
        }
    }
    let (level, message) = if name == "记录" {
        (args[0].to_string(), args[1].to_string())
    } else {
        (name.to_string(), args[0].to_string())
    };
    let fields = named
        .get("字段")
        .map(to_json)
        .transpose()?
        .unwrap_or(JsonValue::Object(Default::default()));
    let entry = serde_json::json!({
        "时间": Local::now().to_rfc3339(),
        "级别": level,
        "信息": message,
        "字段": fields,
    });
    let text = serde_json::to_string(&entry).map_err(|e| error(e.to_string()))?;
    if let Some(path) = named.get("文件") {
        use std::fs::OpenOptions;
        static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = LOG_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| error("日志写入锁已损坏"))?;
        let path = resolve(base_dir, path);
        ensure_parent(&path)?;
        if let Some(limit) = named
            .get("轮换字节")
            .map(Value::as_decimal)
            .transpose()?
            .map(|value| decimal_to_u64(value, "日志轮换字节"))
            .transpose()?
        {
            if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= limit) {
                let retain = named
                    .get("保留份数")
                    .map(Value::as_decimal)
                    .transpose()?
                    .map(|value| decimal_to_usize(value, "日志保留份数"))
                    .transpose()?
                    .unwrap_or(3)
                    .max(1);
                for index in (1..=retain).rev() {
                    let current = if index == 1 {
                        path.clone()
                    } else {
                        path.with_extension(format!("日志.{}", index - 1))
                    };
                    let next = path.with_extension(format!("日志.{index}"));
                    if current.exists() {
                        if next.exists() {
                            fs::remove_file(&next)
                                .map_err(|e| error(format!("删除旧日志轮换文件失败：{e}")))?;
                        }
                        fs::rename(&current, &next)
                            .map_err(|e| error(format!("轮换日志文件失败：{e}")))?;
                    }
                }
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| error(e.to_string()))?;
        writeln!(file, "{text}").map_err(|e| error(e.to_string()))?;
    } else {
        eprintln!("{text}");
    }
    Ok(Value::Null)
}

/// 系统错误本身不含路径，直接透出会得到“Permission denied”这类无法定位的信息。
pub(super) fn file_error(action: &str, path: &Path, cause: &impl std::fmt::Display) -> String {
    error(format!("{action}文件“{}”失败：{cause}", path.display()))
}

pub(super) fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| file_error("创建目录", parent, &e))?;
    }
    Ok(())
}

fn reject_dangerous_delete(path: &Path) -> Result<(), String> {
    let normalized = normalize(path);
    if normalized.parent().is_none() || normalized.as_os_str().is_empty() {
        return Err(error("拒绝删除文件系统根目录或空路径"));
    }
    Ok(())
}

pub(super) fn named_text(named: &BTreeMap<String, Value>, name: &str, default: &str) -> String {
    named
        .get(name)
        .map(ToString::to_string)
        .unwrap_or_else(|| default.into())
}

fn parse_offset(text: String) -> Result<FixedOffset, String> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let value = text.trim_start_matches(['+', '-']);
    let (hours, minutes) = value
        .split_once(':')
        .ok_or_else(|| error("时区偏移应写成 +08:00"))?;
    let seconds = sign
        * (hours.parse::<i32>().map_err(|e| error(e.to_string()))? * 3600
            + minutes.parse::<i32>().map_err(|e| error(e.to_string()))? * 60);
    FixedOffset::east_opt(seconds).ok_or_else(|| error("时区偏移超出范围"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_file_reads_obey_the_configured_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.txt");
        fs::write(&path, b"12345").unwrap();
        let error = file_call(
            "读取字节",
            vec![Value::Text(path.to_string_lossy().into_owned())],
            BTreeMap::new(),
            Path::new("."),
            4,
        )
        .unwrap_err();
        assert!(error.contains("缓冲上限"));
    }

    #[test]
    fn structured_log_rotates_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("应用.log");
        let named = BTreeMap::from([
            ("文件".into(), Value::Path(Rc::new(path.clone()))),
            ("轮换字节".into(), Value::Number(Decimal::from(1))),
            ("保留份数".into(), Value::Number(Decimal::from(2))),
        ]);
        logging_call(
            "信息",
            vec![Value::Text("第一条".into())],
            named.clone(),
            directory.path(),
        )
        .unwrap();
        logging_call(
            "信息",
            vec![Value::Text("第二条".into())],
            named,
            directory.path(),
        )
        .unwrap();
        assert!(path.exists());
        assert!(path.with_extension("日志.1").exists());
    }
}
