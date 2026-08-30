use super::{checked_div, checked_sum, table_value};
use crate::contracts::decimal_to_usize;
use crate::value::{Table, Value};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn compare_table_values(
    left: Option<&Value>,
    right: Option<&Value>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (None | Some(Value::Null), None | Some(Value::Null)) => Ordering::Equal,
        (None | Some(Value::Null), _) => Ordering::Greater,
        (_, None | Some(Value::Null)) => Ordering::Less,
        (Some(Value::Number(a)), Some(Value::Number(b))) => a.cmp(b),
        (Some(Value::Float(a)), Some(Value::Float(b))) => crate::value::float_order(*a, *b),
        (Some(Value::Number(a)), Some(Value::Float(b))) => decimal_order(a, b),
        (Some(Value::Float(a)), Some(Value::Number(b))) => decimal_order(b, a).reverse(),
        // 表格排序必须是全序：同币种按数额、跨币种先按币种代码，都不退化为显示文本。
        (Some(Value::Money(a)), Some(Value::Money(b))) => crate::value::money_order(a, b)
            .unwrap_or_else(|| a.currency.cmp(&b.currency).then(a.amount.cmp(&b.amount))),
        (Some(Value::Money(a)), Some(Value::Number(b))) => a.amount.cmp(b),
        (Some(Value::Number(a)), Some(Value::Money(b))) => a.cmp(&b.amount),
        (Some(Value::Money(a)), Some(Value::Float(b))) => decimal_order(&a.amount, b),
        (Some(Value::Float(a)), Some(Value::Money(b))) => decimal_order(&b.amount, a).reverse(),
        (Some(Value::DateTime(a)), Some(Value::DateTime(b))) => a.cmp(b),
        (Some(Value::Duration(a)), Some(Value::Duration(b))) => a.cmp(b),
        (Some(Value::Text(a)), Some(Value::Text(b))) => a.cmp(b),
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => a.cmp(b),
        (Some(a), Some(b)) => value_sort_rank(a)
            .cmp(&value_sort_rank(b))
            .then_with(|| a.to_string().cmp(&b.to_string())),
    }
}

/// 表格里数字与浮点数混排时按同一规范映射（浮点数取最短往返十进制表示）比较，
/// 与`转数字`的转换规则一致；无法规范化的非有限浮点数排在数值之后。
fn decimal_order(number: &Decimal, float: &f64) -> std::cmp::Ordering {
    match Decimal::from_str_exact(&float.to_string()) {
        Ok(other) => number.cmp(&other),
        Err(_) => std::cmp::Ordering::Less,
    }
}

fn value_sort_rank(value: &Value) -> u8 {
    match value {
        Value::Number(_) | Value::Float(_) | Value::Money(_) => 0,
        Value::DateTime(_) | Value::Duration(_) => 1,
        Value::Text(_) | Value::Path(_) => 2,
        Value::Bool(_) => 3,
        _ => 4,
    }
}

#[cfg(test)]
pub(super) fn join_tables(
    left: &Table,
    right: &Table,
    keys: &[String],
    kind: &str,
) -> Result<Value, String> {
    join_tables_limited(
        left,
        right,
        keys,
        kind,
        crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
    )
}

pub(super) fn join_tables_limited(
    left: &Table,
    right: &Table,
    keys: &[String],
    kind: &str,
    collection_limit: usize,
) -> Result<Value, String> {
    if keys.is_empty() {
        return Err("连接至少需要一个键".to_string());
    }
    let key_of = |row: &BTreeMap<String, Value>| {
        keys.iter()
            .map(|key| row.get(key).map(ToString::to_string).unwrap_or_default())
            .collect::<Vec<_>>()
    };
    type IndexedRows<'a> = BTreeMap<Vec<String>, Vec<(usize, &'a BTreeMap<String, Value>)>>;
    let mut right_index: IndexedRows<'_> = BTreeMap::new();
    for (index, row) in right.rows.iter().enumerate() {
        right_index
            .entry(key_of(row))
            .or_default()
            .push((index, row));
    }
    let mut matched_right = BTreeSet::new();
    let mut rows = Vec::new();
    for left_row in &left.rows {
        if let Some(right_rows) = right_index.get(&key_of(left_row)) {
            for (index, right_row) in right_rows {
                matched_right.insert(*index);
                ensure_output_capacity(&rows, collection_limit, "表格连接")?;
                rows.push(merge_rows(left_row, right_row, keys));
            }
        } else if matches!(kind, "左连接" | "全连接") {
            ensure_output_capacity(&rows, collection_limit, "表格连接")?;
            rows.push(left_row.clone());
        }
    }
    if matches!(kind, "右连接" | "全连接") {
        for (index, right_row) in right.rows.iter().enumerate() {
            if !matched_right.contains(&index) {
                ensure_output_capacity(&rows, collection_limit, "表格连接")?;
                rows.push(right_row.clone());
            }
        }
    }
    Ok(table_value(rows))
}

fn merge_rows(
    left: &BTreeMap<String, Value>,
    right: &BTreeMap<String, Value>,
    keys: &[String],
) -> BTreeMap<String, Value> {
    let mut row = left.clone();
    for (key, value) in right {
        if keys.contains(key) {
            continue;
        }
        let output = if row.contains_key(key) {
            format!("{key}_右")
        } else {
            key.clone()
        };
        row.insert(output, value.clone());
    }
    row
}

pub(super) fn pivot_table(
    table: &Table,
    args: &[Value],
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    if args.len() < 3 {
        return Err("透视需要索引列、展开列和值列".to_string());
    }
    let index = args[0].to_string();
    let column = args[1].to_string();
    let value = args[2].to_string();
    let operation = named
        .get("聚合")
        .map(ToString::to_string)
        .unwrap_or_else(|| "求和".into());
    let mut groups: BTreeMap<String, BTreeMap<String, Vec<Decimal>>> = BTreeMap::new();
    for row in &table.rows {
        let index_value = row.get(&index).map(ToString::to_string).unwrap_or_default();
        let column_value = row
            .get(&column)
            .map(ToString::to_string)
            .unwrap_or_default();
        if let Some(number) = row.get(&value).and_then(|value| value.as_decimal().ok()) {
            groups
                .entry(index_value)
                .or_default()
                .entry(column_value)
                .or_default()
                .push(number);
        }
    }
    let rows = groups
        .into_iter()
        .map(|(index_value, columns)| {
            let mut row = BTreeMap::from([(index.clone(), Value::Text(index_value))]);
            for (column, values) in columns {
                let total = checked_sum(values.iter().copied())?;
                let result = if operation == "平均值" && !values.is_empty() {
                    checked_div(total, values.len())?
                } else {
                    total
                };
                row.insert(column, Value::Number(result));
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(table_value(rows))
}

#[cfg(test)]
pub(super) fn unpivot_table(table: &Table, args: &[Value]) -> Result<Value, String> {
    unpivot_table_limited(table, args, crate::resource::DEFAULT_MAX_COLLECTION_ITEMS)
}

pub(super) fn unpivot_table_limited(
    table: &Table,
    args: &[Value],
    collection_limit: usize,
) -> Result<Value, String> {
    if args.is_empty() {
        return Err("反透视需要保留列".to_string());
    }
    let ids = list_or_single(&args[0], collection_limit)?
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let selected = args
        .get(1)
        .map(|value| list_or_single(value, collection_limit))
        .transpose()?
        .map(|values| values.iter().map(ToString::to_string).collect::<Vec<_>>());
    let mut rows = Vec::new();
    for source in &table.rows {
        for (column, value) in source {
            if ids.contains(column)
                || selected
                    .as_ref()
                    .is_some_and(|selected| !selected.contains(column))
            {
                continue;
            }
            let mut row = ids
                .iter()
                .map(|id| (id.clone(), source.get(id).cloned().unwrap_or(Value::Null)))
                .collect::<BTreeMap<_, _>>();
            row.insert("变量".into(), Value::Text(column.clone()));
            row.insert("值".into(), value.clone());
            ensure_output_capacity(&rows, collection_limit, "表格反透视")?;
            rows.push(row);
        }
    }
    Ok(table_value(rows))
}

fn list_or_single(value: &Value, collection_limit: usize) -> Result<Vec<Value>, String> {
    match value {
        Value::List(_) | Value::Set(_) | Value::NumericArray(_) | Value::Range(_) => {
            value.iter_values_limited(collection_limit)
        }
        _ => Ok(vec![value.clone()]),
    }
}

fn ensure_output_capacity(
    rows: &[BTreeMap<String, Value>],
    collection_limit: usize,
    operation: &str,
) -> Result<(), String> {
    if rows.len() >= collection_limit {
        Err(format!("{operation}结果超过集合项数限制"))
    } else {
        Ok(())
    }
}

pub(super) fn rank_table(
    table: &Table,
    args: &[Value],
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let column = args
        .first()
        .ok_or_else(|| "排名缺少列名".to_string())?
        .to_string();
    let output = named
        .get("结果列")
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{column}排名"));
    let descending = named
        .get("顺序")
        .is_none_or(|value| value.to_string() == "降序");
    let mut order = table
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            (
                index,
                row.get(&column)
                    .and_then(|value| value.as_f64().ok())
                    .unwrap_or(f64::NAN),
            )
        })
        .collect::<Vec<_>>();
    order.sort_by(|left, right| {
        let order = left.1.total_cmp(&right.1);
        if descending {
            order.reverse()
        } else {
            order
        }
    });
    let mut ranks = vec![0; table.rows.len()];
    for (rank, (index, _)) in order.into_iter().enumerate() {
        ranks[index] = rank + 1;
    }
    Ok(table_value(
        table
            .rows
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, mut row)| {
                row.insert(output.clone(), Value::Number(Decimal::from(ranks[index])));
                row
            })
            .collect(),
    ))
}

pub(super) fn rolling_table(
    table: &Table,
    args: &[Value],
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("滚动统计需要列名和窗口大小".to_string());
    }
    let column = args[0].to_string();
    let window = decimal_to_usize(args[1].as_decimal()?, "窗口大小")?.max(1);
    let operation = named
        .get("聚合")
        .map(ToString::to_string)
        .unwrap_or_else(|| "平均值".into());
    let output = named
        .get("结果列")
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{column}滚动{operation}"));
    let values = table
        .rows
        .iter()
        .map(|row| row.get(&column).and_then(|value| value.as_decimal().ok()))
        .collect::<Vec<_>>();
    let rows = table
        .rows
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, mut row)| {
            let start = index.saturating_add(1).saturating_sub(window);
            let items = values[start..=index]
                .iter()
                .filter_map(|value| *value)
                .collect::<Vec<_>>();
            let total = checked_sum(items.iter().copied())?;
            let result = if operation == "求和" || items.is_empty() {
                total
            } else {
                checked_div(total, items.len())?
            };
            row.insert(output.clone(), Value::Number(result));
            Ok(row)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(table_value(rows))
}

pub(super) fn resample_table(
    table: &Table,
    args: &[Value],
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("时间重采样需要日期列和值列".to_string());
    }
    let date_column = args[0].to_string();
    let value_column = args[1].to_string();
    let frequency = named
        .get("频率")
        .map(ToString::to_string)
        .unwrap_or_else(|| "月".into());
    let mut groups: BTreeMap<String, Vec<Decimal>> = BTreeMap::new();
    for row in &table.rows {
        let date = row
            .get(&date_column)
            .map(ToString::to_string)
            .unwrap_or_default();
        let key = match frequency.as_str() {
            "年" => date.get(..4).unwrap_or(&date),
            "月" => date.get(..7).unwrap_or(&date),
            _ => &date,
        };
        if let Some(value) = row
            .get(&value_column)
            .and_then(|value| value.as_decimal().ok())
        {
            groups.entry(key.into()).or_default().push(value);
        }
    }
    Ok(table_value(
        groups
            .into_iter()
            .map(|(period, values)| {
                Ok(BTreeMap::from([
                    ("期间".into(), Value::Text(period)),
                    (value_column.clone(), Value::Number(checked_sum(values)?)),
                ]))
            })
            .collect::<Result<Vec<_>, String>>()?,
    ))
}

pub(super) fn validate_table(table: &Table, args: &[Value]) -> Result<Value, String> {
    let Some(Value::Record(schema)) = args.first() else {
        return Err("校验需要 schema 记录".to_string());
    };
    let mut issues = Vec::new();
    for (column, rules) in schema.borrow().iter() {
        let Value::Record(rules) = rules else {
            continue;
        };
        let rules = rules.borrow();
        if let Some(expected) = rules.get("单位") {
            let Some(actual) = table
                .column_metadata
                .get(column)
                .and_then(|metadata| metadata.unit.as_ref())
            else {
                issues.push(Value::record(BTreeMap::from([
                    ("行".into(), Value::Number(Decimal::ZERO)),
                    ("列".into(), Value::Text(column.clone())),
                    ("信息".into(), Value::Text("缺少单位/口径元数据".into())),
                ])));
                continue;
            };
            if actual.name != expected.to_string() {
                issues.push(Value::record(BTreeMap::from([
                    ("行".into(), Value::Number(Decimal::ZERO)),
                    ("列".into(), Value::Text(column.clone())),
                    (
                        "信息".into(),
                        Value::Text(format!("单位要求是{}，实际是{}", expected, actual.name)),
                    ),
                ])));
            }
        }
        let mut seen = BTreeSet::new();
        for (index, row) in table.rows.iter().enumerate() {
            let value = row.get(column).unwrap_or(&Value::Null);
            if rules.get("非空").is_some_and(Value::truth) && matches!(value, Value::Null) {
                issues.push(Value::record(BTreeMap::from([
                    ("行".into(), Value::Number(Decimal::from(index + 1))),
                    ("列".into(), Value::Text(column.clone())),
                    ("信息".into(), Value::Text("不能为空".into())),
                ])));
            }
            if let Some(expected) = rules.get("类型") {
                if !matches!(value, Value::Null) && value.type_name() != expected.to_string() {
                    issues.push(Value::record(BTreeMap::from([
                        ("行".into(), Value::Number(Decimal::from(index + 1))),
                        ("列".into(), Value::Text(column.clone())),
                        ("信息".into(), Value::Text(format!("要求是{}", expected))),
                    ])));
                }
            }
            if rules.get("唯一").is_some_and(Value::truth) && !seen.insert(value.to_string()) {
                issues.push(Value::record(BTreeMap::from([
                    ("行".into(), Value::Number(Decimal::from(index + 1))),
                    ("列".into(), Value::Text(column.clone())),
                    ("信息".into(), Value::Text("值不唯一".into())),
                ])));
            }
        }
    }
    Ok(Value::record(BTreeMap::from([
        ("通过".into(), Value::Bool(issues.is_empty())),
        ("问题".into(), Value::list(issues)),
    ])))
}
