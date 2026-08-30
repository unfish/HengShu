use std::fs;
use std::process::Command;

mod cli_common;
use cli_common::binary;

#[test]
fn sqlite_streaming_query_is_lazy_at_the_language_boundary() {
    let root = env!("CARGO_MANIFEST_DIR");
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("数据库流.代码");
    fs::write(
        &source,
        "引入 数据库\n定义 连接 为 数据库 打开SQLite(\":memory:\")\n连接 执行(\"CREATE TABLE 数据(编号 INTEGER)\")\n连接 批量执行(\"INSERT INTO 数据 VALUES (?)\", [[1], [2], [3]])\n定义 数据流 为 连接 流式查询(\"SELECT 编号 FROM 数据 ORDER BY 编号\")\n输出(长度(数据流 收集()))\n",
    )
    .unwrap();
    let run = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "3");
}

#[test]
fn modules_can_hide_internal_bindings_with_chinese_visibility() {
    let root = env!("CARGO_MANIFEST_DIR");
    let directory = tempfile::tempdir().unwrap();
    let module = directory.path().join("财务模块.代码");
    let public_source = directory.path().join("公开接口.代码");
    fs::write(
        &module,
        "内部 定义 密钥 为 \"不可导出\"\n公开 定义 名称 为 \"衡枢\"\n",
    )
    .unwrap();
    fs::write(&public_source, "引入 财务模块\n输出(财务模块 的 名称)\n").unwrap();
    let public_run = Command::new(binary())
        .args(["运行", public_source.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        public_run.status.success(),
        "{}",
        String::from_utf8_lossy(&public_run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&public_run.stdout).trim(), "衡枢");

    fs::write(&public_source, "引入 财务模块\n输出(财务模块 的 密钥)\n").unwrap();
    let denied = Command::new(binary())
        .args(["运行", public_source.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("没有“密钥”"));
}

#[test]
fn benchmark_command_enforces_average_threshold() {
    let root = env!("CARGO_MANIFEST_DIR");
    let pass = Command::new(binary())
        .args([
            "基准",
            "examples/成绩分析.代码",
            "--次数",
            "1",
            "--最大平均毫秒",
            "5000",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        pass.status.success(),
        "{}",
        String::from_utf8_lossy(&pass.stderr)
    );

    let fail = Command::new(binary())
        .args([
            "基准",
            "examples/成绩分析.代码",
            "--次数",
            "1",
            "--最大平均毫秒",
            "0",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!fail.status.success());
    assert!(String::from_utf8_lossy(&fail.stderr).contains("基准回归"));
}

#[test]
fn differently_spelled_imports_load_one_module_and_still_detect_cycles() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("子目录");
    let source = directory.path().join("主程序.代码");
    let output = directory.path().join("同一模块程序");
    fs::create_dir(&nested).unwrap();
    fs::write(
        nested.join("共享.代码"),
        "输出(\"共享顶层\")\n公开 定义 值 为 3\n",
    )
    .unwrap();
    fs::write(
        nested.join("邻居.代码"),
        "引入 共享\n公开 定义 转发 为 共享 的 值\n",
    )
    .unwrap();
    fs::write(
        &source,
        "从 子目录/共享 引入 值\n从 子目录/邻居 引入 转发\n输出(值, 转发)\n",
    )
    .unwrap();
    let run = Command::new(binary())
        .arg("运行")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout)
            .lines()
            .collect::<Vec<_>>(),
        ["共享顶层", "3 3"]
    );

    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let packaged = Command::new(&output).output().unwrap();
    assert!(
        packaged.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&packaged.stdout)
            .lines()
            .collect::<Vec<_>>(),
        ["共享顶层", "3 3"]
    );

    let cycle_source = directory.path().join("循环主.代码");
    fs::write(nested.join("环甲.代码"), "引入 环乙\n").unwrap();
    fs::write(nested.join("环乙.代码"), "引入 环甲\n").unwrap();
    fs::write(&cycle_source, "引入 子目录/环甲\n").unwrap();
    for arguments in [
        vec!["运行".to_string(), cycle_source.display().to_string()],
        vec![
            "打包".to_string(),
            cycle_source.display().to_string(),
            "-o".to_string(),
            directory.path().join("循环程序").display().to_string(),
        ],
    ] {
        let rejected = Command::new(binary()).args(&arguments).output().unwrap();
        assert!(!rejected.status.success(), "{arguments:?}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("模块循环依赖"),
            "{arguments:?}\n{}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }
}

#[test]
fn project_dependency_runs_and_is_embedded_when_packaged() {
    let directory = tempfile::tempdir().unwrap();
    let dependency = directory.path().join("财务依赖");
    let project = directory.path().join("分析项目");

    for (name, path) in [("财务依赖", &dependency), ("分析项目", &project)] {
        let created = Command::new(binary())
            .arg("新建")
            .arg(name)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
    }
    fs::write(
        dependency.join("src/主程序.代码"),
        "定义功能 计算税后（金额）：\n    返回 金额 * 0.87\n结束\n",
    )
    .unwrap();
    fs::write(
        project.join("src/主程序.代码"),
        "引入 财务依赖\n输出(财务依赖 计算税后(100))\n",
    )
    .unwrap();
    let added = Command::new(binary())
        .args([
            "添加",
            "财务依赖",
            "--路径",
            "../财务依赖",
            "--版本",
            "^0.1",
        ])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let run = Command::new(binary())
        .arg("运行")
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "87");

    let output = directory.path().join("分析程序");
    let packed = Command::new(binary())
        .arg("打包")
        .arg(&project)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    fs::remove_dir_all(&dependency).unwrap();
    fs::remove_dir_all(&project).unwrap();
    let standalone = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        standalone.status.success(),
        "{}",
        String::from_utf8_lossy(&standalone.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&standalone.stdout).trim(), "87");
}
