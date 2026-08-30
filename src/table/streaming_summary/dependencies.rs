use std::collections::HashSet;

use super::aggregate::{MetricOperation, MetricSpec};
use super::language_error;

fn referenced_metrics(metric: &MetricSpec) -> Option<[&str; 2]> {
    match &metric.operation {
        MetricOperation::Difference { left, right } => Some([left.as_str(), right.as_str()]),
        MetricOperation::Ratio {
            numerator,
            denominator,
            ..
        } => Some([numerator.as_str(), denominator.as_str()]),
        _ => None,
    }
}

/// 计算指标只能引用它前面已经定义的指标，因此声明顺序本身就排除了循环引用。
pub(super) fn validate_backward_references(
    operation: &MetricOperation,
    previous: &[MetricSpec],
) -> Result<(), String> {
    let references = match operation {
        MetricOperation::Difference { left, right } => [left, right],
        MetricOperation::Ratio {
            numerator,
            denominator,
            ..
        } => [numerator, denominator],
        _ => {
            return Err(language_error("内部错误：计算指标类型与解析结果不一致"));
        }
    };
    for reference in references {
        if !previous.iter().any(|metric| metric.name == *reference) {
            return Err(language_error(format!(
                "计算指标引用了尚未定义的指标“{reference}”"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_selected_metric_dependencies(
    plan_name: &str,
    selected: &[usize],
    metrics: &[MetricSpec],
) -> Result<(), String> {
    let names = selected
        .iter()
        .map(|index| metrics[*index].name.as_str())
        .collect::<HashSet<_>>();
    for index in selected {
        let metric = &metrics[*index];
        if let Some(references) = referenced_metrics(metric) {
            for reference in references {
                if !names.contains(reference) {
                    return Err(language_error(format!(
                        "分组方案“{plan_name}”的指标“{}”还需要指标“{reference}”",
                        metric.name
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn sum(name: &str) -> MetricSpec {
        MetricSpec {
            name: name.to_string(),
            operation: MetricOperation::Sum {
                column: name.to_string(),
                index: 0,
                dedup: None,
            },
        }
    }

    fn difference(name: &str, left: &str, right: &str) -> MetricSpec {
        MetricSpec {
            name: name.to_string(),
            operation: MetricOperation::Difference {
                left: left.to_string(),
                right: right.to_string(),
            },
        }
    }

    fn ratio(name: &str, numerator: &str, denominator: &str) -> MetricSpec {
        MetricSpec {
            name: name.to_string(),
            operation: MetricOperation::Ratio {
                numerator: numerator.to_string(),
                denominator: denominator.to_string(),
                percent: false,
            },
        }
    }

    #[test]
    fn backward_references_must_already_be_defined() {
        let previous = vec![sum("销售额"), sum("成本")];
        let good = difference("毛利", "销售额", "成本");
        assert!(validate_backward_references(&good.operation, &previous).is_ok());

        let forward = ratio("比率", "尚未定义", "销售额");
        assert!(validate_backward_references(&forward.operation, &previous)
            .unwrap_err()
            .contains("尚未定义"));
    }

    #[test]
    fn aggregate_operations_are_not_calculation_metrics() {
        let error = validate_backward_references(&MetricOperation::Count, &[]).unwrap_err();
        assert!(error.contains("内部错误"), "{error}");
    }

    #[test]
    fn selecting_a_derived_metric_requires_selecting_its_inputs() {
        let metrics = vec![
            sum("销售额"),
            sum("成本"),
            difference("毛利", "销售额", "成本"),
        ];
        assert!(validate_selected_metric_dependencies("全部", &[0, 1, 2], &metrics).is_ok());
        let error = validate_selected_metric_dependencies("坏方案", &[2], &metrics).unwrap_err();
        assert!(error.contains("还需要指标“销售额”"), "{error}");
    }

    #[test]
    fn plans_that_omit_derived_metrics_stay_valid() {
        let metrics = vec![
            sum("销售额"),
            difference("毛利", "销售额", "成本"),
            MetricSpec {
                name: "时差".to_string(),
                operation: MetricOperation::DurationAverage {
                    columns: ["开始".into(), "结束".into()],
                    indices: [0, 1],
                    divisor: Decimal::from(3_600_000),
                    dedup: None,
                },
            },
        ];
        assert!(validate_selected_metric_dependencies("仅聚合", &[0, 2], &metrics).is_ok());
    }
}
