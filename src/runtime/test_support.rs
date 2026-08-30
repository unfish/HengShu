//! 运行时单元测试共用的执行入口。

use super::interpreter::execute;

pub(super) fn run(source: &str) -> Vec<String> {
    execute(source, ".", false).unwrap().output
}
