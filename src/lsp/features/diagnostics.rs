//! 诊断：把解析/检查错误发布为 LSP 诊断。

use crate::lsp::documents::lsp_position;
use crate::lsp::session::notify;
use crate::{checker, parser};
use serde_json::json;
use std::io::Write;

pub(in crate::lsp) fn publish(
    output: &mut impl Write,
    uri: &str,
    text: &str,
) -> Result<(), String> {
    let diagnostic = crate::resource::on_compile_stack(|| {
        Ok(parser::parse_diagnostic(text)
            .and_then(|program| checker::check_diagnostic(&program))
            .err())
    })?
    .map(|error| {
        let start = lsp_position(text, error.span.start);
        let end = lsp_position(text, error.span.end);
        json!({
            "range": {
                "start": {"line": start.0, "character": start.1},
                "end": {"line": end.0, "character": end.1}
            },
            "severity": 1,
            "source": "衡枢",
            "message": format!("{}：{}", error.stage.category(), error.message)
        })
    });
    notify(
        output,
        "textDocument/publishDiagnostics",
        json!({"uri": uri, "diagnostics": diagnostic.into_iter().collect::<Vec<_>>() }),
    )
}
