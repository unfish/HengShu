//! 完成矩阵：正常、返回、跳出、继续和抛出穿过块、循环、任务组及一至多层`try/catch/finally`。

mod common;

use common::{assert_three_paths_failure, assert_three_paths_output, ExecutionStatus};

#[test]
fn completion_matrix_normal_through_nested_try_catch_finally() {
    let output = assert_three_paths_output(
        r#"尝试：
    尝试：
        输出("正文")
    捕获 错误：
        输出("内层捕获")
    最终：
        输出("内层最终")
    结束
最终：
    输出("外层最终")
结束
输出("结束")"#,
    );
    assert_eq!(output, ["正文", "内层最终", "外层最终", "结束"]);
}

#[test]
fn completion_matrix_return_through_try_catch_finally() {
    let output = assert_three_paths_output(
        r#"定义功能 载体（）：
    尝试：
        尝试：
            输出("正文")
            返回 "值"
        捕获 错误：
            输出("内层捕获")
        最终：
            输出("内层最终")
        结束
        输出("不可达")
    最终：
        输出("外层最终")
    结束
    输出("也不可达")
结束
输出(载体())"#,
    );
    assert_eq!(output, ["正文", "内层最终", "外层最终", "值"]);
}

#[test]
fn completion_matrix_break_through_try_finally() {
    let output = assert_three_paths_output(
        r#"遍历 [1, 2, 3]，记作 数：
    尝试：
        如果 数 = 2：
            跳出循环
        结束
        输出("循环", 数)
    最终：
        输出("最终", 数)
    结束
结束
输出("结束")"#,
    );
    assert_eq!(output, ["循环 1", "最终 1", "最终 2", "结束"]);
}

#[test]
fn completion_matrix_continue_through_try_finally() {
    let output = assert_three_paths_output(
        r#"遍历 [1, 2, 3]，记作 数：
    尝试：
        如果 数 = 2：
            继续下一轮
        结束
        输出("循环", 数)
    最终：
        输出("最终", 数)
    结束
结束
输出("结束")"#,
    );
    assert_eq!(
        output,
        ["循环 1", "最终 1", "最终 2", "循环 3", "最终 3", "结束"]
    );
}

#[test]
fn completion_matrix_throw_through_nested_try_catch_finally() {
    let output = assert_three_paths_output(
        r#"尝试：
    尝试：
        抛出 "内错"
    最终：
        输出("内层最终")
    结束
捕获 错误：
    输出("外层捕获", 错误)
最终：
    输出("外层最终")
结束
输出("结束")"#,
    );
    assert_eq!(
        output,
        ["内层最终", "外层捕获 运行错误：内错", "外层最终", "结束"]
    );
}

#[test]
fn completion_matrix_finally_overrides_return_with_throw() {
    let outcome = assert_three_paths_failure(
        r#"定义功能 载体（）：
    尝试：
        返回 42
    最终：
        抛出 "覆盖"
    结束
结束
载体()"#,
    );
    assert_eq!(outcome.status, ExecutionStatus::Failure);
    assert!(outcome.message().contains("覆盖"));
}

#[test]
fn completion_matrix_finally_overrides_throw_with_return() {
    let output = assert_three_paths_output(
        r#"定义功能 载体（）：
    尝试：
        抛出 "原错"
    最终：
        返回 "覆盖值"
    结束
结束
输出(载体())"#,
    );
    assert_eq!(output, ["覆盖值"]);
}

#[test]
fn completion_matrix_task_group_break_and_continue_are_contained() {
    let output = assert_three_paths_output(
        r#"引入 并发
遍历 [1, 2, 3]，记作 数：
    任务组：
        定义 任务 为 并发 启动(定义功能（）：
            返回 数
        结束)
        输出("任务组", 任务 等待())
    结束
    如果 数 = 2：
        跳出循环
    结束
结束
输出("结束")"#,
    );
    assert_eq!(output, ["任务组 1", "任务组 2", "结束"]);
}

#[test]
fn completion_matrix_while_loop_break_and_continue() {
    let output = assert_three_paths_output(
        r#"定义 数 为 0
当 数 < 10 时重复：
    数 为 数 + 1
    如果 数 = 2：
        继续下一轮
    结束
    如果 数 = 5：
        跳出循环
    结束
    输出(数)
结束
输出("结束")"#,
    );
    assert_eq!(output, ["1", "3", "4", "结束"]);
}

#[test]
fn completion_matrix_return_across_multiple_finally_layers() {
    let output = assert_three_paths_output(
        r#"定义功能 载体（）：
    尝试：
        尝试：
            尝试：
                返回 "深层"
            最终：
                输出("最终一")
            结束
        最终：
            输出("最终二")
        结束
    最终：
        输出("最终三")
    结束
结束
输出(载体())"#,
    );
    assert_eq!(output, ["最终一", "最终二", "最终三", "深层"]);
}

#[test]
fn completion_matrix_throw_caught_after_finally() {
    let output = assert_three_paths_output(
        r#"尝试：
    尝试：
        抛出 "甲"
    最终：
        输出("最终")
    结束
捕获 错误：
    输出("捕获", 错误)
结束
输出("结束")"#,
    );
    assert_eq!(output, ["最终", "捕获 运行错误：甲", "结束"]);
}

#[test]
fn completion_matrix_break_and_continue_issued_from_inside_a_finally() {
    let output = assert_three_paths_output(
        r#"遍历 [1, 2]，记作 项：
    尝试：
        输出("正文", 项)
    最终：
        跳出循环
    结束
结束
遍历 [1, 2]，记作 项：
    尝试：
        输出("再来", 项)
    最终：
        继续下一轮
    结束
结束
输出("结束")"#,
    );
    assert_eq!(output, ["正文 1", "再来 1", "再来 2", "结束"]);
}

#[test]
fn completion_matrix_break_leaves_a_loop_nested_inside_an_outer_finally() {
    let output = assert_three_paths_output(
        r#"尝试：
    输出("外层正文")
最终：
    遍历 [1, 2]，记作 项：
        尝试：
            输出("内层正文", 项)
        最终：
            跳出循环
        结束
    结束
    输出("外层最终")
结束
输出("结束")"#,
    );
    assert_eq!(output, ["外层正文", "内层正文 1", "外层最终", "结束"]);
}

#[test]
fn completion_matrix_continue_leaves_a_loop_nested_inside_an_outer_finally() {
    let output = assert_three_paths_output(
        r#"尝试：
    输出("外层正文")
最终：
    遍历 [1, 2]，记作 项：
        尝试：
            输出("内层正文", 项)
        最终：
            继续下一轮
        结束
    结束
    输出("外层最终")
结束
输出("结束")"#,
    );
    assert_eq!(
        output,
        ["外层正文", "内层正文 1", "内层正文 2", "外层最终", "结束"]
    );
}
