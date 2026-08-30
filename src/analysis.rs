use crate::resource::DEFAULT_MAX_COLLECTION_ITEMS;
use crate::runtime::Interpreter;
use crate::value::*;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use statrs::distribution::{ContinuousCDF, Normal};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

mod forecasting;
mod insights;
mod io;
mod linear_algebra;
mod models;
mod statistics;

use forecasting::{
    batch_demand_forecast, demand_forecast, demand_stability, detect_outliers, exclude_outliers,
    forecast_accuracy, safety_stock, winsorize_outliers,
};
use insights::{cohort, contribution, data_profile, funnel, period_compare};
use io::{chunk_aggregate, open_parquet_stream, read_csv_chunks, read_parquet};
use linear_algebra::{eigenvalues, matrix_multiply, singular_values, solve};
use models::{
    adjust_p_values, chi_square_test, confidence_interval, decompose_series, fisher_exact,
    forecast_series, linear_program, linear_regression, logistic_regression, mann_whitney_u,
    one_sample_t, one_way_anova, paired_t, shapiro, spearman, two_sample_t,
};
pub use models::{regression_attr, regression_method};
use statistics::{
    autocorrelation, correlation, covariance, excess_kurtosis, mean, mean_absolute_deviation,
    median_absolute_deviation, quantile, skewness, variance,
};

fn error(message: impl AsRef<str>) -> String {
    message.as_ref().to_string()
}
fn array_value(array: NumericArray) -> Value {
    Value::NumericArray(Rc::new(array))
}
fn clean(array: &NumericArray) -> Vec<f64> {
    array
        .data
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect()
}
fn arg_text(named: &BTreeMap<String, Value>, name: &str, default: &str) -> String {
    named
        .get(name)
        .map(ToString::to_string)
        .unwrap_or_else(|| default.into())
}
fn arg_usize(named: &BTreeMap<String, Value>, name: &str, default: usize) -> Result<usize, String> {
    named
        .get(name)
        .map(|value| value_usize(value, name))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn value_usize(value: &Value, name: &str) -> Result<usize, String> {
    let value = value.as_decimal()?;
    if !value.fract().is_zero() {
        return Err(error(format!("{name}必须是非负整数")));
    }
    value
        .to_usize()
        .ok_or_else(|| error(format!("{name}超出支持范围")))
}

pub fn analysis_native(
    interpreter: &mut Interpreter,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
) -> Result<Value, String> {
    crate::contracts::validate_arity(name, args.len(), analysis_arity(name))?;
    let collection_limit = interpreter.permissions.collection_limit();
    for value in args.iter().chain(named.values()) {
        ensure_collection_limit(value, collection_limit).map_err(error)?;
    }
    match name {
        "数组" | "数值数组" => to_array(args.first().ok_or_else(|| error("数组缺少数据"))?)
            .map(array_value)
            .map_err(error),
        "从表格" => match args.first() {
            Some(Value::Table(v)) => Ok(Value::Table(v.clone())),
            _ => Err(error("从表格需要表格对象")),
        },
        "线性回归" => linear_regression(&args[0], &args[1]),
        "单样本T检验" => one_sample_t(
            &args[0],
            args.get(1)
                .or_else(|| named.get("总体均值"))
                .map(Value::as_f64)
                .transpose()?
                .unwrap_or(0.0),
        ),
        "独立双样本t检验" => two_sample_t(&args[0], &args[1]),
        "正态分布概率" => {
            let x = args[0].as_f64()?;
            let mean = args
                .get(1)
                .or_else(|| named.get("均值"))
                .map(Value::as_f64)
                .transpose()?
                .unwrap_or(0.0);
            let std = args
                .get(2)
                .or_else(|| named.get("标准差"))
                .map(Value::as_f64)
                .transpose()?
                .unwrap_or(1.0);
            Ok(Value::Float(
                Normal::new(mean, std)
                    .map_err(|e| error(e.to_string()))?
                    .cdf(x),
            ))
        }
        "正态分布分位点" => {
            let p = args
                .first()
                .or_else(|| named.get("概率"))
                .map(Value::as_f64)
                .transpose()?
                .unwrap_or(0.5);
            if !p.is_finite() || !(0.0..1.0).contains(&p) {
                return Err(error("正态分布分位点的概率必须在 0 和 1 之间"));
            }
            Ok(Value::Float(
                Normal::new(0.0, 1.0)
                    .map_err(|error_value| error(error_value.to_string()))?
                    .inverse_cdf(p),
            ))
        }
        "夏皮罗威尔克检验" => shapiro(&args[0]),
        "单因素方差分析" => one_way_anova(&args[0]),
        "卡方检验" => chi_square_test(&args[0], args.get(1)),
        "曼惠特尼U检验" => mann_whitney_u(&args[0], &args[1]),
        "配对样本t检验" => paired_t(&args[0], &args[1]),
        "Spearman相关" => spearman(&args[0], &args[1]),
        "Fisher精确检验" => fisher_exact(&args[0]),
        "多重检验校正" => adjust_p_values(&args[0], &named),
        "均值置信区间" => confidence_interval(&args[0], &named),
        "逻辑回归" => logistic_regression(&args[0], &args[1]),
        "时间序列分解" => decompose_series(&args[0], &named),
        "线性预测" => forecast_series(&args[0], &named),
        "异常值检测" => detect_outliers(&args[0], &named),
        "排除异常值" => exclude_outliers(&args[0], &named),
        "缩尾异常值" => winsorize_outliers(&args[0], &named),
        "预测准确度" => forecast_accuracy(&args[0], &args[1]),
        "需求稳定性" => demand_stability(&args[0], &named),
        "需求预测" => {
            demand_forecast(&args[0], &named, interpreter.permissions.collection_limit())
        }
        "安全库存" => safety_stock(&args[0], &named),
        "批量需求预测" => {
            batch_demand_forecast(&args[0], &named, interpreter.permissions.collection_limit())
        }
        "线性规划" => linear_program(&args[0], &args[1], &named),
        "数据概览" => data_profile(&args[0], &named, collection_limit),
        "期间比较" => period_compare(&args[0], &named, collection_limit),
        "贡献分析" => contribution(&args[0], &named, collection_limit, false),
        "Pareto分析" => contribution(&args[0], &named, collection_limit, true),
        "队列分析" => cohort(&args[0], &named, collection_limit),
        "漏斗分析" => funnel(&args[0], &named, collection_limit),
        "读取CSV分块" => {
            let chunk_size = args
                .get(1)
                .map(|value| value_usize(value, "块大小"))
                .transpose()?
                .unwrap_or(arg_usize(&named, "块大小", 100_000)?);
            read_csv_chunks(
                &authorized_path(interpreter, &args[0])?.to_string_lossy(),
                chunk_size,
            )
        }
        "读取Parquet" => read_parquet(&authorized_path(interpreter, &args[0])?.to_string_lossy()),
        "读取Parquet流" => {
            open_parquet_stream(&authorized_path(interpreter, &args[0])?.to_string_lossy())
        }
        "分块聚合" => chunk_aggregate(
            &authorized_path(interpreter, &args[0])?.to_string_lossy(),
            &args[1].to_string(),
            &arg_text(&named, "统计量", "平均值"),
            arg_usize(&named, "块大小", 100_000)?,
            arg_usize(&named, "工作者", 1)?.min(interpreter.permissions.collection_limit()),
        ),
        _ => Err(error(format!("未知统计功能“{name}”"))),
    }
}

fn analysis_arity(name: &str) -> Option<(usize, Option<usize>)> {
    Some(match name {
        "数组"
        | "数值数组"
        | "从表格"
        | "夏皮罗威尔克检验"
        | "单因素方差分析"
        | "均值置信区间"
        | "时间序列分解"
        | "线性预测"
        | "异常值检测"
        | "排除异常值"
        | "缩尾异常值"
        | "需求稳定性"
        | "需求预测"
        | "安全库存"
        | "批量需求预测"
        | "数据概览"
        | "期间比较"
        | "贡献分析"
        | "Pareto分析"
        | "队列分析"
        | "漏斗分析"
        | "读取Parquet"
        | "读取Parquet流" => (1, Some(1)),
        "Fisher精确检验" | "多重检验校正" => (1, Some(1)),
        "正态分布分位点" => (0, Some(1)),
        "单样本T检验" | "卡方检验" | "读取CSV分块" => (1, Some(2)),
        "正态分布概率" => (1, Some(3)),
        "线性回归"
        | "独立双样本t检验"
        | "曼惠特尼U检验"
        | "配对样本t检验"
        | "Spearman相关"
        | "逻辑回归"
        | "线性规划"
        | "预测准确度" => (2, Some(2)),
        "分块聚合" => (3, Some(5)),
        _ => return None,
    })
}

fn authorized_path(interpreter: &Interpreter, value: &Value) -> Result<PathBuf, String> {
    let path = PathBuf::from(value.to_string());
    let path = if path.is_absolute() {
        path
    } else {
        interpreter.base_dir.join(path)
    };
    interpreter.permissions.check_file(&path)?;
    Ok(path)
}

fn vector(data: Vec<f64>) -> NumericArray {
    NumericArray {
        rows: data.len(),
        cols: 1,
        data,
    }
}

pub fn to_array(value: &Value) -> Result<NumericArray, String> {
    match value {
        Value::NumericArray(v) => Ok((**v).clone()),
        Value::List(rows) => {
            let rows = rows.borrow();
            if rows.first().is_some_and(|v| matches!(v, Value::List(_))) {
                let height = rows.len();
                let mut data = Vec::new();
                let mut width = None;
                for row in rows.iter() {
                    let values = row.iter_values_limited(DEFAULT_MAX_COLLECTION_ITEMS)?;
                    if width
                        .replace(values.len())
                        .is_some_and(|w| w != values.len())
                    {
                        return Err("矩阵每行长度必须一致".into());
                    }
                    for v in values {
                        data.push(if matches!(v, Value::Null) {
                            f64::NAN
                        } else {
                            v.as_f64()?
                        })
                    }
                }
                Ok(NumericArray {
                    data,
                    rows: height,
                    cols: width.unwrap_or(0),
                })
            } else {
                let data = rows
                    .iter()
                    .map(|v| {
                        if matches!(v, Value::Null) {
                            Ok(f64::NAN)
                        } else {
                            v.as_f64()
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(NumericArray {
                    rows: data.len(),
                    cols: 1,
                    data,
                })
            }
        }
        _ => {
            let data = value
                .iter_values_limited(DEFAULT_MAX_COLLECTION_ITEMS)?
                .iter()
                .map(Value::as_f64)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(NumericArray {
                rows: data.len(),
                cols: 1,
                data,
            })
        }
    }
}

pub fn array_attr(array: &NumericArray, name: &str) -> Option<Value> {
    match name {
        "形状" | "维度" => Some(Value::list(if array.cols == 1 {
            vec![Value::Number(Decimal::from(array.rows))]
        } else {
            vec![
                Value::Number(Decimal::from(array.rows)),
                Value::Number(Decimal::from(array.cols)),
            ]
        })),
        "行数" => Some(Value::Number(Decimal::from(array.rows))),
        "列数" => Some(Value::Number(Decimal::from(array.cols))),
        "元素个数" => Some(Value::Number(Decimal::from(array.data.len()))),
        _ => None,
    }
}

pub fn array_method(
    array: &NumericArray,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
) -> Result<Value, String> {
    let mut values = clean(array);
    if values.is_empty() && !matches!(name, "填补缺失" | "去除缺失") {
        return Err(error("有效数据不能为空"));
    }
    match name {
        "平均值" | "平均" => finite_float(mean(&values), "平均值"),
        "总和" => finite_float(values.iter().sum(), "总和"),
        "中位数" => Ok(Value::Float(quantile(&mut values, 0.5))),
        "方差" => {
            let degrees = arg_usize(&named, "自由度", 0)?;
            if degrees >= values.len() {
                return Err(error("自由度必须小于有效数据数量"));
            }
            Ok(Value::Float(variance(&values, degrees)))
        }
        "标准差" | "标准偏差" => {
            let degrees = arg_usize(&named, "自由度", 0)?;
            if degrees >= values.len() {
                return Err(error("自由度必须小于有效数据数量"));
            }
            Ok(Value::Float(variance(&values, degrees).sqrt()))
        }
        "极差" => finite_float(
            values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - values.iter().copied().fold(f64::INFINITY, f64::min),
            "极差",
        ),
        "四分位距" => {
            let mut lower = values.clone();
            let mut upper = values.clone();
            finite_float(
                quantile(&mut upper, 0.75) - quantile(&mut lower, 0.25),
                "四分位距",
            )
        }
        "平均绝对偏差" => finite_float(mean_absolute_deviation(&values), "平均绝对偏差"),
        "中位绝对偏差" => finite_float(median_absolute_deviation(&values), "中位绝对偏差"),
        "变异系数" => {
            let center = mean(&values);
            if center.abs() <= f64::EPSILON {
                return Err(error("平均值为零，无法计算变异系数"));
            }
            finite_float(variance(&values, 0).sqrt() / center.abs(), "变异系数")
        }
        "偏度" => skewness(&values)
            .and_then(|value| finite_float(value, "偏度"))
            .map_err(|message| {
                if message.starts_with("第 ") {
                    message
                } else {
                    error(message)
                }
            }),
        "峰度" | "超额峰度" => excess_kurtosis(&values)
            .and_then(|value| finite_float(value, "峰度"))
            .map_err(|message| {
                if message.starts_with("第 ") {
                    message
                } else {
                    error(message)
                }
            }),
        "自相关" => {
            let lag = args
                .first()
                .map(|value| value_usize(value, "滞后"))
                .transpose()?
                .unwrap_or(arg_usize(&named, "滞后", 1)?);
            autocorrelation(&values, lag)
                .and_then(|value| finite_float(value, "自相关"))
                .map_err(|message| {
                    if message.starts_with("第 ") {
                        message
                    } else {
                        error(message)
                    }
                })
        }
        "移动平均" => {
            let window = args
                .first()
                .map(|value| value_usize(value, "窗口"))
                .transpose()?
                .unwrap_or(arg_usize(&named, "窗口", 3)?);
            if window == 0 || window > values.len() {
                return Err(error("移动平均窗口必须大于零且不超过有效数据数量"));
            }
            let moving = values.windows(window).map(mean).collect::<Vec<_>>();
            Ok(array_value(vector(moving)))
        }
        "最小值" => Ok(Value::Float(
            values.iter().copied().fold(f64::INFINITY, f64::min),
        )),
        "最大值" => Ok(Value::Float(
            values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        )),
        "分位数" | "百分位数" => {
            let qs = to_array(
                args.first()
                    .or_else(|| named.get("概率"))
                    .ok_or_else(|| error("分位数缺少概率"))?,
            )?;
            let out = qs
                .data
                .into_iter()
                .map(|mut q| -> Result<f64, String> {
                    if name == "百分位数" && q > 1.0 {
                        q /= 100.0
                    }
                    if !q.is_finite() || !(0.0..=1.0).contains(&q) {
                        return Err(error("分位数概率必须在 0 和 1 之间"));
                    }
                    let mut data = values.clone();
                    Ok(quantile(&mut data, q))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(array_value(NumericArray {
                rows: out.len(),
                cols: 1,
                data: out,
            }))
        }
        "累计和" | "累积和" => {
            let mut sum = 0.0;
            let mut data = Vec::with_capacity(array.data.len());
            for value in &array.data {
                if !value.is_nan() {
                    sum += value;
                    if !sum.is_finite() {
                        return Err(error("累计和产生了非有限结果"));
                    }
                }
                data.push(sum);
            }
            Ok(array_value(NumericArray {
                data,
                rows: array.rows,
                cols: array.cols,
            }))
        }
        "累计积" => {
            let mut product = 1.0;
            let mut data = Vec::with_capacity(array.data.len());
            for value in &array.data {
                if !value.is_nan() {
                    product *= value;
                    if !product.is_finite() {
                        return Err(error("累计积产生了非有限结果"));
                    }
                }
                data.push(product);
            }
            Ok(array_value(NumericArray {
                data,
                rows: array.rows,
                cols: array.cols,
            }))
        }
        "去除缺失" => Ok(array_value(NumericArray {
            rows: values.len(),
            cols: 1,
            data: values,
        })),
        "填补缺失" => {
            let replacement = args
                .first()
                .or_else(|| named.get("值"))
                .map(Value::as_f64)
                .transpose()?
                .unwrap_or(0.0);
            Ok(array_value(NumericArray {
                data: array
                    .data
                    .iter()
                    .map(|v| if v.is_nan() { replacement } else { *v })
                    .collect(),
                rows: array.rows,
                cols: array.cols,
            }))
        }
        "相关系数" | "相关" => correlation(array, &to_array(&args[0])?)
            .map(Value::Float)
            .map_err(error),
        "协方差" => covariance(array, &to_array(&args[0])?)
            .map(Value::Float)
            .map_err(error),
        "矩阵乘" => matrix_multiply(array, &to_array(&args[0])?).map(array_value),
        "求解方程" => solve(array, &to_array(&args[0])?).map(array_value),
        "特征值" => eigenvalues(array).map(array_value),
        "奇异值" => singular_values(array).map(array_value),
        "直方统计" | "直方图" => {
            let bins = args
                .first()
                .map(|value| value_usize(value, "分组数"))
                .transpose()?
                .unwrap_or(arg_usize(&named, "分组数", 10)?);
            if bins > DEFAULT_MAX_COLLECTION_ITEMS {
                return Err(error("分组数超过集合资源限制"));
            }
            histogram(&values, bins).map_err(error)
        }
        "描述" => Ok(Value::record(describe(&values))),
        "正态化" => {
            let m = mean(&values);
            let s = variance(&values, 0).sqrt();
            if !s.is_finite() || s <= f64::EPSILON {
                return Err(error("数据方差为零，无法正态化"));
            }
            Ok(array_value(NumericArray {
                data: array.data.iter().map(|v| (v - m) / s).collect(),
                rows: array.rows,
                cols: array.cols,
            }))
        }
        _ => Err(error(format!("数值数组不支持动作“{name}”。"))),
    }
}

fn finite_float(value: f64, operation: &str) -> Result<Value, String> {
    if value.is_finite() {
        Ok(Value::Float(value))
    } else {
        Err(error(format!("{operation}产生了非有限结果")))
    }
}

fn histogram(values: &[f64], bins: usize) -> Result<Value, String> {
    if bins == 0 {
        return Err("分组数必须大于零".into());
    }
    if values.is_empty() {
        return Err("直方统计需要至少一个数值".into());
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let width = (max - min) / bins as f64;
    let mut counts = Vec::new();
    counts
        .try_reserve_exact(bins)
        .map_err(|_| "直方图分组数超出可分配范围".to_string())?;
    counts.resize(bins, 0.0);
    for value in values {
        let index = if width > 0.0 {
            (((*value - min) / width) as usize).min(bins - 1)
        } else {
            0
        };
        counts[index] += 1.0
    }
    let edge_count = bins
        .checked_add(1)
        .ok_or_else(|| "直方图分组数超出支持范围".to_string())?;
    let mut edges = Vec::new();
    edges
        .try_reserve_exact(edge_count)
        .map_err(|_| "直方图分组数超出可分配范围".to_string())?;
    edges.extend((0..=bins).map(|i| min + i as f64 * width));
    Ok(Value::record(BTreeMap::from([
        (
            "计数".into(),
            array_value(NumericArray {
                rows: counts.len(),
                cols: 1,
                data: counts,
            }),
        ),
        (
            "边界".into(),
            array_value(NumericArray {
                rows: edges.len(),
                cols: 1,
                data: edges,
            }),
        ),
    ])))
}
fn describe(values: &[f64]) -> BTreeMap<String, Value> {
    let mut v = values.to_vec();
    let minimum = v.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    BTreeMap::from([
        ("数量".into(), Value::Float(v.len() as f64)),
        ("平均值".into(), Value::Float(mean(&v))),
        ("标准差".into(), Value::Float(variance(&v, 0).sqrt())),
        ("最小值".into(), Value::Float(minimum)),
        ("四分位一".into(), Value::Float(quantile(&mut v, 0.25))),
        ("中位数".into(), Value::Float(quantile(&mut v, 0.5))),
        ("四分位三".into(), Value::Float(quantile(&mut v, 0.75))),
        ("最大值".into(), Value::Float(maximum)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    #[test]
    fn statistics() {
        let a = to_array(&Value::list(vec![
            Value::Float(1.0),
            Value::Float(2.0),
            Value::Float(3.0),
        ]))
        .unwrap();
        assert_eq!(
            array_method(&a, "平均值", vec![], BTreeMap::new())
                .unwrap()
                .to_string(),
            "2"
        )
    }
    #[test]
    fn robust_array_statistics_match_known_values() {
        let array = to_array(&Value::list(
            [1.0, 2.0, 3.0, 4.0].into_iter().map(Value::Float).collect(),
        ))
        .unwrap();
        for (method, expected) in [
            ("极差", 3.0),
            ("四分位距", 1.5),
            ("平均绝对偏差", 1.0),
            ("中位绝对偏差", 1.0),
        ] {
            let value = array_method(&array, method, Vec::new(), BTreeMap::new())
                .unwrap()
                .as_f64()
                .unwrap();
            assert!((value - expected).abs() < 1e-12, "{method}");
        }
        let moving = array_method(
            &array,
            "移动平均",
            vec![Value::Number(Decimal::from(2))],
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            moving
                .iter_values()
                .unwrap()
                .iter()
                .map(|value| value.as_f64().unwrap())
                .collect::<Vec<_>>(),
            vec![1.5, 2.5, 3.5]
        );
    }
    #[test]
    fn histogram_handles_constant_and_empty_inputs() {
        let constant = to_array(&Value::list(
            [2.0, 2.0, 2.0].into_iter().map(Value::Float).collect(),
        ))
        .unwrap();
        let counts = array_method(
            &constant,
            "直方统计",
            vec![Value::Number(Decimal::from(4))],
            BTreeMap::new(),
        )
        .unwrap();
        let Value::Record(record) = counts else {
            panic!()
        };
        let totals = record.borrow()["计数"]
            .iter_values()
            .unwrap()
            .iter()
            .map(|value| value.as_f64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(totals, vec![3.0, 0.0, 0.0, 0.0]);

        assert_eq!(histogram(&[], 4).unwrap_err(), "直方统计需要至少一个数值");
    }

    #[test]
    fn regression_known_line() {
        let y = Value::list(
            [3., 5., 7., 9., 11.]
                .into_iter()
                .map(Value::Float)
                .collect(),
        );
        let x = Value::list(
            [1., 2., 3., 4., 5.]
                .into_iter()
                .map(|v| Value::list(vec![Value::Float(v)]))
                .collect(),
        );
        let Value::Regression(r) = linear_regression(&y, &x).unwrap() else {
            panic!()
        };
        assert!((r.r_squared - 1.0).abs() < 1e-10);
        assert!((r.coefficients[1] - 2.0).abs() < 1e-10)
    }

    #[test]
    fn reads_real_parquet_data() {
        use arrow_array::{Float64Array, RecordBatch, StringArray};
        use arrow_schema::{DataType, Field, Schema};
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("销售.parquet");
        let schema = Arc::new(Schema::new(vec![
            Field::new("地区", DataType::Utf8, false),
            Field::new("销售额", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["华东", "华南", "华北"])),
                Arc::new(Float64Array::from(vec![1200.0, 800.0, 2000.0])),
            ],
        )
        .unwrap();
        let file = File::create(&path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let Value::List(chunks) = read_parquet(path.to_str().unwrap()).unwrap() else {
            panic!()
        };
        let Value::Table(table) = &chunks.borrow()[0] else {
            panic!()
        };
        assert_eq!(table.borrow().rows.len(), 3);
        assert_eq!(table.borrow().rows[0]["地区"].to_string(), "华东");

        let Value::Stream(stream) = open_parquet_stream(path.to_str().unwrap()).unwrap() else {
            panic!()
        };
        for expected in ["华东", "华南", "华北"] {
            let row = match &stream.borrow().source {
                StreamSource::Decoded(rows) => rows.borrow_mut().next_value().unwrap().unwrap(),
                _ => panic!(),
            };
            let Value::Record(row) = row else { panic!() };
            assert_eq!(row.borrow()["地区"].to_string(), expected);
        }
        let end = match &stream.borrow().source {
            StreamSource::Decoded(rows) => rows.borrow_mut().next_value().unwrap(),
            _ => panic!(),
        };
        assert!(end.is_none());
    }

    #[test]
    fn matrix_statistics_and_t_test() {
        let matrix = to_array(&Value::list(vec![
            Value::list(vec![Value::Float(2.0), Value::Float(1.0)]),
            Value::list(vec![Value::Float(1.0), Value::Float(2.0)]),
        ]))
        .unwrap();
        let vector = to_array(&Value::list(vec![Value::Float(5.0), Value::Float(4.0)])).unwrap();
        let solved = solve(&matrix, &vector).unwrap();
        assert!((solved.data[0] - 2.0).abs() < 1e-10);
        assert!((solved.data[1] - 1.0).abs() < 1e-10);
        let sample = Value::list([1., 2., 3., 4., 5.].into_iter().map(Value::Float).collect());
        let Value::Record(result) = one_sample_t(&sample, 3.0).unwrap() else {
            panic!()
        };
        assert!((result.borrow()["P值"].as_f64().unwrap() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn two_sample_t_rejects_zero_variance_samples() {
        let a = Value::list(vec![Value::Float(1.0), Value::Float(1.0)]);
        let b = Value::list(vec![Value::Float(2.0), Value::Float(2.0)]);
        assert!(two_sample_t(&a, &b).unwrap_err().contains("方差为零"));
    }

    #[test]
    fn degenerate_statistical_inputs_return_errors() {
        let constant = Value::list(vec![Value::Float(1.0); 5]);
        assert!(one_sample_t(&constant, 0.0)
            .unwrap_err()
            .contains("方差为零"));
        assert!(shapiro(&constant).unwrap_err().contains("方差为零"));

        let x = Value::list(
            [1., 2., 3., 4., 5.]
                .into_iter()
                .map(|value| Value::list(vec![Value::Float(value)]))
                .collect(),
        );
        assert!(linear_regression(&constant, &x)
            .unwrap_err()
            .contains("因变量方差为零"));

        assert!(
            one_way_anova(&Value::list(vec![constant.clone(), constant.clone()]))
                .unwrap_err()
                .contains("组内方差为零")
        );
        assert!(confidence_interval(
            &constant,
            &BTreeMap::from([("置信度".into(), Value::Float(1.0))]),
        )
        .unwrap_err()
        .contains("置信度"));
        assert!(analysis_native(
            &mut Interpreter::new(".", false),
            "正态分布分位点",
            vec![Value::Float(1.0)],
            BTreeMap::new(),
        )
        .unwrap_err()
        .contains("概率"));
        let array = to_array(&constant).unwrap();
        assert!(array_method(&array, "正态化", Vec::new(), BTreeMap::new())
            .unwrap_err()
            .contains("方差为零"));
        assert!(
            array_method(&array, "相关系数", vec![constant], BTreeMap::new())
                .unwrap_err()
                .contains("方差为零")
        );

        let logistic_y = Value::list(
            [0., 0., 0., 1., 1., 1.]
                .into_iter()
                .map(Value::Float)
                .collect(),
        );
        let logistic_x = Value::list(
            [0., 1., 2., 1., 2., 3.]
                .into_iter()
                .map(|value| Value::list(vec![Value::Float(value)]))
                .collect(),
        );
        let Value::Regression(result) = logistic_regression(&logistic_y, &logistic_x).unwrap()
        else {
            panic!("需要逻辑回归结果")
        };
        assert!(result.r_squared.is_finite());
        assert!(result.p_values.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn chunk_aggregate_is_streaming_and_parallel() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("数据.csv");
        std::fs::write(&path, "数值\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        let result = chunk_aggregate(path.to_str().unwrap(), "数值", "平均值", 3, 2).unwrap();
        assert!((result.as_f64().unwrap() - 5.5).abs() < 1e-12);
    }
}
