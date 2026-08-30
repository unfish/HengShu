//! Bounded execution observations and an offline Notebook-style report.

use crate::execution::{ExecutionEvent, ObservationBudget, ObservationMode};
use crate::execution::{Metadata, ValueSummary};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const NOTEBOOK_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationError {
    EventLimit,
    ByteLimit,
    TimeLimit,
    NonMonotonicStep,
}

impl std::fmt::Display for ObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::EventLimit => "执行观察事件超过预算",
            Self::ByteLimit => "执行观察报告超过字节预算",
            Self::TimeLimit => "执行观察超过时间预算",
            Self::NonMonotonicStep => "执行观察步骤号不递增",
        })
    }
}

impl std::error::Error for ObservationError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Notebook {
    pub version: u32,
    pub mode: ObservationMode,
    pub events: Vec<ExecutionEvent>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub parents: Vec<String>,
    pub metadata: crate::execution::Metadata,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceGraph {
    pub version: u32,
    pub nodes: BTreeMap<String, ProvenanceNode>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphBudget {
    pub max_nodes: usize,
    pub max_parents: usize,
    pub max_bytes: usize,
}

impl Default for GraphBudget {
    fn default() -> Self {
        Self {
            max_nodes: 100_000,
            max_parents: 32,
            max_bytes: 32 * 1024 * 1024,
        }
    }
}

impl ProvenanceGraph {
    pub fn add(
        &mut self,
        kind: &str,
        label: &str,
        parents: impl IntoIterator<Item = String>,
        metadata: crate::execution::Metadata,
        budget: GraphBudget,
    ) -> Result<String, ObservationError> {
        let mut parents = parents
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        parents.sort();
        if parents.len() > budget.max_parents {
            return Err(ObservationError::EventLimit);
        }
        let id = node_id(kind, label, &parents);
        if self.nodes.contains_key(&id) {
            return Ok(id);
        }
        if self.nodes.len() >= budget.max_nodes {
            self.truncated = true;
            return Err(ObservationError::EventLimit);
        }
        self.nodes.insert(
            id.clone(),
            ProvenanceNode {
                id: id.clone(),
                kind: kind.into(),
                label: label.into(),
                parents,
                metadata,
            },
        );
        let bytes = serde_json::to_vec(self)
            .map(|value| value.len())
            .unwrap_or(usize::MAX);
        if bytes > budget.max_bytes {
            self.nodes.remove(&id);
            self.truncated = true;
            return Err(ObservationError::ByteLimit);
        }
        Ok(id)
    }

    pub fn stable_digest(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("编码来源图失败：{error}"))
    }

    pub fn to_html(&self) -> Result<String, String> {
        let json = self.to_json()?;
        Ok(format!(
            "<!doctype html><meta charset=\"utf-8\"><title>衡枢来源图</title><pre>{}</pre>",
            escape_html(&json)
        ))
    }
}

/// 专用来源节点类型：对账、模板替换、报告 PDF、脱敏和单位转换。
pub const NODE_RECONCILIATION: &str = "对账";
pub const NODE_TEMPLATE: &str = "模板替换";
pub const NODE_REPORT_PDF: &str = "报告PDF";
pub const NODE_MASKING: &str = "脱敏";
pub const NODE_UNIT_CONVERSION: &str = "单位转换";

/// 对账节点的身份信息，避免节点构造函数参数过多。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconciliationNode<'a> {
    pub status: &'a str,
    pub left_id: Option<&'a str>,
    pub right_id: Option<&'a str>,
    pub rule_version: &'a str,
}

impl ProvenanceGraph {
    pub fn add_reconciliation(
        &mut self,
        node: ReconciliationNode<'_>,
        parents: impl IntoIterator<Item = String>,
        metadata: crate::execution::Metadata,
        budget: GraphBudget,
    ) -> Result<String, ObservationError> {
        let label = format!(
            "{}|左:{}|右:{}|规则:{}",
            node.status,
            node.left_id.unwrap_or("无"),
            node.right_id.unwrap_or("无"),
            node.rule_version
        );
        self.add(NODE_RECONCILIATION, &label, parents, metadata, budget)
    }

    pub fn add_template(
        &mut self,
        template: &str,
        output: &str,
        targets: &[String],
        parents: impl IntoIterator<Item = String>,
        metadata: crate::execution::Metadata,
        budget: GraphBudget,
    ) -> Result<String, ObservationError> {
        let mut targets = targets.to_vec();
        targets.sort();
        let label = format!("{template}->{output}|目标:{}", targets.join(","));
        self.add(NODE_TEMPLATE, &label, parents, metadata, budget)
    }

    pub fn add_report_pdf(
        &mut self,
        output: &str,
        pages: usize,
        parents: impl IntoIterator<Item = String>,
        metadata: crate::execution::Metadata,
        budget: GraphBudget,
    ) -> Result<String, ObservationError> {
        let label = format!("{output}|页数:{pages}");
        self.add(NODE_REPORT_PDF, &label, parents, metadata, budget)
    }

    pub fn add_masking(
        &mut self,
        field: &str,
        strategy: &str,
        parents: impl IntoIterator<Item = String>,
        metadata: crate::execution::Metadata,
        budget: GraphBudget,
    ) -> Result<String, ObservationError> {
        let label = format!("{field}|策略:{strategy}");
        self.add(NODE_MASKING, &label, parents, metadata, budget)
    }

    pub fn add_unit_conversion(
        &mut self,
        from: &str,
        to: &str,
        basis: &str,
        parents: impl IntoIterator<Item = String>,
        metadata: crate::execution::Metadata,
        budget: GraphBudget,
    ) -> Result<String, ObservationError> {
        let label = format!("{from}->{to}|依据:{basis}");
        self.add(NODE_UNIT_CONVERSION, &label, parents, metadata, budget)
    }

    /// 默认视图只显示逻辑字段和坐标；查看敏感原值需要端点与敏感类别双重授权。
    pub fn view(&self, policy: &crate::privacy::EndpointPolicy) -> Result<ProvenanceGraph, String> {
        let sensitive = self.nodes.values().any(|node| {
            !node.metadata.sensitivity.labels.is_empty()
                && !node.metadata.sensitivity.masked
                && !node.metadata.sensitivity.pseudonymized
        });
        if !sensitive {
            return Ok(self.clone());
        }
        match policy.allow(
            crate::privacy::Endpoint::EvidenceView,
            &self
                .nodes
                .values()
                .fold(
                    crate::execution::Metadata::default(),
                    |accumulated, node| accumulated.union(&node.metadata),
                )
                .sensitivity,
        ) {
            Ok(()) => Ok(self.clone()),
            Err(_) => Ok(self.redacted()),
        }
    }

    /// 去掉敏感标签节点的标签细节，只保留类型、坐标和父子关系。
    fn redacted(&self) -> ProvenanceGraph {
        let mut redacted = self.clone();
        for node in redacted.nodes.values_mut() {
            if node.metadata.sensitivity.labels.is_empty()
                || node.metadata.sensitivity.masked
                || node.metadata.sensitivity.pseudonymized
            {
                continue;
            }
            node.label = node
                .metadata
                .provenance
                .iter()
                .filter_map(|reference| reference.location.clone())
                .collect::<Vec<_>>()
                .join(",");
            if node.label.is_empty() {
                node.label = "<需要敏感查看权限>".into();
            }
        }
        redacted
    }
}

fn node_id(kind: &str, label: &str, parents: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(label.as_bytes());
    for parent in parents {
        hasher.update([0]);
        hasher.update(parent.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl Notebook {
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("编码 Notebook 失败：{error}"))
    }

    pub fn to_html(&self) -> Result<String, String> {
        let json = self.to_json()?;
        Ok(format!(
            "<!doctype html><meta charset=\"utf-8\"><title>衡枢执行过程</title><pre>{}</pre>",
            escape_html(&json)
        ))
    }
}

#[derive(Clone, Debug)]
pub struct EventCollector {
    mode: ObservationMode,
    budget: ObservationBudget,
    events: Vec<ExecutionEvent>,
    bytes: usize,
    truncated: bool,
    started: std::time::Instant,
}

pub fn summarize_value(value: &Value, metadata: Metadata, max_chars: usize) -> ValueSummary {
    let type_name = value.type_name().to_owned();
    let rendered = value.to_string();
    let truncated = rendered.chars().count() > max_chars;
    let sample = if metadata.sensitivity.labels.is_empty()
        && !metadata.sensitivity.masked
        && !metadata.sensitivity.pseudonymized
    {
        Some(rendered.chars().take(max_chars).collect())
    } else {
        Some("<已遮挡>".into())
    };
    let length = match value {
        Value::Text(text) => Some(text.chars().count()),
        Value::List(values) => Some(values.borrow().len()),
        Value::Record(record) => Some(record.borrow().len()),
        Value::Table(table) => Some(table.borrow().rows.len()),
        Value::Bytes(bytes) => Some(bytes.borrow().len()),
        _ => None,
    };
    ValueSummary {
        type_name,
        length,
        sample,
        truncated,
        metadata,
    }
}

impl EventCollector {
    pub fn new(mode: ObservationMode, budget: ObservationBudget) -> Self {
        Self {
            mode,
            budget,
            events: Vec::new(),
            bytes: 0,
            truncated: false,
            started: std::time::Instant::now(),
        }
    }

    pub fn record(&mut self, mut event: ExecutionEvent) -> Result<(), ObservationError> {
        if self.mode == ObservationMode::Off {
            return Ok(());
        }
        if self
            .events
            .last()
            .is_some_and(|previous| event.step <= previous.step)
        {
            return Err(ObservationError::NonMonotonicStep);
        }
        if self.started.elapsed().as_millis() > u128::from(self.budget.max_time_ms) {
            return Err(ObservationError::TimeLimit);
        }
        if self.events.len() >= self.budget.max_events {
            return self.limit(ObservationError::EventLimit);
        }
        if self.mode == ObservationMode::Summary {
            event.inputs.clear();
            event.outputs.clear();
            event.provenance.clear();
        }
        let size = serde_json::to_vec(&event)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if self.bytes.saturating_add(size) > self.budget.max_bytes {
            return self.limit(ObservationError::ByteLimit);
        }
        self.bytes += size;
        self.events.push(event);
        Ok(())
    }

    pub fn finish(self) -> Notebook {
        Notebook {
            version: crate::execution::CONTRACT_VERSION,
            mode: self.mode,
            events: self.events,
            truncated: self.truncated,
        }
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn limit(&mut self, error: ObservationError) -> Result<(), ObservationError> {
        self.truncated = true;
        if self.mode == ObservationMode::Detailed {
            Err(error)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::ExecutionEvent;

    fn event(step: u64) -> ExecutionEvent {
        ExecutionEvent {
            version: Default::default(),
            step,
            source_span: None,
            stage: "测试".into(),
            description: "步骤".into(),
            inputs: Default::default(),
            outputs: Default::default(),
            artifacts: Vec::new(),
            warnings: Vec::new(),
            provenance: Vec::new(),
        }
    }

    #[test]
    fn off_mode_does_not_change_or_record_execution() {
        let mut collector = EventCollector::new(ObservationMode::Off, ObservationBudget::default());
        collector.record(event(1)).unwrap();
        assert!(collector.finish().events.is_empty());
    }

    #[test]
    fn detailed_mode_enforces_event_and_order_budgets() {
        let mut collector = EventCollector::new(
            ObservationMode::Detailed,
            ObservationBudget {
                max_events: 1,
                ..Default::default()
            },
        );
        collector.record(event(1)).unwrap();
        assert_eq!(
            collector.record(event(1)),
            Err(ObservationError::NonMonotonicStep)
        );
        assert_eq!(
            collector.record(event(2)),
            Err(ObservationError::EventLimit)
        );
    }

    #[test]
    fn notebook_html_is_static_and_escaped() {
        let mut collector =
            EventCollector::new(ObservationMode::Snapshots, ObservationBudget::default());
        let mut item = event(1);
        item.description = "<script>".into();
        collector.record(item).unwrap();
        let html = collector.finish().to_html().unwrap();
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn provenance_nodes_are_content_addressed_and_order_stable() {
        let mut first = ProvenanceGraph {
            version: 1,
            ..Default::default()
        };
        let a = first
            .add(
                "输入",
                "a.csv",
                [],
                Default::default(),
                GraphBudget::default(),
            )
            .unwrap();
        let b = first
            .add(
                "聚合",
                "金额",
                [a.clone()],
                Default::default(),
                GraphBudget::default(),
            )
            .unwrap();
        let digest = first.stable_digest();
        let duplicate = first
            .add(
                "聚合",
                "金额",
                [a.clone(), a],
                Default::default(),
                GraphBudget::default(),
            )
            .unwrap();
        assert_eq!(b, duplicate);
        let mut second = ProvenanceGraph {
            version: 1,
            ..Default::default()
        };
        let a2 = second
            .add(
                "输入",
                "a.csv",
                [],
                Default::default(),
                GraphBudget::default(),
            )
            .unwrap();
        second
            .add(
                "聚合",
                "金额",
                [a2],
                Default::default(),
                GraphBudget::default(),
            )
            .unwrap();
        assert_eq!(digest, second.stable_digest());
    }

    #[test]
    fn value_summaries_are_bounded_and_mask_sensitive_values() {
        let value = Value::Text("abcdefghijklmnopqrstuvwxyz".into());
        let summary = summarize_value(&value, Metadata::default(), 5);
        assert_eq!(summary.length, Some(26));
        assert_eq!(summary.sample.as_deref(), Some("abcde"));
        assert!(summary.truncated);
        let mut metadata = Metadata::default();
        metadata
            .sensitivity
            .labels
            .insert(crate::execution::SensitiveLabel::Medical);
        let masked = summarize_value(&value, metadata, 100);
        assert_eq!(masked.sample.as_deref(), Some("<已遮挡>"));
    }

    #[test]
    fn provenance_graph_exports_static_json_and_html() {
        let mut graph = ProvenanceGraph {
            version: 1,
            ..Default::default()
        };
        graph
            .add(
                "输入",
                "a.csv",
                [],
                Default::default(),
                GraphBudget::default(),
            )
            .unwrap();
        let json = graph.to_json().unwrap();
        assert!(json.contains("a.csv"));
        let html = graph.to_html().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(!html.contains("<script"));
    }
}
