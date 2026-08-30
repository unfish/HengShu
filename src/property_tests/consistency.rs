//! 解释器与 VM 的语义一致性属性。

use super::support::{configuration, restricted_semantic_program, run_vm, structured_errors};
use crate::compiler;
use crate::parser;
use crate::runtime::Interpreter;
use crate::security::PermissionSet;
use crate::vm;
use proptest::prelude::*;
use std::collections::BTreeMap;

proptest! {
    #![proptest_config(configuration())]

    #[test]
    fn interpreter_and_vm_match_for_integer_arithmetic(
        left in -1_000_000_i64..=1_000_000,
        right in -1_000_000_i64..=1_000_000,
    ) {
        let source = format!("输出(({left} + {right}) * 2)");
        let reference = Interpreter::execute_source(&source, ".", false).unwrap().output;
        let actual = run_vm(&source).unwrap();
        prop_assert_eq!(actual, reference);
    }

    #[test]
    fn restricted_ast_programs_match_across_source_and_vm(
        source in restricted_semantic_program()
    ) {
        let program = parser::parse(&source).map_err(TestCaseError::fail)?;
        crate::checker::check(&program).map_err(TestCaseError::fail)?;
        let reference = Interpreter::execute_source(&source, ".", false)
            .map_err(TestCaseError::fail)?
            .output;
        let actual = run_vm(&source).map_err(TestCaseError::fail)?;
        prop_assert_eq!(actual, reference, "源码：\n{}", source);
    }
}

#[test]
fn interpreter_and_vm_match_shared_semantic_corpus() {
    let programs = [
        "定义 总计 为 0\n当 总计 < 4 时重复：\n总计 为 总计 + 1\n结束\n输出(总计)",
        "定义功能 制作（起点）：\n返回 定义功能（增量）为 起点 + 增量\n结束\n输出(制作(10)(2))",
        "尝试：\n抛出 \"失败\"\n捕获 异常：\n输出(异常 的 信息)\n最终：\n输出(\"清理\")\n结束",
        "定义 值 为 [1, 2, 3]\n值 加入(4)\n输出(值, 包含(值, 3))",
        "定义功能 计算（值，增量 为 2）：\n返回 值 + 增量\n结束\n输出(计算(3), 计算(3, 增量 为 4))",
        "定义功能 后继（值，增量 为 值 + 1）：\n返回 增量\n结束\n输出(后继(4))",
        "定义 订单数 为 1234567\n参数化输出(\"订单数：{订单数:千分位,0位小数}\")",
        "引入 统计分析\n定义 稳定 为 统计分析 需求稳定性([5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5])\n输出(稳定 的 稳定, 稳定 的 需求类型)",
        "引入 统计分析\n定义 预测 为 统计分析 需求预测([10, 11, 12, 13, 14, 15, 16, 17], 步数 为 2, 验证期 为 2)\n输出(预测 的 最佳模型, 长度(预测 的 预测))",
    ];
    for source in programs {
        let interpreter = Interpreter::execute_source(source, ".", false)
            .unwrap()
            .output;
        let vm = run_vm(source).unwrap();
        assert_eq!(vm, interpreter, "差分程序：\n{source}");
    }
}

#[test]
fn interpreter_and_vm_match_evaluation_order_and_call_contracts() {
    let programs = [
        "定义 列表值 为 [0, 0]\n定义 次序 为 0\n定义功能 下一个（）：\n全局 次序\n次序 为 次序 + 1\n返回 次序\n结束\n列表值[下一个()] 为 下一个()\n输出(列表值, 次序)",
        "要求 是，否则说明 1 / 0\n输出(\"通过\")",
        "输出(0 且 1, 1 或 \"文本\")",
        "定义 次序 为 0\n定义功能 下一个（）：\n全局 次序\n次序 为 次序 + 1\n返回 次序\n结束\n定义功能 组合（甲，乙）：\n输出(甲, 乙)\n结束\n组合(乙 为 下一个(), 甲 为 下一个())",
        "定义 __衡枢临时1 为 \"用户值\"\n匹配 2：\n当 2：\n输出(__衡枢临时1, 当前值)\n结束",
    ];
    for source in programs {
        let reference = Interpreter::execute_source(source, ".", false)
            .unwrap()
            .output;
        let actual = run_vm(source).unwrap();
        assert_eq!(actual, reference, "差分程序：\n{source}");
    }
    assert_eq!(run_vm(programs[0]).unwrap(), ["[0, 1] 2"]);
    assert_eq!(run_vm(programs[1]).unwrap(), ["通过"]);
    assert_eq!(run_vm(programs[2]).unwrap(), ["否 是"]);
    assert_eq!(run_vm(programs[3]).unwrap(), ["2 1"]);
    assert_eq!(run_vm(programs[4]).unwrap(), ["用户值 2"]);

    for invalid in [
        "定义功能 合计（甲，乙）：\n返回 甲 + 乙\n结束\n合计(1, 甲 为 2, 乙 为 3)",
        "结构 客户：\n姓名\n结束\n客户(\"多余\", 姓名 为 \"甲\")",
    ] {
        assert!(Interpreter::execute_source(invalid, ".", false).is_err());
        assert!(run_vm(invalid).is_err());
    }
}

#[test]
fn interpreter_and_vm_share_strict_numeric_type_relations() {
    let source = "引入 统计分析\n定义 浮一 为 统计分析 数组([1, 2]) 平均值()，类型限定为 浮点数\n定义 浮二 为 统计分析 数组([2, 3]) 平均值()\n要求 浮一 的类型是 浮点数\n输出(类型是(浮一, \"浮点数\"), 类型是(浮一, \"数字\"), 类型是(浮一 + 浮二, \"浮点数\"), 转数字(浮一) = 1.5)\n匹配 浮一：\n当 类型是 浮点数：\n输出(\"匹配\")\n结束";
    let interpreter = Interpreter::execute_source(source, ".", false)
        .unwrap()
        .output;
    let vm = run_vm(source).unwrap();
    assert_eq!(interpreter, ["是 否 是 是", "匹配"]);
    assert_eq!(vm, interpreter);

    let mixed = "引入 统计分析\n定义 浮 为 统计分析 数组([1, 2]) 平均值()\n";
    for expression in ["浮 = 1.5", "浮 < 1.5", "浮 + 1", "1 + 浮", "1.5 > 浮"] {
        let source = format!("{mixed}输出({expression})");
        let (interpreter, vm) = structured_errors(
            &source,
            PermissionSet::for_source(std::path::Path::new(".")),
        );
        assert_eq!(
            interpreter.message,
            "数字和浮点数不能参与同一次运算或比较，请先用“转数字”把浮点数转换为数字。",
            "{expression}"
        );
        assert_eq!(vm.message, interpreter.message, "{expression}");
    }

    for invalid in [
        "输出(是 + 1)",
        "输出(\"1\" + 1)",
        "引入 统计分析\n定义 浮 为 统计分析 数组([1, 2]) 平均值()，类型限定为 数字",
        "引入 财务\n定义 金额 为 财务 金额(10)\n输出(金额 * 是)",
    ] {
        assert!(
            Interpreter::execute_source(invalid, ".", false).is_err(),
            "{invalid}"
        );
        assert!(run_vm(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn interpreter_and_vm_preserve_structured_task_errors() {
    let source = "引入 并发\n定义功能 工作（）：\n抛出 错误(\"失败\", 类别 为 \"任务类别\")\n结束\n定义 任务 为 并发 启动(工作)\n任务 等待()";
    let permissions = PermissionSet::for_source(std::path::Path::new("."));
    let interpreter = crate::runtime::execute_with_options_structured(
        source,
        ".",
        false,
        Vec::new(),
        permissions.clone(),
        false,
    )
    .err()
    .expect("解释器任务应该传播错误");
    let program = crate::parser::parse(source).unwrap();
    let vm = crate::vm::execute_structured(
        &crate::compiler::compile(&program).unwrap(),
        BTreeMap::new(),
        ".",
        false,
        Vec::new(),
        permissions,
    )
    .err()
    .expect("VM 任务应该传播错误");

    for error in [&interpreter, &vm] {
        assert_eq!(error.category, "任务类别");
        assert_eq!(error.span.start.line, 3);
        assert_eq!(error.stack.as_ref(), ["工作"]);
        assert_eq!(error.message, "失败");
    }
}

#[test]
fn interpreter_and_vm_share_task_group_exit_state_machine() {
    let source = r#"
引入 并发
定义功能 慢任务（）：
    当 是 时重复：
    结束
结束
定义功能 提前返回（）：
    任务组：
        并发 启动(慢任务)
        返回 7
    结束
结束
输出(提前返回())
遍历 [1]，记作 编号：
    任务组：
        并发 启动(慢任务)
        跳出循环
    结束
结束
遍历 [1]，记作 编号：
    任务组：
        并发 启动(慢任务)
        继续下一轮
    结束
结束
尝试：
    任务组：
        并发 启动(慢任务)
        抛出 "正文失败"
    结束
捕获 异常：
    输出(异常 的 信息)
结束
"#;
    let started = std::time::Instant::now();
    let interpreter = Interpreter::execute_source(source, ".", false)
        .unwrap()
        .output;
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    let started = std::time::Instant::now();
    let vm = run_vm(source).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(interpreter, ["7", "正文失败"]);
    assert_eq!(vm, interpreter);
}

#[test]
fn interpreter_and_vm_keep_secondary_task_group_errors_as_causes() {
    let source = r#"
引入 并发
定义功能 主失败（）：
    定义 计数 为 0
    当 计数 < 20000 时重复：
        计数 为 计数 + 1
    结束
    抛出 错误("主任务失败", 类别 为 "主任务类别")
结束
定义功能 次失败（）：
    抛出 错误("次任务失败", 类别 为 "次任务类别")
结束
任务组：
    并发 启动(主失败)
    并发 启动(次失败)
结束
"#;
    let permissions = PermissionSet::for_source(std::path::Path::new("."));
    let interpreter = crate::runtime::execute_with_options_structured(
        source,
        ".",
        false,
        Vec::new(),
        permissions.clone(),
        false,
    )
    .err()
    .expect("源码解释器应传播任务组错误");
    let program = parser::parse(source).unwrap();
    let vm = crate::vm::execute_structured(
        &compiler::compile(&program).unwrap(),
        BTreeMap::new(),
        ".",
        false,
        Vec::new(),
        permissions,
    )
    .err()
    .expect("VM 应传播任务组错误");

    for error in [&interpreter, &vm] {
        assert_eq!(error.category, "主任务类别");
        assert_eq!(error.message, "主任务失败");
        let cause = error.cause.as_deref().expect("次级任务错误应保留在原因链");
        assert_eq!(cause.category, "次任务类别");
        assert_eq!(cause.message, "次任务失败");
    }
}

#[test]
fn source_api_and_vm_preserve_structured_module_errors() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("坏模块.代码"), "定义 值 为 @").unwrap();
    let source = "输出(0)\n引入 坏模块";
    let permissions = PermissionSet::for_source(directory.path());
    let source_error = crate::runtime::execute_with_options_structured(
        source,
        directory.path(),
        false,
        Vec::new(),
        permissions,
        false,
    )
    .err()
    .expect("源码模块解析应失败");
    assert_eq!(source_error.category, "模块错误");
    assert_eq!(source_error.span.start.line, 2);
    let cause = source_error.cause.as_deref().expect("应保留模块内解析诊断");
    assert_eq!(cause.category, "词法错误");
    assert_eq!(cause.span.start.line, 1);
    assert!(cause.span.start.column > 1);

    let api_error =
        crate::api::run_source(source, crate::api::RunOptions::new(directory.path())).unwrap_err();
    assert_eq!(api_error.category(), Some("模块错误"));
    assert_eq!(api_error.line(), Some(2));
    let api_cause = api_error.cause().expect("API 应保留模块内解析诊断");
    assert_eq!(api_cause.category(), Some("词法错误"));
    assert_eq!(api_cause.line(), Some(1));

    let mut main = compiler::compile(&parser::parse("输出(0)\n引入 坏模块").unwrap()).unwrap();
    main.module_imports
        .insert("坏模块".into(), "模块:坏".into());
    let mut malformed = compiler::compile_module(&parser::parse("输出(1)").unwrap()).unwrap();
    malformed.version = 0;
    let vm_error = vm::execute_structured(
        &main,
        BTreeMap::from([("模块:坏".into(), malformed)]),
        directory.path(),
        false,
        Vec::new(),
        PermissionSet::for_source(directory.path()),
    )
    .err()
    .expect("VM 应在导入操作验证模块字节码");
    assert_eq!(vm_error.category, "模块错误");
    assert_eq!(vm_error.span.start.line, 2);

    let module_source = "抛出 错误(\"模块执行失败\", 类别 为 \"模块自定义类别\")";
    std::fs::write(directory.path().join("执行模块.代码"), module_source).unwrap();
    let source = "引入 执行模块";
    let source_error = crate::runtime::execute_with_options_structured(
        source,
        directory.path(),
        false,
        Vec::new(),
        PermissionSet::for_source(directory.path()),
        false,
    )
    .err()
    .expect("源码模块执行应失败");
    let mut main = compiler::compile(&parser::parse(source).unwrap()).unwrap();
    main.module_imports
        .insert("执行模块".into(), "模块:执行".into());
    let module = compiler::compile_module(&parser::parse(module_source).unwrap()).unwrap();
    let vm_error = vm::execute_structured(
        &main,
        BTreeMap::from([("模块:执行".into(), module)]),
        directory.path(),
        false,
        Vec::new(),
        PermissionSet::for_source(directory.path()),
    )
    .err()
    .expect("VM 模块执行应失败");
    for error in [&source_error, &vm_error] {
        assert_eq!(error.category, "模块自定义类别");
        assert_eq!(error.span.start.line, 1);
        assert_eq!(error.message, "模块执行失败");
    }
}

#[test]
fn interpreter_and_vm_use_semantic_capture_bindings() {
    let source = "引入 并发\n定义 外部 为 7\n定义功能 制作（默认值 为 外部）：\n如果 否：\n定义 外部 为 99\n结束\n遍历 []，记作 外部：\n输出(外部)\n结束\n尝试：\n抛出 \"触发\"\n捕获 外部：\n输出(外部 的 信息)\n结束\n匹配 默认值：\n当 记作 外部：\n定义 分支局部 为 外部\n结束\n定义 局部 为 3\n返回 定义功能（）为 外部 + 局部\n结束\n定义 闭包 为 制作()\n定义 任务 为 并发 启动(闭包)\n输出(任务 等待())";
    let program = parser::parse(source).unwrap();
    let external = program
        .symbols
        .occurrences
        .iter()
        .find(|occurrence| occurrence.declaration && occurrence.name == "外部")
        .map(|occurrence| occurrence.identity)
        .expect("外部绑定应该具有稳定身份");
    let capture_sets = program
        .symbols
        .functions
        .iter()
        .map(|function| {
            function
                .captures
                .iter()
                .map(|capture| (capture.id, capture.name.as_str()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(capture_sets
        .iter()
        .any(|captures| captures.iter().any(|(_, name)| *name == "局部")));
    assert!(capture_sets.iter().all(|captures| captures
        .iter()
        .filter(|(_, name)| *name == "外部")
        .all(|(id, _)| Some(crate::ast::SymbolId::Binding(*id)) == Some(external))));

    let bytecode = compiler::compile(&program).unwrap();
    let bytecode_captures = bytecode
        .functions
        .iter()
        .flat_map(|function| &function.captures)
        .collect::<Vec<_>>();
    assert!(bytecode_captures
        .iter()
        .any(|capture| capture.name == "外部"));
    assert!(bytecode_captures
        .iter()
        .any(|capture| capture.name == "局部"));

    let interpreter = Interpreter::execute_source(source, ".", false)
        .unwrap()
        .output;
    let vm = vm::execute(
        &bytecode,
        BTreeMap::new(),
        ".",
        false,
        Vec::new(),
        PermissionSet::for_source(std::path::Path::new(".")),
    )
    .unwrap()
    .output;
    assert_eq!(interpreter, ["触发", "10"]);
    assert_eq!(vm, interpreter);
}

#[test]
fn interpreter_and_vm_share_unary_numeric_semantics() {
    let source = "引入 统计分析\n定义 浮 为 统计分析 数组([1, 2]) 平均值()\n输出(-浮, 类型是(-浮, \"浮点数\"))";
    let interpreter = Interpreter::execute_source(source, ".", false)
        .unwrap()
        .output;
    let vm = run_vm(source).unwrap();
    assert_eq!(interpreter, ["-1.5 是"]);
    assert_eq!(vm, interpreter);

    let invalid = "输出(-是)";
    assert!(Interpreter::execute_source(invalid, ".", false).is_err());
    assert!(run_vm(invalid).is_err());
}

#[test]
fn interpreter_and_vm_match_block_scope_shadowing_and_cleanup() {
    let programs = [
        "定义 值 为 1\n如果 是：\n定义 值 为 2\n输出(值)\n结束\n输出(值)",
        "定义 值 为 1\n尝试：\n定义 值 为 2\n输出(值)\n捕获 异常：\n定义 值 为 3\n最终：\n定义 值 为 4\n输出(值)\n结束\n输出(值)",
        "定义 值 为 1\n匹配 2：\n当 1：\n定义 值 为 2\n其他：\n定义 值 为 3\n输出(值)\n结束\n输出(值)",
        "定义 次数 为 0\n当 次数 < 2 时重复：\n定义 临时 为 次数\n次数 为 次数 + 1\n继续下一轮\n结束\n输出(次数)",
    ];
    for source in programs {
        let interpreter = Interpreter::execute_source(source, ".", false)
            .unwrap()
            .output;
        let vm = run_vm(source).unwrap();
        assert_eq!(vm, interpreter, "差分程序：\n{source}");
    }
}
