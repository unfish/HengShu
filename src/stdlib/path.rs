use super::error;
use crate::value::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

pub(super) fn path_call(name: &str, args: Vec<Value>, base_dir: &Path) -> Result<Value, String> {
    match name {
        "拼接" => {
            let mut path = path_from(&args[0]);
            for component in &args[1..] {
                path.push(component.to_string());
            }
            Ok(Value::Path(Rc::new(path)))
        }
        "绝对路径" => fs::canonicalize(resolve(base_dir, &args[0]))
            .map(|path| Value::Path(Rc::new(path)))
            .map_err(|error_value| error(error_value.to_string())),
        "规范化" => Ok(Value::Path(Rc::new(normalize(&path_from(&args[0]))))),
        "当前目录" => std::env::current_dir()
            .map(|path| Value::Path(Rc::new(path)))
            .map_err(|error_value| error(error_value.to_string())),
        _ => Err(error(format!("路径处理不支持“{name}”"))),
    }
}

pub(super) fn path_from(value: &Value) -> PathBuf {
    match value {
        Value::Path(path) => path.as_ref().clone(),
        value => PathBuf::from(value.to_string()),
    }
}

pub(super) fn resolve(base_dir: &Path, value: &Value) -> PathBuf {
    let path = path_from(value);
    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}

pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}
