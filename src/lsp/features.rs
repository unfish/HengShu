//! LSP 功能模块入口。

mod completion;
pub(in crate::lsp) mod definition;
mod diagnostics;
pub(in crate::lsp) mod rename;

pub(in crate::lsp) use completion::keywords;
pub(in crate::lsp) use definition::{definition_result, word_at};
pub(in crate::lsp) use diagnostics::publish as publish_diagnostics;
pub(in crate::lsp) use rename::rename_result;
