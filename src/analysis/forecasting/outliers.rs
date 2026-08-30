use super::{named_f64, named_text, EPSILON};
use crate::analysis::statistics::{mean, median_absolute_deviation, quantile, variance};
use crate::analysis::{array_value, clean, error, to_array, vector};
use crate::value::Value;
use std::collections::BTreeMap;

pub(super) struct OutlierAnalysis {
    pub(super) method: String,
    pub(super) lower: f64,
    pub(super) upper: f64,
    pub(super) flags: Vec<bool>,
}

pub(in crate::analysis) fn detect_outliers(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = demand_values(value, false)?;
    let analysis = analyze_outliers(&values, named)?;
    let abnormal_indices = analysis
        .flags
        .iter()
        .enumerate()
        .filter(|(_, flag)| **flag)
        .map(|(index, _)| (index + 1) as f64)
        .collect::<Vec<_>>();
    let abnormal_values = values
        .iter()
        .zip(&analysis.flags)
        .filter(|(_, flag)| **flag)
        .map(|(value, _)| *value)
        .collect::<Vec<_>>();
    let retained = values
        .iter()
        .zip(&analysis.flags)
        .filter(|(_, flag)| !**flag)
        .map(|(value, _)| *value)
        .collect::<Vec<_>>();
    let winsorized = values
        .iter()
        .map(|value| value.clamp(analysis.lower, analysis.upper))
        .collect::<Vec<_>>();
    Ok(Value::record(BTreeMap::from([
        ("方法".into(), Value::Text(analysis.method)),
        ("下界".into(), Value::Float(analysis.lower)),
        ("上界".into(), Value::Float(analysis.upper)),
        (
            "异常数量".into(),
            Value::Float(abnormal_indices.len() as f64),
        ),
        ("异常索引".into(), array_value(vector(abnormal_indices))),
        ("异常值".into(), array_value(vector(abnormal_values))),
        ("保留值".into(), array_value(vector(retained))),
        ("缩尾值".into(), array_value(vector(winsorized))),
        (
            "掩码".into(),
            Value::list(analysis.flags.into_iter().map(Value::Bool).collect()),
        ),
    ])))
}

pub(in crate::analysis) fn exclude_outliers(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = demand_values(value, false)?;
    let analysis = analyze_outliers(&values, named)?;
    Ok(array_value(vector(
        values
            .into_iter()
            .zip(analysis.flags)
            .filter(|(_, flag)| !*flag)
            .map(|(value, _)| value)
            .collect(),
    )))
}

pub(in crate::analysis) fn winsorize_outliers(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = demand_values(value, false)?;
    let analysis = analyze_outliers(&values, named)?;
    Ok(array_value(vector(
        values
            .into_iter()
            .map(|value| value.clamp(analysis.lower, analysis.upper))
            .collect(),
    )))
}

pub(super) fn preprocess_outliers(
    values: &[f64],
    named: &BTreeMap<String, Value>,
) -> Result<(Vec<f64>, usize), String> {
    let handling = named_text(named, "异常处理", "缩尾");
    if !matches!(handling.as_str(), "不处理" | "排除" | "缩尾") {
        return Err(error("异常处理只支持“不处理”“排除”或“缩尾”"));
    }
    if values.len() < 2 || handling == "不处理" {
        return Ok((values.to_vec(), 0));
    }
    let analysis = analyze_outliers(values, named)?;
    let count = analysis.flags.iter().filter(|flag| **flag).count();
    let output = match handling.as_str() {
        "排除" => values
            .iter()
            .zip(&analysis.flags)
            .filter(|(_, flag)| !**flag)
            .map(|(value, _)| *value)
            .collect(),
        "缩尾" => values
            .iter()
            .map(|value| value.clamp(analysis.lower, analysis.upper))
            .collect(),
        _ => return Err(error("异常处理只支持“不处理”“排除”或“缩尾”")),
    };
    Ok((output, count))
}

pub(super) fn preprocess_forecast_values(
    values: &[f64],
    train_end: usize,
    named: &BTreeMap<String, Value>,
) -> Result<(Vec<f64>, usize), String> {
    let handling = named_text(named, "异常处理", "缩尾");
    if handling == "排除" {
        return Err(error(
            "需求预测不能排除周期，请使用“不处理”或“缩尾”保持时间轴",
        ));
    }
    if !matches!(handling.as_str(), "不处理" | "缩尾") {
        return Err(error("需求预测的异常处理只支持“不处理”或“缩尾”"));
    }
    if handling == "不处理" || train_end < 2 {
        return Ok((values.to_vec(), 0));
    }
    let analysis = analyze_outliers(&values[..train_end], named)?;
    let count = values
        .iter()
        .filter(|value| **value < analysis.lower || **value > analysis.upper)
        .count();
    Ok((
        values
            .iter()
            .map(|value| value.clamp(analysis.lower, analysis.upper))
            .collect(),
        count,
    ))
}

pub(super) fn analyze_outliers(
    values: &[f64],
    named: &BTreeMap<String, Value>,
) -> Result<OutlierAnalysis, String> {
    if values.len() < 2 {
        return Err(error("异常值分析至少需要两个有效数据"));
    }
    let method = named_text(named, "方法", "四分位距");
    let default_threshold = if method == "修正Z分数" {
        3.5
    } else if method == "Z分数" {
        3.0
    } else {
        1.5
    };
    let threshold = named_f64(named, "阈值", default_threshold)?;
    if threshold <= 0.0 {
        return Err(error("异常值阈值必须大于零"));
    }
    let (lower, upper) = match method.as_str() {
        "四分位距" | "IQR" => {
            let mut lower_values = values.to_vec();
            let mut upper_values = values.to_vec();
            let q1 = quantile(&mut lower_values, 0.25);
            let q3 = quantile(&mut upper_values, 0.75);
            let range = q3 - q1;
            (q1 - threshold * range, q3 + threshold * range)
        }
        "Z分数" => {
            let center = mean(values);
            let deviation = variance(values, 0).sqrt();
            (
                center - threshold * deviation,
                center + threshold * deviation,
            )
        }
        "修正Z分数" => {
            let mut ordered = values.to_vec();
            let center = quantile(&mut ordered, 0.5);
            let deviation = median_absolute_deviation(values);
            if deviation <= EPSILON {
                (center, center)
            } else {
                let distance = threshold * deviation / 0.674_489_750_196_081_7;
                (center - distance, center + distance)
            }
        }
        _ => return Err(error(format!("不支持异常值方法“{method}”"))),
    };
    if !lower.is_finite() || !upper.is_finite() {
        return Err(error("异常值边界不是有限数值"));
    }
    Ok(OutlierAnalysis {
        method,
        lower,
        upper,
        flags: values
            .iter()
            .map(|value| *value < lower || *value > upper)
            .collect(),
    })
}

pub(super) fn demand_values(value: &Value, nonnegative: bool) -> Result<Vec<f64>, String> {
    let values = clean(&to_array(value)?);
    if values.is_empty() {
        return Err(error("有效数据不能为空"));
    }
    if nonnegative && values.iter().any(|value| *value < 0.0) {
        return Err(error("需求数据必须是非负数"));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::super::forecast_models::forecast_from;
    use super::*;

    fn values(items: &[f64]) -> Value {
        Value::list(items.iter().copied().map(Value::Float).collect())
    }

    #[test]
    fn outlier_methods_preserve_order_and_winsorize() {
        let source = values(&[10.0, 11.0, 9.0, 10.0, 100.0]);
        let named = BTreeMap::new();
        let Value::Record(result) = detect_outliers(&source, &named).unwrap() else {
            panic!()
        };
        assert_eq!(result.borrow()["异常数量"].to_string(), "1");
        let cleaned = exclude_outliers(&source, &named).unwrap();
        assert_eq!(cleaned.iter_values().unwrap().len(), 4);
        let winsorized = winsorize_outliers(&source, &named).unwrap();
        assert!(winsorized.iter_values().unwrap()[4].as_f64().unwrap() < 100.0);
    }

    #[test]
    fn forecast_outlier_fit_uses_only_the_training_prefix() {
        let named = BTreeMap::from([
            ("异常处理".into(), Value::Text("缩尾".into())),
            ("方法".into(), Value::Text("四分位距".into())),
        ]);
        let ordinary_suffix = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0];
        let extreme_suffix = [
            10.0,
            11.0,
            12.0,
            13.0,
            1_000_000.0,
            -1_000_000.0,
            16.0,
            17.0,
        ];
        let (ordinary, _) = preprocess_forecast_values(&ordinary_suffix, 4, &named).unwrap();
        let (extreme, _) = preprocess_forecast_values(&extreme_suffix, 4, &named).unwrap();
        assert_eq!(&ordinary[..4], &extreme[..4]);
        assert_eq!(ordinary.len(), ordinary_suffix.len());
        assert_eq!(extreme.len(), extreme_suffix.len());

        let excluded = BTreeMap::from([("异常处理".into(), Value::Text("排除".into()))]);
        let error = match forecast_from(&ordinary_suffix, &excluded, 100) {
            Ok(_) => panic!("排除策略应被拒绝"),
            Err(error) => error,
        };
        assert!(error.contains("不能排除周期"));
    }

    #[test]
    fn winsorization_keeps_values_on_the_boundary_and_clamps_outside_values() {
        let named = BTreeMap::from([
            ("异常处理".into(), Value::Text("缩尾".into())),
            ("方法".into(), Value::Text("四分位距".into())),
            ("阈值".into(), Value::Float(1.0)),
        ]);
        // 训练段 [0, 2, 4, 6] 的线性插值 Q1=1.5、Q3=4.5，边界恰为 [-1.5, 7.5]。
        let (cleaned, count) =
            preprocess_forecast_values(&[0.0, 2.0, 4.0, 6.0, 7.5, 8.0], 4, &named).unwrap();
        assert_eq!(count, 1, "恰在上界的值不应算异常");
        assert_eq!(cleaned, vec![0.0, 2.0, 4.0, 6.0, 7.5, 7.5]);
    }

    #[test]
    fn modified_z_score_detects_deviation_when_mad_is_zero() {
        let named = BTreeMap::from([("方法".into(), Value::Text("修正Z分数".into()))]);
        let Value::Record(result) =
            detect_outliers(&values(&[5.0, 5.0, 5.0, 5.0, 9.0]), &named).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.borrow()["异常数量"].to_string(), "1");
    }
}
