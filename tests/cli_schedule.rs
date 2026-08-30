//! 排程命令集成测试：无界面单次运行、运行历史落库和自动化声明校验。

mod cli_common;

use std::fs;
use std::process::Command;

fn project(directory: &std::path::Path, automation: &str) {
    fs::create_dir_all(directory.join("src")).unwrap();
    fs::write(
        directory.join("衡枢.toml"),
        format!(
            r#"
["项目"]
"名称" = "排程示例"
"版本" = "0.1.0"
"入口" = "src/主程序.代码"

["工具"."日报"]
version = 1
name = "日报"
tool_version = "1.0.0"
entry = "src/主程序.代码"

["工具"."日报".cli]
command = "日报"
output_format = "text"
output_root = "结果"

{automation}
"#
        ),
    )
    .unwrap();
    fs::write(
        directory.join("src/主程序.代码"),
        "输出（\"排程运行完成\"）\n",
    )
    .unwrap();
}

#[test]
fn schedule_trigger_runs_headless_and_records_history() {
    let directory = tempfile::tempdir().unwrap();
    project(
        directory.path(),
        r#"
["自动化"."日报"]
"时区" = "Asia/Shanghai"
"触发" = { "日历" = { "表达式" = "每周一 08:00" } }
"#,
    );
    let output = Command::new(cli_common::binary())
        .args(["排程", "触发", "--工具", "日报"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("排程运行完成"), "{text}");

    let history = Command::new(cli_common::binary())
        .args(["历史"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&history.stdout);
    assert!(json.contains("\"tool\": \"日报\""), "{json}");
    assert!(json.contains("Succeeded"), "{json}");
}

#[test]
fn packaged_automation_runs_without_source_and_records_history() {
    let directory = tempfile::tempdir().unwrap();
    project(
        directory.path(),
        r#"
["自动化"."日报"]
"时区" = "Asia/Shanghai"
"触发" = { "日历" = { "表达式" = "每周一 08:00" } }
"#,
    );
    let package = directory.path().join("日报工具");
    let built = Command::new(cli_common::binary())
        .args([
            "打包",
            ".",
            "--工具",
            "日报",
            "-o",
            package.to_str().unwrap(),
        ])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    fs::remove_dir_all(directory.path().join("src")).unwrap();
    fs::remove_file(directory.path().join("衡枢.toml")).unwrap();
    let run = Command::new(&package)
        .args(["--衡枢自动化触发", "--衡枢自动化立即", "--工具", "日报"])
        .current_dir("/")
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("排程运行完成"), "{text}");
    let history = directory.path().join(".衡枢运行历史.db");
    assert!(history.is_file());
    let history = hengshu::automation::RunHistory::open(&history, 10).unwrap();
    let records = history.list(10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].tool, "日报");
    assert_eq!(records[0].status, hengshu::automation::RunStatus::Succeeded);
    assert_eq!(records[0].output, ["排程运行完成"]);
    assert!(records[0].trigger_key.is_some());
    assert!(records[0].resources.output_bytes > 0);
}

#[test]
fn schedule_install_requires_an_automation_declaration_and_a_packaged_binary() {
    let directory = tempfile::tempdir().unwrap();
    project(directory.path(), "");
    let missing = Command::new(cli_common::binary())
        .args(["排程", "安装", "--工具", "日报"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&missing.stderr);
    assert!(!missing.status.success());
    assert!(text.contains("没有自动化声明"), "{text}");

    let declared = tempfile::tempdir().unwrap();
    project(
        declared.path(),
        r#"
["自动化"."日报"]
"时区" = "Asia/Shanghai"
"触发" = { "文件夹" = { "根目录" = "待处理", "扩展名" = "xlsx" } }
"#,
    );
    let unpacked = Command::new(cli_common::binary())
        .args(["排程", "安装", "--工具", "日报"])
        .current_dir(declared.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&unpacked.stderr);
    assert!(!unpacked.status.success());
    assert!(text.contains("已打包的 CLI 产物"), "{text}");
}

#[test]
fn schedule_rejects_unknown_subcommands_and_invalid_timezones() {
    let directory = tempfile::tempdir().unwrap();
    project(
        directory.path(),
        r#"
["自动化"."日报"]
"时区" = "不存在/时区"
"触发" = { "日历" = { "表达式" = "每周一 08:00" } }
"#,
    );
    let invalid = Command::new(cli_common::binary())
        .args(["排程", "安装", "--工具", "日报"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("时区无效"),
        "{}",
        String::from_utf8_lossy(&invalid.stderr)
    );

    let unknown = Command::new(cli_common::binary())
        .args(["排程", "重启", "--工具", "日报"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(!unknown.status.success());
}
