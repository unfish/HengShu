use super::{demand_values, error, named_f64, optional_float, preprocess_outliers};
use crate::analysis::statistics::{mean, variance};
use crate::value::Value;
use statrs::distribution::{ContinuousCDF, Normal};
use std::collections::BTreeMap;

pub(super) struct InventoryResult {
    average_demand: f64,
    demand_standard_deviation: f64,
    service_factor: f64,
    lead_time_demand: f64,
    pub(super) safety_stock: f64,
    pub(super) lower: f64,
    pub(super) upper: f64,
    pub(super) suggested_replenishment: Option<f64>,
}

pub(in crate::analysis) fn safety_stock(
    value: &Value,
    named: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let values = demand_values(value, true)?;
    let (cleaned, _) = preprocess_outliers(&values, named)?;
    let predicted = named
        .get("预测需求")
        .map(|value| demand_values(value, true))
        .transpose()?;
    let result = inventory_from(&cleaned, predicted.as_deref(), named)?;
    Ok(Value::record(inventory_record(&result)))
}

pub(super) fn inventory_from(
    history: &[f64],
    predicted: Option<&[f64]>,
    named: &BTreeMap<String, Value>,
) -> Result<InventoryResult, String> {
    if history.is_empty() {
        return Err(error("安全库存至少需要一个历史需求周期"));
    }
    let service_level = named_f64(named, "服务水平", 0.95)?;
    if !(0.5..1.0).contains(&service_level) {
        return Err(error("服务水平必须大于等于 0.5 且小于 1"));
    }
    let lead_time = named_f64(named, "提前期", 2.0)?;
    let review_period = named_f64(named, "审查周期", 1.0)?;
    let lead_time_deviation = named_f64(named, "提前期标准差", 0.0)?;
    let has_inventory_position = named.contains_key("当前库存") || named.contains_key("在途库存");
    let current_inventory = named_f64(named, "当前库存", 0.0)?;
    let in_transit = named_f64(named, "在途库存", 0.0)?;
    if lead_time <= 0.0
        || review_period < 0.0
        || lead_time_deviation < 0.0
        || current_inventory < 0.0
        || in_transit < 0.0
    {
        return Err(error("提前期必须大于零，其余库存参数必须是非负数"));
    }
    let average_demand = predicted
        .filter(|values| !values.is_empty())
        .map(mean)
        .unwrap_or_else(|| mean(history));
    let demand_standard_deviation = if history.len() > 1 {
        variance(history, 1).sqrt()
    } else {
        0.0
    };
    let service_factor = Normal::new(0.0, 1.0)
        .map_err(|value| error(value.to_string()))?
        .inverse_cdf(service_level);
    let demand_variance = lead_time * demand_standard_deviation.powi(2)
        + average_demand.powi(2) * lead_time_deviation.powi(2);
    let safety_stock = service_factor * demand_variance.sqrt();
    let lead_time_demand = average_demand * lead_time;
    let lower = (lead_time_demand + safety_stock).max(0.0);
    let protection_period = lead_time + review_period;
    let upper_variance = protection_period * demand_standard_deviation.powi(2)
        + average_demand.powi(2) * lead_time_deviation.powi(2);
    let upper =
        (average_demand * protection_period + service_factor * upper_variance.sqrt()).max(lower);
    let suggested_replenishment =
        has_inventory_position.then_some((upper - current_inventory - in_transit).max(0.0));
    if [
        average_demand,
        demand_standard_deviation,
        service_factor,
        lead_time_demand,
        safety_stock,
        lower,
        upper,
    ]
    .into_iter()
    .any(|value| !value.is_finite())
        || suggested_replenishment.is_some_and(|value| !value.is_finite())
    {
        return Err(error("安全库存计算产生了非有限结果"));
    }
    Ok(InventoryResult {
        average_demand,
        demand_standard_deviation,
        service_factor,
        lead_time_demand,
        safety_stock,
        lower,
        upper,
        suggested_replenishment,
    })
}

pub(super) fn inventory_record(value: &InventoryResult) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "预测平均周期需求".into(),
            Value::Float(value.average_demand),
        ),
        (
            "历史需求标准差".into(),
            Value::Float(value.demand_standard_deviation),
        ),
        ("服务系数".into(), Value::Float(value.service_factor)),
        ("提前期需求".into(), Value::Float(value.lead_time_demand)),
        ("安全库存".into(), Value::Float(value.safety_stock)),
        ("补货点".into(), Value::Float(value.lower)),
        ("库存下限".into(), Value::Float(value.lower)),
        ("库存上限".into(), Value::Float(value.upper)),
        (
            "建议补货量".into(),
            optional_float(value.suggested_replenishment),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::super::parse_week;
    use super::*;

    fn values(items: &[f64]) -> Value {
        Value::list(items.iter().copied().map(Value::Float).collect())
    }

    #[test]
    fn safety_stock_accepts_single_period_and_normalizes_week_dates() {
        let inventory = safety_stock(&values(&[8.0]), &BTreeMap::new()).unwrap();
        let Value::Record(inventory) = inventory else {
            panic!()
        };
        assert_eq!(inventory.borrow()["历史需求标准差"].to_string(), "0");
        assert!(matches!(inventory.borrow()["建议补货量"], Value::Null));
        assert_eq!(
            parse_week("2026-01-07", "周").unwrap().to_string(),
            "2026-01-05"
        );
    }
}
