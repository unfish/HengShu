use super::{array_value, error, value_usize, vector};
use crate::value::{Table, Value};
use chrono::{Datelike, Duration, NaiveDate};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const EPSILON: f64 = 1e-12;
const FORECAST_INTERVAL_CONFIDENCE: f64 = 0.95;
const FORECAST_INTERVAL_Z: f64 = 1.96;
const FORECAST_INTERVAL_METHOD: &str = "验证RMSE正态近似";

mod forecast_models;
mod inventory;
mod outliers;
mod stability;

pub(super) use forecast_models::forecast_accuracy;
use forecast_models::{forecast_from, forecast_model, ForecastOutcome};
pub(super) use inventory::safety_stock;
use inventory::{inventory_from, inventory_record};
use outliers::{demand_values, preprocess_outliers};
pub(super) use outliers::{detect_outliers, exclude_outliers, winsorize_outliers};
pub(super) use stability::demand_stability;
use stability::stability_from;

pub(super) fn demand_forecast(
    value: &Value,
    named: &BTreeMap<String, Value>,
    collection_limit: usize,
) -> Result<Value, String> {
    let values = demand_values(value, true)?;
    let outcome = forecast_from(&values, named, collection_limit)?;
    Ok(forecast_outcome_value(outcome))
}

pub(super) fn batch_demand_forecast(
    value: &Value,
    named: &BTreeMap<String, Value>,
    collection_limit: usize,
) -> Result<Value, String> {
    let Value::Table(table) = value else {
        return Err(error("批量需求预测需要周销量表格"));
    };
    let product_columns = named_columns(named, "商品列", &["商品"], collection_limit)?;
    let period_column = named_text(named, "周期列", "周");
    let demand_column = named_text(named, "销量列", "销量");
    let only_stable = named.get("只保留稳定").is_none_or(Value::truth);
    let only_reliable = named.get("只保留可靠预测").is_some_and(Value::truth);
    let maximum_validation_wape = named_f64(named, "最大验证WAPE", 50.0)?;
    if maximum_validation_wape < 0.0 {
        return Err(error("最大验证WAPE必须是非负数"));
    }
    let rows = &table.borrow().rows;
    if rows.is_empty() {
        return Err(error("批量需求预测的周销量表格不能为空"));
    }

    let mut groups: BTreeMap<Vec<String>, BTreeMap<NaiveDate, f64>> = BTreeMap::new();
    let mut periods = BTreeSet::new();
    for row in rows {
        let key = product_columns
            .iter()
            .map(|column| {
                row.get(column)
                    .map(ToString::to_string)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| error(format!("批量需求预测缺少商品列“{column}”")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let period_text = row
            .get(&period_column)
            .map(ToString::to_string)
            .ok_or_else(|| error(format!("批量需求预测缺少周期列“{period_column}”")))?;
        let period = parse_week(&period_text, &period_column)?;
        let demand = row
            .get(&demand_column)
            .ok_or_else(|| error(format!("批量需求预测缺少销量列“{demand_column}”")))?
            .as_f64()
            .map_err(|_| error(format!("销量列“{demand_column}”必须是数字")))?;
        if !demand.is_finite() || demand < 0.0 {
            return Err(error(format!("销量列“{demand_column}”必须是有限非负数")));
        }
        if !groups.contains_key(&key) && groups.len() >= collection_limit {
            return Err(error("批量需求预测的商品数量超过集合资源限制"));
        }
        if !periods.contains(&period) && periods.len() >= collection_limit {
            return Err(error("批量需求预测的周期数量超过集合资源限制"));
        }
        let slot = groups.entry(key).or_default().entry(period).or_default();
        *slot += demand;
        if !slot.is_finite() {
            return Err(error("批量需求预测的周销量求和溢出"));
        }
        periods.insert(period);
    }
    let start = *periods
        .first()
        .ok_or_else(|| error("批量需求预测没有有效周期"))?;
    let end = *periods
        .last()
        .ok_or_else(|| error("批量需求预测没有有效周期"))?;
    let timeline = weekly_timeline(start, end, collection_limit)?;

    let mut output = Vec::new();
    for (key, weekly) in groups {
        let values = timeline
            .iter()
            .map(|period| weekly.get(period).copied().unwrap_or(0.0))
            .collect::<Vec<_>>();
        let stability = stability_from(&values, named)?;
        if only_stable && !stability.stable {
            continue;
        }
        let forecast = forecast_from(&values, named, collection_limit)?;
        let reliable = forecast
            .validation_accuracy
            .wape
            .is_some_and(|value| value <= maximum_validation_wape);
        if only_reliable && !reliable {
            continue;
        }
        let inventory = inventory_from(&forecast.cleaned, Some(&forecast.forecast), named)?;
        let mut row = product_columns
            .iter()
            .cloned()
            .zip(key.into_iter().map(Value::Text))
            .collect::<BTreeMap<_, _>>();
        row.extend(stability.to_record());
        row.extend(inventory_record(&inventory));
        row.insert(
            "开始周".into(),
            Value::Text(start.format("%Y-%m-%d").to_string()),
        );
        row.insert(
            "结束周".into(),
            Value::Text(end.format("%Y-%m-%d").to_string()),
        );
        row.insert(
            "异常周期数".into(),
            Value::Float(forecast.outlier_count as f64),
        );
        row.insert("最佳模型".into(), Value::Text(forecast.model.name().into()));
        row.insert("模型参数".into(), Value::Text(forecast.model.parameters()));
        row.insert(
            "区间置信度".into(),
            Value::Float(FORECAST_INTERVAL_CONFIDENCE),
        );
        row.insert(
            "区间方法".into(),
            Value::Text(FORECAST_INTERVAL_METHOD.into()),
        );
        row.insert(
            "验证WAPE".into(),
            optional_float(forecast.validation_accuracy.wape),
        );
        row.insert(
            "验证RMSE".into(),
            Value::Float(forecast.validation_accuracy.rmse),
        );
        row.insert(
            "验证偏差".into(),
            Value::Float(forecast.validation_accuracy.bias),
        );
        row.insert("预测可靠".into(), Value::Bool(reliable));
        row.insert(
            "预测质量".into(),
            Value::Text(forecast.validation_accuracy.quality().into()),
        );
        row.insert(
            "未来预测".into(),
            Value::list(
                forecast
                    .forecast
                    .iter()
                    .copied()
                    .map(Value::Float)
                    .collect(),
            ),
        );
        row.insert(
            "预测下限".into(),
            Value::list(
                forecast
                    .forecast
                    .iter()
                    .map(|value| {
                        Value::Float(
                            (value - FORECAST_INTERVAL_Z * forecast.validation_accuracy.rmse)
                                .max(0.0),
                        )
                    })
                    .collect(),
            ),
        );
        row.insert(
            "预测上限".into(),
            Value::list(
                forecast
                    .forecast
                    .iter()
                    .map(|value| {
                        Value::Float(
                            value + FORECAST_INTERVAL_Z * forecast.validation_accuracy.rmse,
                        )
                    })
                    .collect(),
            ),
        );
        row.insert(
            "滚动验证".into(),
            Value::Table(Rc::new(RefCell::new(Table {
                rows: forecast.rolling.clone(),
                ..Default::default()
            }))),
        );
        row.insert(
            "未来预测总量".into(),
            Value::Float(forecast.forecast.iter().sum()),
        );
        for evaluation in &forecast.evaluations {
            let model_name = evaluation.model.name();
            row.insert(
                format!("{model_name}WAPE"),
                optional_float(evaluation.accuracy.wape),
            );
            row.insert(
                format!("{model_name}RMSE"),
                Value::Float(evaluation.accuracy.rmse),
            );
            let candidate_forecast = forecast_model(
                &evaluation.model,
                &forecast.cleaned,
                forecast.forecast.len(),
            )?;
            row.insert(
                format!("{model_name}预测总量"),
                Value::Float(candidate_forecast.iter().map(|value| value.max(0.0)).sum()),
            );
        }
        if output.len() >= collection_limit {
            return Err(error("批量需求预测结果超过集合资源限制"));
        }
        output.push(row);
    }
    output.sort_by(|left, right| {
        let left = left
            .get("平均周期销量")
            .and_then(|value| value.as_f64().ok())
            .unwrap_or(0.0);
        let right = right
            .get("平均周期销量")
            .and_then(|value| value.as_f64().ok())
            .unwrap_or(0.0);
        right.total_cmp(&left)
    });
    Ok(Value::Table(Rc::new(RefCell::new(Table {
        rows: output,
        ..Default::default()
    }))))
}

fn forecast_outcome_value(outcome: ForecastOutcome) -> Value {
    let evaluations = outcome
        .evaluations
        .iter()
        .map(|evaluation| {
            let mut row = evaluation.accuracy.to_record();
            row.insert("模型".into(), Value::Text(evaluation.model.name().into()));
            row.insert("参数".into(), Value::Text(evaluation.model.parameters()));
            row
        })
        .collect::<Vec<_>>();
    Value::record(BTreeMap::from([
        ("最佳模型".into(), Value::Text(outcome.model.name().into())),
        ("模型参数".into(), Value::Text(outcome.model.parameters())),
        (
            "区间置信度".into(),
            Value::Float(FORECAST_INTERVAL_CONFIDENCE),
        ),
        (
            "区间方法".into(),
            Value::Text(FORECAST_INTERVAL_METHOD.into()),
        ),
        ("预测".into(), array_value(vector(outcome.forecast.clone()))),
        (
            "模型评估".into(),
            Value::Table(Rc::new(RefCell::new(Table {
                rows: evaluations,
                ..Default::default()
            }))),
        ),
        (
            "验证指标".into(),
            Value::record(outcome.validation_accuracy.to_record()),
        ),
        (
            "预测质量".into(),
            Value::Text(outcome.validation_accuracy.quality().into()),
        ),
        (
            "异常周期数".into(),
            Value::Float(outcome.outlier_count as f64),
        ),
        ("建模序列".into(), array_value(vector(outcome.cleaned))),
        (
            "滚动验证".into(),
            Value::Table(Rc::new(RefCell::new(Table {
                rows: outcome.rolling,
                ..Default::default()
            }))),
        ),
        ("滚动汇总".into(), Value::record(outcome.rolling_summary)),
        (
            "预测下限".into(),
            array_value(vector(
                outcome
                    .forecast
                    .iter()
                    .map(|value| {
                        (value - FORECAST_INTERVAL_Z * outcome.validation_accuracy.rmse).max(0.0)
                    })
                    .collect(),
            )),
        ),
        (
            "预测上限".into(),
            array_value(vector(
                outcome
                    .forecast
                    .iter()
                    .map(|value| value + FORECAST_INTERVAL_Z * outcome.validation_accuracy.rmse)
                    .collect(),
            )),
        ),
        ("字段说明".into(), forecast_field_descriptions()),
    ]))
}

fn forecast_field_descriptions() -> Value {
    let rows = [
        ("最佳模型", "按留出验证 WAPE 优先、RMSE 次优选出的模型"),
        ("模型参数", "最佳模型的实际拟合参数或窗口/季节周期"),
        ("预测", "使用完整建模序列生成的非负未来点预测"),
        ("模型评估", "各候选模型在同一留出验证段上的误差指标"),
        (
            "验证指标",
            "最佳模型在留出验证段上的 MAE、RMSE、MAPE、sMAPE、WAPE 和偏差",
        ),
        (
            "预测质量",
            "依据验证 WAPE 划分的可用性等级；无法计算 WAPE 时为无法评估",
        ),
        (
            "异常周期数",
            "按训练段拟合的异常边界作用于完整序列后命中的周期数",
        ),
        ("建模序列", "保留原时间位置并按所选异常策略处理后的序列"),
        ("滚动验证", "最佳模型按时间前缀逐折一步预测的规范化明细"),
        ("滚动汇总", "滚动验证的汇总误差、离散程度、最差折和可靠性"),
        (
            "预测下限",
            "点预测减去 1.96 倍验证 RMSE 后截断到零的近似下限",
        ),
        ("预测上限", "点预测加上 1.96 倍验证 RMSE 的近似上限"),
        ("区间置信度", "预测上下限采用的名义置信度；0.95 表示 95%"),
        (
            "区间方法",
            "区间由留出验证 RMSE 的正态近似构造，不是参数估计置信区间",
        ),
    ]
    .into_iter()
    .map(|(field, description)| {
        BTreeMap::from([
            ("字段".into(), Value::Text(field.into())),
            ("说明".into(), Value::Text(description.into())),
        ])
    })
    .collect();
    Value::Table(Rc::new(RefCell::new(Table {
        rows,
        ..Default::default()
    })))
}

fn named_text(named: &BTreeMap<String, Value>, name: &str, default: &str) -> String {
    named
        .get(name)
        .map(ToString::to_string)
        .unwrap_or_else(|| default.into())
}

fn named_f64(named: &BTreeMap<String, Value>, name: &str, default: f64) -> Result<f64, String> {
    let value = named
        .get(name)
        .map(Value::as_f64)
        .transpose()?
        .unwrap_or(default);
    if value.is_finite() {
        Ok(value)
    } else {
        Err(error(format!("{name}必须是有限数字")))
    }
}

fn named_usize(
    named: &BTreeMap<String, Value>,
    name: &str,
    default: usize,
) -> Result<usize, String> {
    named
        .get(name)
        .map(|value| value_usize(value, name))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn named_columns(
    named: &BTreeMap<String, Value>,
    name: &str,
    defaults: &[&str],
    collection_limit: usize,
) -> Result<Vec<String>, String> {
    let Some(value) = named.get(name) else {
        return Ok(defaults.iter().map(|value| (*value).to_string()).collect());
    };
    match value {
        Value::List(_) | Value::Set(_) | Value::NumericArray(_) => value
            .iter_values_limited(collection_limit)?
            .into_iter()
            .map(|value| Ok(value.to_string()))
            .collect(),
        _ => Ok(vec![value.to_string()]),
    }
}

fn parse_week(value: &str, column: &str) -> Result<NaiveDate, String> {
    let date = value
        .get(..10)
        .ok_or_else(|| error(format!("周期列“{column}”的值“{value}”不是日期")))?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| error(format!("周期列“{column}”的值“{value}”不是有效日期")))?;
    date.checked_sub_signed(Duration::days(date.weekday().num_days_from_monday().into()))
        .ok_or_else(|| error(format!("周期列“{column}”的值“{value}”超出日期支持范围")))
}

fn weekly_timeline(
    start: NaiveDate,
    end: NaiveDate,
    collection_limit: usize,
) -> Result<Vec<NaiveDate>, String> {
    let mut timeline = Vec::new();
    let mut current = start;
    while current <= end {
        if timeline.len() >= collection_limit {
            return Err(error("周序列长度超过集合资源限制"));
        }
        timeline.push(current);
        current = current
            .checked_add_signed(Duration::days(7))
            .ok_or_else(|| error("周序列日期超出支持范围"))?;
    }
    Ok(timeline)
}

fn optional_float(value: Option<f64>) -> Value {
    value.map(Value::Float).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(items: &[f64]) -> Value {
        Value::list(items.iter().copied().map(Value::Float).collect())
    }

    #[test]
    fn demand_forecast_selects_finite_model_and_inventory_bounds() {
        let source = values(&[
            10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0, 21.0,
        ]);
        let outcome = forecast_from(
            &source
                .iter_values()
                .unwrap()
                .iter()
                .map(|value| value.as_f64().unwrap())
                .collect::<Vec<_>>(),
            &BTreeMap::new(),
            100,
        )
        .unwrap();
        assert_eq!(outcome.forecast.len(), 4);
        assert!(outcome
            .forecast
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0));
        let inventory =
            inventory_from(&outcome.cleaned, Some(&outcome.forecast), &BTreeMap::new()).unwrap();
        assert!(inventory.upper >= inventory.lower);
        assert!(inventory.lower >= inventory.safety_stock);
        assert!(inventory.suggested_replenishment.is_none());
        let with_inventory = inventory_from(
            &outcome.cleaned,
            Some(&outcome.forecast),
            &BTreeMap::from([("当前库存".into(), Value::Float(10.0))]),
        )
        .unwrap();
        assert!(with_inventory.suggested_replenishment.is_some());
    }

    #[test]
    fn forecast_result_documents_parameters_and_interval_semantics() {
        let result = demand_forecast(
            &values(&[10.0, 12.0, 11.0, 13.0, 12.0, 14.0, 13.0, 15.0]),
            &BTreeMap::from([
                ("步数".into(), Value::Float(2.0)),
                ("验证期".into(), Value::Float(2.0)),
                ("异常处理".into(), Value::Text("不处理".into())),
            ]),
            100,
        )
        .unwrap();
        let Value::Record(result) = result else {
            panic!("forecast result must be a record")
        };
        let result = result.borrow();
        assert!(!result["模型参数"].to_string().is_empty());
        assert_eq!(result["区间置信度"].as_f64().unwrap(), 0.95);
        assert_eq!(result["区间方法"].to_string(), "验证RMSE正态近似");

        let Value::Table(descriptions) = &result["字段说明"] else {
            panic!("field descriptions must be a normalized table")
        };
        let fields = descriptions
            .borrow()
            .rows
            .iter()
            .map(|row| row["字段"].to_string())
            .collect::<BTreeSet<_>>();
        for field in [
            "最佳模型",
            "模型参数",
            "预测下限",
            "预测上限",
            "区间置信度",
            "区间方法",
        ] {
            assert!(fields.contains(field), "字段说明缺少 {field}");
        }
    }

    #[test]
    fn batch_forecast_fills_missing_weeks_and_filters_unstable_products() {
        let mut rows = Vec::new();
        for week in 0..12 {
            let date = NaiveDate::from_ymd_opt(2026, 1, 5)
                .unwrap()
                .checked_add_signed(Duration::days(week * 7))
                .unwrap();
            rows.push(BTreeMap::from([
                ("商品".into(), Value::Text("稳定商品".into())),
                (
                    "周".into(),
                    Value::Text(date.format("%Y-%m-%d").to_string()),
                ),
                ("销量".into(), Value::Float(10.0 + week as f64 % 2.0)),
            ]));
            if week % 4 == 0 {
                rows.push(BTreeMap::from([
                    ("商品".into(), Value::Text("间歇商品".into())),
                    (
                        "周".into(),
                        Value::Text(date.format("%Y-%m-%d").to_string()),
                    ),
                    ("销量".into(), Value::Float(20.0)),
                ]));
            }
        }
        let table = Value::Table(Rc::new(RefCell::new(Table {
            rows,
            ..Default::default()
        })));
        let Value::Table(result) = batch_demand_forecast(&table, &BTreeMap::new(), 100).unwrap()
        else {
            panic!()
        };
        let result = result.borrow();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["商品"].to_string(), "稳定商品");
        assert_eq!(result.rows[0]["周期数"].to_string(), "12");
        assert_eq!(result.rows[0]["稳定"].to_string(), "是");
    }

    #[test]
    fn batch_forecast_rejects_new_products_and_periods_before_insertion() {
        let two_products = Value::Table(Rc::new(RefCell::new(Table {
            rows: vec![
                BTreeMap::from([
                    ("商品".into(), Value::Text("甲".into())),
                    ("周".into(), Value::Text("2026-01-05".into())),
                    ("销量".into(), Value::Float(1.0)),
                ]),
                BTreeMap::from([
                    ("商品".into(), Value::Text("乙".into())),
                    ("周".into(), Value::Text("2026-01-05".into())),
                    ("销量".into(), Value::Float(1.0)),
                ]),
            ],
            ..Default::default()
        })));
        let product_error = batch_demand_forecast(&two_products, &BTreeMap::new(), 1)
            .err()
            .unwrap();
        assert!(product_error.contains("商品数量"), "{product_error}");

        let two_periods = Value::Table(Rc::new(RefCell::new(Table {
            rows: vec![
                BTreeMap::from([
                    ("商品".into(), Value::Text("甲".into())),
                    ("周".into(), Value::Text("2026-01-05".into())),
                    ("销量".into(), Value::Float(1.0)),
                ]),
                BTreeMap::from([
                    ("商品".into(), Value::Text("甲".into())),
                    ("周".into(), Value::Text("2026-01-12".into())),
                    ("销量".into(), Value::Float(1.0)),
                ]),
            ],
            ..Default::default()
        })));
        let period_error = batch_demand_forecast(&two_periods, &BTreeMap::new(), 1)
            .err()
            .unwrap();
        assert!(period_error.contains("周期数量"), "{period_error}");
    }
}
