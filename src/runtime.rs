//! 运行时：解释器状态、语句/表达式求值、原生契约与模块加载。
//!
//! 本文件只做模块编排和跨层共用的错误标记；具体职责见各子模块。

mod access;
mod builtins;
mod calls;
mod contracts;
mod environment;
mod error;
mod eval_expr;
mod eval_stmt;
mod input;
mod interpreter;
mod limits;
mod methods;
pub(crate) mod modules;
mod test_runner;
#[cfg(test)]
mod test_support;
mod vm_bridge;

use contracts::{method_arity, method_named_args, native_arity, native_named_args};
pub(crate) use error::RuntimeError;
use eval_stmt::Completion;
pub(crate) use interpreter::Interpreter;
#[cfg(test)]
pub(crate) use interpreter::{execute, execute_with_options_structured};
pub(crate) use interpreter::{execute_output_with_observation_report, execute_with_args};
pub(crate) use test_runner::{execute_tests_on_execution_stack, list_tests};

/// 只能返回文本错误的层（标准库、表格与统计原生功能）用该标记声明资源类失败，
/// `RuntimeError::new` 会把它还原成“资源错误”类别，标记本身不会出现在消息里。
pub(crate) const RESOURCE_ERROR_MARK: &str = "\u{1}资源\u{1}";
pub(crate) const CANCELLED_ERROR_MARK: &str = "\u{1}取消\u{1}";
pub(crate) const TIMED_OUT_ERROR_MARK: &str = "\u{1}超时\u{1}";

/// 把文本错误标注为资源类失败。
pub(crate) fn resource_error(message: impl AsRef<str>) -> String {
    format!("{RESOURCE_ERROR_MARK}{}", message.as_ref())
}

pub(crate) fn cancelled_error(message: impl AsRef<str>) -> String {
    format!("{CANCELLED_ERROR_MARK}{}", message.as_ref())
}

pub(crate) fn timed_out_error(message: impl AsRef<str>) -> String {
    format!("{TIMED_OUT_ERROR_MARK}{}", message.as_ref())
}
