use crate::value::NumericArray;

pub(super) fn mean(values: &[f64]) -> f64 {
    let count = values.len() as f64;
    values.iter().map(|value| value / count).sum()
}

pub(super) fn variance(values: &[f64], degrees_of_freedom: usize) -> f64 {
    let center = mean(values);
    values
        .iter()
        .map(|value| (value - center).powi(2))
        .sum::<f64>()
        / values.len().saturating_sub(degrees_of_freedom).max(1) as f64
}

pub(super) fn mean_absolute_deviation(values: &[f64]) -> f64 {
    let center = mean(values);
    values
        .iter()
        .map(|value| (value - center).abs())
        .sum::<f64>()
        / values.len() as f64
}

pub(super) fn median_absolute_deviation(values: &[f64]) -> f64 {
    let mut ordered = values.to_vec();
    let center = quantile(&mut ordered, 0.5);
    let mut deviations = values
        .iter()
        .map(|value| (value - center).abs())
        .collect::<Vec<_>>();
    quantile(&mut deviations, 0.5)
}

pub(super) fn skewness(values: &[f64]) -> Result<f64, String> {
    if values.len() < 3 {
        return Err("偏度至少需要三个有效数据".into());
    }
    let count = values.len() as f64;
    let center = mean(values);
    let second = values
        .iter()
        .map(|value| (value - center).powi(2))
        .sum::<f64>()
        / count;
    if !second.is_finite() || second <= f64::EPSILON {
        return Err("数据方差为零，无法计算偏度".into());
    }
    let third = values
        .iter()
        .map(|value| (value - center).powi(3))
        .sum::<f64>()
        / count;
    Ok((count * (count - 1.0)).sqrt() / (count - 2.0) * third / second.powf(1.5))
}

pub(super) fn excess_kurtosis(values: &[f64]) -> Result<f64, String> {
    if values.len() < 4 {
        return Err("峰度至少需要四个有效数据".into());
    }
    let count = values.len() as f64;
    let center = mean(values);
    let second_sum = values
        .iter()
        .map(|value| (value - center).powi(2))
        .sum::<f64>();
    if !second_sum.is_finite() || second_sum <= f64::EPSILON {
        return Err("数据方差为零，无法计算峰度".into());
    }
    let fourth_sum = values
        .iter()
        .map(|value| (value - center).powi(4))
        .sum::<f64>();
    let biased_excess = count * fourth_sum / second_sum.powi(2) - 3.0;
    Ok((count - 1.0) / ((count - 2.0) * (count - 3.0)) * ((count + 1.0) * biased_excess + 6.0))
}

pub(super) fn autocorrelation(values: &[f64], lag: usize) -> Result<f64, String> {
    if lag == 0 || lag >= values.len() {
        return Err("自相关滞后必须大于零且小于有效数据数量".into());
    }
    let center = mean(values);
    let denominator = values
        .iter()
        .map(|value| (value - center).powi(2))
        .sum::<f64>();
    if !denominator.is_finite() || denominator <= f64::EPSILON {
        return Err("数据方差为零，无法计算自相关".into());
    }
    let numerator = values[..values.len() - lag]
        .iter()
        .zip(&values[lag..])
        .map(|(left, right)| (left - center) * (right - center))
        .sum::<f64>();
    Ok(numerator / denominator)
}

pub(super) fn quantile(values: &mut [f64], probability: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let position = probability.clamp(0.0, 1.0) * (values.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        values[lower]
    } else {
        values[lower] + (values[upper] - values[lower]) * (position - lower as f64)
    }
}

pub(super) fn covariance(left: &NumericArray, right: &NumericArray) -> Result<f64, String> {
    let (left, right) = paired(left, right)?;
    if left.len() < 2 {
        return Err("协方差至少需要两个有效数据对".into());
    }
    let (left_mean, right_mean) = (mean(&left), mean(&right));
    Ok(left
        .iter()
        .zip(right)
        .map(|(left, right)| (left - left_mean) * (right - right_mean))
        .sum::<f64>()
        / (left.len() - 1) as f64)
}

pub(super) fn correlation(left: &NumericArray, right: &NumericArray) -> Result<f64, String> {
    let (left, right) = paired(left, right)?;
    if left.len() < 2 {
        return Err("相关系数至少需要两个有效数据对".into());
    }
    let (left_mean, right_mean) = (mean(&left), mean(&right));
    let numerator = left
        .iter()
        .zip(&right)
        .map(|(left, right)| (left - left_mean) * (right - right_mean))
        .sum::<f64>();
    let left_sum = left
        .iter()
        .map(|value| (value - left_mean).powi(2))
        .sum::<f64>();
    let right_sum = right
        .iter()
        .map(|value| (value - right_mean).powi(2))
        .sum::<f64>();
    if !left_sum.is_finite()
        || !right_sum.is_finite()
        || left_sum <= f64::EPSILON
        || right_sum <= f64::EPSILON
    {
        return Err("数据方差为零，无法计算相关系数".into());
    }
    Ok(numerator / (left_sum * right_sum).sqrt())
}

fn paired(left: &NumericArray, right: &NumericArray) -> Result<(Vec<f64>, Vec<f64>), String> {
    if left.data.len() != right.data.len() {
        return Err("两组数据长度必须一致".into());
    }
    let mut finite_left = Vec::new();
    let mut finite_right = Vec::new();
    for (left, right) in left.data.iter().zip(&right.data) {
        if left.is_finite() && right.is_finite() {
            finite_left.push(*left);
            finite_right.push(*right);
        }
    }
    Ok((finite_left, finite_right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_statistics_match_known_values() {
        let values = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(mean(&values), 2.5);
        assert!((variance(&values, 0) - 1.25).abs() < 1e-12);
        let mut quantiles = values;
        assert_eq!(quantile(&mut quantiles, 0.5), 2.5);
        assert_eq!(mean_absolute_deviation(&values), 1.0);
        assert_eq!(median_absolute_deviation(&values), 1.0);
        assert!(skewness(&values).unwrap().abs() < 1e-12);
        assert!((excess_kurtosis(&values).unwrap() + 1.2).abs() < 1e-12);
        assert!((autocorrelation(&values, 1).unwrap() - 0.25).abs() < 1e-12);
    }

    #[test]
    fn higher_moments_reject_degenerate_samples() {
        assert!(skewness(&[1.0, 1.0, 1.0]).unwrap_err().contains("方差为零"));
        assert!(excess_kurtosis(&[1.0, 1.0, 1.0, 1.0])
            .unwrap_err()
            .contains("方差为零"));
        assert!(autocorrelation(&[1.0, 1.0], 1)
            .unwrap_err()
            .contains("方差为零"));
    }
}
