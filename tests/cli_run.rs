use std::fs;
use std::process::Command;

mod cli_common;
use cli_common::{assert_single_location, binary};

#[test]
fn chinese_cli_checks_and_runs_code_files() {
    let root = env!("CARGO_MANIFEST_DIR");
    let check = Command::new(binary())
        .args(["检查", "examples/成绩分析.代码"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("语法检查通过"));

    let run = Command::new(binary())
        .args(["运行", "examples/闭包核算.代码"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "12.5");
}

#[test]
fn cli_run_can_write_a_static_notebook_report() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("过程.代码");
    let report = directory.path().join("过程.json");
    std::fs::write(&source, "定义 值 为 1\n输出(值)\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_衡枢");
    let output = Command::new(binary)
        .args([
            "运行",
            source.to_str().unwrap(),
            "--观察",
            "快照",
            "--过程报告",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let notebook: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert_eq!(notebook["version"], 1);
    assert!(!notebook["events"].as_array().unwrap().is_empty());
}

#[test]
fn cli_run_writes_the_html_viewer_when_the_report_path_ends_with_html() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("过程.代码");
    let report = directory.path().join("过程报告.html");
    std::fs::write(&source, "定义 值 为 1\n输出(值)\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_衡枢"))
        .args([
            "运行",
            source.to_str().unwrap(),
            "--观察",
            "快照",
            "--过程报告",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = std::fs::read_to_string(report).unwrap();
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(html.contains("衡枢执行过程"), "{html}");
    assert!(!html.contains("<script"), "{html}");
}

#[test]
fn cli_evidence_stores_and_replays_source_without_source_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("凭证源.代码");
    let evidence = directory.path().join("运行.凭证");
    std::fs::write(&source, "输出(42)\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_衡枢");
    let stored = Command::new(binary)
        .args([
            "存证",
            source.to_str().unwrap(),
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
    std::fs::remove_file(&source).unwrap();
    let replay = Command::new(binary)
        .args([
            "复算",
            evidence.to_str().unwrap(),
            "--输入根",
            directory.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let bytecode_replay = Command::new(binary)
        .args([
            "复算",
            evidence.to_str().unwrap(),
            "--输入根",
            directory.path().to_str().unwrap(),
            "--字节码",
        ])
        .output()
        .unwrap();
    assert!(
        bytecode_replay.status.success(),
        "{}",
        String::from_utf8_lossy(&bytecode_replay.stderr)
    );
    assert!(
        String::from_utf8_lossy(&bytecode_replay.stdout).contains("字节码载荷"),
        "{}",
        String::from_utf8_lossy(&bytecode_replay.stdout)
    );
}

#[test]
fn native_errors_render_one_location_line_on_every_execution_path() {
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        ("除零", "输出(1 / 0)\n", 1usize, "不能除以零。"),
        (
            "越界",
            "定义 数列 为 [1, 2]\n输出(数列[5])\n",
            2,
            "下标越界。",
        ),
        ("缺模块", "引入 不存在的模块\n", 1, "找不到模块"),
    ];

    for (name, code, line, fragment) in cases {
        let source = directory.path().join(format!("{name}.代码"));
        fs::write(&source, code).unwrap();
        let interpreted = Command::new(binary())
            .args(["运行", source.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(interpreted.status.code(), Some(1), "{name}");
        assert_single_location(
            &String::from_utf8_lossy(&interpreted.stderr),
            line,
            fragment,
        );
    }

    let packaged_source = directory.path().join("打包除零.代码");
    fs::write(&packaged_source, "输出(1 / 0)\n").unwrap();
    let packaged = directory.path().join("打包除零程序");
    let compiled = Command::new(binary())
        .arg("打包")
        .arg(&packaged_source)
        .arg("-o")
        .arg(&packaged)
        .output()
        .unwrap();
    assert_eq!(compiled.status.code(), Some(0));
    let packaged_run = Command::new(&packaged).output().unwrap();
    assert_eq!(packaged_run.status.code(), Some(1));
    assert_single_location(
        &String::from_utf8_lossy(&packaged_run.stderr),
        1,
        "不能除以零。",
    );
}

#[test]
fn deep_recursion_returns_language_error_in_a_child_process() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("递归.代码");
    fs::write(&source, "定义功能 无限（）：\n返回 无限()\n结束\n无限()\n").unwrap();
    let result = Command::new(binary())
        .args(["运行", source.to_str().unwrap(), "--最大调用深度", "8"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("功能调用深度超过资源限制"));
}

#[test]
fn default_call_depth_budget_returns_language_error_on_every_execution_path() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("默认深度.代码");
    fs::write(&source, "定义功能 无限（）：\n返回 无限()\n结束\n无限()\n").unwrap();

    let interpreted = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(interpreted.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&interpreted.stderr).contains("功能调用深度超过资源限制"),
        "{}",
        String::from_utf8_lossy(&interpreted.stderr)
    );

    let packaged = directory.path().join("默认深度程序");
    let compiled = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&packaged)
        .output()
        .unwrap();
    assert_eq!(compiled.status.code(), Some(0));
    let packaged_run = Command::new(&packaged).output().unwrap();
    assert_eq!(packaged_run.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&packaged_run.stderr).contains("功能调用深度超过资源限制"),
        "{}",
        String::from_utf8_lossy(&packaged_run.stderr)
    );

    let suite = directory.path().join("默认深度测试.代码");
    fs::write(
        &suite,
        "定义功能 无限（）：\n返回 无限()\n结束\n测试 \"默认深度\"：\n无限()\n结束\n",
    )
    .unwrap();
    let tested = Command::new(binary())
        .args(["测试", suite.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(tested.status.code(), Some(1));
    let tested_text = format!(
        "{}{}",
        String::from_utf8_lossy(&tested.stdout),
        String::from_utf8_lossy(&tested.stderr)
    );
    assert!(
        tested_text.contains("功能调用深度超过资源限制"),
        "{tested_text}"
    );
}

#[test]
fn flat_deep_expressions_report_a_syntax_depth_error_on_check_build_and_run() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("深表达式.代码");
    let terms = std::iter::repeat_n("1", 3000)
        .collect::<Vec<_>>()
        .join(" + ");
    fs::write(&source, format!("定义 值 为 {terms}\n输出(值)\n")).unwrap();
    let packaged = directory.path().join("深表达式程序");
    let commands = [
        vec!["检查".to_string(), source.display().to_string()],
        vec![
            "打包".to_string(),
            source.display().to_string(),
            "-o".to_string(),
            packaged.display().to_string(),
        ],
        vec!["运行".to_string(), source.display().to_string()],
    ];
    let mut rendered = Vec::new();
    for command in commands {
        let result = Command::new(binary()).args(&command).output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.status.code(), Some(1), "{command:?}：{text}");
        assert!(text.contains("语法嵌套深度超过"), "{command:?}：{text}");
        rendered.push(text);
    }
    assert!(
        rendered.windows(2).all(|pair| pair[0] == pair[1]),
        "三条入口对同一解析错误的渲染不一致：{rendered:?}"
    );
    assert!(!packaged.exists());
}

#[test]
fn long_operator_chains_within_the_budget_check_build_and_run() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("长运算链.代码");
    let terms = std::iter::repeat_n("1", 500)
        .collect::<Vec<_>>()
        .join(" + ");
    fs::write(&source, format!("定义 值 为 {terms}\n输出(值)\n")).unwrap();
    let packaged = directory.path().join("长运算链程序");
    for command in [
        vec!["检查".to_string(), source.display().to_string()],
        vec![
            "打包".to_string(),
            source.display().to_string(),
            "-o".to_string(),
            packaged.display().to_string(),
        ],
        vec!["运行".to_string(), source.display().to_string()],
    ] {
        let result = Command::new(binary()).args(&command).output().unwrap();
        assert!(
            result.status.success(),
            "{command:?}：{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let run = Command::new(&packaged).output().unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "500");
}

#[test]
fn deep_equality_finally_compilation_and_vm_cycles_never_crash_child_processes() {
    let directory = tempfile::tempdir().unwrap();

    let nested = format!("{}1{}", "[".repeat(24), "]".repeat(24));
    let equality_source = directory.path().join("深层相等.代码");
    fs::write(
        &equality_source,
        format!("定义 左 为 {nested}\n定义 右 为 {nested}\n输出(左 = 右)\n"),
    )
    .unwrap();
    let equality = Command::new(binary())
        .args(["运行", equality_source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(equality.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&equality.stdout).trim(), "是");

    let finally_source = directory.path().join("最终完成.代码");
    let finally_output = directory.path().join("最终完成程序");
    fs::write(
        &finally_source,
        "定义功能 计算（）：\n尝试：\n返回 1\n最终：\n返回 2\n结束\n结束\n输出(计算())\n",
    )
    .unwrap();
    let compiled = Command::new(binary())
        .arg("打包")
        .arg(&finally_source)
        .arg("-o")
        .arg(&finally_output)
        .output()
        .unwrap();
    assert_eq!(compiled.status.code(), Some(0));
    let finally_run = Command::new(&finally_output).output().unwrap();
    assert_eq!(finally_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&finally_run.stdout).trim(), "2");

    let cycle_source = directory.path().join("自引用.代码");
    let cycle_output = directory.path().join("自引用程序");
    fs::write(
        &cycle_source,
        "定义 容器 为 [空]\n容器[1] 为 容器\n输出(容器)\n",
    )
    .unwrap();
    let compiled = Command::new(binary())
        .arg("打包")
        .arg(&cycle_source)
        .arg("-o")
        .arg(&cycle_output)
        .output()
        .unwrap();
    assert_eq!(compiled.status.code(), Some(0));
    let cycle = Command::new(&cycle_output).output().unwrap();
    assert_eq!(cycle.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&cycle.stderr).contains("容器不能直接或间接包含自身"),
        "{}",
        String::from_utf8_lossy(&cycle.stderr)
    );
}

#[test]
fn command_line_and_native_edge_cases_return_language_errors_instead_of_panicking() {
    let directory = tempfile::tempdir().unwrap();
    for arguments in [vec!["运行"], vec!["检查"], vec!["打包"], vec!["测试"]] {
        let output = Command::new(binary())
            .args(&arguments)
            .current_dir(directory.path())
            .output()
            .unwrap();
        let code = output.status.code();
        assert!(
            code == Some(1) || code == Some(2),
            "{arguments:?} 退出码 {code:?}：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let source = directory.path().join("十六进制.代码");
    fs::write(&source, "引入 编码\n输出(编码 十六进制解码(\"中中\"))\n").unwrap();
    let output = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("不是十六进制字符"), "{stderr}");

    let source = directory.path().join("替换.代码");
    fs::write(
        &source,
        "引入 文本处理\n输出(文本处理 替换(\"abc\", \"a\"))\n",
    )
    .unwrap();
    let output = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("文本处理.替换"), "{stderr}");
}

#[test]
fn detached_tasks_are_reclaimed_before_the_program_exits() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("游离任务.代码");
    fs::write(
        &source,
        "引入 并发\n并发 启动(定义功能（）：\n    引入 并发\n    引入 文件系统\n    并发 休眠(300)\n    文件系统 写入文本(\"游离产出.txt\", \"已回收\")\n结束)\n输出(\"主程序结束\")\n",
    )
    .unwrap();
    let run = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let produced = directory.path().join("游离产出.txt");
    assert_eq!(fs::read_to_string(&produced).unwrap(), "已回收");

    fs::remove_file(&produced).unwrap();
    let packaged = directory.path().join("游离任务程序");
    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&packaged)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let packaged_run = Command::new(&packaged).output().unwrap();
    assert!(
        packaged_run.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged_run.stderr)
    );
    assert_eq!(fs::read_to_string(&produced).unwrap(), "已回收");
}

/// 端点授权属于权限上限：源码路径和独立包（VM）路径都必须在敏感数据外发前拒绝。
#[test]
fn endpoint_authority_is_enforced_on_both_the_source_and_packaged_paths() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("名单.代码");
    fs::write(
        &source,
        "引入 表格处理\n\
定义 名单 为 表格处理 把记录整理成表格([{姓名: \"张三\"}])\n\
输出(名单 设置列敏感(\"姓名\", \"Identity\"))\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("衡枢.toml"),
        "[\"项目\"]\n\
\"名称\" = \"端点\"\n\
\"版本\" = \"0.1.0\"\n\
\"入口\" = \"名单.代码\"\n\
\"许可证\" = \"MIT\"\n\n\
[\"权限\"]\n\
\"允许端点\" = [\"Pdf\"]\n\
\"允许敏感类别\" = [\"Identity\"]\n",
    )
    .unwrap();

    let run = Command::new(binary())
        .arg("运行")
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!run.status.success(), "源码路径应拒绝未授权端点");
    let source_error = String::from_utf8_lossy(&run.stderr).to_string();
    assert!(source_error.contains("端点未授权"), "{source_error}");

    let packaged = directory.path().join("名单程序");
    let packed = Command::new(binary())
        .arg("打包")
        .arg(directory.path())
        .arg("-o")
        .arg(&packaged)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let packaged_run = Command::new(&packaged).output().unwrap();
    assert!(!packaged_run.status.success(), "独立包应拒绝未授权端点");
    let packaged_error = String::from_utf8_lossy(&packaged_run.stderr).to_string();
    assert!(packaged_error.contains("端点未授权"), "{packaged_error}");

    let masked_directory = tempfile::tempdir().unwrap();
    fs::write(
        masked_directory.path().join("名单.代码"),
        "引入 表格处理\n\
引入 隐私\n\
定义 名单 为 表格处理 把记录整理成表格([{姓名: 隐私 掩码(\"张三\")}])\n\
定义 标记 为 名单 设置列敏感(\"姓名\", \"Identity\")\n\
输出(标记 标记列脱敏(\"姓名\"))\n",
    )
    .unwrap();
    fs::write(
        masked_directory.path().join("衡枢.toml"),
        "[\"项目\"]\n\
\"名称\" = \"端点\"\n\
\"版本\" = \"0.1.0\"\n\
\"入口\" = \"名单.代码\"\n\
\"许可证\" = \"MIT\"\n\n\
[\"权限\"]\n\
\"允许端点\" = [\"Pdf\"]\n\
\"允许敏感类别\" = [\"Identity\"]\n",
    )
    .unwrap();

    let masked_run = Command::new(binary())
        .arg("运行")
        .arg(masked_directory.path())
        .output()
        .unwrap();
    assert!(
        masked_run.status.success(),
        "脱敏后源码路径应放行：{}",
        String::from_utf8_lossy(&masked_run.stderr)
    );

    let masked_packaged = masked_directory.path().join("名单程序");
    let masked_pack = Command::new(binary())
        .arg("打包")
        .arg(masked_directory.path())
        .arg("-o")
        .arg(&masked_packaged)
        .output()
        .unwrap();
    assert!(
        masked_pack.status.success(),
        "{}",
        String::from_utf8_lossy(&masked_pack.stderr)
    );
    let masked_packaged_run = Command::new(&masked_packaged).output().unwrap();
    assert!(
        masked_packaged_run.status.success(),
        "脱敏后独立包应放行：{}",
        String::from_utf8_lossy(&masked_packaged_run.stderr)
    );
}
