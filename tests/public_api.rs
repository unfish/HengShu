use hengshu::api::{
    check_source, compile_source, compile_source_in, format_source, run_compiled, run_source,
    run_source_outcome, CompiledProgram, ErrorKind, RunOptions, SourcePosition, SourceSpan,
};
use hengshu::execution::{InvocationRequest, ObservationMode, RunTermination};

#[test]
fn public_api_checks_runs_and_compiles_source() {
    check_source("定义 值 为 2\n输出(值 + 3)").unwrap();

    let result = run_source("定义 值 为 2\n输出(值 + 3)", RunOptions::new(".")).unwrap();
    assert_eq!(result.output, ["5"]);

    let compiled = compile_source("输出(1 + 2)").unwrap();
    compiled.verify().unwrap();
    assert!(compiled.instruction_count() > 0);
    let bytes = compiled.to_bytes().unwrap();
    assert!(!bytes.is_empty());
    CompiledProgram::from_bytes(&bytes).unwrap();
    assert_eq!(
        run_compiled("输出(1 + 2)", RunOptions::new("."))
            .unwrap()
            .output,
        ["3"]
    );

    let formatted = format_source("如果 是:\n输出(1)\n结束").unwrap();
    assert_eq!(formatted, format_source(&formatted).unwrap());
}

#[test]
fn public_api_observation_is_shared_by_source_and_vm_without_changing_output() {
    let source = "定义 值 为 2\n输出(值 + 3)";
    let mut observed = RunOptions::new(".");
    observed.observation = ObservationMode::Snapshots;
    let source_result = run_source(source, observed.clone()).unwrap();
    let vm_result = run_compiled(source, observed).unwrap();
    assert_eq!(source_result.output, ["5"]);
    assert_eq!(vm_result.output, ["5"]);
    assert!(source_result
        .observation
        .as_ref()
        .is_some_and(|report| !report.events.is_empty()));
    assert!(vm_result
        .observation
        .as_ref()
        .is_some_and(|report| !report.events.is_empty()));
    assert_eq!(
        run_source(source, RunOptions::new(".")).unwrap().output,
        ["5"]
    );
}

#[test]
fn structured_run_outcome_is_shared_by_source_and_vm() {
    let source = "输出(7)";
    let source_outcome = hengshu::api::run_source_outcome(source, RunOptions::new("."));
    let vm_outcome = hengshu::api::run_compiled_outcome(source, RunOptions::new("."));
    assert!(source_outcome.success && vm_outcome.success);
    assert_eq!(source_outcome.output, vm_outcome.output);
    assert!(source_outcome.diagnostics.is_empty() && vm_outcome.diagnostics.is_empty());
}

#[test]
fn invocation_contract_drives_typed_program_arguments_and_observation() {
    let mut request = InvocationRequest::new("程序.代码", "run-1", "结果");
    request
        .parameters
        .insert("模式".into(), serde_json::Value::String("月报".into()));
    request.observation = ObservationMode::Snapshots;
    let outcome = hengshu::api::run_invocation("引入 环境\n输出(环境 程序参数())", request);
    assert!(outcome.success, "{:?}", outcome.diagnostics);
    assert_eq!(outcome.output, ["[模式=\"月报\"]"]);
    assert!(!outcome.events.is_empty());
}

#[test]
fn public_api_rejects_trailing_or_malformed_bytecode() {
    let compiled = compile_source("输出(1)").unwrap();
    let mut bytes = compiled.to_bytes().unwrap();
    bytes.push(0);
    assert_eq!(
        CompiledProgram::from_bytes(&bytes).unwrap_err().kind(),
        ErrorKind::Serialization
    );
    assert_eq!(
        CompiledProgram::from_bytes(&[0xff, 0x00])
            .unwrap_err()
            .kind(),
        ErrorKind::Serialization
    );
}

#[test]
fn public_api_reports_processing_stage_and_source_line() {
    let parse = check_source("定义 值 为 @").unwrap_err();
    assert_eq!(parse.kind(), ErrorKind::Parse);
    assert_eq!(parse.line(), Some(1));
    assert_eq!(parse.column(), Some(8));
    assert_eq!(
        parse.span(),
        Some(SourceSpan {
            start: SourcePosition { line: 1, column: 8 },
            end: SourcePosition { line: 1, column: 9 },
        })
    );

    let check = check_source("定义 已知 为 1\n输出(未知)").unwrap_err();
    assert_eq!(check.kind(), ErrorKind::Check);
    assert_eq!(check.line(), Some(2));
    assert_eq!(check.column(), Some(4));
    assert_eq!(
        check.span(),
        Some(SourceSpan {
            start: SourcePosition { line: 2, column: 4 },
            end: SourcePosition { line: 2, column: 6 },
        })
    );

    let runtime = run_source("定义 值 为 1\n输出(值 % 0)", RunOptions::new(".")).unwrap_err();
    assert_eq!(runtime.kind(), ErrorKind::Runtime);
    assert_eq!(runtime.line(), Some(2));
    assert_eq!(runtime.column(), Some(1));
    assert_eq!(runtime.category(), Some("运行错误"));

    assert_eq!(parse.category(), Some("词法错误"));
    assert_eq!(check.category(), Some("检查错误"));
    for error in [&parse, &check, &runtime] {
        assert!(
            !error.message().contains("行发生"),
            "诊断消息不应包含位置文本：{}",
            error.message()
        );
    }
}

#[test]
fn public_api_obeys_custom_resource_limits() {
    let mut options = RunOptions::new(".");
    options.permissions.max_output_bytes = Some(2);
    let error = run_source("输出(123)", options).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Runtime);
    assert!(error.message().contains("最大输出字节"));

    let input = run_source("输入()", RunOptions::new(".")).unwrap_err();
    assert_eq!(input.kind(), ErrorKind::Runtime);
    assert!(input.message().contains("交互式标准输入"));

    let structured = run_source(
        "抛出 错误(\"失败\"，类别 为 \"自定义\")",
        RunOptions::new("."),
    )
    .unwrap_err();
    assert_eq!(structured.category(), Some("自定义"));
    assert_eq!(structured.message(), "失败");

    let nested = run_source(
        "定义功能 计算（）：\n定义 零 为 0\n返回 1 / 零\n结束\n计算()",
        RunOptions::new("."),
    )
    .unwrap_err();
    assert_eq!(nested.stack(), ["计算"]);

    let overridden = run_source(
        "尝试：\n尝试：\n抛出 错误(\"原始\"，类别 为 \"原始类别\")\n最终：\n抛出 错误(\"覆盖\"，类别 为 \"覆盖类别\")\n结束\n捕获 已捕获：\n抛出 已捕获\n结束",
        RunOptions::new("."),
    )
    .unwrap_err();
    assert_eq!(overridden.category(), Some("覆盖类别"));
    let cause = overridden.cause().expect("覆盖错误应保留原错误");
    assert_eq!(cause.category(), Some("原始类别"));
    assert_eq!(cause.message(), "原始");

    let parameter = run_source(
        "定义功能 计算（必需）：\n返回 必需\n结束\n计算()",
        RunOptions::new("."),
    )
    .unwrap_err();
    assert_eq!(parameter.line(), Some(4));
    assert_eq!(
        parameter.definition_span().map(|span| span.start.line),
        Some(1)
    );
}

#[test]
fn public_api_failures_carry_prior_output_and_structured_report() {
    let source = "输出(\"先打印\")\n定义功能 计算（必需）：\n返回 必需\n结束\n计算()";
    let paths = [
        run_source(source, RunOptions::new(".")).unwrap_err(),
        run_compiled(source, RunOptions::new(".")).unwrap_err(),
    ];
    for error in &paths {
        assert_eq!(error.output(), ["先打印"]);
        let report = error.report();
        assert_eq!(report.category.as_deref(), error.category());
        assert_eq!(report.message, error.message());
        assert_eq!(report.stack, error.stack());
        assert_eq!(report.span, error.span());
        assert_eq!(report.definition_span, error.definition_span());
        assert_eq!(report.span.map(|span| span.start.line), Some(5));
        assert_eq!(report.definition_span.map(|span| span.start.line), Some(2));
    }
    assert_eq!(paths[0].report(), paths[1].report());

    let overridden = run_source(
        "尝试：\n抛出 错误(\"原始\"，类别 为 \"原始类别\")\n最终：\n抛出 错误(\"覆盖\"，类别 为 \"覆盖类别\")\n结束",
        RunOptions::new("."),
    )
    .unwrap_err();
    let cause = overridden.report().cause.expect("原因链必须进入结构化诊断");
    assert_eq!(cause.category.as_deref(), Some("原始类别"));
    assert_eq!(cause.message, "原始");
}

/// 收紧后的集合项数上限必须同时约束两条路径：VM 的字面量构造和属性赋值新增键
/// 曾经绕过检查，只有解释器报资源错误。
#[test]
fn public_api_enforces_collection_limits_on_interpreter_and_bytecode() {
    let over_limit = [
        "定义 表 为 [1, 2, 3]",
        "定义 录 为 {甲: 1, 乙: 2, 丙: 3}",
        "定义 录 为 {甲: 1, 乙: 2}\n录 的 丙 为 3",
        "定义 表 为 [1, 2]\n表 加入(3)",
    ];
    for source in over_limit {
        for run in [run_source, run_compiled] {
            let mut options = RunOptions::new(".");
            options.permissions.max_collection_items = Some(2);
            let error = run(source, options).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Runtime, "{source}");
            assert_eq!(error.category(), Some("资源错误"), "{source}");
            assert_eq!(error.message(), "集合项数超过资源限制", "{source}");
        }
    }

    for run in [run_source, run_compiled] {
        let mut options = RunOptions::new(".");
        options.permissions.max_collection_items = Some(3);
        let result = run(
            "定义 录 为 {甲: 1, 乙: 2}\n录 的 丙 为 3\n输出(录 的 丙)",
            options,
        )
        .unwrap();
        assert_eq!(result.output, ["3"]);
    }
}

#[test]
fn public_api_rejects_oversized_source_before_processing() {
    let source = "x".repeat(16 * 1024 * 1024 + 1);
    assert_eq!(
        check_source(&source).unwrap_err().kind(),
        ErrorKind::Resource
    );
    assert_eq!(
        compile_source(&source).unwrap_err().kind(),
        ErrorKind::Resource
    );
    assert_eq!(
        run_source(&source, RunOptions::new("."))
            .unwrap_err()
            .kind(),
        ErrorKind::Resource
    );
    assert_eq!(
        format_source(&source).unwrap_err().kind(),
        ErrorKind::Resource
    );
}

#[test]
fn public_api_parses_long_chains_and_reports_over_budget_nesting() {
    let long_chain = format!(
        "输出({})",
        std::iter::repeat_n("1", 500)
            .collect::<Vec<_>>()
            .join(" + ")
    );
    check_source(&long_chain).unwrap();
    assert_eq!(
        run_source(&long_chain, RunOptions::new("."))
            .unwrap()
            .output,
        ["500"]
    );
    assert_eq!(
        run_compiled(&long_chain, RunOptions::new("."))
            .unwrap()
            .output,
        ["500"]
    );

    let over_budget = format!(
        "输出({})",
        std::iter::repeat_n("1", 4096)
            .collect::<Vec<_>>()
            .join(" + ")
    );
    let error = check_source(&over_budget).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Parse);
    assert!(error.message().contains("语法嵌套深度超过"), "{error}");
}

#[test]
fn compiled_programs_carry_their_modules() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("辅模.代码"), "公开 定义 值 为 99").unwrap();
    let compiled = compile_source_in("从 辅模 引入 值\n输出(值)", dir.path()).unwrap();
    let bytes = compiled.to_bytes().unwrap();
    let restored = CompiledProgram::from_bytes(&bytes).unwrap();
    let result = restored.run(RunOptions::new(dir.path())).unwrap();
    assert_eq!(result.output, ["99"]);
}

#[test]
fn structured_outcomes_distinguish_total_timeout_from_other_resource_failures() {
    let mut timeout = RunOptions::new(".");
    timeout.permissions.execution_timeout_ms = Some(1);
    let outcome = run_source_outcome("当 是 时重复：\n结束", timeout);
    assert!(!outcome.success);
    assert_eq!(outcome.termination, RunTermination::TimedOut);

    let mut collection = RunOptions::new(".");
    collection.permissions.max_collection_items = Some(1);
    let outcome = run_source_outcome("定义 值 为 [1, 2]", collection);
    assert!(!outcome.success);
    assert_eq!(outcome.termination, RunTermination::Failed);
}
