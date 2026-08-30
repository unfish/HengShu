//! 依赖管理：清单、锁文件、依赖缓存与审计。
//!
//! 清单和锁文件是同一套依赖事实的两种表示，校验、缓存和审计都要拿两者比对，
//! 因此它们归在同一模块；项目脚手架与根目录定位留在 `project.rs`。

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use super::find_root;

pub const MANIFEST: &str = "衡枢.toml";
pub const LOCKFILE: &str = "衡枢.lock";

#[derive(Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(rename = "项目")]
    pub project: ProjectInfo,
    #[serde(rename = "依赖", default)]
    pub dependencies: BTreeMap<String, Dependency>,
    #[serde(rename = "权限", default)]
    pub permissions: PermissionManifest,
    #[serde(rename = "工具", default)]
    pub tools: BTreeMap<String, crate::execution::ToolManifest>,
    /// 按工具名声明的自动化触发；缺省表示该工具不安装 OS 任务。
    #[serde(rename = "自动化", default)]
    pub automations: BTreeMap<String, crate::automation::AutomationSpec>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    #[serde(rename = "名称")]
    pub name: String,
    #[serde(rename = "版本")]
    pub version: String,
    #[serde(rename = "入口")]
    pub entry: String,
    #[serde(rename = "许可证", default = "default_license")]
    pub license: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    #[serde(rename = "路径")]
    pub path: String,
    #[serde(rename = "版本", default = "any_version")]
    pub version: String,
    #[serde(rename = "校验固定", default)]
    pub checksum_pin: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PermissionManifest {
    /// 用户主动选择能力限制。默认信任项目代码；旧清单只要声明了任一
    /// 能力范围，也继续按受限清单解释。
    #[serde(rename = "限制能力", default)]
    pub restricted: bool,
    #[serde(rename = "文件", default)]
    pub files: Vec<String>,
    #[serde(rename = "网络", default)]
    pub network: Vec<String>,
    #[serde(rename = "环境", default)]
    pub environment: Vec<String>,
    #[serde(rename = "允许子进程", default)]
    pub process: bool,
    #[serde(
        rename = "最长执行时间",
        default,
        deserialize_with = "deserialize_optional_duration",
        serialize_with = "serialize_optional_duration"
    )]
    pub execution_timeout_ms: Option<u64>,
    #[serde(rename = "最大输出字节", default)]
    pub max_output_bytes: Option<usize>,
    #[serde(rename = "最大集合项数", default)]
    pub max_collection_items: Option<usize>,
    #[serde(rename = "最大缓冲字节", default)]
    pub max_buffer_bytes: Option<usize>,
    #[serde(rename = "最大调用深度", default)]
    pub max_call_depth: Option<usize>,
    #[serde(rename = "最大值嵌套深度", default)]
    pub max_value_nesting: Option<usize>,
    #[serde(rename = "最大模块深度", default)]
    pub max_module_depth: Option<usize>,
    #[serde(rename = "最大通道容量", default)]
    pub max_channel_capacity: Option<usize>,
    #[serde(rename = "最大并行工作者", default)]
    pub max_parallel_workers: Option<usize>,
    #[serde(rename = "允许敏感类别", default)]
    pub sensitive_labels: std::collections::BTreeSet<crate::execution::SensitiveLabel>,
    /// 允许外发敏感数据的输出端点；为空表示不额外限制端点，保持旧工程行为。
    #[serde(rename = "允许端点", default)]
    pub endpoints: std::collections::BTreeSet<crate::privacy::Endpoint>,
}

impl PermissionManifest {
    pub fn restricts_capabilities(&self) -> bool {
        self.restricted
            || !self.files.is_empty()
            || !self.network.is_empty()
            || !self.environment.is_empty()
            || self.process
            || !self.sensitive_labels.is_empty()
            || !self.endpoints.is_empty()
    }
}

fn deserialize_optional_duration<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|text| {
            crate::contracts::parse_duration_milliseconds_message(&text, "最长执行时间")
                .map_err(serde::de::Error::custom)
                .and_then(|milliseconds| {
                    u64::try_from(milliseconds)
                        .map_err(|_| serde::de::Error::custom("最长执行时间不能是负数"))
                })
        })
        .transpose()
}

fn serialize_optional_duration<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    value.map(natural_duration_text).serialize(serializer)
}

fn natural_duration_text(milliseconds: u64) -> String {
    if divides_evenly(milliseconds, 3_600_000) {
        format!("{}小时", milliseconds / 3_600_000)
    } else if divides_evenly(milliseconds, 60_000) {
        format!("{}分钟", milliseconds / 60_000)
    } else if divides_evenly(milliseconds, 1_000) {
        format!("{}秒", milliseconds / 1_000)
    } else {
        format!("{milliseconds}毫秒")
    }
}

fn divides_evenly(value: u64, divisor: u64) -> bool {
    value.checked_rem(divisor) == Some(0)
}

#[derive(Serialize, Deserialize)]
struct Lock {
    #[serde(rename = "版本")]
    version: u32,
    #[serde(rename = "依赖")]
    dependencies: BTreeMap<String, LockedDependency>,
}

#[derive(Serialize, Deserialize)]
struct LockedDependency {
    #[serde(rename = "版本")]
    version: String,
    #[serde(rename = "路径")]
    path: String,
    #[serde(rename = "校验")]
    checksum: String,
}

#[derive(Serialize)]
struct AuditDependency {
    name: String,
    version: String,
    source: String,
    license: String,
    checksum: String,
    integrity_pinned: bool,
}

fn any_version() -> String {
    "*".into()
}

fn default_license() -> String {
    "未声明".into()
}

pub fn add_dependency(root: &Path, name: &str, path: &str, version: &str) -> Result<(), String> {
    VersionReq::parse(version).map_err(|e| format!("依赖版本要求无效：{e}"))?;
    let mut manifest = read_manifest(root)?;
    manifest.dependencies.insert(
        name.into(),
        Dependency {
            path: path.into(),
            version: version.into(),
            checksum_pin: None,
        },
    );
    write_manifest(root, &manifest)?;
    update_lock(root)
}

pub fn remove_dependency(root: &Path, name: &str) -> Result<(), String> {
    let mut manifest = read_manifest(root)?;
    manifest.dependencies.remove(name);
    write_manifest(root, &manifest)?;
    update_lock(root)
}

pub fn update_lock(root: &Path) -> Result<(), String> {
    let manifest = read_manifest(root)?;
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut dependencies = BTreeMap::new();
    for (name, dependency) in manifest.dependencies {
        let path = root.join(&dependency.path);
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("依赖“{name}”路径无效：{e}"))?;
        let dependency_manifest = read_manifest(&canonical)?;
        let version = Version::parse(&dependency_manifest.project.version)
            .map_err(|e| format!("依赖“{name}”版本无效：{e}"))?;
        let requirement = VersionReq::parse(&dependency.version)
            .map_err(|e| format!("依赖“{name}”版本要求无效：{e}"))?;
        if !requirement.matches(&version) {
            return Err(format!(
                "依赖“{name}”版本 {} 不满足 {}",
                version, dependency.version
            ));
        }
        let checksum = checksum_tree(&canonical)?;
        if let Some(expected) = dependency.checksum_pin {
            if expected != checksum {
                return Err(format!("依赖“{name}”校验固定与内容不一致"));
            }
        }
        dependencies.insert(
            name,
            LockedDependency {
                version: version.to_string(),
                path: portable_lock_path(&canonical_root, &canonical)?,
                checksum,
            },
        );
    }
    let text = toml::to_string_pretty(&Lock {
        version: 1,
        dependencies,
    })
    .map_err(|e| e.to_string())?;
    crate::security::atomic_write_file(&root.join(LOCKFILE), text.as_bytes(), true)
}

pub fn resolve_module(base_dir: &Path, name: &str) -> Option<PathBuf> {
    let root = find_root(base_dir)?;
    let manifest = read_manifest(&root).ok()?;
    let lock = read_lock(&root).ok()?;
    let (package, rest) = name.split_once('/').unwrap_or((name, ""));
    let declared = manifest.dependencies.get(package)?;
    let dependency = lock.dependencies.get(package)?;
    validate_locked_dependency(package, declared, dependency).ok()?;
    let package_root = resolve_lock_path(&root, &dependency.path).ok()?;
    let package_root = if checksum_tree(&package_root)
        .ok()
        .is_some_and(|sum| sum == dependency.checksum)
    {
        package_root
    } else {
        let cache = root
            .join(".衡枢缓存")
            .join(package)
            .join(&dependency.version);
        if checksum_tree(&cache)
            .ok()
            .is_some_and(|sum| sum == dependency.checksum)
        {
            cache
        } else {
            return None;
        }
    };
    if rest.is_empty() {
        let manifest = read_manifest(&package_root).ok()?;
        Some(package_root.join(manifest.project.entry))
    } else {
        let mut path = package_root.join(rest);
        if path.extension().is_none() {
            path.set_extension("代码");
        }
        Some(path)
    }
}

pub fn cache_dependency(root: &Path, name: &str) -> Result<PathBuf, String> {
    let manifest = read_manifest(root)?;
    let dependency = manifest
        .dependencies
        .get(name)
        .ok_or_else(|| format!("没有声明依赖“{name}”"))?;
    let source = root
        .join(&dependency.path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let dependency_manifest = read_manifest(&source)?;
    let target = root
        .join(".衡枢缓存")
        .join(name)
        .join(&dependency_manifest.project.version);
    if target.exists() {
        fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
    }
    copy_tree(&source, &target)?;
    let checksum = checksum_tree(&target)?;
    let lock = read_lock(root)?;
    let locked = lock
        .dependencies
        .get(name)
        .ok_or_else(|| format!("锁文件缺少依赖“{name}”"))?;
    validate_locked_dependency(name, dependency, locked)?;
    let expected = &locked.checksum;
    if &checksum != expected {
        return Err(format!("依赖“{name}”缓存校验失败"));
    }
    Ok(target)
}

pub fn audit(root: &Path) -> Result<String, String> {
    let manifest = read_manifest(root)?;
    let lock = read_lock(root)?;
    if manifest.dependencies.keys().ne(lock.dependencies.keys()) {
        return Err("衡枢.toml 与衡枢.lock 的依赖集合不一致，请运行“衡枢 更新”".into());
    }
    let mut dependencies = Vec::new();
    for (name, locked) in lock.dependencies {
        let declared = manifest
            .dependencies
            .get(&name)
            .ok_or_else(|| format!("清单缺少锁定依赖“{name}”"))?;
        validate_locked_dependency(&name, declared, &locked)?;
        let source = resolve_lock_path(root, &locked.path)?;
        let actual = checksum_tree(&source)?;
        if actual != locked.checksum {
            return Err(format!("依赖“{name}”内容与衡枢.lock不一致"));
        }
        let dependency_manifest = read_manifest(&source)?;
        if dependency_manifest.project.version != locked.version {
            return Err(format!(
                "依赖“{name}”实际版本 {} 与锁定版本 {} 不一致",
                dependency_manifest.project.version, locked.version
            ));
        }
        dependencies.push(AuditDependency {
            name,
            version: locked.version,
            source: locked.path,
            license: dependency_manifest.project.license,
            checksum: locked.checksum,
            integrity_pinned: declared.checksum_pin.is_some(),
        });
    }
    serde_json::to_string_pretty(&serde_json::json!({
        "项目": manifest.project.name,
        "版本": manifest.project.version,
        "许可证": manifest.project.license,
        "依赖": dependencies,
        "状态": "通过"
    }))
    .map_err(|e| e.to_string())
}

fn read_lock(root: &Path) -> Result<Lock, String> {
    let lock: Lock = toml::from_str(
        &crate::resource::read_string_limited(
            &root.join(LOCKFILE),
            crate::resource::MAX_METADATA_BYTES,
        )
        .map_err(|e| format!("读取 {} 失败：{e}", root.join(LOCKFILE).display()))?,
    )
    .map_err(|e| format!("锁文件格式无效：{e}"))?;
    if lock.version != 1 {
        return Err(format!("不支持的锁文件版本 {}，当前版本为 1", lock.version));
    }
    Ok(lock)
}

fn validate_locked_dependency(
    name: &str,
    declared: &Dependency,
    locked: &LockedDependency,
) -> Result<(), String> {
    if declared
        .checksum_pin
        .as_ref()
        .is_some_and(|expected| expected != &locked.checksum)
    {
        return Err(format!("依赖“{name}”校验固定与锁文件不一致"));
    }
    let requirement = VersionReq::parse(&declared.version)
        .map_err(|e| format!("依赖“{name}”版本要求无效：{e}"))?;
    let version =
        Version::parse(&locked.version).map_err(|e| format!("依赖“{name}”锁定版本无效：{e}"))?;
    if !requirement.matches(&version) {
        return Err(format!(
            "依赖“{name}”锁定版本 {version} 不满足 {}",
            declared.version
        ));
    }
    Ok(())
}

fn resolve_lock_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return Err("锁文件依赖路径必须是相对于项目根的路径".into());
    }
    Ok(root.join(path))
}

fn portable_lock_path(root: &Path, target: &Path) -> Result<String, String> {
    let relative = relative_path(root, target)
        .ok_or_else(|| "依赖与项目必须位于可使用相对路径表示的文件系统中".to_string())?;
    Ok(relative.to_string_lossy().into_owned())
}

fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    if from.first() != to.first() {
        return None;
    }
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

pub fn read_manifest(root: &Path) -> Result<Manifest, String> {
    toml::from_str(
        &crate::resource::read_string_limited(
            &root.join(MANIFEST),
            crate::resource::MAX_METADATA_BYTES,
        )
        .map_err(|e| format!("读取 {} 失败：{e}", root.join(MANIFEST).display()))?,
    )
    .map_err(|e| e.to_string())
}

pub(super) fn write_manifest(root: &Path, manifest: &Manifest) -> Result<(), String> {
    fs::write(
        root.join(MANIFEST),
        toml::to_string_pretty(manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn checksum_tree(root: &Path) -> Result<String, String> {
    let mut files =
        WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| {
                !entry.path().components().any(|component| {
                    matches!(component.as_os_str().to_str(), Some("target" | ".git"))
                }) && entry.file_name() != LOCKFILE
            })
            .map(|entry| entry.path().to_path_buf())
            .collect::<Vec<_>>();
    files.sort();
    let mut digest = Sha256::new();
    for file in files {
        digest.update(
            file.strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .as_bytes(),
        );
        let mut input = fs::File::open(&file).map_err(|e| e.to_string())?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    for entry in WalkDir::new(source) {
        let entry = entry.map_err(|e| e.to_string())?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(|e| e.to_string())?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(entry.path(), destination).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::create;
    #[test]
    fn creates_project_and_locks_local_dependency() {
        let directory = tempfile::tempdir().unwrap();
        let dependency = directory.path().join("依赖");
        let project = directory.path().join("项目");
        create("依赖", &dependency).unwrap();
        create("项目", &project).unwrap();
        add_dependency(&project, "依赖", "../依赖", "^0.1").unwrap();
        assert!(project.join(LOCKFILE).exists());
        let lock: Lock =
            toml::from_str(&fs::read_to_string(project.join(LOCKFILE)).unwrap()).unwrap();
        assert!(!Path::new(&lock.dependencies["依赖"].path).is_absolute());
        assert!(resolve_module(&project, "依赖").unwrap().exists());
        let report = audit(&project).unwrap();
        assert!(report.contains("\"状态\": \"通过\""));
        assert!(report.contains("\"integrity_pinned\": false"));
        assert!(!report.contains("\"signed\""));
        fs::write(dependency.join("src/主程序.代码"), "输出(\"已篡改\")\n").unwrap();
        assert!(audit(&project).unwrap_err().contains("不一致"));
        assert!(resolve_module(&project, "依赖").is_none());
    }

    #[test]
    fn checksum_pin_uses_only_the_current_field_name() {
        assert!(toml::from_str::<Dependency>(
            "\"路径\" = \"../依赖\"\n\"版本\" = \"*\"\n\"签名\" = \"旧摘要\"\n"
        )
        .is_err());
        let dependency: Dependency =
            toml::from_str("\"路径\" = \"../依赖\"\n\"版本\" = \"*\"\n\"校验固定\" = \"摘要\"\n")
                .unwrap();
        assert_eq!(dependency.checksum_pin.as_deref(), Some("摘要"));
        let serialized = toml::to_string(&dependency).unwrap();
        assert!(serialized.contains("校验固定"));
        assert!(!serialized.contains("签名"));

        let directory = tempfile::tempdir().unwrap();
        let dependency_root = directory.path().join("依赖");
        let project_root = directory.path().join("项目");
        create("依赖", &dependency_root).unwrap();
        create("项目", &project_root).unwrap();
        add_dependency(&project_root, "依赖", "../依赖", "*").unwrap();
        let mut manifest = read_manifest(&project_root).unwrap();
        manifest.dependencies.get_mut("依赖").unwrap().checksum_pin = Some("错误摘要".into());
        write_manifest(&project_root, &manifest).unwrap();
        assert!(update_lock(&project_root)
            .unwrap_err()
            .contains("校验固定与内容不一致"));
    }

    #[test]
    fn relative_lock_paths_survive_project_moves_and_absolute_locks_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("原目录");
        let dependency = original.join("依赖");
        let project = original.join("项目");
        create("依赖", &dependency).unwrap();
        create("项目", &project).unwrap();
        add_dependency(&project, "依赖", "../依赖", "^0.1").unwrap();

        let moved = directory.path().join("移动后");
        fs::rename(&original, &moved).unwrap();
        let moved_project = moved.join("项目");
        assert!(resolve_module(&moved_project, "依赖").unwrap().exists());

        let mut lock: Lock =
            toml::from_str(&fs::read_to_string(moved_project.join(LOCKFILE)).unwrap()).unwrap();
        lock.dependencies.get_mut("依赖").unwrap().path = moved
            .join("依赖")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        fs::write(
            moved_project.join(LOCKFILE),
            toml::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();
        assert!(resolve_module(&moved_project, "依赖").is_none());
        assert!(audit(&moved_project)
            .unwrap_err()
            .contains("必须是相对于项目根"));
    }

    #[test]
    fn lock_writer_rejects_paths_without_a_relative_representation() {
        let error = portable_lock_path(Path::new("卷甲/项目"), Path::new("卷乙/依赖")).unwrap_err();
        assert!(error.contains("相对路径"));
    }

    #[test]
    fn runtime_and_audit_enforce_pin_lock_set_and_lock_version() {
        let directory = tempfile::tempdir().unwrap();
        let dependency = directory.path().join("依赖");
        let project = directory.path().join("项目");
        create("依赖", &dependency).unwrap();
        create("项目", &project).unwrap();
        add_dependency(&project, "依赖", "../依赖", "*").unwrap();

        let pinned = checksum_tree(&dependency).unwrap();
        let mut manifest = read_manifest(&project).unwrap();
        manifest.dependencies.get_mut("依赖").unwrap().checksum_pin = Some(pinned);
        write_manifest(&project, &manifest).unwrap();
        update_lock(&project).unwrap();

        fs::write(dependency.join("src/主程序.代码"), "输出(\"篡改\")\n").unwrap();
        let forged = checksum_tree(&dependency).unwrap();
        let mut lock = read_lock(&project).unwrap();
        lock.dependencies.get_mut("依赖").unwrap().checksum = forged;
        fs::write(
            project.join(LOCKFILE),
            toml::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();
        assert!(resolve_module(&project, "依赖").is_none());
        assert!(audit(&project).unwrap_err().contains("校验固定"));

        let mut lock = read_lock(&project).unwrap();
        lock.dependencies.clear();
        fs::write(
            project.join(LOCKFILE),
            toml::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();
        assert!(audit(&project).unwrap_err().contains("依赖集合不一致"));

        lock.version = 99;
        fs::write(
            project.join(LOCKFILE),
            toml::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();
        assert!(audit(&project).unwrap_err().contains("锁文件版本"));
    }

    #[test]
    fn cached_dependency_resolves_after_source_disappears() {
        let directory = tempfile::tempdir().unwrap();
        let dependency = directory.path().join("依赖");
        let project = directory.path().join("项目");
        create("依赖", &dependency).unwrap();
        create("项目", &project).unwrap();
        add_dependency(&project, "依赖", "../依赖", "^0.1").unwrap();
        let cache = cache_dependency(&project, "依赖").unwrap();
        assert!(cache.join("src/主程序.代码").exists());
        fs::remove_dir_all(&dependency).unwrap();
        assert!(resolve_module(&project, "依赖").unwrap().exists());
    }

    #[test]
    fn manifest_uses_natural_duration_text_for_execution_limit() {
        let text = r#"
["项目"]
"名称" = "示例"
"版本" = "0.1.0"
"入口" = "src/主程序.代码"

["权限"]
"最长执行时间" = "1.5秒"
"最大值嵌套深度" = 12
"最大模块深度" = 7
"#;
        let manifest: Manifest = toml::from_str(text).unwrap();
        assert_eq!(manifest.permissions.execution_timeout_ms, Some(1_500));
        assert_eq!(manifest.permissions.max_value_nesting, Some(12));
        assert_eq!(manifest.permissions.max_module_depth, Some(7));
        let encoded = toml::to_string(&manifest).unwrap();
        assert!(encoded.contains("\"最长执行时间\" = \"1500毫秒\""));
        let invalid = text.replace("1.5秒", "一会儿");
        let error = match toml::from_str::<Manifest>(&invalid) {
            Ok(_) => panic!("无效时间不应通过"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("带单位"));
    }
}
