//! Deterministic bounded batch delivery orchestration.

use crate::execution::{Artifact, ContractVersion, ExecutionEvent, Metadata, RunOutcome};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitPolicy {
    AllOrNothing,
    KeepSuccessful,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchBudget {
    pub max_items: usize,
    pub max_bytes: u64,
    pub max_time_ms: u64,
}

impl Default for BatchBudget {
    fn default() -> Self {
        Self {
            max_items: 10_000,
            max_bytes: 512 * 1024 * 1024,
            max_time_ms: 300_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchItem {
    pub index: usize,
    pub name: String,
    pub payload: Vec<u8>,
    pub kind: String,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchResult {
    pub index: usize,
    pub name: String,
    pub success: bool,
    pub error: Option<String>,
    pub artifact: Option<Artifact>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchOutcome {
    pub results: Vec<BatchResult>,
    pub artifacts: Vec<Artifact>,
    pub bytes: u64,
    pub cancelled: bool,
    pub events: Vec<ExecutionEvent>,
}

impl BatchOutcome {
    pub fn append_to(self, outcome: &mut RunOutcome) {
        outcome.resources.output_bytes = outcome
            .resources
            .output_bytes
            .saturating_add(self.bytes.min(usize::MAX as u64) as usize);
        outcome.artifacts.extend(self.artifacts);
        outcome.events.extend(self.events);
        outcome.events.sort_by_key(|event| event.step);
        if self.cancelled || self.results.iter().any(|result| !result.success) {
            outcome.success = false;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldFormat {
    Preserve,
    Text,
    Decimal {
        scale: u32,
    },
    Date {
        output: String,
    },
    Boolean {
        true_text: String,
        false_text: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldMapping {
    pub source: String,
    pub target: String,
    pub required: bool,
    pub default: Option<JsonValue>,
    pub format: FieldFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub index: usize,
    pub fields: BTreeMap<String, JsonValue>,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappedBatchRecord {
    pub index: usize,
    pub fields: BTreeMap<String, JsonValue>,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupRule {
    pub fields: Vec<String>,
    pub separator: String,
}

pub fn map_batch_fields(
    records: &[BatchRecord],
    mappings: &[FieldMapping],
) -> Result<Vec<MappedBatchRecord>, String> {
    let mut targets = BTreeSet::new();
    for mapping in mappings {
        if mapping.source.trim().is_empty() || mapping.target.trim().is_empty() {
            return Err("批量字段映射的源字段和目标字段不能为空".into());
        }
        if !targets.insert(mapping.target.clone()) {
            return Err(format!("批量目标字段重复：{}", mapping.target));
        }
    }
    records
        .iter()
        .map(|record| {
            let mut fields = BTreeMap::new();
            for mapping in mappings {
                let value = record
                    .fields
                    .get(&mapping.source)
                    .filter(|value| !value.is_null())
                    .cloned()
                    .or_else(|| mapping.default.clone());
                let Some(value) = value else {
                    if mapping.required {
                        return Err(format!(
                            "批量第 {} 项缺少必填字段：{}",
                            record.index, mapping.source
                        ));
                    }
                    fields.insert(mapping.target.clone(), JsonValue::Null);
                    continue;
                };
                fields.insert(
                    mapping.target.clone(),
                    format_field(&value, &mapping.format)?,
                );
            }
            Ok(MappedBatchRecord {
                index: record.index,
                fields,
                metadata: record.metadata.clone(),
            })
        })
        .collect()
}

pub fn group_batch_records(
    records: Vec<MappedBatchRecord>,
    rule: &GroupRule,
) -> Result<BTreeMap<String, Vec<MappedBatchRecord>>, String> {
    if rule.fields.is_empty() {
        return Err("批量分组至少需要一个字段".into());
    }
    let mut groups = BTreeMap::<String, Vec<MappedBatchRecord>>::new();
    for record in records {
        let key = rule
            .fields
            .iter()
            .map(|field| {
                record
                    .fields
                    .get(field)
                    .filter(|value| !value.is_null())
                    .map(json_text)
                    .ok_or_else(|| format!("批量第 {} 项缺少分组字段：{field}", record.index))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(&rule.separator);
        groups.entry(key).or_default().push(record);
    }
    Ok(groups)
}

pub fn render_filename(
    pattern: &str,
    fields: &BTreeMap<String, JsonValue>,
) -> Result<String, String> {
    let mut output = pattern.to_string();
    for (field, value) in fields {
        output = output.replace(&format!("{{{{{field}}}}}"), &json_text(value));
    }
    if output.contains("{{") || output.contains("}}") {
        return Err("输出文件名规则包含未解析字段".into());
    }
    sanitize_filename(&output)
}

pub struct MappedBatchPlan<'a> {
    pub mappings: &'a [FieldMapping],
    pub filename_pattern: &'a str,
    pub kind: &'a str,
    pub policy: CommitPolicy,
    pub budget: BatchBudget,
}

pub fn deliver_mapped_batch<F>(
    root: &Path,
    records: &[BatchRecord],
    plan: MappedBatchPlan<'_>,
    cancel: impl Fn() -> bool,
    mut render: F,
) -> Result<BatchOutcome, String>
where
    F: FnMut(&MappedBatchRecord) -> Result<Vec<u8>, String>,
{
    if records.len() > plan.budget.max_items {
        return Err("批量项数超过预算".into());
    }
    let mut items = Vec::with_capacity(records.len());
    let mut failures = Vec::new();
    for record in records {
        if cancel() {
            return Ok(BatchOutcome {
                results: failures,
                artifacts: Vec::new(),
                bytes: 0,
                cancelled: true,
                events: Vec::new(),
            });
        }
        let mapped = match map_batch_fields(std::slice::from_ref(record), plan.mappings) {
            Ok(mut values) => values.remove(0),
            Err(error) => {
                failures.push(BatchResult {
                    index: record.index,
                    name: String::new(),
                    success: false,
                    error: Some(error),
                    artifact: None,
                });
                continue;
            }
        };
        let name = match render_filename(plan.filename_pattern, &mapped.fields) {
            Ok(name) => name,
            Err(error) => {
                failures.push(BatchResult {
                    index: record.index,
                    name: String::new(),
                    success: false,
                    error: Some(error),
                    artifact: None,
                });
                continue;
            }
        };
        match render(&mapped) {
            Ok(payload) => items.push(BatchItem {
                index: record.index,
                name,
                payload,
                kind: plan.kind.into(),
                metadata: mapped.metadata,
            }),
            Err(error) => failures.push(BatchResult {
                index: record.index,
                name,
                success: false,
                error: Some(error),
                artifact: None,
            }),
        }
    }
    if plan.policy == CommitPolicy::AllOrNothing && !failures.is_empty() {
        failures.sort_by_key(|result| result.index);
        return Ok(BatchOutcome {
            results: failures,
            artifacts: Vec::new(),
            bytes: 0,
            cancelled: false,
            events: Vec::new(),
        });
    }
    let mut outcome = write_batch(root, items, plan.policy, plan.budget, &cancel)?;
    outcome.results.extend(failures);
    outcome.results.sort_by_key(|result| result.index);
    for result in outcome.results.iter().filter(|result| !result.success) {
        outcome.events.push(ExecutionEvent {
            version: ContractVersion::default(),
            step: result.index as u64 + 1,
            source_span: None,
            stage: "批量交付失败".into(),
            description: format!("批量第 {} 项生成失败", result.index),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: Vec::new(),
            warnings: result.error.clone().into_iter().collect(),
            provenance: Vec::new(),
        });
    }
    outcome.events.sort_by_key(|event| event.step);
    Ok(outcome)
}

fn format_field(value: &JsonValue, format: &FieldFormat) -> Result<JsonValue, String> {
    match format {
        FieldFormat::Preserve => Ok(value.clone()),
        FieldFormat::Text => Ok(JsonValue::String(json_text(value))),
        FieldFormat::Decimal { scale } => {
            if *scale > 28 {
                return Err("批量小数格式精度必须在 0 到 28 之间".into());
            }
            let decimal = Decimal::from_str_exact(&json_text(value))
                .map_err(|_| "批量字段不是合法小数".to_string())?;
            Ok(JsonValue::String(format!(
                "{decimal:.scale$}",
                scale = *scale as usize
            )))
        }
        FieldFormat::Date { output } => {
            let date = NaiveDate::parse_from_str(&json_text(value), "%Y-%m-%d")
                .map_err(|_| "批量日期字段必须是 YYYY-MM-DD".to_string())?;
            Ok(JsonValue::String(date.format(output).to_string()))
        }
        FieldFormat::Boolean {
            true_text,
            false_text,
        } => value
            .as_bool()
            .map(|value| JsonValue::String(if value { true_text } else { false_text }.clone()))
            .ok_or_else(|| "批量逻辑字段必须是布尔值".into()),
    }
}

fn json_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) => value.clone(),
        JsonValue::Null => String::new(),
        value => value.to_string(),
    }
}

pub fn sanitize_filename(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err("输出文件名不能为空或为保留路径".into());
    }
    if Path::new(trimmed).is_absolute()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
    {
        return Err("输出文件名不能包含绝对路径、路径分隔符或 ..".into());
    }
    let invalid = ['<', '>', ':', '"', '|', '?', '*'];
    if trimmed
        .chars()
        .any(|character| invalid.contains(&character) || character.is_control())
    {
        return Err("输出文件名包含非法字符".into());
    }
    if trimmed.len() > 240 {
        return Err("输出文件名过长".into());
    }
    Ok(trimmed.to_owned())
}

pub fn write_batch(
    root: &Path,
    mut items: Vec<BatchItem>,
    policy: CommitPolicy,
    budget: BatchBudget,
    cancel: impl Fn() -> bool,
) -> Result<BatchOutcome, String> {
    if items.len() > budget.max_items {
        return Err("批量项数超过预算".into());
    }
    items.sort_by_key(|item| item.index);
    // Validate every destination before creating anything. This prevents a
    // later collision from leaving an earlier batch partially committed.
    let mut planned_names = BTreeSet::new();
    for item in &items {
        let name = sanitize_filename(&item.name)?;
        if !planned_names.insert(name.clone()) {
            return Err(format!("批量输出文件重名：{name}"));
        }
        let destination = root.join(&name);
        if destination.symlink_metadata().is_ok() {
            return Err(format!("批量输出目标已存在：{name}"));
        }
    }
    let started = std::time::Instant::now();
    let mut names = BTreeSet::new();
    let mut staged = Vec::<(PathBuf, PathBuf, BatchResult)>::new();
    let mut results = Vec::with_capacity(items.len());
    let mut bytes = 0u64;
    for item in items {
        if cancel() {
            return rollback(staged, results, bytes, true, policy);
        }
        if started.elapsed().as_millis() > u128::from(budget.max_time_ms) {
            return Err("批量输出超过时间预算".into());
        }
        let safe_name = sanitize_filename(&item.name)?;
        if !names.insert(safe_name.clone()) {
            return Err(format!("批量输出文件重名：{safe_name}"));
        }
        bytes = bytes.saturating_add(item.payload.len() as u64);
        if bytes > budget.max_bytes {
            return Err("批量输出字节超过预算".into());
        }
        let destination = root.join(&safe_name);
        let temporary = root.join(format!(".衡枢临时-{}-{}", item.index, safe_name));
        fs::write(&temporary, &item.payload)
            .map_err(|error| format!("写入批量临时文件失败：{error}"))?;
        let mut artifact =
            Artifact::from_bytes(destination.clone(), item.kind.clone(), &item.payload);
        artifact.metadata = item.metadata.clone();
        artifact.step_id = Some(item.index as u64 + 1);
        staged.push((
            temporary,
            destination,
            BatchResult {
                index: item.index,
                name: safe_name,
                success: true,
                error: None,
                artifact: Some(artifact),
            },
        ));
    }
    let mut artifacts = Vec::with_capacity(staged.len());
    let mut committed = Vec::new();
    for (temporary, destination, result) in &staged {
        if let Err(error) = fs::rename(temporary, destination) {
            for (temporary, _, _) in &staged {
                let _ = fs::remove_file(temporary);
            }
            for path in committed {
                let _ = fs::remove_file(path);
            }
            return Err(format!("提交批量输出失败：{error}"));
        }
        committed.push(destination.clone());
        if let Some(artifact) = result.artifact.clone() {
            artifacts.push(artifact);
        }
        results.push(result.clone());
    }
    results.sort_by_key(|result| result.index);
    let events = artifact_events(&artifacts);
    Ok(BatchOutcome {
        results,
        artifacts,
        bytes,
        cancelled: false,
        events,
    })
}

fn rollback(
    staged: Vec<(PathBuf, PathBuf, BatchResult)>,
    mut results: Vec<BatchResult>,
    bytes: u64,
    cancelled: bool,
    policy: CommitPolicy,
) -> Result<BatchOutcome, String> {
    if policy == CommitPolicy::AllOrNothing {
        for (temporary, _, _) in &staged {
            let _ = fs::remove_file(temporary);
        }
        return Ok(BatchOutcome {
            results,
            artifacts: Vec::new(),
            bytes: 0,
            cancelled,
            events: Vec::new(),
        });
    }
    let mut artifacts = Vec::new();
    for (temporary, destination, result) in staged {
        if fs::rename(&temporary, &destination).is_ok() {
            if let Some(artifact) = result.artifact.clone() {
                artifacts.push(artifact);
            }
            results.push(result);
        }
    }
    results.sort_by_key(|result| result.index);
    let events = artifact_events(&artifacts);
    Ok(BatchOutcome {
        results,
        artifacts,
        bytes,
        cancelled,
        events,
    })
}

fn artifact_events(artifacts: &[Artifact]) -> Vec<ExecutionEvent> {
    artifacts
        .iter()
        .map(|artifact| ExecutionEvent {
            version: ContractVersion::default(),
            step: artifact.step_id.unwrap_or(0),
            source_span: None,
            stage: "交付物".into(),
            description: format!("生成批量交付物 {}", artifact.display_name),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: vec![artifact.sha256.clone()],
            warnings: Vec::new(),
            provenance: artifact.metadata.provenance.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn filenames_are_safe_and_batch_results_are_stable() {
        assert!(sanitize_filename("../secret").is_err());
        assert!(sanitize_filename("/tmp/out").is_err());
        let directory = tempdir().unwrap();
        let outcome = write_batch(
            directory.path(),
            vec![
                BatchItem {
                    index: 2,
                    name: "b.txt".into(),
                    payload: b"b".to_vec(),
                    kind: "text/plain".into(),
                    metadata: Metadata::default(),
                },
                BatchItem {
                    index: 1,
                    name: "a.txt".into(),
                    payload: b"a".to_vec(),
                    kind: "text/plain".into(),
                    metadata: Metadata::default(),
                },
            ],
            CommitPolicy::AllOrNothing,
            BatchBudget::default(),
            || false,
        )
        .unwrap();
        assert_eq!(
            outcome
                .results
                .iter()
                .map(|result| result.index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(fs::read(directory.path().join("a.txt")).unwrap(), b"a");
    }

    #[test]
    fn batch_artifacts_preserve_sensitive_and_provenance_metadata() {
        let directory = tempdir().unwrap();
        let mut metadata = Metadata::default();
        metadata
            .sensitivity
            .labels
            .insert(crate::execution::SensitiveLabel::Financial);
        metadata.provenance.push(crate::execution::ProvenanceRef {
            source_id: "input".into(),
            location: Some("row:1".into()),
            step_id: Some(2),
        });
        let outcome = write_batch(
            directory.path(),
            vec![BatchItem {
                index: 0,
                name: "out.txt".into(),
                payload: b"x".to_vec(),
                kind: "text/plain".into(),
                metadata: metadata.clone(),
            }],
            CommitPolicy::AllOrNothing,
            BatchBudget::default(),
            || false,
        )
        .unwrap();
        assert_eq!(outcome.artifacts[0].metadata, metadata);
    }

    #[test]
    fn cancellation_under_all_or_nothing_leaves_no_success_files() {
        let directory = tempdir().unwrap();
        let outcome = write_batch(
            directory.path(),
            vec![BatchItem {
                index: 1,
                name: "a.txt".into(),
                payload: b"a".to_vec(),
                kind: "text/plain".into(),
                metadata: Metadata::default(),
            }],
            CommitPolicy::AllOrNothing,
            BatchBudget::default(),
            || true,
        )
        .unwrap();
        assert!(outcome.cancelled);
        assert!(!directory.path().join("a.txt").exists());
    }

    #[test]
    fn field_mapping_applies_defaults_formats_and_safe_filename_rules() {
        let records = vec![BatchRecord {
            index: 0,
            fields: BTreeMap::from([
                ("姓名".into(), JsonValue::String("甲".into())),
                ("金额".into(), JsonValue::String("12.5".into())),
                ("日期".into(), JsonValue::String("2026-08-24".into())),
            ]),
            metadata: Metadata::default(),
        }];
        let mapped = map_batch_fields(
            &records,
            &[
                FieldMapping {
                    source: "姓名".into(),
                    target: "name".into(),
                    required: true,
                    default: None,
                    format: FieldFormat::Text,
                },
                FieldMapping {
                    source: "金额".into(),
                    target: "amount".into(),
                    required: true,
                    default: None,
                    format: FieldFormat::Decimal { scale: 2 },
                },
                FieldMapping {
                    source: "缺省".into(),
                    target: "status".into(),
                    required: false,
                    default: Some(JsonValue::String("正常".into())),
                    format: FieldFormat::Preserve,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            mapped[0].fields["amount"],
            JsonValue::String("12.50".into())
        );
        assert_eq!(
            render_filename("{{name}}-{{status}}.pdf", &mapped[0].fields).unwrap(),
            "甲-正常.pdf"
        );
    }

    #[test]
    fn batch_outcome_contains_artifact_events_with_provenance() {
        let directory = tempdir().unwrap();
        let mut metadata = Metadata::default();
        metadata.provenance.push(crate::execution::ProvenanceRef {
            source_id: "input".into(),
            location: None,
            step_id: Some(1),
        });
        let outcome = write_batch(
            directory.path(),
            vec![BatchItem {
                index: 0,
                name: "a.txt".into(),
                payload: b"x".to_vec(),
                kind: "text/plain".into(),
                metadata,
            }],
            CommitPolicy::AllOrNothing,
            BatchBudget::default(),
            || false,
        )
        .unwrap();
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(
            outcome.events[0].artifacts,
            vec![outcome.artifacts[0].sha256.clone()]
        );
        let mut run = RunOutcome::default();
        outcome.append_to(&mut run);
        assert_eq!(run.artifacts.len(), 1);
        assert_eq!(run.events.len(), 1);
        assert_eq!(run.resources.output_bytes, 1);
    }

    #[test]
    fn mapped_delivery_keeps_item_failures_and_success_artifacts_together() {
        let directory = tempdir().unwrap();
        let records = vec![
            BatchRecord {
                index: 0,
                fields: BTreeMap::from([("姓名".into(), JsonValue::String("甲".into()))]),
                metadata: Metadata::default(),
            },
            BatchRecord {
                index: 1,
                fields: BTreeMap::new(),
                metadata: Metadata::default(),
            },
        ];
        let outcome = deliver_mapped_batch(
            directory.path(),
            &records,
            MappedBatchPlan {
                mappings: &[FieldMapping {
                    source: "姓名".into(),
                    target: "name".into(),
                    required: true,
                    default: None,
                    format: FieldFormat::Text,
                }],
                filename_pattern: "{{name}}.pdf",
                kind: "application/pdf",
                policy: CommitPolicy::KeepSuccessful,
                budget: BatchBudget::default(),
            },
            || false,
            |record| Ok(format!("PDF:{}", record.fields["name"]).into_bytes()),
        )
        .unwrap();
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(outcome.results.len(), 2);
        assert!(outcome.results.iter().any(|result| !result.success));
        assert_eq!(outcome.events.len(), 2);
    }

    #[test]
    fn mapping_is_bounded_and_stable_at_product_acceptance_sizes() {
        let mapping = [FieldMapping {
            source: "id".into(),
            target: "id".into(),
            required: true,
            default: None,
            format: FieldFormat::Preserve,
        }];
        for size in [0usize, 1, 200, 10_000] {
            let records = (0..size)
                .map(|index| BatchRecord {
                    index,
                    fields: BTreeMap::from([("id".into(), JsonValue::from(index))]),
                    metadata: Metadata::default(),
                })
                .collect::<Vec<_>>();
            let mapped = map_batch_fields(&records, &mapping).unwrap();
            assert_eq!(mapped.len(), size);
            assert!(mapped
                .windows(2)
                .all(|items| items[0].index < items[1].index));
        }
    }

    #[test]
    fn disk_failure_does_not_report_a_success_artifact() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("missing");
        let result = write_batch(
            &missing,
            vec![BatchItem {
                index: 0,
                name: "a.pdf".into(),
                payload: b"pdf".to_vec(),
                kind: "application/pdf".into(),
                metadata: Metadata::default(),
            }],
            CommitPolicy::AllOrNothing,
            BatchBudget::default(),
            || false,
        );
        assert!(result.is_err());
        assert!(!missing.join("a.pdf").exists());
    }
}
