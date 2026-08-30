use crate::analysis::{error, finite_float};
use crate::value::{Table, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

fn table(value: &Value) -> Result<std::cell::Ref<'_, Table>, String> {
    match value {
        Value::Table(value) => Ok(value.borrow()),
        _ => Err(error("经营分析需要表格对象")),
    }
}

fn text(named: &BTreeMap<String, Value>, name: &str) -> Result<String, String> {
    named
        .get(name)
        .map(ToString::to_string)
        .ok_or_else(|| error(format!("经营分析缺少“{name}”参数")))
}

fn optional_text(named: &BTreeMap<String, Value>, name: &str) -> Option<String> {
    named.get(name).map(ToString::to_string)
}

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|value| value.as_f64().ok())
        .filter(|v| v.is_finite())
}

fn result(
    method: &str,
    metrics: BTreeMap<String, Value>,
    rows: Vec<BTreeMap<String, Value>>,
    warnings: Vec<String>,
) -> Value {
    result_with(
        method,
        metrics,
        rows,
        warnings,
        crate::execution::Metadata::default(),
        BTreeMap::new(),
    )
}

/// 与 [`result`] 相同，但保留来源表的单位/来源/敏感标签，并允许附加规范化章节。
/// 元数据必须随结果传播：聚合和概览不得把敏感标签洗掉，否则导出就绕过了授权。
fn result_with(
    method: &str,
    metrics: BTreeMap<String, Value>,
    rows: Vec<BTreeMap<String, Value>>,
    warnings: Vec<String>,
    metadata: crate::execution::Metadata,
    extras: BTreeMap<String, Value>,
) -> Value {
    let mut record = BTreeMap::from([
        ("类型".to_string(), Value::Text("分析结果".into())),
        ("方法".to_string(), Value::Text(method.into())),
        ("指标".to_string(), Value::record(metrics)),
        (
            "数据".to_string(),
            Value::Table(std::rc::Rc::new(std::cell::RefCell::new(Table {
                rows,
                metadata,
                ..Default::default()
            }))),
        ),
        (
            // 警告合并成标量文本，和统计检验结果保持一致，也让结果能无损写进 CSV。
            "警告".to_string(),
            if warnings.is_empty() {
                Value::Null
            } else {
                Value::Text(warnings.join("；"))
            },
        ),
    ]);
    record.extend(extras);
    Value::record(record)
}

/// 规范化表格：单元格是标量，可以直接被报告列规范驱动并无损导出。
fn normalized_table(rows: Vec<BTreeMap<String, Value>>) -> Value {
    Value::Table(std::rc::Rc::new(std::cell::RefCell::new(Table {
        rows,
        ..Default::default()
    })))
}

pub(super) fn data_profile(
    value: &Value,
    named: &BTreeMap<String, Value>,
    limit: usize,
) -> Result<Value, String> {
    let table = table(value)?;
    let columns = table
        .rows
        .iter()
        .flat_map(|row| row.keys().cloned())
        .collect::<BTreeSet<_>>();
    let top_n = named
        .get("TopN")
        .map(|v| crate::analysis::value_usize(v, "TopN"))
        .transpose()?
        .unwrap_or(10)
        .min(limit);
    if top_n == 0 {
        return Err(error("TopN必须大于零"));
    }
    let mut rows = Vec::new();
    let mut frequency_rows = Vec::new();
    let mut warnings = Vec::new();
    let mut truncated_columns = 0usize;
    let mut sensitive_columns = 0usize;
    let mut anomaly_total = 0usize;
    for column in columns {
        let mut missing = 0usize;
        let mut numeric = Vec::new();
        let mut frequencies = BTreeMap::<String, usize>::new();
        for row in &table.rows {
            let value = row.get(&column);
            if value.is_none() || matches!(value, Some(Value::Null)) {
                missing += 1;
                continue;
            }
            let value = value.expect("checked above");
            if let Some(value) = number(Some(value)) {
                numeric.push(value);
            }
            *frequencies.entry(value.to_string()).or_default() += 1;
        }
        numeric.sort_by(f64::total_cmp);
        let type_name = table
            .rows
            .iter()
            .filter_map(|r| r.get(&column))
            .find(|v| !matches!(v, Value::Null))
            .map(Value::type_name)
            .unwrap_or("空");
        let unique_values = frequencies.len();
        let valid = table.rows.len() - missing;
        let mut top = frequencies.into_iter().collect::<Vec<_>>();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        // 高基数列只保留预算内 Top N，但必须显式标记截断和覆盖率，不能静默丢类别。
        let truncated = unique_values > top_n;
        top.truncate(top_n);
        let shown = top.iter().map(|(_, count)| *count).sum::<usize>();
        let coverage = if valid == 0 {
            0.0
        } else {
            shown as f64 / valid as f64
        };
        for (rank, (label, count)) in top.iter().enumerate() {
            frequency_rows.push(BTreeMap::from([
                ("列".to_string(), Value::Text(column.clone())),
                ("排名".to_string(), Value::Float((rank + 1) as f64)),
                ("值".to_string(), Value::Text(label.clone())),
                ("数量".to_string(), Value::Float(*count as f64)),
                (
                    "占比".to_string(),
                    Value::Float(if valid == 0 {
                        0.0
                    } else {
                        *count as f64 / valid as f64
                    }),
                ),
            ]));
        }
        if truncated {
            warnings.push(format!(
                "列“{column}”有 {unique_values} 个唯一值，高频值只展示前 {top_n} 个"
            ));
        }
        let column_metadata = table.column_metadata.get(&column);
        let unit = column_metadata
            .and_then(|metadata| metadata.unit.as_ref())
            .map(|unit| Value::Text(unit.name.clone()))
            .unwrap_or(Value::Null);
        let labels = column_metadata
            .map(|metadata| {
                metadata
                    .sensitivity
                    .labels
                    .iter()
                    .map(crate::table::sensitive_label_name)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut row = BTreeMap::from([
            ("列".to_string(), Value::Text(column.clone())),
            ("类型".to_string(), Value::Text(type_name.into())),
            ("数量".to_string(), Value::Float(table.rows.len() as f64)),
            ("有效数量".to_string(), Value::Float(valid as f64)),
            ("缺失数量".to_string(), Value::Float(missing as f64)),
            (
                "缺失率".to_string(),
                Value::Float(if table.rows.is_empty() {
                    0.0
                } else {
                    missing as f64 / table.rows.len() as f64
                }),
            ),
            ("唯一值".to_string(), Value::Float(unique_values as f64)),
            (
                // 高频值是标量文本，完整明细放在“高频明细”规范化表里，避免嵌套单元格。
                "高频值".to_string(),
                if top.is_empty() {
                    Value::Null
                } else {
                    Value::Text(
                        top.iter()
                            .map(|(label, count)| format!("{label}={count}"))
                            .collect::<Vec<_>>()
                            .join("；"),
                    )
                },
            ),
            ("高频值截断".to_string(), Value::Bool(truncated)),
            ("高频值覆盖率".to_string(), Value::Float(coverage)),
            ("单位".to_string(), unit),
            (
                "敏感标签".to_string(),
                if labels.is_empty() {
                    Value::Null
                } else {
                    Value::Text(labels.join("、"))
                },
            ),
        ]);
        if let Some(minimum) = numeric.first() {
            row.insert("最小值".into(), Value::Float(*minimum));
        }
        if let Some(maximum) = numeric.last() {
            row.insert("最大值".into(), Value::Float(*maximum));
        }
        if !numeric.is_empty() {
            let mean = numeric.iter().sum::<f64>() / numeric.len() as f64;
            let variance =
                numeric.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / numeric.len() as f64;
            let q1 = quantile(&numeric, 0.25);
            let q3 = quantile(&numeric, 0.75);
            // 四分位距法：落在 [Q1-1.5IQR, Q3+1.5IQR] 之外的算异常，只统计不删除。
            let spread = q3 - q1;
            let low = q1 - 1.5 * spread;
            let high = q3 + 1.5 * spread;
            let outliers = numeric
                .iter()
                .filter(|value| **value < low || **value > high)
                .count();
            row.insert("平均值".into(), finite_float(mean, "数据概览平均值")?);
            row.insert(
                "标准差".into(),
                finite_float(variance.sqrt(), "数据概览标准差")?,
            );
            row.insert("P25".into(), Value::Float(q1));
            row.insert("中位数".into(), Value::Float(quantile(&numeric, 0.5)));
            row.insert("P75".into(), Value::Float(q3));
            row.insert("四分位距".into(), Value::Float(spread));
            row.insert("异常数量".into(), Value::Float(outliers as f64));
            row.insert(
                "异常率".into(),
                Value::Float(outliers as f64 / numeric.len() as f64),
            );
            if outliers > 0 {
                anomaly_total += outliers;
            }
        }
        if truncated {
            truncated_columns += 1;
        }
        if !labels.is_empty() {
            sensitive_columns += 1;
        }
        rows.push(row);
    }
    let duplicate_count = table.rows.len().saturating_sub(unique_rows(&table.rows));
    if duplicate_count > 0 {
        warnings.push(format!("存在 {duplicate_count} 行完全重复的记录"));
    }
    let metrics = BTreeMap::from([
        ("行数".to_string(), Value::Float(table.rows.len() as f64)),
        ("列数".to_string(), Value::Float(rows.len() as f64)),
        ("重复行".to_string(), Value::Float(duplicate_count as f64)),
        (
            "截断列数".to_string(),
            Value::Float(truncated_columns as f64),
        ),
        ("异常值总数".to_string(), Value::Float(anomaly_total as f64)),
        (
            "敏感列数".to_string(),
            Value::Float(sensitive_columns as f64),
        ),
        (
            "来源数".to_string(),
            Value::Float(table.metadata.provenance.len() as f64),
        ),
    ]);
    let descriptions = [
        (
            "缺失率",
            "该列为空的比例；高缺失率会让均值和分位数失去代表性。",
        ),
        ("唯一值", "该列不同取值的个数；接近行数说明近似主键。"),
        (
            "高频值",
            "按出现次数排序的前若干取值；完整明细见“高频明细”。",
        ),
        (
            "高频值截断",
            "为真表示唯一值超过 TopN，高频值只展示了一部分，不是全部类别。",
        ),
        ("高频值覆盖率", "已展示的高频值占该列有效数据的比例。"),
        (
            "异常数量",
            "落在 [P25−1.5×四分位距, P75+1.5×四分位距] 之外的数量；只统计，不删除数据。",
        ),
        ("单位", "该列的单位/口径；空表示来源未声明单位。"),
        ("敏感标签", "该列携带的敏感标签；导出和外发时需要对应授权。"),
    ];
    Ok(result_with(
        "数据概览",
        metrics,
        rows,
        warnings,
        // 概览结果继承来源表的单位、来源和敏感标签，导出时仍会触发同一套授权检查。
        table.metadata.clone(),
        BTreeMap::from([
            ("高频明细".to_string(), normalized_table(frequency_rows)),
            (
                "字段说明".to_string(),
                normalized_table(
                    descriptions
                        .into_iter()
                        .map(|(field, meaning)| {
                            BTreeMap::from([
                                ("字段".to_string(), Value::Text(field.into())),
                                ("含义".to_string(), Value::Text(meaning.into())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]),
    ))
}

fn unique_rows(rows: &[BTreeMap<String, Value>]) -> usize {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|(k, v)| format!("{k}={v};"))
                .collect::<String>()
        })
        .collect::<HashSet<_>>()
        .len()
}

fn quantile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let position = p * (values.len() - 1) as f64;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    values[low] + (values[high] - values[low]) * (position - low as f64)
}

pub(super) fn period_compare(
    value: &Value,
    named: &BTreeMap<String, Value>,
    limit: usize,
) -> Result<Value, String> {
    let table = table(value)?;
    let period_column = text(named, "周期列")?;
    let metric_column = text(named, "指标列")?;
    let group_column = optional_text(named, "分组列");
    let mut aggregates = BTreeMap::<(String, String), f64>::new();
    for row in &table.rows {
        let period = row
            .get(&period_column)
            .map(ToString::to_string)
            .ok_or_else(|| error(format!("表格不存在周期列“{period_column}”")))?;
        let Some(metric) = number(row.get(&metric_column)) else {
            continue;
        };
        let group = group_column
            .as_ref()
            .map(|c| {
                row.get(c)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "空".into())
            })
            .unwrap_or_default();
        let entry = aggregates.entry((period, group)).or_default();
        *entry += metric;
        if !entry.is_finite() {
            return Err(error("期间比较聚合溢出"));
        }
    }
    let periods = aggregates
        .keys()
        .map(|(p, _)| p.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (index, period) in periods.iter().enumerate() {
        let previous = index.checked_sub(1).and_then(|i| periods.get(i));
        let groups = aggregates
            .keys()
            .filter(|(p, _)| p == period)
            .map(|(_, g)| g.clone())
            .collect::<BTreeSet<_>>();
        for group in groups {
            let current = aggregates
                .get(&(period.clone(), group.clone()))
                .copied()
                .unwrap_or(0.0);
            let previous_value =
                previous.and_then(|p| aggregates.get(&(p.clone(), group.clone())).copied());
            let change = previous_value
                .map(|v| current - v)
                .map(Value::Float)
                .unwrap_or(Value::Null);
            let rate = previous_value
                .filter(|v| v.abs() > f64::EPSILON)
                .map(|v| Value::Float((current - v) / v.abs()))
                .unwrap_or(Value::Null);
            let mut row = BTreeMap::from([
                ("周期".into(), Value::Text(period.clone())),
                ("本期".into(), Value::Float(current)),
                (
                    "上期".into(),
                    previous_value.map(Value::Float).unwrap_or(Value::Null),
                ),
                ("变化量".into(), change),
                ("变化率".into(), rate),
            ]);
            if group_column.is_some() {
                row.insert("分组".into(), Value::Text(group));
            }
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }
    }
    Ok(result(
        "期间比较",
        BTreeMap::from([("周期数".into(), Value::Float(periods.len() as f64))]),
        rows,
        Vec::new(),
    ))
}

pub(super) fn contribution(
    value: &Value,
    named: &BTreeMap<String, Value>,
    limit: usize,
    pareto: bool,
) -> Result<Value, String> {
    let table = table(value)?;
    let category = text(named, "分类列")?;
    let metric = text(named, "指标列")?;
    let mut totals = BTreeMap::<String, f64>::new();
    for row in &table.rows {
        let category_value = row
            .get(&category)
            .map(ToString::to_string)
            .unwrap_or_else(|| "空".into());
        if let Some(metric_value) = number(row.get(&metric)) {
            *totals.entry(category_value).or_default() += metric_value;
        }
    }
    let total = totals.values().sum::<f64>();
    let mut ordered = totals.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut cumulative = 0.0;
    let mut rows = Vec::new();
    for (category, value) in ordered.into_iter().take(limit) {
        cumulative += value;
        let share = if total.abs() <= f64::EPSILON {
            None
        } else {
            Some(value / total)
        };
        let cumulative_share = if total.abs() <= f64::EPSILON {
            None
        } else {
            Some(cumulative / total)
        };
        let class = if pareto {
            Some(if cumulative_share.unwrap_or(1.0) <= 0.8 {
                "A"
            } else if cumulative_share.unwrap_or(1.0) <= 0.95 {
                "B"
            } else {
                "C"
            })
        } else {
            None
        };
        let mut row = BTreeMap::from([
            ("分类".into(), Value::Text(category)),
            ("指标值".into(), Value::Float(value)),
            (
                "贡献率".into(),
                share.map(Value::Float).unwrap_or(Value::Null),
            ),
            ("累计值".into(), Value::Float(cumulative)),
            (
                "累计占比".into(),
                cumulative_share.map(Value::Float).unwrap_or(Value::Null),
            ),
        ]);
        if let Some(class) = class {
            let original = row.remove("分类").unwrap_or(Value::Null);
            row.insert("原分类".into(), original);
            row.insert("分类".into(), Value::Text(class.to_string()));
        }
        rows.push(row);
    }
    Ok(result(
        if pareto {
            "Pareto分析"
        } else {
            "贡献分析"
        },
        BTreeMap::from([
            ("总值".into(), Value::Float(total)),
            ("分类数".into(), Value::Float(rows.len() as f64)),
        ]),
        rows,
        if total.abs() <= f64::EPSILON {
            vec!["总值为零，贡献率不可比较".into()]
        } else {
            Vec::new()
        },
    ))
}

pub(super) fn funnel(
    value: &Value,
    named: &BTreeMap<String, Value>,
    limit: usize,
) -> Result<Value, String> {
    let table = table(value)?;
    let entity = text(named, "实体列")?;
    let step_column = text(named, "步骤列")?;
    let steps = named
        .get("步骤")
        .ok_or_else(|| error("漏斗分析缺少“步骤”参数"))?
        .iter_values_limited(limit)?
        .into_iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>();
    if steps.is_empty()
        || steps.len() > limit
        || steps.iter().collect::<HashSet<_>>().len() != steps.len()
    {
        return Err(error("漏斗步骤必须非空、唯一且不超过集合预算"));
    }
    let mut entities = BTreeMap::<String, HashSet<String>>::new();
    for row in &table.rows {
        let Some(id) = row.get(&entity).map(ToString::to_string) else {
            continue;
        };
        let Some(step) = row.get(&step_column).map(ToString::to_string) else {
            continue;
        };
        entities.entry(step).or_default().insert(id);
    }
    let first_count = entities.get(&steps[0]).map_or(0, HashSet::len) as f64;
    let mut rows = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let count = entities.get(step).map_or(0, HashSet::len) as f64;
        let previous = index.checked_sub(1).and_then(|i| entities.get(&steps[i]));
        let previous_count = previous.map_or(0, HashSet::len) as f64;
        rows.push(BTreeMap::from([
            ("步骤".into(), Value::Text(step.clone())),
            ("到达数".into(), Value::Float(count)),
            (
                "上一步转化率".into(),
                if previous_count > 0.0 {
                    Value::Float(count / previous_count)
                } else {
                    Value::Null
                },
            ),
            (
                "总体转化率".into(),
                if first_count > 0.0 {
                    Value::Float(count / first_count)
                } else {
                    Value::Null
                },
            ),
        ]));
    }
    Ok(result(
        "漏斗分析",
        BTreeMap::from([
            ("步骤数".into(), Value::Float(steps.len() as f64)),
            ("首步人数".into(), Value::Float(first_count)),
        ]),
        rows,
        Vec::new(),
    ))
}

pub(super) fn cohort(
    value: &Value,
    named: &BTreeMap<String, Value>,
    limit: usize,
) -> Result<Value, String> {
    let table = table(value)?;
    let entity = text(named, "实体列")?;
    let period = text(named, "周期列")?;
    let mut first = HashMap::<String, String>::new();
    let mut active = HashSet::<(String, String)>::new();
    for row in &table.rows {
        let Some(id) = row.get(&entity).map(ToString::to_string) else {
            continue;
        };
        let Some(p) = row.get(&period).map(ToString::to_string) else {
            continue;
        };
        first
            .entry(id.clone())
            .and_modify(|v| {
                if p < *v {
                    *v = p.clone();
                }
            })
            .or_insert(p.clone());
        active.insert((id, p));
    }
    let mut counts = BTreeMap::<(String, String), usize>::new();
    let mut cohort_sizes = BTreeMap::<String, usize>::new();
    for (id, cohort) in first {
        *cohort_sizes.entry(cohort.clone()).or_default() += 1;
        for (entity_id, p) in active.iter().filter(|(entity_id, _)| entity_id == &id) {
            let _ = entity_id;
            *counts.entry((cohort.clone(), p.clone())).or_default() += 1;
        }
    }
    let mut rows = counts
        .into_iter()
        .take(limit)
        .map(|((cohort, p), count)| {
            let size = cohort_sizes.get(&cohort).copied().unwrap_or(0);
            BTreeMap::from([
                ("队列".into(), Value::Text(cohort)),
                ("周期".into(), Value::Text(p)),
                ("数量".into(), Value::Float(count as f64)),
                (
                    "留存率".into(),
                    if size > 0 {
                        Value::Float(count as f64 / size as f64)
                    } else {
                        Value::Null
                    },
                ),
            ])
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.get("队列")
            .map(ToString::to_string)
            .cmp(&b.get("队列").map(ToString::to_string))
            .then_with(|| {
                a.get("周期")
                    .map(ToString::to_string)
                    .cmp(&b.get("周期").map(ToString::to_string))
            })
    });
    Ok(result(
        "队列分析",
        BTreeMap::from([("队列数".into(), Value::Float(cohort_sizes.len() as f64))]),
        rows,
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_record(value: Value) -> BTreeMap<String, Value> {
        match value {
            Value::Record(record) => record.borrow().clone(),
            other => panic!("expected result, got {other}"),
        }
    }

    fn table_rows(value: &Value) -> Vec<BTreeMap<String, Value>> {
        match value {
            Value::Table(table) => table.borrow().rows.clone(),
            other => panic!("expected table, got {other}"),
        }
    }

    fn column_row(rows: &[BTreeMap<String, Value>], column: &str) -> BTreeMap<String, Value> {
        rows.iter()
            .find(|row| row.get("列").map(ToString::to_string).as_deref() == Some(column))
            .cloned()
            .unwrap_or_else(|| panic!("missing column {column}"))
    }

    fn cell(row: &BTreeMap<String, Value>, name: &str) -> f64 {
        row.get(name)
            .and_then(|value| value.as_f64().ok())
            .unwrap_or_else(|| panic!("missing numeric {name}"))
    }

    /// 概览结果的所有单元格必须是标量，否则报告的 CSV 导出会拒绝这张表。
    #[test]
    fn profile_rows_are_scalar_so_csv_can_express_them() {
        let record = profile_record(data_profile(&sample(), &BTreeMap::new(), 100).unwrap());
        for row in table_rows(&record["数据"]) {
            for (name, value) in &row {
                assert!(
                    !matches!(value, Value::Record(_) | Value::Table(_) | Value::List(_)),
                    "概览字段 {name} 是嵌套结构，CSV 无法无损表达"
                );
            }
        }
        // 高频明细单独作为规范化表，而不是塞进单元格。
        let detail = table_rows(&record["高频明细"]);
        assert!(!detail.is_empty());
        for row in &detail {
            for key in ["列", "排名", "值", "数量", "占比"] {
                assert!(row.contains_key(key), "高频明细缺少 {key}");
            }
        }
        // 字段说明面向读者，覆盖容易误读的字段。
        let descriptions = table_rows(&record["字段说明"]);
        let fields = descriptions
            .iter()
            .map(|row| row["字段"].to_string())
            .collect::<Vec<_>>();
        for field in ["缺失率", "高频值截断", "异常数量", "敏感标签"] {
            assert!(fields.contains(&field.to_string()), "字段说明缺少 {field}");
        }
    }

    #[test]
    fn profile_marks_truncated_high_cardinality_columns() {
        let rows = (0..12)
            .map(|index| {
                BTreeMap::from([("编号".to_string(), Value::Text(format!("C{index:02}")))])
            })
            .collect::<Vec<_>>();
        let wide = Value::Table(std::rc::Rc::new(std::cell::RefCell::new(Table {
            rows,
            ..Default::default()
        })));
        let record = profile_record(
            data_profile(
                &wide,
                &BTreeMap::from([("TopN".to_string(), Value::Float(5.0))]),
                100,
            )
            .unwrap(),
        );
        let row = column_row(&table_rows(&record["数据"]), "编号");
        assert_eq!(cell(&row, "唯一值"), 12.0);
        assert!(matches!(row["高频值截断"], Value::Bool(true)), "{row:?}");
        // 12 个唯一值里只展示 5 个，覆盖率是 5/12。
        assert!((cell(&row, "高频值覆盖率") - 5.0 / 12.0).abs() < 1e-12);
        assert_eq!(
            cell(&profile_record(record["指标"].clone()), "截断列数"),
            1.0
        );
        assert!(
            record["警告"].to_string().contains("只展示前 5 个"),
            "截断必须进警告：{}",
            record["警告"]
        );

        // TopN 覆盖全部类别时不得标记截断。
        let record = profile_record(data_profile(&sample(), &BTreeMap::new(), 100).unwrap());
        let row = column_row(&table_rows(&record["数据"]), "地区");
        assert!(matches!(row["高频值截断"], Value::Bool(false)));
        assert!((cell(&row, "高频值覆盖率") - 1.0).abs() < 1e-12);
    }

    /// 100 是明显离群点：P25=10、P75=30、四分位距=20，上界 30+30=60。
    #[test]
    fn profile_counts_outliers_without_removing_them() {
        let rows = [10.0, 12.0, 20.0, 30.0, 100.0]
            .into_iter()
            .map(|value| BTreeMap::from([("金额".to_string(), Value::Float(value))]))
            .collect::<Vec<_>>();
        let source = Value::Table(std::rc::Rc::new(std::cell::RefCell::new(Table {
            rows,
            ..Default::default()
        })));
        let record = profile_record(data_profile(&source, &BTreeMap::new(), 100).unwrap());
        let row = column_row(&table_rows(&record["数据"]), "金额");
        assert_eq!(cell(&row, "P25"), 12.0);
        assert_eq!(cell(&row, "P75"), 30.0);
        assert_eq!(cell(&row, "四分位距"), 18.0);
        assert_eq!(cell(&row, "异常数量"), 1.0);
        assert!((cell(&row, "异常率") - 0.2).abs() < 1e-12);
        // 异常值只统计，不从有效数量里剔除。
        assert_eq!(cell(&row, "有效数量"), 5.0);
        assert_eq!(cell(&row, "最大值"), 100.0);
    }

    /// 概览不能把敏感标签洗掉，否则导出会绕过授权。
    #[test]
    fn profile_propagates_units_sensitivity_and_provenance() {
        use crate::execution::{
            Metadata, ProvenanceRef, SensitiveLabel, Sensitivity, UnitIdentity,
        };
        let mut column_metadata = BTreeMap::new();
        column_metadata.insert(
            "销售额".to_string(),
            Metadata {
                unit: Some(UnitIdentity {
                    name: "元".into(),
                    dimension: "货币".into(),
                    scale_numerator: 1,
                    scale_denominator: 1,
                    basis: None,
                }),
                sensitivity: Sensitivity {
                    labels: [SensitiveLabel::Financial].into_iter().collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let source = Value::Table(std::rc::Rc::new(std::cell::RefCell::new(Table {
            rows: vec![
                BTreeMap::from([("销售额".to_string(), Value::Float(100.0))]),
                BTreeMap::from([("销售额".to_string(), Value::Float(150.0))]),
            ],
            metadata: Metadata {
                provenance: vec![ProvenanceRef {
                    source_id: "订单表".into(),
                    location: None,
                    step_id: None,
                }],
                sensitivity: Sensitivity {
                    labels: [SensitiveLabel::Financial].into_iter().collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
            column_metadata,
        })));
        let record = profile_record(data_profile(&source, &BTreeMap::new(), 100).unwrap());
        let row = column_row(&table_rows(&record["数据"]), "销售额");
        assert_eq!(row["单位"].to_string(), "元");
        assert_eq!(row["敏感标签"].to_string(), "财务");

        let metrics = profile_record(record["指标"].clone());
        assert_eq!(cell(&metrics, "敏感列数"), 1.0);
        assert_eq!(cell(&metrics, "来源数"), 1.0);

        // 结果表本身必须继续携带标签，导出时才会触发同一套授权检查。
        let Value::Table(result_table) = &record["数据"] else {
            panic!("expected table")
        };
        let result_table = result_table.borrow();
        assert!(
            !result_table.sensitivity().labels.is_empty(),
            "概览结果洗掉了敏感标签"
        );
        assert_eq!(result_table.metadata.provenance.len(), 1);
    }

    fn sample() -> Value {
        Value::Table(std::rc::Rc::new(std::cell::RefCell::new(Table {
            rows: vec![
                BTreeMap::from([
                    ("月份".into(), Value::Text("一月".into())),
                    ("地区".into(), Value::Text("华东".into())),
                    ("销售额".into(), Value::Float(100.0)),
                ]),
                BTreeMap::from([
                    ("月份".into(), Value::Text("二月".into())),
                    ("地区".into(), Value::Text("华东".into())),
                    ("销售额".into(), Value::Float(150.0)),
                ]),
                BTreeMap::from([
                    ("月份".into(), Value::Text("二月".into())),
                    ("地区".into(), Value::Text("华南".into())),
                    ("销售额".into(), Value::Float(50.0)),
                ]),
            ],
            ..Default::default()
        })))
    }

    #[test]
    fn profile_reports_quality_and_numeric_distribution() {
        let value = data_profile(&sample(), &BTreeMap::new(), 100).unwrap();
        let Value::Record(result) = value else {
            panic!("expected result")
        };
        let result = result.borrow();
        assert_eq!(result["方法"].to_string(), "数据概览");
        let Value::Table(rows) = &result["数据"] else {
            panic!("expected rows")
        };
        assert!(rows
            .borrow()
            .rows
            .iter()
            .any(|row| row["列"].to_string() == "销售额" && row["平均值"].to_string() == "100"));
    }

    #[test]
    fn contribution_is_sorted_and_pareto_keeps_original_category() {
        let value = contribution(
            &sample(),
            &BTreeMap::from([
                (String::from("分类列"), Value::Text("地区".into())),
                (String::from("指标列"), Value::Text("销售额".into())),
            ]),
            100,
            true,
        )
        .unwrap();
        let Value::Record(result) = value else {
            panic!("expected result")
        };
        let result = result.borrow();
        let Value::Table(rows) = &result["数据"] else {
            panic!("expected rows")
        };
        assert_eq!(rows.borrow().rows[0]["原分类"].to_string(), "华东");
        assert!(rows.borrow().rows[0].contains_key("累计占比"));
    }

    #[test]
    fn period_compare_marks_missing_base_period_as_null() {
        let value = period_compare(
            &sample(),
            &BTreeMap::from([
                (String::from("周期列"), Value::Text("月份".into())),
                (String::from("指标列"), Value::Text("销售额".into())),
            ]),
            100,
        )
        .unwrap();
        let Value::Record(result) = value else {
            panic!("expected result")
        };
        let result = result.borrow();
        let Value::Table(rows) = &result["数据"] else {
            panic!("expected rows")
        };
        assert!(matches!(rows.borrow().rows[0]["上期"], Value::Null));
    }
}
