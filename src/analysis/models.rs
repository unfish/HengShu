use super::statistics::{mean, variance};
use super::{arg_text, arg_usize, array_value, clean, error, to_array, vector};
use crate::resource::DEFAULT_MAX_COLLECTION_ITEMS;
use crate::value::{
    ClassificationMetrics, NumericArray, RegressionDiagnostics, RegressionResult, Rows, Table,
    Value,
};
use nalgebra::{DMatrix, DVector};
use statrs::distribution::{ChiSquared, ContinuousCDF, FisherSnedecor, Normal, StudentsT};
use std::collections::BTreeMap;
use std::rc::Rc;

pub(super) fn one_way_anova(value: &Value) -> Result<Value, String> {
    let groups = value
        .iter_values_limited(DEFAULT_MAX_COLLECTION_ITEMS)?
        .iter()
        .map(|group| to_array(group).map(|array| clean(&array)))
        .collect::<Result<Vec<_>, _>>()?;
    if groups.len() < 2 || groups.iter().any(|group| group.is_empty()) {
        return Err(error("方差分析 至少需要两个非空分组"));
    }
    let count = groups.iter().map(Vec::len).sum::<usize>();
    let grand_mean = groups.iter().flatten().sum::<f64>() / count as f64;
    let between = groups
        .iter()
        .map(|group| group.len() as f64 * (mean(group) - grand_mean).powi(2))
        .sum::<f64>();
    let within = groups
        .iter()
        .map(|group| {
            let group_mean = mean(group);
            group
                .iter()
                .map(|value| (value - group_mean).powi(2))
                .sum::<f64>()
        })
        .sum::<f64>();
    let df_between = (groups.len() - 1) as f64;
    let df_within = (count - groups.len()) as f64;
    if df_within <= 0.0 || !within.is_finite() || within <= f64::EPSILON {
        return Err(error("组内方差为零或自由度不足，无法执行 方差分析"));
    }
    let statistic = (between / df_between) / (within / df_within);
    let p = 1.0
        - FisherSnedecor::new(df_between, df_within)
            .map_err(|e| error(e.to_string()))?
            .cdf(statistic);
    // η² = 组间平方和 / 总平方和，衡量分组能解释的方差比例。
    let eta_squared = between / (between + within);
    let mut outcome = TestOutcome::new("单因素方差分析", statistic, p, count)
        .degrees(df_between)
        .effect("η²", eta_squared, EffectScale::Association)
        .assumptions("各组独立、近似正态、方差齐性；不满足时改用非参数检验")
        .estimate("分组数", Value::Float(groups.len() as f64));
    if groups.iter().any(|group| group.len() < 3) {
        outcome = outcome.warn("存在样本量小于 3 的分组，F 检验结果不稳定");
    }
    outcome.into_value()
}

pub(super) fn chi_square_test(observed: &Value, expected: Option<&Value>) -> Result<Value, String> {
    let observed = clean(&to_array(observed)?);
    if observed.len() < 2 || observed.iter().any(|value| *value < 0.0) {
        return Err(error("卡方检验需要至少两个非负观测值"));
    }
    let expected = match expected {
        Some(expected) => clean(&to_array(expected)?),
        None => vec![observed.iter().sum::<f64>() / observed.len() as f64; observed.len()],
    };
    if observed.len() != expected.len() || expected.iter().any(|value| *value <= 0.0) {
        return Err(error("期望值必须与观测值等长且大于零"));
    }
    let degrees = (observed.len() - 1) as f64;
    let statistic = observed
        .iter()
        .zip(&expected)
        .map(|(observed, expected)| (observed - expected).powi(2) / expected)
        .sum::<f64>();
    let p = 1.0
        - ChiSquared::new(degrees)
            .map_err(|e| error(e.to_string()))?
            .cdf(statistic);
    let total = observed.iter().sum::<f64>();
    // 拟合度检验的 Cramér's V 用 sqrt(χ² / (N · df))。
    let cramers_v = if total > 0.0 && degrees > 0.0 {
        (statistic / (total * degrees)).sqrt()
    } else {
        0.0
    };
    let mut outcome = TestOutcome::new("卡方拟合度检验", statistic, p, observed.len())
        .degrees(degrees)
        .effect("Cramér's V", cramers_v, EffectScale::Association)
        .assumptions("观测独立、期望频数充足；期望频数过小时结果不可靠")
        .estimate("观测总数", Value::Float(total));
    if expected.iter().any(|value| *value < 5.0) {
        outcome =
            outcome.warn("存在期望频数小于 5 的类别，卡方近似不可靠，建议合并类别或改用精确检验");
    }
    outcome.into_value()
}

pub(super) fn mann_whitney_u(left: &Value, right: &Value) -> Result<Value, String> {
    let left = clean(&to_array(left)?);
    let right = clean(&to_array(right)?);
    if left.is_empty() || right.is_empty() {
        return Err(error("曼-惠特尼 U 检验需要两个非空样本"));
    }
    let mut combined = left
        .iter()
        .map(|value| (*value, 0_usize))
        .chain(right.iter().map(|value| (*value, 1_usize)))
        .collect::<Vec<_>>();
    combined.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut rank_sum = 0.0;
    let mut index = 0;
    while index < combined.len() {
        let mut end = index + 1;
        while end < combined.len() && combined[end].0 == combined[index].0 {
            end += 1;
        }
        let rank = (index + 1 + end) as f64 / 2.0;
        rank_sum += combined[index..end]
            .iter()
            .filter(|(_, group)| *group == 0)
            .count() as f64
            * rank;
        index = end;
    }
    let n1 = left.len() as f64;
    let n2 = right.len() as f64;
    let u1 = rank_sum - n1 * (n1 + 1.0) / 2.0;
    let mean_u = n1 * n2 / 2.0;
    let standard = (n1 * n2 * (n1 + n2 + 1.0) / 12.0).sqrt();
    let z = (u1 - mean_u) / standard;
    let normal = Normal::new(0.0, 1.0).map_err(|error_value| error(error_value.to_string()))?;
    let p = 2.0 * (1.0 - normal.cdf(z.abs()));
    // 秩双列相关：r = 2U/(n1·n2) − 1，落在 [-1, 1]，可直接当效应量读。
    let rank_biserial = 2.0 * u1 / (n1 * n2) - 1.0;
    let mut outcome = TestOutcome::new("曼-惠特尼U检验", u1, p, left.len() + right.len())
        .effect("秩双列相关", rank_biserial, EffectScale::Association)
        .assumptions("两样本独立、分布形状相近；P 值使用正态近似，未做连续性校正")
        .estimate("Z值", Value::Float(z));
    if left.len() < 8 || right.len() < 8 {
        outcome = outcome.warn("存在样本量小于 8 的分组，正态近似的 P 值偏乐观");
    }
    outcome.into_value()
}

pub(super) fn confidence_interval(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = clean(&to_array(value)?);
    if values.len() < 2 {
        return Err(error("置信区间至少需要两个样本"));
    }
    let confidence = named
        .get("置信度")
        .map(Value::as_f64)
        .transpose()?
        .unwrap_or(0.95);
    if !confidence.is_finite() || !(0.0..1.0).contains(&confidence) {
        return Err(error("置信度必须在 0 和 1 之间"));
    }
    let center = mean(&values);
    let standard_error = (variance(&values, 1) / values.len() as f64).sqrt();
    let critical = StudentsT::new(0.0, 1.0, (values.len() - 1) as f64)
        .map_err(|error_value| error(error_value.to_string()))?
        .inverse_cdf(0.5 + confidence / 2.0);
    Ok(Value::record(BTreeMap::from([
        ("均值".into(), Value::Float(center)),
        (
            "下限".into(),
            Value::Float(center - critical * standard_error),
        ),
        (
            "上限".into(),
            Value::Float(center + critical * standard_error),
        ),
        ("置信度".into(), Value::Float(confidence)),
    ])))
}

pub(super) fn logistic_regression(y: &Value, x: &Value) -> Result<Value, String> {
    let y = to_array(y)?;
    let x = to_array(x)?;
    if y.data.len() != x.rows || y.data.iter().any(|value| !matches!(*value, 0.0 | 1.0)) {
        return Err(error("逻辑回归因变量必须是与自变量等行的 0/1 数据"));
    }
    let n = y.data.len();
    let p = x.cols + 1;
    if n <= p || !y.data.contains(&0.0) || !y.data.contains(&1.0) {
        return Err(error(
            "逻辑回归需要多于参数数量的样本，且因变量必须同时包含 0 和 1",
        ));
    }
    let mut design = Vec::with_capacity(n * p);
    for row in 0..n {
        design.push(1.0);
        for column in 0..x.cols {
            design.push(x.data[row * x.cols + column]);
        }
    }
    let matrix = DMatrix::from_row_slice(n, p, &design);
    let target = DVector::from_vec(y.data);
    let mut beta = DVector::zeros(p);
    for _ in 0..100 {
        let linear = &matrix * &beta;
        let probability = linear.map(|value| 1.0 / (1.0 + (-value).exp()));
        let weights = probability.map(|value| (value * (1.0 - value)).max(1e-9));
        let weighted = DMatrix::from_diagonal(&weights);
        let hessian = matrix.transpose() * &weighted * &matrix;
        let gradient = matrix.transpose() * (&target - &probability);
        let Some(step) = hessian.lu().solve(&gradient) else {
            return Err(error("逻辑回归矩阵不可逆"));
        };
        beta += &step;
        if step.norm() < 1e-8 {
            break;
        }
    }
    let fitted = (&matrix * &beta)
        .map(|value| 1.0 / (1.0 + (-value).exp()))
        .iter()
        .map(|value| value.clamp(1e-12, 1.0 - 1e-12))
        .collect::<Vec<_>>();
    let mean_y = target.iter().sum::<f64>() / n as f64;
    let model_likelihood = target
        .iter()
        .zip(&fitted)
        .map(|(actual, probability)| {
            actual * probability.ln() + (1.0 - actual) * (1.0 - probability).ln()
        })
        .sum::<f64>();
    let null_likelihood = target
        .iter()
        .map(|actual| actual * mean_y.ln() + (1.0 - actual) * (1.0 - mean_y).ln())
        .sum::<f64>();
    let r_squared = 1.0 - model_likelihood / null_likelihood;
    let weights = DVector::from_iterator(
        n,
        fitted
            .iter()
            .map(|probability| probability * (1.0 - probability)),
    );
    let information = matrix.transpose() * DMatrix::from_diagonal(&weights) * &matrix;
    let covariance = information
        .try_inverse()
        .ok_or_else(|| error("逻辑回归信息矩阵不可逆，无法计算显著性"))?;
    let normal = Normal::new(0.0, 1.0).map_err(|error_value| error(error_value.to_string()))?;
    let p_values = (0..p)
        .map(|index| {
            let standard_error = covariance[(index, index)].sqrt();
            let p_value = 2.0 * (1.0 - normal.cdf((beta[index] / standard_error).abs()));
            if !standard_error.is_finite() || standard_error <= 0.0 || !p_value.is_finite() {
                return Err(error("逻辑回归产生了非有限显著性结果"));
            }
            Ok(p_value)
        })
        .collect::<Result<Vec<_>, String>>()?;
    if !r_squared.is_finite()
        || beta.iter().any(|value| !value.is_finite())
        || fitted.iter().any(|value| !value.is_finite())
    {
        return Err(error("逻辑回归产生了非有限结果"));
    }
    let standard_errors = (0..p)
        .map(|index| covariance[(index, index)].sqrt())
        .collect::<Vec<_>>();
    let residuals = target
        .iter()
        .zip(&fitted)
        .map(|(actual, probability)| actual - probability)
        .collect::<Vec<_>>();
    let log_loss = -model_likelihood / n as f64;
    let mae = residuals.iter().map(|value| value.abs()).sum::<f64>() / n as f64;
    let brier_rmse = (residuals.iter().map(|value| value * value).sum::<f64>() / n as f64).sqrt();
    let classification = classification_metrics(target.as_slice(), &fitted, log_loss)?;
    Ok(Value::Regression(Rc::new(RegressionResult {
        coefficients: beta.iter().copied().collect(),
        r_squared,
        adjusted_r_squared: r_squared,
        p_values,
        standard_errors,
        fitted,
        residuals,
        rmse: brier_rmse,
        mae,
        diagnostics: None,
        classification: Some(classification),
    })))
}

/// 混淆矩阵按 0.5 阈值统计；AUC 用秩和公式，平票按 0.5 计入。
fn classification_metrics(
    target: &[f64],
    fitted: &[f64],
    log_loss: f64,
) -> Result<ClassificationMetrics, String> {
    let mut true_positive = 0usize;
    let mut false_positive = 0usize;
    let mut true_negative = 0usize;
    let mut false_negative = 0usize;
    for (actual, probability) in target.iter().zip(fitted) {
        match (*actual >= 0.5, *probability >= 0.5) {
            (true, true) => true_positive += 1,
            (false, true) => false_positive += 1,
            (false, false) => true_negative += 1,
            (true, false) => false_negative += 1,
        }
    }
    let total = target.len();
    let accuracy = (true_positive + true_negative) as f64 / total as f64;
    let positives = true_positive + false_negative;
    let negatives = true_negative + false_positive;
    let roc_auc = if positives == 0 || negatives == 0 {
        // 单一类别下 AUC 无定义；报告 0.5 表示没有区分能力，而不是假装完美。
        0.5
    } else {
        let concordant = target
            .iter()
            .zip(fitted)
            .filter(|(actual, _)| **actual >= 0.5)
            .map(|(_, positive)| {
                target
                    .iter()
                    .zip(fitted)
                    .filter(|(actual, _)| **actual < 0.5)
                    .map(|(_, negative)| match positive.total_cmp(negative) {
                        std::cmp::Ordering::Greater => 1.0,
                        std::cmp::Ordering::Equal => 0.5,
                        std::cmp::Ordering::Less => 0.0,
                    })
                    .sum::<f64>()
            })
            .sum::<f64>();
        concordant / (positives * negatives) as f64
    };
    if !accuracy.is_finite() || !roc_auc.is_finite() || !log_loss.is_finite() {
        return Err(error("逻辑回归分类指标产生了非有限结果"));
    }
    Ok(ClassificationMetrics {
        log_loss,
        accuracy,
        true_positive,
        false_positive,
        true_negative,
        false_negative,
        roc_auc,
    })
}

pub(super) fn decompose_series(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = clean(&to_array(value)?);
    let period = arg_usize(named, "周期", 4)?.max(2);
    let required = period
        .checked_mul(2)
        .ok_or_else(|| error("时间序列周期超出支持范围"))?;
    if values.len() < required {
        return Err(error("时间序列长度至少需要两个周期"));
    }
    let trend = (0..values.len())
        .map(|index| {
            let start = index.saturating_add(1).saturating_sub(period);
            mean(&values[start..=index])
        })
        .collect::<Vec<_>>();
    let residual = values
        .iter()
        .zip(&trend)
        .map(|(value, trend)| value - trend)
        .collect::<Vec<_>>();
    let mut seasonal_pattern = vec![0.0; period];
    let mut counts = vec![0; period];
    for (index, value) in residual.iter().enumerate() {
        seasonal_pattern[index % period] += value;
        counts[index % period] += 1;
    }
    for (value, count) in seasonal_pattern.iter_mut().zip(counts) {
        *value /= count.max(1) as f64;
    }
    let seasonal = (0..values.len())
        .map(|index| seasonal_pattern[index % period])
        .collect::<Vec<_>>();
    let noise = values
        .iter()
        .zip(&trend)
        .zip(&seasonal)
        .map(|((value, trend), seasonal)| value - trend - seasonal)
        .collect::<Vec<_>>();
    Ok(Value::record(BTreeMap::from([
        ("趋势".into(), array_value(vector(trend))),
        ("季节".into(), array_value(vector(seasonal))),
        ("残差".into(), array_value(vector(noise))),
    ])))
}

pub(super) fn forecast_series(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = clean(&to_array(value)?);
    if values.len() < 2 {
        return Err(error("线性预测至少需要两个观测值"));
    }
    let steps = arg_usize(named, "步数", 1)?;
    if steps > crate::resource::DEFAULT_MAX_COLLECTION_ITEMS {
        return Err(error("预测步数超过集合资源限制"));
    }
    let x_mean = (values.len() - 1) as f64 / 2.0;
    let y_mean = mean(&values);
    let slope = values
        .iter()
        .enumerate()
        .map(|(index, value)| (index as f64 - x_mean) * (value - y_mean))
        .sum::<f64>()
        / (0..values.len())
            .map(|index| (index as f64 - x_mean).powi(2))
            .sum::<f64>();
    let intercept = y_mean - slope * x_mean;
    let end = values
        .len()
        .checked_add(steps)
        .ok_or_else(|| error("预测步数超出支持范围"))?;
    let mut forecast = Vec::new();
    forecast
        .try_reserve_exact(steps)
        .map_err(|_| error("预测步数超出可分配范围"))?;
    forecast.extend((values.len()..end).map(|index| intercept + slope * index as f64));
    Ok(array_value(vector(forecast)))
}

pub(super) fn linear_program(
    objective: &Value,
    constraints: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    use minilp::{ComparisonOp, OptimizationDirection, Problem};
    let objective = clean(&to_array(objective)?);
    let direction = if arg_text(named, "方向", "最大化") == "最小化" {
        OptimizationDirection::Minimize
    } else {
        OptimizationDirection::Maximize
    };
    let mut problem = Problem::new(direction);
    let variables = objective
        .iter()
        .map(|coefficient| problem.add_var(*coefficient, (0.0, f64::INFINITY)))
        .collect::<Vec<_>>();
    for constraint in constraints.iter_values_limited(DEFAULT_MAX_COLLECTION_ITEMS)? {
        let parts = constraint.iter_values_limited(DEFAULT_MAX_COLLECTION_ITEMS)?;
        if parts.len() < 2 {
            return Err(error("线性规划约束应包含系数列表和上限"));
        }
        let coefficients = clean(&to_array(&parts[0])?);
        if coefficients.len() != variables.len() {
            return Err(error("线性规划约束维度不匹配"));
        }
        problem.add_constraint(
            variables
                .iter()
                .zip(coefficients)
                .map(|(variable, coefficient)| (*variable, coefficient)),
            ComparisonOp::Le,
            parts[1].as_f64()?,
        );
    }
    let solution = problem.solve().map_err(|e| error(e.to_string()))?;
    let values = variables
        .iter()
        .map(|variable| solution[*variable])
        .collect::<Vec<_>>();
    let objective_value = objective
        .iter()
        .zip(&values)
        .map(|(coefficient, value)| coefficient * value)
        .sum();
    Ok(Value::record(BTreeMap::from([
        ("变量".into(), array_value(vector(values))),
        ("目标值".into(), Value::Float(objective_value)),
    ])))
}

pub(super) fn one_sample_t(value: &Value, pop: f64) -> Result<Value, String> {
    let a = clean(&to_array(value)?);
    if a.len() < 2 {
        return Err(error("t 检验至少需要两个样本"));
    }
    let standard_error = (variance(&a, 1) / a.len() as f64).sqrt();
    if !standard_error.is_finite() || standard_error <= f64::EPSILON {
        return Err(error("样本方差为零，无法执行单样本 t 检验"));
    }
    let t = (mean(&a) - pop) / standard_error;
    let degrees = (a.len() - 1) as f64;
    let p = 2.0
        * (1.0
            - StudentsT::new(0.0, 1.0, degrees)
                .map_err(|error_value| error(error_value.to_string()))?
                .cdf(t.abs()));
    let difference = mean(&a) - pop;
    let deviation = variance(&a, 1).sqrt();
    let margin = t_critical(degrees, DEFAULT_CONFIDENCE)? * standard_error;
    let mut outcome = TestOutcome::new("单样本t检验", t, p, a.len())
        .degrees(degrees)
        .effect(
            "Cohen's d",
            difference / deviation,
            EffectScale::StandardizedMean,
        )
        .estimate("均值差", Value::Float(difference))
        .interval(difference - margin, difference + margin)
        .assumptions("样本独立、近似正态；小样本时对偏态敏感");
    if a.len() < 30 {
        outcome = outcome.warn("样本量小于 30，正态性假设对结论影响较大");
    }
    outcome.into_value()
}
pub(super) fn two_sample_t(a: &Value, b: &Value) -> Result<Value, String> {
    let a = clean(&to_array(a)?);
    let b = clean(&to_array(b)?);
    if a.len() < 2 || b.len() < 2 {
        return Err(error("独立 t 检验每组至少需要两个样本"));
    }
    let va = variance(&a, 1);
    let vb = variance(&b, 1);
    let se = (va / a.len() as f64 + vb / b.len() as f64).sqrt();
    if !se.is_finite() || se <= f64::EPSILON {
        return Err(error("两组样本方差为零，无法执行独立 t 检验"));
    }
    let t = (mean(&a) - mean(&b)) / se;
    let df = (va / a.len() as f64 + vb / b.len() as f64).powi(2)
        / ((va / a.len() as f64).powi(2) / (a.len() - 1) as f64
            + (vb / b.len() as f64).powi(2) / (b.len() - 1) as f64);
    if !t.is_finite() || !df.is_finite() || df <= 0.0 {
        return Err(error("样本无法产生有效的独立 t 检验自由度"));
    }
    let distribution =
        StudentsT::new(0.0, 1.0, df).map_err(|_| error("样本无法产生有效的独立 t 检验分布"))?;
    let p = 2.0 * (1.0 - distribution.cdf(t.abs()));
    let difference = mean(&a) - mean(&b);
    // 合并标准差用于 Cohen's d；Welch 自由度只用于 P 值和区间。
    let pooled = (((a.len() - 1) as f64 * va + (b.len() - 1) as f64 * vb)
        / (a.len() + b.len() - 2) as f64)
        .sqrt();
    let margin = t_critical(df, DEFAULT_CONFIDENCE)? * se;
    let mut outcome = TestOutcome::new("独立样本t检验", t, p, a.len() + b.len())
        .degrees(df)
        .effect(
            "Cohen's d",
            difference / pooled,
            EffectScale::StandardizedMean,
        )
        .estimate("均值差", Value::Float(difference))
        .interval(difference - margin, difference + margin)
        .assumptions("两组独立、近似正态；使用 Welch 校正，不要求方差齐性");
    if a.len() < 30 || b.len() < 30 {
        outcome = outcome.warn("存在样本量小于 30 的分组，正态性假设对结论影响较大");
    }
    outcome.into_value()
}
/// 效应量的量级判读阈值不同族系不一样，混用会得出错误的“大/小效应”结论。
#[derive(Clone, Copy)]
enum EffectScale {
    /// Cohen's d 一类的标准化均值差。
    StandardizedMean,
    /// 相关系数、Cramér's V、η² 一类落在 [0,1] 的关联强度。
    Association,
}

impl EffectScale {
    fn label(self, value: f64) -> &'static str {
        let value = value.abs();
        let cuts = match self {
            Self::StandardizedMean => [0.2, 0.5, 0.8],
            Self::Association => [0.1, 0.3, 0.5],
        };
        if value < cuts[0] {
            "极小"
        } else if value < cuts[1] {
            "小"
        } else if value < cuts[2] {
            "中"
        } else {
            "大"
        }
    }
}

/// 统计检验的统一结果形状。
/// 各检验只填自己能提供的字段，渲染集中在 `into_value`，避免继续增加零散属性名。
/// 所有字段都是标量，因此结果表可以无损写进 CSV。
struct TestOutcome {
    method: &'static str,
    statistic: f64,
    p_value: f64,
    degrees_of_freedom: Option<f64>,
    sample_size: usize,
    /// 只有配对类检验才有意义：因缺失而被丢弃的数据对数量。
    missing_pairs: Option<usize>,
    effect: Option<(&'static str, f64, EffectScale)>,
    /// 检验特有的业务估计量，按其领域名称输出，例如均值差、相关系数、优势比。
    estimate: Option<(&'static str, Value)>,
    interval: Option<(f64, f64)>,
    confidence: f64,
    assumptions: &'static str,
    warnings: Vec<String>,
}

const DEFAULT_CONFIDENCE: f64 = 0.95;
const SIGNIFICANCE_LEVEL: f64 = 0.05;

impl TestOutcome {
    fn new(method: &'static str, statistic: f64, p_value: f64, sample_size: usize) -> Self {
        Self {
            method,
            statistic,
            p_value,
            degrees_of_freedom: None,
            sample_size,
            missing_pairs: None,
            effect: None,
            estimate: None,
            interval: None,
            confidence: DEFAULT_CONFIDENCE,
            assumptions: "",
            warnings: Vec::new(),
        }
    }

    fn degrees(mut self, degrees: f64) -> Self {
        self.degrees_of_freedom = Some(degrees);
        self
    }

    fn missing(mut self, missing: usize) -> Self {
        self.missing_pairs = Some(missing);
        self
    }

    fn effect(mut self, name: &'static str, value: f64, scale: EffectScale) -> Self {
        self.effect = Some((name, value, scale));
        self
    }

    fn estimate(mut self, name: &'static str, value: Value) -> Self {
        self.estimate = Some((name, value));
        self
    }

    fn interval(mut self, low: f64, high: f64) -> Self {
        self.interval = Some((low.min(high), low.max(high)));
        self
    }

    fn assumptions(mut self, assumptions: &'static str) -> Self {
        self.assumptions = assumptions;
        self
    }

    fn warn(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    fn significant(&self) -> bool {
        self.p_value < SIGNIFICANCE_LEVEL
    }

    /// 面向业务读者的解读；不显著时明确说明这不等于“两者相同”。
    fn reading(&self) -> String {
        let level = (SIGNIFICANCE_LEVEL * 100.0) as u32;
        let mut reading = if self.significant() {
            format!(
                "在 {level}% 显著性水平下结果显著（P={:.4}）。",
                self.p_value
            )
        } else {
            format!(
                "在 {level}% 显著性水平下未发现显著结果（P={:.4}）；这不等于两者相同，也可能是样本量不足。",
                self.p_value
            )
        };
        if let Some((name, value, scale)) = self.effect {
            reading.push_str(&format!(
                "{name}={:.4}，属于{}效应。",
                value,
                scale.label(value)
            ));
        }
        if let Some((low, high)) = self.interval {
            reading.push_str(&format!(
                "{:.0}% 置信区间为 [{low:.4}, {high:.4}]。",
                self.confidence * 100.0
            ));
        }
        reading
    }

    fn into_value(self) -> Result<Value, String> {
        if !self.statistic.is_finite()
            || !self.p_value.is_finite()
            || !(0.0..=1.0).contains(&self.p_value)
        {
            return Err(error("统计检验产生了非有限或无效结果"));
        }
        if let Some(degrees) = self.degrees_of_freedom {
            if !degrees.is_finite() || degrees <= 0.0 {
                return Err(error("统计检验自由度必须是正的有限值"));
            }
        }
        if let Some((_, value, _)) = self.effect {
            if !value.is_finite() {
                return Err(error("统计检验效应量不是有限值"));
            }
        }
        if let Some((low, high)) = self.interval {
            if !low.is_finite() || !high.is_finite() {
                return Err(error("统计检验置信区间不是有限值"));
            }
        }
        let optional_float = |value: Option<f64>| value.map(Value::Float).unwrap_or(Value::Null);
        let reading = self.reading();
        let mut record = BTreeMap::from([
            ("类型".to_string(), Value::Text("统计检验".into())),
            ("方法".to_string(), Value::Text(self.method.into())),
            ("统计量".to_string(), Value::Float(self.statistic)),
            ("P值".to_string(), Value::Float(self.p_value)),
            (
                "自由度".to_string(),
                optional_float(self.degrees_of_freedom),
            ),
            ("样本量".to_string(), Value::Float(self.sample_size as f64)),
            (
                "缺失配对数".to_string(),
                optional_float(self.missing_pairs.map(|count| count as f64)),
            ),
            (
                "效应量名称".to_string(),
                self.effect
                    .map(|(name, _, _)| Value::Text(name.into()))
                    .unwrap_or(Value::Null),
            ),
            (
                "效应量".to_string(),
                optional_float(self.effect.map(|(_, value, _)| value)),
            ),
            (
                "效应量判读".to_string(),
                self.effect
                    .map(|(_, value, scale)| Value::Text(scale.label(value).into()))
                    .unwrap_or(Value::Null),
            ),
            (
                "置信下限".to_string(),
                optional_float(self.interval.map(|(low, _)| low)),
            ),
            (
                "置信上限".to_string(),
                optional_float(self.interval.map(|(_, high)| high)),
            ),
            ("置信度".to_string(), Value::Float(self.confidence)),
            (
                "显著性".to_string(),
                Value::Text(
                    if self.significant() {
                        "显著"
                    } else {
                        "不显著"
                    }
                    .into(),
                ),
            ),
            (
                "方法前提".to_string(),
                if self.assumptions.is_empty() {
                    Value::Null
                } else {
                    Value::Text(self.assumptions.into())
                },
            ),
            (
                "警告".to_string(),
                if self.warnings.is_empty() {
                    Value::Null
                } else {
                    Value::Text(self.warnings.join("；"))
                },
            ),
            ("数字含义".to_string(), Value::Text(reading)),
        ]);
        if let Some((name, value)) = self.estimate {
            record.insert(name.to_string(), value);
        }
        Ok(Value::record(record))
    }
}

/// t 分布双侧临界值，用于均值类检验的置信区间。
fn t_critical(degrees: f64, confidence: f64) -> Result<f64, String> {
    StudentsT::new(0.0, 1.0, degrees)
        .map_err(|reason| error(reason.to_string()))
        .map(|distribution| distribution.inverse_cdf(1.0 - (1.0 - confidence) / 2.0))
}

pub(super) fn paired_t(left: &Value, right: &Value) -> Result<Value, String> {
    let left = to_array(left)?;
    let right = to_array(right)?;
    if left.data.len() != right.data.len() {
        return Err(error("配对样本t检验需要等长且至少两个有效数据对"));
    }
    let differences = left
        .data
        .iter()
        .zip(&right.data)
        .filter(|(a, b)| (*a).is_finite() && (*b).is_finite())
        .map(|(a, b)| a - b)
        .collect::<Vec<_>>();
    if differences.len() < 2 {
        return Err(error("配对样本t检验需要至少两个有效数据对"));
    }
    let center = mean(&differences);
    let sample_variance = variance(&differences, 1);
    if !sample_variance.is_finite() || sample_variance <= f64::EPSILON {
        return Err(error("配对差值方差为零，无法执行配对样本t检验"));
    }
    let statistic = center / (sample_variance / differences.len() as f64).sqrt();
    let degrees = (differences.len() - 1) as f64;
    let distribution = StudentsT::new(0.0, 1.0, degrees).map_err(|e| error(e.to_string()))?;
    let p = 2.0 * (1.0 - distribution.cdf(statistic.abs()));
    let deviation = sample_variance.sqrt();
    let margin =
        t_critical(degrees, DEFAULT_CONFIDENCE)? * deviation / (differences.len() as f64).sqrt();
    let mut outcome = TestOutcome::new("配对样本t检验", statistic, p, differences.len())
        .degrees(degrees)
        // 等长输入里因任一侧缺失而被丢弃的数据对。
        .missing(left.data.len() - differences.len())
        .effect(
            "Cohen's d",
            center / deviation,
            EffectScale::StandardizedMean,
        )
        .estimate("均值差", Value::Float(center))
        .interval(center - margin, center + margin)
        .assumptions("同一对象的前后测量、差值近似正态；缺失对已成对剔除");
    if left.data.len() != differences.len() {
        outcome = outcome.warn(format!(
            "有 {} 对数据因缺失被剔除",
            left.data.len() - differences.len()
        ));
    }
    outcome.into_value()
}

pub(super) fn spearman(left: &Value, right: &Value) -> Result<Value, String> {
    let left = clean(&to_array(left)?);
    let right = clean(&to_array(right)?);
    if left.len() != right.len() || left.len() < 3 {
        return Err(error("Spearman相关至少需要三个等长有效数据对"));
    }
    let rank = |values: &[f64]| {
        let mut indexed = values.iter().copied().enumerate().collect::<Vec<_>>();
        indexed.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut output = vec![0.0; values.len()];
        let mut start = 0;
        while start < indexed.len() {
            let mut end = start + 1;
            while end < indexed.len() && indexed[end].1 == indexed[start].1 {
                end += 1;
            }
            let average = (start + end + 1) as f64 / 2.0;
            for (index, _) in &indexed[start..end] {
                output[*index] = average;
            }
            start = end;
        }
        output
    };
    let x = NumericArray {
        rows: left.len(),
        cols: 1,
        data: rank(&left),
    };
    let y = NumericArray {
        rows: right.len(),
        cols: 1,
        data: rank(&right),
    };
    let coefficient = crate::analysis::statistics::correlation(&x, &y).map_err(error)?;
    let degrees = (left.len() - 2) as f64;
    let statistic = coefficient * (degrees / (1.0 - coefficient * coefficient).max(1e-12)).sqrt();
    let distribution = StudentsT::new(0.0, 1.0, degrees).map_err(|e| error(e.to_string()))?;
    let p = 2.0 * (1.0 - distribution.cdf(statistic.abs()));
    let mut outcome = TestOutcome::new("Spearman相关", statistic, p, left.len())
        .degrees(degrees)
        .effect("Spearman ρ", coefficient, EffectScale::Association)
        .estimate("相关系数", Value::Float(coefficient))
        .assumptions("单调关系、按秩计算，对异常值稳健；相关不等于因果");
    // Fisher z 变换求区间；|ρ|=1 时变换发散，此时不给区间而是给警告。
    if coefficient.abs() < 1.0 && left.len() > 3 {
        let z = 0.5 * ((1.0 + coefficient) / (1.0 - coefficient)).ln();
        let spread = 1.96 / ((left.len() - 3) as f64).sqrt();
        outcome = outcome.interval((z - spread).tanh(), (z + spread).tanh());
    } else {
        outcome = outcome.warn("相关系数达到边界或样本过少，无法给出置信区间");
    }
    outcome.into_value()
}

pub(super) fn fisher_exact(value: &Value) -> Result<Value, String> {
    let array = to_array(value)?;
    if array.rows != 2
        || array.cols != 2
        || array
            .data
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0 || v.fract() != 0.0)
    {
        return Err(error("Fisher精确检验需要2x2非负整数列联表"));
    }
    let values = array.data;
    let (a, b, c, d) = (
        values[0] as u64,
        values[1] as u64,
        values[2] as u64,
        values[3] as u64,
    );
    let log_factorial = |n: u64| (1..=n).map(|i| (i as f64).ln()).sum::<f64>();
    let log_probability = |aa: u64, bb: u64, cc: u64, dd: u64| {
        log_factorial(aa + bb)
            + log_factorial(cc + dd)
            + log_factorial(aa + cc)
            + log_factorial(bb + dd)
            - log_factorial(aa)
            - log_factorial(bb)
            - log_factorial(cc)
            - log_factorial(dd)
            - log_factorial(aa + bb + cc + dd)
    };
    let observed = log_probability(a, b, c, d).exp();
    let row_one = a + b;
    let row_two = c + d;
    let column_one = a + c;
    let minimum = column_one.saturating_sub(row_two);
    let maximum = column_one.min(row_one);
    let mut p = 0.0;
    for aa in minimum..=maximum {
        let bb = row_one - aa;
        let cc = column_one - aa;
        let dd = row_two - cc;
        let probability = log_probability(aa, bb, cc, dd).exp();
        if probability <= observed + 1e-12 {
            p += probability;
        }
    }
    let odds = if b == 0 || c == 0 {
        Value::Null
    } else {
        Value::Float(a as f64 * d as f64 / (b as f64 * c as f64))
    };
    let total = (a + b + c + d) as f64;
    // 2x2 表的 phi 系数等价于 Cramér's V，衡量两个二分变量的关联强度。
    let denominator = ((a + b) as f64 * (c + d) as f64 * (a + c) as f64 * (b + d) as f64).sqrt();
    let phi = if denominator > 0.0 {
        (a as f64 * d as f64 - b as f64 * c as f64) / denominator
    } else {
        0.0
    };
    let mut outcome = TestOutcome::new("Fisher精确检验", observed, p.min(1.0), total as usize)
        .effect("phi 系数", phi, EffectScale::Association)
        .estimate("优势比", odds.clone())
        .assumptions("2x2 列联表、观测独立、边际固定；精确计算不依赖大样本近似");
    if matches!(odds, Value::Null) {
        outcome = outcome.warn("存在零单元格，优势比无定义");
    }
    outcome.into_value()
}

pub(super) fn adjust_p_values(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = clean(&to_array(value)?);
    if values.is_empty() || values.iter().any(|p| !(0.0..=1.0).contains(p)) {
        return Err(error("多重检验校正需要非空且在0到1之间的P值"));
    }
    let method = named
        .get("方法")
        .map(ToString::to_string)
        .unwrap_or_else(|| "Holm".into());
    let mut indexed = values.iter().copied().enumerate().collect::<Vec<_>>();
    indexed.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut adjusted = vec![0.0; values.len()];
    match method.as_str() {
        "Bonferroni" => {
            for (index, p) in values.iter().enumerate() {
                adjusted[index] = (*p * values.len() as f64).min(1.0);
            }
        }
        "Holm" => {
            let mut previous = 0.0;
            for (rank, (index, p)) in indexed.iter().enumerate() {
                let value = (*p * (values.len() - rank) as f64).max(previous).min(1.0);
                adjusted[*index] = value;
                previous = value;
            }
        }
        "Benjamini-Hochberg" => {
            let mut previous = 1.0;
            for (rank, (index, p)) in indexed.iter().enumerate().rev() {
                let value = (*p * values.len() as f64 / (rank + 1) as f64)
                    .min(previous)
                    .min(1.0);
                adjusted[*index] = value;
                previous = value;
            }
        }
        _ => {
            return Err(error(
                "多重检验校正方法只支持Bonferroni、Holm或Benjamini-Hochberg",
            ))
        }
    }
    Ok(Value::record(BTreeMap::from([
        ("方法".into(), Value::Text(method)),
        (
            "原始P值".into(),
            Value::list(values.into_iter().map(Value::Float).collect()),
        ),
        (
            "校正P值".into(),
            Value::list(adjusted.into_iter().map(Value::Float).collect()),
        ),
    ])))
}
pub(super) fn shapiro(value: &Value) -> Result<Value, String> {
    let mut x = clean(&to_array(value)?);
    let n = x.len();
    if !(3..=5000).contains(&n) {
        return Err(error("夏皮罗-威尔克检验要求 3 到 5000 个样本"));
    }
    x.sort_by(f64::total_cmp);
    let normal = Normal::new(0.0, 1.0).map_err(|error_value| error(error_value.to_string()))?;
    let mut m = (1..=n)
        .map(|i| normal.inverse_cdf((i as f64 - 0.375) / (n as f64 + 0.25)))
        .collect::<Vec<_>>();
    let norm = m.iter().map(|v| v * v).sum::<f64>().sqrt();
    for v in &mut m {
        *v /= norm
    }
    let numerator = m.iter().zip(&x).map(|(a, b)| a * b).sum::<f64>().powi(2);
    let mean = mean(&x);
    let denominator = x.iter().map(|v| (v - mean).powi(2)).sum::<f64>();
    if !denominator.is_finite() || denominator <= f64::EPSILON {
        return Err(error("样本方差为零，无法执行 夏皮罗-威尔克检验"));
    }
    let w = (numerator / denominator).clamp(0.0, 1.0);
    let ln_n = (n as f64).ln();
    let mu = -1.5861 - 0.31082 * ln_n - 0.083751 * ln_n.powi(2) + 0.0038915 * ln_n.powi(3);
    let sigma = (-0.4803 - 0.082676 * ln_n + 0.0030302 * ln_n.powi(2)).exp();
    let z = ((1.0 - w).ln() - mu) / sigma;
    let p = 1.0 - normal.cdf(z);
    TestOutcome::new("夏皮罗-威尔克正态性检验", w, p.clamp(0.0, 1.0), n)
        .estimate("W统计量", Value::Float(w))
        .assumptions("样本独立、连续取值；P 值使用对数正态近似，大样本下对轻微偏离过度敏感")
        .warn("不显著只说明没有足够证据拒绝正态，不能反过来证明数据是正态的")
        .into_value()
}

pub(super) fn linear_regression(y: &Value, x: &Value) -> Result<Value, String> {
    let y = to_array(y)?;
    let x = to_array(x)?;
    if y.data.len() != x.rows {
        return Err(error("回归因变量与自变量行数必须一致"));
    }
    let n = y.data.len();
    let p = x.cols + 1;
    if n <= p {
        return Err(error("回归样本数必须大于参数数量"));
    }
    let mut design = Vec::with_capacity(n * p);
    for row in 0..n {
        design.push(1.0);
        for col in 0..x.cols {
            design.push(x.data[row * x.cols + col])
        }
    }
    let xm = DMatrix::from_row_slice(n, p, &design);
    let yv = DVector::from_vec(y.data);
    let xtx = xm.transpose() * &xm;
    let inverse = xtx
        .clone()
        .try_inverse()
        .ok_or_else(|| error("回归设计矩阵不可逆"))?;
    let beta = &inverse * xm.transpose() * &yv;
    let fitted = &xm * &beta;
    let y_mean = yv.iter().sum::<f64>() / n as f64;
    let sse = yv
        .iter()
        .zip(fitted.iter())
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f64>();
    let sst = yv.iter().map(|v| (v - y_mean).powi(2)).sum::<f64>();
    if !sst.is_finite() || sst <= f64::EPSILON {
        return Err(error("回归因变量方差为零，无法计算回归统计量"));
    }
    let r_squared = 1.0 - sse / sst;
    let mse = sse / (n - p) as f64;
    let dist = StudentsT::new(0.0, 1.0, (n - p) as f64)
        .map_err(|error_value| error(error_value.to_string()))?;
    let p_values = (0..p)
        .map(|i| {
            let se = (mse * inverse[(i, i)]).sqrt();
            if !se.is_finite() {
                return Err(error("回归标准误不是有限值，无法计算系数显著性"));
            }
            let p_value = 2.0 * (1.0 - dist.cdf((beta[i] / se).abs()));
            if !p_value.is_finite() {
                return Err(error("回归产生了非有限显著性结果"));
            }
            Ok(p_value)
        })
        .collect::<Result<Vec<_>, String>>()?;
    if !r_squared.is_finite()
        || beta.iter().any(|value| !value.is_finite())
        || fitted.iter().any(|value| !value.is_finite())
    {
        return Err(error("回归产生了非有限结果"));
    }
    let standard_errors = (0..p)
        .map(|i| (mse * inverse[(i, i)]).sqrt())
        .collect::<Vec<_>>();
    let residuals = yv
        .iter()
        .zip(fitted.iter())
        .map(|(actual, fitted)| actual - fitted)
        .collect::<Vec<_>>();
    let rmse = (sse / n as f64).sqrt();
    let mae = residuals.iter().map(|value| value.abs()).sum::<f64>() / n as f64;
    let adjusted_r_squared = 1.0 - (1.0 - r_squared) * (n - 1) as f64 / (n - p) as f64;
    let diagnostics = ols_diagnostics(&xm, &inverse, &residuals, mse, n, p, x.cols)?;
    Ok(Value::Regression(Rc::new(RegressionResult {
        coefficients: beta.iter().copied().collect(),
        r_squared,
        adjusted_r_squared,
        p_values,
        standard_errors,
        fitted: fitted.iter().copied().collect(),
        residuals,
        rmse,
        mae,
        diagnostics: Some(diagnostics),
        classification: None,
    })))
}

/// 影响诊断复用回归已算出的设计矩阵和 `(XᵗX)⁻¹`，不重新解一次最小二乘。
fn ols_diagnostics(
    design: &DMatrix<f64>,
    inverse: &DMatrix<f64>,
    residuals: &[f64],
    mse: f64,
    n: usize,
    p: usize,
    predictors: usize,
) -> Result<RegressionDiagnostics, String> {
    let sigma = mse.sqrt();
    if !sigma.is_finite() || sigma <= 0.0 {
        return Err(error("回归残差方差为零，无法计算影响诊断"));
    }
    let mut leverage = Vec::with_capacity(n);
    let mut standardized_residuals = Vec::with_capacity(n);
    let mut cooks_distance = Vec::with_capacity(n);
    for (row, residual) in residuals.iter().enumerate() {
        let x_row = design.row(row).transpose();
        let hat = (x_row.transpose() * inverse * &x_row)[(0, 0)];
        // 杠杆值理论上落在 (0, 1]；数值误差可能略微越界，夹紧后再参与除法。
        let hat = hat.clamp(0.0, 1.0 - f64::EPSILON);
        let standardized = residual / (sigma * (1.0 - hat).sqrt());
        let cooks = standardized.powi(2) / p as f64 * (hat / (1.0 - hat));
        if !hat.is_finite() || !standardized.is_finite() || !cooks.is_finite() {
            return Err(error("回归影响诊断产生了非有限结果"));
        }
        leverage.push(hat);
        standardized_residuals.push(standardized);
        cooks_distance.push(cooks);
    }
    // VIF_j = (XᵗX)⁻¹_jj * Σ(x_ij - x̄_j)²，由 Var(β_j) 的两种写法相等推出。
    let mut variance_inflation = Vec::with_capacity(predictors);
    for predictor in 0..predictors {
        let column = predictor + 1;
        let mean = (0..n).map(|row| design[(row, column)]).sum::<f64>() / n as f64;
        let deviation = (0..n)
            .map(|row| (design[(row, column)] - mean).powi(2))
            .sum::<f64>();
        let vif = inverse[(column, column)] * deviation;
        if !vif.is_finite() {
            return Err(error("回归共线性诊断产生了非有限结果"));
        }
        variance_inflation.push(vif.max(1.0));
    }
    Ok(RegressionDiagnostics {
        standardized_residuals,
        leverage,
        cooks_distance,
        variance_inflation,
    })
}
pub fn regression_attr(result: &RegressionResult, name: &str) -> Option<Value> {
    match name {
        "R平方" => Some(Value::Float(result.r_squared)),
        "调整R平方" => Some(Value::Float(result.adjusted_r_squared)),
        "RMSE" => Some(Value::Float(result.rmse)),
        "MAE" => Some(Value::Float(result.mae)),
        "对数损失" => result
            .classification
            .as_ref()
            .map(|metrics| Value::Float(metrics.log_loss)),
        "准确率" => result
            .classification
            .as_ref()
            .map(|metrics| Value::Float(metrics.accuracy)),
        "AUC" => result
            .classification
            .as_ref()
            .map(|metrics| Value::Float(metrics.roc_auc)),
        _ => None,
    }
}

/// 诊断结果以规范化表格返回，报告侧用列规范驱动展示，不接收嵌套记录。
fn diagnostics_table(rows: Rows) -> Value {
    Value::Table(Rc::new(std::cell::RefCell::new(Table {
        rows,
        ..Default::default()
    })))
}

fn influence_rows(diagnostics: &RegressionDiagnostics) -> Rows {
    diagnostics
        .standardized_residuals
        .iter()
        .zip(&diagnostics.leverage)
        .zip(&diagnostics.cooks_distance)
        .enumerate()
        .map(|(index, ((standardized, leverage), cooks))| {
            BTreeMap::from([
                ("观测".into(), Value::Float((index + 1) as f64)),
                ("标准化残差".into(), Value::Float(*standardized)),
                ("杠杆值".into(), Value::Float(*leverage)),
                ("Cook距离".into(), Value::Float(*cooks)),
            ])
        })
        .collect()
}

fn collinearity_rows(diagnostics: &RegressionDiagnostics) -> Rows {
    diagnostics
        .variance_inflation
        .iter()
        .enumerate()
        .map(|(index, vif)| {
            BTreeMap::from([
                ("变量".into(), Value::Float((index + 1) as f64)),
                ("VIF".into(), Value::Float(*vif)),
            ])
        })
        .collect()
}

fn classification_rows(metrics: &ClassificationMetrics) -> Rows {
    vec![BTreeMap::from([
        ("对数损失".into(), Value::Float(metrics.log_loss)),
        ("准确率".into(), Value::Float(metrics.accuracy)),
        ("AUC".into(), Value::Float(metrics.roc_auc)),
        ("真正例".into(), Value::Float(metrics.true_positive as f64)),
        ("假正例".into(), Value::Float(metrics.false_positive as f64)),
        ("真负例".into(), Value::Float(metrics.true_negative as f64)),
        ("假负例".into(), Value::Float(metrics.false_negative as f64)),
    ])]
}

pub fn regression_method(
    result: &RegressionResult,
    name: &str,
    _args: Vec<Value>,
) -> Result<Value, String> {
    let array = |data: Vec<f64>| {
        array_value(NumericArray {
            rows: data.len(),
            cols: 1,
            data,
        })
    };
    match name {
        "系数" | "参数" => Ok(array(result.coefficients.clone())),
        "P值" => Ok(array(result.p_values.clone())),
        "标准误" => Ok(array(result.standard_errors.clone())),
        "预测" | "拟合值" => Ok(array(result.fitted.clone())),
        "残差" => Ok(array(result.residuals.clone())),
        "诊断" => result
            .diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics_table(influence_rows(diagnostics)))
            .ok_or_else(|| error("当前回归结果没有影响诊断，只有线性回归提供该表")),
        "共线性" => result
            .diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics_table(collinearity_rows(diagnostics)))
            .ok_or_else(|| error("当前回归结果没有共线性诊断，只有线性回归提供该表")),
        "分类指标" => result
            .classification
            .as_ref()
            .map(|metrics| diagnostics_table(classification_rows(metrics)))
            .ok_or_else(|| error("当前回归结果没有分类指标，只有逻辑回归提供该表")),
        "摘要" | "报告" => Ok(Value::Text(match &result.classification {
            Some(metrics) => format!(
                "逻辑回归：对数损失={}，准确率={}，AUC={}，系数=[{}]",
                metrics.log_loss,
                metrics.accuracy,
                metrics.roc_auc,
                result
                    .coefficients
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None => format!(
                "线性回归：R平方={}，调整R平方={}，RMSE={}，MAE={}，系数=[{}]",
                result.r_squared,
                result.adjusted_r_squared,
                result.rmse,
                result.mae,
                result
                    .coefficients
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })),
        _ => Err(error(format!("回归结果不支持动作“{name}”。"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_of(value: &Value) -> BTreeMap<String, Value> {
        match value {
            Value::Record(record) => record.borrow().clone(),
            other => panic!("expected record, got {other}"),
        }
    }

    fn float(record: &BTreeMap<String, Value>, name: &str) -> f64 {
        record
            .get(name)
            .and_then(|value| value.as_f64().ok())
            .unwrap_or_else(|| panic!("missing numeric {name}"))
    }

    fn text(record: &BTreeMap<String, Value>, name: &str) -> String {
        record
            .get(name)
            .map(ToString::to_string)
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    /// 统一契约里的公共字段必须在每个检验上都出现，且全是标量，
    /// 否则报告侧的列规范和 CSV 导出会各自猜结构。
    const TEST_RESULT_FIELDS: [&str; 15] = [
        "类型",
        "方法",
        "统计量",
        "P值",
        "自由度",
        "样本量",
        "缺失配对数",
        "效应量名称",
        "效应量",
        "效应量判读",
        "置信下限",
        "置信上限",
        "置信度",
        "显著性",
        "数字含义",
    ];

    #[test]
    fn every_test_shares_the_same_scalar_result_contract() {
        let a = column(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        let b = column(vec![2.0, 4.0, 5.0, 4.0, 6.0]);
        let results = vec![
            one_sample_t(&a, 2.0).unwrap(),
            two_sample_t(&a, &b).unwrap(),
            paired_t(&a, &b).unwrap(),
            spearman(&a, &b).unwrap(),
            shapiro(&column(vec![1.0, 2.0, 3.0, 4.0, 9.0])).unwrap(),
            chi_square_test(&column(vec![10.0, 20.0, 30.0]), None).unwrap(),
            mann_whitney_u(&a, &b).unwrap(),
            one_way_anova(&Value::list(vec![a.clone(), b.clone()])).unwrap(),
            fisher_exact(&matrix(2, 2, vec![8.0, 2.0, 1.0, 9.0])).unwrap(),
        ];
        for result in &results {
            let record = record_of(result);
            assert_eq!(text(&record, "类型"), "统计检验");
            for field in TEST_RESULT_FIELDS {
                assert!(
                    record.contains_key(field),
                    "{} 缺少统一字段 {field}",
                    text(&record, "方法")
                );
            }
            // 所有单元格都必须是标量，才能无损写进 CSV。
            for (name, value) in &record {
                assert!(
                    !matches!(value, Value::Record(_) | Value::Table(_) | Value::List(_)),
                    "字段 {name} 是嵌套结构，CSV 无法无损表达"
                );
            }
            let p = float(&record, "P值");
            assert!((0.0..=1.0).contains(&p), "P 值必须落在 [0,1]，实际 {p}");
            let significance = text(&record, "显著性");
            assert_eq!(significance == "显著", p < 0.05, "显著性判断与 P 值不一致");
            if record.get("置信下限").is_some_and(|v| v.as_f64().is_ok()) {
                assert!(
                    float(&record, "置信下限") <= float(&record, "置信上限"),
                    "置信区间必须有序"
                );
            }
        }
    }

    /// 教科书算例：a=[1,2,3,4,5] 对总体均值 2，
    /// 均值 3、样本标准差 √2.5、标准误 √0.5，t=(3-2)/√0.5。
    #[test]
    fn one_sample_t_reports_hand_computed_effect_and_interval() {
        let record = record_of(&one_sample_t(&column(vec![1.0, 2.0, 3.0, 4.0, 5.0]), 2.0).unwrap());
        assert_eq!(text(&record, "方法"), "单样本t检验");
        assert!((float(&record, "统计量") - 1.0 / 0.5f64.sqrt()).abs() < 1e-9);
        assert_eq!(float(&record, "自由度"), 4.0);
        assert_eq!(float(&record, "样本量"), 5.0);
        assert_eq!(float(&record, "均值差"), 1.0);
        // Cohen's d = 均值差 / 样本标准差 = 1 / √2.5
        assert!((float(&record, "效应量") - 1.0 / 2.5f64.sqrt()).abs() < 1e-9);
        assert_eq!(text(&record, "效应量名称"), "Cohen's d");
        // 1/√2.5 ≈ 0.632 落在 0.5..0.8，属于“中”效应。
        assert_eq!(text(&record, "效应量判读"), "中");
        assert_eq!(float(&record, "置信度"), 0.95);
        // 区间以均值差为中心。
        let center = (float(&record, "置信下限") + float(&record, "置信上限")) / 2.0;
        assert!((center - 1.0).abs() < 1e-9);
        assert!(text(&record, "方法前提").contains("正态"));
    }

    #[test]
    fn paired_test_counts_pairs_dropped_for_missing_values() {
        let left = column(vec![1.0, 2.0, 3.0, f64::NAN, 5.0]);
        let right = column(vec![2.0, 4.0, 5.0, 4.0, 6.0]);
        let record = record_of(&paired_t(&left, &right).unwrap());
        assert_eq!(float(&record, "样本量"), 4.0, "缺失对应被成对剔除");
        assert_eq!(float(&record, "缺失配对数"), 1.0);
        assert!(text(&record, "警告").contains("1 对"), "{record:?}");
    }

    #[test]
    fn non_significant_results_do_not_claim_equivalence() {
        // 两组几乎相同，P 值应该很大。
        let record = record_of(
            &two_sample_t(
                &column(vec![1.0, 2.0, 3.0, 4.0]),
                &column(vec![1.0, 2.0, 3.0, 4.1]),
            )
            .unwrap(),
        );
        assert_eq!(text(&record, "显著性"), "不显著");
        let reading = text(&record, "数字含义");
        assert!(
            reading.contains("这不等于两者相同"),
            "不显著必须说明它不证明相等：{reading}"
        );
    }

    #[test]
    fn chi_square_warns_about_small_expected_counts() {
        // 期望频数 = 平均值 2，低于 5 的经验门槛。
        let record = record_of(&chi_square_test(&column(vec![1.0, 3.0]), None).unwrap());
        assert!(text(&record, "警告").contains("期望频数小于 5"));
        assert_eq!(text(&record, "效应量名称"), "Cramér's V");
    }

    #[test]
    fn shapiro_warns_that_non_significance_is_not_proof_of_normality() {
        let record = record_of(&shapiro(&column(vec![1.0, 2.0, 3.0, 4.0, 5.0])).unwrap());
        assert!(text(&record, "警告").contains("不能反过来证明"));
    }

    #[test]
    fn fisher_exact_keeps_odds_ratio_and_flags_zero_cells() {
        let record = record_of(&fisher_exact(&matrix(2, 2, vec![8.0, 2.0, 1.0, 9.0])).unwrap());
        // 优势比 = (8*9)/(2*1) = 36
        assert!((float(&record, "优势比") - 36.0).abs() < 1e-9);
        assert_eq!(text(&record, "效应量名称"), "phi 系数");

        let zero = record_of(&fisher_exact(&matrix(2, 2, vec![8.0, 0.0, 1.0, 9.0])).unwrap());
        assert!(matches!(zero["优势比"], Value::Null));
        assert!(text(&zero, "警告").contains("优势比无定义"));
    }

    #[test]
    fn spearman_keeps_correlation_and_bounds_the_interval() {
        // 完全单调递增：ρ=1，Fisher z 发散，因此不给区间而给警告。
        let perfect = record_of(
            &spearman(
                &column(vec![1.0, 2.0, 3.0, 4.0]),
                &column(vec![2.0, 4.0, 6.0, 8.0]),
            )
            .unwrap(),
        );
        assert!((float(&perfect, "相关系数") - 1.0).abs() < 1e-9);
        assert!(matches!(perfect["置信下限"], Value::Null));
        assert!(text(&perfect, "警告").contains("无法给出置信区间"));

        let partial = record_of(
            &spearman(
                &column(vec![1.0, 2.0, 3.0, 4.0, 5.0]),
                &column(vec![2.0, 1.0, 4.0, 3.0, 5.0]),
            )
            .unwrap(),
        );
        let low = float(&partial, "置信下限");
        let high = float(&partial, "置信上限");
        assert!(low <= high);
        // 相关系数的区间必须落在 [-1, 1]。
        assert!((-1.0..=1.0).contains(&low) && (-1.0..=1.0).contains(&high));
    }

    fn matrix(rows: usize, cols: usize, data: Vec<f64>) -> Value {
        array_value(NumericArray { rows, cols, data })
    }

    fn column(data: Vec<f64>) -> Value {
        let rows = data.len();
        matrix(rows, 1, data)
    }

    fn regression(value: &Value) -> Rc<RegressionResult> {
        match value {
            Value::Regression(result) => result.clone(),
            other => panic!("expected regression, got {other}"),
        }
    }

    fn table_rows(value: &Value) -> Rows {
        match value {
            Value::Table(table) => table.borrow().rows.clone(),
            other => panic!("expected table, got {other}"),
        }
    }

    fn cell(rows: &Rows, row: usize, name: &str) -> f64 {
        rows[row]
            .get(name)
            .and_then(|value| value.as_f64().ok())
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    /// y=[2,4,5,4,5], x=[1,2,3,4,5]：斜率 0.6、截距 2.2、SSE 2.4、MSE 0.8，
    /// 杠杆值 1/n+(x-x̄)²/Sxx，可以手算核对。
    #[test]
    fn ols_diagnostics_match_hand_computed_leverage_residuals_and_cooks_distance() {
        let result = regression(
            &linear_regression(
                &column(vec![2.0, 4.0, 5.0, 4.0, 5.0]),
                &column(vec![1.0, 2.0, 3.0, 4.0, 5.0]),
            )
            .unwrap(),
        );
        let diagnostics = result.diagnostics.as_ref().expect("线性回归应有影响诊断");
        assert_eq!(diagnostics.leverage.len(), 5);
        for (actual, expected) in diagnostics.leverage.iter().zip([0.6, 0.3, 0.2, 0.3, 0.6]) {
            assert!((actual - expected).abs() < 1e-9, "{actual} vs {expected}");
        }
        // 帽子矩阵的迹等于参数个数，是杠杆值正确性的强不变量。
        let trace = diagnostics.leverage.iter().sum::<f64>();
        assert!((trace - 2.0).abs() < 1e-9, "trace={trace}");
        assert!(
            (diagnostics.standardized_residuals[0] + std::f64::consts::SQRT_2).abs() < 1e-12,
            "第一个标准化残差应恰好是 -√2"
        );
        assert!((diagnostics.standardized_residuals[2] - 1.25).abs() < 1e-9);
        assert!((diagnostics.cooks_distance[0] - 1.5).abs() < 1e-9);
        assert!((diagnostics.cooks_distance[2] - 0.195_312_5).abs() < 1e-9);
        // 单个预测变量不可能与自己共线，VIF 必须恰好是 1。
        assert_eq!(diagnostics.variance_inflation.len(), 1);
        assert!((diagnostics.variance_inflation[0] - 1.0).abs() < 1e-9);
        assert!(result.classification.is_none(), "线性回归不产生分类指标");
    }

    /// x1=[1..5] 与 x2=[1,2,3,4,6] 的相关系数平方为 144/148，
    /// 因此两个预测变量的 VIF 都应是 1/(1-144/148)=37。
    #[test]
    fn variance_inflation_detects_near_collinear_predictors() {
        let x = matrix(
            5,
            2,
            vec![
                1.0, 1.0, //
                2.0, 2.0, //
                3.0, 3.0, //
                4.0, 4.0, //
                5.0, 6.0,
            ],
        );
        let result =
            regression(&linear_regression(&column(vec![1.0, 3.0, 2.0, 5.0, 4.0]), &x).unwrap());
        let diagnostics = result.diagnostics.as_ref().expect("线性回归应有影响诊断");
        assert_eq!(diagnostics.variance_inflation.len(), 2);
        for vif in &diagnostics.variance_inflation {
            assert!((vif - 37.0).abs() < 1e-6, "vif={vif}");
        }
    }

    #[test]
    fn regression_exposes_diagnostics_as_normalized_tables() {
        let result = regression(
            &linear_regression(
                &column(vec![2.0, 4.0, 5.0, 4.0, 5.0]),
                &column(vec![1.0, 2.0, 3.0, 4.0, 5.0]),
            )
            .unwrap(),
        );
        let influence = table_rows(&regression_method(&result, "诊断", Vec::new()).unwrap());
        assert_eq!(influence.len(), 5);
        assert_eq!(cell(&influence, 0, "观测"), 1.0);
        assert!((cell(&influence, 0, "杠杆值") - 0.6).abs() < 1e-9);
        assert!((cell(&influence, 0, "Cook距离") - 1.5).abs() < 1e-9);
        // 规范化表格的单元格必须是标量，才能无损写进 CSV。
        assert!(influence.iter().all(|row| row
            .values()
            .all(|value| !matches!(value, Value::Record(_) | Value::Table(_)))));

        let collinearity = table_rows(&regression_method(&result, "共线性", Vec::new()).unwrap());
        assert_eq!(collinearity.len(), 1);
        assert!((cell(&collinearity, 0, "VIF") - 1.0).abs() < 1e-9);

        let message = regression_method(&result, "分类指标", Vec::new()).unwrap_err();
        assert!(message.contains("只有逻辑回归"), "{message}");
    }

    #[test]
    fn classification_metrics_report_confusion_matrix_and_rank_based_auc() {
        let metrics =
            classification_metrics(&[1.0, 1.0, 0.0, 0.0], &[0.9, 0.4, 0.6, 0.1], 0.5).unwrap();
        assert_eq!(metrics.true_positive, 1);
        assert_eq!(metrics.false_negative, 1);
        assert_eq!(metrics.false_positive, 1);
        assert_eq!(metrics.true_negative, 1);
        assert!((metrics.accuracy - 0.5).abs() < 1e-12);
        // 正例概率 [0.9,0.4] 对负例 [0.6,0.1] 的胜出对数为 3，共 4 对。
        assert!((metrics.roc_auc - 0.75).abs() < 1e-12);

        // 全部预测相同时没有区分能力，平票各计 0.5。
        let tied = classification_metrics(&[1.0, 0.0], &[0.5, 0.5], 0.7).unwrap();
        assert!((tied.roc_auc - 0.5).abs() < 1e-12);

        // 单一类别下 AUC 无定义，报告 0.5 而不是假装完美。
        let single = classification_metrics(&[1.0, 1.0], &[0.9, 0.8], 0.2).unwrap();
        assert!((single.roc_auc - 0.5).abs() < 1e-12);
        assert!((single.accuracy - 1.0).abs() < 1e-12);
    }
}
