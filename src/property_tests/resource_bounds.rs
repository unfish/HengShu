//! 资源边界：调用深度、执行时限与取消在两条执行路径上的一致表现。

use super::support::structured_errors;
use crate::compiler;
use crate::parser;
use crate::security::PermissionSet;
use crate::vm;
use std::collections::BTreeMap;

#[test]
fn interpreter_and_vm_report_timeout_at_the_current_operation() {
    let source = "引入 并发\n并发 休眠(持续时间(\"20毫秒\"))";
    let mut permissions = PermissionSet::restricted_for_source(std::path::Path::new("."));
    permissions.execution_timeout_ms = Some(1);
    let interpreter = crate::runtime::execute_with_options_structured(
        source,
        ".",
        false,
        Vec::new(),
        permissions.clone(),
        false,
    )
    .err()
    .expect("解释器应该报告执行超时");
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
    .expect("VM 应该报告执行超时");
    assert_eq!(interpreter.span.start.line, 2);
    assert_eq!(vm.span.start.line, 2);
    assert!(interpreter.message.contains("时间限制"));
    assert!(vm.message.contains("时间限制"));
}

#[test]
fn interpreter_and_vm_propagate_task_resource_context() {
    let recursive = "引入 并发\n定义功能 递归（）：\n返回 递归()\n结束\n定义 任务 为 并发 启动(递归)\n任务 等待()";
    let mut permissions = PermissionSet::restricted_for_source(std::path::Path::new("."));
    permissions.max_call_depth = Some(5);
    let (interpreter, vm) = structured_errors(recursive, permissions);
    for error in [&interpreter, &vm] {
        assert_eq!(error.category, "资源错误", "{}", error.message);
        assert!(error.message.contains("调用深度"));
    }

    let nested = "引入 并发\n定义功能 工作（）：\n返回 [[1]]\n结束\n定义 任务 为 并发 启动(工作)\n任务 等待()";
    let mut permissions = PermissionSet::restricted_for_source(std::path::Path::new("."));
    permissions.max_value_nesting = Some(2);
    let (interpreter, vm) = structured_errors(nested, permissions);
    for error in [&interpreter, &vm] {
        assert_eq!(error.category, "资源错误", "{}", error.message);
        assert!(error.message.contains("嵌套层级"));
    }

    let denied = "引入 并发\n定义功能 工作（）：\n引入 文件系统\n返回 文件系统 读取文本(\"/不允许读取\")\n结束\n定义 任务 为 并发 启动(工作)\n任务 等待()";
    let mut permissions = PermissionSet::restricted_for_source(std::path::Path::new("."));
    permissions.file_roots.clear();
    let (interpreter, vm) = structured_errors(denied, permissions);
    for error in [&interpreter, &vm] {
        assert!(error.message.contains("权限"), "{}", error.message);
    }

    let timeout = "引入 并发\n定义功能 工作（）：\n当 是 时重复：\n结束\n结束\n定义 任务 为 并发 启动(工作)\n任务 等待()";
    let mut permissions = PermissionSet::for_source(std::path::Path::new("."));
    permissions.execution_timeout_ms = Some(10);
    let (interpreter, vm) = structured_errors(timeout, permissions);
    for error in [&interpreter, &vm] {
        assert_eq!(error.category, "资源错误", "{}", error.message);
        assert!(error.message.contains("时间限制"));
    }
}

#[test]
fn interpreter_and_vm_reject_the_same_call_depth() {
    let mut permissions = PermissionSet::for_source(std::path::Path::new("."));
    permissions.max_call_depth = Some(8);
    for depth in 1..=12 {
        let source = format!(
            "定义功能 递归（数）：\n如果 数 <= 0：\n返回 0\n结束\n返回 1 + 递归(数 - 1)\n结束\n输出(递归({depth}))"
        );
        let interpreter = crate::runtime::execute_with_options_structured(
            &source,
            ".",
            false,
            Vec::new(),
            permissions.clone(),
            false,
        )
        .map(|interpreter| interpreter.output);
        let program = parser::parse(&source).expect("源码应该可解析");
        let vm = vm::execute_structured(
            &compiler::compile(&program).expect("源码应该可编译"),
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            permissions.clone(),
        )
        .map(|interpreter| interpreter.output);
        match (interpreter, vm) {
            (Ok(left), Ok(right)) => assert_eq!(left, right, "深度 {depth}"),
            (Err(left), Err(right)) => {
                assert_eq!(left.message, "功能调用深度超过资源限制", "深度 {depth}");
                assert_eq!(right.message, left.message, "深度 {depth}");
                assert_eq!(right.category, left.category, "深度 {depth}");
            }
            (left, right) => panic!("深度 {depth} 的失败边界不一致：{left:?} 与 {right:?}"),
        }
    }
}

#[test]
fn blocking_waits_report_the_execution_budget_as_a_resource_error() {
    let waits = [
        "引入 并发\n定义 通道 为 并发 通道(1)\n定义 值 为 通道 接收()",
        "引入 并发\n定义 通道 为 并发 通道(1)\n定义 数据流 为 并发 通道转流(通道)\n定义 值 为 数据流 下一个()",
        "引入 并发\n定义功能 工作（）：\n当 是 时重复：\n结束\n结束\n定义 任务 为 并发 启动(工作)\n任务 等待()",
        "引入 并发\n定义功能 慢（数值）：\n当 是 时重复：\n结束\n结束\n定义 结果 为 并发 并行映射([1], 慢)",
    ];
    for source in waits {
        let mut permissions = PermissionSet::for_source(std::path::Path::new("."));
        permissions.execution_timeout_ms = Some(50);
        let (interpreter, vm) = structured_errors(source, permissions);
        for error in [&interpreter, &vm] {
            assert_eq!(error.category, "资源错误", "{source}\n{}", error.message);
            assert!(
                error.message.contains("时间限制"),
                "{source}\n{}",
                error.message
            );
            assert!(
                !error.message.contains("行发生"),
                "{source}\n{}",
                error.message
            );
        }
    }
}

#[test]
fn interpreter_and_vm_keep_resource_and_cancellation_operation_ranges() {
    fn errors(
        source: &str,
        permissions: PermissionSet,
    ) -> (crate::runtime::RuntimeError, crate::runtime::RuntimeError) {
        let interpreter = crate::runtime::execute_with_options_structured(
            source,
            ".",
            false,
            Vec::new(),
            permissions.clone(),
            false,
        )
        .err()
        .expect("源码解释器应报告资源错误");
        let program = parser::parse(source).unwrap();
        let vm = vm::execute_structured(
            &compiler::compile(&program).unwrap(),
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .expect("VM 应报告资源错误");
        (interpreter, vm)
    }

    let recursive = "定义功能 递归（）：\n返回 递归()\n结束\n递归()";
    let mut permissions = PermissionSet::for_source(std::path::Path::new("."));
    permissions.max_call_depth = Some(4);
    let (interpreter, vm) = errors(recursive, permissions);
    for error in [&interpreter, &vm] {
        assert_ne!(error.span.start.line, 0);
        assert!(error.message.contains("调用深度"));
    }

    let cancelled = r#"
引入 并发
定义功能 工作（）：
    当 是 时重复：
    结束
结束
定义 任务 为 并发 启动(工作)
任务 取消()
任务 等待()
"#;
    let (interpreter, vm) = errors(
        cancelled,
        PermissionSet::for_source(std::path::Path::new(".")),
    );
    for error in [&interpreter, &vm] {
        assert_ne!(error.span.start.line, 0);
        assert_eq!(error.category, "资源错误");
        assert!(error.message.contains("任务已经取消"));
    }
}
