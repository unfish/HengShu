//! JSON-RPC 传输层：读消息、写消息、错误响应，与具体功能解耦。
//!
//! 会话本身不理解定义、重命名、诊断这些概念；它只做协议编解码和分发。

use crate::resource::{DEFAULT_MAX_BUFFER_BYTES, MAX_METADATA_BYTES};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub(in crate::lsp) fn read_message(input: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut length = None;
    let mut header_bytes = 0_usize;
    loop {
        let Some(line) = crate::resource::read_line_limited(input, MAX_METADATA_BYTES)
            .map_err(|e| format!("LSP 消息头无效：{e}"))?
        else {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err("LSP 消息头被截断".into())
            };
        };
        header_bytes = header_bytes
            .checked_add(line.len())
            .filter(|total| *total <= MAX_METADATA_BYTES)
            .ok_or_else(|| "LSP 消息头超过大小限制".to_string())?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| "LSP 消息缺少 Content-Length".to_string())?;
    if length > DEFAULT_MAX_BUFFER_BYTES {
        return Err(format!(
            "LSP 消息超过 {DEFAULT_MAX_BUFFER_BYTES} 字节缓冲上限"
        ));
    }
    let mut body = Vec::new();
    body.try_reserve_exact(length)
        .map_err(|_| "无法为 LSP 消息分配内存".to_string())?;
    body.resize(length, 0);
    input.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub(in crate::lsp) fn respond(
    output: &mut impl Write,
    id: Option<Value>,
    result: Value,
) -> Result<(), String> {
    write_message(
        output,
        &json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result}),
    )
}

pub(in crate::lsp) fn respond_error(
    output: &mut impl Write,
    id: Option<Value>,
    code: i32,
    message: &str,
) -> Result<(), String> {
    write_message(
        output,
        &json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "error": {"code": code, "message": message}
        }),
    )
}

pub(in crate::lsp) fn notify(
    output: &mut impl Write,
    method: &str,
    params: Value,
) -> Result<(), String> {
    write_message(
        output,
        &json!({"jsonrpc": "2.0", "method": method, "params": params}),
    )
}

pub(in crate::lsp) fn write_message(output: &mut impl Write, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
    output.write_all(&body).map_err(|e| e.to_string())?;
    output.flush().map_err(|e| e.to_string())
}
