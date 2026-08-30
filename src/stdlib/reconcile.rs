use crate::reconciliation::{
    reconcile_with_cancellation, Cell, ReconciliationRule, ReconciliationStatus, Record,
};
use crate::runtime::Interpreter;
use crate::value::{Table, Value};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

pub(crate) fn reconcile_call(
    interpreter: &mut Interpreter,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    crate::contracts::validate_arity(name, args.len(), arity(name))?;
    match name {
        "比对" => {
            let rule = parse_rule(&args[2])?;
            let key_field = key_field(&args[2])?;
            let left = parse_records(&args[0], "左侧", key_field.as_deref(), &rule, interpreter)?;
            let right = parse_records(&args[1], "右侧", key_field.as_deref(), &rule, interpreter)?;
            let cancelled = interpreter.cancellation_flag();
            let rows = reconcile_with_cancellation(&left, &right, &rule, &|| {
                cancelled
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
            })
            .map_err(|error| error.to_string())?;
            let rows = rows
                .into_iter()
                .map(|row| {
                    BTreeMap::from([
                        ("状态".into(), Value::Text(status_name(row.status).into())),
                        ("左编号".into(), optional_text(row.left_id)),
                        ("右编号".into(), optional_text(row.right_id)),
                        (
                            "评分".into(),
                            row.score
                                .map(|score| Value::Number(score.into()))
                                .unwrap_or(Value::Null),
                        ),
                        ("规则版本".into(), Value::Text(row.rule_version)),
                        (
                            "差异".into(),
                            Value::list(
                                row.differences
                                    .into_iter()
                                    .map(|difference| {
                                        Value::record(BTreeMap::from([
                                            ("字段".into(), Value::Text(difference.field)),
                                            ("左值".into(), cell_to_value(difference.left)),
                                            ("右值".into(), cell_to_value(difference.right)),
                                            (
                                                "差额".into(),
                                                difference
                                                    .delta
                                                    .map(Value::Number)
                                                    .unwrap_or(Value::Null),
                                            ),
                                        ]))
                                    })
                                    .collect(),
                            ),
                        ),
                    ])
                })
                .collect();
            Ok(Value::Table(Rc::new(std::cell::RefCell::new(Table {
                rows,
                ..Default::default()
            }))))
        }
        "统计" => {
            let Value::Table(table) = &args[0] else {
                return Err("对账统计需要对账结果表".to_string());
            };
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for row in &table.borrow().rows {
                let status = row
                    .get("状态")
                    .ok_or_else(|| "对账结果表缺少“状态”列".to_string())?;
                *counts.entry(status.to_string()).or_default() += 1;
            }
            Ok(Value::record(
                counts
                    .into_iter()
                    .map(|(status, count)| (status, Value::Number(count.into())))
                    .collect(),
            ))
        }
        _ => Err(format!("对账模块不支持“{name}”")),
    }
}

fn arity(name: &str) -> Option<(usize, Option<usize>)> {
    match name {
        "比对" => Some((3, Some(3))),
        "统计" => Some((1, Some(1))),
        _ => None,
    }
}

fn status_name(status: ReconciliationStatus) -> &'static str {
    match status {
        ReconciliationStatus::Matched => "已匹配",
        ReconciliationStatus::LeftOnly => "仅左侧",
        ReconciliationStatus::RightOnly => "仅右侧",
        ReconciliationStatus::FieldMismatch => "字段不一致",
        ReconciliationStatus::DuplicateKey => "重复键",
        ReconciliationStatus::Suspected => "疑似匹配",
        ReconciliationStatus::Ambiguous => "多义",
        ReconciliationStatus::Undecided => "未判定",
        ReconciliationStatus::RuleError => "规则错误",
    }
}

fn optional_text(value: Option<String>) -> Value {
    value.map(Value::Text).unwrap_or(Value::Null)
}

fn cell_to_value(cell: Option<Cell>) -> Value {
    match cell {
        None | Some(Cell::Null) => Value::Null,
        Some(Cell::Text(text)) | Some(Cell::Date(text)) => Value::Text(text),
        Some(Cell::Number(number)) => Value::Number(number),
        Some(Cell::Bool(flag)) => Value::Bool(flag),
    }
}

fn key_field(rule: &Value) -> Result<Option<String>, String> {
    let Value::Record(record) = rule else {
        return Err("对账规则必须是记录".to_string());
    };
    Ok(match record.borrow().get("编号字段") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.to_string()),
    })
}

fn parse_rule(value: &Value) -> Result<ReconciliationRule, String> {
    let Value::Record(record) = value else {
        return Err("对账规则必须是记录".to_string());
    };
    let record = record.borrow();
    let names = |key: &str| -> Result<Vec<String>, String> {
        match record.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::List(values)) => Ok(values.borrow().iter().map(Value::to_string).collect()),
            Some(_) => Err(format!("对账规则“{key}”需要字段名列表")),
        }
    };
    let mut rule = ReconciliationRule {
        exact_keys: names("精确键")?,
        compare_fields: names("比较字段")?,
        amount_fields: names("金额字段")?.into_iter().collect::<BTreeSet<_>>(),
        fuzzy_fields: names("模糊字段")?,
        ..Default::default()
    };
    if let Some(version) = record.get("版本") {
        rule.version = version.to_string();
    }
    if let Some(tolerance) = record.get("绝对容差") {
        rule.absolute_tolerance = tolerance.as_decimal()?;
    }
    if let Some(tolerance) = record.get("相对容差") {
        rule.relative_tolerance = Some(tolerance.as_decimal()?);
    }
    if let Some(threshold) = record.get("模糊阈值") {
        rule.fuzzy_threshold =
            crate::contracts::decimal_to_usize(threshold.as_decimal()?, "对账模糊阈值")? as u32;
    }
    if let Some(limit) = record.get("候选上限") {
        rule.max_candidate_edges =
            crate::contracts::decimal_to_usize(limit.as_decimal()?, "对账候选上限")?;
    }
    match record.get("日期窗口") {
        None | Some(Value::Null) => {}
        Some(Value::Record(windows)) => {
            for (field, days) in windows.borrow().iter() {
                let days = crate::contracts::decimal_to_usize(days.as_decimal()?, "对账日期窗口")?;
                rule.date_window_days.insert(field.clone(), days as i64);
            }
        }
        Some(_) => return Err("对账规则“日期窗口”需要字段到天数的记录".to_string()),
    }
    Ok(rule)
}

fn parse_records(
    value: &Value,
    side: &str,
    key_field: Option<&str>,
    rule: &ReconciliationRule,
    interpreter: &Interpreter,
) -> Result<Vec<Record>, String> {
    let (rows, column_metadata) = match value {
        Value::Table(table) => {
            let table = table.borrow();
            (table.rows.clone(), table.column_metadata.clone())
        }
        Value::List(values) => {
            let mut rows = Vec::new();
            for (index, entry) in values.borrow().iter().enumerate() {
                let Value::Record(record) = entry else {
                    return Err(format!("{side}第 {} 项不是记录", index + 1));
                };
                rows.push(record.borrow().clone());
            }
            (rows, BTreeMap::new())
        }
        _ => return Err(format!("{side}需要表格或一组记录")),
    };
    if rows.len() > interpreter.permissions.collection_limit() {
        return Err("对账记录数量超过集合项数限制".to_string());
    }
    let mut records = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        let id = match key_field {
            Some(field) => row
                .get(field)
                .ok_or_else(|| format!("{side}缺少编号字段“{field}”"))?
                .to_string(),
            None => (index + 1).to_string(),
        };
        let mut fields = BTreeMap::new();
        for (name, cell) in &row {
            fields.insert(
                name.clone(),
                to_cell(cell, rule.date_window_days.contains_key(name))?,
            );
        }
        let mut record = Record::new(id, fields);
        record.field_metadata = column_metadata.clone();
        records.push(record);
    }
    Ok(records)
}

fn to_cell(value: &Value, as_date: bool) -> Result<Cell, String> {
    if as_date {
        return match value {
            Value::Null => Ok(Cell::Null),
            Value::DateTime(moment) => Ok(Cell::Date(moment.date_naive().to_string())),
            other => Ok(Cell::Date(other.to_string())),
        };
    }
    match value {
        Value::Null => Ok(Cell::Null),
        Value::Bool(flag) => Ok(Cell::Bool(*flag)),
        Value::Number(number) => Ok(Cell::Number(*number)),
        Value::Float(number) => Decimal::try_from(*number)
            .map(Cell::Number)
            .map_err(|_| "对账字段的浮点数无法表示为定点数".to_string()),
        Value::List(_) | Value::Record(_) | Value::Table(_) => {
            Err("对账字段不支持列表、记录或表格".to_string())
        }
        other => Ok(Cell::Text(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "{精确键: [\"凭证号\"], 比较字段: [\"金额\"], 金额字段: [\"金额\"], 绝对容差: 0.01, 编号字段: \"凭证号\"}";

    #[test]
    fn reconciliation_agrees_between_the_interpreter_and_the_vm() {
        let directory = tempfile::tempdir().unwrap();
        let source = format!(
            "引入 对账\n\
定义 流水 为 [{{凭证号: \"A1\", 金额: 100}}, {{凭证号: \"A2\", 金额: 200}}]\n\
定义 台账 为 [{{凭证号: \"A1\", 金额: 100}}, {{凭证号: \"A3\", 金额: 300}}]\n\
定义 结果 为 对账 比对(流水, 台账, {RULE})\n\
定义 统计 为 对账 统计(结果)\n\
输出(结果 的 行数)\n\
输出(统计 的 已匹配)\n\
输出(统计 的 仅左侧)\n\
输出(统计 的 仅右侧)"
        );
        let interpreted =
            crate::api::run_source(&source, crate::api::RunOptions::new(directory.path()))
                .unwrap()
                .output;
        let compiled =
            crate::api::run_compiled(&source, crate::api::RunOptions::new(directory.path()))
                .unwrap()
                .output;
        assert_eq!(interpreted, compiled);
        assert_eq!(interpreted, ["3", "1", "1", "1"]);
    }

    #[test]
    fn amount_differences_are_reported_with_a_delta() {
        let directory = tempfile::tempdir().unwrap();
        let source = format!(
            "引入 对账\n\
定义 结果 为 对账 比对([{{凭证号: \"A1\", 金额: 100}}], [{{凭证号: \"A1\", 金额: 130}}], {RULE})\n\
定义 行 为 (结果 转记录列表())[1]\n\
定义 差异 为 (行 的 差异)[1]\n\
输出(行 的 状态)\n\
输出(差异 的 字段)\n\
输出(差异 的 差额)"
        );
        let output = crate::api::run_source(&source, crate::api::RunOptions::new(directory.path()))
            .unwrap()
            .output;
        assert_eq!(output, ["字段不一致", "金额", "-30"]);
    }

    #[test]
    fn invalid_rules_and_records_are_rejected() {
        assert!(parse_rule(&Value::Text("规则".into())).is_err());
        assert!(parse_rule(&Value::record(BTreeMap::from([(
            "精确键".into(),
            Value::Number(1.into())
        )])))
        .is_err());
        assert!(parse_rule(&Value::record(BTreeMap::from([(
            "日期窗口".into(),
            Value::Number(1.into())
        )])))
        .is_err());
        assert!(to_cell(&Value::list(Vec::new()), false).is_err());
        assert_eq!(
            to_cell(&Value::Text("2026-01-02".into()), true).unwrap(),
            Cell::Date("2026-01-02".into())
        );
    }
}
