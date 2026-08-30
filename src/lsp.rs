//! 衡枢语言服务：入口 + 会话调度。
//!
//! 具体功能（定义、重命名、诊断、补全）在 `features` 子模块；JSON-RPC 编解码在 `session`；
//! 文档存储与 UTF-16 位置换算在 `documents`。本文件只把它们拼起来。

mod documents;
mod features;
mod session;

use documents::store_document;
use features::{definition_result, keywords, publish_diagnostics, rename_result, word_at};
use serde_json::{json, Value};
use session::{notify, read_message, respond, respond_error};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

pub fn run() -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    run_session(&mut input, &mut output)
}

fn run_session(input: &mut impl BufRead, output: &mut impl Write) -> Result<(), String> {
    let mut documents = BTreeMap::<String, String>::new();
    loop {
        let Some(message) = read_message(input)? else {
            break;
        };
        if !handle_message(output, &mut documents, &message)? {
            break;
        }
    }
    Ok(())
}

fn handle_message(
    output: &mut impl Write,
    documents: &mut BTreeMap<String, String>,
    message: &Value,
) -> Result<bool, String> {
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let id = message.get("id").cloned();
    match method {
        "initialize" => respond(
            output,
            id,
            json!({
                "capabilities": {
                    "textDocumentSync": 1,
                    "hoverProvider": true,
                    "definitionProvider": true,
                    "renameProvider": true,
                    "completionProvider": {"triggerCharacters": [" ", "的"]}
                },
                "serverInfo": {"name": "衡枢语言服务", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?,
        "shutdown" => respond(output, id, Value::Null)?,
        "exit" => return Ok(false),
        "textDocument/didOpen" => {
            if let Some(document) = message.pointer("/params/textDocument") {
                let uri = document["uri"].as_str().unwrap_or("").to_string();
                let text = document["text"].as_str().unwrap_or("").to_string();
                match store_document(documents, uri.clone(), text.clone()) {
                    Ok(()) => publish_diagnostics(output, &uri, &text)?,
                    Err(error) => notify(
                        output,
                        "window/logMessage",
                        json!({"type": 1, "message": error}),
                    )?,
                }
            }
        }
        "textDocument/didChange" => {
            let uri = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(text) = message
                .pointer("/params/contentChanges/0/text")
                .and_then(Value::as_str)
            {
                match store_document(documents, uri.clone(), text.into()) {
                    Ok(()) => publish_diagnostics(output, &uri, text)?,
                    Err(error) => notify(
                        output,
                        "window/logMessage",
                        json!({"type": 1, "message": error}),
                    )?,
                }
            }
        }
        "textDocument/completion" => respond(
            output,
            id,
            Value::Array(
                keywords()
                    .into_iter()
                    .map(|keyword| json!({"label": keyword, "kind": 14}))
                    .collect(),
            ),
        )?,
        "textDocument/hover" => {
            let result = documents::document_and_position(message, documents)
                .and_then(|(text, line, character)| word_at(text, line, character))
                .map(|word| {
                    json!({
                        "contents": {
                            "kind": "markdown",
                            "value": format!("**{word}**\n\n衡枢中文标识符或关键字")
                        }
                    })
                })
                .unwrap_or(Value::Null);
            respond(output, id, result)?;
        }
        "textDocument/definition" => match definition_result(message, documents) {
            Ok(result) => respond(output, id, result)?,
            Err(error) => respond_error(output, id, -32602, &error)?,
        },
        "textDocument/rename" => match rename_result(message, documents) {
            Ok(result) => respond(output, id, result)?,
            Err(error) => respond_error(output, id, -32602, &error)?,
        },
        _ if id.is_some() => respond(output, id, Value::Null)?,
        _ => {}
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;
    use crate::resource::{DEFAULT_MAX_BUFFER_BYTES, MAX_METADATA_BYTES, MAX_SOURCE_BYTES};
    use documents::{lsp_position, MAX_LSP_DOCUMENTS};
    use features::definition::symbols_at;
    use features::rename::{MAX_LSP_EDITS, MAX_LSP_IDENTIFIER_BYTES};
    use session::write_message;
    use std::io::{BufReader, Cursor};

    #[test]
    fn finds_words_definitions_and_diagnostics() {
        let source = "定义 金额 为 1\n输出(金额)";
        assert_eq!(word_at(source, 1, 4).unwrap(), "金额");
        let uri = "file:///test.代码";
        let documents = BTreeMap::from([(uri.into(), source.into())]);
        let request = json!({
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 4}
            }
        });
        let definition = definition_result(&request, &documents).unwrap();
        assert_eq!(
            definition["range"]["start"],
            json!({"line": 0, "character": 3})
        );
        assert!(parser::parse("定义").is_err());
    }

    #[test]
    fn rename_is_binding_aware_and_ignores_strings_comments_and_shadowing() {
        let uri = "file:///test.代码";
        let source = "定义 值 为 1\n输出(\"值\", 值) 注：值不应修改\n如果 是：\n定义 值 为 2\n输出(值)\n结束\n输出(值)\n";
        let documents = BTreeMap::from([(uri.into(), source.into())]);
        let request = json!({
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 8},
                "newName": "总值"
            }
        });
        let result = rename_result(&request, &documents).unwrap();
        let edits = result["changes"][uri].as_array().unwrap();
        assert_eq!(edits.len(), 3);
        assert_eq!(
            edits
                .iter()
                .filter(|edit| edit["range"]["start"]["line"] == 3)
                .count(),
            0
        );
    }

    #[test]
    fn semantic_index_resolves_types_methods_fields_and_implicit_bindings() {
        let source = "结构 客户：\n姓名\n结束\n定义功能 客户 问候（自己）：\n输出(自己 的 姓名)\n结束\n定义 客户甲 为 客户(姓名 为 \"甲\")，类型限定为 客户\n客户甲 问候()\n匹配 客户甲：\n当 类型是 客户：\n输出(当前值 的 姓名)\n结束";
        let program = parser::parse(source).unwrap();
        assert!(program.symbols.types.iter().any(|ty| ty.name == "客户"));
        assert!(program
            .symbols
            .methods
            .iter()
            .any(|method| method.name == "问候"));
        for kind in [
            crate::ast::SymbolKind::Type,
            crate::ast::SymbolKind::Method,
            crate::ast::SymbolKind::Field,
            crate::ast::SymbolKind::Implicit,
        ] {
            assert!(program
                .symbols
                .occurrences
                .iter()
                .any(|occurrence| occurrence.kind == kind));
        }

        let uri = "file:///semantic.代码";
        let documents = BTreeMap::from([(uri.into(), source.into())]);
        let definition = |line, character| {
            definition_result(
                &json!({
                    "params": {
                        "textDocument": {"uri": uri},
                        "position": {"line": line, "character": character}
                    }
                }),
                &documents,
            )
            .unwrap()
        };
        assert_eq!(definition(6, 28)["range"]["start"]["line"], 0);
        assert_eq!(definition(7, 4)["range"]["start"]["line"], 3);
        assert_eq!(definition(10, 3)["range"]["start"]["line"], 9);
    }

    #[test]
    fn position_lookup_separates_implicit_bindings_from_written_names() {
        let uri = "file:///范围.代码";
        let source = "结构 客户：\n姓名\n结束\n定义 甲 为 客户(姓名 为 \"甲\")，类型限定为 客户\n匹配 甲：\n当 类型是 客户：\n输出(当前值 的 姓名)\n当 记作 乙：\n输出(乙)\n结束";
        let program = parser::parse(source).unwrap();
        for occurrence in program
            .symbols
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.kind == crate::ast::SymbolKind::Implicit)
        {
            assert_eq!(occurrence.span.start, occurrence.span.end);
        }

        let documents = BTreeMap::from([(uri.into(), source.into())]);
        let at = |line, character| {
            symbols_at(&program, source, line, character)
                .into_iter()
                .map(|occurrence| (occurrence.name.clone(), occurrence.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            at(5, 6),
            [("客户".to_string(), crate::ast::SymbolKind::Type)]
        );
        assert_eq!(
            at(7, 5),
            [("乙".to_string(), crate::ast::SymbolKind::Value)]
        );
        // 结构名同时声明构造器绑定和类型，两者共享同一段文本。
        assert_eq!(
            at(0, 3),
            [
                ("客户".to_string(), crate::ast::SymbolKind::Value),
                ("客户".to_string(), crate::ast::SymbolKind::Type),
            ]
        );

        let rename = |line, character, new_name: &str| {
            rename_result(
                &json!({
                    "params": {
                        "textDocument": {"uri": uri},
                        "position": {"line": line, "character": character},
                        "newName": new_name
                    }
                }),
                &documents,
            )
        };
        let edits = rename(0, 3, "买家").unwrap()["changes"][uri]
            .as_array()
            .unwrap()
            .clone();
        let mut ranges = edits
            .iter()
            .map(|edit| {
                (
                    edit["range"]["start"]["line"].as_u64().unwrap(),
                    edit["range"]["start"]["character"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        ranges.sort_unstable();
        assert_eq!(ranges, [(0, 3), (3, 7), (3, 26), (5, 6)]);

        assert!(rename(6, 3, "值").unwrap_err().contains("不能重命名"));
    }

    #[test]
    fn positions_use_utf16_code_units() {
        let text = "输出(\"😀\", 金额)";
        let position = crate::diagnostic::SourcePosition { line: 1, column: 9 };
        assert_eq!(lsp_position(text, position), (0, 9));
    }

    #[test]
    fn json_rpc_messages_round_trip_with_content_length() {
        let message = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &message).unwrap();
        let mut input = BufReader::new(Cursor::new(bytes));
        assert_eq!(read_message(&mut input).unwrap(), Some(message));
    }

    #[test]
    fn lsp_input_and_session_resources_are_bounded() {
        let oversized = format!("Content-Length: {}\r\n\r\n", DEFAULT_MAX_BUFFER_BYTES + 1);
        let mut input = BufReader::new(Cursor::new(oversized));
        assert!(read_message(&mut input).is_err());

        let oversized_header = format!("X-Test: {}\r\n\r\n", "x".repeat(MAX_METADATA_BYTES + 1));
        let mut input = BufReader::new(Cursor::new(oversized_header));
        assert!(read_message(&mut input).is_err());

        let mut documents = (0..MAX_LSP_DOCUMENTS)
            .map(|index| (format!("file:///{index}"), String::new()))
            .collect::<BTreeMap<_, _>>();
        assert!(store_document(&mut documents, "file:///extra".into(), String::new()).is_err());
        assert!(store_document(
            &mut BTreeMap::new(),
            "file:///large".into(),
            "x".repeat(MAX_SOURCE_BYTES + 1),
        )
        .is_err());
    }

    #[test]
    fn rename_limits_identifier_and_edit_count() {
        let uri = "file:///test.代码";
        let source = format!(
            "定义 值 为 1\n输出({})",
            std::iter::repeat_n("值", MAX_LSP_EDITS + 1)
                .collect::<Vec<_>>()
                .join(",")
        );
        let documents = BTreeMap::from([(uri.into(), source)]);
        let request = |new_name: &str| {
            json!({
                "params": {
                    "textDocument": {"uri": uri},
                    "position": {"line": 0, "character": 3},
                    "newName": new_name
                }
            })
        };
        assert!(rename_result(&request("新值"), &documents).is_err());
        assert!(rename_result(
            &request(&"x".repeat(MAX_LSP_IDENTIFIER_BYTES + 1)),
            &documents,
        )
        .is_err());
    }

    #[test]
    fn rename_rejects_same_scope_and_capture_conflicts() {
        let uri = "file:///test.代码";
        let same_scope = "定义 甲 为 1\n定义 乙 为 2\n输出(甲)\n";
        let documents = BTreeMap::from([(uri.into(), same_scope.into())]);
        let request = json!({
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 3},
                "newName": "乙"
            }
        });
        assert!(rename_result(&request, &documents)
            .unwrap_err()
            .contains("同一作用域"));

        let capture = "定义 外层 为 1\n如果 是：\n定义 内层 为 2\n输出(外层)\n结束\n";
        let documents = BTreeMap::from([(uri.into(), capture.into())]);
        let request = json!({
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 3, "character": 3},
                "newName": "内层"
            }
        });
        assert!(rename_result(&request, &documents)
            .unwrap_err()
            .contains("捕获"));
    }

    #[test]
    fn request_errors_are_serialized_without_preventing_later_responses() {
        let mut output = Vec::new();
        respond_error(&mut output, Some(json!(1)), -32602, "无效重命名").unwrap();
        respond(&mut output, Some(json!(2)), json!({"ok": true})).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\"id\":1"));
        assert!(text.contains("\"error\""));
        assert!(text.contains("\"id\":2"));
        assert!(text.contains("\"result\""));

        let uri = "file:///broken.代码";
        let documents = BTreeMap::from([(uri.into(), "定义".into())]);
        let request = json!({
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 0}
            }
        });
        assert_eq!(
            definition_result(&request, &documents).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn document_resource_error_preserves_session_and_existing_document() {
        let uri = "file:///kept.代码";
        let mut documents = (0..MAX_LSP_DOCUMENTS - 1)
            .map(|index| (format!("file:///占位/{index}"), String::new()))
            .collect::<BTreeMap<_, _>>();
        documents.insert(uri.into(), "定义 值 为 1\n输出(值)".into());
        let mut output = Vec::new();
        let rejected = json!({
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///extra.代码", "text": "输出(2)"}}
        });
        assert!(handle_message(&mut output, &mut documents, &rejected).unwrap());
        assert!(!documents.contains_key("file:///extra.代码"));

        let hover = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 4}
            }
        });
        assert!(handle_message(&mut output, &mut documents, &hover).unwrap());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("window/logMessage"));
        assert!(output.contains("\"id\":9"));
        assert!(output.contains("衡枢中文标识符"));
    }
}
