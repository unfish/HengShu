//! 衡枢中文财务与数据分析语言的嵌入式接口。
//!
//! 当前未发布版本不承诺 API 或字节码向下兼容性。
//! AST、解释器环境、运行时值和虚拟机均属于内部实现细节。

mod analysis;
mod ast;
mod bytecode;
mod checker;
mod compiler;
mod concurrency;
mod contracts;
mod diagnostic;
mod dotenv;
mod finance;
mod formatter;
mod lexer;
mod lsp;
mod ooxml;
mod pack;
mod parser;
mod project;
mod release;
mod report;
mod resource;
mod runtime;
mod security;
mod stdlib;
mod stream;
mod symbols;
mod table;
mod value;
mod vm;

pub mod api;
pub mod automation;
pub mod batch;
pub mod cli;
pub mod delivery;
pub mod delivery_template;
pub mod desktop;
pub mod evidence;
pub mod execution;
pub mod ingest;
pub mod privacy;
pub mod provenance;
pub mod reconciliation;
pub mod smtp;
pub mod template;
pub mod units;

pub use security::PermissionSet;

#[cfg(test)]
mod property_tests;
