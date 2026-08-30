use super::{demand_values, error, named_f64, named_usize, optional_float, EPSILON};
use crate::analysis::statistics::{mean, variance};
use crate::value::Value;
use std::collections::BTreeMap;

pub(super) struct Stability {
    periods: usize,
    nonzero_periods: usize,
    nonzero_rate: f64,
    mean: f64,
    standard_deviation: f64,
    coefficient_of_variation: Option<f64>,
    adi: Option<f64>,
    nonzero_cv_squared: Option<f64>,
    pub(super) demand_type: String,
    longest_streak: usize,
    recent_streak: usize,
    trend: f64,
    pub(super) stable: bool,
}

impl Stability {
    pub(super) fn to_record(&self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("周期数".into(), Value::Float(self.periods as f64)),
            (
                "有销量周期数".into(),
                Value::Float(self.nonzero_periods as f64),
            ),
            ("有销量率".into(), Value::Float(self.nonzero_rate)),
            ("平均周期销量".into(), Value::Float(self.mean)),
            ("销量标准差".into(), Value::Float(self.standard_deviation)),
            (
                "变异系数".into(),
                optional_float(self.coefficient_of_variation),
            ),
            ("ADI".into(), optional_float(self.adi)),
            (
                "非零需求变异系数平方".into(),
                optional_float(self.nonzero_cv_squared),
            ),
            ("需求类型".into(), Value::Text(self.demand_type.clone())),
            (
                "最长连续有销量周期".into(),
                Value::Float(self.longest_streak as f64),
            ),
            (
                "最近连续有销量周期".into(),
                Value::Float(self.recent_streak as f64),
            ),
            ("周期趋势".into(), Value::Float(self.trend)),
            ("稳定".into(), Value::Bool(self.stable)),
        ])
    }
}

pub(in crate::analysis) fn demand_stability(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = demand_values(value, true)?;
    stability_from(&values, named).map(|value| Value::record(value.to_record()))
}

pub(super) fn stability_from(
    values: &[f64],
    named: &BTreeMap<String, Value>,
) -> Result<Stability, String> {
    if values.is_empty() {
        return Err(error("需求稳定性分析至少需要一个周期"));
    }
    let minimum_periods = named_usize(named, "最低周期数", 12)?;
    let minimum_nonzero_rate = named_f64(named, "最低有销量率", 0.8)?;
    let maximum_cv = named_f64(named, "最大变异系数", 1.0)?;
    let minimum_average = named_f64(named, "最低平均销量", 0.0)?;
    if !(0.0..=1.0).contains(&minimum_nonzero_rate) || maximum_cv < 0.0 || minimum_average < 0.0 {
        return Err(error(
            "稳定性阈值必须是有效非负数，有销量率必须在 0 和 1 之间",
        ));
    }
    let nonzero = values
        .iter()
        .copied()
        .filter(|value| *value > EPSILON)
        .collect::<Vec<_>>();
    let average = mean(values);
    let standard_deviation = variance(values, 0).sqrt();
    let coefficient_of_variation =
        (average.abs() > EPSILON).then_some(standard_deviation / average.abs());
    let nonzero_rate = nonzero.len() as f64 / values.len() as f64;
    let adi = (!nonzero.is_empty()).then_some(values.len() as f64 / nonzero.len() as f64);
    let nonzero_cv_squared = if nonzero.len() >= 2 {
        let center = mean(&nonzero);
        (center.abs() > EPSILON).then_some(variance(&nonzero, 0) / center.powi(2))
    } else {
        None
    };
    let demand_type = match (adi, nonzero_cv_squared) {
        (None, _) => "无需求",
        (Some(adi), Some(cv_squared)) if adi < 1.32 && cv_squared < 0.49 => "平稳需求",
        (Some(adi), Some(_)) if adi < 1.32 => "波动需求",
        (Some(_), Some(cv_squared)) if cv_squared < 0.49 => "间歇需求",
        (Some(_), Some(_)) => "块状需求",
        (Some(adi), None) if adi < 1.32 => "平稳需求",
        (Some(_), None) => "间歇需求",
    }
    .to_string();
    let (longest_streak, recent_streak) = demand_streaks(values);
    let trend = linear_slope(values);
    let stable = values.len() >= minimum_periods
        && average >= minimum_average
        && nonzero_rate >= minimum_nonzero_rate
        && coefficient_of_variation.is_some_and(|value| value <= maximum_cv);
    Ok(Stability {
        periods: values.len(),
        nonzero_periods: nonzero.len(),
        nonzero_rate,
        mean: average,
        standard_deviation,
        coefficient_of_variation,
        adi,
        nonzero_cv_squared,
        demand_type,
        longest_streak,
        recent_streak,
        trend,
        stable,
    })
}

fn demand_streaks(values: &[f64]) -> (usize, usize) {
    let mut longest = 0;
    let mut current = 0;
    for value in values {
        if *value > EPSILON {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    (longest, current)
}

fn linear_slope(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let x_mean = (values.len() - 1) as f64 / 2.0;
    let y_mean = mean(values);
    let denominator = (0..values.len())
        .map(|index| (index as f64 - x_mean).powi(2))
        .sum::<f64>();
    values
        .iter()
        .enumerate()
        .map(|(index, value)| (index as f64 - x_mean) * (value - y_mean))
        .sum::<f64>()
        / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_uses_zero_periods_and_adi_classification() {
        let stable = stability_from(&[5.0; 12], &BTreeMap::new()).unwrap();
        assert!(stable.stable);
        assert_eq!(stable.demand_type, "平稳需求");
        let intermittent = stability_from(
            &[5.0, 0.0, 0.0, 5.0, 0.0, 0.0, 5.0, 0.0, 0.0, 5.0, 0.0, 0.0],
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(!intermittent.stable);
        assert_eq!(intermittent.demand_type, "间歇需求");
    }
}
