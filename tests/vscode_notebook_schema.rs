//! 过程报告的 VS Code 查看入口：扩展声明的 JSON schema 必须与实际写出的 Notebook
//! 字段保持一致，否则编辑器里的校验会把正确的过程报告标成错误。

use hengshu::api::{run_source, RunOptions};
use hengshu::execution::ObservationMode;
use serde_json::Value;

fn schema() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/editors/vscode/schemas/notebook.schema.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn notebook() -> Value {
    let mut options = RunOptions::new(".");
    options.observation = ObservationMode::Snapshots;
    let report = run_source("定义 甲 为 1 + 2\n输出(甲)\n", options)
        .unwrap()
        .observation
        .expect("快照模式必须产生过程报告");
    serde_json::to_value(*report).unwrap()
}

#[test]
fn the_vscode_extension_declares_the_notebook_schema() {
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/editors/vscode/package.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let validation = &manifest["contributes"]["jsonValidation"][0];
    assert_eq!(validation["url"], "./schemas/notebook.schema.json");
    assert!(validation["fileMatch"]
        .as_array()
        .unwrap()
        .iter()
        .any(|pattern| pattern == "过程报告.json"));
}

#[test]
fn the_notebook_schema_covers_every_written_field() {
    let schema = schema();
    let notebook = notebook();
    let properties = schema["properties"].as_object().unwrap();
    for key in notebook.as_object().unwrap().keys() {
        assert!(
            properties.contains_key(key),
            "schema 缺少过程报告字段：{key}"
        );
    }
    let event_properties = schema["definitions"]["事件"]["properties"]
        .as_object()
        .unwrap();
    let event = &notebook["events"][0];
    for key in event.as_object().unwrap().keys() {
        assert!(
            event_properties.contains_key(key),
            "schema 缺少执行事件字段：{key}"
        );
    }
    assert!(schema["properties"]["mode"]["enum"]
        .as_array()
        .unwrap()
        .contains(&notebook["mode"]));
}
