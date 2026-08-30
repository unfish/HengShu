use sha2::{Digest, Sha256};
use std::fs;
use std::process::Command;

mod cli_common;
use cli_common::binary;

#[test]
fn packaged_module_methods_enforce_owner_visibility() {
    let directory = tempfile::tempdir().unwrap();
    let module = directory.path().join("类型模块.代码");
    let source = directory.path().join("主程序.代码");
    let output = directory.path().join("方法程序");
    fs::write(
        &module,
        "公开 结构 客户：\n姓名\n结束\n内部 定义功能 客户 私有（自己）：\n返回 自己 的 姓名\n结束\n公开 定义功能 客户 可见（自己）：\n返回 自己 私有()\n结束\n",
    )
    .unwrap();
    fs::write(
        &source,
        "引入 类型模块\n定义 甲 为 类型模块 的 客户(姓名 为 \"甲\")\n输出(甲 可见())\n",
    )
    .unwrap();
    let source_run = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(source_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&source_run.stdout).trim(), "甲");

    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(packed.status.code(), Some(0));
    let packaged_run = Command::new(&output).output().unwrap();
    assert_eq!(packaged_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&packaged_run.stdout).trim(), "甲");

    fs::write(
        &source,
        "引入 类型模块\n定义 甲 为 类型模块 的 客户(姓名 为 \"甲\")\n输出(甲 私有())\n",
    )
    .unwrap();
    let denied = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("仅限所属模块内部访问"));
}

#[test]
fn packaged_modules_keep_qualified_and_selective_nominal_types_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let first_module = directory.path().join("甲模块.代码");
    let second_module = directory.path().join("乙模块.代码");
    let source = directory.path().join("主程序.代码");
    let output = directory.path().join("名义类型程序");
    fs::write(
        &first_module,
        "公开 结构 客户：\n姓名\n结束\n公开 定义功能 客户 标记（自己）：\n返回 \"甲\"\n结束\n",
    )
    .unwrap();
    fs::write(
        &second_module,
        "公开 结构 客户：\n姓名\n结束\n公开 定义功能 客户 标记（自己）：\n返回 \"乙\"\n结束\n",
    )
    .unwrap();
    fs::write(
        &source,
        "引入 甲模块\n引入 乙模块\n定义 甲 为 甲模块 的 客户(姓名 为 \"甲\")，类型限定为 甲模块.客户\n定义 乙 为 乙模块 的 客户(姓名 为 \"乙\")，类型限定为 乙模块.客户\n输出(甲 标记(), 乙 标记(), 类型是(甲, \"甲模块.客户\"), 类型是(甲, \"乙模块.客户\"))\n",
    )
    .unwrap();
    let run = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "甲 乙 是 否");
    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(packed.status.code(), Some(0));
    let packaged = Command::new(&output).output().unwrap();
    assert_eq!(packaged.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&packaged.stdout).trim(),
        "甲 乙 是 否"
    );

    fs::write(
        &source,
        "从 甲模块 引入 客户\n定义 甲 为 客户(姓名 为 \"甲\")，类型限定为 客户\n输出(甲 标记(), 类型是(甲, \"客户\"))\n",
    )
    .unwrap();
    let selective = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(selective.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&selective.stdout).trim(), "甲 是");
}

#[test]
fn internal_import_bindings_stay_hidden_in_source_and_packaged_modules() {
    let root = env!("CARGO_MANIFEST_DIR");
    let directory = tempfile::tempdir().unwrap();
    let dependency = directory.path().join("依赖模块.代码");
    let facade = directory.path().join("接口模块.代码");
    let source = directory.path().join("主程序.代码");
    let output = directory.path().join("内部导入程序");

    fs::write(&dependency, "公开 定义 值 为 42\n").unwrap();
    fs::write(
        &facade,
        "内部 引入 依赖模块\n内部 从 依赖模块 引入 值\n公开 定义 名称 为 \"接口\"\n",
    )
    .unwrap();
    fs::write(&source, "引入 接口模块\n输出(接口模块 的 依赖模块)\n").unwrap();

    let source_run = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!source_run.status.success());
    assert!(String::from_utf8_lossy(&source_run.stderr).contains("没有“依赖模块”"));

    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let packaged_run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(!packaged_run.status.success());
    assert!(
        String::from_utf8_lossy(&packaged_run.stderr).contains("没有“依赖模块”"),
        "{}",
        String::from_utf8_lossy(&packaged_run.stderr)
    );

    fs::write(&source, "引入 接口模块\n输出(接口模块 的 值)\n").unwrap();
    let selective_run = Command::new(binary())
        .args(["运行", source.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!selective_run.status.success());
    assert!(String::from_utf8_lossy(&selective_run.stderr).contains("没有“值”"));

    let selective_packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        selective_packed.status.success(),
        "{}",
        String::from_utf8_lossy(&selective_packed.stderr)
    );
    let selective_packaged_run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(!selective_packaged_run.status.success());
    assert!(
        String::from_utf8_lossy(&selective_packaged_run.stderr).contains("没有“值”"),
        "{}",
        String::from_utf8_lossy(&selective_packaged_run.stderr)
    );
}

#[test]
fn packaged_program_runs_without_source_directory() {
    let root = env!("CARGO_MANIFEST_DIR");
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("程序.代码");
    let output = directory.path().join("独立程序");
    fs::write(
        &source,
        "引入 统计分析\n定义 数据 为 统计分析 数组([1, 2, 3, 4, 5])\n输出(数据 平均值())\n",
    )
    .unwrap();
    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    fs::remove_file(&source).unwrap();
    let run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "3");
}

#[test]
fn packaged_program_supports_natural_duration_values() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("持续时间.代码");
    let output = directory.path().join("持续时间程序");
    fs::copy("tests/data/持续时间打包.代码", &source).unwrap();
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
    fs::remove_file(&source).unwrap();
    let run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1200毫秒");
}

#[test]
fn packaged_program_embeds_local_code_modules() {
    let root = env!("CARGO_MANIFEST_DIR");
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("主程序.代码");
    let module = directory.path().join("财务工具.代码");
    let output = directory.path().join("模块程序");
    fs::write(&module, "定义功能 加税（金额）：\n返回 金额 * 1.13\n结束\n").unwrap();
    fs::write(&source, "引入 财务工具\n输出(财务工具 加税(100))\n").unwrap();
    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    fs::remove_file(&source).unwrap();
    fs::remove_file(&module).unwrap();
    let run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "113");
}

/// 打包必须对导入模块跑完整的解析与静态检查，并且失败身份与`运行`路径一致；
/// 否则语义非法的模块会被封进产物，只在运行时才暴露，且诊断里看不出是哪个模块。
#[test]
fn packaging_reports_module_parse_and_check_failures_like_running_does() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("主程序.代码");
    let module = directory.path().join("损坏模块.代码");
    let output = directory.path().join("损坏程序");
    fs::write(&source, "引入 损坏模块\n输出(损坏模块 的 名称)\n").unwrap();

    for (content, fragment) in [
        ("定义功能 甲（）：\n返回 1\n", "模块“损坏模块”解析失败"),
        ("返回 1\n", "模块“损坏模块”静态检查失败"),
    ] {
        fs::write(&module, content).unwrap();
        let packed = Command::new(binary())
            .arg("打包")
            .arg(&source)
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            !packed.status.success(),
            "{}",
            String::from_utf8_lossy(&packed.stdout)
        );
        assert!(
            String::from_utf8_lossy(&packed.stderr).contains(fragment),
            "{}",
            String::from_utf8_lossy(&packed.stderr)
        );
        assert!(!output.exists(), "打包失败不应留下产物");

        let run = Command::new(binary())
            .arg("运行")
            .arg(&source)
            .output()
            .unwrap();
        assert!(!run.status.success());
        assert!(
            String::from_utf8_lossy(&run.stderr).contains(fragment),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

/// 模块身份按规范化路径判定：同一个文件在根程序里写作`子目录/共享`、在同目录模块里写作`共享`，
/// 顶层只能执行一次；跨书写路径的循环也必须照样被发现，否则模块状态会出现两份。
#[test]
fn packaged_program_runs_bytecode_closures_and_exception_tables_without_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("闭包异常.代码");
    let output = directory.path().join("闭包异常程序");
    fs::write(
        &source,
        "定义功能 制作（起点）：\n返回 定义功能（增量）为 起点 + 增量\n结束\n定义 累加 为 制作(10)\n尝试：\n输出(累加(2))\n定义功能 深层（）：\n抛出 \"故障\"\n结束\n深层()\n捕获 异常：\n输出(异常 的 信息)\n最终：\n输出(\"清理\")\n结束\n",
    )
    .unwrap();
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
    fs::remove_file(&source).unwrap();
    let run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "12\n故障\n清理"
    );
}

#[test]
fn packaged_program_starts_bytecode_tasks_without_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("并发字节码.代码");
    let output = directory.path().join("并发字节码程序");
    fs::write(
        &source,
        "引入 并发\n定义功能 工作（值）：\n返回 值 * 2\n结束\n定义 任务 为 并发 启动(工作, [21])\n输出(任务 等待())\n",
    )
    .unwrap();
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
    fs::remove_file(&source).unwrap();
    let run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");
}

#[test]
fn packaged_nested_modules_resolve_sibling_imports() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("子");
    fs::create_dir(&nested).unwrap();
    let source = directory.path().join("主程序.代码");
    let output = directory.path().join("嵌套模块程序");
    fs::write(nested.join("乙.代码"), "定义 值 为 42\n").unwrap();
    fs::write(nested.join("甲.代码"), "引入 乙\n定义 结果 为 乙 的 值\n").unwrap();
    fs::write(&source, "引入 子/甲\n输出(甲 的 结果)\n").unwrap();
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
    fs::remove_file(&source).unwrap();
    fs::remove_dir_all(&nested).unwrap();
    let run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");
}

#[test]
fn packaged_program_defaults_to_trusted_user_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let source = source_dir.path().join("读取授权文件.代码");
    let data = directory.path().join("授权数据.txt");
    let output = source_dir.path().join("授权程序");
    fs::write(&data, "打包权限有效").unwrap();
    fs::write(
        &source,
        format!(
            "引入 文件系统\n输出(文件系统 读取文本(\"{}\"))\n",
            data.display()
        ),
    )
    .unwrap();

    let default_output = source_dir.path().join("默认程序");
    let default_pack = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&default_output)
        .output()
        .unwrap();
    assert!(default_pack.status.success());
    let default_run = Command::new(&default_output)
        .current_dir("/")
        .output()
        .unwrap();
    assert!(default_run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&default_run.stdout).trim(),
        "打包权限有效"
    );

    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .arg("--允许")
        .arg(format!("文件:{}", directory.path().display()))
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    fs::remove_file(&source).unwrap();
    let run = Command::new(&output).current_dir("/").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "打包权限有效");
}

#[test]
fn release_archive_is_reproducible_and_contains_checksum() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("发布测试.代码");
    let first = directory.path().join("第一次");
    let second = directory.path().join("第二次");
    fs::write(&source, "输出(\"发布成功\")\n").unwrap();

    for output in [&first, &second] {
        let release = Command::new(binary())
            .arg("发布")
            .arg(&source)
            .arg("-o")
            .arg(output)
            .output()
            .unwrap();
        assert!(
            release.status.success(),
            "{}",
            String::from_utf8_lossy(&release.stderr)
        );
    }
    let first_archive = fs::read_dir(&first)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let second_archive = fs::read_dir(&second)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        Sha256::digest(fs::read(&first_archive).unwrap()),
        Sha256::digest(fs::read(&second_archive).unwrap())
    );

    let decoder = flate2::read::GzDecoder::new(fs::File::open(first_archive).unwrap());
    let names = tar::Archive::new(decoder)
        .entries()
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "发布测试"));
    assert!(names.iter().any(|name| name == "SHA256SUMS"));
}

#[test]
fn packaged_program_anchors_relative_paths_to_its_own_directory() {
    let build_dir = tempfile::tempdir().unwrap();
    let deploy_dir = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let source = build_dir.path().join("读写相对文件.代码");
    let built = build_dir.path().join("相对根程序");
    let deployed = deploy_dir.path().join("相对根程序");
    fs::write(
        &source,
        "引入 文件系统\n文件系统 写入文本(\"产出.txt\", \"相对根有效\")\n输出(文件系统 读取文本(\"产出.txt\"))\n",
    )
    .unwrap();
    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&built)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    fs::copy(&built, &deployed).unwrap();
    let run = Command::new(&deployed)
        .current_dir(run_dir.path())
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "相对根有效");
    assert!(deploy_dir.path().join("产出.txt").exists());
    assert!(!run_dir.path().join("产出.txt").exists());
    assert!(!build_dir.path().join("产出.txt").exists());
}

/// 改写载荷任意一个字节必须报“内嵌程序校验失败”：曾因载荷定位本身依赖摘要匹配，
/// 被改写的包既不执行也不报损坏，而是静默退化成普通命令行工具。
#[test]
fn tampered_packaged_payload_is_rejected_as_corrupted() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("问候.代码");
    let built = dir.path().join("问候程序");
    let tampered = dir.path().join("问候程序改");
    fs::write(&source, "输出(\"你好\")\n").unwrap();
    let packed = Command::new(binary())
        .arg("打包")
        .arg(&source)
        .arg("-o")
        .arg(&built)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let intact = Command::new(&built).output().unwrap();
    assert!(
        intact.status.success(),
        "{}",
        String::from_utf8_lossy(&intact.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&intact.stdout).trim(), "你好");

    let magic = b"HENGSHU_PAYLOAD2";
    let mut bytes = fs::read(&built).unwrap();
    let tail = bytes
        .windows(magic.len())
        .rposition(|window| window == magic)
        .expect("打包程序应带尾部标记");
    let footer_start = bytes[..tail]
        .windows(magic.len())
        .rposition(|window| window == magic)
        .expect("尾部应有成对标记");
    bytes[footer_start - 1] ^= 0xff;
    fs::write(&tampered, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tampered, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = Command::new(&tampered).output().unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("内嵌程序校验失败"), "{stderr}");
    assert!(String::from_utf8_lossy(&run.stdout).trim().is_empty());
}
