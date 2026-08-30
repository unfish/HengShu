//! GUI 与 CLI 宿主产物的回归：分别打包并验证产物类型、参数类型解析、任意当前目录
//! 下的路径锚定，以及只读安装目录下的结构化失败。无图形环境启动按项目约定不做验收。

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_衡枢")
}

fn coverage_run() -> bool {
    std::env::var_os("CARGO_LLVM_COV").is_some()
}

fn project(directory: &Path, manifest: &str, program: &str) {
    std::fs::write(directory.join("衡枢.toml"), manifest).unwrap();
    std::fs::write(directory.join("主程序.代码"), program).unwrap();
}

fn tool_manifest(parameters: &str) -> String {
    tool_manifest_with_permissions(parameters, "")
}

/// 工具权限只能收紧项目权限，因此需要写文件的夹具在项目段声明文件权限。
fn tool_manifest_with_permissions(parameters: &str, project_permissions: &str) -> String {
    format!(
        r#"["项目"]
"名称" = "宿主夹具"
"版本" = "0.1.0"
"入口" = "主程序.代码"
"许可证" = "MIT"
{project_permissions}

["工具"."宿主工具"]
version = 1
"名称" = "宿主工具"
"工具版本" = "1"
"入口" = "main"
"参数" = {{ {parameters} }}

["工具"."宿主工具".gui]
title = "宿主工具"
width = 640
height = 480
output_root = "结果"

["工具"."宿主工具".cli]
command = "宿主工具"
output_format = "text"
output_root = "结果"
"#
    )
}

fn build(source: &Path, output: &PathBuf, gui: bool) {
    let mut command = Command::new(binary());
    command
        .arg("打包")
        .arg(source)
        .arg("--工具")
        .arg("宿主工具")
        .arg("-o")
        .arg(output);
    if gui {
        command.arg("--界面");
    }
    let build = command.output().unwrap();
    assert!(
        build.status.success(),
        "打包失败：{}",
        String::from_utf8_lossy(&build.stderr)
    );
}

#[test]
fn cli_and_gui_packages_declare_their_own_host() {
    let directory = tempfile::tempdir().unwrap();
    project(
        directory.path(),
        &tool_manifest(""),
        "输出(\"宿主产物就绪\")\n",
    );
    let cli_product = directory.path().join("命令行产物");
    let gui_product = directory.path().join("界面产物");
    build(directory.path(), &cli_product, false);
    if !coverage_run() {
        build(directory.path(), &gui_product, true);
    } else {
        eprintln!("覆盖率运行不装配外部 GUI 宿主；GUI 宿主由正常全目标测试验证");
    }

    let mut products = vec![(&cli_product, "CLI")];
    if !coverage_run() {
        products.push((&gui_product, "GUI"));
    }
    for (product, expected) in products {
        let inspect = Command::new(binary())
            .arg("验证包")
            .arg(product)
            .output()
            .unwrap();
        assert!(
            inspect.status.success(),
            "验证包失败：{}",
            String::from_utf8_lossy(&inspect.stderr)
        );
        let report = String::from_utf8_lossy(&inspect.stdout);
        assert!(report.contains(expected), "验证包未报告宿主类型：{report}");
        assert!(report.contains("宿主工具"), "验证包未报告工具名：{report}");
    }

    let run = Command::new(&cli_product).output().unwrap();
    assert!(
        run.status.success(),
        "命令行产物运行失败：{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "宿主产物就绪");
}

#[test]
fn cli_package_parses_declared_parameter_types_and_rejects_wrong_ones() {
    let directory = tempfile::tempdir().unwrap();
    project(
        directory.path(),
        &tool_manifest(
            r#""数量" = { type_name = "数字", required = true }, "启用" = { type_name = "逻辑" }, "名称" = { type_name = "文本" }"#,
        ),
        "引入 环境\n遍历 环境 程序参数()，记作 项：\n    输出(项)\n结束\n",
    );
    let product = directory.path().join("参数产物");
    build(directory.path(), &product, false);

    let run = Command::new(&product)
        .args(["--参数", "数量=3", "--参数", "启用=是", "--参数", "名称=甲"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "参数产物运行失败：{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let lines = String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(lines.contains(&"数量=3.0".to_string()), "{lines:?}");
    assert!(lines.contains(&"启用=true".to_string()), "{lines:?}");
    assert!(lines.contains(&"名称=\"甲\"".to_string()), "{lines:?}");

    let english = Command::new(&product)
        .args(["--参数", "数量=3", "--参数", "启用=false"])
        .output()
        .unwrap();
    assert!(
        english.status.success(),
        "英文逻辑值必须继续可用：{}",
        String::from_utf8_lossy(&english.stderr)
    );
    assert!(
        String::from_utf8_lossy(&english.stdout).contains("启用=false"),
        "{}",
        String::from_utf8_lossy(&english.stdout)
    );

    let wrong = Command::new(&product)
        .args(["--参数", "数量=三个"])
        .output()
        .unwrap();
    assert!(!wrong.status.success(), "错误类型的参数必须被拒绝");
    let message = String::from_utf8_lossy(&wrong.stderr);
    assert!(message.contains("数量"), "拒绝信息未指出参数名：{message}");

    let missing = Command::new(&product).output().unwrap();
    assert!(!missing.status.success(), "缺少必填参数必须失败");
}

#[test]
fn packaged_tool_anchors_output_to_its_own_directory_from_any_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    project(
        directory.path(),
        &tool_manifest_with_permissions("", "\n[\"权限\"]\n\"文件\" = [\".\"]\n"),
        "引入 文件系统\n文件系统 写入文本(\"结果.txt\", \"完成\")\n输出(\"已写出\")\n",
    );
    let install = directory.path().join("安装目录");
    std::fs::create_dir_all(&install).unwrap();
    let product = install.join("锚定产物");
    build(directory.path(), &product, false);

    let elsewhere = tempfile::tempdir().unwrap();
    let run = Command::new(&product)
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "从其他目录运行失败：{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(install.join("结果.txt")).unwrap(),
        "完成",
        "输出必须落在产物所在目录"
    );
    assert!(
        !elsewhere.path().join("结果.txt").exists(),
        "输出不能落在当前工作目录"
    );
}

#[cfg(unix)]
#[test]
fn read_only_install_directory_fails_with_a_language_error_instead_of_a_panic() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    project(
        directory.path(),
        &tool_manifest_with_permissions("", "\n[\"权限\"]\n\"文件\" = [\".\"]\n"),
        "引入 文件系统\n文件系统 写入文本(\"结果.txt\", \"完成\")\n输出(\"不应到达\")\n",
    );
    let install = directory.path().join("只读安装目录");
    std::fs::create_dir_all(&install).unwrap();
    let product = install.join("只读产物");
    build(directory.path(), &product, false);
    std::fs::set_permissions(&install, std::fs::Permissions::from_mode(0o555)).unwrap();

    let run = Command::new(&product).output().unwrap();
    std::fs::set_permissions(&install, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!run.status.success(), "只读安装目录下写出必须失败");
    let message = String::from_utf8_lossy(&run.stderr);
    assert!(
        message.contains("结果.txt") || message.contains("写入"),
        "失败信息必须指出写入目标：{message}"
    );
    assert!(
        !message.contains("panicked"),
        "只读目录不能导致 panic：{message}"
    );
}
