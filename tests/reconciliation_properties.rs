//! 对账属性测试：输入顺序不影响结果、并列得分保持歧义、候选爆炸和取消返回结构化错误、
//! 单位不一致不静默通过。

use hengshu::execution::{Metadata, UnitIdentity};
use hengshu::reconciliation::{
    reconcile, reconcile_with_cancellation, Cell, ReconciliationError, ReconciliationRule,
    ReconciliationStatus, Record,
};
use std::collections::BTreeMap;

fn record(id: &str, fields: &[(&str, &str)]) -> Record {
    Record::new(
        id,
        fields
            .iter()
            .map(|(name, value)| (name.to_string(), Cell::Text(value.to_string())))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn exact_rule() -> ReconciliationRule {
    ReconciliationRule {
        exact_keys: vec!["凭证号".into()],
        compare_fields: vec!["金额".into()],
        ..Default::default()
    }
}

fn statuses(rows: &[hengshu::reconciliation::ReconciliationRow]) -> Vec<(String, String)> {
    rows.iter()
        .map(|row| {
            (
                format!("{:?}", row.status),
                format!("{:?}/{:?}", row.left_id, row.right_id),
            )
        })
        .collect()
}

#[test]
fn input_order_does_not_change_the_result() {
    let rule = exact_rule();
    let left = vec![
        record("l1", &[("凭证号", "V1"), ("金额", "100")]),
        record("l2", &[("凭证号", "V2"), ("金额", "200")]),
        record("l3", &[("凭证号", "V3"), ("金额", "300")]),
    ];
    let right = vec![
        record("r1", &[("凭证号", "V1"), ("金额", "100")]),
        record("r3", &[("凭证号", "V3"), ("金额", "999")]),
        record("r9", &[("凭证号", "V9"), ("金额", "1")]),
    ];
    let expected = statuses(&reconcile(&left, &right, &rule).unwrap());

    let mut shuffled_left = left.clone();
    shuffled_left.reverse();
    let mut shuffled_right = right.clone();
    shuffled_right.rotate_left(2);
    let actual = statuses(&reconcile(&shuffled_left, &shuffled_right, &rule).unwrap());
    assert_eq!(actual, expected);
    assert!(expected.iter().any(|(status, _)| status == "FieldMismatch"));
    assert!(expected.iter().any(|(status, _)| status == "LeftOnly"));
    assert!(expected.iter().any(|(status, _)| status == "RightOnly"));
}

#[test]
fn tied_fuzzy_scores_stay_ambiguous_in_any_order() {
    let rule = ReconciliationRule {
        fuzzy_fields: vec!["名称".into()],
        fuzzy_threshold: 60,
        ..Default::default()
    };
    let left = vec![record("l1", &[("名称", "北京 建设 公司")])];
    let right = vec![
        record("r1", &[("名称", "北京建设公司")]),
        record("r2", &[("名称", "北京建设公司")]),
    ];
    let rows = reconcile(&left, &right, &rule).unwrap();
    assert!(
        rows.iter()
            .any(|row| row.status == ReconciliationStatus::Ambiguous),
        "{rows:?}"
    );
    assert!(!rows
        .iter()
        .any(|row| row.status == ReconciliationStatus::Matched));

    let mut reversed = right.clone();
    reversed.reverse();
    assert_eq!(
        statuses(&reconcile(&left, &reversed, &rule).unwrap()),
        statuses(&rows)
    );
}

#[test]
fn candidate_explosion_and_cancellation_return_structured_errors() {
    let rule = ReconciliationRule {
        fuzzy_fields: vec!["名称".into()],
        fuzzy_threshold: 10,
        max_candidate_edges: 1,
        ..Default::default()
    };
    let left = (0..4)
        .map(|index| record(&format!("l{index}"), &[("名称", "北京建设公司")]))
        .collect::<Vec<_>>();
    let right = (0..4)
        .map(|index| record(&format!("r{index}"), &[("名称", "北京建设公司")]))
        .collect::<Vec<_>>();
    assert_eq!(
        reconcile(&left, &right, &rule).unwrap_err(),
        ReconciliationError::CandidateLimit
    );

    let keyed_left = vec![record("l1", &[("凭证号", "V1"), ("金额", "100")])];
    let keyed_right = vec![record("r1", &[("凭证号", "V1"), ("金额", "100")])];
    let cancelled = reconcile_with_cancellation(&keyed_left, &keyed_right, &exact_rule(), &|| true);
    assert_eq!(cancelled.unwrap_err(), ReconciliationError::Cancelled);
    assert!(
        reconcile_with_cancellation(&keyed_left, &keyed_right, &exact_rule(), &|| false).is_ok()
    );
}

#[test]
fn incompatible_units_report_rule_error_instead_of_matching() {
    let rule = exact_rule();
    let unit = |name: &str, numerator: i64| Metadata {
        unit: Some(UnitIdentity {
            name: name.into(),
            dimension: "货币".into(),
            scale_numerator: numerator,
            scale_denominator: 1,
            basis: Some("CNY".into()),
        }),
        ..Default::default()
    };
    let mut left = record("l1", &[("凭证号", "V1"), ("金额", "100")]);
    left.field_metadata.insert("金额".into(), unit("元", 1));
    let mut right = record("r1", &[("凭证号", "V1"), ("金额", "100")]);
    right
        .field_metadata
        .insert("金额".into(), unit("万元", 10_000));

    let rows = reconcile(&[left], &[right], &rule).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, ReconciliationStatus::RuleError);
}
