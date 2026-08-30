//! Shared execution contracts used by source, packaged, GUI and CLI runners.
//!
//! The contracts deliberately contain only portable data. Runtime handles such as
//! cancellation tokens are supplied by the caller and are not serialized into a
//! request or an evidence record.

use crate::api::DiagnosticReport;
use crate::security::PermissionSet;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const CONTRACT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractVersion(pub u32);

impl Default for ContractVersion {
    fn default() -> Self {
        Self(CONTRACT_VERSION)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TriggerSource {
    Interactive,
    Gui,
    Cli,
    Schedule,
    FileWatch,
    Replay,
    Embedded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ObservationMode {
    #[default]
    Off,
    Summary,
    Snapshots,
    Detailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservationBudget {
    pub max_events: usize,
    pub max_bytes: usize,
    pub max_table_sample_rows: usize,
    pub max_time_ms: u64,
}

impl Default for ObservationBudget {
    fn default() -> Self {
        Self {
            max_events: 10_000,
            max_bytes: 8 * 1024 * 1024,
            max_table_sample_rows: 20,
            max_time_ms: 30_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InvocationRequest {
    pub version: ContractVersion,
    pub program: String,
    pub tool_version: String,
    pub parameters: BTreeMap<String, JsonValue>,
    pub trigger: TriggerSource,
    pub run_id: String,
    pub permissions: PermissionSet,
    pub output_root: PathBuf,
    pub observation: ObservationMode,
    pub observation_budget: ObservationBudget,
}

impl InvocationRequest {
    pub fn new(
        program: impl Into<String>,
        run_id: impl Into<String>,
        output_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            version: ContractVersion::default(),
            program: program.into(),
            tool_version: env!("CARGO_PKG_VERSION").into(),
            parameters: BTreeMap::new(),
            trigger: TriggerSource::Interactive,
            run_id: run_id.into(),
            permissions: PermissionSet::for_source(Path::new(".")),
            output_root: output_root.into(),
            observation: ObservationMode::default(),
            observation_budget: ObservationBudget::default(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version.0 != CONTRACT_VERSION {
            return Err(format!("执行契约版本不支持：{}", self.version.0));
        }
        if self.program.trim().is_empty() || self.tool_version.trim().is_empty() {
            return Err("执行契约缺少程序或工具版本".into());
        }
        if self.run_id.trim().is_empty() {
            return Err("执行契约缺少运行 ID".into());
        }
        if self.output_root.as_os_str().is_empty() {
            return Err("执行契约缺少输出根目录".into());
        }
        Ok(())
    }

    pub fn encode_program_args(&self) -> Result<Vec<String>, String> {
        self.parameters
            .iter()
            .map(|(name, value)| {
                if name.trim().is_empty() {
                    return Err("执行参数名称不能为空".into());
                }
                let encoded = serde_json::to_string(value)
                    .map_err(|error| format!("执行参数编码失败：{error}"))?;
                Ok(format!("{name}={encoded}"))
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceSummary {
    pub elapsed_ms: u64,
    pub output_bytes: usize,
    pub observation_bytes: usize,
    pub peak_events: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub version: ContractVersion,
    pub path: PathBuf,
    pub kind: String,
    pub display_name: String,
    pub purpose: String,
    pub bytes: u64,
    pub sha256: String,
    pub metadata: Metadata,
    pub step_id: Option<u64>,
}

impl Artifact {
    pub fn from_bytes(path: impl Into<PathBuf>, kind: impl Into<String>, bytes: &[u8]) -> Self {
        let path = path.into();
        let display_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        Self {
            version: ContractVersion::default(),
            path,
            kind: kind.into(),
            display_name,
            purpose: String::new(),
            bytes: bytes.len() as u64,
            sha256: hex_digest(bytes),
            metadata: Metadata::default(),
            step_id: None,
        }
    }

    pub fn observe_file(path: impl Into<PathBuf>, kind: impl Into<String>) -> Result<Self, String> {
        let path = path.into();
        let mut file = fs::File::open(&path).map_err(|error| format!("读取交付物失败：{error}"))?;
        let bytes = file
            .metadata()
            .map_err(|error| format!("读取交付物元数据失败：{error}"))?
            .len();
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            use std::io::Read;
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("读取交付物失败：{error}"))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        let display_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        Ok(Self {
            version: ContractVersion::default(),
            path,
            kind: kind.into(),
            display_name,
            purpose: String::new(),
            bytes,
            sha256: digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            metadata: Metadata::default(),
            step_id: None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputObservation {
    pub version: ContractVersion,
    pub logical_name: String,
    pub normalized_path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub modified_unix_ms: Option<i128>,
    pub structure_version: Option<String>,
    pub sensitive: Sensitivity,
}

impl InputObservation {
    pub fn observe(
        logical_name: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let path = path.into();
        let metadata = fs::metadata(&path).map_err(|error| format!("读取输入失败：{error}"))?;
        const MAX_OBSERVED_BYTES: u64 = 64 * 1024 * 1024;
        if metadata.len() > MAX_OBSERVED_BYTES {
            return Err("输入超过观察摘要字节预算".into());
        }
        let bytes = fs::read(&path).map_err(|error| format!("读取输入失败：{error}"))?;
        let modified_unix_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i128);
        Ok(Self {
            version: ContractVersion::default(),
            logical_name: logical_name.into(),
            normalized_path: lexical_normalize(&path),
            sha256: hex_digest(&bytes),
            bytes: bytes.len() as u64,
            modified_unix_ms,
            structure_version: Some(structure_fingerprint(&path, &bytes)),
            sensitive: Sensitivity::default(),
        })
    }
}

fn structure_fingerprint(path: &Path, bytes: &[u8]) -> String {
    // Structure is intentionally separate from the full content digest: CSV
    // headers and OOXML part names remain comparable even when cell values
    // change.
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let shape = if extension.eq_ignore_ascii_case("csv") {
        bytes
            .split(|byte| *byte == b'\n' || *byte == b'\r')
            .next()
            .unwrap_or_default()
            .to_vec()
    } else if extension.eq_ignore_ascii_case("xlsx") || extension.eq_ignore_ascii_case("docx") {
        let mut names = Vec::new();
        let mut markers = Vec::new();
        if let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
            if crate::ooxml::validate_archive(
                &mut archive,
                crate::ooxml::OoxmlBudget {
                    max_entries: 10_000,
                    max_entry_bytes: 64 * 1024 * 1024,
                    max_total_bytes: 512 * 1024 * 1024,
                },
            )
            .is_err()
            {
                return hex_digest(b"OOXML structure exceeds budget");
            }
            for index in 0..archive.len().min(10_000) {
                if let Ok(mut entry) = archive.by_index(index) {
                    let name = entry.name().to_string();
                    names.push(name.clone());
                    if extension.eq_ignore_ascii_case("xlsx")
                        && (name == "xl/workbook.xml" || name.starts_with("xl/worksheets/"))
                    {
                        if let Ok(xml) =
                            crate::ooxml::read_bytes_limited(&mut entry, &name, 4 * 1024 * 1024)
                        {
                            let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
                            let mut buffer = Vec::new();
                            while let Ok(event) = reader.read_event_into(&mut buffer) {
                                if matches!(event, quick_xml::events::Event::Eof) {
                                    break;
                                }
                                if let quick_xml::events::Event::Start(start)
                                | quick_xml::events::Event::Empty(start) = event
                                {
                                    let tag =
                                        String::from_utf8_lossy(start.name().as_ref()).to_string();
                                    if matches!(tag.as_str(), "sheet" | "mergeCell" | "row" | "col")
                                    {
                                        let mut attrs = start
                                            .attributes()
                                            .filter_map(Result::ok)
                                            .map(|attribute| {
                                                format!(
                                                    "{}={}",
                                                    String::from_utf8_lossy(attribute.key.as_ref()),
                                                    String::from_utf8_lossy(&attribute.value)
                                                )
                                            })
                                            .collect::<Vec<_>>();
                                        attrs.sort();
                                        markers.push(format!("{tag}:{}", attrs.join(",")));
                                    }
                                }
                                buffer.clear();
                            }
                        }
                    }
                }
            }
        }
        names.sort();
        markers.sort();
        format!("{}\n--\n{}", names.join("\n"), markers.join("\n")).into_bytes()
    } else {
        extension.as_bytes().to_vec()
    };
    let mut hasher = Sha256::new();
    hasher.update(extension.as_bytes());
    hasher.update([0]);
    hasher.update(shape);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub version: ContractVersion,
    pub step: u64,
    pub source_span: Option<crate::diagnostic::SourceSpan>,
    pub stage: String,
    pub description: String,
    pub inputs: BTreeMap<String, ValueSummary>,
    pub outputs: BTreeMap<String, ValueSummary>,
    pub artifacts: Vec<String>,
    pub warnings: Vec<String>,
    pub provenance: Vec<ProvenanceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValueSummary {
    pub type_name: String,
    pub length: Option<usize>,
    pub sample: Option<String>,
    pub truncated: bool,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub version: ContractVersion,
    pub success: bool,
    #[serde(default)]
    pub termination: RunTermination,
    pub output: Vec<String>,
    pub diagnostics: Vec<DiagnosticReport>,
    pub artifacts: Vec<Artifact>,
    pub events: Vec<ExecutionEvent>,
    pub notification_status: Vec<String>,
    pub resources: ResourceSummary,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum RunTermination {
    #[default]
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl Default for RunOutcome {
    fn default() -> Self {
        Self {
            version: ContractVersion::default(),
            success: true,
            termination: RunTermination::Completed,
            output: Vec::new(),
            diagnostics: Vec::new(),
            artifacts: Vec::new(),
            events: Vec::new(),
            notification_status: Vec::new(),
            resources: ResourceSummary::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PathContext {
    pub version: ContractVersion,
    pub program_root: PathBuf,
    pub working_dir: PathBuf,
    pub input_root: PathBuf,
    pub output_root: PathBuf,
    pub temp_root: PathBuf,
    pub permission_root: PathBuf,
}

impl PathContext {
    pub fn new(
        program_root: impl Into<PathBuf>,
        working_dir: impl Into<PathBuf>,
        input_root: impl Into<PathBuf>,
        output_root: impl Into<PathBuf>,
        temp_root: impl Into<PathBuf>,
        permission_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            version: ContractVersion::default(),
            program_root: lexical_normalize(&program_root.into()),
            working_dir: lexical_normalize(&working_dir.into()),
            input_root: lexical_normalize(&input_root.into()),
            output_root: lexical_normalize(&output_root.into()),
            temp_root: lexical_normalize(&temp_root.into()),
            permission_root: lexical_normalize(&permission_root.into()),
        }
    }

    pub fn source(source: impl AsRef<Path>, output_root: impl Into<PathBuf>) -> Self {
        let source = source.as_ref();
        let root = source.parent().unwrap_or_else(|| Path::new("."));
        let root = lexical_normalize(root);
        Self::new(
            &root,
            &root,
            &root,
            output_root.into(),
            root.join(".hengshu-tmp"),
            &root,
        )
    }

    pub fn resolve_input(&self, relative: impl AsRef<Path>) -> Result<PathBuf, String> {
        resolve_beneath(&self.input_root, relative.as_ref())
    }
    pub fn resolve_output(&self, relative: impl AsRef<Path>) -> Result<PathBuf, String> {
        resolve_beneath(&self.output_root, relative.as_ref())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Sensitivity {
    pub labels: BTreeSet<SensitiveLabel>,
    pub masked: bool,
    pub pseudonymized: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum SensitiveLabel {
    Identity,
    Contact,
    Financial,
    Medical,
    Education,
    Custom(String),
}

impl Sensitivity {
    pub fn union(&self, other: &Self) -> Self {
        let mut labels = self.labels.clone();
        labels.extend(other.labels.iter().cloned());
        Self {
            labels,
            masked: self.masked && other.masked,
            pseudonymized: self.pseudonymized && other.pseudonymized,
        }
    }
    pub fn mask(mut self) -> Self {
        self.masked = true;
        self
    }
    pub fn pseudonymize(mut self) -> Self {
        self.pseudonymized = true;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnitIdentity {
    pub name: String,
    pub dimension: String,
    pub scale_numerator: i64,
    pub scale_denominator: i64,
    pub basis: Option<String>,
}

impl UnitIdentity {
    pub fn from_decimal_scale(
        name: impl Into<String>,
        dimension: impl Into<String>,
        scale: Decimal,
        basis: Option<String>,
    ) -> Result<Self, String> {
        if scale.is_zero() {
            return Err("单位比例必须非零".into());
        }
        let denominator = 10i64.checked_pow(scale.scale()).ok_or("单位比例精度过大")?;
        let numerator = i64::try_from(scale.mantissa()).map_err(|_| "单位比例超出可移植范围")?;
        let divisor = gcd_i64(numerator, denominator);
        Ok(Self {
            name: name.into(),
            dimension: dimension.into(),
            scale_numerator: numerator / divisor,
            scale_denominator: denominator / divisor,
            basis,
        })
    }

    pub fn compatible_with(&self, other: &Self) -> bool {
        self.dimension == other.dimension
            && self.scale_numerator.saturating_mul(other.scale_denominator)
                == other.scale_numerator.saturating_mul(self.scale_denominator)
            && self.basis == other.basis
    }
}

fn gcd_i64(mut left: i64, mut right: i64) -> i64 {
    left = left.abs();
    right = right.abs();
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceRef {
    pub source_id: String,
    pub location: Option<String>,
    pub step_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub sensitivity: Sensitivity,
    pub unit: Option<UnitIdentity>,
    #[serde(default)]
    pub units: BTreeMap<String, UnitIdentity>,
    pub provenance: Vec<ProvenanceRef>,
}

impl Metadata {
    pub fn try_union(&self, other: &Self) -> Result<Self, String> {
        if let (Some(left), Some(right)) = (&self.unit, &other.unit) {
            if left != right {
                return Err(format!(
                    "单位/口径元数据不兼容：{} -> {}",
                    left.name, right.name
                ));
            }
        }
        for (field, right) in &other.units {
            if self.units.get(field).is_some_and(|left| left != right) {
                return Err(format!("字段“{field}”的单位/口径元数据不兼容"));
            }
        }
        Ok(self.union(other))
    }

    pub fn union(&self, other: &Self) -> Self {
        let mut provenance = self.provenance.clone();
        for reference in &other.provenance {
            if !provenance.contains(reference) {
                provenance.push(reference.clone());
            }
        }
        Self {
            sensitivity: self.sensitivity.union(&other.sensitivity),
            unit: self.unit.clone().or_else(|| other.unit.clone()),
            units: {
                let mut units = self.units.clone();
                for (field, unit) in &other.units {
                    units.entry(field.clone()).or_insert_with(|| unit.clone());
                }
                units
            },
            provenance,
        }
    }
    pub fn aggregate<I>(items: I) -> Self
    where
        I: IntoIterator<Item = Self>,
    {
        items
            .into_iter()
            .fold(Self::default(), |left, right| left.union(&right))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolManifest {
    pub version: ContractVersion,
    #[serde(alias = "名称")]
    pub name: String,
    #[serde(alias = "工具版本", alias = "版本")]
    pub tool_version: String,
    #[serde(alias = "入口")]
    pub entry: String,
    #[serde(default, alias = "参数")]
    pub parameters: BTreeMap<String, ParameterSpec>,
    pub gui: Option<GuiSpec>,
    pub cli: Option<CliSpec>,
    #[serde(default, alias = "权限")]
    pub permissions: ToolPermissionSpec,
}

impl ToolManifest {
    /// Parse command-line or GUI text fields according to the declared type
    /// before applying the normal schema validation.
    pub fn parse_text_parameters(
        &self,
        parameters: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, JsonValue>, String> {
        let mut parsed = BTreeMap::new();
        for (name, value) in parameters {
            let spec = self
                .parameters
                .get(name)
                .ok_or_else(|| format!("工具没有参数“{name}”"))?;
            let json = match spec.type_name.trim() {
                "整数" | "integer" | "int" => value
                    .parse::<i64>()
                    .map(JsonValue::from)
                    .map_err(|_| format!("参数“{name}”必须是整数"))?,
                "小数" | "数字" | "number" | "decimal" => {
                    let number = value
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .ok_or_else(|| format!("参数“{name}”必须是数字"))?;
                    JsonValue::Number(number)
                }
                "逻辑" | "布尔" | "bool" | "boolean" => match value.trim() {
                    "是" => JsonValue::Bool(true),
                    "否" => JsonValue::Bool(false),
                    other => other
                        .parse::<bool>()
                        .map(JsonValue::from)
                        .map_err(|_| format!("参数“{name}”必须是逻辑值"))?,
                },
                _ => JsonValue::String(value.clone()),
            };
            parsed.insert(name.clone(), json);
        }
        Ok(parsed)
    }

    /// Convert validated tool values into the portable program-argument form.
    /// The language already exposes `环境.程序参数`; using one canonical
    /// encoding keeps GUI, CLI and embedded packages behaviorally identical.
    pub fn encode_program_args(
        &self,
        parameters: &BTreeMap<String, JsonValue>,
    ) -> Result<Vec<String>, String> {
        let parameters = self.validate_parameters(parameters)?;
        parameters
            .into_iter()
            .map(|(name, value)| {
                let encoded = serde_json::to_string(&value)
                    .map_err(|error| format!("参数“{name}”编码失败：{error}"))?;
                Ok(format!("{name}={encoded}"))
            })
            .collect()
    }

    /// Validate and coerce a parameter map before execution. The returned map
    /// is deterministic and contains no undeclared keys; secret values are
    /// validated like ordinary values but are never included in diagnostics.
    pub fn validate_parameters(
        &self,
        parameters: &BTreeMap<String, JsonValue>,
    ) -> Result<BTreeMap<String, JsonValue>, String> {
        for name in parameters.keys() {
            if !self.parameters.contains_key(name) {
                return Err(format!("工具没有参数“{name}”"));
            }
        }
        let mut validated = BTreeMap::new();
        for (name, spec) in &self.parameters {
            let Some(value) = parameters.get(name) else {
                if spec.required {
                    return Err(format!("工具缺少参数“{name}”"));
                }
                continue;
            };
            validated.insert(name.clone(), coerce_parameter(name, spec, value)?);
        }
        Ok(validated)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.entry.trim().is_empty() {
            return Err("工具名称和入口不能为空".into());
        }
        if self.tool_version.trim().is_empty() {
            return Err("工具版本不能为空".into());
        }
        for (name, parameter) in &self.parameters {
            if name.trim().is_empty() || parameter.type_name.trim().is_empty() {
                return Err("工具参数名称和类型不能为空".into());
            }
        }
        if let Some(gui) = &self.gui {
            validate_output_root(gui.output_root.as_deref())?;
            if gui.width == 0 || gui.height == 0 {
                return Err("GUI 窗口尺寸必须大于零".into());
            }
        }
        if let Some(cli) = &self.cli {
            validate_output_root(cli.output_root.as_deref())?;
        }
        Ok(())
    }

    pub fn output_root(&self, kind: OutputKind) -> PathBuf {
        let declared = match kind {
            OutputKind::Gui => self.gui.as_ref().and_then(|gui| gui.output_root.as_deref()),
            OutputKind::Cli => self.cli.as_ref().and_then(|cli| cli.output_root.as_deref()),
        };
        PathBuf::from(declared.unwrap_or("结果"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputKind {
    Gui,
    Cli,
}

fn validate_output_root(value: Option<&str>) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err("工具输出目录必须是包目录内的非空相对路径".into());
    }
    Ok(())
}

/// Optional tool-level caps in `衡枢.toml`. `None` means “inherit the project
/// value”; a present value can only make the project capability narrower.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionSpec {
    #[serde(default, alias = "文件")]
    pub files: Option<Vec<PathBuf>>,
    #[serde(default, alias = "网络")]
    pub network: Option<Vec<String>>,
    #[serde(default, alias = "环境")]
    pub environment: Option<Vec<String>>,
    #[serde(default, alias = "允许子进程")]
    pub process: Option<bool>,
    #[serde(default, alias = "全部")]
    pub unrestricted: Option<bool>,
    #[serde(default, alias = "最长执行时间")]
    pub execution_timeout_ms: Option<u64>,
    #[serde(default, alias = "最大输出字节")]
    pub max_output_bytes: Option<usize>,
    #[serde(default, alias = "最大集合项数")]
    pub max_collection_items: Option<usize>,
    #[serde(default, alias = "最大缓冲字节")]
    pub max_buffer_bytes: Option<usize>,
    #[serde(default, alias = "最大调用深度")]
    pub max_call_depth: Option<usize>,
    #[serde(default, alias = "最大值嵌套深度")]
    pub max_value_nesting: Option<usize>,
    #[serde(default, alias = "最大模块深度")]
    pub max_module_depth: Option<usize>,
    #[serde(default, alias = "最大通道容量")]
    pub max_channel_capacity: Option<usize>,
    #[serde(default, alias = "最大并行工作者")]
    pub max_parallel_workers: Option<usize>,
    #[serde(default, alias = "允许敏感类别")]
    pub sensitive_labels: Option<BTreeSet<SensitiveLabel>>,
}

impl ToolPermissionSpec {
    /// Apply a tool declaration to an already resolved project permission set.
    /// Every capability is checked against the project ceiling before it is
    /// replaced, so a malformed or over-broad tool declaration fails early.
    pub fn restrict(
        &self,
        project: &crate::security::PermissionSet,
        base_dir: &Path,
    ) -> Result<crate::security::PermissionSet, String> {
        let mut result = project.clone();
        if let Some(unrestricted) = self.unrestricted {
            if unrestricted && !project.unrestricted {
                return Err("工具声明不能扩大“全部”权限".into());
            }
            result.unrestricted = unrestricted;
        }
        if let Some(process) = self.process {
            if process && !project.allow_process && !project.unrestricted {
                return Err("工具声明不能授予子进程权限".into());
            }
            result.allow_process = process;
        }
        if let Some(files) = &self.files {
            let mut roots = Vec::with_capacity(files.len());
            for file in files {
                let path = if file.is_absolute() {
                    file.clone()
                } else {
                    base_dir.join(file)
                };
                if !project.unrestricted {
                    project
                        .check_file(&path)
                        .map_err(|_| format!("工具声明不能扩大文件权限“{}”", path.display()))?;
                }
                roots.push(path);
            }
            result.file_roots = roots;
        }
        if let Some(hosts) = &self.network {
            if !project.unrestricted {
                for host in hosts {
                    project
                        .check_network(host)
                        .map_err(|_| format!("工具声明不能扩大网络权限“{host}”"))?;
                }
            }
            result.network_hosts = hosts.clone();
        }
        if let Some(names) = &self.environment {
            if !project.unrestricted {
                for name in names {
                    project
                        .check_environment(name)
                        .map_err(|_| format!("工具声明不能扩大环境变量权限“{name}”"))?;
                }
            }
            result.environment_names = names.clone();
        }
        result.execution_timeout_ms = restrict_u64(
            project.execution_timeout_ms,
            self.execution_timeout_ms,
            "最长执行时间",
        )?;
        result.max_output_bytes = restrict_usize(
            project.max_output_bytes,
            self.max_output_bytes,
            "最大输出字节",
        )?;
        result.max_collection_items = restrict_usize(
            project.max_collection_items,
            self.max_collection_items,
            "最大集合项数",
        )?;
        result.max_buffer_bytes = restrict_usize(
            project.max_buffer_bytes,
            self.max_buffer_bytes,
            "最大缓冲字节",
        )?;
        result.max_call_depth =
            restrict_usize(project.max_call_depth, self.max_call_depth, "最大调用深度")?;
        result.max_value_nesting = restrict_usize(
            project.max_value_nesting,
            self.max_value_nesting,
            "最大值嵌套深度",
        )?;
        result.max_module_depth = restrict_usize(
            project.max_module_depth,
            self.max_module_depth,
            "最大模块深度",
        )?;
        result.max_channel_capacity = restrict_usize(
            project.max_channel_capacity,
            self.max_channel_capacity,
            "最大通道容量",
        )?;
        result.max_parallel_workers = restrict_usize(
            project.max_parallel_workers,
            self.max_parallel_workers,
            "最大并行工作者",
        )?;
        if let Some(labels) = &self.sensitive_labels {
            if !project.unrestricted && !labels.is_subset(&project.sensitive_labels) {
                return Err("工具声明不能扩大敏感类别权限".into());
            }
            result.sensitive_labels = labels.clone();
        }
        Ok(result)
    }
}

fn restrict_usize(
    project: Option<usize>,
    requested: Option<usize>,
    name: &str,
) -> Result<Option<usize>, String> {
    let Some(requested) = requested else {
        return Ok(project);
    };
    if project.is_some_and(|limit| requested > limit) {
        return Err(format!("工具声明不能放宽{name}"));
    }
    Ok(Some(requested))
}

fn restrict_u64(
    project: Option<u64>,
    requested: Option<u64>,
    name: &str,
) -> Result<Option<u64>, String> {
    let Some(requested) = requested else {
        return Ok(project);
    };
    if project.is_some_and(|limit| requested > limit) {
        return Err(format!("工具声明不能放宽{name}"));
    }
    Ok(Some(requested))
}

fn coerce_parameter(
    name: &str,
    spec: &ParameterSpec,
    value: &JsonValue,
) -> Result<JsonValue, String> {
    let type_name = spec.type_name.trim();
    if value.is_null() {
        return if spec.required {
            Err(format!("参数“{name}”不能是空值"))
        } else {
            Ok(JsonValue::Null)
        };
    }
    match type_name {
        "文本" | "text" | "字符串" => value
            .as_str()
            .map(|value| JsonValue::String(value.to_owned()))
            .ok_or_else(|| format!("参数“{name}”必须是文本")),
        "整数" | "integer" | "int" => {
            let number = value
                .as_i64()
                .ok_or_else(|| format!("参数“{name}”必须是整数"))?;
            Ok(JsonValue::from(number))
        }
        "小数" | "数字" | "number" | "decimal" => {
            if !value.is_number() {
                return Err(format!("参数“{name}”必须是数字"));
            }
            Ok(value.clone())
        }
        "逻辑" | "布尔" | "bool" | "boolean" => value
            .as_bool()
            .map(JsonValue::from)
            .ok_or_else(|| format!("参数“{name}”必须是逻辑值")),
        "日期" | "date" => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("参数“{name}”必须是日期文本"))?;
            chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .map(|_| JsonValue::String(text.to_owned()))
                .map_err(|_| format!("参数“{name}”日期格式必须是 YYYY-MM-DD"))
        }
        "时间" | "日期时间" | "datetime" => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("参数“{name}”必须是日期时间文本"))?;
            chrono::DateTime::parse_from_rfc3339(text)
                .map(|_| JsonValue::String(text.to_owned()))
                .map_err(|_| format!("参数“{name}”日期时间必须是 RFC3339"))
        }
        "路径" | "文件" | "文件夹" | "path" | "file" | "folder" | "秘密" | "secret" => {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(|value| JsonValue::String(value.to_owned()))
                .ok_or_else(|| format!("参数“{name}”必须是非空文本"))
        }
        type_name if type_name.starts_with("枚举:") || type_name.starts_with("enum:") => {
            let choices = type_name
                .split_once(':')
                .map(|(_, choices)| choices)
                .unwrap_or_default()
                .split(',')
                .collect::<BTreeSet<_>>();
            let selected = value_string(name, value)?;
            if choices.contains(selected.as_str()) {
                Ok(JsonValue::String(selected))
            } else {
                Err(format!("参数“{name}”不在枚举范围内"))
            }
        }
        _ => Err(format!("参数“{name}”类型声明未知：{type_name}")),
    }
}

fn value_string(name: &str, value: &JsonValue) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("参数“{name}”必须是文本"))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParameterSpec {
    pub type_name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub description: String,
}

impl ParameterSpec {
    /// 秘密类型本身就是秘密，无需再显式声明 `secret`。
    pub fn is_secret(&self) -> bool {
        self.secret || matches!(self.type_name.trim(), "秘密" | "secret")
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuiSpec {
    pub title: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub output_root: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CliSpec {
    pub command: String,
    pub output_format: String,
    #[serde(default)]
    pub output_root: Option<String>,
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn resolve_beneath(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.is_absolute() {
        return Err("路径必须是相对路径".into());
    }
    let candidate = lexical_normalize(&root.join(relative));
    let root = lexical_normalize(root);
    if candidate == root
        || candidate.starts_with(root.join("")) && candidate.strip_prefix(&root).is_ok()
    {
        Ok(candidate)
    } else {
        Err("路径不能离开输入/输出根目录".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn contracts_round_trip_as_stable_json() {
        let mut request = InvocationRequest::new("工具", "run-1", "结果");
        request.parameters.insert("数量".into(), JsonValue::from(2));
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: InvocationRequest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn metadata_union_preserves_labels_units_and_deduplicates_sources() {
        let mut left = Metadata::default();
        left.sensitivity.labels.insert(SensitiveLabel::Financial);
        left.provenance.push(ProvenanceRef {
            source_id: "a".into(),
            location: None,
            step_id: Some(1),
        });
        let mut right = Metadata::default();
        right.sensitivity.labels.insert(SensitiveLabel::Identity);
        right.provenance.push(left.provenance[0].clone());
        right.unit = Some(UnitIdentity {
            name: "元".into(),
            dimension: "货币".into(),
            scale_numerator: 1,
            scale_denominator: 1,
            basis: None,
        });
        let merged = left.union(&right);
        assert_eq!(merged.sensitivity.labels.len(), 2);
        assert_eq!(merged.provenance.len(), 1);
        assert_eq!(merged.unit.as_ref().unwrap().name, "元");
    }

    #[test]
    fn unit_identity_requires_dimension_scale_and_basis_compatibility() {
        let yuan = UnitIdentity {
            name: "元".into(),
            dimension: "货币".into(),
            scale_numerator: 1,
            scale_denominator: 1,
            basis: Some("CNY".into()),
        };
        let same = UnitIdentity {
            name: "元".into(),
            dimension: "货币".into(),
            scale_numerator: 1,
            scale_denominator: 1,
            basis: Some("CNY".into()),
        };
        let wan = UnitIdentity {
            name: "万元".into(),
            dimension: "货币".into(),
            scale_numerator: 10_000,
            scale_denominator: 1,
            basis: Some("CNY".into()),
        };
        assert!(yuan.compatible_with(&same));
        assert!(!yuan.compatible_with(&wan));
    }

    #[test]
    fn paths_cannot_escape_context_roots() {
        let context = PathContext::new("程序", "工作", "输入", "输出", "临时", "权限");
        assert_eq!(
            context.resolve_input("a/b").unwrap(),
            PathBuf::from("输入/a/b")
        );
        assert!(context.resolve_input("../秘密").is_err());
        assert!(context.resolve_input("/tmp/秘密").is_err());
    }

    #[test]
    fn file_observations_are_content_addressed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("输入.txt");
        fs::write(&path, b"abc").unwrap();
        let observation = InputObservation::observe("数据", &path).unwrap();
        assert_eq!(observation.bytes, 3);
        assert_eq!(observation.sha256, hex_digest(b"abc"));
    }

    #[test]
    fn tool_permissions_can_only_narrow_project_limits() {
        let directory = tempfile::tempdir().unwrap();
        let mut project = crate::security::PermissionSet::restricted_for_source(directory.path());
        project.max_output_bytes = Some(100);
        project.network_hosts = vec!["api.example.com".into()];
        let spec = ToolPermissionSpec {
            network: Some(vec!["api.example.com".into()]),
            max_output_bytes: Some(50),
            ..Default::default()
        };
        let narrowed = spec.restrict(&project, directory.path()).unwrap();
        assert_eq!(narrowed.max_output_bytes, Some(50));
        assert!(ToolPermissionSpec {
            max_output_bytes: Some(101),
            ..Default::default()
        }
        .restrict(&project, directory.path())
        .is_err());
        assert!(ToolPermissionSpec {
            network: Some(vec!["evil.example.com".into()]),
            ..Default::default()
        }
        .restrict(&project, directory.path())
        .is_err());
    }

    #[test]
    fn tool_parameter_schema_rejects_unknown_missing_and_wrong_values() {
        let tool = ToolManifest {
            version: Default::default(),
            name: "报表".into(),
            tool_version: "1.0.0".into(),
            entry: "src/主程序.代码".into(),
            parameters: BTreeMap::from([
                (
                    "日期".into(),
                    ParameterSpec {
                        type_name: "日期".into(),
                        required: true,
                        secret: false,
                        description: String::new(),
                    },
                ),
                (
                    "模式".into(),
                    ParameterSpec {
                        type_name: "枚举:月报,季报".into(),
                        required: false,
                        secret: false,
                        description: String::new(),
                    },
                ),
            ]),
            gui: None,
            cli: None,
            permissions: ToolPermissionSpec::default(),
        };
        let valid = BTreeMap::from([
            ("日期".into(), JsonValue::String("2026-08-22".into())),
            ("模式".into(), JsonValue::String("月报".into())),
        ]);
        assert_eq!(tool.validate_parameters(&valid).unwrap(), valid);
        assert!(tool.validate_parameters(&BTreeMap::new()).is_err());
        assert!(tool
            .validate_parameters(&BTreeMap::from([(
                "日期".into(),
                JsonValue::String("昨天".into())
            )]))
            .is_err());
        assert!(tool
            .validate_parameters(&BTreeMap::from([
                ("日期".into(), JsonValue::String("2026-08-22".into())),
                ("未知".into(), JsonValue::Null)
            ]))
            .is_err());
    }

    #[test]
    fn tool_parameters_have_one_stable_program_argument_encoding() {
        let tool = ToolManifest {
            version: Default::default(),
            name: "报表".into(),
            tool_version: "1".into(),
            entry: "main".into(),
            parameters: BTreeMap::from([
                (
                    "日期".into(),
                    ParameterSpec {
                        type_name: "日期".into(),
                        required: true,
                        secret: false,
                        description: String::new(),
                    },
                ),
                (
                    "含税".into(),
                    ParameterSpec {
                        type_name: "逻辑".into(),
                        required: true,
                        secret: false,
                        description: String::new(),
                    },
                ),
            ]),
            gui: None,
            cli: None,
            permissions: Default::default(),
        };
        let args = tool
            .encode_program_args(&BTreeMap::from([
                ("日期".into(), JsonValue::String("2026-08-22".into())),
                ("含税".into(), JsonValue::Bool(true)),
            ]))
            .unwrap();
        assert_eq!(args, ["含税=true", "日期=\"2026-08-22\""]);
    }

    #[test]
    fn text_parameters_are_coerced_before_validation() {
        let tool = ToolManifest {
            version: Default::default(),
            name: "工具".into(),
            tool_version: "1".into(),
            entry: "main".into(),
            parameters: BTreeMap::from([
                (
                    "数量".into(),
                    ParameterSpec {
                        type_name: "整数".into(),
                        required: true,
                        secret: false,
                        description: String::new(),
                    },
                ),
                (
                    "启用".into(),
                    ParameterSpec {
                        type_name: "逻辑".into(),
                        required: true,
                        secret: false,
                        description: String::new(),
                    },
                ),
            ]),
            gui: None,
            cli: None,
            permissions: Default::default(),
        };
        let values = tool
            .parse_text_parameters(&BTreeMap::from([
                ("数量".into(), "12".into()),
                ("启用".into(), "true".into()),
            ]))
            .unwrap();
        assert_eq!(values["数量"], JsonValue::from(12));
        assert_eq!(values["启用"], JsonValue::from(true));
    }

    #[test]
    fn declared_output_roots_are_relative_and_bounded() {
        let mut tool = ToolManifest {
            version: Default::default(),
            name: "工具".into(),
            tool_version: "1".into(),
            entry: "main".into(),
            parameters: BTreeMap::new(),
            gui: Some(GuiSpec {
                title: "工具".into(),
                width: 800,
                height: 600,
                output_root: Some("结果/日报".into()),
            }),
            cli: None,
            permissions: Default::default(),
        };
        assert_eq!(
            tool.output_root(OutputKind::Gui),
            PathBuf::from("结果/日报")
        );
        assert!(tool.validate().is_ok());
        tool.gui.as_mut().unwrap().output_root = Some("../外部".into());
        assert!(tool.validate().is_err());
    }
}
