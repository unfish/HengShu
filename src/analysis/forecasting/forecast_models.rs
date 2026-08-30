use super::outliers::{demand_values, preprocess_forecast_values};
use super::{error, named_usize, optional_float, EPSILON};
use crate::analysis::statistics::mean;
use crate::value::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) struct Accuracy {
    pub(super) count: usize,
    pub(super) mae: f64,
    pub(super) rmse: f64,
    pub(super) mape: Option<f64>,
    pub(super) smape: f64,
    pub(super) wape: Option<f64>,
    pub(super) bias: f64,
}

impl Accuracy {
    /// WAPE 阈值到预测质量标签的映射：阈值与准确度指标同源，必须一起改。
    pub(super) fn quality(self) -> &'static str {
        match self.wape {
            Some(value) if value <= 20.0 => "优秀",
            Some(value) if value <= 35.0 => "良好",
            Some(value) if value <= 50.0 => "可用",
            Some(_) => "谨慎使用",
            None => "无法评估",
        }
    }

    pub(super) fn selection_score(self) -> f64 {
        self.wape.unwrap_or(self.mae)
    }

    pub(super) fn to_record(self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("样本数".into(), Value::Float(self.count as f64)),
            ("MAE".into(), Value::Float(self.mae)),
            ("RMSE".into(), Value::Float(self.rmse)),
            ("MAPE".into(), optional_float(self.mape)),
            ("sMAPE".into(), Value::Float(self.smape)),
            ("WAPE".into(), optional_float(self.wape)),
            ("偏差".into(), Value::Float(self.bias)),
        ])
    }
}

#[derive(Clone)]
pub(super) enum ForecastModel {
    Naive,
    MovingAverage { window: usize },
    ExponentialSmoothing { alpha: f64 },
    Holt { alpha: f64, beta: f64 },
    SeasonalNaive { period: usize },
}

impl ForecastModel {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Naive => "朴素",
            Self::MovingAverage { .. } => "移动平均",
            Self::ExponentialSmoothing { .. } => "指数平滑",
            Self::Holt { .. } => "霍尔特趋势",
            Self::SeasonalNaive { .. } => "季节朴素",
        }
    }

    pub(super) fn parameters(&self) -> String {
        match self {
            Self::Naive => "无".into(),
            Self::MovingAverage { window } => format!("窗口={window}"),
            Self::ExponentialSmoothing { alpha } => format!("平滑系数={alpha:.2}"),
            Self::Holt { alpha, beta } => {
                format!("水平系数={alpha:.2},趋势系数={beta:.2}")
            }
            Self::SeasonalNaive { period } => format!("季节周期={period}"),
        }
    }
}

pub(super) struct ModelEvaluation {
    pub(super) model: ForecastModel,
    pub(super) accuracy: Accuracy,
}

pub(super) struct ForecastOutcome {
    pub(super) model: ForecastModel,
    pub(super) forecast: Vec<f64>,
    pub(super) evaluations: Vec<ModelEvaluation>,
    pub(super) validation_accuracy: Accuracy,
    pub(super) cleaned: Vec<f64>,
    pub(super) outlier_count: usize,
    pub(super) rolling: Vec<BTreeMap<String, Value>>,
    pub(super) rolling_summary: BTreeMap<String, Value>,
}

pub(in crate::analysis) fn forecast_accuracy(
    actual: &Value,
    predicted: &Value,
) -> Result<Value, String> {
    let actual = demand_values(actual, false)?;
    let predicted = demand_values(predicted, false)?;
    calculate_accuracy(&actual, &predicted).map(|value| Value::record(value.to_record()))
}

pub(super) fn forecast_from(
    values: &[f64],
    named: &BTreeMap<String, Value>,
    collection_limit: usize,
) -> Result<ForecastOutcome, String> {
    if values.len() < 3 {
        return Err(error("需求预测至少需要三个周期"));
    }
    let steps = named_usize(named, "步数", 4)?;
    if steps == 0 || steps > collection_limit {
        return Err(error("预测步数必须大于零且不超过集合资源限制"));
    }
    let requested_validation = named_usize(named, "验证期", 4)?;
    let validation = requested_validation.min(values.len().saturating_sub(2));
    if validation == 0 {
        return Err(error("验证期必须大于零且至少保留两个训练周期"));
    }
    let train_end = values.len() - validation;
    let (cleaned, outlier_count) = preprocess_forecast_values(values, train_end, named)?;
    let train = &cleaned[..train_end];
    let actual = &cleaned[train_end..];
    let window = named_usize(named, "移动窗口", 4)?;
    if window == 0 {
        return Err(error("移动窗口必须大于零"));
    }
    let seasonal_period = named_usize(named, "季节周期", 0)?;
    if seasonal_period == 1 {
        return Err(error("季节周期必须为零或至少为二"));
    }
    let mut models = vec![
        ForecastModel::Naive,
        ForecastModel::MovingAverage {
            window: window.min(train.len()),
        },
        ForecastModel::ExponentialSmoothing {
            alpha: optimize_exponential_alpha(train),
        },
    ];
    if train.len() >= 3 {
        let (alpha, beta) = optimize_holt(train);
        models.push(ForecastModel::Holt { alpha, beta });
    }
    if seasonal_period >= 2 && train.len() >= seasonal_period {
        models.push(ForecastModel::SeasonalNaive {
            period: seasonal_period,
        });
    }
    let mut evaluations = Vec::with_capacity(models.len());
    for model in models {
        let predicted = forecast_model(&model, train, validation)?;
        let accuracy = calculate_accuracy(actual, &predicted)?;
        evaluations.push(ModelEvaluation { model, accuracy });
    }
    let best_index = evaluations
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            left.accuracy
                .selection_score()
                .total_cmp(&right.accuracy.selection_score())
                .then_with(|| left.accuracy.rmse.total_cmp(&right.accuracy.rmse))
        })
        .map(|(index, _)| index)
        .ok_or_else(|| error("没有可用预测模型"))?;
    let model = evaluations[best_index].model.clone();
    let validation_accuracy = evaluations[best_index].accuracy;
    let forecast = forecast_model(&model, &cleaned, steps)?
        .into_iter()
        .map(|value| value.max(0.0))
        .collect::<Vec<_>>();
    let rolling = rolling_validation(&cleaned, validation, collection_limit, &model, named)?;
    Ok(ForecastOutcome {
        model,
        forecast,
        evaluations,
        validation_accuracy,
        cleaned,
        outlier_count,
        rolling: rolling.folds,
        rolling_summary: rolling.summary,
    })
}

/// 滚动验证的逐折明细与汇总；明细直接作为规范化表格进入报告。
#[cfg_attr(test, derive(Debug))]
struct RollingValidation {
    folds: Vec<BTreeMap<String, Value>>,
    summary: BTreeMap<String, Value>,
}

/// 每折用被选中的模型在时间前缀上重新预测一步；参数沿用初始训练集的拟合结果，
/// 因此折间比较的是同一个模型在更多历史上的表现，而不是换了一套超参数。
fn rolling_validation(
    values: &[f64],
    validation: usize,
    limit: usize,
    model: &ForecastModel,
    named: &BTreeMap<String, Value>,
) -> Result<RollingValidation, String> {
    let sliding = match named.get("滚动方式").map(ToString::to_string).as_deref() {
        None | Some("扩展窗口") => false,
        Some("滑动窗口") => true,
        Some(other) => {
            return Err(error(format!(
                "滚动方式只支持扩展窗口或滑动窗口，收到“{other}”"
            )))
        }
    };
    let requested = named_usize(named, "滚动折数", validation)?;
    let folds = requested.min(values.len().saturating_sub(2)).min(limit);
    let start = values.len().saturating_sub(folds);
    if folds == 0 {
        return Ok(RollingValidation {
            folds: Vec::new(),
            summary: BTreeMap::new(),
        });
    }
    let mut rows = Vec::with_capacity(folds);
    let mut absolute_errors = Vec::with_capacity(folds);
    let mut squared_errors = Vec::with_capacity(folds);
    let mut symmetric = Vec::with_capacity(folds);
    let mut actual_total = 0.0f64;
    for index in 0..folds {
        let train_end = start + index;
        let train_start = if sliding {
            train_end.saturating_sub(start)
        } else {
            0
        };
        let history = &values[train_start..train_end];
        if history.is_empty() {
            return Err(error("滚动验证的训练窗口为空"));
        }
        let predicted = forecast_model(model, history, 1)?
            .first()
            .copied()
            .ok_or_else(|| error("滚动验证没有得到预测值"))?
            .max(0.0);
        let actual = values[train_end];
        let bias = predicted - actual;
        let absolute = bias.abs();
        let scale = actual.abs() + predicted.abs();
        let smape = if scale > 0.0 {
            200.0 * absolute / scale
        } else {
            0.0
        };
        if !predicted.is_finite() || !smape.is_finite() {
            return Err(error("滚动验证产生了非有限结果"));
        }
        absolute_errors.push(absolute);
        squared_errors.push(bias * bias);
        symmetric.push(smape);
        actual_total += actual.abs();
        rows.push(BTreeMap::from([
            ("折次".into(), Value::Float((index + 1) as f64)),
            ("训练起点".into(), Value::Float((train_start + 1) as f64)),
            ("训练结束位置".into(), Value::Float(train_end as f64)),
            ("训练样本数".into(), Value::Float(history.len() as f64)),
            ("验证位置".into(), Value::Float((train_end + 1) as f64)),
            ("实际值".into(), Value::Float(actual)),
            ("预测值".into(), Value::Float(predicted)),
            ("偏差".into(), Value::Float(bias)),
            ("MAE".into(), Value::Float(absolute)),
            ("RMSE".into(), Value::Float(absolute)),
            ("sMAPE".into(), Value::Float(smape)),
            (
                "WAPE".into(),
                if actual.abs() > 0.0 {
                    Value::Float(absolute / actual.abs())
                } else {
                    Value::Null
                },
            ),
        ]));
    }
    let count = folds as f64;
    let mean_absolute = absolute_errors.iter().sum::<f64>() / count;
    // 汇总 RMSE 是平方误差均值的平方根，不是逐折 RMSE 的算术平均。
    let aggregate_rmse = (squared_errors.iter().sum::<f64>() / count).sqrt();
    let mean_symmetric = symmetric.iter().sum::<f64>() / count;
    let dispersion = (absolute_errors
        .iter()
        .map(|value| (value - mean_absolute).powi(2))
        .sum::<f64>()
        / count)
        .sqrt();
    // 并列最差时取最早那一折，避免报告出来的折次随迭代顺序漂移。
    let (worst_index, worst_absolute) = absolute_errors.iter().copied().enumerate().fold(
        (0usize, f64::NEG_INFINITY),
        |(worst_index, worst), (index, value)| {
            if value > worst {
                (index, value)
            } else {
                (worst_index, worst)
            }
        },
    );
    // WAPE 用误差总和除以实际值总和，避免小实际值把相对误差放大成无意义的数字。
    let aggregate_wape = if actual_total > 0.0 {
        Some(absolute_errors.iter().sum::<f64>() / actual_total)
    } else {
        None
    };
    let summary = BTreeMap::from([
        ("折数".to_string(), Value::Float(count)),
        (
            "滚动方式".to_string(),
            Value::Text(
                if sliding {
                    "滑动窗口"
                } else {
                    "扩展窗口"
                }
                .into(),
            ),
        ),
        ("平均MAE".to_string(), Value::Float(mean_absolute)),
        ("汇总RMSE".to_string(), Value::Float(aggregate_rmse)),
        ("平均sMAPE".to_string(), Value::Float(mean_symmetric)),
        (
            "汇总WAPE".to_string(),
            aggregate_wape.map(Value::Float).unwrap_or(Value::Null),
        ),
        ("MAE标准差".to_string(), Value::Float(dispersion)),
        ("最差折".to_string(), Value::Float((worst_index + 1) as f64)),
        ("最差折MAE".to_string(), Value::Float(worst_absolute)),
        (
            "可靠性".to_string(),
            aggregate_wape
                .map(|wape| Value::Float((1.0 - wape).clamp(0.0, 1.0)))
                .unwrap_or(Value::Null),
        ),
    ]);
    Ok(RollingValidation {
        folds: rows,
        summary,
    })
}

pub(super) fn forecast_model(
    model: &ForecastModel,
    history: &[f64],
    steps: usize,
) -> Result<Vec<f64>, String> {
    if history.is_empty() {
        return Err(error("预测历史数据不能为空"));
    }
    let forecast = match model {
        ForecastModel::Naive => vec![*history.last().unwrap_or(&0.0); steps],
        ForecastModel::MovingAverage { window } => {
            if *window == 0 || *window > history.len() {
                return Err(error("移动平均窗口超出历史数据范围"));
            }
            let mut extended = history.to_vec();
            let mut output = Vec::with_capacity(steps);
            for _ in 0..steps {
                let value = mean(&extended[extended.len() - window..]);
                output.push(value);
                extended.push(value);
            }
            output
        }
        ForecastModel::ExponentialSmoothing { alpha } => {
            let level = exponential_level(history, *alpha);
            vec![level; steps]
        }
        ForecastModel::Holt { alpha, beta } => {
            if history.len() < 2 {
                return Err(error("霍尔特趋势预测至少需要两个周期"));
            }
            let (level, trend) = holt_state(history, *alpha, *beta);
            (1..=steps)
                .map(|step| level + trend * step as f64)
                .collect()
        }
        ForecastModel::SeasonalNaive { period } => {
            if *period < 2 || history.len() < *period {
                return Err(error("季节朴素预测的历史数据不足一个季节"));
            }
            let mut extended = history.to_vec();
            let mut output = Vec::with_capacity(steps);
            for _ in 0..steps {
                let value = extended[extended.len() - period];
                output.push(value);
                extended.push(value);
            }
            output
        }
    };
    if forecast.iter().any(|value| !value.is_finite()) {
        return Err(error(format!("{}预测产生了非有限结果", model.name())));
    }
    Ok(forecast)
}

fn optimize_exponential_alpha(values: &[f64]) -> f64 {
    (1..=19)
        .map(|step| step as f64 * 0.05)
        .min_by(|left, right| {
            exponential_error(values, *left).total_cmp(&exponential_error(values, *right))
        })
        .unwrap_or(0.3)
}

fn exponential_error(values: &[f64], alpha: f64) -> f64 {
    let mut level = values[0];
    values[1..]
        .iter()
        .map(|actual| {
            let error = actual - level;
            level = alpha * actual + (1.0 - alpha) * level;
            error.powi(2)
        })
        .sum()
}

fn exponential_level(values: &[f64], alpha: f64) -> f64 {
    values[1..].iter().fold(values[0], |level, value| {
        alpha * value + (1.0 - alpha) * level
    })
}

fn optimize_holt(values: &[f64]) -> (f64, f64) {
    let mut best = (0.3, 0.1);
    let mut best_error = f64::INFINITY;
    for alpha_step in 1..=9 {
        for beta_step in 1..=9 {
            let alpha = alpha_step as f64 / 10.0;
            let beta = beta_step as f64 / 10.0;
            let error = holt_error(values, alpha, beta);
            if error < best_error {
                best_error = error;
                best = (alpha, beta);
            }
        }
    }
    best
}

fn holt_error(values: &[f64], alpha: f64, beta: f64) -> f64 {
    let mut level = values[0];
    let mut trend = values[1] - values[0];
    let mut total = 0.0;
    for actual in &values[1..] {
        let prediction = level + trend;
        total += (actual - prediction).powi(2);
        let previous_level = level;
        level = alpha * actual + (1.0 - alpha) * prediction;
        trend = beta * (level - previous_level) + (1.0 - beta) * trend;
    }
    total
}

fn holt_state(values: &[f64], alpha: f64, beta: f64) -> (f64, f64) {
    let mut level = values[0];
    let mut trend = values[1] - values[0];
    for actual in &values[1..] {
        let prediction = level + trend;
        let previous_level = level;
        level = alpha * actual + (1.0 - alpha) * prediction;
        trend = beta * (level - previous_level) + (1.0 - beta) * trend;
    }
    (level, trend)
}

fn calculate_accuracy(actual: &[f64], predicted: &[f64]) -> Result<Accuracy, String> {
    if actual.len() != predicted.len() || actual.is_empty() {
        return Err(error("实际值与预测值必须等长且不能为空"));
    }
    if actual
        .iter()
        .chain(predicted)
        .any(|value| !value.is_finite())
    {
        return Err(error("预测准确度只接受有限数值"));
    }
    let count = actual.len() as f64;
    let errors = actual
        .iter()
        .zip(predicted)
        .map(|(actual, predicted)| predicted - actual)
        .collect::<Vec<_>>();
    let mae = errors.iter().map(|error| error.abs()).sum::<f64>() / count;
    let rmse = (errors.iter().map(|error| error.powi(2)).sum::<f64>() / count).sqrt();
    let percentage_errors = actual
        .iter()
        .zip(&errors)
        .filter(|(actual, _)| actual.abs() > EPSILON)
        .map(|(actual, error)| error.abs() / actual.abs())
        .collect::<Vec<_>>();
    let mape = (!percentage_errors.is_empty())
        .then(|| percentage_errors.iter().sum::<f64>() / percentage_errors.len() as f64 * 100.0);
    let smape_terms = actual
        .iter()
        .zip(predicted)
        .filter_map(|(actual, predicted)| {
            let denominator = actual.abs() + predicted.abs();
            (denominator > EPSILON).then_some(2.0 * (predicted - actual).abs() / denominator)
        })
        .collect::<Vec<_>>();
    let smape = if smape_terms.is_empty() {
        0.0
    } else {
        smape_terms.iter().sum::<f64>() / smape_terms.len() as f64 * 100.0
    };
    let actual_total = actual.iter().map(|value| value.abs()).sum::<f64>();
    let wape = (actual_total > EPSILON)
        .then(|| errors.iter().map(|error| error.abs()).sum::<f64>() / actual_total * 100.0);
    let bias = errors.iter().sum::<f64>() / count;
    if [mae, rmse, smape, bias]
        .into_iter()
        .any(|value| !value.is_finite())
    {
        return Err(error("预测准确度产生了非有限结果"));
    }
    Ok(Accuracy {
        count: actual.len(),
        mae,
        rmse,
        mape,
        smape,
        wape,
        bias,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn number(row: &BTreeMap<String, Value>, name: &str) -> f64 {
        row.get(name)
            .and_then(|value| value.as_f64().ok())
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    /// 序列 [10,12,11,13,12,14]，移动平均窗口 2，扩展窗口三折：
    /// 预测 11.5/12.0/12.5，实际 13/12/14，绝对误差 1.5/0/1.5，全部可手算。
    #[test]
    fn rolling_folds_predict_with_the_selected_model_not_the_previous_observation() {
        let series = [10.0, 12.0, 11.0, 13.0, 12.0, 14.0];
        let RollingValidation { folds, summary } = rolling_validation(
            &series,
            3,
            1_000,
            &ForecastModel::MovingAverage { window: 2 },
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(folds.len(), 3);
        // 旧实现会直接拿上一期观测值 11 当预测；模型参与后应是最近两期均值 11.5。
        assert!((number(&folds[0], "预测值") - 11.5).abs() < 1e-9);
        assert!((number(&folds[1], "预测值") - 12.0).abs() < 1e-9);
        assert!((number(&folds[2], "预测值") - 12.5).abs() < 1e-9);
        assert!((number(&folds[0], "MAE") - 1.5).abs() < 1e-9);
        assert!((number(&folds[1], "MAE")).abs() < 1e-9);
        assert!((number(&folds[0], "偏差") + 1.5).abs() < 1e-9);
        // 扩展窗口下训练集逐折增长。
        assert_eq!(
            folds
                .iter()
                .map(|fold| number(fold, "训练样本数"))
                .collect::<Vec<_>>(),
            vec![3.0, 4.0, 5.0]
        );
        for fold in &folds {
            assert!(fold.contains_key("RMSE"));
            assert!(fold.contains_key("sMAPE"));
            assert!(fold.contains_key("WAPE"));
            assert!(fold.contains_key("验证位置"));
        }

        assert!((number(&summary, "折数") - 3.0).abs() < 1e-9);
        assert!((number(&summary, "平均MAE") - 1.0).abs() < 1e-9);
        // 汇总 RMSE = sqrt((1.5²+0+1.5²)/3) = sqrt(1.5)
        assert!((number(&summary, "汇总RMSE") - 1.5f64.sqrt()).abs() < 1e-9);
        // 汇总 WAPE = 3 / (13+12+14)
        assert!((number(&summary, "汇总WAPE") - 3.0 / 39.0).abs() < 1e-9);
        assert!((number(&summary, "可靠性") - (1.0 - 3.0 / 39.0)).abs() < 1e-9);
        assert!((number(&summary, "最差折") - 1.0).abs() < 1e-9);
        assert!((number(&summary, "最差折MAE") - 1.5).abs() < 1e-9);
        assert!(number(&summary, "MAE标准差") > 0.0);
        assert_eq!(summary["滚动方式"].to_string(), "扩展窗口");
    }

    #[test]
    fn rolling_validation_supports_sliding_window_and_configurable_folds() {
        let series = [10.0, 12.0, 11.0, 13.0, 12.0, 14.0];
        let RollingValidation { folds, summary } = rolling_validation(
            &series,
            3,
            1_000,
            &ForecastModel::Naive,
            &BTreeMap::from([("滚动方式".to_string(), Value::Text("滑动窗口".into()))]),
        )
        .unwrap();
        // 滑动窗口的训练集长度固定，起点逐折前移。
        assert!(folds
            .iter()
            .all(|fold| (number(fold, "训练样本数") - 3.0).abs() < 1e-9));
        assert_eq!(
            folds
                .iter()
                .map(|fold| number(fold, "训练起点"))
                .collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0]
        );
        assert_eq!(summary["滚动方式"].to_string(), "滑动窗口");

        let RollingValidation { folds: fewer, .. } = rolling_validation(
            &series,
            3,
            1_000,
            &ForecastModel::Naive,
            &BTreeMap::from([("滚动折数".to_string(), Value::Float(2.0))]),
        )
        .unwrap();
        assert_eq!(fewer.len(), 2);

        let message = rolling_validation(
            &series,
            3,
            1_000,
            &ForecastModel::Naive,
            &BTreeMap::from([("滚动方式".to_string(), Value::Text("随机".into()))]),
        )
        .unwrap_err();
        assert!(message.contains("滚动方式只支持"), "{message}");
    }

    #[test]
    fn rolling_validation_reports_null_reliability_for_all_zero_demand() {
        let RollingValidation { folds, summary } = rolling_validation(
            &[0.0, 0.0, 0.0, 0.0],
            2,
            1_000,
            &ForecastModel::Naive,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(folds.len(), 2);
        // 实际值全为零时相对误差无定义，必须返回空值而不是 0 或 NaN。
        assert!(matches!(summary["汇总WAPE"], Value::Null));
        assert!(matches!(summary["可靠性"], Value::Null));
        assert!(matches!(folds[0]["WAPE"], Value::Null));
        assert!((number(&summary, "平均MAE")).abs() < 1e-12);
    }

    #[test]
    fn forecast_short_series_has_a_fixed_boundary() {
        let error = forecast_from(&[10.0, 11.0], &BTreeMap::new(), 100)
            .err()
            .expect("two periods must be rejected");
        assert!(error.contains("至少需要三个周期"), "{error}");

        let outcome = forecast_from(
            &[10.0, 11.0, 12.0],
            &BTreeMap::from([("步数".into(), Value::Float(1.0))]),
            100,
        )
        .expect("three periods are the supported minimum");
        assert_eq!(outcome.forecast.len(), 1);
        assert!(outcome.forecast[0].is_finite());
    }

    #[test]
    fn seasonal_naive_repeats_the_fixed_period() {
        let forecast = forecast_model(
            &ForecastModel::SeasonalNaive { period: 4 },
            &[10.0, 20.0, 30.0, 40.0, 11.0, 21.0, 31.0, 41.0],
            6,
        )
        .unwrap();
        assert_eq!(forecast, vec![11.0, 21.0, 31.0, 41.0, 11.0, 21.0]);

        let error = forecast_model(
            &ForecastModel::SeasonalNaive { period: 4 },
            &[10.0, 20.0, 30.0],
            1,
        )
        .unwrap_err();
        assert!(error.contains("不足一个季节"), "{error}");
    }

    #[test]
    fn forecast_limits_steps_and_rolling_folds_before_allocation() {
        let series = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0];
        let error = forecast_from(
            &series,
            &BTreeMap::from([("步数".into(), Value::Float(3.0))]),
            2,
        )
        .err()
        .expect("forecast steps above the collection limit must fail");
        assert!(error.contains("集合资源限制"), "{error}");

        let RollingValidation { folds, .. } = rolling_validation(
            &series,
            4,
            2,
            &ForecastModel::Naive,
            &BTreeMap::from([("滚动折数".into(), Value::Float(10.0))]),
        )
        .unwrap();
        assert_eq!(folds.len(), 2);
    }

    proptest! {
        #[test]
        fn naive_forecast_is_finite_and_repeats_the_last_observation(
            history in proptest::collection::vec(0.0f64..1_000_000.0, 3..64),
            steps in 1usize..32,
        ) {
            let forecast = forecast_model(&ForecastModel::Naive, &history, steps).unwrap();
            prop_assert_eq!(forecast.len(), steps);
            let last = *history.last().unwrap();
            prop_assert!(forecast.iter().all(|value| value.is_finite() && *value == last));
        }
    }

    fn values(items: &[f64]) -> Value {
        Value::list(items.iter().copied().map(Value::Float).collect())
    }

    #[test]
    fn forecast_accuracy_handles_zero_actuals() {
        let Value::Record(result) =
            forecast_accuracy(&values(&[0.0, 10.0]), &values(&[0.0, 8.0])).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.borrow()["MAE"].to_string(), "1");
        assert_eq!(result.borrow()["WAPE"].to_string(), "20");
        assert_eq!(result.borrow()["MAPE"].to_string(), "20");
        let Value::Record(all_zero) =
            forecast_accuracy(&values(&[0.0, 0.0]), &values(&[0.0, 0.0])).unwrap()
        else {
            panic!()
        };
        assert!(matches!(all_zero.borrow()["WAPE"], Value::Null));
    }
}
