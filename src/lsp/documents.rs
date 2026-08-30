//! 文档存储与位置解析：LSP 消息里的 UTF-16 坐标到源码字符位置的换算。

use crate::diagnostic::SourcePosition;
use crate::resource::{DEFAULT_MAX_BUFFER_BYTES, MAX_SOURCE_BYTES};
use serde_json::Value;
use std::collections::BTreeMap;

pub(in crate::lsp) const MAX_LSP_DOCUMENTS: usize = 1024;

pub(in crate::lsp) fn store_document(
    documents: &mut BTreeMap<String, String>,
    uri: String,
    text: String,
) -> Result<(), String> {
    if text.len() > MAX_SOURCE_BYTES {
        return Err(format!("LSP 文档超过 {MAX_SOURCE_BYTES} 字节源码上限"));
    }
    if !documents.contains_key(&uri) && documents.len() >= MAX_LSP_DOCUMENTS {
        return Err(format!("LSP 会话文档超过 {MAX_LSP_DOCUMENTS} 项限制"));
    }
    let previous = documents.get(&uri).map_or(0, String::len);
    let total = documents
        .values()
        .try_fold(0_usize, |total, value| total.checked_add(value.len()))
        .ok_or_else(|| "LSP 会话文档总大小溢出".to_string())?
        .checked_sub(previous)
        .and_then(|total| total.checked_add(text.len()))
        .filter(|total| *total <= DEFAULT_MAX_BUFFER_BYTES)
        .ok_or_else(|| "LSP 会话文档超过总缓冲上限".to_string())?;
    debug_assert!(total <= DEFAULT_MAX_BUFFER_BYTES);
    documents.insert(uri, text);
    Ok(())
}

pub(in crate::lsp) fn document_and_position<'a>(
    message: &Value,
    documents: &'a BTreeMap<String, String>,
) -> Option<(&'a str, usize, usize)> {
    let uri = message
        .pointer("/params/textDocument/uri")
        .and_then(Value::as_str)?;
    let line = message
        .pointer("/params/position/line")
        .and_then(Value::as_u64)? as usize;
    let character = message
        .pointer("/params/position/character")
        .and_then(Value::as_u64)? as usize;
    Some((documents.get(uri)?, line, character))
}

pub(in crate::lsp) fn lsp_position(text: &str, position: SourcePosition) -> (usize, usize) {
    let line_index = position.line.saturating_sub(1);
    let character = text
        .lines()
        .nth(line_index)
        .map(|line| {
            line.chars()
                .take(position.column.saturating_sub(1))
                .map(char::len_utf16)
                .sum()
        })
        .unwrap_or(0);
    (line_index, character)
}

pub(in crate::lsp) fn scalar_column(line: &str, utf16_character: usize) -> usize {
    let mut utf16 = 0;
    let mut scalar = 0;
    for character in line.chars() {
        if utf16 >= utf16_character {
            break;
        }
        utf16 += character.len_utf16();
        scalar += 1;
    }
    scalar
}
