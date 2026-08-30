//! 定义查找与光标处标识符提取：Hover 与 Go-to-Definition 共用。

use crate::ast::{Program, SymbolOccurrence};
use crate::lsp::documents::{document_and_position, lsp_position, scalar_column};
use crate::parser;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(in crate::lsp) fn word_at(text: &str, line: usize, character: usize) -> Option<String> {
    let line = text.lines().nth(line)?;
    let characters = line.chars().collect::<Vec<_>>();
    let scalar_character = scalar_column(line, character);
    let mut start = scalar_character.min(characters.len());
    while start > 0 && is_word(characters[start - 1]) {
        start -= 1;
    }
    let mut end = scalar_character.min(characters.len());
    while end < characters.len() && is_word(characters[end]) {
        end += 1;
    }
    (start < end).then(|| characters[start..end].iter().collect())
}

pub(in crate::lsp) fn definition_result(
    message: &Value,
    documents: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let Some((text, line, character)) = document_and_position(message, documents) else {
        return Ok(Value::Null);
    };
    let Ok(program) = crate::resource::on_compile_stack(|| Ok(parser::parse_diagnostic(text)))?
    else {
        return Ok(Value::Null);
    };
    let Some(symbol) = symbols_at(&program, text, line, character).first().copied() else {
        return Ok(Value::Null);
    };
    let Some(declaration) = program
        .symbols
        .occurrences
        .iter()
        .find(|candidate| candidate.identity == symbol.identity && candidate.declaration)
    else {
        return Ok(Value::Null);
    };
    let start = lsp_position(text, declaration.span.start);
    let end = lsp_position(text, declaration.span.end);
    let uri = message
        .pointer("/params/textDocument/uri")
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({
        "uri": uri,
        "range": {
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1}
        }
    }))
}

/// 同一段源码文本可以同时是多个语义实体的名字：结构名既声明构造器绑定也声明类型。
/// 因此按光标位置查找返回该位置上的全部实体，由调用方决定如何取舍，
/// 而不是取先登记的那一个。空范围属于没有源码文本的隐式声明，永远不会被选中。
pub(in crate::lsp) fn symbols_at<'a>(
    program: &'a Program,
    text: &str,
    line: usize,
    utf16_character: usize,
) -> Vec<&'a SymbolOccurrence> {
    let Some(source_line) = text.lines().nth(line) else {
        return Vec::new();
    };
    let column = scalar_column(source_line, utf16_character) + 1;
    program
        .symbols
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.span.start.line == line + 1
                && occurrence.span.start.column <= column
                && column < occurrence.span.end.column
        })
        .collect()
}

fn is_word(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}
