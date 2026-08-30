use crate::stream::StreamRecord;
use crate::value::Value;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::aggregate::{GroupState, MetricOperation, MetricSpec};
use super::dependencies::{validate_backward_references, validate_selected_metric_dependencies};
use super::dimension::{DimensionSpec, DimensionTransform};
use super::group_key::GroupKey;
use super::language_error;

pub(super) struct GroupPlan {
    pub(super) name: String,
    pub(super) dimensions: Vec<DimensionSpec>,
    pub(super) metric_indices: Vec<usize>,
    pub(super) groups: HashMap<GroupKey, GroupState>,
}

pub(super) struct FilterSpec {
    index: usize,
    start: Option<String>,
    end: Option<String>,
}

impl FilterSpec {
    pub(super) fn matches(&self, record: &StreamRecord) -> bool {
        let value = record.get(self.index).unwrap_or("").trim();
        !value.is_empty()
            && self
                .start
                .as_ref()
                .is_none_or(|start| value >= start.as_str())
            && self.end.as_ref().is_none_or(|end| value < end.as_str())
    }
}

pub(super) fn parse_filter(value: &Value, headers: &[String]) -> Result<FilterSpec, String> {
    let record = record_fields(value, "过滤条件")?;
    let column = required_text(&record, "列", "过滤条件")?;
    let start = record
        .get("开始")
        .map(ToString::to_string)
        .filter(|value| !value.is_empty());
    let end = record
        .get("结束")
        .map(ToString::to_string)
        .filter(|value| !value.is_empty());
    if start.is_none() && end.is_none() {
        return Err(language_error("过滤条件至少需要“开始”或“结束”"));
    }
    if start
        .as_ref()
        .zip(end.as_ref())
        .is_some_and(|(start, end)| start >= end)
    {
        return Err(language_error("过滤条件的开始必须早于结束"));
    }
    Ok(FilterSpec {
        index: column_index(headers, &column)?,
        start,
        end,
    })
}

pub(super) fn parse_plans(
    value: &Value,
    headers: &[String],
    metrics: &[MetricSpec],
    collection_limit: usize,
) -> Result<Vec<GroupPlan>, String> {
    value
        .iter_values_limited(collection_limit)
        .map_err(|_| language_error("分组方案必须是列表"))?
        .into_iter()
        .map(|value| {
            let record = record_fields(&value, "分组方案")?;
            let name = required_text(&record, "名称", "分组方案")?;
            let dimensions = record
                .get("维度")
                .ok_or_else(|| language_error(format!("分组方案“{name}”缺少“维度”")))?
                .iter_values_limited(collection_limit)
                .map_err(|_| language_error(format!("分组方案“{name}”的维度必须是列表")))?
                .into_iter()
                .map(|value| parse_dimension(&value, headers))
                .collect::<Result<Vec<_>, _>>()?;
            let selected_names = record
                .get("指标")
                .map(|value| {
                    let values = value
                        .iter_values_limited(collection_limit)
                        .map_err(|_| language_error(format!("分组方案“{name}”的指标必须是列表")))?;
                    Ok::<HashSet<String>, String>(
                        values
                            .into_iter()
                            .map(|value| value.to_string())
                            .collect::<HashSet<_>>(),
                    )
                })
                .transpose()?;
            if selected_names.as_ref().is_some_and(HashSet::is_empty) {
                return Err(language_error(format!("分组方案“{name}”至少需要一个指标")));
            }
            if let Some(selected_names) = &selected_names {
                for selected in selected_names {
                    if !metrics.iter().any(|metric| metric.name == *selected) {
                        return Err(language_error(format!(
                            "分组方案“{name}”引用了未知指标“{selected}”"
                        )));
                    }
                }
            }
            let metric_indices = metrics
                .iter()
                .enumerate()
                .filter_map(|(index, metric)| {
                    selected_names
                        .as_ref()
                        .is_none_or(|names| names.contains(&metric.name))
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            validate_selected_metric_dependencies(&name, &metric_indices, metrics)?;
            Ok(GroupPlan {
                name,
                dimensions,
                metric_indices,
                groups: HashMap::new(),
            })
        })
        .collect()
}

fn parse_dimension(value: &Value, headers: &[String]) -> Result<DimensionSpec, String> {
    let (column, output, transform) = match value {
        Value::Text(column) => (column.clone(), column.clone(), DimensionTransform::Raw),
        Value::Record(_) => {
            let record = record_fields(value, "维度")?;
            let column = required_text(&record, "列", "维度")?;
            let output = record
                .get("名称")
                .map(ToString::to_string)
                .unwrap_or_else(|| column.clone());
            let transform = match record.get("转换").map(ToString::to_string).as_deref() {
                None | Some("原值") => DimensionTransform::Raw,
                Some("日期") => DimensionTransform::Date,
                Some("周") | Some("周一") => DimensionTransform::Week,
                Some("月份") => DimensionTransform::Month,
                Some("季度") => DimensionTransform::Quarter,
                Some("年份") => DimensionTransform::Year,
                Some(other) => return Err(language_error(format!("不支持维度转换“{other}”"))),
            };
            (column, output, transform)
        }
        _ => return Err(language_error("维度必须是列名文本或配置记录")),
    };
    let index = column_index(headers, &column)?;
    Ok(DimensionSpec {
        column,
        output,
        index,
        transform,
    })
}

pub(super) fn parse_metrics(
    value: &Value,
    headers: &[String],
    collection_limit: usize,
) -> Result<Vec<MetricSpec>, String> {
    let values = value
        .iter_values_limited(collection_limit)
        .map_err(|_| language_error("指标必须是列表"))?;
    let mut metrics = Vec::with_capacity(values.len());
    let mut names = HashSet::new();
    for value in values {
        let record = record_fields(&value, "指标")?;
        let name = required_text(&record, "名称", "指标")?;
        if !names.insert(name.clone()) {
            return Err(language_error(format!("指标名称“{name}”重复")));
        }
        let operation = if let Some(operation) = record.get("聚合") {
            parse_aggregate(&record, &operation.to_string(), headers, collection_limit)?
        } else if let Some(calculation) = record.get("计算") {
            parse_calculation(&record, &calculation.to_string(), &metrics)?
        } else {
            return Err(language_error(format!("指标“{name}”需要“聚合”或“计算”")));
        };
        metrics.push(MetricSpec { name, operation });
    }
    Ok(metrics)
}

fn parse_aggregate(
    record: &BTreeMap<String, Value>,
    operation: &str,
    headers: &[String],
    collection_limit: usize,
) -> Result<MetricOperation, String> {
    if operation == "计数" {
        return Ok(MetricOperation::Count);
    }
    if operation == "非空计数" {
        let column = required_text(record, "列", "指标")?;
        return Ok(MetricOperation::NonNullCount {
            index: column_index(headers, &column)?,
        });
    }
    if operation == "时差平均" {
        let columns = record
            .get("列")
            .ok_or_else(|| language_error("时差平均指标缺少“列”"))?
            .iter_values_limited(collection_limit)
            .map_err(|_| language_error("时差平均的“列”必须是列表"))?
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let columns: [String; 2] = columns
            .try_into()
            .map_err(|_| language_error("时差平均需要开始、结束两列"))?;
        let indices = [
            column_index(headers, &columns[0])?,
            column_index(headers, &columns[1])?,
        ];
        let divisor = match record.get("单位").map(ToString::to_string).as_deref() {
            None | Some("小时") => Decimal::from(3_600_000),
            Some("分钟") => Decimal::from(60_000),
            Some("天") => Decimal::from(86_400_000),
            Some(other) => return Err(language_error(format!("不支持时差单位“{other}”"))),
        };
        return Ok(MetricOperation::DurationAverage {
            columns,
            indices,
            divisor,
            dedup: optional_dedup_index(record, headers)?,
        });
    }
    if operation == "乘积求和" {
        let columns = record
            .get("列")
            .ok_or_else(|| language_error("乘积求和指标缺少“列”"))?
            .iter_values_limited(collection_limit)
            .map_err(|_| language_error("乘积求和的“列”必须是列表"))?
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if columns.len() < 2 {
            return Err(language_error("乘积求和至少需要两列"));
        }
        let indices = columns
            .iter()
            .map(|column| column_index(headers, column))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(MetricOperation::ProductSum {
            columns,
            indices,
            dedup: optional_dedup_index(record, headers)?,
        });
    }
    let column = required_text(record, "列", "指标")?;
    let index = column_index(headers, &column)?;
    match operation {
        "求和" => Ok(MetricOperation::Sum {
            column,
            index,
            dedup: optional_dedup_index(record, headers)?,
        }),
        "绝对值求和" => Ok(MetricOperation::AbsoluteSum {
            column,
            index,
            dedup: optional_dedup_index(record, headers)?,
        }),
        "去重计数" => Ok(MetricOperation::DistinctCount { index }),
        "平均值" => Ok(MetricOperation::Average {
            column,
            index,
            dedup: optional_dedup_index(record, headers)?,
        }),
        "最小值" => Ok(MetricOperation::Minimum { column, index }),
        "最大值" => Ok(MetricOperation::Maximum { column, index }),
        other => Err(language_error(format!("不支持聚合方式“{other}”"))),
    }
}

fn parse_calculation(
    record: &BTreeMap<String, Value>,
    calculation: &str,
    previous: &[MetricSpec],
) -> Result<MetricOperation, String> {
    let operation = match calculation {
        "差值" => MetricOperation::Difference {
            left: required_text(record, "左", "差值指标")?,
            right: required_text(record, "右", "差值指标")?,
        },
        "比率" | "百分比" => MetricOperation::Ratio {
            numerator: required_text(record, "分子", "比率指标")?,
            denominator: required_text(record, "分母", "比率指标")?,
            percent: calculation == "百分比",
        },
        other => return Err(language_error(format!("不支持指标计算“{other}”"))),
    };
    validate_backward_references(&operation, previous)?;
    Ok(operation)
}

fn optional_dedup_index(
    record: &BTreeMap<String, Value>,
    headers: &[String],
) -> Result<Option<usize>, String> {
    record
        .get("去重列")
        .map(ToString::to_string)
        .map(|column| column_index(headers, &column))
        .transpose()
}

fn record_fields(value: &Value, context: &str) -> Result<BTreeMap<String, Value>, String> {
    match value {
        Value::Record(record) => Ok(record.borrow().clone()),
        _ => Err(language_error(format!("{context}必须是记录"))),
    }
}

fn required_text(
    record: &BTreeMap<String, Value>,
    key: &str,
    context: &str,
) -> Result<String, String> {
    record
        .get(key)
        .map(ToString::to_string)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| language_error(format!("{context}缺少“{key}”")))
}

fn column_index(headers: &[String], column: &str) -> Result<usize, String> {
    headers
        .iter()
        .position(|header| header.as_str() == column)
        .ok_or_else(|| language_error(format!("CSV 不存在列“{column}”")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> Vec<String> {
        ["日期", "省份", "订单", "销售额", "成本", "开始", "结束"]
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn record(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        Value::record(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn text(value: &str) -> Value {
        Value::Text(value.to_string())
    }

    fn sum_metric(name: &str, column: &str) -> Value {
        record([
            ("名称", text(name)),
            ("聚合", text("求和")),
            ("列", text(column)),
        ])
    }

    fn parse(metrics: &Value) -> Result<Vec<MetricSpec>, String> {
        parse_metrics(metrics, &headers(), 100)
    }

    fn metric_error(metrics: &Value) -> String {
        parse(metrics).err().unwrap()
    }

    fn plan_error(plans: &Value, metrics: &[MetricSpec]) -> String {
        parse_plans(plans, &headers(), metrics, 100).err().unwrap()
    }

    fn filter_error(value: &Value) -> String {
        parse_filter(value, &headers()).err().unwrap()
    }

    #[test]
    fn every_aggregate_and_calculation_keyword_is_accepted() {
        let metrics = Value::list(vec![
            record([("名称", text("记录数")), ("聚合", text("计数"))]),
            record([
                ("名称", text("非空数")),
                ("聚合", text("非空计数")),
                ("列", text("销售额")),
            ]),
            sum_metric("销售额", "销售额"),
            record([
                ("名称", text("绝对值")),
                ("聚合", text("绝对值求和")),
                ("列", text("销售额")),
            ]),
            record([
                ("名称", text("成本额")),
                ("聚合", text("乘积求和")),
                ("列", Value::list(vec![text("成本"), text("销售额")])),
            ]),
            record([
                ("名称", text("订单数")),
                ("聚合", text("去重计数")),
                ("列", text("订单")),
            ]),
            record([
                ("名称", text("均价")),
                ("聚合", text("平均值")),
                ("列", text("销售额")),
            ]),
            record([
                ("名称", text("最低")),
                ("聚合", text("最小值")),
                ("列", text("销售额")),
            ]),
            record([
                ("名称", text("最高")),
                ("聚合", text("最大值")),
                ("列", text("销售额")),
            ]),
            record([
                ("名称", text("时长")),
                ("聚合", text("时差平均")),
                ("列", Value::list(vec![text("开始"), text("结束")])),
                ("单位", text("分钟")),
            ]),
            record([
                ("名称", text("毛利")),
                ("计算", text("差值")),
                ("左", text("销售额")),
                ("右", text("成本额")),
            ]),
            record([
                ("名称", text("毛利率")),
                ("计算", text("百分比")),
                ("分子", text("毛利")),
                ("分母", text("销售额")),
            ]),
            record([
                ("名称", text("成本率")),
                ("计算", text("比率")),
                ("分子", text("成本额")),
                ("分母", text("销售额")),
            ]),
        ]);
        let parsed = parse(&metrics).unwrap();
        assert_eq!(parsed.len(), 13);
        assert!(matches!(parsed[0].operation, MetricOperation::Count));
        assert!(matches!(
            parsed[9].operation,
            MetricOperation::DurationAverage {
                divisor,
                ..
            } if divisor == Decimal::from(60_000)
        ));
        assert!(matches!(
            parsed[11].operation,
            MetricOperation::Ratio { percent: true, .. }
        ));
        assert!(matches!(
            parsed[12].operation,
            MetricOperation::Ratio { percent: false, .. }
        ));
    }

    #[test]
    fn metric_parsing_rejects_malformed_configurations() {
        let cases = [
            (Value::list(vec![text("不是记录")]), "指标必须是记录"),
            (
                Value::list(vec![record([("聚合", text("计数"))])]),
                "指标缺少“名称”",
            ),
            (
                Value::list(vec![
                    sum_metric("重复", "销售额"),
                    sum_metric("重复", "成本"),
                ]),
                "指标名称“重复”重复",
            ),
            (
                Value::list(vec![record([("名称", text("无操作"))])]),
                "指标“无操作”需要“聚合”或“计算”",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("未知")),
                    ("聚合", text("中位数")),
                    ("列", text("销售额")),
                ])]),
                "不支持聚合方式“中位数”",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("未知计算")),
                    ("计算", text("乘积")),
                ])]),
                "不支持指标计算“乘积”",
            ),
            (
                Value::list(vec![sum_metric("缺列", "不存在")]),
                "CSV 不存在列“不存在”",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("单列乘积")),
                    ("聚合", text("乘积求和")),
                    ("列", Value::list(vec![text("销售额")])),
                ])]),
                "乘积求和至少需要两列",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("三列时差")),
                    ("聚合", text("时差平均")),
                    (
                        "列",
                        Value::list(vec![text("开始"), text("结束"), text("日期")]),
                    ),
                ])]),
                "时差平均需要开始、结束两列",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("坏单位")),
                    ("聚合", text("时差平均")),
                    ("列", Value::list(vec![text("开始"), text("结束")])),
                    ("单位", text("秒")),
                ])]),
                "不支持时差单位“秒”",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("坏去重")),
                    ("聚合", text("求和")),
                    ("列", text("销售额")),
                    ("去重列", text("不存在")),
                ])]),
                "CSV 不存在列“不存在”",
            ),
        ];
        for (metrics, expected) in cases {
            let error = metric_error(&metrics);
            assert!(error.contains(expected), "预期“{expected}”，实际“{error}”");
        }
        assert!(metric_error(&Value::Number(Decimal::ONE)).contains("指标必须是列表"));
    }

    #[test]
    fn dimensions_accept_column_text_or_transform_records() {
        let metrics = parse(&Value::list(vec![record([
            ("名称", text("记录数")),
            ("聚合", text("计数")),
        ])]))
        .unwrap();
        let plans = Value::list(vec![record([
            ("名称", text("方案")),
            (
                "维度",
                Value::list(vec![
                    text("省份"),
                    record([("列", text("日期")), ("转换", text("月份"))]),
                    record([
                        ("列", text("日期")),
                        ("名称", text("周一")),
                        ("转换", text("周")),
                    ]),
                ]),
            ),
        ])]);
        let parsed = parse_plans(&plans, &headers(), &metrics, 100).unwrap();
        assert_eq!(parsed.len(), 1);
        let outputs = parsed[0]
            .dimensions
            .iter()
            .map(|dimension| dimension.output.as_str())
            .collect::<Vec<_>>();
        assert_eq!(outputs, vec!["省份", "日期", "周一"]);
        assert_eq!(parsed[0].metric_indices, vec![0]);
    }

    #[test]
    fn plan_parsing_rejects_malformed_configurations() {
        let metrics = parse(&Value::list(vec![
            record([("名称", text("记录数")), ("聚合", text("计数"))]),
            sum_metric("销售额", "销售额"),
        ]))
        .unwrap();
        let cases = [
            (
                Value::list(vec![record([("维度", Value::list(vec![]))])]),
                "分组方案缺少“名称”",
            ),
            (
                Value::list(vec![record([("名称", text("无维度"))])]),
                "分组方案“无维度”缺少“维度”",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("维度非列表")),
                    ("维度", Value::Number(Decimal::ONE)),
                ])]),
                "分组方案“维度非列表”的维度必须是列表",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("坏维度")),
                    ("维度", Value::list(vec![Value::Number(Decimal::ONE)])),
                ])]),
                "维度必须是列名文本或配置记录",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("坏转换")),
                    (
                        "维度",
                        Value::list(vec![record([("列", text("日期")), ("转换", text("小时"))])]),
                    ),
                ])]),
                "不支持维度转换“小时”",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("空指标")),
                    ("维度", Value::list(vec![])),
                    ("指标", Value::list(vec![])),
                ])]),
                "分组方案“空指标”至少需要一个指标",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("未知指标")),
                    ("维度", Value::list(vec![])),
                    ("指标", Value::list(vec![text("不存在")])),
                ])]),
                "分组方案“未知指标”引用了未知指标“不存在”",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("指标非列表")),
                    ("维度", Value::list(vec![])),
                    ("指标", Value::Number(Decimal::ONE)),
                ])]),
                "分组方案“指标非列表”的指标必须是列表",
            ),
            (
                Value::list(vec![record([
                    ("名称", text("坏列")),
                    ("维度", Value::list(vec![text("不存在")])),
                ])]),
                "CSV 不存在列“不存在”",
            ),
            (Value::list(vec![text("不是记录")]), "分组方案必须是记录"),
        ];
        for (plans, expected) in cases {
            let error = plan_error(&plans, &metrics);
            assert!(error.contains(expected), "预期“{expected}”，实际“{error}”");
        }
        assert!(plan_error(&Value::Number(Decimal::ONE), &metrics).contains("分组方案必须是列表"));
    }

    #[test]
    fn filters_need_a_column_and_an_ordered_half_open_range() {
        let filter = parse_filter(
            &record([
                ("列", text("日期")),
                ("开始", text("2026-02-01")),
                ("结束", text("2026-03-01")),
            ]),
            &headers(),
        )
        .unwrap();
        assert!(filter.matches(&StreamRecord::from_fields(["2026-02-01".to_string()])));
        assert!(!filter.matches(&StreamRecord::from_fields(["2026-03-01".to_string()])));
        assert!(!filter.matches(&StreamRecord::from_fields([String::new()])));

        let cases = [
            (record([("开始", text("2026-01-01"))]), "过滤条件缺少“列”"),
            (
                record([("列", text("日期"))]),
                "过滤条件至少需要“开始”或“结束”",
            ),
            (
                record([
                    ("列", text("日期")),
                    ("开始", text("2026-03-01")),
                    ("结束", text("2026-02-01")),
                ]),
                "过滤条件的开始必须早于结束",
            ),
            (
                record([("列", text("不存在")), ("开始", text("2026-01-01"))]),
                "CSV 不存在列“不存在”",
            ),
            (text("不是记录"), "过滤条件必须是记录"),
        ];
        for (value, expected) in cases {
            let error = filter_error(&value);
            assert!(error.contains(expected), "预期“{expected}”，实际“{error}”");
        }
    }

    #[test]
    fn open_ended_filters_only_bound_one_side() {
        let from = parse_filter(
            &record([("列", text("日期")), ("开始", text("2026-02-01"))]),
            &headers(),
        )
        .unwrap();
        assert!(from.matches(&StreamRecord::from_fields(["2026-09-01".to_string()])));
        assert!(!from.matches(&StreamRecord::from_fields(["2026-01-31".to_string()])));

        let until = parse_filter(
            &record([("列", text("日期")), ("结束", text("2026-02-01"))]),
            &headers(),
        )
        .unwrap();
        assert!(until.matches(&StreamRecord::from_fields(["2026-01-31".to_string()])));
        assert!(!until.matches(&StreamRecord::from_fields(["2026-02-01".to_string()])));
    }
}
