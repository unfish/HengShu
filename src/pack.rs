use crate::ast::{Expr, MatchPattern, Program, Stmt};
use crate::bytecode::BytecodeProgram;
use crate::checker;
use crate::compiler;
use crate::execution::ToolManifest;
use crate::parser::parse;
use crate::resource::{
    read_file_limited, read_string_limited, MAX_EXECUTABLE_BYTES, MAX_SOURCE_BYTES,
};
use crate::security::PermissionSet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 16] = b"HENGSHU_PAYLOAD2";
const FOOTER: usize = 16 + 32 + 8 + 16;
const MAX_PACKAGED_MODULES: usize = 4096;
const MAX_PACKAGED_SOURCE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) const PAYLOAD_VERSION: u32 = 14;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PackageKind {
    Cli,
    Gui,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageDescriptor {
    pub kind: PackageKind,
    pub tool: Option<ToolManifest>,
    #[serde(default)]
    pub automation: Option<crate::automation::AutomationSpec>,
    pub entry_version: u32,
}

impl PackageDescriptor {
    pub fn validate(&self) -> Result<(), String> {
        if self.entry_version == 0 {
            return Err("包入口版本不能为 0".into());
        }
        if self.kind == PackageKind::Gui && self.tool.is_none() {
            return Err("GUI 包需要工具 schema，请指定 --工具".into());
        }
        if let Some(tool) = &self.tool {
            tool.validate()?;
            match self.kind {
                PackageKind::Gui if tool.gui.is_none() => return Err("GUI 包缺少 GUI 配置".into()),
                PackageKind::Cli if tool.cli.is_none() => return Err("CLI 包缺少 CLI 配置".into()),
                _ => {}
            }
        }
        if let Some(automation) = &self.automation {
            if self.kind != PackageKind::Cli || self.tool.is_none() {
                return Err("自动化声明只能固化到带工具 schema 的 CLI 包".into());
            }
            automation.validate()?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Payload {
    version: u32,
    bytecode: BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    source: String,
    permissions: PermissionSet,
    descriptor: PackageDescriptor,
}

pub struct EmbeddedProgram {
    pub bytecode: BytecodeProgram,
    pub modules: BTreeMap<String, BytecodeProgram>,
    #[allow(dead_code)]
    pub source: String,
    pub permissions: PermissionSet,
    pub descriptor: PackageDescriptor,
}

pub fn build(
    source_path: &Path,
    output_path: &Path,
    permissions: PermissionSet,
) -> Result<(), String> {
    build_with_kind(
        source_path,
        output_path,
        permissions,
        PackageKind::Cli,
        None,
    )
}

pub fn build_with_kind(
    source_path: &Path,
    output_path: &Path,
    permissions: PermissionSet,
    kind: PackageKind,
    tool: Option<ToolManifest>,
) -> Result<(), String> {
    let automation = packaged_automation(source_path, tool.as_ref());
    PackageDescriptor {
        kind,
        tool: tool.clone(),
        automation: automation.clone(),
        entry_version: 1,
    }
    .validate()?;
    let source = read_string_limited(source_path, MAX_SOURCE_BYTES)
        .map_err(|e| format!("读取源码失败：{e}"))?;
    let source_snapshot = source.clone();
    let source_dir = source_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let permissions = permissions.into_portable_roots(&source_dir);
    let (bytecode, modules) = crate::resource::on_compile_stack(move || {
        let program = parse(&source)?;
        checker::check(&program)?;
        compile_with_modules(&program, &source_dir)
    })?;
    let payload = encode(
        &bytecode,
        &modules,
        source_snapshot,
        permissions,
        PackageDescriptor {
            kind,
            tool,
            automation,
            entry_version: 1,
        },
    )?;
    let current = pack_host_binary(kind)?;
    let mut executable = read_file_limited(&current, MAX_EXECUTABLE_BYTES)
        .map_err(|e| format!("读取衡枢二进制失败：{e}"))?;
    if let Some((start, _)) = payload_bounds(&executable) {
        executable.truncate(start);
    }
    executable
        .len()
        .checked_add(payload.len())
        .and_then(|size| size.checked_add(FOOTER))
        .filter(|size| *size <= MAX_EXECUTABLE_BYTES)
        .ok_or_else(|| format!("打包程序超过 {MAX_EXECUTABLE_BYTES} 字节上限"))?;
    #[cfg(target_os = "macos")]
    let linkedit = prepare_macos(&mut executable)?;
    executable.extend_from_slice(&payload);
    executable.extend_from_slice(MAGIC);
    executable.extend_from_slice(&Sha256::digest(&payload));
    executable.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    executable.extend_from_slice(MAGIC);
    #[cfg(target_os = "macos")]
    update_linkedit(&mut executable, linkedit)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败：{e}"))?;
    }
    fs::write(output_path, executable).map_err(|e| format!("写入打包程序失败：{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(output_path)
            .map_err(|e| e.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(output_path, permissions).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(output_path)
            .status()
            .map_err(|e| format!("启动代码签名失败：{e}"))?;
        if !status.success() {
            return Err("macOS 独立程序代码签名失败".into());
        }
    }
    Ok(())
}

fn packaged_automation(
    source_path: &Path,
    tool: Option<&ToolManifest>,
) -> Option<crate::automation::AutomationSpec> {
    let tool = tool?;
    let root = crate::project::find_root(source_path)?;
    crate::project::read_manifest(&root)
        .ok()?
        .automations
        .get(&tool.name)
        .cloned()
}

fn pack_host_binary(kind: PackageKind) -> Result<PathBuf, String> {
    if kind == PackageKind::Cli {
        return std::env::current_exe().map_err(|error| format!("无法定位衡枢二进制：{error}"));
    }
    if let Ok(path) = std::env::var("HENGSHU_GUI_BINARY") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }
    let current =
        std::env::current_exe().map_err(|error| format!("无法定位衡枢二进制：{error}"))?;
    let directory = current.parent().ok_or("无法定位 GUI 二进制目录")?;
    let candidates = if cfg!(target_os = "macos") {
        let mut candidates = vec![
            directory.join("衡枢GUI"),
            directory.join("衡枢GUI.app/Contents/MacOS/衡枢GUI"),
        ];
        let mut app_executables = Vec::new();
        if let Ok(entries) = fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("app") {
                    continue;
                }
                let contents = path.join("Contents/MacOS");
                if let Ok(executables) = fs::read_dir(contents) {
                    app_executables.extend(executables.flatten().map(|entry| entry.path()));
                }
            }
        }
        app_executables.sort();
        app_executables.dedup();
        if app_executables.len() == 1 {
            candidates.push(app_executables.remove(0));
        } else if app_executables.len() > 1 {
            return Err(
                "同目录存在多个 macOS GUI App，无法安全选择 GUI 宿主；请设置 HENGSHU_GUI_BINARY"
                    .into(),
            );
        }
        candidates
    } else if cfg!(windows) {
        vec![directory.join("衡枢GUI.exe"), directory.join("衡枢GUI")]
    } else {
        vec![directory.join("衡枢GUI")]
    };
    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .ok_or_else(|| {
            "找不到 GUI 宿主二进制：请将衡枢GUI（或 macOS 的衡枢GUI.app）放在衡枢同目录，或设置 HENGSHU_GUI_BINARY".to_string()
        })
}

fn packaged_module_type_id(module_id: &str) -> Result<u32, String> {
    let value = module_id
        .strip_prefix("模块:")
        .ok_or_else(|| format!("无效的打包模块身份“{module_id}”"))?;
    u32::from_str_radix(value, 16).map_err(|_| format!("无效的打包模块身份“{module_id}”"))
}

pub fn read_embedded() -> Result<Option<EmbeddedProgram>, String> {
    let path = std::env::current_exe().map_err(|e| e.to_string())?;
    read_embedded_path(&path)
}

pub fn read_embedded_path(path: &Path) -> Result<Option<EmbeddedProgram>, String> {
    let bytes = read_file_limited(path, MAX_EXECUTABLE_BYTES).map_err(|e| e.to_string())?;
    let Some((start, end)) = payload_bounds(&bytes) else {
        return Ok(None);
    };
    let footer = &bytes[end..end + FOOTER];
    let expected = &footer[16..48];
    let payload = &bytes[start..end];
    use subtle::ConstantTimeEq;
    if !bool::from(Sha256::digest(payload).as_slice().ct_eq(expected)) {
        return Err("内嵌程序校验失败，文件可能已经损坏".into());
    }
    decode(payload).map(Some)
}

fn payload_bounds(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < FOOTER {
        return None;
    }
    for magic_at in (FOOTER - MAGIC.len()..=bytes.len() - MAGIC.len()).rev() {
        if &bytes[magic_at..magic_at + MAGIC.len()] != MAGIC {
            continue;
        }
        let footer_start = magic_at + MAGIC.len() - FOOTER;
        if &bytes[footer_start..footer_start + MAGIC.len()] != MAGIC {
            continue;
        }
        let length_at = footer_start + MAGIC.len() + 32;
        let length = u64::from_le_bytes(bytes[length_at..length_at + 8].try_into().ok()?) as usize;
        if length == 0 {
            continue;
        }
        let Some(start) = footer_start.checked_sub(length) else {
            continue;
        };
        return Some((start, footer_start));
    }
    None
}

#[cfg(target_os = "macos")]
fn prepare_macos(bytes: &mut Vec<u8>) -> Result<usize, String> {
    if bytes.len() < 32 || read_u32(bytes, 0)? != 0xfeedfacf {
        return Err("当前衡枢程序不是受支持的 64 位 Mach-O".into());
    }
    let commands = read_u32(bytes, 16)? as usize;
    let commands_size = read_u32(bytes, 20)? as usize;
    let mut offset = 32usize;
    let mut linkedit = None;
    let mut signature_offset = None;
    let mut signature_command = None;
    for _ in 0..commands {
        if offset + 8 > bytes.len() {
            return Err("Mach-O 加载命令被截断".into());
        }
        let command = read_u32(bytes, offset)?;
        let size = read_u32(bytes, offset + 4)? as usize;
        if size < 8 || offset + size > bytes.len() {
            return Err("Mach-O 加载命令长度无效".into());
        }
        if command == 0x19 && size >= 72 {
            let segment = &bytes[offset + 8..offset + 24];
            if segment.starts_with(b"__LINKEDIT") {
                linkedit = Some(offset);
            }
        } else if command == 0x1d && size >= 16 {
            let data_offset = read_u32(bytes, offset + 8)? as usize;
            if data_offset > 0 {
                signature_offset = Some(data_offset);
            }
            signature_command = Some((offset, size));
        }
        offset += size;
    }
    if let Some(signature_offset) = signature_offset {
        if signature_offset <= bytes.len() {
            bytes.truncate(signature_offset);
        }
    }
    if let Some((command_offset, command_size)) = signature_command {
        let command_end = 32 + commands_size;
        bytes.copy_within(command_offset + command_size..command_end, command_offset);
        bytes[command_end - command_size..command_end].fill(0);
        bytes[16..20].copy_from_slice(&((commands - 1) as u32).to_le_bytes());
        bytes[20..24].copy_from_slice(&((commands_size - command_size) as u32).to_le_bytes());
        if linkedit.is_some_and(|offset| offset > command_offset) {
            linkedit = linkedit.map(|offset| offset - command_size);
        }
    }
    linkedit.ok_or_else(|| "Mach-O 缺少 __LINKEDIT 段".into())
}

#[cfg(target_os = "macos")]
fn update_linkedit(bytes: &mut [u8], offset: usize) -> Result<(), String> {
    if offset + 56 > bytes.len() {
        return Err("Mach-O __LINKEDIT 段无效".into());
    }
    let file_offset = read_u64(bytes, offset + 40)? as usize;
    if file_offset > bytes.len() {
        return Err("Mach-O __LINKEDIT 文件偏移无效".into());
    }
    let file_size = bytes.len() - file_offset;
    let virtual_size = (file_size + 0x3fff) & !0x3fff;
    bytes[offset + 32..offset + 40].copy_from_slice(&(virtual_size as u64).to_le_bytes());
    bytes[offset + 48..offset + 56].copy_from_slice(&(file_size as u64).to_le_bytes());
    Ok(())
}

#[cfg(target_os = "macos")]
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "Mach-O 偏移溢出".to_string())?;
    let data = bytes
        .get(offset..end)
        .ok_or_else(|| "Mach-O 数据被截断".to_string())?;
    let data: [u8; 4] = data
        .try_into()
        .map_err(|_| "Mach-O 32 位字段长度无效".to_string())?;
    Ok(u32::from_le_bytes(data))
}

#[cfg(target_os = "macos")]
fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| "Mach-O 偏移溢出".to_string())?;
    let data = bytes
        .get(offset..end)
        .ok_or_else(|| "Mach-O 数据被截断".to_string())?;
    let data: [u8; 8] = data
        .try_into()
        .map_err(|_| "Mach-O 64 位字段长度无效".to_string())?;
    Ok(u64::from_le_bytes(data))
}

fn encode(
    bytecode: &BytecodeProgram,
    modules: &BTreeMap<String, BytecodeProgram>,
    source: String,
    permissions: PermissionSet,
    descriptor: PackageDescriptor,
) -> Result<Vec<u8>, String> {
    let payload = Payload {
        version: PAYLOAD_VERSION,
        bytecode: bytecode.clone(),
        modules: modules.clone(),
        source,
        permissions,
        descriptor,
    };
    postcard::to_allocvec(&payload).map_err(|e| format!("编译中间表示失败：{e}"))
}

fn decode(bytes: &[u8]) -> Result<EmbeddedProgram, String> {
    let (payload, remaining): (Payload, &[u8]) =
        postcard::take_from_bytes(bytes).map_err(|e| format!("读取编译中间表示失败：{e}"))?;
    if !remaining.is_empty() {
        return Err("编译载荷包含未知尾部数据".into());
    }
    if payload.version != PAYLOAD_VERSION {
        return Err(format!(
            "不支持的编译载荷版本 {}，当前版本为 {PAYLOAD_VERSION}",
            payload.version
        ));
    }
    payload.descriptor.validate()?;
    payload.bytecode.verify()?;
    for (name, bytecode) in &payload.modules {
        bytecode
            .verify()
            .map_err(|error| format!("模块“{name}”字节码无效：{error}"))?;
    }
    Ok(EmbeddedProgram {
        bytecode: payload.bytecode,
        modules: payload.modules,
        source: payload.source,
        permissions: payload.permissions,
        descriptor: payload.descriptor,
    })
}

#[derive(Debug)]
struct CollectedModules {
    root_imports: BTreeMap<String, String>,
    programs: BTreeMap<String, Program>,
    module_imports: BTreeMap<String, BTreeMap<String, String>>,
}

/// 编译主程序及其相对导入的模块。独立打包和 Rust 嵌入 API 共用此实现，
/// 因此内存字节码 VM 与独立包看到同一套模块身份和模块导入映射。
pub(crate) fn compile_with_modules(
    program: &Program,
    base: &Path,
) -> Result<(BytecodeProgram, BTreeMap<String, BytecodeProgram>), String> {
    let module_programs = collect_modules(program, base)?;
    let mut bytecode = compiler::compile(program)?;
    bytecode.module_imports = module_programs.root_imports;
    let modules = module_programs
        .programs
        .into_iter()
        .map(|(module_id, mut program)| {
            program.set_module_identity(packaged_module_type_id(&module_id)?);
            let mut bytecode = compiler::compile_module(&program)?;
            bytecode.module_imports = module_programs
                .module_imports
                .get(&module_id)
                .cloned()
                .unwrap_or_default();
            Ok((module_id, bytecode))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    Ok((bytecode, modules))
}

fn collect_modules(program: &[Stmt], base: &Path) -> Result<CollectedModules, String> {
    let mut result = CollectedModules {
        root_imports: BTreeMap::new(),
        programs: BTreeMap::new(),
        module_imports: BTreeMap::new(),
    };
    let mut total_source_bytes = 0_usize;
    let mut path_ids = BTreeMap::<PathBuf, String>::new();
    let mut next_module_id = 1_u32;
    let mut root_modules = Vec::new();
    collect_imports(program, &mut root_modules);
    let mut pending = root_modules
        .into_iter()
        .map(|module| {
            (
                module,
                Vec::<String>::new(),
                Vec::<String>::new(),
                base.to_path_buf(),
                None::<String>,
            )
        })
        .collect::<Vec<_>>();
    while let Some((name, id_stack, display_stack, current_base, owner)) = pending.pop() {
        if crate::runtime::modules::builtin_module_functions(&name).is_some() {
            continue;
        }
        let mut path = current_base.join(&name);
        if path.extension().is_none() {
            path.set_extension("代码");
        }
        if !path.exists() {
            if let Some(dependency_path) = crate::project::resolve_module(&current_base, &name) {
                path = dependency_path;
            }
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| format!("打包时找不到模块“{name}”：{}", path.display()))?;
        let module_id = if let Some(module_id) = path_ids.get(&canonical) {
            module_id.clone()
        } else {
            let module_id = format!("模块:{next_module_id:08x}");
            next_module_id = next_module_id
                .checked_add(1)
                .ok_or_else(|| "打包模块 ID 已耗尽".to_string())?;
            path_ids.insert(canonical, module_id.clone());
            module_id
        };
        if let Some(owner) = &owner {
            result
                .module_imports
                .entry(owner.clone())
                .or_default()
                .insert(name.clone(), module_id.clone());
        } else {
            result.root_imports.insert(name.clone(), module_id.clone());
        }
        if let Some(index) = id_stack.iter().position(|id| id == &module_id) {
            let mut cycle = display_stack[index..].to_vec();
            cycle.push(name);
            return Err(format!("模块循环依赖：{}", cycle.join(" -> ")));
        }
        if result.programs.contains_key(&module_id) {
            continue;
        }
        if result.programs.len() >= MAX_PACKAGED_MODULES {
            return Err(format!("打包模块超过 {MAX_PACKAGED_MODULES} 项限制"));
        }
        let source = read_string_limited(&path, MAX_SOURCE_BYTES)
            .map_err(|_| format!("打包时找不到模块“{name}”：{}", path.display()))?;
        total_source_bytes = total_source_bytes
            .checked_add(source.len())
            .filter(|size| *size <= MAX_PACKAGED_SOURCE_BYTES)
            .ok_or_else(|| format!("打包模块源码累计超过 {MAX_PACKAGED_SOURCE_BYTES} 字节限制"))?;
        let child = parse(&source).map_err(|error| format!("模块“{name}”解析失败：\n{error}"))?;
        checker::check(&child).map_err(|error| format!("模块“{name}”静态检查失败：\n{error}"))?;
        let child_base = path.parent().unwrap_or(&current_base).to_path_buf();
        let mut child_id_stack = id_stack;
        child_id_stack.push(module_id.clone());
        let mut child_display_stack = display_stack;
        child_display_stack.push(name);
        let mut child_modules = Vec::new();
        collect_imports(&child, &mut child_modules);
        pending.extend(child_modules.into_iter().map(|module| {
            (
                module,
                child_id_stack.clone(),
                child_display_stack.clone(),
                child_base.clone(),
                Some(module_id.clone()),
            )
        }));
        result.programs.insert(module_id, child);
    }
    Ok(result)
}

fn collect_imports(statements: &[Stmt], imports: &mut Vec<String>) {
    for statement in statements {
        collect_statement_imports(statement, imports);
    }
}

fn collect_statement_imports(statement: &Stmt, imports: &mut Vec<String>) {
    match statement {
        Stmt::Export { statement, .. } => collect_statement_imports(statement, imports),
        Stmt::TaskGroup { body, .. } | Stmt::Test { body, .. } | Stmt::TestHook { body, .. } => {
            collect_imports(body, imports)
        }
        Stmt::Define { expr, .. } | Stmt::Expr { expr, .. } | Stmt::Throw { expr, .. } => {
            collect_expression_imports(expr, imports)
        }
        Stmt::Assign { target, expr, .. } => {
            collect_expression_imports(target, imports);
            collect_expression_imports(expr, imports);
        }
        Stmt::If {
            branches,
            otherwise,
            ..
        } => {
            for (condition, body) in branches {
                collect_expression_imports(condition, imports);
                collect_imports(body, imports);
            }
            collect_imports(otherwise, imports);
        }
        Stmt::While {
            condition, body, ..
        } => {
            collect_expression_imports(condition, imports);
            collect_imports(body, imports);
        }
        Stmt::For { iterable, body, .. } => {
            collect_expression_imports(iterable, imports);
            collect_imports(body, imports);
        }
        Stmt::Function { params, body, .. } => {
            for parameter in params {
                if let Some(default) = &parameter.default {
                    collect_expression_imports(default, imports);
                }
            }
            collect_imports(body, imports);
        }
        Stmt::Return { expr, .. } => {
            if let Some(expr) = expr {
                collect_expression_imports(expr, imports);
            }
        }
        Stmt::Import { module, .. } => imports.push(module.clone()),
        Stmt::Try {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            collect_imports(body, imports);
            collect_imports(catch_body, imports);
            collect_imports(finally_body, imports);
        }
        Stmt::Require { expr, message, .. } => {
            collect_expression_imports(expr, imports);
            if let Some(message) = message {
                collect_expression_imports(message, imports);
            }
        }
        Stmt::Match {
            expr,
            arms,
            otherwise,
            ..
        } => {
            collect_expression_imports(expr, imports);
            for arm in arms {
                if let MatchPattern::Value(value) = &arm.pattern {
                    collect_expression_imports(value, imports);
                }
                if let Some(guard) = &arm.guard {
                    collect_expression_imports(guard, imports);
                }
                collect_imports(&arm.body, imports);
            }
            collect_imports(otherwise, imports);
        }
        Stmt::Struct { .. } | Stmt::Global { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

fn collect_expression_imports(expression: &Expr, imports: &mut Vec<String>) {
    match expression {
        Expr::List(values, _) => {
            for value in values {
                collect_expression_imports(value, imports);
            }
        }
        Expr::Record(fields, _) => {
            for (_, value) in fields {
                collect_expression_imports(value, imports);
            }
        }
        Expr::Unary { expr, .. } => collect_expression_imports(expr, imports),
        Expr::Binary { left, right, .. } => {
            collect_expression_imports(left, imports);
            collect_expression_imports(right, imports);
        }
        Expr::Call {
            receiver,
            args,
            named,
            ..
        } => {
            collect_expression_imports(receiver, imports);
            for argument in args {
                collect_expression_imports(argument, imports);
            }
            for argument in named {
                collect_expression_imports(&argument.value, imports);
            }
        }
        Expr::Attr { receiver, .. } => collect_expression_imports(receiver, imports),
        Expr::Index {
            receiver, index, ..
        } => {
            collect_expression_imports(receiver, imports);
            collect_expression_imports(index, imports);
        }
        Expr::Function { params, body, .. } => {
            for parameter in params {
                if let Some(default) = &parameter.default {
                    collect_expression_imports(default, imports);
                }
            }
            collect_imports(body, imports);
        }
        Expr::Literal(..) | Expr::Name(..) => {}
    }
}

pub fn default_output(source: &Path) -> PathBuf {
    if source
        .extension()
        .is_some_and(|extension| extension == "代码")
    {
        source.with_extension("")
    } else {
        PathBuf::from(format!("{}.程序", source.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile_collected(
        program: &Program,
        collected: CollectedModules,
    ) -> (BytecodeProgram, BTreeMap<String, BytecodeProgram>) {
        let CollectedModules {
            root_imports,
            programs,
            module_imports,
        } = collected;
        let mut main = compiler::compile(program).unwrap();
        main.module_imports = root_imports;
        let modules = programs
            .into_iter()
            .map(|(module_id, program)| {
                let mut bytecode = compiler::compile_module(&program).unwrap();
                bytecode.module_imports =
                    module_imports.get(&module_id).cloned().unwrap_or_default();
                (module_id, bytecode)
            })
            .collect();
        (main, modules)
    }

    #[test]
    fn payload_round_trip() {
        let program = parse("输出(2)").unwrap();
        let modules = BTreeMap::from([(
            "工具".into(),
            compiler::compile(&parse("输出(1)").unwrap()).unwrap(),
        )]);
        let bytecode = compiler::compile(&program).unwrap();
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.max_call_depth = Some(12);
        permissions.max_value_nesting = Some(9);
        permissions.max_module_depth = Some(4);
        let decoded = decode(
            &encode(
                &bytecode,
                &modules,
                "输出(2)".into(),
                permissions,
                PackageDescriptor {
                    kind: PackageKind::Cli,
                    tool: None,
                    automation: None,
                    entry_version: 1,
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.bytecode, bytecode);
        assert_eq!(decoded.modules["工具"], modules["工具"]);
        assert_eq!(decoded.permissions.max_call_depth, Some(12));
        assert_eq!(decoded.permissions.max_value_nesting, Some(9));
        assert_eq!(decoded.permissions.max_module_depth, Some(4));
        assert_eq!(decoded.descriptor.kind, PackageKind::Cli);
        assert_eq!(decoded.source, "输出(2)");
    }

    #[test]
    fn package_descriptor_requires_matching_entry_configuration() {
        let tool = crate::execution::ToolManifest {
            version: Default::default(),
            name: "工具".into(),
            tool_version: "1".into(),
            entry: "main".into(),
            parameters: BTreeMap::new(),
            gui: None,
            cli: Some(crate::execution::CliSpec {
                command: "run".into(),
                output_format: "json".into(),
                output_root: None,
            }),
            permissions: Default::default(),
        };
        assert!(PackageDescriptor {
            kind: PackageKind::Gui,
            tool: Some(tool.clone()),
            automation: None,
            entry_version: 1
        }
        .validate()
        .is_err());
        assert!(PackageDescriptor {
            kind: PackageKind::Gui,
            tool: None,
            automation: None,
            entry_version: 1
        }
        .validate()
        .is_err());
        assert!(PackageDescriptor {
            kind: PackageKind::Cli,
            tool: Some(tool),
            automation: None,
            entry_version: 1
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn module_cycle_is_reported_with_path() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("甲.代码"), "引入 乙\n").unwrap();
        fs::write(directory.path().join("乙.代码"), "引入 甲\n").unwrap();
        let program = parse("引入 甲\n").unwrap();
        let error = collect_modules(&program, directory.path()).unwrap_err();
        assert!(error.contains("循环依赖"));
        assert!(error.contains("甲"));
        assert!(error.contains("乙"));
    }

    #[test]
    fn nested_modules_resolve_imports_from_their_own_directory() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("子")).unwrap();
        fs::write(directory.path().join("子/甲.代码"), "引入 乙\n").unwrap();
        fs::write(directory.path().join("子/乙.代码"), "定义 值 为 1\n").unwrap();
        let program = parse("引入 子/甲\n").unwrap();
        let modules = collect_modules(&program, directory.path()).unwrap();
        assert_eq!(modules.programs.len(), 2);
    }

    #[test]
    fn visibility_wrapped_imports_are_embedded() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("依赖.代码"), "定义 值 为 1\n").unwrap();
        let program = parse("内部 引入 依赖\n").unwrap();
        let modules = collect_modules(&program, directory.path()).unwrap();
        assert_eq!(modules.programs.len(), 1);
    }

    #[test]
    fn imports_inside_functions_and_branches_are_embedded() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("依赖.代码"), "定义 值 为 7\n").unwrap();
        let program = parse(
            "定义功能 读取（）：\n如果 是：\n引入 依赖\n返回 依赖 的 值\n结束\n结束\n输出(读取())\n",
        )
        .unwrap();
        let collected = collect_modules(&program, directory.path()).unwrap();
        assert_eq!(collected.programs.len(), 1);
        assert!(collected.root_imports.contains_key("依赖"));
    }

    #[test]
    fn same_import_spelling_in_different_directories_has_unique_ids() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("甲")).unwrap();
        fs::create_dir_all(directory.path().join("乙")).unwrap();
        fs::write(
            directory.path().join("甲/入口.代码"),
            "引入 工具\n定义 甲值 为 工具 的 值\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("乙/入口.代码"),
            "引入 工具\n定义 乙值 为 工具 的 值\n",
        )
        .unwrap();
        fs::write(directory.path().join("甲/工具.代码"), "定义 值 为 1\n").unwrap();
        fs::write(directory.path().join("乙/工具.代码"), "定义 值 为 2\n").unwrap();
        let source = "从 甲/入口 引入 甲值\n从 乙/入口 引入 乙值\n输出(甲值, 乙值)\n";
        let program = parse(source).unwrap();
        let collected = collect_modules(&program, directory.path()).unwrap();
        let tool_ids = collected
            .module_imports
            .values()
            .filter_map(|imports| imports.get("工具"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(tool_ids.len(), 2);
        let interpreted = crate::runtime::execute(source, directory.path(), false).unwrap();
        let (main, modules) = compile_collected(&program, collected);
        let compiled = crate::vm::execute(
            &main,
            modules,
            directory.path(),
            false,
            Vec::new(),
            PermissionSet::for_source(directory.path()),
        )
        .unwrap();
        assert_eq!(compiled.output, interpreted.output);
        assert_eq!(compiled.output, ["1 2"]);
    }

    #[test]
    fn module_identity_is_reproducible_across_checkout_directories() {
        fn prepare(root: &Path) -> Program {
            fs::create_dir_all(root.join("子")).unwrap();
            fs::write(
                root.join("子/入口.代码"),
                "引入 工具\n定义 值 为 工具 的 值\n",
            )
            .unwrap();
            fs::write(root.join("子/工具.代码"), "定义 值 为 9\n").unwrap();
            parse("引入 子/入口\n输出(入口 的 值)\n").unwrap()
        }

        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_program = prepare(first.path());
        let second_program = prepare(second.path());
        let first_collected = collect_modules(&first_program, first.path()).unwrap();
        let second_collected = collect_modules(&second_program, second.path()).unwrap();
        let first_compiled = compile_collected(&first_program, first_collected);
        let second_compiled = compile_collected(&second_program, second_collected);
        assert_eq!(first_compiled, second_compiled);
    }
}
