//! Notebook 观察测试：循环、任务、流和大表在快照模式下事件稳定且受预算约束，
//! 关闭观察不改变运行结果，敏感值在事件里被遮挡。

use hengshu::api::{compile_source_in, run_source, RunOptions};
use hengshu::evidence::redacted_comparison_view;
use hengshu::execution::{ObservationBudget, ObservationMode};
use std::path::Path;

fn snapshot_options(directory: &Path) -> RunOptions {
    RunOptions {
        observation: ObservationMode::Snapshots,
        ..RunOptions::new(directory)
    }
}

fn event_shape(notebook: &hengshu::provenance::Notebook) -> Vec<String> {
    notebook
        .events
        .iter()
        .map(|event| format!("{}:{:?}", event.step, event.stage))
        .collect()
}

const LOOP_PROGRAM: &str =
    "定义 合计 为 0\n遍历 [1, 2, 3]，记作 项：\n    合计 为 合计 + 项\n结束\n输出(合计)";

#[test]
fn repeated_runs_produce_identical_event_shapes() {
    let directory = tempfile::tempdir().unwrap();
    let first = run_source(LOOP_PROGRAM, snapshot_options(directory.path())).unwrap();
    let second = run_source(LOOP_PROGRAM, snapshot_options(directory.path())).unwrap();
    let first_notebook = first.observation.as_ref().unwrap();
    let second_notebook = second.observation.as_ref().unwrap();
    assert!(!first_notebook.events.is_empty());
    assert_eq!(event_shape(first_notebook), event_shape(second_notebook));
    assert_eq!(first.output, second.output);
    assert_eq!(first.output, vec!["6".to_string()]);
}

#[test]
fn redacted_comparison_view_contains_structure_without_sensitive_values() {
    let directory = tempfile::tempdir().unwrap();
    let expected = run_source("输出(\"秘密值\")", RunOptions::new(directory.path())).unwrap();
    let actual = run_source("输出(\"另一个值\")", RunOptions::new(directory.path())).unwrap();
    let expected_outcome =
        hengshu::api::run_source_outcome("输出(\"秘密值\")", RunOptions::new(directory.path()));
    let actual_outcome =
        hengshu::api::run_source_outcome("输出(\"另一个值\")", RunOptions::new(directory.path()));
    let view = redacted_comparison_view(&expected_outcome, &actual_outcome, &[], &[]);
    let json = view.to_json().unwrap();
    let html = view.to_html();
    assert!(!view.matches);
    assert!(json.contains("Output"));
    assert!(!json.contains("秘密值"));
    assert!(!json.contains("另一个值"));
    assert!(!html.contains("秘密值"));
    assert!(!html.contains("另一个值"));
    assert_eq!(expected.output, ["秘密值"]);
    assert_eq!(actual.output, ["另一个值"]);
}

#[test]
fn observation_off_matches_snapshot_results_for_tasks_streams_and_tables() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("大表.csv"),
        std::iter::once("编号,金额\n".to_string())
            .chain((1..=200).map(|index| format!("{index},{}\n", index * 2)))
            .collect::<String>(),
    )
    .unwrap();
    let program = "引入 表格处理\n引入 并发\n定义 表 为 表格处理 读取表格(\"大表.csv\")\n定义 通 为 并发 通道(1)\n通 发送(表 的 行数)\n定义 行数 为 通 接收()\n定义 任 为 并发 启动(定义功能（）：\n    返回 行数 * 2\n结束)\n输出(行数)\n输出(任 等待())";

    let observed = run_source(program, snapshot_options(directory.path())).unwrap();
    let silent = run_source(program, RunOptions::new(directory.path())).unwrap();
    assert_eq!(observed.output, silent.output);
    assert_eq!(observed.output, vec!["200".to_string(), "400".to_string()]);
    assert!(silent.observation.is_none());
    assert!(!observed.observation.unwrap().events.is_empty());
}

#[test]
fn snapshot_budget_truncates_instead_of_growing_without_bound() {
    let directory = tempfile::tempdir().unwrap();
    let options = RunOptions {
        observation: ObservationMode::Snapshots,
        observation_budget: ObservationBudget {
            max_events: 3,
            ..ObservationBudget::default()
        },
        ..RunOptions::new(directory.path())
    };
    let result = run_source(LOOP_PROGRAM, options).unwrap();
    let notebook = result.observation.unwrap();
    assert!(notebook.events.len() <= 3, "{}", notebook.events.len());
    assert!(notebook.truncated);
    assert_eq!(result.output, vec!["6".to_string()]);
}

#[test]
fn interpreter_and_vm_agree_on_event_shape_and_json_export() {
    let directory = tempfile::tempdir().unwrap();
    let interpreted = run_source(LOOP_PROGRAM, snapshot_options(directory.path())).unwrap();
    let compiled = compile_source_in(LOOP_PROGRAM, directory.path()).unwrap();
    let executed = compiled.run(snapshot_options(directory.path())).unwrap();
    assert_eq!(interpreted.output, executed.output);

    let notebook = interpreted.observation.as_ref().unwrap();
    let json = notebook.to_json().unwrap();
    assert!(json.contains("\"version\""));
    assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    let html = notebook.to_html().unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(!html.contains("<script"));
}

#[test]
fn each_execution_path_declares_its_own_event_granularity_and_source_line() {
    const ONE_LINE: &str = "输出(6 * 2)";
    let directory = tempfile::tempdir().unwrap();

    let interpreted = run_source(ONE_LINE, snapshot_options(directory.path())).unwrap();
    let statements = interpreted.observation.as_ref().unwrap();
    assert_eq!(statements.events.len(), 1, "一条语句只记录一个语句事件");
    assert!(statements
        .events
        .iter()
        .all(|event| event.stage == "statement"));

    let compiled = compile_source_in(ONE_LINE, directory.path()).unwrap();
    let executed = compiled.run(snapshot_options(directory.path())).unwrap();
    let instructions = executed.observation.as_ref().unwrap();
    assert_eq!(interpreted.output, executed.output);
    assert!(
        instructions.events.len() > statements.events.len(),
        "字节码按指令记录，条数应多于语句数：{}",
        instructions.events.len()
    );
    assert!(instructions
        .events
        .iter()
        .all(|event| event.stage == "instruction"));

    for notebook in [statements, instructions] {
        for event in &notebook.events {
            let span = event
                .source_span
                .as_ref()
                .expect("事件必须带源码位置，否则界面无法解释指令归属哪一行");
            assert_eq!(span.start.line, 1, "程序只有一行");
        }
    }
}
