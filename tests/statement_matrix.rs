//! 语句矩阵：每种语句分别嵌套于顶层、具名功能、匿名功能、条件两个分支、两类循环、
//! 尝试、捕获、最终、匹配分支、匹配兜底、任务组和模块顶层，三条执行路径必须给出同一序列。

mod common;

use common::{
    assert_three_paths_failure, assert_three_paths_output, assert_three_paths_with_modules,
    ExecutionStatus,
};

/// 覆盖定义、赋值、表达式、条件、条件循环、遍历、功能声明、返回、匿名功能、结构声明、
/// 尝试/捕获/最终、抛出、要求、匹配、任务组、跳出、继续和全局语句的可嵌套正文。
const STATEMENT_BODY: &str = r#"定义 甲 为 1，类型限定为 数字
输出("定义", 甲)
甲 为 2
定义 表 为 [10, 20]
定义 录 为 {键: 1}
表[1] 为 11
录 的 键 为 2
输出("赋值", 甲, 表[1], 录 的 键)
输出("表达式")
如果 甲 = 1：
    输出("如果一")
否则，如果 甲 = 2：
    输出("如果二")
否则：
    输出("如果三")
结束
定义 数 为 0
当 数 < 4 时重复：
    数 为 数 + 1
    如果 数 = 2：
        继续下一轮
    结束
    如果 数 = 3：
        跳出循环
    结束
    输出("当", 数)
结束
遍历 [1, 2]，记作 项：
    输出("遍历", 项)
结束
定义功能 加一（值 为 5）：
    返回 值 + 1
结束
输出("功能", 加一(), 加一(7))
定义 匿名 为 定义功能（值）：
    返回 值 * 2
结束
输出("匿名", 匿名(4))
结构 点：
    横
结束
输出("结构", 点(横 为 3) 的 横)
尝试：
    抛出 "错"
捕获 错误：
    输出("捕获", 错误)
最终：
    输出("最终")
结束
要求 甲 = 2，否则说明 "甲应为二"
输出("要求")
匹配 甲：
    当 1：
        输出("匹配值")
    当 类型是 文本：
        输出("匹配类型")
    当 记作 别名，并且满足 别名 = 2：
        输出("匹配记作", 别名, 当前值)
    其他：
        输出("匹配兜底")
结束
匹配 "无"：
    当 1：
        输出("兜底前")
    其他：
        输出("匹配兜底")
结束
任务组：
    定义 任务 为 并发 启动(定义功能（）：
        返回 9
    结束)
    输出("任务组", 任务 等待())
结束
定义功能 改根（）：
    全局 根标记
    根标记 为 根标记 + 1
结束
改根()
输出("全局", 根标记)"#;

const EXPECTED_OUTPUT: &[&str] = &[
    "定义 1",
    "赋值 2 11 2",
    "表达式",
    "如果二",
    "当 1",
    "遍历 1",
    "遍历 2",
    "功能 6 8",
    "匿名 8",
    "结构 3",
    "捕获 运行错误：错",
    "最终",
    "要求",
    "匹配记作 2 2",
    "匹配兜底",
    "任务组 9",
    "全局 1",
];

/// 正文引用的并发模块与根绑定必须在每个宿主作用域的顶层可用。
const PRELUDE: &str = "引入 并发\n定义 根标记 为 0\n";

fn indented_body() -> String {
    STATEMENT_BODY
        .lines()
        .map(|line| format!("    {line}\n"))
        .collect()
}

/// 把语句正文嵌入指定上下文，并要求三条路径给出同一输出序列。
fn assert_statements_in_context(prefix: &str, suffix: &str) {
    let source = format!("{PRELUDE}{prefix}\n{}{suffix}\n", indented_body());
    assert_eq!(
        assert_three_paths_output(&source),
        EXPECTED_OUTPUT,
        "{source}"
    );
}

#[test]
fn statement_matrix_at_top_level() {
    let source = format!("{PRELUDE}{STATEMENT_BODY}\n");
    assert_eq!(assert_three_paths_output(&source), EXPECTED_OUTPUT);
}

#[test]
fn statement_matrix_in_named_function() {
    assert_statements_in_context("定义功能 载体（）：", "结束\n载体()");
}

#[test]
fn statement_matrix_in_anonymous_function() {
    assert_statements_in_context("定义 载体 为 定义功能（）：", "结束\n载体()");
}

#[test]
fn statement_matrix_in_condition_branch() {
    assert_statements_in_context("如果 是：", "结束");
}

#[test]
fn statement_matrix_in_condition_else_branch() {
    assert_statements_in_context("如果 否：\n    定义 空转 为 0\n否则：", "结束");
}

#[test]
fn statement_matrix_in_while_loop() {
    assert_statements_in_context("定义 一次 为 是\n当 一次 时重复：\n    一次 为 否", "结束");
}

#[test]
fn statement_matrix_in_for_loop() {
    assert_statements_in_context("遍历 [1]，记作 轮次：", "结束");
}

#[test]
fn statement_matrix_in_try_body() {
    assert_statements_in_context("尝试：", "最终：\n    定义 收尾 为 0\n结束");
}

#[test]
fn statement_matrix_in_catch_branch() {
    assert_statements_in_context("尝试：\n    抛出 \"触发\"\n捕获 外层错误：", "结束");
}

#[test]
fn statement_matrix_in_finally_branch() {
    assert_statements_in_context("尝试：\n    定义 空转 为 0\n最终：", "结束");
}

#[test]
fn statement_matrix_in_match_branch() {
    assert_statements_in_context("匹配 1：\n    当 1：", "结束");
}

#[test]
fn statement_matrix_in_match_fallback() {
    assert_statements_in_context(
        "匹配 \"无\"：\n    当 1：\n        定义 空转 为 0\n    其他：",
        "结束",
    );
}

#[test]
fn statement_matrix_in_task_group() {
    assert_statements_in_context("任务组：", "结束");
}

#[test]
fn statement_matrix_in_module_top_level() {
    let module = format!("{PRELUDE}{STATEMENT_BODY}\n");
    let outcome = assert_three_paths_with_modules(
        "引入 语句载体模块\n",
        &[("语句载体模块", module.as_str())],
    );
    assert_eq!(outcome.status, ExecutionStatus::Success);
    assert_eq!(outcome.output, EXPECTED_OUTPUT);
}

#[test]
fn statement_matrix_failed_requirement_reports_its_own_line() {
    let outcome = assert_three_paths_failure(
        r#"定义 甲 为 1
要求 甲 == 2"#,
    );
    assert_eq!(outcome.line(), Some(2));
    assert_eq!(outcome.error_category(), Some("运行错误"));
}
