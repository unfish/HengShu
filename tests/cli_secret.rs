//! 秘密参数不落地测试：诊断、结构化诊断文件和运行历史都不得出现秘密值。

mod cli_common;

use std::fs;
use std::process::Command;

fn project(directory: &std::path::Path) {
    fs::create_dir_all(directory.join("src")).unwrap();
    fs::write(
        directory.join("衡枢.toml"),
        r#"
["项目"]
"名称" = "秘密示例"
"版本" = "0.1.0"
"入口" = "src/主程序.代码"

["工具"."导出"]
version = 1
name = "导出"
tool_version = "1.0.0"
entry = "src/主程序.代码"

["工具"."导出"."参数"."口令"]
type_name = "秘密"
required = true

["自动化"."导出"]
"时区" = "Asia/Shanghai"
"触发" = { "日历" = { "表达式" = "每周一 08:00" } }
"#,
    )
    .unwrap();
    // 程序把全部参数当作路径读取，让参数值进入权限诊断，从而检验清理是否生效。
    fs::write(
        directory.join("src/主程序.代码"),
        "引入 环境\n引入 文件系统\n定义 参数们 为 环境 程序参数()\n文件系统 读取文本(参数们)\n",
    )
    .unwrap();
}

#[test]
fn secret_parameters_are_scrubbed_from_diagnostics_and_report_files() {
    let directory = tempfile::tempdir().unwrap();
    project(directory.path());
    let report = directory.path().join("诊断.json");
    let output = Command::new(cli_common::binary())
        .args([
            "运行",
            ".",
            "--工具",
            "导出",
            "--参数",
            "口令=超级机密口令值",
        ])
        .env("HENGSHU_DIAGNOSTIC_FILE", &report)
        .current_dir(directory.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(!stderr.contains("超级机密口令值"), "{stderr}");
    assert!(stderr.contains("秘密已隐藏"), "{stderr}");
    let json = fs::read_to_string(&report).unwrap();
    assert!(!json.contains("超级机密口令值"), "{json}");
    assert!(json.contains("秘密已隐藏"), "{json}");
}

#[test]
fn schedule_history_records_never_contain_secret_values() {
    let directory = tempfile::tempdir().unwrap();
    project(directory.path());
    let trigger = Command::new(cli_common::binary())
        .args([
            "排程",
            "触发",
            "--工具",
            "导出",
            "--参数",
            "口令=超级机密口令值",
        ])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(!trigger.status.success());
    assert!(!String::from_utf8_lossy(&trigger.stderr).contains("超级机密口令值"));

    let history = Command::new(cli_common::binary())
        .args(["历史"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&history.stdout);
    assert!(json.contains("Failed"), "{json}");
    assert!(!json.contains("超级机密口令值"), "{json}");
    assert!(json.contains("秘密已隐藏"), "{json}");
}
