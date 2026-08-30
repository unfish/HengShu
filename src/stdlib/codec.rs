use super::error;
use super::local::{ensure_parent, named_text};
use super::path::resolve;
use crate::contracts::decimal_to_u8;
use crate::resource::read_string_limited;
use crate::value::Value;
use base64::Engine;
use encoding_rs::Encoding;
use rust_decimal::Decimal;
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;

pub(super) fn encoding_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    collection_limit: usize,
) -> Result<Value, String> {
    match name {
        "转字节" => encode(&args[0].to_string(), named_text(&named, "编码", "UTF-8")).map(bytes),
        "转文本" => decode(
            &as_bytes_limited(&args[0], collection_limit)?,
            named_text(&named, "编码", "UTF-8"),
        )
        .map(Value::Text),
        "Base64编码" => Ok(Value::Text(
            base64::engine::general_purpose::STANDARD
                .encode(as_bytes_limited(&args[0], collection_limit)?),
        )),
        "Base64解码" => base64::engine::general_purpose::STANDARD
            .decode(args[0].to_string())
            .map(bytes)
            .map_err(|error_value| error(error_value.to_string())),
        "十六进制编码" => Ok(Value::Text(
            as_bytes_limited(&args[0], collection_limit)?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )),
        "十六进制解码" => decode_hex(&args[0].to_string()).map(bytes),
        _ => Err(error(format!("编码模块不支持“{name}”"))),
    }
}

pub(super) fn json_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    buffer_limit: usize,
    collection_limit: usize,
) -> Result<Value, String> {
    match name {
        "解析" => serde_json::from_str::<JsonValue>(&args[0].to_string())
            .map(|value| from_json(&value))
            .map_err(|error_value| error(error_value.to_string())),
        "生成" => {
            let json = to_json(&args[0])?;
            if named.get("美化").is_some_and(Value::truth) {
                serde_json::to_string_pretty(&json)
            } else {
                serde_json::to_string(&json)
            }
            .map(Value::Text)
            .map_err(|error_value| error(error_value.to_string()))
        }
        "读取行" => {
            let text = read_string_limited(&resolve(base_dir, &args[0]), buffer_limit)
                .map_err(|error_value| error(error_value.to_string()))?;
            let mut values = Vec::new();
            for line_text in text
                .lines()
                .filter(|text_line| !text_line.trim().is_empty())
            {
                if values.len() >= collection_limit {
                    return Err(error("JSON 行数超过集合资源限制"));
                }
                values.push(
                    serde_json::from_str::<JsonValue>(line_text)
                        .map(|value| from_json(&value))
                        .map_err(|error_value| error(error_value.to_string()))?,
                );
            }
            Ok(Value::list(values))
        }
        "写入行" => {
            let path = resolve(base_dir, &args[0]);
            ensure_parent(&path)?;
            let mut output = String::new();
            for value in args[1].iter_values_limited(collection_limit)? {
                output.push_str(
                    &serde_json::to_string(&to_json(&value)?)
                        .map_err(|error_value| error(error_value.to_string()))?,
                );
                output.push('\n');
            }
            fs::write(path, output)
                .map(|_| Value::Null)
                .map_err(|error_value| error(error_value.to_string()))
        }
        _ => Err(error(format!("JSON 模块不支持“{name}”"))),
    }
}

pub(super) fn toml_call(name: &str, args: Vec<Value>) -> Result<Value, String> {
    match name {
        "解析" => {
            let value: toml::Value = toml::from_str(&args[0].to_string())
                .map_err(|error_value| error(error_value.to_string()))?;
            let json = serde_json::to_value(value)
                .map_err(|error_value| error(error_value.to_string()))?;
            Ok(from_json(&json))
        }
        "生成" => {
            let json = to_json(&args[0])?;
            let value = toml::Value::try_from(json)
                .map_err(|error_value| error(error_value.to_string()))?;
            toml::to_string_pretty(&value)
                .map(Value::Text)
                .map_err(|error_value| error(error_value.to_string()))
        }
        _ => Err(error(format!("TOML 模块不支持“{name}”"))),
    }
}

pub(super) fn to_json(value: &Value) -> Result<JsonValue, String> {
    Ok(match value {
        Value::Null => JsonValue::Null,
        Value::Bool(value) => JsonValue::Bool(*value),
        Value::Number(value) => JsonValue::Number(
            serde_json::Number::from_str(&value.to_string())
                .map_err(|error_value| error(error_value.to_string()))?,
        ),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Value::Text(value) => JsonValue::String(value.clone()),
        Value::List(values) | Value::Set(values) => JsonValue::Array(
            values
                .borrow()
                .iter()
                .map(to_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Record(values) => JsonValue::Object(
            values
                .borrow()
                .iter()
                .map(|(key, value)| Ok((key.clone(), to_json(value)?)))
                .collect::<Result<_, String>>()?,
        ),
        Value::StructInstance(value) => JsonValue::Object(
            value
                .borrow()
                .fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), to_json(value)?)))
                .collect::<Result<_, String>>()?,
        ),
        Value::Table(table) => JsonValue::Array(
            table
                .borrow()
                .rows
                .iter()
                .map(|row| to_json(&Value::record(row.clone())))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Bytes(value) => JsonValue::String(
            base64::engine::general_purpose::STANDARD.encode(value.borrow().as_slice()),
        ),
        Value::Path(value) => JsonValue::String(value.to_string_lossy().into_owned()),
        Value::DateTime(value) => JsonValue::String(value.to_rfc3339()),
        Value::Duration(value) => JsonValue::Number(value.num_milliseconds().into()),
        Value::Money(value) => serde_json::json!({
            "数额": value.amount.to_string(),
            "币种": value.currency,
            "小数位": value.scale,
        }),
        Value::Error(value) => serde_json::json!({
            "类别": value.category,
            "信息": value.message,
            "行号": value.line,
            "调用栈": value.stack,
            "原因": value.cause.as_ref().map(|cause| format!("{}：{}", cause.category, cause.message)),
        }),
        _ => return Err(error(format!("{}不能转换为 JSON", value.type_name()))),
    })
}

pub(super) fn from_json(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Bool(*value),
        JsonValue::Number(value) => Decimal::from_str(&value.to_string())
            .map(Value::Number)
            .unwrap_or_else(|_| Value::Float(value.as_f64().unwrap_or(f64::NAN))),
        JsonValue::String(value) => Value::Text(value.clone()),
        JsonValue::Array(values) => Value::list(values.iter().map(from_json).collect()),
        JsonValue::Object(values) => Value::record(
            values
                .iter()
                .map(|(key, value)| (key.clone(), from_json(value)))
                .collect(),
        ),
    }
}

pub(super) fn bytes(value: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(value)))
}

pub(super) fn as_bytes(value: &Value) -> Result<Vec<u8>, String> {
    as_bytes_limited(value, crate::resource::DEFAULT_MAX_COLLECTION_ITEMS)
}

fn as_bytes_limited(value: &Value, collection_limit: usize) -> Result<Vec<u8>, String> {
    match value {
        Value::Bytes(value) => Ok(value.borrow().clone()),
        Value::Text(value) => Ok(value.as_bytes().to_vec()),
        _ => value
            .iter_values_limited(collection_limit)?
            .iter()
            .map(|value| decimal_to_u8(value.as_decimal()?, "字节值"))
            .collect(),
    }
}

pub(super) fn encode(text: &str, label: String) -> Result<Vec<u8>, String> {
    if label.eq_ignore_ascii_case("UTF-8") {
        return Ok(text.as_bytes().to_vec());
    }
    let encoding = Encoding::for_label(label.as_bytes())
        .ok_or_else(|| error(format!("不支持编码“{label}”")))?;
    let (data, _, had_errors) = encoding.encode(text);
    if had_errors {
        return Err(error(format!("文本不能完整转换为“{label}”")));
    }
    Ok(data.into_owned())
}

pub(super) fn decode(data: &[u8], label: String) -> Result<String, String> {
    if label.eq_ignore_ascii_case("UTF-8") {
        return String::from_utf8(data.to_vec())
            .map_err(|error_value| error(error_value.to_string()));
    }
    let encoding = Encoding::for_label(label.as_bytes())
        .ok_or_else(|| error(format!("不支持编码“{label}”")))?;
    let (text, _, had_errors) = encoding.decode(data);
    if had_errors {
        return Err(error(format!("字节不是有效的“{label}”文本")));
    }
    Ok(text.into_owned())
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    let digits = text.chars().collect::<Vec<_>>();
    if digits.len() % 2 != 0 {
        return Err(error("十六进制文本长度必须是偶数"));
    }
    digits
        .chunks(2)
        .map(|pair| {
            let high = pair[0]
                .to_digit(16)
                .ok_or_else(|| error(format!("“{}”不是十六进制字符", pair[0])))?;
            let low = pair[1]
                .to_digit(16)
                .ok_or_else(|| error(format!("“{}”不是十六进制字符", pair[1])))?;
            Ok((high * 16 + low) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::compression_call;
    use super::*;

    #[test]
    fn json_encoding_and_compression_round_trip() {
        let value = Value::record(BTreeMap::from([
            ("名称".into(), Value::Text("衡枢".into())),
            ("数量".into(), Value::Number(Decimal::from(3))),
        ]));
        let generated = json_call(
            "生成",
            vec![value],
            BTreeMap::new(),
            Path::new("."),
            crate::resource::DEFAULT_MAX_BUFFER_BYTES,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap()
        .to_string();
        let parsed = json_call(
            "解析",
            vec![Value::Text(generated)],
            BTreeMap::new(),
            Path::new("."),
            crate::resource::DEFAULT_MAX_BUFFER_BYTES,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        let Value::Record(parsed) = parsed else {
            panic!("需要记录");
        };
        assert_eq!(parsed.borrow()["名称"].to_string(), "衡枢");

        let compressed = compression_call(
            "Gzip压缩",
            vec![Value::Text("中文数据".into())],
            crate::resource::DEFAULT_MAX_BUFFER_BYTES,
        )
        .unwrap();
        let decompressed = compression_call(
            "Gzip解压",
            vec![compressed.clone()],
            crate::resource::DEFAULT_MAX_BUFFER_BYTES,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(as_bytes(&decompressed).unwrap()).unwrap(),
            "中文数据"
        );
        assert!(compression_call("Gzip解压", vec![compressed], 2)
            .unwrap_err()
            .contains("缓冲上限"));
    }
}
