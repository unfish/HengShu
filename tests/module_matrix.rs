//! 模块矩阵：普通引入与选择引入、可见性、类型身份、模块顶层副作用、
//! 相对导入路径与循环依赖等在三路径上的一致行为。

mod common;

use common::{
    assert_three_paths_failure_with_modules, assert_three_paths_output_with_modules,
    assert_three_paths_with_modules, ExecutionStatus,
};

#[test]
fn module_matrix_plain_import_exposes_module_value() {
    let module_source = "公开 定义 常量 为 42";
    let output = assert_three_paths_output_with_modules(
        "引入 公开模块\n输出(公开模块 的 常量)\n",
        &[("公开模块", module_source)],
    );
    assert_eq!(output, ["42"]);
}

#[test]
fn module_matrix_selective_import_binds_named_symbol() {
    let module_source = "公开 定义 名 为 \"甲\"\n公开 定义 数 为 7";
    let output = assert_three_paths_output_with_modules(
        "从 选导 引入 名, 数\n输出(名, 数)\n",
        &[("选导", module_source)],
    );
    assert_eq!(output, ["甲 7"]);
}

#[test]
fn module_matrix_internal_binding_is_hidden() {
    let module_source = "内部 定义 私 为 1\n公开 定义 见 为 2";
    let outcome = assert_three_paths_failure_with_modules(
        "从 隐藏 引入 私\n输出(私)\n",
        &[("隐藏", module_source)],
    );
    assert!(outcome.message().contains("私"));
}

#[test]
fn module_matrix_type_identity_survives_selective_import() {
    let module_source = r#"公开 结构 客户：
    姓名
结束"#;
    let output = assert_three_paths_output_with_modules(
        r#"从 客户模 引入 客户
定义 对象 为 客户(姓名 为 "乙")
输出(类型是(对象, "客户"), 对象 的 姓名)
"#,
        &[("客户模", module_source)],
    );
    assert_eq!(output, ["是 乙"]);
}

#[test]
fn module_matrix_qualified_type_reference_from_plain_import() {
    let module_source = r#"公开 结构 点：
    横
结束
公开 定义功能 建点（横）：
    返回 点(横 为 横)
结束"#;
    let output = assert_three_paths_output_with_modules(
        r#"引入 图形
定义 位 为 图形 的 建点(5)，类型限定为 图形.点
输出(类型是(位, "图形.点"), 位 的 横)
"#,
        &[("图形", module_source)],
    );
    assert_eq!(output, ["是 5"]);
}

#[test]
fn module_matrix_qualified_type_reference_rejects_other_type() {
    let module_source = r#"公开 结构 点：
    横
结束"#;
    let outcome = assert_three_paths_failure_with_modules(
        r#"引入 图形
定义 位 为 1，类型限定为 图形.点
"#,
        &[("图形", module_source)],
    );
    assert!(outcome.message().contains("要求是"));
}

#[test]
fn module_matrix_module_top_level_side_effects_run_once() {
    let module_source = r#"输出("加载")
公开 定义 值 为 1"#;
    let outcome = assert_three_paths_with_modules(
        "引入 副作用\n从 副作用 引入 值\n输出(副作用 的 值, 值)\n",
        &[("副作用", module_source)],
    );
    assert_eq!(outcome.status, ExecutionStatus::Success);
    assert_eq!(outcome.output, ["加载", "1 1"]);
}

#[test]
fn module_matrix_public_method_is_reachable_across_import() {
    let module_source = r#"公开 结构 计：
    值
结束
公开 定义功能 计 加倍（自己）：
    返回 自己 的 值 * 2
结束"#;
    let output = assert_three_paths_output_with_modules(
        r#"从 计模 引入 计
定义 个 为 计(值 为 5)
输出(个 加倍())
"#,
        &[("计模", module_source)],
    );
    assert_eq!(output, ["10"]);
}

#[test]
fn module_matrix_keeps_module_top_level_output() {
    let module_source = r#"输出("模块副作用")
公开 定义 数 为 1"#;
    let output = assert_three_paths_output_with_modules(
        "从 观察 引入 数\n输出(数)\n",
        &[("观察", module_source)],
    );
    assert_eq!(output, ["模块副作用", "1"]);
}

#[test]
fn module_matrix_shares_plain_and_selective_imports() {
    let module_source = "公开 定义 甲 为 10\n公开 定义 乙 为 20";
    let output = assert_three_paths_output_with_modules(
        "引入 混合\n从 混合 引入 乙\n输出(混合 的 甲, 乙)\n",
        &[("混合", module_source)],
    );
    assert_eq!(output, ["10 20"]);
}

#[test]
fn module_matrix_diamond_import_runs_shared_module_once() {
    let output = assert_three_paths_output_with_modules(
        "引入 左\n引入 右\n定义 和 为 左 的 左值 + 右 的 右值\n输出(和)\n",
        &[
            ("共享", "输出(\"共享顶层\")\n公开 定义 值 为 3"),
            ("左", "引入 共享\n公开 定义 左值 为 共享 的 值"),
            ("右", "引入 共享\n公开 定义 右值 为 共享 的 值"),
        ],
    );
    assert_eq!(output, ["共享顶层", "6"]);
}

#[test]
fn module_matrix_shared_module_state_is_one_instance() {
    let output = assert_three_paths_output_with_modules(
        "引入 甲侧\n引入 乙侧\n甲侧 的 加一()\n输出(乙侧 的 读取())\n",
        &[
            (
                "计数器",
                "公开 定义 计数 为 [0]\n公开 定义功能 增加（）：\n    计数[1] 为 计数[1] + 1\n结束",
            ),
            (
                "甲侧",
                "引入 计数器\n公开 定义功能 加一（）：\n    计数器 的 增加()\n结束",
            ),
            (
                "乙侧",
                "引入 计数器\n公开 定义功能 读取（）：\n    返回 计数器 的 计数[1]\n结束",
            ),
        ],
    );
    assert_eq!(output, ["1"]);
}
