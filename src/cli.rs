use crate::evidence::{
    compare_inputs, verify_input_root, EvidenceManifest, ExternalDependency,
    ExternalDependencyLevel, EVIDENCE_VERSION,
};
use crate::execution::{ContractVersion, ObservationBudget, ObservationMode};
use crate::security::{PermissionRequest, PermissionSet};
use crate::{checker, lsp, pack, parser, project, release, runtime, vm};
use std::fs;
use std::path::{Path, PathBuf};

fn usage() {
    eprintln!("衡枢中文数据编程语言\n\n用法：\n  衡枢 新建 <项目名> [目录]\n  衡枢 运行 [程序.代码或项目目录] [--允许 类别:范围] [-- 程序参数]\n  衡枢 检查 [程序.代码或项目目录]\n  衡枢 打包 <程序.代码或项目目录> [-o 输出文件] [--允许 类别:范围]\n  衡枢 验证包 <独立程序>\n  衡枢 存证 <程序.代码> [-o 凭证文件]\n  衡枢 验证凭证 <凭证文件>\n  衡枢 复算 <凭证文件> --输入根 <目录> [--输入=逻辑名=相对路径] [--字节码]\n  衡枢 排程 <安装|查询|卸载|触发> --工具 <名称> [--程序 CLI产物] [--轮询 秒]\n  衡枢 历史 [--数量 N] [-o 输出文件]\n  衡枢 测试 [目录或测试文件.代码] [--筛选 名称] [--列出] [--遇错即停] [--允许空测试]\n  衡枢 格式化 [目录] [--检查]\n  衡枢 文档 [项目目录] [-o 输出目录]\n  衡枢 基准 <程序.代码> [--次数 N] [--最大平均毫秒 N]\n  衡枢 添加 <依赖名> --路径 <目录> [--版本 要求]\n  衡枢 移除 <依赖名>\n  衡枢 更新\n  衡枢 缓存 <依赖名>\n  衡枢 审计 [项目目录] [-o 报告文件]\n  衡枢 发布 <程序.代码或项目目录> [-o 输出目录]\n  衡枢 版本\n  衡枢 帮助");
}

fn project_or_source(value: Option<&String>) -> Result<PathBuf, String> {
    let path = value
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
    if path.is_dir() {
        project::project_entry(&path)
    } else {
        Ok(path)
    }
}

fn rest(args: &[String], from: usize) -> &[String] {
    args.get(from..).unwrap_or_default()
}

fn option(args: &[String], names: &[&str]) -> Option<String> {
    args.iter()
        .position(|argument| names.contains(&argument.as_str()))
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn tool_parameters(
    path: &Path,
    command_args: &[String],
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, String> {
    let Some(root) = project::find_root(path) else {
        return Ok(std::collections::BTreeMap::new());
    };
    let manifest = project::read_manifest(&root)?;
    let tool_name = option(command_args, &["--工具", "--tool"]);
    let Some(tool_name) = tool_name else {
        return Ok(std::collections::BTreeMap::new());
    };
    let tool = manifest
        .tools
        .get(&tool_name)
        .ok_or_else(|| format!("没有工具声明“{tool_name}”"))?;
    tool.validate()?;
    let mut parameters = std::collections::BTreeMap::new();
    if let Some(file) = option(command_args, &["--参数文件", "--parameter-file"]) {
        let path = root.join(&file);
        if Path::new(&file).is_absolute()
            || Path::new(&file)
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err("参数文件不能离开项目目录".into());
        }
        let bytes = crate::resource::read_file_limited(&path, crate::resource::MAX_METADATA_BYTES)
            .map_err(|error| format!("读取参数文件失败：{error}"))?;
        parameters = serde_json::from_slice(&bytes)
            .map_err(|error| format!("参数文件必须是 JSON 对象：{error}"))?;
    }
    let mut text_parameters = std::collections::BTreeMap::new();
    let mut index = 0;
    while index < command_args.len() {
        if matches!(command_args[index].as_str(), "--参数" | "--parameter") {
            let assignment = command_args.get(index + 1).ok_or("--参数 后缺少 名称=值")?;
            let (name, value) = assignment
                .split_once('=')
                .ok_or("--参数 必须写成 名称=值")?;
            text_parameters.insert(name.to_string(), value.to_string());
            index += 2;
        } else {
            index += 1;
        }
    }
    parameters.extend(tool.parse_text_parameters(&text_parameters)?);
    let parameters = tool.validate_parameters(&parameters)?;
    for (name, value) in &parameters {
        if tool
            .parameters
            .get(name)
            .is_some_and(|parameter| parameter.is_secret())
        {
            if let Some(text) = value.as_str() {
                crate::privacy::register_secret(text);
            }
        }
    }
    Ok(parameters)
}

fn embedded_tool_arguments(
    args: &[String],
    tool: Option<&crate::execution::ToolManifest>,
) -> Result<Vec<String>, String> {
    let Some(tool) = tool else {
        return Ok(args.to_vec());
    };
    let separator = args.iter().position(|arg| arg == "--");
    let option_args = &args[..separator.unwrap_or(args.len())];
    let passthrough = separator
        .and_then(|index| args.get(index + 1..))
        .unwrap_or_default();
    let mut parameters = std::collections::BTreeMap::new();
    let mut index = 0;
    while index < option_args.len() {
        if matches!(option_args[index].as_str(), "--参数" | "--parameter") {
            let assignment = option_args.get(index + 1).ok_or("--参数 后缺少 名称=值")?;
            let (name, value) = assignment
                .split_once('=')
                .ok_or("--参数 必须写成 名称=值")?;
            parameters.insert(name.to_string(), value.to_string());
            index += 2;
        } else {
            index += 1;
        }
    }
    let parsed = tool.parse_text_parameters(&parameters)?;
    let mut encoded = tool.encode_program_args(&parsed)?;
    encoded.extend(passthrough.iter().cloned());
    Ok(encoded)
}

fn test_path(args: &[String]) -> Option<PathBuf> {
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--筛选" | "--filter" => index += 2,
            "--列出" | "--list" | "--遇错即停" | "--fail-fast" | "--允许空测试"
            | "--allow-empty" => index += 1,
            argument if argument.starts_with('-') => index += 1,
            argument => return Some(PathBuf::from(argument)),
        }
    }
    None
}
fn source(path: &Path) -> Result<String, String> {
    crate::resource::read_string_limited(path, crate::resource::MAX_SOURCE_BYTES)
        .map_err(|e| format!("读取源码失败：{e}"))
}
fn tighten<T: Ord>(slot: &mut Option<T>, value: T, fixed: bool) {
    *slot = Some(match slot.take() {
        Some(existing) if fixed => existing.min(value),
        _ => value,
    });
}

fn permissions(path: &Path, args: &[String]) -> Result<PermissionSet, String> {
    let base_dir = path.parent().unwrap_or(Path::new("."));
    let mut permissions = PermissionSet::for_source(base_dir);
    let fixed = permissions.fixed;
    let mut request = PermissionRequest::default();
    let mut index = 0;
    while index < args.len() {
        if matches!(args[index].as_str(), "--允许" | "--allow") {
            let specification = args.get(index + 1).ok_or("--允许 后缺少权限范围")?;
            request.add(specification, base_dir)?;
            index += 2;
        } else if matches!(args[index].as_str(), "--最长执行时间" | "--timeout") {
            let value = args.get(index + 1).ok_or("--最长执行时间 后缺少时间")?;
            let milliseconds =
                crate::contracts::parse_duration_milliseconds_message(value, "最长执行时间")?;
            tighten(
                &mut permissions.execution_timeout_ms,
                u64::try_from(milliseconds).map_err(|_| "最长执行时间不能是负数")?,
                fixed,
            );
            index += 2;
        } else if matches!(
            args[index].as_str(),
            "--最大输出字节" | "--max-output-bytes"
        ) {
            tighten(
                &mut permissions.max_output_bytes,
                args.get(index + 1)
                    .ok_or("--最大输出字节 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大输出字节必须是非负整数")?,
                fixed,
            );
            index += 2;
        } else if matches!(
            args[index].as_str(),
            "--最大集合项数" | "--max-collection-items"
        ) {
            tighten(
                &mut permissions.max_collection_items,
                args.get(index + 1)
                    .ok_or("--最大集合项数 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大集合项数必须是非负整数")?,
                fixed,
            );
            index += 2;
        } else if matches!(
            args[index].as_str(),
            "--最大缓冲字节" | "--max-buffer-bytes"
        ) {
            tighten(
                &mut permissions.max_buffer_bytes,
                args.get(index + 1)
                    .ok_or("--最大缓冲字节 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大缓冲字节必须是非负整数")?,
                fixed,
            );
            index += 2;
        } else if matches!(args[index].as_str(), "--最大调用深度" | "--max-call-depth") {
            tighten(
                &mut permissions.max_call_depth,
                args.get(index + 1)
                    .ok_or("--最大调用深度 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大调用深度必须是正整数")?,
                fixed,
            );
            index += 2;
        } else if matches!(
            args[index].as_str(),
            "--最大值嵌套深度" | "--max-value-nesting"
        ) {
            tighten(
                &mut permissions.max_value_nesting,
                args.get(index + 1)
                    .ok_or("--最大值嵌套深度 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大值嵌套深度必须是正整数")?,
                fixed,
            );
            index += 2;
        } else if matches!(
            args[index].as_str(),
            "--最大模块深度" | "--max-module-depth"
        ) {
            tighten(
                &mut permissions.max_module_depth,
                args.get(index + 1)
                    .ok_or("--最大模块深度 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大模块深度必须是正整数")?,
                fixed,
            );
            index += 2;
        } else if matches!(
            args[index].as_str(),
            "--最大通道容量" | "--max-channel-capacity"
        ) {
            tighten(
                &mut permissions.max_channel_capacity,
                args.get(index + 1)
                    .ok_or("--最大通道容量 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大通道容量必须是正整数")?,
                fixed,
            );
            index += 2;
        } else if matches!(
            args[index].as_str(),
            "--最大并行工作者" | "--max-parallel-workers"
        ) {
            tighten(
                &mut permissions.max_parallel_workers,
                args.get(index + 1)
                    .ok_or("--最大并行工作者 后缺少数值")?
                    .parse()
                    .map_err(|_| "最大并行工作者必须是正整数")?,
                fixed,
            );
            index += 2;
        } else {
            index += 1;
        }
    }
    permissions.apply(request)?;
    Ok(permissions)
}

fn host_permissions(path: &Path, runtime: &PermissionSet) -> PermissionSet {
    let mut host = runtime.clone();
    let root = project::find_root(path).unwrap_or_else(|| {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    });
    let root = root.canonicalize().unwrap_or(root);
    if !host.file_roots.contains(&root) {
        host.file_roots.push(root);
    }
    host
}

/// 渲染面向人的错误文本，并在`HENGSHU_DIAGNOSTIC_FILE`指向文件时同时写出结构化诊断。
/// 渲染文本会丢掉列号、调用栈和原因链，工具链与差分门禁需要一份不经反解的结构化身份。
fn render_and_report(error: &runtime::RuntimeError, permissions: Option<&PermissionSet>) -> String {
    if let Ok(path) = std::env::var(crate::diagnostic::DIAGNOSTIC_FILE_VARIABLE) {
        // 旁路写入失败不能掩盖真正的错误，因此只影响旁路本身。
        if let Ok(report) = serde_json::to_string(&error.report()) {
            let path = Path::new(&path);
            if permissions.is_none_or(|permissions| permissions.check_file(path).is_ok()) {
                let _ = crate::security::atomic_write_file(
                    path,
                    crate::privacy::scrub_secrets(&report).as_bytes(),
                    true,
                );
            }
        }
    }
    crate::privacy::scrub_secrets(&error.render())
}

fn run_file(path: &Path, args: Vec<String>, command_args: &[String]) -> Result<(), String> {
    let parameters = tool_parameters(path, command_args)?;
    let mut args = args;
    let output_root = project::find_root(path).and_then(|root| {
        option(command_args, &["--工具", "--tool"]).and_then(|tool_name| {
            project::read_manifest(&root)
                .ok()
                .and_then(|manifest| manifest.tools.get(&tool_name).cloned())
                .map(|tool| root.join(tool.output_root(crate::execution::OutputKind::Cli)))
        })
    });
    if let Some(output_root) = &output_root {
        fs::create_dir_all(output_root).map_err(|error| format!("创建输出目录失败：{error}"))?;
    }
    if let Some(root) = project::find_root(path) {
        if let Some(tool_name) = option(command_args, &["--工具", "--tool"]) {
            let manifest = project::read_manifest(&root)?;
            let tool = manifest
                .tools
                .get(&tool_name)
                .ok_or_else(|| format!("没有工具声明“{tool_name}”"))?;
            args.extend(tool.encode_program_args(&parameters)?);
        }
    }
    let text = source(path)?;
    let observation = option(command_args, &["--观察", "--observe"])
        .map(|value| match value.as_str() {
            "摘要" | "summary" => Ok(ObservationMode::Summary),
            "快照" | "snapshots" => Ok(ObservationMode::Snapshots),
            "详细" | "detailed" => Ok(ObservationMode::Detailed),
            "关闭" | "off" => Ok(ObservationMode::Off),
            _ => Err("--观察 只支持 关闭、摘要、快照、详细".to_string()),
        })
        .transpose()?
        .unwrap_or(ObservationMode::Off);
    let report_path = option(command_args, &["--过程报告", "--notebook"]).map(PathBuf::from);
    let run_permissions = permissions(path, command_args)?;
    let report_permissions = host_permissions(path, &run_permissions);
    let result = runtime::execute_output_with_observation_report(
        text,
        path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        true,
        args,
        run_permissions.clone(),
        true,
        observation,
        ObservationBudget::default(),
    )
    .map_err(|failure| render_and_report(&failure.error, Some(&report_permissions)))?;
    if let (Some(path), Some(notebook)) = (report_path, result.notebook) {
        let html = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
            });
        let content = if html {
            notebook.to_html()?
        } else {
            notebook.to_json()?
        };
        run_permissions
            .write_file_atomic(
                &path,
                content.as_bytes(),
                crate::privacy::Endpoint::LocalFile,
                &crate::execution::Sensitivity::default(),
            )
            .map_err(|error| format!("写出过程报告失败：{error}"))?;
    }
    if let Some(output_root) = output_root {
        let mut artifacts = fs::read_dir(&output_root)
            .map_err(|error| format!("读取输出目录失败：{error}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        artifacts.sort();
        for artifact in artifacts.into_iter().take(256) {
            let observed = crate::execution::Artifact::observe_file(&artifact, "cli-output")?;
            println!(
                "交付物：{}（{} 字节，SHA-256 {}）",
                observed.display_name, observed.bytes, observed.sha256
            );
        }
    }
    Ok(())
}

/// 从工程清单解析工具的自动化声明，生成当前平台的用户级任务安装计划。
mod automation;
use automation::{embedded_automation_trigger, schedule_plan, schedule_trigger};
fn check_file(path: &Path) -> Result<(), String> {
    let text = source(path)?;
    crate::resource::on_compile_stack(move || {
        let program = parser::parse(&text)?;
        checker::check(&program)
    })?;
    println!("语法检查通过");
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    crate::execution::hex_digest(bytes)
}

fn evidence_store(source: &Path, output: &Path, command_args: &[String]) -> Result<(), String> {
    let bytes = crate::resource::read_file_limited(source, crate::resource::MAX_SOURCE_BYTES)
        .map_err(|error| format!("读取源码失败：{error}"))?;
    let source_text =
        String::from_utf8(bytes.clone()).map_err(|_| "源码不是 UTF-8，不能存证".to_string())?;
    let source_dir = source.parent().unwrap_or(Path::new("."));
    PermissionSet::for_source(source_dir).check_file(output)?;
    let mut parameters = tool_parameters(source, command_args)?;
    let mut run_options = crate::api::RunOptions::new(source_dir);
    let mut secret_names = std::collections::BTreeSet::new();
    if let Some(root) = project::find_root(source) {
        if let Some(tool_name) = option(command_args, &["--工具", "--tool"]) {
            let manifest = project::read_manifest(&root)?;
            let tool = manifest
                .tools
                .get(&tool_name)
                .ok_or_else(|| format!("没有工具声明“{tool_name}”"))?;
            run_options.program_args = tool.encode_program_args(&parameters)?;
            secret_names.extend(
                tool.parameters
                    .iter()
                    .filter(|(_, parameter)| parameter.is_secret())
                    .map(|(name, _)| name.clone()),
            );
        }
    }
    parameters.retain(|name, _| !secret_names.contains(name));
    crate::evidence::start_observing();
    if let Some(file) = option(command_args, &["--参数文件", "--parameter-file"]) {
        let parameter_root = project::find_root(source).unwrap_or_else(|| source_dir.to_path_buf());
        crate::evidence::observe_input_file("参数文件", &parameter_root.join(file));
    }
    let run = crate::api::run_source_outcome(&source_text, run_options);
    let observed = crate::evidence::observations();
    // Use paths observed immediately before the actual writes. This avoids
    // treating unrelated historical files in the source directory as outputs.
    let observed_metadata = crate::evidence::metadata_from_observations(&observed);
    crate::evidence::stop_observing();
    let artifacts = run
        .artifacts
        .iter()
        .filter(|artifact| artifact.path.as_path() != source && artifact.path.as_path() != output)
        .filter_map(|artifact| {
            let mut artifact = artifact.clone();
            artifact.path = artifact.path.strip_prefix(source_dir).ok()?.to_path_buf();
            Some(artifact)
        })
        .collect::<Vec<_>>();
    // 凭证保存相对源码目录的文件根，复算时再锚定到用户提供的输入根，
    // 否则凭证会携带存证机器的绝对路径而无法在别处复算。
    let permissions =
        crate::security::PermissionSet::for_source(source_dir).into_portable_roots(source_dir);
    let mut external_dependencies = if observed.externals.is_empty() {
        vec![ExternalDependency {
            kind: "无".into(),
            identity: "本机执行".into(),
            fingerprint: None,
            level: ExternalDependencyLevel::None,
        }]
    } else {
        observed.externals
    };
    if !secret_names.is_empty() {
        external_dependencies.push(ExternalDependency {
            kind: "秘密参数".into(),
            identity: secret_names.into_iter().collect::<Vec<_>>().join(","),
            fingerprint: None,
            level: ExternalDependencyLevel::FingerprintOnly,
        });
    }
    let bytecode_digest = crate::api::compile_source_in(&source_text, source_dir)
        .and_then(|compiled| compiled.to_bytes())
        .map(|bytes| digest_bytes(&bytes))
        .ok();
    let success = run.success;
    let failure_message = run
        .diagnostics
        .first()
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_else(|| "未知执行失败".into());
    let manifest = EvidenceManifest {
        version: ContractVersion::default(),
        evidence_version: EVIDENCE_VERSION,
        run_id: format!("{}-{}", std::process::id(), &digest_bytes(&bytes)[..12]),
        program_digest: digest_bytes(&bytes),
        source_path: source.file_name().map(PathBuf::from),
        source_text: Some(source_text),
        output: run.output,
        success,
        permissions: Some(permissions),
        bytecode_digest,
        parameters,
        inputs: observed.inputs,
        artifacts,
        diagnostics: run.diagnostics,
        external_dependencies,
        resources: run.resources,
        metadata: observed_metadata,
    };
    manifest.write_file_authorized(
        output,
        crate::resource::MAX_METADATA_BYTES,
        &PermissionSet::for_source(source_dir),
    )?;
    if success {
        Ok(())
    } else {
        Err(format!("存证执行失败：{failure_message}（失败凭证已写出）"))
    }
}

fn evidence_verify(path: &Path) -> Result<(), String> {
    let manifest = EvidenceManifest::read_file(path, crate::resource::MAX_METADATA_BYTES)?;
    let policy = manifest
        .permissions
        .as_ref()
        .map(crate::security::PermissionSet::endpoint_policy)
        .unwrap_or_else(crate::privacy::EndpointPolicy::permissive);
    let visible = manifest.view(&policy);
    println!(
        "凭证版本 {}，输入 {} 项，外部依赖 {} 项，参数 {} 项，输出 {} 行",
        visible.evidence_version,
        visible.inputs.len(),
        visible.external_dependencies.len(),
        visible.parameters.len(),
        visible.output.len()
    );
    Ok(())
}

fn evidence_replay(
    path: &Path,
    root: &Path,
    mappings: &[(String, PathBuf)],
    bytecode: bool,
) -> Result<(), String> {
    let manifest = EvidenceManifest::read_file(path, crate::resource::MAX_METADATA_BYTES)?;
    let mapping = mappings
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    let actual = verify_input_root(root, &manifest.inputs, &mapping)?;
    let differences = compare_inputs(&manifest.inputs, &actual);
    if !differences.is_empty() {
        return Err(format!("复算输入不一致：{differences:?}"));
    }
    if manifest
        .external_dependencies
        .iter()
        .any(|dependency| dependency.level == ExternalDependencyLevel::FingerprintOnly)
    {
        return Err("输入指纹验证通过，但存在未录制外部依赖；不能声称完整复算".into());
    }
    let source = manifest
        .source_text
        .clone()
        .ok_or("凭证没有可重放源码载荷")?;
    let mut options = crate::api::RunOptions::new(root);
    if let Some(mut permissions) = manifest.permissions.clone() {
        permissions.anchor_file_roots(root);
        options.permissions = permissions;
    }
    options.program_args = manifest
        .parameters
        .iter()
        .map(|(name, value)| {
            serde_json::to_string(value)
                .map(|value| format!("{name}={value}"))
                .map_err(|error| format!("复算参数编码失败：{error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if bytecode {
        let compiled = crate::api::compile_source_in(&source, root)
            .and_then(|compiled| compiled.to_bytes())
            .map_err(|error| format!("复算编译失败：{}", error.message()))?;
        let digest = digest_bytes(&compiled);
        match manifest.bytecode_digest.as_deref() {
            Some(expected) if expected != digest => {
                return Err(format!(
                    "复算字节码摘要不一致：期望 {expected}，实际 {digest}"
                ));
            }
            None => return Err("凭证没有记录字节码摘要，不能做字节码复算".into()),
            _ => {}
        }
    }
    crate::evidence::start_observing();
    let mut actual = if bytecode {
        crate::api::run_compiled_outcome(&source, options)
    } else {
        crate::api::run_source_outcome(&source, options)
    };
    let observed = crate::evidence::observations();
    crate::evidence::stop_observing();
    for artifact in &mut actual.artifacts {
        if let Ok(relative) = artifact.path.strip_prefix(root) {
            artifact.path = relative.to_path_buf();
        }
    }
    for artifact in &manifest.artifacts {
        if artifact.path.is_absolute()
            || artifact
                .path
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(format!("凭证交付物路径不安全：{}", artifact.path.display()));
        }
    }
    let expected = crate::execution::RunOutcome {
        success: manifest.success,
        output: manifest.output.clone(),
        diagnostics: manifest.diagnostics.clone(),
        artifacts: manifest.artifacts.clone(),
        resources: manifest.resources.clone(),
        ..crate::execution::RunOutcome::default()
    };
    let comparison = crate::evidence::compare_run(
        &expected,
        &actual,
        &manifest.external_dependencies,
        &observed.externals,
    );
    if !comparison.matches() {
        return Err(format!("复算结果不一致：{:?}", comparison.differences));
    }
    if manifest.success {
        println!(
            "复算成功：运行结果、诊断、交付物和资源摘要一致（{}）",
            if bytecode {
                "字节码载荷"
            } else {
                "源码载荷"
            }
        );
    } else {
        println!("复算一致：失败类别、诊断和此前输出一致");
    }
    Ok(())
}

pub fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(embedded) = pack::read_embedded()? {
        // 独立包的相对路径基准是程序自身所在目录，与源码路径以源码目录为基准一一对应；
        // 若用工作目录做基准，同一份包在不同目录下的可访问范围会随启动位置漂移。
        let base = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .map_or_else(|| std::env::current_dir().map_err(|e| e.to_string()), Ok)?;
        if args.iter().any(|argument| argument == "--衡枢自动化触发") {
            return embedded_automation_trigger(embedded, &args, &base);
        }
        let mut permissions = embedded.permissions;
        permissions.anchor_file_roots(&base);
        if embedded.descriptor.kind == pack::PackageKind::Gui {
            return Err("当前是 GUI 独立程序，不能由 CLI 入口运行".into());
        }
        let program_args = embedded_tool_arguments(&args, embedded.descriptor.tool.as_ref())?;
        vm::execute_structured(
            &embedded.bytecode,
            embedded.modules,
            base,
            true,
            program_args,
            permissions,
        )
        .map_err(|error| render_and_report(&error, None))?;
        return Ok(());
    }
    if args.is_empty() {
        usage();
        return Err("缺少命令".into());
    }
    match args[0].as_str() {
        "新建" | "new" => {
            let name = args.get(1).ok_or("缺少项目名")?;
            let directory = args
                .get(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| name.into());
            project::create(name, &directory)?;
            println!("已创建衡枢项目：{}", directory.display());
            Ok(())
        }
        "运行" | "run" => run_file(
            &project_or_source(args.get(1).filter(|value| value.as_str() != "--"))?,
            args.iter()
                .position(|argument| argument == "--")
                .map(|index| rest(&args, index + 1).to_vec())
                .unwrap_or_default(),
            rest(&args, 2)
                .split(|argument| argument == "--")
                .next()
                .unwrap_or_default(),
        ),
        "检查" | "check" => check_file(&project_or_source(args.get(1))?),
        "存证" | "evidence" => {
            let source = project_or_source(args.get(1))?;
            let output = option(&args, &["-o", "--输出", "--output"])
                .map(PathBuf::from)
                .unwrap_or_else(|| source.with_extension("凭证"));
            evidence_store(&source, &output, &args)?;
            println!("凭证已写出：{}", output.display());
            Ok(())
        }
        "验证凭证" | "verify-evidence" => {
            evidence_verify(&PathBuf::from(args.get(1).ok_or("缺少凭证路径")?))
        }
        "复算" | "replay" => {
            let evidence = PathBuf::from(args.get(1).ok_or("缺少凭证路径")?);
            let root =
                PathBuf::from(option(&args, &["--输入根", "--input-root"]).ok_or("缺少 --输入根")?);
            let mappings = args
                .iter()
                .filter_map(|argument| argument.strip_prefix("--输入="))
                .filter_map(|value| {
                    value
                        .split_once('=')
                        .map(|(name, path)| (name.to_string(), PathBuf::from(path)))
                })
                .collect::<Vec<_>>();
            let bytecode = args
                .iter()
                .any(|argument| argument == "--字节码" || argument == "--bytecode");
            evidence_replay(&evidence, &root, &mappings, bytecode)
        }
        "历史" | "history" => {
            let root = project::find_root(&std::env::current_dir().map_err(|e| e.to_string())?)
                .ok_or("找不到衡枢.toml")?;
            let database = option(&args, &["--数据库", "--database"])
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join(".衡枢运行历史.db"));
            let history = crate::automation::RunHistory::open_recover(&database, 10_000)?;
            let limit = option(&args, &["--数量", "--limit"])
                .and_then(|value| value.parse().ok())
                .unwrap_or(100);
            let json = history.export_json(limit)?;
            if let Some(output) = option(&args, &["-o", "--输出", "--output"]) {
                host_permissions(&root, &crate::security::PermissionSet::for_source(&root))
                    .write_file_atomic(
                        Path::new(&output),
                        json.as_bytes(),
                        crate::privacy::Endpoint::LocalFile,
                        &crate::execution::Sensitivity::default(),
                    )?;
                println!("运行历史已导出：{output}");
            } else {
                println!("{json}");
            }
            Ok(())
        }
        "排程" | "schedule" => {
            let action = args.get(1).map(String::as_str).unwrap_or("帮助");
            let tool_name = option(&args, &["--工具", "--tool"]).ok_or("缺少 --工具")?;
            if matches!(action, "触发" | "trigger") {
                return schedule_trigger(&tool_name, &args);
            }
            let poll_seconds = option(&args, &["--轮询", "--poll"])
                .and_then(|value| value.parse().ok())
                .unwrap_or(60);
            let program = option(&args, &["--程序", "--program"]).map(PathBuf::from);
            let (root, plan) = schedule_plan(&tool_name, program, poll_seconds)?;
            let permissions = crate::security::PermissionSet::for_source(&root);
            match action {
                "安装" | "install" => {
                    let log = crate::automation::scheduler::install(&plan, &permissions)?;
                    println!("已安装用户级排程任务：{tool_name}\n{log}");
                    Ok(())
                }
                "查询" | "query" | "状态" | "status" => {
                    println!(
                        "{}",
                        crate::automation::scheduler::query(&plan, &permissions)?
                    );
                    Ok(())
                }
                "卸载" | "uninstall" | "移除" => {
                    let log = crate::automation::scheduler::uninstall(&plan, &permissions)?;
                    println!("已卸载用户级排程任务：{tool_name}\n{log}");
                    Ok(())
                }
                other => Err(format!(
                    "排程子命令只支持 安装、查询、卸载、触发，收到“{other}”"
                )),
            }
        }
        "打包" | "build" | "compile" => {
            let input = project_or_source(args.get(1))?;
            let output = args
                .iter()
                .position(|a| a == "-o" || a == "--output" || a == "输出")
                .and_then(|i| args.get(i + 1))
                .map(PathBuf::from)
                .unwrap_or_else(|| pack::default_output(&input));
            let permissions = permissions(&input, rest(&args, 2))?;
            let kind = if args
                .iter()
                .any(|argument| matches!(argument.as_str(), "--界面" | "--gui"))
            {
                pack::PackageKind::Gui
            } else {
                pack::PackageKind::Cli
            };
            let tool = option(&args, &["--工具", "--tool"]).and_then(|name| {
                project::find_root(&input)
                    .and_then(|root| project::read_manifest(&root).ok())
                    .and_then(|manifest| manifest.tools.get(&name).cloned())
            });
            pack::build_with_kind(&input, &output, permissions, kind, tool)?;
            println!("已生成独立程序：{}", output.display());
            Ok(())
        }
        "验证包" | "inspect-package" => {
            let path = PathBuf::from(args.get(1).ok_or("缺少独立程序路径")?);
            let embedded = pack::read_embedded_path(&path)?
                .ok_or_else(|| format!("文件没有内嵌衡枢载荷：{}", path.display()))?;
            let kind = match embedded.descriptor.kind {
                pack::PackageKind::Cli => "CLI",
                pack::PackageKind::Gui => "GUI",
            };
            println!(
                "包验证通过：类型={kind}，载荷版本={}，入口版本={}，模块={}，工具={}",
                pack::PAYLOAD_VERSION,
                embedded.descriptor.entry_version,
                embedded.modules.len(),
                embedded
                    .descriptor
                    .tool
                    .as_ref()
                    .map(|tool| tool.name.as_str())
                    .unwrap_or("无")
            );
            Ok(())
        }
        "添加" | "add" => {
            let root = project::find_root(&std::env::current_dir().map_err(|e| e.to_string())?)
                .ok_or("当前目录不在衡枢项目中")?;
            project::add_dependency(
                &root,
                args.get(1).ok_or("缺少依赖名")?,
                &option(&args, &["--路径", "--path"]).ok_or("缺少 --路径")?,
                &option(&args, &["--版本", "--version"]).unwrap_or_else(|| "*".into()),
            )?;
            println!("依赖已经添加并锁定");
            Ok(())
        }
        "移除" | "remove" => {
            let root = project::find_root(&std::env::current_dir().map_err(|e| e.to_string())?)
                .ok_or("当前目录不在衡枢项目中")?;
            project::remove_dependency(&root, args.get(1).ok_or("缺少依赖名")?)?;
            println!("依赖已经移除");
            Ok(())
        }
        "更新" | "update" => {
            let root = project::find_root(&std::env::current_dir().map_err(|e| e.to_string())?)
                .ok_or("当前目录不在衡枢项目中")?;
            project::update_lock(&root)?;
            println!("衡枢.lock 已更新");
            Ok(())
        }
        "缓存" | "cache" => {
            let root = project::find_root(&std::env::current_dir().map_err(|e| e.to_string())?)
                .ok_or("当前目录不在衡枢项目中")?;
            let path = project::cache_dependency(&root, args.get(1).ok_or("缺少依赖名")?)?;
            println!("依赖已缓存：{}", path.display());
            Ok(())
        }
        "审计" | "audit" => {
            let path = args
                .get(1)
                .filter(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
            let root = project::find_root(&path).ok_or("找不到衡枢.toml")?;
            let report = project::audit(&root)?;
            if let Some(output) = option(&args, &["-o", "--输出", "--output"]) {
                crate::security::PermissionSet::for_source(&root)
                    .write_file_atomic(
                        Path::new(&output),
                        report.as_bytes(),
                        crate::privacy::Endpoint::LocalFile,
                        &crate::execution::Sensitivity::default(),
                    )
                    .map_err(|e| format!("写入审计报告失败：{e}"))?;
                println!("依赖审计通过：{output}");
            } else {
                println!("{report}");
            }
            Ok(())
        }
        "发布" | "release" => {
            let input = project_or_source(args.get(1))?;
            let output = option(&args, &["-o", "--输出", "--output"])
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
            let archive = release::build(&input, &output, permissions(&input, rest(&args, 2))?)?;
            println!("发布归档已生成：{}", archive.display());
            Ok(())
        }
        "格式化" | "format" => {
            let path = args
                .get(1)
                .filter(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
            let check = args
                .iter()
                .any(|argument| matches!(argument.as_str(), "--检查" | "--check"));
            let changed = project::format(&path, check)?;
            if check && changed > 0 {
                return Err(format!("有 {changed} 个文件需要格式化"));
            }
            println!("格式化完成，处理 {changed} 个变更文件");
            Ok(())
        }
        "测试" | "test" => {
            let explicit = test_path(&args);
            let path = match explicit {
                Some(path) => path,
                None => {
                    let current = std::env::current_dir().map_err(|e| e.to_string())?;
                    project::find_root(&current).unwrap_or(current)
                }
            };
            let filter = option(&args, &["--筛选", "--filter"]);
            let list_only = args
                .iter()
                .any(|argument| matches!(argument.as_str(), "--列出" | "--list"));
            let fail_fast = args
                .iter()
                .any(|argument| matches!(argument.as_str(), "--遇错即停" | "--fail-fast"));
            let allow_empty = args
                .iter()
                .any(|argument| matches!(argument.as_str(), "--允许空测试" | "--allow-empty"));
            let report = project::test_with_options(
                &path,
                project::TestOptions {
                    filter: filter.as_deref(),
                    list_only,
                    fail_fast,
                    allow_empty,
                },
            )?;
            if list_only {
                for (file, name) in &report.listed {
                    println!("{} :: {name}", file.display());
                }
                println!("共发现 {} 项测试", report.selected);
                return Ok(());
            }
            for (file, name, error) in &report.failed {
                eprintln!("测试失败：{} :: {name}\n{error}", file.display());
            }
            println!(
                "测试结果：{} 项通过，{} 项失败",
                report.passed.len(),
                report.failed.len()
            );
            if report.failed.is_empty() {
                Ok(())
            } else {
                Err("存在失败测试".into())
            }
        }
        "文档" | "doc" => {
            let path = args
                .get(1)
                .filter(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
            let root = project::find_root(&path).unwrap_or(path);
            let output = option(&args, &["-o", "--输出", "--output"])
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("文档/index.html"));
            project::document(&root, &output)?;
            println!("项目文档已生成：{}", output.display());
            Ok(())
        }
        "基准" | "bench" => {
            let source = project_or_source(args.get(1))?;
            let iterations = option(&args, &["--次数", "--iterations"])
                .and_then(|value| value.parse().ok())
                .unwrap_or(10);
            let (total, average) = project::benchmark(&source, iterations)?;
            println!("基准结果：{iterations} 次共 {total:.6} 秒，平均 {average:.6} 秒");
            if let Some(limit) = option(&args, &["--最大平均毫秒", "--max-average-ms"])
                .and_then(|value| value.parse::<f64>().ok())
            {
                let actual = average * 1000.0;
                if actual > limit {
                    return Err(format!(
                        "基准回归：平均 {actual:.3} 毫秒，超过阈值 {limit:.3} 毫秒"
                    ));
                }
            }
            Ok(())
        }
        "语言服务" | "lsp" => lsp::run(),
        "版本" | "version" | "--version" => {
            println!("衡枢 {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "帮助" | "help" | "--help" | "-h" => {
            usage();
            Ok(())
        }
        other => Err(format!("未知命令“{other}”")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_execution_limit_uses_natural_duration_text() {
        let args = vec!["--最长执行时间".into(), "1.5秒".into()];
        let parsed = permissions(Path::new("示例.代码"), &args).unwrap();
        assert_eq!(parsed.execution_timeout_ms, Some(1_500));

        let invalid = vec!["--最长执行时间".into(), "1500".into()];
        assert!(permissions(Path::new("示例.代码"), &invalid)
            .unwrap_err()
            .contains("带单位"));
    }

    #[test]
    fn command_line_resource_depth_options_are_forwarded() {
        let args = vec![
            "--最大值嵌套深度".into(),
            "12".into(),
            "--最大模块深度".into(),
            "7".into(),
        ];
        let parsed = permissions(Path::new("示例.代码"), &args).unwrap();
        assert_eq!(parsed.max_value_nesting, Some(12));
        assert_eq!(parsed.max_module_depth, Some(7));
    }

    fn fixed_project_entry(directory: &Path) -> PathBuf {
        fs::write(
            directory.join(project::MANIFEST),
            "[\"项目\"]\n\"名称\" = \"演示\"\n\"版本\" = \"0.1.0\"\n\"入口\" = \"主程序.代码\"\n\n[\"权限\"]\n\"文件\" = [\"数据\"]\n\"网络\" = [\"*.example.com\"]\n\"最长执行时间\" = \"10秒\"\n",
        )
        .unwrap();
        directory.join("主程序.代码")
    }

    #[test]
    fn project_permissions_cannot_be_widened_from_the_command_line() {
        let directory = tempfile::tempdir().unwrap();
        let entry = fixed_project_entry(directory.path());
        for specification in [
            "文件:/etc",
            "网络:evil.example.org",
            "环境:PATH",
            "子进程",
            "全部",
        ] {
            let args = vec!["--允许".to_string(), specification.to_string()];
            let error = permissions(&entry, &args).unwrap_err();
            assert!(
                error.contains("工程已固定权限"),
                "{specification} 应被拒绝，实际：{error}"
            );
        }
        let relaxed = vec!["--最长执行时间".to_string(), "30秒".to_string()];
        assert_eq!(
            permissions(&entry, &relaxed).unwrap().execution_timeout_ms,
            Some(10_000)
        );
    }

    #[test]
    fn tool_parameter_file_and_inline_values_are_validated_before_run() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(project::MANIFEST),
            r#"["项目"]
"名称" = "演示"
"版本" = "0.1.0"
"入口" = "主程序.代码"

["工具"."报表"]
version = 1
name = "报表"
tool_version = "1.0.0"
entry = "主程序.代码"

["工具"."报表"."参数"."日期"]
type_name = "日期"
required = true
"#,
        )
        .unwrap();
        let entry = directory.path().join("主程序.代码");
        fs::write(&entry, "输出(1)\n").unwrap();
        fs::write(
            directory.path().join("参数.json"),
            r#"{"日期":"2026-08-22"}"#,
        )
        .unwrap();
        let values = tool_parameters(
            &entry,
            &[
                "--工具".into(),
                "报表".into(),
                "--参数文件".into(),
                "参数.json".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            values["日期"],
            serde_json::Value::String("2026-08-22".into())
        );
        assert!(tool_parameters(
            &entry,
            &[
                "--工具".into(),
                "报表".into(),
                "--参数文件".into(),
                "../外部.json".into()
            ]
        )
        .is_err());
    }

    #[test]
    fn project_permissions_can_be_tightened_from_the_command_line() {
        let directory = tempfile::tempdir().unwrap();
        let entry = fixed_project_entry(directory.path());
        let args = vec![
            "--允许".to_string(),
            "文件:数据/明细".to_string(),
            "--允许".to_string(),
            "网络:接口.example.com".to_string(),
            "--最长执行时间".to_string(),
            "5秒".to_string(),
        ];
        let parsed = permissions(&entry, &args).unwrap();
        assert_eq!(parsed.file_roots.len(), 1);
        assert!(parsed.file_roots[0].ends_with("数据/明细"));
        assert_eq!(parsed.network_hosts, vec!["接口.example.com".to_string()]);
        assert_eq!(parsed.execution_timeout_ms, Some(5_000));
    }

    #[test]
    fn plain_scripts_still_take_command_line_grants() {
        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("脚本.代码");
        let args = vec![
            "--允许".to_string(),
            "网络:example.com".to_string(),
            "--最长执行时间".to_string(),
            "30秒".to_string(),
        ];
        let parsed = permissions(&entry, &args).unwrap();
        assert!(!parsed.fixed);
        assert_eq!(parsed.network_hosts, vec!["example.com".to_string()]);
        assert_eq!(parsed.execution_timeout_ms, Some(30_000));
    }
}
