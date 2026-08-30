//! 三路径语义门禁的基础组合用例。语句、完成、绑定、类型、调用、值、模块和并发矩阵
//! 分别位于 `tests/*_matrix.rs`，共用 `tests/common` 中的同一差分工具。

mod common;

use common::{assert_three_paths, assert_three_paths_output, ExecutionStatus};

#[test]
fn three_path_gate_compares_successful_semantic_composition() {
    let source = r#"
引入 并发
定义 外层 为 1
如果 是：
    定义 外层 为 2
    输出(外层)
结束
输出(外层)
定义功能 计算（值，增量 为 3）：
    返回 值 + 增量
结束
输出(计算(4))
结构 客户：
姓名
结束
定义功能 客户 标签（自己）：
    返回 自己 的 姓名
结束
定义 对象 为 客户(姓名 为 "甲")
输出(对象 标签(), 类型是(对象, "客户"))
尝试：
    输出("正文")
最终：
    输出("清理")
结束
定义功能 原样（值）：
    返回 值
结束
定义 结果 为 并发 并行映射([集合([1, 2])], 原样)
输出(类型是(结果[1], "集合"))
"#;
    assert_eq!(
        assert_three_paths_output(source),
        ["2", "1", "7", "甲 是", "正文", "清理", "是"]
    );
}

#[test]
fn three_path_gate_compares_structured_runtime_failures() {
    let outcome = assert_three_paths("定义 值 为 [1]\n输出(值[0])");
    assert_eq!(outcome.status, ExecutionStatus::Failure);
    assert_eq!(outcome.error_category(), Some("运行错误"));
    assert_eq!(outcome.line(), Some(2));
    assert!(outcome.message().contains("索引必须从 1 开始"));
}

#[test]
fn three_path_gate_gives_statement_context_failures_the_statement_line() {
    let outcome = assert_three_paths("输出(\"甲\")\n要求 否，否则说明 \"条件失败\"");
    assert_eq!(outcome.status, ExecutionStatus::Failure);
    assert_eq!(outcome.line(), Some(2));
    assert_eq!(outcome.message(), "条件失败");
}

/// 门禁自检：失败前的输出、调用栈和退出码都必须在三条路径上真实采集并逐一比较。
/// 这三个维度若形同虚设，打包路径的输出被硬编码为空、调用栈丢失都不会被发现。
#[test]
fn three_path_gate_compares_output_stack_and_exit_code_of_failures() {
    let outcome = assert_three_paths(
        r#"输出("先打印")
定义功能 内层（）：
    抛出 "深处出错"
结束
定义功能 外层（）：
    返回 内层()
结束
输出(外层())"#,
    );
    assert_eq!(outcome.status, ExecutionStatus::Failure);
    assert_eq!(outcome.output, ["先打印"]);
    assert_eq!(outcome.exit_code, 1);
    assert_eq!(outcome.stack(), ["外层", "内层"]);
    assert_eq!(outcome.line(), Some(3));
    assert_eq!(outcome.message(), "深处出错");
}
