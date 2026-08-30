use crate::project;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use url::Url;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionSet {
    pub file_roots: Vec<PathBuf>,
    pub network_hosts: Vec<String>,
    pub environment_names: Vec<String>,
    pub allow_process: bool,
    pub unrestricted: bool,
    pub execution_timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub max_collection_items: Option<usize>,
    #[serde(default)]
    pub max_buffer_bytes: Option<usize>,
    #[serde(default)]
    pub max_call_depth: Option<usize>,
    #[serde(default)]
    pub max_value_nesting: Option<usize>,
    #[serde(default)]
    pub max_module_depth: Option<usize>,
    #[serde(default)]
    pub max_channel_capacity: Option<usize>,
    #[serde(default)]
    pub max_parallel_workers: Option<usize>,
    #[serde(default)]
    pub sensitive_labels: BTreeSet<crate::execution::SensitiveLabel>,
    /// 允许外发敏感数据的输出端点；为空表示不额外限制端点。
    #[serde(default)]
    pub endpoints: BTreeSet<crate::privacy::Endpoint>,
    /// 工程清单固定的权限上限。为真时命令行只能收紧，不能放宽。
    #[serde(skip)]
    pub fixed: bool,
}

/// 命令行请求的权限。先收集完整请求，再与既有权限求交或求并。
#[derive(Clone, Debug, Default)]
pub struct PermissionRequest {
    file_roots: Vec<PathBuf>,
    network_hosts: Vec<String>,
    environment_names: Vec<String>,
    allow_process: bool,
    unrestricted: bool,
}

impl PermissionRequest {
    pub fn add(&mut self, specification: &str, base_dir: &Path) -> Result<(), String> {
        if specification == "全部" {
            self.unrestricted = true;
            return Ok(());
        }
        if specification == "子进程" {
            self.allow_process = true;
            return Ok(());
        }
        let (kind, value) = specification
            .split_once(':')
            .ok_or_else(|| format!("权限“{specification}”应写成 类别:范围"))?;
        match kind {
            "文件" => self
                .file_roots
                .push(physical(&absolute(base_dir, Path::new(value)))),
            "网络" => self.network_hosts.push(value.into()),
            "环境" => self.environment_names.push(value.into()),
            _ => return Err(format!("未知权限类别“{kind}”")),
        }
        Ok(())
    }
}

impl PermissionSet {
    /// Resolve the project ceiling and optionally apply one named tool's
    /// restrictions. Missing tool declarations preserve legacy behavior.
    pub fn for_tool(base_dir: &Path, name: &str) -> Result<Self, String> {
        let project = Self::for_source(base_dir);
        let Some(root) = project::find_root(base_dir) else {
            return Ok(project);
        };
        let manifest = project::read_manifest(&root)?;
        let Some(tool) = manifest.tools.get(name) else {
            return Ok(project);
        };
        tool.validate()?;
        tool.permissions.restrict(&project, &root)
    }

    pub fn for_source(base_dir: &Path) -> Self {
        if let Some(root) = project::find_root(base_dir) {
            if let Ok(manifest) = project::read_manifest(&root) {
                let restricted = manifest.permissions.restricts_capabilities();
                let file_roots = if restricted && manifest.permissions.files.is_empty() {
                    vec![physical(&absolute(&root, Path::new(".")))]
                } else {
                    manifest
                        .permissions
                        .files
                        .iter()
                        .map(|path| physical(&absolute(&root, Path::new(path))))
                        .collect()
                };
                return Self {
                    file_roots,
                    network_hosts: manifest.permissions.network,
                    environment_names: manifest.permissions.environment,
                    allow_process: manifest.permissions.process,
                    unrestricted: !restricted,
                    execution_timeout_ms: manifest.permissions.execution_timeout_ms,
                    max_output_bytes: manifest.permissions.max_output_bytes,
                    max_collection_items: manifest.permissions.max_collection_items,
                    max_buffer_bytes: manifest.permissions.max_buffer_bytes,
                    max_call_depth: manifest.permissions.max_call_depth,
                    max_value_nesting: manifest.permissions.max_value_nesting,
                    max_module_depth: manifest.permissions.max_module_depth,
                    max_channel_capacity: manifest.permissions.max_channel_capacity,
                    max_parallel_workers: manifest.permissions.max_parallel_workers,
                    sensitive_labels: manifest.permissions.sensitive_labels,
                    endpoints: manifest.permissions.endpoints,
                    fixed: true,
                };
            }
        }
        Self {
            file_roots: vec![physical(&absolute(base_dir, Path::new(".")))],
            network_hosts: Vec::new(),
            environment_names: Vec::new(),
            allow_process: false,
            unrestricted: true,
            execution_timeout_ms: None,
            max_output_bytes: None,
            max_collection_items: None,
            max_buffer_bytes: None,
            max_call_depth: None,
            max_value_nesting: None,
            max_module_depth: None,
            max_channel_capacity: None,
            max_parallel_workers: None,
            sensitive_labels: BTreeSet::new(),
            endpoints: BTreeSet::new(),
            fixed: false,
        }
    }

    /// Explicit sandbox for hosts and tests that execute code they do not trust.
    /// Normal user-owned source uses `for_source`, which is trusted by default.
    pub fn restricted_for_source(base_dir: &Path) -> Self {
        let mut permissions = Self::for_source(base_dir);
        permissions.file_roots = vec![physical(&absolute(base_dir, Path::new(".")))];
        permissions.network_hosts.clear();
        permissions.environment_names.clear();
        permissions.allow_process = false;
        permissions.unrestricted = false;
        permissions.fixed = false;
        permissions
    }

    /// 打包时把位于源码目录内的文件根改写成相对源码目录的相对路径，
    /// 使同一份包不再携带构建机器的绝对路径。显式给出的目录外绝对根按原样保留。
    pub fn into_portable_roots(mut self, source_dir: &Path) -> Self {
        let base = physical(&absolute(Path::new("."), source_dir));
        self.file_roots = self
            .file_roots
            .into_iter()
            .map(|root| match root.strip_prefix(&base) {
                Ok(relative) if relative.as_os_str().is_empty() => PathBuf::from("."),
                Ok(relative) => relative.to_path_buf(),
                Err(_) => root,
            })
            .collect();
        self
    }

    /// 运行打包程序时以程序自身所在目录为基准还原相对文件根，
    /// 让同一份包的可访问范围恒等于“程序目录下的同一组相对位置”，不随启动位置漂移。
    pub fn anchor_file_roots(&mut self, base_dir: &Path) {
        for root in &mut self.file_roots {
            if root.is_relative() {
                *root = physical(&absolute(base_dir, root));
            }
        }
    }

    pub fn buffer_limit(&self) -> usize {
        self.max_buffer_bytes
            .unwrap_or(crate::resource::DEFAULT_MAX_BUFFER_BYTES)
    }

    pub fn collection_limit(&self) -> usize {
        self.max_collection_items
            .unwrap_or(crate::resource::DEFAULT_MAX_COLLECTION_ITEMS)
    }

    pub fn output_limit(&self) -> usize {
        self.max_output_bytes
            .unwrap_or(crate::resource::DEFAULT_MAX_OUTPUT_BYTES)
    }

    pub fn call_depth_limit(&self) -> usize {
        self.max_call_depth
            .unwrap_or(crate::resource::DEFAULT_MAX_CALL_DEPTH)
            .max(1)
    }

    pub fn value_nesting_limit(&self) -> usize {
        self.max_value_nesting
            .unwrap_or(crate::resource::DEFAULT_MAX_VALUE_NESTING)
            .max(1)
    }

    pub fn module_depth_limit(&self) -> usize {
        self.max_module_depth
            .unwrap_or(crate::resource::DEFAULT_MAX_MODULE_DEPTH)
            .max(1)
    }

    /// 通道容量上限。内建上限同时是绝对上限，工程只能收得更紧。
    pub fn channel_capacity_limit(&self) -> usize {
        self.max_channel_capacity
            .unwrap_or(crate::resource::DEFAULT_MAX_CHANNEL_CAPACITY)
            .clamp(1, crate::resource::DEFAULT_MAX_CHANNEL_CAPACITY)
    }

    /// 并行工作者数量上限。内建上限同时是绝对上限，工程只能收得更紧。
    pub fn parallel_worker_limit(&self) -> usize {
        self.max_parallel_workers
            .unwrap_or(crate::resource::DEFAULT_MAX_PARALLEL_WORKERS)
            .clamp(1, crate::resource::DEFAULT_MAX_PARALLEL_WORKERS)
    }

    pub fn apply(&mut self, request: PermissionRequest) -> Result<(), String> {
        if !self.fixed {
            self.unrestricted |= request.unrestricted;
            self.allow_process |= request.allow_process;
            self.file_roots.extend(request.file_roots);
            self.network_hosts.extend(request.network_hosts);
            self.environment_names.extend(request.environment_names);
            return Ok(());
        }
        if request.unrestricted && !self.unrestricted {
            return Err("工程已固定权限，命令行不能授予“全部”权限".into());
        }
        if request.allow_process {
            self.check_process()
                .map_err(|_| "工程已固定权限，命令行不能授予子进程权限".to_string())?;
        }
        for root in &request.file_roots {
            self.check_file(root).map_err(|_| {
                format!("工程已固定权限，命令行不能授予文件路径“{}”", root.display())
            })?;
        }
        for host in &request.network_hosts {
            self.check_network(host)
                .map_err(|_| format!("工程已固定权限，命令行不能授予网络主机“{host}”"))?;
        }
        for name in &request.environment_names {
            self.check_environment(name)
                .map_err(|_| format!("工程已固定权限，命令行不能授予环境变量“{name}”"))?;
        }
        if !request.file_roots.is_empty() {
            self.file_roots = request.file_roots;
        }
        if !request.network_hosts.is_empty() {
            self.network_hosts = request.network_hosts;
        }
        if !request.environment_names.is_empty() {
            self.environment_names = request.environment_names;
        }
        Ok(())
    }

    /// 端点策略：清单未声明端点时视为全部允许，保持旧工程行为不变。
    pub fn endpoint_policy(&self) -> crate::privacy::EndpointPolicy {
        let allowed = if self.endpoints.is_empty() {
            crate::privacy::EndpointPolicy::all_endpoints()
        } else {
            self.endpoints.clone()
        };
        crate::privacy::EndpointPolicy {
            allowed,
            sensitive_allowed: self.sensitive_labels.clone(),
        }
    }

    pub fn check_file(&self, path: &Path) -> Result<(), String> {
        if self.unrestricted {
            return Ok(());
        }
        let path = physical(&absolute(Path::new("."), path));
        if self.file_roots.iter().any(|root| path.starts_with(root)) {
            Ok(())
        } else {
            Err(error(format!("没有访问文件路径“{}”的权限", path.display())))
        }
    }

    /// Host-owned documents use the same file, output-budget and sensitive
    /// endpoint boundary as language-level delivery before any bytes are made
    /// visible at the destination.
    pub fn write_file_atomic(
        &self,
        path: &Path,
        bytes: &[u8],
        endpoint: crate::privacy::Endpoint,
        sensitivity: &crate::execution::Sensitivity,
    ) -> Result<(), String> {
        self.check_file(path)?;
        if bytes.len() > self.output_limit() {
            return Err("宿主输出超过字节预算".into());
        }
        if !sensitivity.labels.is_empty() && !sensitivity.masked && !sensitivity.pseudonymized {
            self.endpoint_policy().allow(endpoint, sensitivity)?;
        }
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("宿主输出不能覆盖符号链接".into());
        }
        atomic_write_file(path, bytes, true)
    }

    pub fn check_network(&self, address: &str) -> Result<(), String> {
        if self.unrestricted {
            return Ok(());
        }
        let host = network_host(address);
        if self.network_hosts.iter().any(|allowed| {
            let allowed = normalize_host(allowed);
            allowed == "*"
                || allowed == host
                || allowed.strip_prefix("*.").is_some_and(|suffix| {
                    host.len() > suffix.len()
                        && host.ends_with(suffix)
                        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
                })
        }) {
            Ok(())
        } else {
            Err(error(format!("没有访问网络主机“{host}”的权限")))
        }
    }

    pub fn check_environment(&self, name: &str) -> Result<(), String> {
        if self.unrestricted
            || self
                .environment_names
                .iter()
                .any(|allowed| allowed == "*" || allowed == name)
        {
            Ok(())
        } else {
            Err(error(format!("没有读取环境变量“{name}”的权限")))
        }
    }

    pub fn check_process(&self) -> Result<(), String> {
        if self.unrestricted || self.allow_process {
            Ok(())
        } else {
            Err(error("没有启动子进程的权限"))
        }
    }

    pub fn check_sensitive(&self, label: &crate::execution::SensitiveLabel) -> Result<(), String> {
        if self.unrestricted || self.sensitive_labels.contains(label) {
            Ok(())
        } else {
            Err(format!("没有外发敏感类别“{label:?}”的权限"))
        }
    }
}

/// Streaming single-file transaction. The temporary file lives beside the
/// destination, and is deleted automatically unless `commit` succeeds.
pub(crate) struct AtomicOutput {
    path: PathBuf,
    temporary: tempfile::NamedTempFile,
}

impl AtomicOutput {
    pub(crate) fn new(path: &Path) -> Result<Self, String> {
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("输出不能覆盖符号链接".into());
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|error| format!("创建输出目录失败：{error}"))?;
        let temporary = tempfile::Builder::new()
            .prefix(".衡枢-")
            .suffix(".临时")
            .tempfile_in(parent)
            .map_err(|error| format!("创建输出临时文件失败：{error}"))?;
        Ok(Self {
            path: path.to_path_buf(),
            temporary,
        })
    }

    pub(crate) fn file_mut(&mut self) -> &mut std::fs::File {
        self.temporary.as_file_mut()
    }

    pub(crate) fn commit(mut self, replace: bool) -> Result<(), String> {
        self.temporary
            .as_file_mut()
            .sync_all()
            .map_err(|error| format!("同步输出临时文件失败：{error}"))?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        if replace {
            self.temporary
                .persist(&self.path)
                .map_err(|error| format!("提交输出失败：{}", error.error))?;
        } else {
            self.temporary
                .persist_noclobber(&self.path)
                .map_err(|error| format!("提交输出失败：{}", error.error))?;
        }
        if let Ok(parent_file) = std::fs::File::open(parent) {
            let _ = parent_file.sync_all();
        }
        Ok(())
    }
}

/// Commit one complete file from a random temporary file in the destination
/// directory. The same-filesystem persist is atomic; dropping on any error
/// removes the temporary file.
pub(crate) fn atomic_write_file(path: &Path, bytes: &[u8], replace: bool) -> Result<(), String> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err("输出不能覆盖符号链接".into());
    }
    let mut output = AtomicOutput::new(path)?;
    output
        .file_mut()
        .write_all(bytes)
        .map_err(|error| format!("写入输出临时文件失败：{error}"))?;
    output.commit(replace)
}

fn network_host(address: &str) -> String {
    if let Some(host) = Url::parse(address)
        .ok()
        .and_then(|url| url.host_str().map(normalize_host))
    {
        return host;
    }
    if let Ok(address) = address.parse::<std::net::SocketAddr>() {
        return normalize_host(&address.ip().to_string());
    }
    if let Ok(address) = address.parse::<std::net::IpAddr>() {
        return normalize_host(&address.to_string());
    }
    let host = address
        .rsplit_once(':')
        .filter(|(_, port)| port.parse::<u16>().is_ok())
        .map_or(address, |(host, _)| host);
    normalize_host(host)
}

fn normalize_host(host: &str) -> String {
    host.trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_lowercase()
}

fn absolute(base: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalize(&path)
}

fn normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            component => output.push(component.as_os_str()),
        }
    }
    output
}

fn physical(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return normalize(path);
        };
        missing.push(name.to_os_string());
        let Some(parent) = existing.parent() else {
            return normalize(path);
        };
        existing = parent;
    }
    let mut result = existing
        .canonicalize()
        .unwrap_or_else(|_| normalize(existing));
    for component in missing.iter().rev() {
        result.push(component);
    }
    normalize(&result)
}

fn error(message: impl AsRef<str>) -> String {
    message.as_ref().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_owned_source_is_trusted_by_default_but_keeps_resource_limits() {
        let directory = tempfile::tempdir().unwrap();
        let permissions = PermissionSet::for_source(directory.path());
        assert!(permissions
            .check_file(&directory.path().join("data.csv"))
            .is_ok());
        assert!(permissions.check_file(Path::new("/etc/passwd")).is_ok());
        assert!(permissions.check_network("https://example.com").is_ok());
        assert!(permissions.check_environment("PATH").is_ok());
        assert!(permissions.check_process().is_ok());
        assert_eq!(
            permissions.buffer_limit(),
            crate::resource::DEFAULT_MAX_BUFFER_BYTES
        );
        assert_eq!(
            permissions.collection_limit(),
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS
        );
        assert_eq!(
            permissions.value_nesting_limit(),
            crate::resource::DEFAULT_MAX_VALUE_NESTING
        );
        assert_eq!(
            permissions.module_depth_limit(),
            crate::resource::DEFAULT_MAX_MODULE_DEPTH
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_permission_resolves_symlink_ancestors() {
        use std::os::unix::fs::symlink;

        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), allowed.path().join("外部链接")).unwrap();
        let permissions = PermissionSet::restricted_for_source(allowed.path());
        assert!(permissions
            .check_file(&allowed.path().join("外部链接/secret.txt"))
            .is_err());
    }

    #[test]
    fn wildcard_network_permissions_respect_domain_boundaries() {
        let mut permissions = PermissionSet::restricted_for_source(Path::new("."));
        permissions.network_hosts = vec!["*.example.com".into()];
        assert!(permissions.check_network("https://api.example.com").is_ok());
        assert!(permissions
            .check_network("https://evil-example.com")
            .is_err());
        assert!(permissions.check_network("https://example.com").is_err());
        assert!(permissions
            .check_network("https://API.EXAMPLE.COM./path")
            .is_ok());

        permissions.network_hosts = vec!["::1".into()];
        assert!(permissions.check_network("[::1]:8080").is_ok());
        assert!(permissions.check_network("::1").is_ok());

        permissions.network_hosts = vec!["example.com".into()];
        assert!(permissions
            .check_network("https://用户:密码@example.com:8443/path")
            .is_ok());
    }

    #[test]
    fn project_only_restricts_capabilities_when_the_user_requests_it() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join(crate::project::MANIFEST);
        std::fs::write(
            &manifest,
            r#"["项目"]
"名称" = "示例"
"版本" = "0.1.0"
"入口" = "主程序.代码"

["权限"]
"最大输出字节" = 100
"#,
        )
        .unwrap();
        let trusted = PermissionSet::for_source(directory.path());
        assert!(trusted.unrestricted);
        assert!(trusted.fixed);
        assert_eq!(trusted.max_output_bytes, Some(100));

        std::fs::write(
            manifest,
            r#"["项目"]
"名称" = "示例"
"版本" = "0.1.0"
"入口" = "主程序.代码"

["权限"]
"限制能力" = true
"#,
        )
        .unwrap();
        let restricted = PermissionSet::for_source(directory.path());
        assert!(!restricted.unrestricted);
        assert!(restricted
            .check_file(&directory.path().join("数据.csv"))
            .is_ok());
        assert!(restricted.check_network("https://example.com").is_err());
        assert!(restricted.check_environment("PATH").is_err());
        assert!(restricted.check_process().is_err());
    }

    #[test]
    fn tool_manifest_is_optional_and_can_only_tighten_permissions() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(crate::project::MANIFEST),
            r#"
["项目"]
"名称" = "示例"
"版本" = "0.1.0"
"入口" = "src/主程序.代码"

["权限"]
"最大输出字节" = 100

["工具"."报表"]
version = 1
name = "报表"
tool_version = "1.0.0"
entry = "src/主程序.代码"

["工具"."报表"."参数"."日期"]
type_name = "日期"
required = true

["工具"."报表"."权限"]
"最大输出字节" = 50
"#,
        )
        .unwrap();
        let permissions = PermissionSet::for_tool(directory.path(), "报表").unwrap();
        assert_eq!(permissions.max_output_bytes, Some(50));
        assert_eq!(
            PermissionSet::for_tool(directory.path(), "不存在")
                .unwrap()
                .max_output_bytes,
            Some(100)
        );
    }
}
