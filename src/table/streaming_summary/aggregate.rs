use crate::stream::StreamRecord;
use crate::value::Value;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashSet};

use super::language_error;
use super::symbols::{StringInterner, Symbol};

#[derive(Clone)]
pub(super) struct MetricSpec {
    pub(super) name: String,
    pub(super) operation: MetricOperation,
}

#[derive(Clone)]
pub(super) enum MetricOperation {
    Count,
    NonNullCount {
        index: usize,
    },
    Sum {
        column: String,
        index: usize,
        dedup: Option<usize>,
    },
    AbsoluteSum {
        column: String,
        index: usize,
        dedup: Option<usize>,
    },
    ProductSum {
        columns: Vec<String>,
        indices: Vec<usize>,
        dedup: Option<usize>,
    },
    DistinctCount {
        index: usize,
    },
    Average {
        column: String,
        index: usize,
        dedup: Option<usize>,
    },
    Minimum {
        column: String,
        index: usize,
    },
    Maximum {
        column: String,
        index: usize,
    },
    DurationAverage {
        columns: [String; 2],
        indices: [usize; 2],
        divisor: Decimal,
        dedup: Option<usize>,
    },
    Difference {
        left: String,
        right: String,
    },
    Ratio {
        numerator: String,
        denominator: String,
        percent: bool,
    },
}

impl MetricSpec {
    pub(super) fn dedup_index(&self) -> Option<usize> {
        match self.operation {
            MetricOperation::Sum { dedup, .. }
            | MetricOperation::AbsoluteSum { dedup, .. }
            | MetricOperation::ProductSum { dedup, .. }
            | MetricOperation::Average { dedup, .. }
            | MetricOperation::DurationAverage { dedup, .. } => dedup,
            _ => None,
        }
    }
}

pub(super) struct GroupState {
    pub(super) metrics: Vec<AggregateState>,
    pub(super) deduplication: Vec<DeduplicationState>,
}

pub(super) enum AggregateState {
    None,
    Count(u64),
    Sum(Decimal),
    Distinct(HashSet<Symbol>),
    Average { sum: Decimal, count: u64 },
    Extreme(Option<Decimal>),
}

pub(super) struct DeduplicationState {
    pub(super) column_index: usize,
    pub(super) seen: HashSet<Symbol>,
    pub(super) current_is_new: bool,
}

pub(super) fn deduplication_sets(
    metrics: &[MetricSpec],
    metric_indices: &[usize],
) -> Vec<DeduplicationState> {
    let mut indices = metric_indices
        .iter()
        .filter_map(|index| metrics[*index].dedup_index())
        .collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    indices
        .into_iter()
        .map(|column_index| DeduplicationState {
            column_index,
            seen: HashSet::new(),
            current_is_new: false,
        })
        .collect()
}

pub(super) struct RowCache {
    numbers: Vec<Option<Decimal>>,
    number_seen: Vec<bool>,
    datetimes: Vec<Option<chrono::NaiveDateTime>>,
    datetime_seen: Vec<bool>,
}

impl RowCache {
    pub(super) fn new(columns: usize) -> Self {
        Self {
            numbers: vec![None; columns],
            number_seen: vec![false; columns],
            datetimes: vec![None; columns],
            datetime_seen: vec![false; columns],
        }
    }

    pub(super) fn reset(&mut self) {
        self.number_seen.fill(false);
        self.datetime_seen.fill(false);
    }

    fn number(
        &mut self,
        record: &StreamRecord,
        index: usize,
        column: &str,
        csv_line: usize,
    ) -> Result<Option<Decimal>, String> {
        if self.number_seen[index] {
            return Ok(self.numbers[index]);
        }
        let value = record.get(index).unwrap_or("").trim();
        let parsed = if value.is_empty() {
            None
        } else {
            Some(value.parse::<Decimal>().map_err(|_| {
                language_error(format!(
                    "CSV 第 {csv_line} 行的列“{column}”不是数字：{value}"
                ))
            })?)
        };
        self.numbers[index] = parsed;
        self.number_seen[index] = true;
        Ok(parsed)
    }

    fn datetime(
        &mut self,
        record: &StreamRecord,
        index: usize,
        column: &str,
        csv_line: usize,
    ) -> Result<Option<chrono::NaiveDateTime>, String> {
        if self.datetime_seen[index] {
            return Ok(self.datetimes[index]);
        }
        let value = record.get(index).unwrap_or("").trim();
        let parsed = if value.is_empty() {
            None
        } else {
            Some(
                chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f").map_err(
                    |_| {
                        language_error(format!(
                            "CSV 第 {csv_line} 行的列“{column}”不是日期时间：{value}"
                        ))
                    },
                )?,
            )
        };
        self.datetimes[index] = parsed;
        self.datetime_seen[index] = true;
        Ok(parsed)
    }
}

pub(super) struct UpdateContext<'a> {
    pub(super) record: &'a StreamRecord,
    pub(super) cache: &'a mut RowCache,
    pub(super) csv_line: usize,
    pub(super) symbols: &'a mut StringInterner,
}

pub(super) fn initial_state(metric: &MetricSpec) -> AggregateState {
    match metric.operation {
        MetricOperation::Count | MetricOperation::NonNullCount { .. } => AggregateState::Count(0),
        MetricOperation::Sum { .. }
        | MetricOperation::AbsoluteSum { .. }
        | MetricOperation::ProductSum { .. } => AggregateState::Sum(Decimal::ZERO),
        MetricOperation::DistinctCount { .. } => AggregateState::Distinct(HashSet::new()),
        MetricOperation::Average { .. } | MetricOperation::DurationAverage { .. } => {
            AggregateState::Average {
                sum: Decimal::ZERO,
                count: 0,
            }
        }
        MetricOperation::Minimum { .. } | MetricOperation::Maximum { .. } => {
            AggregateState::Extreme(None)
        }
        MetricOperation::Difference { .. } | MetricOperation::Ratio { .. } => AggregateState::None,
    }
}

pub(super) fn update_metric(
    metric: &MetricSpec,
    state: &mut AggregateState,
    context: &mut UpdateContext<'_>,
    enabled: bool,
) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }
    let record = context.record;
    let csv_line = context.csv_line;
    match (&metric.operation, state) {
        (MetricOperation::Count, AggregateState::Count(count)) => {
            *count = count
                .checked_add(1)
                .ok_or_else(|| language_error("计数溢出"))?;
        }
        (MetricOperation::NonNullCount { index }, AggregateState::Count(count)) => {
            if !record.get(*index).unwrap_or("").trim().is_empty() {
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| language_error("非空计数溢出"))?;
            }
        }
        (MetricOperation::Sum { column, index, .. }, AggregateState::Sum(sum)) => {
            if let Some(value) = context.cache.number(record, *index, column, csv_line)? {
                *sum = checked_add(*sum, value)?;
            }
        }
        (MetricOperation::AbsoluteSum { column, index, .. }, AggregateState::Sum(sum)) => {
            if let Some(value) = context.cache.number(record, *index, column, csv_line)? {
                let absolute = if value.is_sign_negative() {
                    Decimal::ZERO
                        .checked_sub(value)
                        .ok_or_else(|| language_error("绝对值运算溢出"))?
                } else {
                    value
                };
                *sum = checked_add(*sum, absolute)?;
            }
        }
        (
            MetricOperation::ProductSum {
                columns, indices, ..
            },
            AggregateState::Sum(sum),
        ) => {
            let mut product = Decimal::ONE;
            for (column, index) in columns.iter().zip(indices) {
                let Some(value) = context.cache.number(record, *index, column, csv_line)? else {
                    return Ok(());
                };
                product = product
                    .checked_mul(value)
                    .ok_or_else(|| language_error(format!("CSV 第 {csv_line} 行乘积运算溢出")))?;
            }
            *sum = checked_add(*sum, product)?;
        }
        (MetricOperation::DistinctCount { index }, AggregateState::Distinct(values)) => {
            let value = record.get(*index).unwrap_or("").trim();
            if !value.is_empty() {
                values.insert(context.symbols.intern(value)?);
            }
        }
        (
            MetricOperation::Average { column, index, .. },
            AggregateState::Average { sum, count },
        ) => {
            if let Some(value) = context.cache.number(record, *index, column, csv_line)? {
                *sum = checked_add(*sum, value)?;
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| language_error("平均值计数溢出"))?;
            }
        }
        (MetricOperation::Minimum { column, index }, AggregateState::Extreme(current)) => {
            if let Some(value) = context.cache.number(record, *index, column, csv_line)? {
                *current = Some(current.map_or(value, |old| old.min(value)));
            }
        }
        (MetricOperation::Maximum { column, index }, AggregateState::Extreme(current)) => {
            if let Some(value) = context.cache.number(record, *index, column, csv_line)? {
                *current = Some(current.map_or(value, |old| old.max(value)));
            }
        }
        (
            MetricOperation::DurationAverage {
                columns,
                indices,
                divisor,
                ..
            },
            AggregateState::Average { sum, count },
        ) => {
            let start = context
                .cache
                .datetime(record, indices[0], &columns[0], csv_line)?;
            let end = context
                .cache
                .datetime(record, indices[1], &columns[1], csv_line)?;
            if let (Some(start), Some(end)) = (start, end) {
                let milliseconds = end.signed_duration_since(start).num_milliseconds();
                let duration = Decimal::from(milliseconds)
                    .checked_div(*divisor)
                    .ok_or_else(|| language_error("时差平均单位换算溢出"))?;
                *sum = checked_add(*sum, duration)?;
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| language_error("时差平均计数溢出"))?;
            }
        }
        (
            MetricOperation::Difference { .. } | MetricOperation::Ratio { .. },
            AggregateState::None,
        ) => {}
        _ => return Err(language_error("指标聚合状态不匹配")),
    }
    Ok(())
}

pub(super) fn finalize_metric(
    metric: &MetricSpec,
    state: &AggregateState,
    row: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    Ok(match (&metric.operation, state) {
        (
            MetricOperation::Count | MetricOperation::NonNullCount { .. },
            AggregateState::Count(count),
        ) => Value::Number(Decimal::from(*count)),
        (
            MetricOperation::Sum { .. }
            | MetricOperation::AbsoluteSum { .. }
            | MetricOperation::ProductSum { .. },
            AggregateState::Sum(sum),
        ) => Value::Number(*sum),
        (MetricOperation::DistinctCount { .. }, AggregateState::Distinct(values)) => {
            Value::Number(Decimal::from(values.len()))
        }
        (
            MetricOperation::Average { .. } | MetricOperation::DurationAverage { .. },
            AggregateState::Average { sum, count },
        ) => {
            if *count == 0 {
                Value::Null
            } else {
                Value::Number(sum.checked_div(Decimal::from(*count)).ok_or_else(|| {
                    language_error(format!("指标“{}”平均值运算溢出", metric.name))
                })?)
            }
        }
        (
            MetricOperation::Minimum { .. } | MetricOperation::Maximum { .. },
            AggregateState::Extreme(value),
        ) => value.map(Value::Number).unwrap_or(Value::Null),
        (MetricOperation::Difference { left, right }, AggregateState::None) => {
            let Some(left) = row.get(left).and_then(decimal_value) else {
                return Ok(Value::Null);
            };
            let Some(right) = row.get(right).and_then(decimal_value) else {
                return Ok(Value::Null);
            };
            Value::Number(
                left.checked_sub(right)
                    .ok_or_else(|| language_error(format!("指标“{}”差值运算溢出", metric.name)))?,
            )
        }
        (
            MetricOperation::Ratio {
                numerator,
                denominator,
                percent,
            },
            AggregateState::None,
        ) => {
            let Some(numerator) = row.get(numerator).and_then(decimal_value) else {
                return Ok(Value::Null);
            };
            let Some(denominator) = row.get(denominator).and_then(decimal_value) else {
                return Ok(Value::Null);
            };
            if denominator.is_zero() {
                Value::Null
            } else {
                let ratio = numerator
                    .checked_div(denominator)
                    .ok_or_else(|| language_error(format!("指标“{}”比率运算溢出", metric.name)))?;
                Value::Number(if *percent {
                    ratio.checked_mul(Decimal::ONE_HUNDRED).ok_or_else(|| {
                        language_error(format!("指标“{}”百分比运算溢出", metric.name))
                    })?
                } else {
                    ratio
                })
            }
        }
        _ => return Err(language_error("指标结果状态不匹配")),
    })
}

fn checked_add(left: Decimal, right: Decimal) -> Result<Decimal, String> {
    left.checked_add(right)
        .ok_or_else(|| language_error("流式汇总求和溢出"))
}

fn decimal_value(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(value) => Some(*value),
        Value::Float(value) => Decimal::from_str_exact(&value.to_string()).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, operation: MetricOperation) -> MetricSpec {
        MetricSpec {
            name: name.to_string(),
            operation,
        }
    }

    fn feed(metric: &MetricSpec, state: &mut AggregateState, rows: &[&[&str]], enabled: bool) {
        let columns = rows.first().map_or(0, |row| row.len());
        let mut cache = RowCache::new(columns);
        let mut symbols = StringInterner::default();
        for (index, row) in rows.iter().enumerate() {
            let record = StreamRecord::from_fields(row.iter().map(|field| field.to_string()));
            cache.reset();
            let mut context = UpdateContext {
                record: &record,
                cache: &mut cache,
                csv_line: index + 2,
                symbols: &mut symbols,
            };
            update_metric(metric, state, &mut context, enabled).unwrap();
        }
    }

    fn finalize(metric: &MetricSpec, state: &AggregateState) -> Value {
        finalize_metric(metric, state, &BTreeMap::new()).unwrap()
    }

    #[test]
    fn counting_metrics_ignore_blank_cells_only_when_non_null_counting() {
        let total = spec("记录数", MetricOperation::Count);
        let mut state = initial_state(&total);
        feed(&total, &mut state, &[&["1"], &[""], &["3"]], true);
        assert_eq!(finalize(&total, &state).to_string(), "3");

        let present = spec("非空", MetricOperation::NonNullCount { index: 0 });
        let mut state = initial_state(&present);
        feed(&present, &mut state, &[&["1"], &[""], &["3"]], true);
        assert_eq!(finalize(&present, &state).to_string(), "2");
    }

    #[test]
    fn numeric_metrics_accumulate_sums_extremes_and_averages() {
        let sum = spec(
            "求和",
            MetricOperation::Sum {
                column: "值".into(),
                index: 0,
                dedup: None,
            },
        );
        let mut state = initial_state(&sum);
        feed(&sum, &mut state, &[&["2"], &["-3"], &[""]], true);
        assert_eq!(finalize(&sum, &state).to_string(), "-1");

        let absolute = spec(
            "绝对值求和",
            MetricOperation::AbsoluteSum {
                column: "值".into(),
                index: 0,
                dedup: None,
            },
        );
        let mut state = initial_state(&absolute);
        feed(&absolute, &mut state, &[&["2"], &["-3"]], true);
        assert_eq!(finalize(&absolute, &state).to_string(), "5");

        let average = spec(
            "平均值",
            MetricOperation::Average {
                column: "值".into(),
                index: 0,
                dedup: None,
            },
        );
        let mut state = initial_state(&average);
        feed(&average, &mut state, &[&["2"], &["4"], &[""]], true);
        assert_eq!(finalize(&average, &state).to_string(), "3");

        let minimum = spec(
            "最小值",
            MetricOperation::Minimum {
                column: "值".into(),
                index: 0,
            },
        );
        let mut state = initial_state(&minimum);
        feed(&minimum, &mut state, &[&["2"], &["4"]], true);
        assert_eq!(finalize(&minimum, &state).to_string(), "2");

        let maximum = spec(
            "最大值",
            MetricOperation::Maximum {
                column: "值".into(),
                index: 0,
            },
        );
        let mut state = initial_state(&maximum);
        feed(&maximum, &mut state, &[&["2"], &["4"]], true);
        assert_eq!(finalize(&maximum, &state).to_string(), "4");
    }

    #[test]
    fn product_sum_skips_rows_with_any_missing_factor() {
        let metric = spec(
            "乘积求和",
            MetricOperation::ProductSum {
                columns: vec!["单价".into(), "数量".into()],
                indices: vec![0, 1],
                dedup: None,
            },
        );
        let mut state = initial_state(&metric);
        feed(&metric, &mut state, &[&["3", "2"], &["5", ""]], true);
        assert_eq!(finalize(&metric, &state).to_string(), "6");
    }

    #[test]
    fn distinct_count_interns_non_empty_values_only() {
        let metric = spec("去重计数", MetricOperation::DistinctCount { index: 0 });
        let mut state = initial_state(&metric);
        feed(
            &metric,
            &mut state,
            &[&["甲"], &["甲"], &[""], &["乙"]],
            true,
        );
        assert_eq!(finalize(&metric, &state).to_string(), "2");
    }

    #[test]
    fn duration_average_converts_to_the_requested_unit() {
        let metric = spec(
            "时差平均",
            MetricOperation::DurationAverage {
                columns: ["开始".into(), "结束".into()],
                indices: [0, 1],
                divisor: Decimal::from(3_600_000),
                dedup: None,
            },
        );
        let mut state = initial_state(&metric);
        feed(
            &metric,
            &mut state,
            &[
                &["2026-01-01 00:00:00", "2026-01-01 02:00:00"],
                &["2026-01-01 00:00:00", "2026-01-01 04:00:00"],
                &["2026-01-01 00:00:00", ""],
            ],
            true,
        );
        assert_eq!(finalize(&metric, &state).to_string(), "3");
    }

    #[test]
    fn disabled_updates_leave_the_state_untouched_for_deduplicated_metrics() {
        let metric = spec(
            "去重求和",
            MetricOperation::Sum {
                column: "值".into(),
                index: 0,
                dedup: Some(1),
            },
        );
        assert_eq!(metric.dedup_index(), Some(1));
        let mut state = initial_state(&metric);
        feed(&metric, &mut state, &[&["7", "甲"]], false);
        assert_eq!(finalize(&metric, &state).to_string(), "0");
    }

    #[test]
    fn empty_averages_and_extremes_finalize_to_null() {
        let average = spec(
            "平均值",
            MetricOperation::Average {
                column: "值".into(),
                index: 0,
                dedup: None,
            },
        );
        assert!(matches!(
            finalize(&average, &initial_state(&average)),
            Value::Null
        ));
        let minimum = spec(
            "最小值",
            MetricOperation::Minimum {
                column: "值".into(),
                index: 0,
            },
        );
        assert!(matches!(
            finalize(&minimum, &initial_state(&minimum)),
            Value::Null
        ));
    }

    #[test]
    fn derived_metrics_read_already_finalized_columns_and_guard_zero_denominators() {
        let difference = spec(
            "毛利额",
            MetricOperation::Difference {
                left: "销售额".into(),
                right: "成本额".into(),
            },
        );
        let mut row = BTreeMap::new();
        row.insert("销售额".to_string(), Value::Number(Decimal::from(20)));
        row.insert("成本额".to_string(), Value::Number(Decimal::from(8)));
        assert_eq!(
            finalize_metric(&difference, &AggregateState::None, &row)
                .unwrap()
                .to_string(),
            "12"
        );

        let percent = spec(
            "毛利率",
            MetricOperation::Ratio {
                numerator: "成本额".into(),
                denominator: "销售额".into(),
                percent: true,
            },
        );
        assert_eq!(
            finalize_metric(&percent, &AggregateState::None, &row)
                .unwrap()
                .to_string(),
            "40"
        );

        row.insert("销售额".to_string(), Value::Number(Decimal::ZERO));
        assert!(matches!(
            finalize_metric(&percent, &AggregateState::None, &row).unwrap(),
            Value::Null
        ));
        row.remove("销售额");
        assert!(matches!(
            finalize_metric(&difference, &AggregateState::None, &row).unwrap(),
            Value::Null
        ));
    }

    #[test]
    fn malformed_cells_report_the_csv_line_and_column() {
        let metric = spec(
            "求和",
            MetricOperation::Sum {
                column: "值".into(),
                index: 0,
                dedup: None,
            },
        );
        let mut cache = RowCache::new(1);
        let record = StreamRecord::from_fields(["非数字".to_string()]);
        let mut symbols = StringInterner::default();
        let mut context = UpdateContext {
            record: &record,
            cache: &mut cache,
            csv_line: 7,
            symbols: &mut symbols,
        };
        let error =
            update_metric(&metric, &mut initial_state(&metric), &mut context, true).unwrap_err();
        assert!(error.contains("CSV 第 7 行的列“值”不是数字"), "{error}");
    }

    #[test]
    fn mismatched_state_and_operation_pairs_are_rejected() {
        let metric = spec("记录数", MetricOperation::Count);
        let mut state = AggregateState::Sum(Decimal::ZERO);
        let record = StreamRecord::from_fields(["1".to_string()]);
        let mut cache = RowCache::new(1);
        let mut symbols = StringInterner::default();
        let mut context = UpdateContext {
            record: &record,
            cache: &mut cache,
            csv_line: 2,
            symbols: &mut symbols,
        };
        assert!(update_metric(&metric, &mut state, &mut context, true)
            .unwrap_err()
            .contains("指标聚合状态不匹配"));
        assert!(finalize_metric(&metric, &state, &BTreeMap::new())
            .unwrap_err()
            .contains("指标结果状态不匹配"));
    }

    #[test]
    fn deduplication_sets_are_unique_and_sorted_by_column() {
        let metrics = vec![
            spec(
                "甲",
                MetricOperation::Sum {
                    column: "值".into(),
                    index: 0,
                    dedup: Some(3),
                },
            ),
            spec(
                "乙",
                MetricOperation::Average {
                    column: "值".into(),
                    index: 0,
                    dedup: Some(1),
                },
            ),
            spec(
                "丙",
                MetricOperation::AbsoluteSum {
                    column: "值".into(),
                    index: 0,
                    dedup: Some(3),
                },
            ),
            spec("丁", MetricOperation::Count),
        ];
        let sets = deduplication_sets(&metrics, &[0, 1, 2, 3]);
        assert_eq!(
            sets.iter()
                .map(|state| state.column_index)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
    }
}
