//! 重命名：找到全部引用、检查捕获与冲突、返回工作区编辑。

use super::definition::symbols_at;
use crate::ast::{SymbolId, SymbolIndex};
use crate::lsp::documents::{document_and_position, lsp_position};
use crate::parser;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(in crate::lsp) const MAX_LSP_EDITS: usize = 10_000;
pub(in crate::lsp) const MAX_LSP_IDENTIFIER_BYTES: usize = 256;

pub(in crate::lsp) fn rename_result(
    message: &Value,
    documents: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let Some((text, line, character)) = document_and_position(message, documents) else {
        return Ok(Value::Null);
    };
    let Some(new_name) = message.pointer("/params/newName").and_then(Value::as_str) else {
        return Ok(Value::Null);
    };
    if new_name.len() > MAX_LSP_IDENTIFIER_BYTES {
        return Err(format!(
            "重命名标识符超过 {MAX_LSP_IDENTIFIER_BYTES} 字节限制"
        ));
    }
    validate_identifier(new_name)?;
    let Some(uri) = message
        .pointer("/params/textDocument/uri")
        .and_then(Value::as_str)
    else {
        return Ok(Value::Null);
    };
    let program = crate::resource::on_compile_stack(|| Ok(parser::parse_diagnostic(text)))?
        .map_err(|error| error.to_string())?;
    let facets = symbols_at(&program, text, line, character);
    if facets.is_empty() {
        return Ok(Value::Null);
    }
    let identities = facets
        .iter()
        .map(|facet| facet.identity)
        .collect::<std::collections::BTreeSet<_>>();
    for identity in &identities {
        ensure_rename_is_safe(&program.symbols, *identity, new_name)?;
    }
    let mut spans = program
        .symbols
        .occurrences
        .iter()
        .filter(|candidate| identities.contains(&candidate.identity))
        .map(|candidate| candidate.span)
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans.dedup();
    if spans.len() > MAX_LSP_EDITS {
        return Err(format!("重命名匹配超过 {MAX_LSP_EDITS} 项限制"));
    }
    let edits = spans
        .into_iter()
        .map(|span| {
            let start = lsp_position(text, span.start);
            let end = lsp_position(text, span.end);
            json!({
                "range": {
                    "start": {"line": start.0, "character": start.1},
                    "end": {"line": end.0, "character": end.1}
                },
                "newText": new_name
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"changes": {uri: edits}}))
}

fn ensure_rename_is_safe(
    index: &SymbolIndex,
    identity: SymbolId,
    new_name: &str,
) -> Result<(), String> {
    let declaration = index
        .occurrences
        .iter()
        .find(|occurrence| occurrence.identity == identity && occurrence.declaration)
        .ok_or_else(|| "找不到待重命名标识符的声明".to_string())?;
    if declaration.span.start == declaration.span.end {
        return Err(format!(
            "“{}”是语言隐式提供的绑定，不能重命名",
            declaration.name
        ));
    }
    if index.occurrences.iter().any(|occurrence| {
        occurrence.declaration
            && occurrence.identity != identity
            && occurrence.scope == declaration.scope
            && occurrence.name == new_name
    }) {
        return Err(format!("重命名为“{new_name}”会与同一作用域中的声明冲突"));
    }
    let parents = index
        .scopes
        .iter()
        .map(|scope| (scope.id, scope.parent))
        .collect::<BTreeMap<_, _>>();
    for reference in index
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.identity == identity && !occurrence.declaration)
    {
        let mut scope = Some(reference.scope);
        while let Some(current) = scope {
            if current == declaration.scope {
                break;
            }
            if index.occurrences.iter().any(|occurrence| {
                occurrence.declaration
                    && occurrence.identity != identity
                    && occurrence.scope == current
                    && occurrence.name == new_name
            }) {
                return Err(format!("重命名为“{new_name}”会使引用被内层声明捕获"));
            }
            scope = parents.get(&current).copied().flatten();
        }
    }
    Ok(())
}

fn validate_identifier(name: &str) -> Result<(), String> {
    let tokens = crate::lexer::lex_diagnostic(name).map_err(|error| error.to_string())?;
    let words = tokens
        .iter()
        .filter(|token| !matches!(token.kind, crate::lexer::TokenKind::Eof))
        .collect::<Vec<_>>();
    if words.len() != 1
        || !matches!(&words[0].kind, crate::lexer::TokenKind::Word(value) if value == name)
        || parser::is_keyword(name)
    {
        return Err(format!("“{name}”不是有效的衡枢标识符"));
    }
    Ok(())
}
