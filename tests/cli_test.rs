use std::fs;
use std::process::Command;

mod cli_common;
use cli_common::binary;

#[test]
fn chinese_test_command_supports_files_directories_projects_and_automation_options() {
    let directory = tempfile::tempdir().unwrap();
    let single = directory.path().join("单项.代码");
    fs::write(&single, "测试 \"单文件\"：\n断言相等(2 + 2, 4)\n结束\n").unwrap();
    let single_run = Command::new(binary())
        .args(["测试", single.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        single_run.status.success(),
        "{}",
        String::from_utf8_lossy(&single_run.stderr)
    );
    assert!(String::from_utf8_lossy(&single_run.stdout).contains("1 项通过"));

    let tests = directory.path().join("自动化测试");
    fs::create_dir_all(tests.join("财务")).unwrap();
    fs::write(
        tests.join("财务/金额.代码"),
        "测试 \"金额边界\"：\n断言接近(10 / 3, 3.333, 允许误差 为 0.001)\n结束\n",
    )
    .unwrap();
    fs::write(
        tests.join("统计.代码"),
        "测试 \"统计通过\"：\n断言不为空([1])\n结束\n",
    )
    .unwrap();
    let listed = Command::new(binary())
        .arg("测试")
        .arg("--列出")
        .arg(&tests)
        .output()
        .unwrap();
    assert!(listed.status.success());
    let listed_output = String::from_utf8_lossy(&listed.stdout);
    assert!(listed_output.contains("财务/金额.代码 :: 金额边界"));
    assert!(listed_output.contains("统计.代码 :: 统计通过"));

    let filtered = Command::new(binary())
        .arg("测试")
        .arg(&tests)
        .args(["--筛选", "统计"])
        .output()
        .unwrap();
    assert!(filtered.status.success());
    assert!(String::from_utf8_lossy(&filtered.stdout).contains("1 项通过"));

    let project = directory.path().join("项目");
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(project.join("衡枢.toml"), "").unwrap();
    fs::write(
        project.join("tests/项目测试.代码"),
        "测试 \"项目根\"：\n断言(是)\n结束\n",
    )
    .unwrap();
    let project_run = Command::new(binary())
        .args(["测试", project.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(project_run.status.success());
    assert!(String::from_utf8_lossy(&project_run.stdout).contains("1 项通过"));

    let empty = directory.path().join("空目录");
    fs::create_dir(&empty).unwrap();
    assert!(!Command::new(binary())
        .args(["测试", empty.to_str().unwrap()])
        .output()
        .unwrap()
        .status
        .success());
    assert!(Command::new(binary())
        .args(["测试", empty.to_str().unwrap(), "--允许空测试"])
        .output()
        .unwrap()
        .status
        .success());
}
