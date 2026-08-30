//! Portable run evidence and input verification for replay.

use crate::execution::{Artifact, ContractVersion, InputObservation, Metadata, ResourceSummary};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const EVIDENCE_VERSION: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ExternalDependencyLevel {
    None,
    FingerprintOnly,
    Replayed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalDependency {
    pub kind: String,
    pub identity: String,
    pub fingerprint: Option<String>,
    pub level: ExternalDependencyLevel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceManifest {
    pub version: ContractVersion,
    pub evidence_version: u32,
    pub run_id: String,
    pub program_digest: String,
    #[serde(default)]
    pub source_path: Option<PathBuf>,
    #[serde(default)]
    pub source_text: Option<String>,
    #[serde(default)]
    pub output: Vec<String>,
    #[serde(default = "evidence_success")]
    pub success: bool,
    #[serde(default)]
    pub permissions: Option<crate::security::PermissionSet>,
    pub bytecode_digest: Option<String>,
    pub parameters: BTreeMap<String, serde_json::Value>,
    pub inputs: Vec<InputObservation>,
    pub artifacts: Vec<Artifact>,
    pub diagnostics: Vec<crate::api::DiagnosticReport>,
    pub external_dependencies: Vec<ExternalDependency>,
    pub resources: ResourceSummary,
    #[serde(default)]
    pub metadata: Metadata,
}

fn evidence_success() -> bool {
    true
}

impl EvidenceManifest {
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("编码凭证失败：{error}"))
    }
    pub fn from_json(text: &str) -> Result<Self, String> {
        let manifest: Self =
            serde_json::from_str(text).map_err(|error| format!("凭证格式无效：{error}"))?;
        if !(1..=EVIDENCE_VERSION).contains(&manifest.evidence_version) {
            return Err(format!("不支持的凭证版本 {}", manifest.evidence_version));
        }
        Ok(manifest)
    }

    pub fn write_file(&self, path: &Path, max_bytes: usize) -> Result<(), String> {
        if path.extension().and_then(|extension| extension.to_str()) != Some("凭证") {
            return Err("凭证文件扩展名必须是 .凭证".into());
        }
        if path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return Err("凭证路径不能包含 ..".into());
        }
        let text = self.to_json()?;
        if text.len() > max_bytes {
            return Err("凭证超过大小预算".into());
        }
        crate::security::atomic_write_file(path, text.as_bytes(), true)
    }

    pub fn write_file_authorized(
        &self,
        path: &Path,
        max_bytes: usize,
        permissions: &crate::security::PermissionSet,
    ) -> Result<(), String> {
        if path.extension().and_then(|extension| extension.to_str()) != Some("凭证") {
            return Err("凭证文件扩展名必须是 .凭证".into());
        }
        if path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return Err("凭证路径不能包含 ..".into());
        }
        let text = self.to_json()?;
        if text.len() > max_bytes {
            return Err("凭证超过大小预算".into());
        }
        permissions.write_file_atomic(
            path,
            text.as_bytes(),
            crate::privacy::Endpoint::LocalFile,
            &self.metadata.sensitivity,
        )
    }

    pub fn read_file(path: &Path, max_bytes: usize) -> Result<Self, String> {
        if path.extension().and_then(|extension| extension.to_str()) != Some("凭证") {
            return Err("凭证文件扩展名必须是 .凭证".into());
        }
        let metadata = std::fs::metadata(path).map_err(|error| format!("读取凭证失败：{error}"))?;
        if metadata.len() > max_bytes as u64 {
            return Err("凭证超过大小预算".into());
        }
        let text =
            std::fs::read_to_string(path).map_err(|error| format!("读取凭证失败：{error}"))?;
        Self::from_json(&text)
    }

    /// 凭证查看视图：未获得 `EvidenceView` 端点与敏感类别双重授权时，
    /// 只保留逻辑名和摘要，参数值和输出文本被替换为占位符。
    pub fn view(&self, policy: &crate::privacy::EndpointPolicy) -> Self {
        let sensitivity = &self.metadata.sensitivity;
        if sensitivity.labels.is_empty() || sensitivity.masked || sensitivity.pseudonymized {
            return self.clone();
        }
        if policy
            .allow(crate::privacy::Endpoint::EvidenceView, sensitivity)
            .is_ok()
        {
            return self.clone();
        }
        let mut redacted = self.clone();
        for value in redacted.parameters.values_mut() {
            *value = serde_json::Value::String("<需要敏感查看权限>".into());
        }
        redacted.output = vec![format!("<{} 行输出需要敏感查看权限>", self.output.len())];
        redacted
    }

    pub fn without_secrets(mut self, secret_names: &std::collections::BTreeSet<String>) -> Self {
        self.parameters
            .retain(|name, _| !secret_names.contains(name));
        self
    }
}

/// 一次运行最多登记的观察条数；超出后只记录溢出计数，不再增长内存。
pub const MAX_OBSERVED_INPUTS: usize = 512;
pub const MAX_OBSERVED_EXTERNALS: usize = 512;
/// 超过该大小的输入不计算摘要，只登记大小和截断标记，避免无界读取。
pub const MAX_FINGERPRINT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
struct Ledger {
    inputs: BTreeMap<String, InputObservation>,
    externals: BTreeMap<(String, String), ExternalDependency>,
    output_paths: BTreeMap<String, PathBuf>,
    truncated: bool,
}

static LEDGERS: std::sync::Mutex<BTreeMap<u64, Ledger>> = std::sync::Mutex::new(BTreeMap::new());
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CURRENT_SESSION: Cell<Option<u64>> = const { Cell::new(None) };
}

struct SessionGuard(Option<u64>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        CURRENT_SESSION.with(|current| current.set(self.0));
    }
}

fn with_ledger(action: impl FnOnce(&mut Ledger)) {
    let Some(session) = CURRENT_SESSION.with(Cell::get) else {
        return;
    };
    if let Ok(mut guard) = LEDGERS.lock() {
        if let Some(ledger) = guard.get_mut(&session) {
            action(ledger);
        }
    }
}

/// 开始登记；只有显式开始后才收集观察，普通运行不为此付出代价。
pub fn start_observing() {
    stop_observing();
    let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut guard) = LEDGERS.lock() {
        guard.insert(session, Ledger::default());
    }
    CURRENT_SESSION.with(|current| current.set(Some(session)));
}

pub fn stop_observing() {
    let session = CURRENT_SESSION.with(|current| current.replace(None));
    if let Some(session) = session {
        if let Ok(mut guard) = LEDGERS.lock() {
            guard.remove(&session);
        }
    }
}

/// 当前运行会话，仅供宿主将观察上下文传入新建的执行线程。
pub(crate) fn current_session() -> Option<u64> {
    CURRENT_SESSION.with(Cell::get)
}

/// 在指定运行会话中执行线程工作，结束后恢复线程原有上下文。
pub(crate) fn with_session<T>(session: Option<u64>, action: impl FnOnce() -> T) -> T {
    CURRENT_SESSION.with(|current| {
        let previous = current.replace(session);
        let _guard = SessionGuard(previous);
        action()
    })
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObservationSnapshot {
    pub inputs: Vec<InputObservation>,
    pub externals: Vec<ExternalDependency>,
    pub output_paths: Vec<PathBuf>,
    pub truncated: bool,
}

pub fn observations() -> ObservationSnapshot {
    let Some(session) = CURRENT_SESSION.with(Cell::get) else {
        return ObservationSnapshot::default();
    };
    let Ok(guard) = LEDGERS.lock() else {
        return ObservationSnapshot::default();
    };
    let Some(ledger) = guard.get(&session) else {
        return ObservationSnapshot::default();
    };
    ObservationSnapshot {
        inputs: ledger.inputs.values().cloned().collect(),
        externals: ledger.externals.values().cloned().collect(),
        output_paths: ledger.output_paths.values().cloned().collect(),
        truncated: ledger.truncated,
    }
}

/// 交付物默认继承本次运行已登记的输入：逐个输入登记来源引用，并合并敏感标签，
/// 这样凭证里的交付物不需要程序显式声明也能追到读过的原始数据。
pub fn observed_metadata() -> crate::execution::Metadata {
    metadata_from_observations(&observations())
}

pub fn metadata_from_observations(snapshot: &ObservationSnapshot) -> crate::execution::Metadata {
    let mut metadata = crate::execution::Metadata::default();
    for input in &snapshot.inputs {
        metadata.provenance.push(crate::execution::ProvenanceRef {
            source_id: input.logical_name.clone(),
            location: Some(input.normalized_path.display().to_string()),
            step_id: None,
        });
        metadata
            .sensitivity
            .labels
            .extend(input.sensitive.labels.iter().cloned());
    }
    metadata
}

pub fn observe_external(kind: &str, identity: &str, level: ExternalDependencyLevel) {
    let key = (kind.to_string(), identity.to_string());
    with_ledger(|ledger| {
        if ledger.externals.contains_key(&key) {
            return;
        }
        if ledger.externals.len() >= MAX_OBSERVED_EXTERNALS {
            ledger.truncated = true;
            return;
        }
        ledger.externals.insert(
            key.clone(),
            ExternalDependency {
                kind: key.0,
                identity: key.1,
                fingerprint: None,
                level,
            },
        );
    });
}

/// 登记一个实际读取的输入文件。文件不存在或超出摘要预算时退化为指纹缺失记录，
/// 不假装可复算。
pub fn observe_input_file(logical_name: &str, path: &Path) {
    let size = std::fs::metadata(path).ok().map(|metadata| metadata.len());
    let observation = match size {
        Some(size) if size <= MAX_FINGERPRINT_BYTES => {
            InputObservation::observe(logical_name, path).ok()
        }
        _ => None,
    };
    with_ledger(|ledger| {
        if ledger.inputs.contains_key(logical_name) {
            return;
        }
        if ledger.inputs.len() >= MAX_OBSERVED_INPUTS {
            ledger.truncated = true;
            return;
        }
        match size {
            Some(size) if size <= MAX_FINGERPRINT_BYTES => {
                if let Some(observation) = observation {
                    ledger.inputs.insert(logical_name.to_string(), observation);
                }
            }
            Some(_) => {
                ledger.truncated = true;
                ledger.externals.insert(
                    ("大文件".to_string(), logical_name.to_string()),
                    ExternalDependency {
                        kind: "大文件".into(),
                        identity: logical_name.to_string(),
                        fingerprint: None,
                        level: ExternalDependencyLevel::FingerprintOnly,
                    },
                );
            }
            None => {}
        }
    });
}

pub fn observe_output_file(path: &Path) {
    let identity = path.display().to_string();
    with_ledger(|ledger| {
        if ledger.output_paths.len() >= MAX_OBSERVED_INPUTS {
            ledger.truncated = true;
            return;
        }
        ledger
            .output_paths
            .entry(identity)
            .or_insert_with(|| path.to_path_buf());
    });
    observe_external(
        "输出文件",
        &path.display().to_string(),
        ExternalDependencyLevel::Replayed,
    );
}

/// 标准输出是可重放的观察：登记内容摘要而不是逐行文本。
pub fn observe_standard_output(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(lines.join("\n").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    with_ledger(|ledger| {
        ledger.externals.insert(
            ("标准输出".to_string(), "程序输出".to_string()),
            ExternalDependency {
                kind: "标准输出".into(),
                identity: "程序输出".into(),
                fingerprint: Some(digest),
                level: ExternalDependencyLevel::Replayed,
            },
        );
    });
}

/// 在权限检查通过、真实 I/O 之前登记标准库边界。参数已由调用方规范化。
pub fn observe_stdlib_boundary(
    module: &str,
    function: &str,
    first: Option<&Path>,
    logical_name: &str,
    identity: &str,
) {
    match module {
        "文件系统" => match function {
            "写入文本" | "写入字节" | "追加文本" | "复制" | "移动" | "删除" | "创建目录" => {
                if let Some(path) = first {
                    observe_output_file(path);
                }
            }
            _ => {
                if let Some(path) = first {
                    observe_input_file(logical_name, path);
                }
            }
        },
        "数据库" => {
            observe_external("数据库", identity, ExternalDependencyLevel::FingerprintOnly)
        }
        "网络" | "网页" => {
            observe_external("网络", identity, ExternalDependencyLevel::FingerprintOnly)
        }
        "环境" => observe_external("环境", identity, ExternalDependencyLevel::FingerprintOnly),
        "进程" => observe_external("子进程", identity, ExternalDependencyLevel::FingerprintOnly),
        "日期时间" if matches!(function, "现在" | "今天") => {
            observe_external("时间", function, ExternalDependencyLevel::FingerprintOnly)
        }
        "密码" if function.starts_with("随机") => {
            observe_external("随机", function, ExternalDependencyLevel::FingerprintOnly)
        }
        _ => {}
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputDifference {
    Missing(String),
    Extra(String),
    Changed(String),
    PathEscape(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RunDifference {
    Success,
    Termination,
    Output,
    Diagnostics,
    Artifacts,
    Resources,
    Observations,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunComparison {
    pub differences: Vec<RunDifference>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunDifferenceView {
    pub kind: RunDifference,
    pub expected: String,
    pub actual: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunComparisonView {
    pub version: u32,
    pub matches: bool,
    pub differences: Vec<RunDifferenceView>,
}

impl RunComparisonView {
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| error.to_string())
    }

    pub fn to_html(&self) -> String {
        let rows = self
            .differences
            .iter()
            .map(|difference| {
                format!(
                    "<tr><th>{:?}</th><td>{}</td><td>{}</td></tr>",
                    difference.kind,
                    escape_html(&difference.expected),
                    escape_html(&difference.actual)
                )
            })
            .collect::<String>();
        format!(
            "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><title>衡枢复算差异</title><style>body{{font:14px system-ui;margin:24px;color:#202428}}table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #b7bec5;padding:8px;text-align:left;vertical-align:top}}th{{background:#eef1f3}}</style><h1>衡枢复算差异</h1><p>一致：{}</p><table><thead><tr><th>类别</th><th>预期摘要</th><th>实际摘要</th></tr></thead><tbody>{rows}</tbody></table></html>",
            if self.matches { "是" } else { "否" }
        )
    }
}

impl RunComparison {
    pub fn matches(&self) -> bool {
        self.differences.is_empty()
    }
}

pub fn compare_run(
    expected: &crate::execution::RunOutcome,
    actual: &crate::execution::RunOutcome,
    expected_dependencies: &[ExternalDependency],
    actual_dependencies: &[ExternalDependency],
) -> RunComparison {
    let mut differences = Vec::new();
    if expected.success != actual.success {
        differences.push(RunDifference::Success);
    }
    if expected.termination != actual.termination {
        differences.push(RunDifference::Termination);
    }
    if expected.output != actual.output {
        differences.push(RunDifference::Output);
    }
    if expected.diagnostics != actual.diagnostics {
        differences.push(RunDifference::Diagnostics);
    }
    if comparable_artifacts(&expected.artifacts) != comparable_artifacts(&actual.artifacts) {
        differences.push(RunDifference::Artifacts);
    }
    let expected_resources = (
        expected.resources.output_bytes,
        expected.resources.observation_bytes,
        expected.resources.peak_events,
    );
    let actual_resources = (
        actual.resources.output_bytes,
        actual.resources.observation_bytes,
        actual.resources.peak_events,
    );
    if expected_resources != actual_resources {
        differences.push(RunDifference::Resources);
    }
    if comparable_dependencies(expected_dependencies)
        != comparable_dependencies(actual_dependencies)
        || comparable_event_shapes(&expected.events) != comparable_event_shapes(&actual.events)
    {
        differences.push(RunDifference::Observations);
    }
    RunComparison { differences }
}

pub fn redacted_comparison_view(
    expected: &crate::execution::RunOutcome,
    actual: &crate::execution::RunOutcome,
    expected_dependencies: &[ExternalDependency],
    actual_dependencies: &[ExternalDependency],
) -> RunComparisonView {
    let comparison = compare_run(expected, actual, expected_dependencies, actual_dependencies);
    let differences = comparison
        .differences
        .iter()
        .cloned()
        .map(|kind| {
            let (expected_summary, actual_summary) = match kind {
                RunDifference::Success => (
                    format!(
                        "success={}, termination={:?}",
                        expected.success, expected.termination
                    ),
                    format!(
                        "success={}, termination={:?}",
                        actual.success, actual.termination
                    ),
                ),
                RunDifference::Termination => (
                    format!("termination={:?}", expected.termination),
                    format!("termination={:?}", actual.termination),
                ),
                RunDifference::Output => (
                    output_summary(&expected.output),
                    output_summary(&actual.output),
                ),
                RunDifference::Diagnostics => (
                    diagnostic_summary(&expected.diagnostics),
                    diagnostic_summary(&actual.diagnostics),
                ),
                RunDifference::Artifacts => (
                    artifact_summary(&expected.artifacts),
                    artifact_summary(&actual.artifacts),
                ),
                RunDifference::Resources => (
                    resource_summary(&expected.resources),
                    resource_summary(&actual.resources),
                ),
                RunDifference::Observations => (
                    observation_summary(&expected.events, expected_dependencies),
                    observation_summary(&actual.events, actual_dependencies),
                ),
            };
            RunDifferenceView {
                kind,
                expected: expected_summary,
                actual: actual_summary,
            }
        })
        .collect();
    RunComparisonView {
        version: crate::execution::CONTRACT_VERSION,
        matches: comparison.matches(),
        differences,
    }
}

fn output_summary(output: &[String]) -> String {
    format!(
        "lines={}, bytes={}",
        output.len(),
        output.iter().map(String::len).sum::<usize>()
    )
}

fn diagnostic_summary(diagnostics: &[crate::api::DiagnosticReport]) -> String {
    let mut categories = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.category.clone())
        .collect::<Vec<_>>();
    categories.sort();
    categories.dedup();
    format!(
        "count={}, categories={}",
        diagnostics.len(),
        categories.join("|")
    )
}

fn artifact_summary(artifacts: &[Artifact]) -> String {
    let mut kinds = artifacts
        .iter()
        .map(|artifact| artifact.kind.clone())
        .collect::<Vec<_>>();
    kinds.sort();
    kinds.dedup();
    format!(
        "count={}, bytes={}, kinds={}",
        artifacts.len(),
        artifacts.iter().map(|artifact| artifact.bytes).sum::<u64>(),
        kinds.join("|")
    )
}

fn resource_summary(resources: &crate::execution::ResourceSummary) -> String {
    format!(
        "output_bytes={}, observation_bytes={}, peak_events={}",
        resources.output_bytes, resources.observation_bytes, resources.peak_events
    )
}

fn observation_summary(
    events: &[crate::execution::ExecutionEvent],
    dependencies: &[ExternalDependency],
) -> String {
    let mut stages = events
        .iter()
        .map(|event| event.stage.clone())
        .collect::<Vec<_>>();
    stages.sort();
    stages.dedup();
    let mut kinds = dependencies
        .iter()
        .map(|dependency| dependency.kind.clone())
        .collect::<Vec<_>>();
    kinds.sort();
    kinds.dedup();
    format!(
        "events={}, stages={}, dependencies={}, kinds={}",
        events.len(),
        stages.join("|"),
        dependencies.len(),
        kinds.join("|")
    )
}

fn comparable_event_shapes(
    events: &[crate::execution::ExecutionEvent],
) -> Vec<(
    u64,
    Option<crate::diagnostic::SourceSpan>,
    String,
    usize,
    usize,
)> {
    events
        .iter()
        .map(|event| {
            (
                event.step,
                event.source_span,
                event.stage.clone(),
                event.artifacts.len(),
                event.warnings.len(),
            )
        })
        .collect()
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn comparable_artifacts(artifacts: &[Artifact]) -> Vec<(String, String, u64, String)> {
    let mut values = artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.path.to_string_lossy().into_owned(),
                artifact.kind.clone(),
                artifact.bytes,
                artifact.sha256.clone(),
            )
        })
        .collect::<Vec<_>>();
    values.sort();
    values
}

fn comparable_dependencies(
    dependencies: &[ExternalDependency],
) -> Vec<(String, String, Option<String>, ExternalDependencyLevel)> {
    let mut values = dependencies
        .iter()
        .filter(|dependency| dependency.level != ExternalDependencyLevel::None)
        .filter(|dependency| dependency.kind != "输出文件")
        .map(|dependency| {
            (
                dependency.kind.clone(),
                dependency.identity.clone(),
                dependency.fingerprint.clone(),
                dependency.level.clone(),
            )
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));
    values
}

pub fn compare_inputs(
    expected: &[InputObservation],
    actual: &[InputObservation],
) -> Vec<InputDifference> {
    let expected = expected
        .iter()
        .map(|item| (&item.logical_name, item))
        .collect::<BTreeMap<_, _>>();
    let actual = actual
        .iter()
        .map(|item| (&item.logical_name, item))
        .collect::<BTreeMap<_, _>>();
    let mut differences = Vec::new();
    for name in expected.keys() {
        match actual.get(name) {
            None => differences.push(InputDifference::Missing((*name).clone())),
            Some(observation)
                if expected[name].sha256 != observation.sha256
                    || expected[name].bytes != observation.bytes
                    || expected[name].structure_version != observation.structure_version =>
            {
                differences.push(InputDifference::Changed((*name).clone()))
            }
            Some(_) => {}
        }
    }
    for name in actual.keys() {
        if !expected.contains_key(name) {
            differences.push(InputDifference::Extra((*name).clone()));
        }
    }
    differences
}

pub fn verify_input_root(
    root: &Path,
    expected: &[InputObservation],
    mapping: &BTreeMap<String, PathBuf>,
) -> Result<Vec<InputObservation>, String> {
    let mut actual = Vec::with_capacity(mapping.len());
    for (logical_name, relative) in mapping {
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(format!("复算输入路径不安全：{logical_name}"));
        }
        let path = root.join(relative);
        actual.push(InputObservation::observe(logical_name, path)?);
    }
    let differences = compare_inputs(expected, &actual);
    if differences.is_empty() {
        Ok(actual)
    } else {
        Err(format!("复算输入指纹不一致：{differences:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Barrier};

    #[test]
    fn evidence_rejects_wrong_version_and_detects_one_byte_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.txt");
        fs::write(&path, b"a").unwrap();
        let expected = InputObservation::observe("data", &path).unwrap();
        fs::write(&path, b"b").unwrap();
        let actual = InputObservation::observe("data", &path).unwrap();
        assert_eq!(
            compare_inputs(&[expected], &[actual]),
            vec![InputDifference::Changed("data".into())]
        );
        let manifest = EvidenceManifest {
            version: Default::default(),
            evidence_version: EVIDENCE_VERSION,
            run_id: "r".into(),
            program_digest: "x".into(),
            source_path: None,
            source_text: None,
            output: Vec::new(),
            success: true,
            permissions: None,
            bytecode_digest: None,
            parameters: BTreeMap::new(),
            inputs: Vec::new(),
            artifacts: Vec::new(),
            diagnostics: Vec::new(),
            external_dependencies: Vec::new(),
            resources: ResourceSummary::default(),
            metadata: Metadata::default(),
        };
        let text = manifest
            .to_json()
            .unwrap()
            .replace("\"evidence_version\": 2", "\"evidence_version\": 3");
        assert!(EvidenceManifest::from_json(&text).is_err());
    }

    #[test]
    fn run_comparison_catches_diagnostics_artifacts_resources_and_observations() {
        let artifact = Artifact::from_bytes("result/report.csv", "text/csv", b"a,b\n1,2\n");
        let expected = crate::execution::RunOutcome {
            output: vec!["完成".into()],
            artifacts: vec![artifact],
            resources: ResourceSummary {
                output_bytes: "完成".len(),
                observation_bytes: 8,
                ..ResourceSummary::default()
            },
            ..crate::execution::RunOutcome::default()
        };
        let dependency = ExternalDependency {
            kind: "标准输出".into(),
            identity: "程序输出".into(),
            fingerprint: Some("abc".into()),
            level: ExternalDependencyLevel::Replayed,
        };
        assert!(compare_run(
            &expected,
            &expected,
            std::slice::from_ref(&dependency),
            std::slice::from_ref(&dependency),
        )
        .matches());

        let mut actual = expected.clone();
        actual.diagnostics.push(crate::api::DiagnosticReport {
            category: Some("运行时".into()),
            span: None,
            message: "失败".into(),
            stack: Vec::new(),
            definition_span: None,
            cause: None,
        });
        actual.artifacts[0].sha256 = "changed".into();
        actual.resources.observation_bytes += 1;
        let changed_dependency = ExternalDependency {
            fingerprint: Some("changed".into()),
            ..dependency.clone()
        };
        assert_eq!(
            compare_run(&expected, &actual, &[dependency], &[changed_dependency]).differences,
            vec![
                RunDifference::Diagnostics,
                RunDifference::Artifacts,
                RunDifference::Resources,
                RunDifference::Observations,
            ]
        );
    }

    #[test]
    fn run_comparison_distinguishes_cancelled_timeout_and_failed_runs() {
        let expected = crate::execution::RunOutcome {
            success: false,
            termination: crate::execution::RunTermination::Cancelled,
            ..Default::default()
        };
        let actual = crate::execution::RunOutcome {
            success: false,
            termination: crate::execution::RunTermination::TimedOut,
            ..Default::default()
        };
        let comparison = compare_run(&expected, &actual, &[], &[]);
        assert_eq!(comparison.differences, vec![RunDifference::Termination]);
        let view = redacted_comparison_view(&expected, &actual, &[], &[]);
        assert!(view.to_json().unwrap().contains("Termination"));
    }

    #[test]
    fn the_observation_ledger_deduplicates_bounds_and_only_collects_when_started() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("台账.csv");
        fs::write(&path, b"a,b\n").unwrap();

        stop_observing();
        observe_input_file("台账.csv", &path);
        observe_external("时间", "现在", ExternalDependencyLevel::FingerprintOnly);
        assert_eq!(observations(), ObservationSnapshot::default());

        start_observing();
        observe_input_file("台账.csv", &path);
        observe_input_file("台账.csv", &path);
        observe_stdlib_boundary("日期时间", "现在", None, "", "");
        observe_stdlib_boundary("进程", "运行", None, "", "/bin/ls");
        observe_stdlib_boundary("日期时间", "格式化", None, "", "");
        observe_standard_output(&["第一行".to_string()]);
        let snapshot = observations();
        assert_eq!(snapshot.inputs.len(), 1);
        assert_eq!(snapshot.inputs[0].logical_name, "台账.csv");
        let kinds = snapshot
            .externals
            .iter()
            .map(|dependency| dependency.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["子进程", "时间", "标准输出"]);
        assert!(snapshot
            .externals
            .iter()
            .any(|dependency| dependency.kind == "标准输出"
                && dependency.level == ExternalDependencyLevel::Replayed
                && dependency.fingerprint.is_some()));
        assert!(!snapshot.truncated);

        for index in 0..MAX_OBSERVED_EXTERNALS + 5 {
            observe_external(
                "网络",
                &format!("host-{index}"),
                ExternalDependencyLevel::FingerprintOnly,
            );
        }
        let bounded = observations();
        assert_eq!(bounded.externals.len(), MAX_OBSERVED_EXTERNALS);
        assert!(bounded.truncated);
        stop_observing();
    }

    #[test]
    fn concurrent_observation_sessions_do_not_mix_and_child_threads_can_join() {
        let barrier = Arc::new(Barrier::new(2));
        let handles = ["运行甲", "运行乙"].map(|identity| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                start_observing();
                let session = current_session();
                barrier.wait();
                let child = std::thread::spawn(move || {
                    with_session(session, || {
                        observe_external("并发运行", identity, ExternalDependencyLevel::Replayed);
                    });
                });
                child.join().unwrap();
                let snapshot = observations();
                stop_observing();
                snapshot
            })
        });
        let snapshots = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(snapshots[0].externals.len(), 1);
        assert_eq!(snapshots[1].externals.len(), 1);
        assert_ne!(
            snapshots[0].externals[0].identity,
            snapshots[1].externals[0].identity
        );
    }

    #[test]
    fn replay_requires_relative_declared_paths() {
        let directory = tempfile::tempdir().unwrap();
        let expected = Vec::new();
        let mapping = BTreeMap::from([("x".into(), PathBuf::from("../secret"))]);
        assert!(verify_input_root(directory.path(), &expected, &mapping).is_err());
    }

    #[test]
    fn evidence_file_round_trip_has_extension_size_and_secret_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run.凭证");
        let mut manifest = EvidenceManifest {
            version: Default::default(),
            evidence_version: EVIDENCE_VERSION,
            run_id: "r".into(),
            program_digest: "x".into(),
            source_path: None,
            source_text: None,
            output: Vec::new(),
            success: true,
            permissions: None,
            bytecode_digest: None,
            parameters: BTreeMap::from([
                ("密码".into(), serde_json::Value::String("secret".into())),
                (
                    "日期".into(),
                    serde_json::Value::String("2026-01-01".into()),
                ),
            ]),
            inputs: Vec::new(),
            artifacts: Vec::new(),
            diagnostics: Vec::new(),
            external_dependencies: Vec::new(),
            resources: ResourceSummary::default(),
            metadata: Metadata::default(),
        };
        manifest = manifest.without_secrets(&std::collections::BTreeSet::from(["密码".into()]));
        manifest.write_file(&path, 1024 * 1024).unwrap();
        let restored = EvidenceManifest::read_file(&path, 1024 * 1024).unwrap();
        assert!(!restored.parameters.contains_key("密码"));
        assert!(EvidenceManifest::read_file(&directory.path().join("run.json"), 1024).is_err());
    }

    #[test]
    fn evidence_round_trip_preserves_unit_metadata() {
        let manifest = EvidenceManifest {
            version: Default::default(),
            evidence_version: EVIDENCE_VERSION,
            run_id: "r".into(),
            program_digest: "x".into(),
            source_path: None,
            source_text: None,
            output: Vec::new(),
            success: true,
            permissions: None,
            bytecode_digest: None,
            parameters: BTreeMap::new(),
            inputs: Vec::new(),
            artifacts: Vec::new(),
            diagnostics: Vec::new(),
            external_dependencies: Vec::new(),
            resources: ResourceSummary::default(),
            metadata: Metadata {
                unit: Some(crate::execution::UnitIdentity {
                    name: "万元".into(),
                    dimension: "货币".into(),
                    scale_numerator: 10000,
                    scale_denominator: 1,
                    basis: Some("CNY".into()),
                }),
                ..Default::default()
            },
        };
        let restored = EvidenceManifest::from_json(&manifest.to_json().unwrap()).unwrap();
        assert_eq!(restored.metadata, manifest.metadata);
        let legacy = manifest
            .to_json()
            .unwrap()
            .replace("\"evidence_version\": 2", "\"evidence_version\": 1");
        assert_eq!(
            EvidenceManifest::from_json(&legacy)
                .unwrap()
                .evidence_version,
            1
        );
    }

    #[test]
    fn observed_inputs_flow_into_artifact_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("流水.csv");
        fs::write(&input, "编号,金额\n1,10\n").unwrap();
        let mut observation = InputObservation::observe("流水.csv", &input).unwrap();
        observation
            .sensitive
            .labels
            .insert(crate::execution::SensitiveLabel::Financial);
        let metadata = metadata_from_observations(&ObservationSnapshot {
            inputs: vec![observation],
            externals: Vec::new(),
            output_paths: Vec::new(),
            truncated: false,
        });
        assert_eq!(metadata.provenance.len(), 1);
        assert_eq!(metadata.provenance[0].source_id, "流水.csv");
        assert!(metadata.provenance[0]
            .location
            .as_deref()
            .is_some_and(|location| location.contains("流水.csv")));
        assert!(metadata
            .sensitivity
            .labels
            .contains(&crate::execution::SensitiveLabel::Financial));
        assert_eq!(
            metadata_from_observations(&ObservationSnapshot::default()),
            Metadata::default()
        );
    }
}
