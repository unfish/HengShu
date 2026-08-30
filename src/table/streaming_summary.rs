use crate::stream::RecordStream;
use crate::value::{Table, Value};
use std::collections::BTreeMap;
use std::rc::Rc;

mod aggregate;
mod dependencies;
mod dimension;
mod group_key;
mod plan;
mod symbols;

use aggregate::{
    deduplication_sets, finalize_metric, initial_state, update_metric, GroupState, RowCache,
    UpdateContext,
};
use group_key::{compare_symbol_keys, dimension_key};
use plan::{parse_filter, parse_metrics, parse_plans};
use symbols::StringInterner;

/// 每读取这么多行检查一次执行预算，避免逐行检查拖慢汇总。
const LIMIT_CHECK_INTERVAL: usize = 4096;

pub(super) fn streaming_multi_summary<F>(
    source: &mut dyn RecordStream,
    plan_value: &Value,
    metric_value: &Value,
    filter_value: Option<&Value>,
    collection_limit: usize,
    mut check_limits: F,
) -> Result<Value, String>
where
    F: FnMut() -> Result<(), String>,
{
    let headers = source.columns().to_vec();
    let metrics = parse_metrics(metric_value, &headers, collection_limit)?;
    let mut plans = parse_plans(plan_value, &headers, &metrics, collection_limit)?;
    let filter = filter_value
        .map(|value| parse_filter(value, &headers))
        .transpose()?;
    if plans.is_empty() {
        return Err(language_error("流式多维汇总至少需要一个分组方案"));
    }
    if metrics.is_empty() {
        return Err(language_error("流式多维汇总至少需要一个指标"));
    }

    let mut total_groups = 0usize;
    let mut row_cache = RowCache::new(headers.len());
    let mut symbols = StringInterner::default();
    let mut record_index = 0usize;
    while let Some(record) = source.next_record().map_err(language_error)? {
        let current_index = record_index;
        record_index += 1;
        if current_index.is_multiple_of(LIMIT_CHECK_INTERVAL) {
            check_limits()?;
        }
        if filter
            .as_ref()
            .is_some_and(|filter| !filter.matches(&record))
        {
            continue;
        }
        row_cache.reset();
        for plan in &mut plans {
            let key = dimension_key(&record, &plan.dimensions, &mut symbols)?;
            let state = match plan.groups.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    total_groups = total_groups.saturating_add(1);
                    if total_groups > collection_limit {
                        return Err(language_error("流式多维汇总的分组数量超过集合项数限制"));
                    }
                    entry.insert(GroupState {
                        metrics: plan
                            .metric_indices
                            .iter()
                            .map(|index| initial_state(&metrics[*index]))
                            .collect(),
                        deduplication: deduplication_sets(&metrics, &plan.metric_indices),
                    })
                }
            };
            for deduplication in &mut state.deduplication {
                let value = record.get(deduplication.column_index).unwrap_or("").trim();
                deduplication.current_is_new =
                    !value.is_empty() && deduplication.seen.insert(symbols.intern(value)?);
            }
            let mut context = UpdateContext {
                record: &record,
                cache: &mut row_cache,
                csv_line: current_index + 2,
                symbols: &mut symbols,
            };
            for (metric_index, aggregate) in plan.metric_indices.iter().zip(&mut state.metrics) {
                let metric = &metrics[*metric_index];
                let enabled = metric
                    .dedup_index()
                    .and_then(|index| {
                        state
                            .deduplication
                            .iter()
                            .find(|candidate| candidate.column_index == index)
                            .map(|candidate| candidate.current_is_new)
                    })
                    .unwrap_or(true);
                update_metric(metric, aggregate, &mut context, enabled)?;
            }
        }
    }
    check_limits()?;

    let mut result = BTreeMap::new();
    for plan in plans {
        let mut rows = Vec::with_capacity(plan.groups.len());
        let mut groups = plan.groups.into_iter().collect::<Vec<_>>();
        groups.sort_by(|(left, _), (right, _)| compare_symbol_keys(left, right, &symbols));
        for (key, group) in groups {
            let mut row = BTreeMap::new();
            for (dimension, symbol) in plan.dimensions.iter().zip(key) {
                let value = symbol
                    .map(|symbol| Value::Text(symbols.resolve(symbol).to_string()))
                    .unwrap_or(Value::Null);
                row.insert(dimension.output.clone(), value);
            }
            for (metric_index, aggregate) in plan.metric_indices.iter().zip(&group.metrics) {
                let metric = &metrics[*metric_index];
                let value = finalize_metric(metric, aggregate, &row)?;
                row.insert(metric.name.clone(), value);
            }
            rows.push(row);
        }
        result.insert(
            plan.name,
            Value::Table(Rc::new(std::cell::RefCell::new(Table {
                rows,
                ..Default::default()
            }))),
        );
    }
    Ok(Value::record(result))
}

fn language_error(message: impl std::fmt::Display) -> String {
    message.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn summarize(
        path: &Path,
        plans: &Value,
        metrics: &Value,
        filter: Option<&Value>,
        collection_limit: usize,
    ) -> Result<Value, String> {
        let mut source = crate::stream::open_csv_records(path)?;
        streaming_multi_summary(
            &mut source,
            plans,
            metrics,
            filter,
            collection_limit,
            || Ok(()),
        )
    }

    fn record(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        Value::record(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn write(directory: &tempfile::TempDir, name: &str, contents: &str) -> std::path::PathBuf {
        let path = directory.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn multi_summary_streams_groups_distinct_values_and_derived_metrics() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(
            &directory,
            "发货.csv",
            "\u{feff}日期,省份,订单,明细,客户,数量,发货数量,销售额,成本价\n2026-01-02,江苏,1,A,甲,2,-2,20,6\n2026-01-03,江苏,1,A,甲,1,-1,10,6\n2026-02-01,浙江,2,B,乙,3,-2,45,10\n",
        );
        let plans = Value::list(vec![
            record([
                ("名称", Value::Text("月份".into())),
                (
                    "维度",
                    Value::list(vec![record([
                        ("列", Value::Text("日期".into())),
                        ("名称", Value::Text("月份".into())),
                        ("转换", Value::Text("月份".into())),
                    ])]),
                ),
            ]),
            record([
                ("名称", Value::Text("周".into())),
                (
                    "维度",
                    Value::list(vec![record([
                        ("列", Value::Text("日期".into())),
                        ("名称", Value::Text("周一".into())),
                        ("转换", Value::Text("周".into())),
                    ])]),
                ),
            ]),
            record([
                ("名称", Value::Text("区域".into())),
                ("维度", Value::list(vec![Value::Text("省份".into())])),
            ]),
        ]);
        let metrics = Value::list(vec![
            record([
                ("名称", Value::Text("记录数".into())),
                ("聚合", Value::Text("计数".into())),
            ]),
            record([
                ("名称", Value::Text("订单数".into())),
                ("聚合", Value::Text("去重计数".into())),
                ("列", Value::Text("订单".into())),
            ]),
            record([
                ("名称", Value::Text("销售额".into())),
                ("聚合", Value::Text("求和".into())),
                ("列", Value::Text("销售额".into())),
            ]),
            record([
                ("名称", Value::Text("发货量".into())),
                ("聚合", Value::Text("绝对值求和".into())),
                ("列", Value::Text("发货数量".into())),
            ]),
            record([
                ("名称", Value::Text("成本额".into())),
                ("聚合", Value::Text("乘积求和".into())),
                (
                    "列",
                    Value::list(vec![
                        Value::Text("成本价".into()),
                        Value::Text("数量".into()),
                    ]),
                ),
            ]),
            record([
                ("名称", Value::Text("去重销售额".into())),
                ("聚合", Value::Text("求和".into())),
                ("列", Value::Text("销售额".into())),
                ("去重列", Value::Text("明细".into())),
            ]),
            record([
                ("名称", Value::Text("去重成本额".into())),
                ("聚合", Value::Text("乘积求和".into())),
                (
                    "列",
                    Value::list(vec![
                        Value::Text("成本价".into()),
                        Value::Text("数量".into()),
                    ]),
                ),
                ("去重列", Value::Text("明细".into())),
            ]),
            record([
                ("名称", Value::Text("毛利额".into())),
                ("计算", Value::Text("差值".into())),
                ("左", Value::Text("销售额".into())),
                ("右", Value::Text("成本额".into())),
            ]),
            record([
                ("名称", Value::Text("毛利率".into())),
                ("计算", Value::Text("百分比".into())),
                ("分子", Value::Text("毛利额".into())),
                ("分母", Value::Text("销售额".into())),
            ]),
        ]);

        let Value::Record(result) = summarize(&path, &plans, &metrics, None, 100).unwrap() else {
            panic!("预期汇总记录")
        };
        let Value::Table(months) = &result.borrow()["月份"] else {
            panic!("预期月份表")
        };
        let months = months.borrow();
        assert_eq!(months.rows.len(), 2);
        assert_eq!(months.rows[0]["月份"].to_string(), "2026-01");
        assert_eq!(months.rows[0]["记录数"].to_string(), "2");
        assert_eq!(months.rows[0]["订单数"].to_string(), "1");
        assert_eq!(months.rows[0]["发货量"].to_string(), "3");
        assert_eq!(months.rows[0]["成本额"].to_string(), "18");
        assert_eq!(months.rows[0]["去重销售额"].to_string(), "20");
        assert_eq!(months.rows[0]["去重成本额"].to_string(), "12");
        assert_eq!(months.rows[0]["毛利额"].to_string(), "12");
        assert_eq!(months.rows[0]["毛利率"].to_string(), "40");
        let Value::Table(weeks) = &result.borrow()["周"] else {
            panic!("预期周表")
        };
        assert_eq!(weeks.borrow().rows[0]["周一"].to_string(), "2025-12-29");
        assert_eq!(weeks.borrow().rows[1]["周一"].to_string(), "2026-01-26");

        let filter = record([
            ("列", Value::Text("日期".into())),
            ("开始", Value::Text("2026-02-01".into())),
            ("结束", Value::Text("2026-03-01".into())),
        ]);
        let Value::Record(filtered) =
            summarize(&path, &plans, &metrics, Some(&filter), 100).unwrap()
        else {
            panic!("预期过滤汇总记录")
        };
        let Value::Table(filtered_months) = &filtered.borrow()["月份"] else {
            panic!("预期过滤月份表")
        };
        assert_eq!(filtered_months.borrow().rows.len(), 1);
        assert_eq!(
            filtered_months.borrow().rows[0]["月份"].to_string(),
            "2026-02"
        );
    }

    #[test]
    fn multi_summary_rejects_unknown_columns_and_forward_metric_references() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(&directory, "数据.csv", "列\n1\n");
        let bad_plan = Value::list(vec![record([
            ("名称", Value::Text("坏方案".into())),
            ("维度", Value::list(vec![Value::Text("不存在".into())])),
        ])]);
        let count = Value::list(vec![record([
            ("名称", Value::Text("数量".into())),
            ("聚合", Value::Text("计数".into())),
        ])]);
        assert!(summarize(&path, &bad_plan, &count, None, 10)
            .unwrap_err()
            .contains("不存在列"));

        let plan = Value::list(vec![record([
            ("名称", Value::Text("全部".into())),
            ("维度", Value::list(vec![])),
        ])]);
        let forward = Value::list(vec![record([
            ("名称", Value::Text("比率".into())),
            ("计算", Value::Text("比率".into())),
            ("分子", Value::Text("尚未定义".into())),
            ("分母", Value::Text("尚未定义".into())),
        ])]);
        assert!(summarize(&path, &plan, &forward, None, 10)
            .unwrap_err()
            .contains("尚未定义"));
    }

    #[test]
    fn plans_can_select_applicable_metrics_and_require_dependencies() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(
            &directory,
            "数据.csv",
            "区域,销售额,成本\n江苏,20,12\n浙江,10,8\n",
        );
        let metrics = Value::list(vec![
            record([
                ("名称", Value::Text("记录数".into())),
                ("聚合", Value::Text("计数".into())),
            ]),
            record([
                ("名称", Value::Text("销售额".into())),
                ("聚合", Value::Text("求和".into())),
                ("列", Value::Text("销售额".into())),
            ]),
            record([
                ("名称", Value::Text("成本".into())),
                ("聚合", Value::Text("求和".into())),
                ("列", Value::Text("成本".into())),
            ]),
            record([
                ("名称", Value::Text("毛利".into())),
                ("计算", Value::Text("差值".into())),
                ("左", Value::Text("销售额".into())),
                ("右", Value::Text("成本".into())),
            ]),
        ]);
        let selected = Value::list(vec![record([
            ("名称", Value::Text("区域".into())),
            ("维度", Value::list(vec![Value::Text("区域".into())])),
            ("指标", Value::list(vec![Value::Text("记录数".into())])),
        ])]);
        let Value::Record(result) = summarize(&path, &selected, &metrics, None, 10).unwrap() else {
            panic!("预期汇总记录")
        };
        let Value::Table(table) = &result.borrow()["区域"] else {
            panic!("预期表格")
        };
        assert!(table.borrow().rows[0].contains_key("记录数"));
        assert!(!table.borrow().rows[0].contains_key("销售额"));

        let missing_dependency = Value::list(vec![record([
            ("名称", Value::Text("坏方案".into())),
            ("维度", Value::list(vec![])),
            ("指标", Value::list(vec![Value::Text("毛利".into())])),
        ])]);
        assert!(summarize(&path, &missing_dependency, &metrics, None, 10)
            .unwrap_err()
            .contains("还需要指标"));
    }

    #[test]
    fn summaries_require_at_least_one_plan_and_one_metric() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(&directory, "数据.csv", "列\n1\n");
        let plan = Value::list(vec![record([
            ("名称", Value::Text("全部".into())),
            ("维度", Value::list(vec![])),
        ])]);
        let count = Value::list(vec![record([
            ("名称", Value::Text("数量".into())),
            ("聚合", Value::Text("计数".into())),
        ])]);
        assert!(summarize(&path, &Value::list(vec![]), &count, None, 10)
            .unwrap_err()
            .contains("至少需要一个分组方案"));
        assert!(summarize(&path, &plan, &Value::list(vec![]), None, 10)
            .unwrap_err()
            .contains("至少需要一个指标"));
    }

    #[test]
    fn group_counts_are_capped_by_the_collection_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(&directory, "数据.csv", "区域\n甲\n乙\n丙\n");
        let plan = Value::list(vec![record([
            ("名称", Value::Text("区域".into())),
            ("维度", Value::list(vec![Value::Text("区域".into())])),
        ])]);
        let count = Value::list(vec![record([
            ("名称", Value::Text("数量".into())),
            ("聚合", Value::Text("计数".into())),
        ])]);
        assert!(summarize(&path, &plan, &count, None, 2)
            .unwrap_err()
            .contains("分组数量超过集合项数限制"));
        assert!(summarize(&path, &plan, &count, None, 3).is_ok());
    }

    #[test]
    fn cancellation_from_the_limit_hook_stops_the_summary() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(&directory, "数据.csv", "区域\n甲\n乙\n");
        let plan = Value::list(vec![record([
            ("名称", Value::Text("区域".into())),
            ("维度", Value::list(vec![Value::Text("区域".into())])),
        ])]);
        let count = Value::list(vec![record([
            ("名称", Value::Text("数量".into())),
            ("聚合", Value::Text("计数".into())),
        ])]);
        let mut source = crate::stream::open_csv_records(&path).unwrap();
        let error = streaming_multi_summary(&mut source, &plan, &count, None, 10, || {
            Err("执行已取消".to_string())
        })
        .unwrap_err();
        assert_eq!(error, "执行已取消");
    }

    #[test]
    fn missing_dimension_values_group_under_null_and_sort_first() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(&directory, "数据.csv", "区域,额\n江苏,10\n,5\n江苏,2\n");
        let plan = Value::list(vec![record([
            ("名称", Value::Text("区域".into())),
            ("维度", Value::list(vec![Value::Text("区域".into())])),
        ])]);
        let metrics = Value::list(vec![record([
            ("名称", Value::Text("额".into())),
            ("聚合", Value::Text("求和".into())),
            ("列", Value::Text("额".into())),
        ])]);
        let Value::Record(result) = summarize(&path, &plan, &metrics, None, 10).unwrap() else {
            panic!("预期汇总记录")
        };
        let Value::Table(table) = &result.borrow()["区域"] else {
            panic!("预期表格")
        };
        let table = table.borrow();
        assert_eq!(table.rows.len(), 2);
        assert!(matches!(table.rows[0]["区域"], Value::Null));
        assert_eq!(table.rows[0]["额"].to_string(), "5");
        assert_eq!(table.rows[1]["区域"].to_string(), "江苏");
        assert_eq!(table.rows[1]["额"].to_string(), "12");
    }
}
