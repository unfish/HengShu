//! 类型矩阵：每个内建类型、抽象类型、具体结构、同名结构、跨模块结构分别验证
//! `类型是`、约束、要求、匹配和赋值的三路径一致性。

mod common;

use common::{assert_three_paths_failure, assert_three_paths_output};

#[test]
fn type_matrix_builtin_type_checks() {
    let output = assert_three_paths_output(
        r#"引入 数学
输出(类型是(1, "数字"))
输出(类型是(数学 平方根(2.0), "浮点数"))
输出(类型是("甲", "文本"))
输出(类型是(是, "逻辑"))
输出(类型是(空, "空"))
输出(类型是([1], "列表"))
输出(类型是({键: 1}, "记录"))
输出(类型是(集合([1, 2]), "集合"))"#,
    );
    assert_eq!(output, ["是", "是", "是", "是", "是", "是", "是", "是"]);
}

#[test]
fn type_matrix_any_accepts_all() {
    let output = assert_three_paths_output(
        r#"定义 甲 为 1，类型限定为 任意
甲 为 "文本"
输出(甲)
甲 为 [1, 2]
输出(甲)"#,
    );
    assert_eq!(output, ["文本", "[1, 2]"]);
}

#[test]
fn type_matrix_struct_instance_abstract_type() {
    let output = assert_three_paths_output(
        r#"结构 点：
    横
结束
定义 位 为 点(横 为 1)
输出(类型是(位, "结构实例"))
输出(类型是(位, "点"))
输出(类型是(1, "结构实例"))"#,
    );
    assert_eq!(output, ["是", "是", "否"]);
}

#[test]
fn type_matrix_struct_instance_constrains_bindings_and_requirements() {
    let output = assert_three_paths_output(
        r#"结构 点：
    横
结束
定义 位 为 点(横 为 1)，类型限定为 结构实例
要求 位 的类型是 结构实例，否则说明 "应是结构实例"
输出(位 的 横)"#,
    );
    assert_eq!(output, ["1"]);

    let outcome = assert_three_paths_failure("定义 数 为 1，类型限定为 结构实例");
    assert_eq!(outcome.message(), "变量“数”要求是结构实例，实际是数字。");
}

#[test]
fn type_matrix_struct_nominal_identity() {
    let output = assert_three_paths_output(
        r#"结构 甲：
    值
结束
定义 实例一 为 甲(值 为 1)
输出(类型是(实例一, "甲"))"#,
    );
    assert_eq!(output, ["是"]);
}

#[test]
fn type_matrix_constraint_rejects_wrong_type() {
    let outcome = assert_three_paths_failure(
        r#"定义 甲 为 1，类型限定为 数字
甲 为 "文本""#,
    );
    assert!(outcome.message().contains("要求是"));
}

#[test]
fn type_matrix_require_with_type() {
    let output = assert_three_paths_output(
        r#"定义 甲 为 42
要求 甲 的类型是 数字，否则说明 "类型错误"
输出("通过")"#,
    );
    assert_eq!(output, ["通过"]);
}

#[test]
fn type_matrix_match_by_type_branch() {
    let output = assert_three_paths_output(
        r#"定义功能 描述（值）：
    匹配 值：
        当 类型是 数字：
            返回 "数字"
        当 类型是 文本：
            返回 "文本"
        其他：
            返回 "其他"
    结束
结束
输出(描述(1), 描述("甲"), 描述([1]))"#,
    );
    assert_eq!(output, ["数字 文本 其他"]);
}

#[test]
fn type_matrix_decimal_and_float_are_separate() {
    let output = assert_three_paths_output(
        r#"输出(类型是(1.5, "数字"))
输出(类型是(1, "浮点数"))"#,
    );
    assert_eq!(output, ["是", "否"]);
}

/// 同名遮蔽结构必须是两个名义类型：内层声明的实例不能满足外层同名类型的约束，
/// 诊断也要把两者区分开而不是都写成同一个名字。
#[test]
fn type_matrix_shadowed_structs_are_distinct_nominal_types() {
    let program = r#"结构 甲：
    值
结束
定义功能 造（）：
    结构 甲：
        另
    结束
    返回 甲(另 为 2)
结束
定义 内 为 造()
输出(类型是(内, "甲"))
输出(内 的 另)"#;
    let output = assert_three_paths_output(program);
    assert_eq!(output, ["否", "2"]);

    let outcome = assert_three_paths_failure(&format!("{program}\n定义 混 为 内，类型限定为 甲"));
    assert_eq!(
        outcome.message(),
        "变量“混”要求是甲，实际是另一处声明的甲。"
    );
}

/// 类型约束与`要求 ... 的类型是`失败时必须报出实际结构名，
/// 曾因统一使用`Value::type_name()`而把所有结构实例都写成“结构实例”。
#[test]
fn type_matrix_constraint_failure_names_the_actual_structure() {
    let prelude = r#"结构 甲：
    值
结束
结构 乙：
    值
结束
定义 实例 为 乙(值 为 1)"#;
    let define = assert_three_paths_failure(&format!("{prelude}\n定义 混 为 实例，类型限定为 甲"));
    assert_eq!(define.message(), "变量“混”要求是甲，实际是乙。");

    let assign = assert_three_paths_failure(&format!(
        "{prelude}\n定义 混 为 甲(值 为 1)，类型限定为 甲\n混 为 实例"
    ));
    assert_eq!(assign.message(), "变量“混”要求是甲，实际是乙。");

    let require = assert_three_paths_failure(&format!("{prelude}\n要求 实例 的类型是 甲"));
    assert_eq!(require.message(), "要求值是甲，实际是乙。");
}
