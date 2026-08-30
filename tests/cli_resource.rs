use std::fs;
use std::process::Command;

mod cli_common;
use cli_common::binary;

#[test]
fn file_permissions_default_to_source_directory() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("权限测试.代码");
    let local = directory.path().join("本地.txt");
    fs::write(
        directory.path().join("衡枢.toml"),
        "[\"项目\"]\n\"名称\" = \"权限测试\"\n\"版本\" = \"0.1.0\"\n\"入口\" = \"权限测试.代码\"\n\n[\"权限\"]\n\"限制能力\" = true\n",
    )
    .unwrap();
    fs::write(&local, "允许读取").unwrap();
    fs::write(
        &source,
        "引入 文件系统\n输出(文件系统 读取文本(\"本地.txt\"))\n",
    )
    .unwrap();
    let run = Command::new(binary())
        .arg("运行")
        .arg(&source)
        .current_dir("/")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "允许读取");

    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        &source,
        format!(
            "引入 文件系统\n输出(文件系统 读取文本(\"{}\"))\n",
            outside.path().display()
        ),
    )
    .unwrap();
    let denied = Command::new(binary())
        .arg("运行")
        .arg(&source)
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("没有访问文件路径"));
}

/// 切换目录必须按被授权的同一个解析结果执行：否则看似相对沙箱的写法可以逃出沙箱，
/// 或授权通过后进程切到了另一个目录。
#[test]
fn changing_directory_authorizes_the_path_it_actually_enters() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("切换目录.代码");
    let nested = directory.path().join("子目录");
    fs::write(
        directory.path().join("衡枢.toml"),
        "[\"项目\"]\n\"名称\" = \"切换目录\"\n\"版本\" = \"0.1.0\"\n\"入口\" = \"切换目录.代码\"\n\n[\"权限\"]\n\"限制能力\" = true\n",
    )
    .unwrap();
    fs::create_dir(&nested).unwrap();
    fs::write(
        &source,
        "引入 环境\n环境 切换目录(\"子目录\")\n输出(环境 当前目录())\n",
    )
    .unwrap();
    let run = Command::new(binary())
        .arg("运行")
        .arg(&source)
        .current_dir("/")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        fs::canonicalize(&nested).unwrap().display().to_string()
    );

    fs::write(
        &source,
        "引入 环境\n环境 切换目录(\"子目录/../../\")\n输出(环境 当前目录())\n",
    )
    .unwrap();
    let denied = Command::new(binary())
        .arg("运行")
        .arg(&source)
        .current_dir("/")
        .output()
        .unwrap();
    assert!(
        !denied.status.success(),
        "{}",
        String::from_utf8_lossy(&denied.stdout)
    );
    assert!(String::from_utf8_lossy(&denied.stderr).contains("没有访问文件路径"));
}
