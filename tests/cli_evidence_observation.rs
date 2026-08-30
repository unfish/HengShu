//! 凭证观察登记测试：输入指纹来自真实读取，时间/随机/网络等边界降级为指纹验证。

mod cli_common;

use std::fs;
use std::process::Command;

fn write(directory: &std::path::Path, program: &str) -> std::path::PathBuf {
    let path = directory.join("主程序.代码");
    fs::write(&path, program).unwrap();
    path
}

#[test]
fn evidence_records_read_inputs_and_output_digest() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("台账.csv"), "科目,金额\n差旅,100\n").unwrap();
    let program = write(
        directory.path(),
        "引入 文件系统\n定义 文本 为 文件系统 读取文本(\"台账.csv\")\n输出(文本)\n",
    );
    let evidence = directory.path().join("运行.凭证");
    let output = Command::new(cli_common::binary())
        .args([
            "存证",
            program.to_str().unwrap(),
            "-o",
            evidence.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = fs::read_to_string(&evidence).unwrap();
    assert!(json.contains("\"logical_name\": \"台账.csv\""), "{json}");
    assert!(json.contains("\"kind\": \"标准输出\""), "{json}");
    assert!(json.contains("\"level\": \"Replayed\""), "{json}");
    assert!(!json.contains("\"kind\": \"runtime\""), "{json}");

    let replay = Command::new(cli_common::binary())
        .args([
            "复算",
            evidence.to_str().unwrap(),
            "--输入根",
            directory.path().to_str().unwrap(),
            "--输入=台账.csv=台账.csv",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&replay.stdout).to_string()
        + &String::from_utf8_lossy(&replay.stderr);
    assert!(replay.status.success(), "{text}");
    assert!(text.contains("复算成功"), "{text}");
}

#[test]
fn changed_input_bytes_fail_replay_before_execution() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("台账.csv"), "科目,金额\n差旅,100\n").unwrap();
    let program = write(
        directory.path(),
        "引入 文件系统\n定义 文本 为 文件系统 读取文本(\"台账.csv\")\n输出(文本)\n",
    );
    let evidence = directory.path().join("运行.凭证");
    Command::new(cli_common::binary())
        .args([
            "存证",
            program.to_str().unwrap(),
            "-o",
            evidence.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    fs::write(directory.path().join("台账.csv"), "科目,金额\n差旅,101\n").unwrap();
    let replay = Command::new(cli_common::binary())
        .args([
            "复算",
            evidence.to_str().unwrap(),
            "--输入根",
            directory.path().to_str().unwrap(),
            "--输入=台账.csv=台账.csv",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&replay.stderr);
    assert!(!replay.status.success(), "{text}");
    assert!(
        text.contains("指纹不一致") || text.contains("不一致"),
        "{text}"
    );
}

#[test]
fn replay_works_from_a_moved_directory_after_the_source_is_deleted() {
    let original = tempfile::tempdir().unwrap();
    fs::write(original.path().join("台账.csv"), "科目,金额\n差旅,100\n").unwrap();
    let program = write(
        original.path(),
        "引入 文件系统\n定义 文本 为 文件系统 读取文本(\"台账.csv\")\n输出(文本)\n",
    );
    let evidence = original.path().join("运行.凭证");
    let stored = Command::new(cli_common::binary())
        .args([
            "存证",
            program.to_str().unwrap(),
            "-o",
            evidence.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        stored.status.success(),
        "{}",
        String::from_utf8_lossy(&stored.stderr)
    );

    let moved = tempfile::tempdir().unwrap();
    fs::copy(&evidence, moved.path().join("运行.凭证")).unwrap();
    fs::copy(
        original.path().join("台账.csv"),
        moved.path().join("台账.csv"),
    )
    .unwrap();
    fs::remove_file(&program).unwrap();
    fs::remove_file(original.path().join("台账.csv")).unwrap();

    let replay = Command::new(cli_common::binary())
        .args([
            "复算",
            moved.path().join("运行.凭证").to_str().unwrap(),
            "--输入根",
            moved.path().to_str().unwrap(),
            "--输入=台账.csv=台账.csv",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&replay.stdout).to_string()
        + &String::from_utf8_lossy(&replay.stderr);
    assert!(replay.status.success(), "{text}");
    assert!(text.contains("复算成功"), "{text}");
}

#[test]
fn unreplayable_boundaries_are_downgraded_to_fingerprint_verification() {
    let directory = tempfile::tempdir().unwrap();
    let program = write(
        directory.path(),
        "引入 日期时间\n定义 此刻 为 日期时间 现在()\n输出(\"完成\")\n",
    );
    let evidence = directory.path().join("运行.凭证");
    let output = Command::new(cli_common::binary())
        .args([
            "存证",
            program.to_str().unwrap(),
            "-o",
            evidence.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = fs::read_to_string(&evidence).unwrap();
    assert!(json.contains("\"kind\": \"时间\""), "{json}");
    assert!(json.contains("FingerprintOnly"), "{json}");

    let replay = Command::new(cli_common::binary())
        .args([
            "复算",
            evidence.to_str().unwrap(),
            "--输入根",
            directory.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&replay.stderr);
    assert!(!replay.status.success(), "{text}");
    assert!(text.contains("不能声称完整复算"), "{text}");
}
