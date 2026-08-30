//! Deterministic, bounded reconciliation primitives.
//!
//! This module has no interpreter state and therefore can be shared by the
//! source interpreter, VM adapters and packaged tools.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

use crate::execution::{Metadata, ProvenanceRef};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Cell {
    Null,
    Text(String),
    Number(Decimal),
    Date(String),
    Bool(bool),
}

impl Cell {
    fn normalized(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Text(value) | Self::Date(value) => normalize_text(value),
            Self::Number(value) => value.normalize().to_string(),
            Self::Bool(value) => value.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub fields: BTreeMap<String, Cell>,
    pub metadata: Metadata,
    #[serde(default)]
    pub field_metadata: BTreeMap<String, Metadata>,
}

impl Record {
    pub fn new(id: impl Into<String>, fields: BTreeMap<String, Cell>) -> Self {
        Self {
            id: id.into(),
            fields,
            metadata: Metadata::default(),
            field_metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationRule {
    pub version: String,
    pub exact_keys: Vec<String>,
    pub compare_fields: Vec<String>,
    pub amount_fields: BTreeSet<String>,
    pub absolute_tolerance: Decimal,
    pub relative_tolerance: Option<Decimal>,
    pub date_window_days: BTreeMap<String, i64>,
    pub fuzzy_fields: Vec<String>,
    pub fuzzy_threshold: u32,
    pub max_candidate_edges: usize,
}

impl Default for ReconciliationRule {
    fn default() -> Self {
        Self {
            version: "1".into(),
            exact_keys: Vec::new(),
            compare_fields: Vec::new(),
            amount_fields: BTreeSet::new(),
            absolute_tolerance: Decimal::ZERO,
            relative_tolerance: None,
            date_window_days: BTreeMap::new(),
            fuzzy_fields: Vec::new(),
            fuzzy_threshold: 90,
            max_candidate_edges: 10_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum ReconciliationStatus {
    Matched,
    LeftOnly,
    RightOnly,
    FieldMismatch,
    DuplicateKey,
    Suspected,
    Ambiguous,
    Undecided,
    RuleError,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Difference {
    pub field: String,
    pub left: Option<Cell>,
    pub right: Option<Cell>,
    pub delta: Option<Decimal>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationRow {
    pub status: ReconciliationStatus,
    pub left_id: Option<String>,
    pub right_id: Option<String>,
    pub score: Option<u32>,
    pub differences: Vec<Difference>,
    pub rule_version: String,
    pub metadata: Metadata,
    pub provenance: Vec<ProvenanceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManualMapping {
    pub version: String,
    pub left_id: String,
    pub right_id: String,
    pub reason: String,
}

pub fn export_manual_mappings(mappings: &[ManualMapping]) -> Result<String, String> {
    let mut mappings = mappings.to_vec();
    mappings.sort_by(|left, right| {
        (&left.left_id, &left.right_id).cmp(&(&right.left_id, &right.right_id))
    });
    serde_json::to_string_pretty(&mappings).map_err(|error| format!("导出人工映射失败：{error}"))
}

pub fn import_manual_mappings(
    text: &str,
    expected_version: &str,
) -> Result<Vec<ManualMapping>, String> {
    let mappings: Vec<ManualMapping> =
        serde_json::from_str(text).map_err(|error| format!("人工映射格式无效：{error}"))?;
    let mut seen = BTreeSet::new();
    for mapping in &mappings {
        if mapping.version != expected_version {
            return Err(format!("人工映射规则版本不匹配：{}", mapping.version));
        }
        if mapping.left_id.is_empty()
            || mapping.right_id.is_empty()
            || !seen.insert((&mapping.left_id, &mapping.right_id))
        {
            return Err("人工映射包含空 ID 或重复映射".into());
        }
    }
    Ok(mappings)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconciliationError {
    EmptyKey,
    NegativeTolerance,
    CandidateLimit,
    MissingField(String),
    Cancelled,
}

impl std::fmt::Display for ReconciliationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyKey => formatter.write_str("对账键不能为空"),
            Self::NegativeTolerance => formatter.write_str("对账容差不能为负数"),
            Self::CandidateLimit => formatter.write_str("对账候选边超过资源上限"),
            Self::MissingField(field) => write!(formatter, "对账字段不存在：{field}"),
            Self::Cancelled => formatter.write_str("对账已取消"),
        }
    }
}

impl std::error::Error for ReconciliationError {}

pub fn reconcile(
    left: &[Record],
    right: &[Record],
    rule: &ReconciliationRule,
) -> Result<Vec<ReconciliationRow>, ReconciliationError> {
    reconcile_with_cancellation(left, right, rule, &|| false)
}

/// 与 [`reconcile`] 相同，但在每条左侧记录前询问取消回调，便于中断长时间对账。
pub fn reconcile_with_cancellation(
    left: &[Record],
    right: &[Record],
    rule: &ReconciliationRule,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<ReconciliationRow>, ReconciliationError> {
    validate_rule(left, right, rule)?;
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_by(|a, b| a.id.cmp(&b.id));
    right.sort_by(|a, b| a.id.cmp(&b.id));
    let has_exact_keys = !rule.exact_keys.is_empty();
    let left_keys = if has_exact_keys {
        index_keys(&left, rule)
    } else {
        BTreeMap::new()
    };
    let right_keys = if has_exact_keys {
        index_keys(&right, rule)
    } else {
        BTreeMap::new()
    };
    let mut used_right = BTreeSet::new();
    let mut used_fuzzy_right = BTreeSet::new();
    let mut rows = Vec::new();
    for item in &left {
        if cancelled() {
            return Err(ReconciliationError::Cancelled);
        }
        if !has_exact_keys {
            rows.push(row(
                ReconciliationStatus::Undecided,
                Some(item),
                None,
                None,
                Vec::new(),
                rule,
            ));
            continue;
        }
        let key = key_for(item, rule);
        let Some(candidates) = right_keys.get(&key) else {
            rows.push(row(
                ReconciliationStatus::LeftOnly,
                Some(item),
                None,
                None,
                Vec::new(),
                rule,
            ));
            continue;
        };
        if candidates.len() != 1 || left_keys.get(&key).is_some_and(|items| items.len() != 1) {
            rows.push(row(
                ReconciliationStatus::DuplicateKey,
                Some(item),
                None,
                None,
                Vec::new(),
                rule,
            ));
            continue;
        }
        let matched = &right[candidates[0]];
        used_right.insert(candidates[0]);
        if !units_compatible(item, matched, rule) {
            rows.push(row(
                ReconciliationStatus::RuleError,
                Some(item),
                Some(matched),
                None,
                Vec::new(),
                rule,
            ));
            continue;
        }
        let differences = differences(item, matched, rule);
        let status = if differences.is_empty() {
            ReconciliationStatus::Matched
        } else {
            ReconciliationStatus::FieldMismatch
        };
        rows.push(row(
            status,
            Some(item),
            Some(matched),
            Some(100),
            differences,
            rule,
        ));
    }
    for (index, item) in right.iter().enumerate() {
        if used_right.contains(&index) {
            continue;
        }
        let key = key_for(item, rule);
        if left_keys.contains_key(&key) {
            continue;
        }
        rows.push(row(
            ReconciliationStatus::RightOnly,
            None,
            Some(item),
            None,
            Vec::new(),
            rule,
        ));
    }
    let mut candidate_edges = 0usize;
    for item in &left {
        if cancelled() {
            return Err(ReconciliationError::Cancelled);
        }
        if left_keys.contains_key(&key_for(item, rule)) {
            continue;
        }
        let candidates = right
            .iter()
            .enumerate()
            .filter(|(index, other)| {
                !used_right.contains(index)
                    && !used_fuzzy_right.contains(index)
                    && fuzzy_score(item, other, rule)
                        .is_some_and(|score| score >= rule.fuzzy_threshold)
            })
            .collect::<Vec<_>>();
        candidate_edges = candidate_edges.saturating_add(candidates.len());
        if candidate_edges > rule.max_candidate_edges {
            return Err(ReconciliationError::CandidateLimit);
        }
        if candidates.len() == 1 {
            let (right_index, right_item) = candidates[0];
            used_fuzzy_right.insert(right_index);
            rows.retain(|row| row.left_id.as_deref() != Some(&item.id));
            rows.push(row(
                ReconciliationStatus::Suspected,
                Some(item),
                Some(right_item),
                fuzzy_score(item, right_item, rule),
                differences(item, right_item, rule),
                rule,
            ));
        } else if candidates.len() > 1 {
            rows.retain(|row| row.left_id.as_deref() != Some(&item.id));
            rows.push(row(
                ReconciliationStatus::Ambiguous,
                Some(item),
                None,
                None,
                Vec::new(),
                rule,
            ));
        }
    }
    rows.sort_by_key(row_key);
    Ok(rows)
}

fn validate_rule(
    left: &[Record],
    right: &[Record],
    rule: &ReconciliationRule,
) -> Result<(), ReconciliationError> {
    if rule.exact_keys.iter().any(|key| key.trim().is_empty()) {
        return Err(ReconciliationError::EmptyKey);
    }
    if rule.absolute_tolerance < Decimal::ZERO
        || rule
            .relative_tolerance
            .is_some_and(|value| value < Decimal::ZERO)
        || rule.date_window_days.values().any(|value| *value < 0)
    {
        return Err(ReconciliationError::NegativeTolerance);
    }
    if rule.fuzzy_threshold > 100 {
        return Err(ReconciliationError::NegativeTolerance);
    }
    for record in left.iter().chain(right) {
        for field in rule
            .exact_keys
            .iter()
            .chain(rule.compare_fields.iter())
            .chain(rule.fuzzy_fields.iter())
        {
            if !record.fields.contains_key(field) {
                return Err(ReconciliationError::MissingField(field.clone()));
            }
        }
    }
    Ok(())
}

fn units_compatible(left: &Record, right: &Record, rule: &ReconciliationRule) -> bool {
    (match (&left.metadata.unit, &right.metadata.unit) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }) && rule.compare_fields.iter().all(|field| {
        match (
            left.field_metadata
                .get(field)
                .and_then(|item| item.unit.as_ref()),
            right
                .field_metadata
                .get(field)
                .and_then(|item| item.unit.as_ref()),
        ) {
            (Some(left), Some(right)) => left == right,
            (None, None) => true,
            _ => false,
        }
    })
}

fn index_keys(records: &[Record], rule: &ReconciliationRule) -> BTreeMap<String, Vec<usize>> {
    let mut index = BTreeMap::new();
    for (position, record) in records.iter().enumerate() {
        index
            .entry(key_for(record, rule))
            .or_insert_with(Vec::new)
            .push(position);
    }
    index
}

fn key_for(record: &Record, rule: &ReconciliationRule) -> String {
    rule.exact_keys
        .iter()
        .map(|field| {
            record
                .fields
                .get(field)
                .map(Cell::normalized)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn differences(left: &Record, right: &Record, rule: &ReconciliationRule) -> Vec<Difference> {
    rule.compare_fields
        .iter()
        .filter_map(|field| {
            let left_value = left.fields.get(field).cloned();
            let right_value = right.fields.get(field).cloned();
            if values_equal(field, left_value.as_ref(), right_value.as_ref(), rule) {
                None
            } else {
                let delta = match (left_value.as_ref(), right_value.as_ref()) {
                    (Some(Cell::Number(a)), Some(Cell::Number(b))) => Some(*a - *b),
                    _ => None,
                };
                Some(Difference {
                    field: field.clone(),
                    left: left_value,
                    right: right_value,
                    delta,
                })
            }
        })
        .collect()
}

fn values_equal(
    field: &str,
    left: Option<&Cell>,
    right: Option<&Cell>,
    rule: &ReconciliationRule,
) -> bool {
    match (left, right) {
        (Some(Cell::Number(a)), Some(Cell::Number(b))) if rule.amount_fields.contains(field) => {
            let difference = (*a - *b).abs();
            if difference <= rule.absolute_tolerance {
                return true;
            }
            rule.relative_tolerance
                .is_some_and(|relative| difference <= a.abs().max(b.abs()) * relative)
        }
        (Some(Cell::Date(left)), Some(Cell::Date(right)))
            if rule.date_window_days.contains_key(field) =>
        {
            date_distance_days(left, right)
                .is_some_and(|distance| distance <= rule.date_window_days[field])
        }
        (Some(left), Some(right)) => left.normalized() == right.normalized(),
        (None, None) => true,
        _ => false,
    }
}

fn fuzzy_score(left: &Record, right: &Record, rule: &ReconciliationRule) -> Option<u32> {
    if rule.fuzzy_fields.is_empty() {
        return None;
    }
    let mut total = 0u32;
    for field in &rule.fuzzy_fields {
        let left = left.fields.get(field)?.normalized();
        let right = right.fields.get(field)?.normalized();
        total = total.saturating_add(similarity_score(&left, &right));
    }
    Some(total / rule.fuzzy_fields.len() as u32)
}

fn similarity_score(left: &str, right: &str) -> u32 {
    if left == right {
        return 100;
    }
    let distance = strsim::levenshtein(left, right);
    let length = left.chars().count().max(right.chars().count());
    100u32.saturating_sub(
        distance
            .checked_mul(100)
            .and_then(|value| value.checked_div(length))
            .unwrap_or(100) as u32,
    )
}

fn date_distance_days(left: &str, right: &str) -> Option<i64> {
    let left = chrono::NaiveDate::parse_from_str(left.get(..10)?, "%Y-%m-%d").ok()?;
    let right = chrono::NaiveDate::parse_from_str(right.get(..10)?, "%Y-%m-%d").ok()?;
    Some((left - right).num_days().abs())
}

fn row(
    status: ReconciliationStatus,
    left: Option<&Record>,
    right: Option<&Record>,
    score: Option<u32>,
    differences: Vec<Difference>,
    rule: &ReconciliationRule,
) -> ReconciliationRow {
    let metadata = match (left, right) {
        (Some(left), Some(right)) => left.metadata.union(&right.metadata),
        (Some(left), None) => left.metadata.clone(),
        (None, Some(right)) => right.metadata.clone(),
        _ => Metadata::default(),
    };
    ReconciliationRow {
        status,
        left_id: left.map(|record| record.id.clone()),
        right_id: right.map(|record| record.id.clone()),
        score,
        differences,
        rule_version: rule.version.clone(),
        provenance: metadata.provenance.clone(),
        metadata,
    }
}

/// 把对账结果接入专用来源节点：每行一个节点，父节点是行携带的来源引用。
pub fn record_provenance(
    graph: &mut crate::provenance::ProvenanceGraph,
    rows: &[ReconciliationRow],
    budget: crate::provenance::GraphBudget,
) -> Result<Vec<String>, crate::provenance::ObservationError> {
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        let status = format!("{:?}", row.status);
        ids.push(
            graph.add_reconciliation(
                crate::provenance::ReconciliationNode {
                    status: &status,
                    left_id: row.left_id.as_deref(),
                    right_id: row.right_id.as_deref(),
                    rule_version: &row.rule_version,
                },
                row.provenance
                    .iter()
                    .map(|reference| reference.source_id.clone()),
                row.metadata.clone(),
                budget,
            )?,
        );
    }
    Ok(ids)
}

fn row_key(row: &ReconciliationRow) -> (String, String, ReconciliationStatus) {
    (
        row.left_id.clone().unwrap_or_default(),
        row.right_id.clone().unwrap_or_default(),
        row.status,
    )
}

fn normalize_text(value: &str) -> String {
    value
        .nfkc()
        .filter(|character| {
            !character.is_whitespace() && !matches!(character, ',' | '，' | '-' | '－' | '_' | '＿')
        })
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, key: &str, amount: i64) -> Record {
        Record::new(
            id,
            BTreeMap::from([
                ("key".into(), Cell::Text(key.into())),
                ("amount".into(), Cell::Number(Decimal::from(amount))),
            ]),
        )
    }

    #[test]
    fn unicode_compatibility_forms_share_one_normalized_key() {
        assert_eq!(normalize_text("ＡＣＭＥ－银行"), normalize_text("acme银行"));
        assert_eq!(normalize_text("Cafe\u{301}"), normalize_text("Café"));
        assert_eq!(similarity_score("发货经营分析", "发货经营报告"), 67);
    }

    #[test]
    fn exact_matching_is_order_independent_and_uses_tolerance() {
        let mut rule = ReconciliationRule {
            exact_keys: vec!["key".into()],
            compare_fields: vec!["amount".into()],
            amount_fields: BTreeSet::from(["amount".into()]),
            absolute_tolerance: Decimal::ONE,
            ..Default::default()
        };
        let left = vec![record("l2", "B", 20), record("l1", "A", 10)];
        let right = vec![record("r1", "A", 11), record("r2", "B", 30)];
        let first = reconcile(&left, &right, &rule).unwrap();
        rule.absolute_tolerance = Decimal::ZERO;
        let second = reconcile(
            &left.into_iter().rev().collect::<Vec<_>>(),
            &right.into_iter().rev().collect::<Vec<_>>(),
            &rule,
        )
        .unwrap();
        assert_eq!(first[0].status, ReconciliationStatus::Matched);
        assert_eq!(second[0].status, ReconciliationStatus::FieldMismatch);
    }

    #[test]
    fn duplicate_keys_are_never_automatically_paired() {
        let rule = ReconciliationRule {
            exact_keys: vec!["key".into()],
            ..Default::default()
        };
        let result = reconcile(
            &[record("l", "A", 1)],
            &[record("r1", "A", 1), record("r2", "A", 1)],
            &rule,
        )
        .unwrap();
        assert_eq!(result[0].status, ReconciliationStatus::DuplicateKey);
    }

    #[test]
    fn candidate_limit_is_enforced() {
        let rule = ReconciliationRule {
            fuzzy_fields: vec!["key".into()],
            max_candidate_edges: 0,
            ..Default::default()
        };
        let result = reconcile(&[record("l", "A", 1)], &[record("r", "A", 1)], &rule);
        assert_eq!(result, Err(ReconciliationError::CandidateLimit));
    }

    #[test]
    fn date_windows_and_fuzzy_scores_are_fixed_and_bounded() {
        let mut rule = ReconciliationRule {
            exact_keys: vec!["key".into()],
            compare_fields: vec!["date".into(), "name".into()],
            date_window_days: BTreeMap::from([("date".into(), 1)]),
            fuzzy_fields: vec!["name".into()],
            fuzzy_threshold: 80,
            ..Default::default()
        };
        let mut left = record("l", "A", 1);
        left.fields
            .insert("date".into(), Cell::Date("2026-08-01".into()));
        left.fields
            .insert("name".into(), Cell::Text("上海银行".into()));
        let mut right = record("r", "A", 1);
        right
            .fields
            .insert("date".into(), Cell::Date("2026-08-02".into()));
        right
            .fields
            .insert("name".into(), Cell::Text("上海银 行".into()));
        assert_eq!(
            reconcile(&[left], &[right], &rule).unwrap()[0].status,
            ReconciliationStatus::Matched
        );
        rule.date_window_days.insert("date".into(), 0);
        let mut left = record("l", "A", 1);
        left.fields
            .insert("date".into(), Cell::Date("2026-08-01".into()));
        left.fields
            .insert("name".into(), Cell::Text("上海银行".into()));
        let mut right = record("r", "A", 1);
        right
            .fields
            .insert("date".into(), Cell::Date("2026-08-02".into()));
        right
            .fields
            .insert("name".into(), Cell::Text("上海银行".into()));
        assert_eq!(
            reconcile(&[left], &[right], &rule).unwrap()[0].status,
            ReconciliationStatus::FieldMismatch
        );
    }

    #[test]
    fn manual_mappings_are_versioned_sorted_and_reject_duplicates() {
        let mappings = vec![
            ManualMapping {
                version: "1".into(),
                left_id: "b".into(),
                right_id: "2".into(),
                reason: "确认".into(),
            },
            ManualMapping {
                version: "1".into(),
                left_id: "a".into(),
                right_id: "1".into(),
                reason: "确认".into(),
            },
        ];
        let text = export_manual_mappings(&mappings).unwrap();
        let restored = import_manual_mappings(&text, "1").unwrap();
        assert_eq!(restored[0].left_id, "a");
        let duplicate = r#"[{"version":"1","left_id":"a","right_id":"1","reason":""},{"version":"1","left_id":"a","right_id":"1","reason":""}]"#;
        assert!(import_manual_mappings(duplicate, "1").is_err());
    }

    #[test]
    fn incompatible_units_are_a_rule_error_not_a_match() {
        let mut left = record("l", "A", 1);
        let mut right = record("r", "A", 1);
        left.metadata.unit = Some(crate::execution::UnitIdentity {
            name: "元".into(),
            dimension: "货币".into(),
            scale_numerator: 1,
            scale_denominator: 1,
            basis: None,
        });
        right.metadata.unit = Some(crate::execution::UnitIdentity {
            name: "万元".into(),
            dimension: "货币".into(),
            scale_numerator: 10_000,
            scale_denominator: 1,
            basis: None,
        });
        let rule = ReconciliationRule {
            exact_keys: vec!["key".into()],
            ..Default::default()
        };
        assert_eq!(
            reconcile(&[left], &[right], &rule).unwrap()[0].status,
            ReconciliationStatus::RuleError
        );
    }
}
