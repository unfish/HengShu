use crate::value::{Grouped, Rows, Table, Value};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::rc::Rc;

pub fn grouped_method(grouped: &Grouped, name: &str, args: Vec<Value>) -> Result<Value, String> {
    let mut groups: BTreeMap<String, Vec<&BTreeMap<String, Value>>> = BTreeMap::new();
    for row in &grouped.rows {
        groups
            .entry(
                row.get(&grouped.column)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "空".into()),
            )
            .or_default()
            .push(row)
    }
    let col = args
        .first()
        .map(ToString::to_string)
        .unwrap_or_else(|| "计数".into());
    let mut rows = Vec::new();
    for (k, items) in groups {
        let mut row = BTreeMap::new();
        row.insert(grouped.column.clone(), Value::Text(k));
        match name {
            "计数" => {
                row.insert("计数".into(), Value::Number(Decimal::from(items.len())));
            }
            "求和" | "平均" | "平均值" => {
                let vals: Vec<_> = items
                    .iter()
                    .filter_map(|r| r.get(&col))
                    .filter_map(|v| v.as_decimal().ok())
                    .collect();
                let sum = checked_sum(vals.iter().copied())?;
                row.insert(
                    col.clone(),
                    Value::Number(if name == "求和" {
                        sum
                    } else if vals.is_empty() {
                        Decimal::ZERO
                    } else {
                        checked_div(sum, vals.len())?
                    }),
                );
            }
            _ => return Err(format!("分组不支持动作“{name}”。")),
        }
        rows.push(row)
    }
    Ok(table_value(rows))
}
pub(super) fn table_value(rows: Rows) -> Value {
    Value::Table(Rc::new(std::cell::RefCell::new(Table {
        rows,
        ..Default::default()
    })))
}

pub(super) fn checked_sum(values: impl IntoIterator<Item = Decimal>) -> Result<Decimal, String> {
    values.into_iter().try_fold(Decimal::ZERO, |sum, value| {
        sum.checked_add(value)
            .ok_or_else(|| "表格数字聚合溢出".to_string())
    })
}

pub(super) fn checked_div(value: Decimal, count: usize) -> Result<Decimal, String> {
    value
        .checked_div(Decimal::from(count))
        .ok_or_else(|| "表格数字平均值溢出".to_string())
}
fn numeric_columns(table: &Table) -> Vec<String> {
    table
        .rows
        .first()
        .map(|r| {
            r.keys()
                .filter(|k| {
                    table
                        .rows
                        .iter()
                        .any(|row| row.get(*k).is_some_and(|v| v.as_f64().is_ok()))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}
pub(super) fn group_table(
    table: &Table,
    cols: &[String],
    operation: &str,
) -> Result<Value, String> {
    let mut groups: BTreeMap<Vec<String>, Vec<&BTreeMap<String, Value>>> = BTreeMap::new();
    for row in &table.rows {
        groups
            .entry(
                cols.iter()
                    .map(|c| {
                        row.get(c)
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "空".into())
                    })
                    .collect(),
            )
            .or_default()
            .push(row)
    }
    let numeric = numeric_columns(table);
    let mut rows = Vec::new();
    for (key, items) in groups {
        let mut row = BTreeMap::new();
        for (c, v) in cols.iter().zip(key) {
            row.insert(c.clone(), Value::Text(v));
        }
        for col in &numeric {
            if cols.contains(col) {
                continue;
            }
            let values = items
                .iter()
                .filter_map(|r| r.get(col))
                .filter_map(|v| v.as_f64().ok())
                .collect::<Vec<_>>();
            if values.is_empty() {
                continue;
            }
            let result = match operation {
                "求和" => values.iter().sum(),
                "计数" => values.len() as f64,
                "最大值" => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                "最小值" => values.iter().copied().fold(f64::INFINITY, f64::min),
                "平均" | "平均值" => values.iter().sum::<f64>() / values.len() as f64,
                _ => return Err(format!("未知聚合方式“{operation}”")),
            };
            row.insert(col.clone(), Value::Float(result));
        }
        rows.push(row)
    }
    Ok(table_value(rows))
}
pub(super) fn correlation_table(table: &Table) -> Result<Value, String> {
    let cols = numeric_columns(table);
    let mut rows = Vec::new();
    for a in &cols {
        let mut row = BTreeMap::new();
        row.insert("列".into(), Value::Text(a.clone()));
        for b in &cols {
            let pairs = table
                .rows
                .iter()
                .filter_map(|r| Some((r.get(a)?.as_f64().ok()?, r.get(b)?.as_f64().ok()?)))
                .collect::<Vec<_>>();
            if pairs.len() < 2 {
                return Err(format!("列“{a}”与“{b}”至少需要两个有效数据对"));
            }
            let (xs, ys): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
            let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
            let (mx, my) = (mean(&xs), mean(&ys));
            let cov = xs
                .iter()
                .zip(&ys)
                .map(|(x, y)| (x - mx) * (y - my))
                .sum::<f64>();
            let sx = xs.iter().map(|x| (x - mx).powi(2)).sum::<f64>().sqrt();
            let sy = ys.iter().map(|y| (y - my).powi(2)).sum::<f64>().sqrt();
            if !sx.is_finite() || !sy.is_finite() || sx <= f64::EPSILON || sy <= f64::EPSILON {
                return Err(format!("列“{a}”或“{b}”方差为零，无法计算相关系数"));
            }
            row.insert(b.clone(), Value::Float(cov / (sx * sy)));
        }
        rows.push(row)
    }
    Ok(table_value(rows))
}
pub(super) fn describe_table(table: &Table) -> Result<Value, String> {
    let mut rows = Vec::new();
    for col in numeric_columns(table) {
        let mut values = table
            .rows
            .iter()
            .filter_map(|r| r.get(&col))
            .filter_map(|v| v.as_f64().ok())
            .collect::<Vec<_>>();
        if values.is_empty() {
            continue;
        }
        values.sort_by(f64::total_cmp);
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let std =
            (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt();
        let minimum = values
            .first()
            .copied()
            .ok_or_else(|| "表格描述统计缺少有效数据".to_string())?;
        let maximum = values
            .last()
            .copied()
            .ok_or_else(|| "表格描述统计缺少有效数据".to_string())?;
        if !mean.is_finite() || !std.is_finite() || !minimum.is_finite() || !maximum.is_finite() {
            return Err("表格描述统计产生了非有限结果".into());
        }
        rows.push(BTreeMap::from([
            ("列".into(), Value::Text(col)),
            ("数量".into(), Value::Float(values.len() as f64)),
            ("平均值".into(), Value::Float(mean)),
            ("标准差".into(), Value::Float(std)),
            ("最小值".into(), Value::Float(minimum)),
            ("中位数".into(), Value::Float(values[values.len() / 2])),
            ("最大值".into(), Value::Float(maximum)),
        ]));
    }
    Ok(table_value(rows))
}
