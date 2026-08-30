//! 绑定矩阵：同名遮蔽、前后声明、全局、参数、循环变量、捕获变量、匹配绑定、
//! 编译器临时和模块同名导入的三路径一致性。

mod common;

use common::{assert_three_paths_output, assert_three_paths_with_modules, ExecutionStatus};

#[test]
fn binding_matrix_shadow_in_nested_blocks() {
    let output = assert_three_paths_output(
        r#"定义 甲 为 1
如果 是：
    定义 甲 为 2
    输出(甲)
结束
输出(甲)"#,
    );
    assert_eq!(output, ["2", "1"]);
}

#[test]
fn binding_matrix_shadow_in_for_loop() {
    let output = assert_three_paths_output(
        r#"定义 甲 为 0
遍历 [1, 2, 3]，记作 甲：
    输出(甲)
结束
输出(甲)"#,
    );
    assert_eq!(output, ["1", "2", "3", "0"]);
}

#[test]
fn binding_matrix_parameter_shadows_outer() {
    let output = assert_three_paths_output(
        r#"定义 值 为 10
定义功能 加工（值）：
    返回 值 + 1
结束
输出(加工(5), 值)"#,
    );
    assert_eq!(output, ["6 10"]);
}

#[test]
fn binding_matrix_closure_captures_outer_binding() {
    let output = assert_three_paths_output(
        r#"定义 外 为 100
定义 闭包 为 定义功能（）：
    返回 外 + 1
结束
外 为 200
输出(闭包())"#,
    );
    assert_eq!(output, ["201"]);
}

#[test]
fn binding_matrix_catch_variable_scoped_to_catch_block() {
    let output = assert_three_paths_output(
        r#"定义 外部 为 "外部"
尝试：
    抛出 "内错"
捕获 错误：
    输出(错误)
结束
输出(外部)"#,
    );
    assert_eq!(output, ["运行错误：内错", "外部"]);
}

#[test]
fn binding_matrix_match_implicit_binding() {
    let output = assert_three_paths_output(
        r#"匹配 42：
    当 记作 别名，并且满足 别名 > 0：
        输出(当前值, 别名)
    其他：
        输出("兜底")
结束"#,
    );
    assert_eq!(output, ["42 42"]);
}

#[test]
fn binding_matrix_keeps_global_declarations_module_scoped() {
    let module_source = r#"公开 定义 根值 为 0
定义功能 修改（）：
    全局 根值
    根值 为 根值 + 1
结束
修改()
修改()
公开 定义 导出值 为 根值"#;
    let outcome = assert_three_paths_with_modules(
        "从 绑定模块 引入 导出值\n输出(导出值)\n",
        &[("绑定模块", module_source)],
    );
    assert_eq!(outcome.status, ExecutionStatus::Success);
    assert_eq!(outcome.output, ["2"]);
}

#[test]
fn binding_matrix_default_parameter_reads_prior_parameters() {
    let output = assert_three_paths_output(
        r#"定义功能 加倍（基础，倍数 为 基础 * 2）：
    返回 倍数
结束
输出(加倍(3), 加倍(3, 倍数 为 10))"#,
    );
    assert_eq!(output, ["6 10"]);
}

#[test]
fn binding_matrix_while_loop_variable_mutated_per_iteration() {
    let output = assert_three_paths_output(
        r#"定义 计 为 0
当 计 < 5 时重复：
    计 为 计 + 1
结束
输出(计)"#,
    );
    assert_eq!(output, ["5"]);
}

#[test]
fn binding_matrix_module_selective_import_does_not_shadow_local() {
    let module_source = "公开 定义 甲 为 100\n公开 定义 乙 为 200\n";
    let outcome = assert_three_paths_with_modules(
        r#"定义 甲 为 1
从 选导模块 引入 乙
输出(甲, 乙)
"#,
        &[("选导模块", module_source)],
    );
    assert_eq!(outcome.status, ExecutionStatus::Success);
    assert_eq!(outcome.output, ["1 200"]);
}
