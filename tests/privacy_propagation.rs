//! 隐私传播测试：列敏感标签在表格运算、跨表连接、任务/通道传递和序列化后不会消失，
//! 端点拒绝的错误消息也不包含敏感值。

mod cli_common;
mod common;

use std::fs;
use std::process::Command;

fn project(directory: &std::path::Path, endpoints: &str, program: &str) -> std::path::PathBuf {
    fs::write(
        directory.join("衡枢.toml"),
        format!(
            "[\"项目\"]\n\"名称\" = \"隐私\"\n\"版本\" = \"0.1.0\"\n\"入口\" = \"主程序.代码\"\n\n[\"权限\"]\n\"文件\" = [\".\"]\n\"允许敏感类别\" = [\"Identity\"]\n\"允许端点\" = [{endpoints}]\n"
        ),
    )
    .unwrap();
    let path = directory.join("主程序.代码");
    fs::write(&path, program).unwrap();
    path
}

fn run(source: &std::path::Path) -> (bool, String) {
    let output = Command::new(cli_common::binary())
        .args(["运行", source.to_str().unwrap()])
        .current_dir(source.parent().unwrap())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

const TABLE: &str = "引入 表格处理\n定义 员工 为 表格处理 把记录整理成表格([{编号: \"E1\", 身份证: \"110101199001011234\"}])\n定义 标注 为 员工 设置列敏感(\"身份证\", \"身份\")\n";

#[test]
fn column_labels_survive_selection_sorting_and_deduplication() {
    let directory = tempfile::tempdir().unwrap();
    let source = project(
        directory.path(),
        "\"Stdout\"",
        &format!(
            "{TABLE}定义 结果 为 标注 选择列([\"身份证\"]) 排序(\"身份证\") 去重(\"身份证\")\n输出(结果 列敏感(\"身份证\") 的 类别)\n"
        ),
    );
    let (success, text) = run(&source);
    assert!(success, "{text}");
    assert!(text.contains("身份"), "{text}");
}

#[test]
fn labels_from_either_side_propagate_across_join() {
    let directory = tempfile::tempdir().unwrap();
    let source = project(
        directory.path(),
        "\"Stdout\"",
        &format!(
            "{TABLE}定义 工资 为 表格处理 把记录整理成表格([{{编号: \"E1\", 金额: 100}}])\n定义 合并 为 工资 连接(标注, \"编号\")\n输出(合并 列敏感(\"身份证\") 的 类别)\n"
        ),
    );
    let (success, text) = run(&source);
    assert!(success, "{text}");
    assert!(text.contains("身份"), "{text}");
}

#[test]
fn labels_survive_channel_transfer() {
    let directory = tempfile::tempdir().unwrap();
    let source = project(
        directory.path(),
        "\"Stdout\"",
        &format!(
            "{TABLE}引入 并发\n定义 通 为 并发 通道(1)\n通 发送(标注)\n定义 收到 为 通 接收()\n输出(收到 列敏感(\"身份证\") 的 类别)\n"
        ),
    );
    let (success, text) = run(&source);
    assert!(success, "{text}");
    assert!(text.contains("身份"), "{text}");
}

#[test]
fn unauthorized_endpoint_refuses_export_without_leaking_values() {
    let directory = tempfile::tempdir().unwrap();
    let source = project(
        directory.path(),
        "\"Stdout\"",
        &format!("{TABLE}表格处理 写入表格(标注, \"导出.csv\")\n"),
    );
    let (success, text) = run(&source);
    assert!(!success, "{text}");
    assert!(text.contains("端点未授权"), "{text}");
    assert!(!text.contains("110101199001011234"), "{text}");
    assert!(!directory.path().join("导出.csv").exists());
}

#[test]
fn masked_column_passes_the_same_endpoint() {
    let directory = tempfile::tempdir().unwrap();
    let source = project(
        directory.path(),
        "\"Stdout\"",
        &format!("{TABLE}表格处理 写入表格(标注 标记列脱敏(\"身份证\"), \"导出.csv\")\n输出(\"已导出\")\n"),
    );
    let (success, text) = run(&source);
    assert!(success, "{text}");
    assert!(directory.path().join("导出.csv").exists(), "{text}");
}

#[test]
fn declared_endpoint_allows_export() {
    let directory = tempfile::tempdir().unwrap();
    let source = project(
        directory.path(),
        "\"Stdout\", \"LocalFile\"",
        &format!("{TABLE}表格处理 写入表格(标注, \"导出.csv\")\n输出(\"已导出\")\n"),
    );
    let (success, text) = run(&source);
    assert!(success, "{text}");
    assert!(directory.path().join("导出.csv").exists(), "{text}");
}

#[test]
fn label_methods_agree_across_three_paths() {
    let output = common::assert_three_paths_output(
        "引入 表格处理\n定义 员工 为 表格处理 把记录整理成表格([{编号: \"E1\", 身份证: \"110101199001011234\"}])\n定义 标注 为 员工 设置列敏感(\"身份证\", \"身份\", \"自定义\")\n输出(标注 列敏感(\"身份证\") 的 类别)\n输出(标注 标记列假名(\"身份证\") 列敏感(\"身份证\") 的 已假名化)",
    );
    assert_eq!(output, ["[身份, 自定义]", "是"]);

    let failure = common::assert_three_paths_failure(
        "引入 表格处理\n定义 员工 为 表格处理 把记录整理成表格([{编号: \"E1\"}])\n员工 设置列敏感(\"缺失列\", \"身份\")",
    );
    assert!(failure.message().contains("表格不存在列"), "{failure:?}");
}
