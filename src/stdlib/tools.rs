use super::codec::{as_bytes, bytes};
use super::error;
use crate::contracts::decimal_to_usize;
use crate::resource::read_to_end_limited;
use crate::value::Value;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::io::Write;

pub(super) fn regex_call(name: &str, args: Vec<Value>) -> Result<Value, String> {
    let regex =
        Regex::new(&args[0].to_string()).map_err(|error_value| error(error_value.to_string()))?;
    let text = args[1].to_string();
    match name {
        "是否匹配" => Ok(Value::Bool(regex.is_match(&text))),
        "查找" => Ok(regex
            .find(&text)
            .map(|found| Value::Text(found.as_str().into()))
            .unwrap_or(Value::Null)),
        "查找全部" => Ok(Value::list(
            regex
                .find_iter(&text)
                .map(|found| Value::Text(found.as_str().into()))
                .collect(),
        )),
        "替换" => Ok(Value::Text(
            regex.replace_all(&text, args[2].to_string()).into_owned(),
        )),
        "分割" => Ok(Value::list(
            regex
                .split(&text)
                .map(|part| Value::Text(part.into()))
                .collect(),
        )),
        _ => Err(error(format!("正则模块不支持“{name}”"))),
    }
}

pub(super) fn compression_call(
    name: &str,
    args: Vec<Value>,
    buffer_limit: usize,
) -> Result<Value, String> {
    match name {
        "Gzip压缩" => {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(&as_bytes(&args[0])?)
                .map_err(|error_value| error(error_value.to_string()))?;
            encoder
                .finish()
                .map(bytes)
                .map_err(|error_value| error(error_value.to_string()))
        }
        "Gzip解压" => {
            let input = as_bytes(&args[0])?;
            let decoder = GzDecoder::new(input.as_slice());
            let output = read_to_end_limited(decoder, buffer_limit)
                .map_err(|error_value| error(error_value.to_string()))?;
            Ok(bytes(output))
        }
        _ => Err(error(format!("压缩模块不支持“{name}”"))),
    }
}

pub(super) fn crypto_call(
    name: &str,
    args: Vec<Value>,
    buffer_limit: usize,
) -> Result<Value, String> {
    match name {
        "安全随机字节" => {
            let size = decimal_to_usize(args[0].as_decimal()?, "随机字节长度")?;
            if size > buffer_limit {
                return Err(error("随机字节长度超过缓冲上限"));
            }
            let mut output = Vec::new();
            output
                .try_reserve_exact(size)
                .map_err(|_| error("随机字节长度超出可分配范围"))?;
            output.resize(size, 0);
            OsRng.fill_bytes(&mut output);
            Ok(bytes(output))
        }
        "SHA256" => Ok(bytes(Sha256::digest(as_bytes(&args[0])?).to_vec())),
        "HMAC_SHA256" => {
            let mut hmac = Hmac::<Sha256>::new_from_slice(&as_bytes(&args[0])?)
                .map_err(|error_value| error(error_value.to_string()))?;
            hmac.update(&as_bytes(&args[1])?);
            Ok(bytes(hmac.finalize().into_bytes().to_vec()))
        }
        "恒定时间相等" => {
            let left = as_bytes(&args[0])?;
            let right = as_bytes(&args[1])?;
            let mut difference = left.len() ^ right.len();
            for index in 0..left.len().max(right.len()) {
                difference |= usize::from(
                    left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
                );
            }
            Ok(Value::Bool(difference == 0))
        }
        _ => Err(error(format!("密码模块不支持“{name}”"))),
    }
}
