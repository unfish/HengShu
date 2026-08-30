//! Explicit units, business bases, money conversion and rounding primitives.

use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Unit {
    pub name: String,
    pub dimension: String,
    pub scale_numerator: Decimal,
    pub scale_denominator: Decimal,
    pub basis: Option<String>,
}

impl Unit {
    pub fn new(
        name: impl Into<String>,
        dimension: impl Into<String>,
        numerator: Decimal,
        denominator: Decimal,
        basis: Option<String>,
    ) -> Result<Self, UnitError> {
        if denominator.is_zero() || numerator.is_zero() {
            return Err(UnitError::InvalidScale);
        }
        Ok(Self {
            name: name.into(),
            dimension: dimension.into(),
            scale_numerator: numerator,
            scale_denominator: denominator,
            basis,
        })
    }
    pub fn compatible(&self, other: &Self) -> bool {
        self.dimension == other.dimension && self.basis == other.basis
    }
    fn scale(&self) -> Decimal {
        self.scale_numerator / self.scale_denominator
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Quantity {
    pub value: Decimal,
    pub unit: Unit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Rounding {
    HalfUp,
    HalfEven,
    Down,
    Up,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnitError {
    InvalidScale,
    Incompatible { from: String, to: String },
    MissingRate,
    InvalidRate,
    InvalidPrecision,
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidScale => formatter.write_str("单位比例必须是非零且分母不能为零"),
            Self::Incompatible { from, to } => write!(formatter, "单位/口径不兼容：{from} -> {to}"),
            Self::MissingRate => formatter.write_str("缺少显式税率或汇率"),
            Self::InvalidRate => formatter.write_str("税率或汇率必须是有限正数"),
            Self::InvalidPrecision => formatter.write_str("精度必须在 0 到 28 之间"),
        }
    }
}
impl std::error::Error for UnitError {}

impl Quantity {
    pub fn convert_to(&self, target: &Unit) -> Result<Self, UnitError> {
        if !self.unit.compatible(target) {
            return Err(UnitError::Incompatible {
                from: self.unit.name.clone(),
                to: target.name.clone(),
            });
        }
        Ok(Self {
            value: self.value * self.unit.scale() / target.scale(),
            unit: target.clone(),
        })
    }
    pub fn round(&self, precision: u32, mode: Rounding) -> Result<Self, UnitError> {
        if precision > 28 {
            return Err(UnitError::InvalidPrecision);
        }
        let strategy = match mode {
            Rounding::HalfUp => RoundingStrategy::MidpointAwayFromZero,
            Rounding::HalfEven => RoundingStrategy::MidpointNearestEven,
            Rounding::Down => RoundingStrategy::ToZero,
            Rounding::Up => RoundingStrategy::AwayFromZero,
        };
        Ok(Self {
            value: self.value.round_dp_with_strategy(precision, strategy),
            unit: self.unit.clone(),
        })
    }
}

pub fn convert_with_rate(
    quantity: &Quantity,
    target: &Unit,
    rate: Option<Decimal>,
) -> Result<Quantity, UnitError> {
    let rate = rate.ok_or(UnitError::MissingRate)?;
    if rate <= Decimal::ZERO {
        return Err(UnitError::InvalidRate);
    }
    if quantity.unit.dimension != target.dimension
        || quantity.unit.basis.is_some() == target.basis.is_some()
            && quantity.unit.basis == target.basis
    {
        return quantity.convert_to(target);
    }
    Ok(Quantity {
        value: quantity.value * rate,
        unit: target.clone(),
    })
}

/// 显式换算并登记专用单位转换来源节点，`basis` 记录税率/汇率等依据。
/// 图预算超限时节点为 `None`，来源图自身置截断标记，换算结果不受影响。
pub fn convert_with_provenance(
    quantity: &Quantity,
    target: &Unit,
    rate: Option<Decimal>,
    basis: &str,
    metadata: &crate::execution::Metadata,
    graph: &mut crate::provenance::ProvenanceGraph,
    budget: crate::provenance::GraphBudget,
) -> Result<(Quantity, Option<String>), UnitError> {
    // 同口径同币种只按比例换算；跨口径或跨币种必须给出汇率/税率依据。
    let converted = match rate {
        Some(_) => convert_with_rate(quantity, target, rate)?,
        None => quantity.convert_to(target)?,
    };
    let node = graph
        .add_unit_conversion(
            &quantity.unit.name,
            &target.name,
            basis,
            metadata
                .provenance
                .iter()
                .map(|reference| reference.source_id.clone()),
            metadata.clone(),
            budget,
        )
        .ok();
    Ok((converted, node))
}

pub fn tax_convert(
    amount: &Quantity,
    target: &Unit,
    tax_rate: Option<Decimal>,
) -> Result<Quantity, UnitError> {
    let rate = tax_rate.ok_or(UnitError::MissingRate)?;
    if rate <= Decimal::ZERO {
        return Err(UnitError::InvalidRate);
    }
    if amount.unit.dimension != target.dimension
        || amount.unit.basis.is_none()
        || target.basis.is_none()
    {
        return Err(UnitError::Incompatible {
            from: amount.unit.name.clone(),
            to: target.name.clone(),
        });
    }
    let multiplier = if amount.unit.basis.as_deref() == target.basis.as_deref() {
        Decimal::ONE
    } else {
        Decimal::ONE + rate
    };
    Ok(Quantity {
        value: amount.value * multiplier,
        unit: target.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yuan() -> Unit {
        Unit::new("元", "货币", Decimal::ONE, Decimal::ONE, Some("CNY".into())).unwrap()
    }
    fn wan() -> Unit {
        Unit::new(
            "万元",
            "货币",
            Decimal::from(10_000),
            Decimal::ONE,
            Some("CNY".into()),
        )
        .unwrap()
    }

    #[test]
    fn quantities_require_explicit_compatible_units_and_rounding() {
        let quantity = Quantity {
            value: Decimal::from(2),
            unit: wan(),
        };
        assert_eq!(
            quantity.convert_to(&yuan()).unwrap().value,
            Decimal::from(20_000)
        );
        assert_eq!(
            quantity.round(0, Rounding::HalfUp).unwrap().value,
            Decimal::from(2)
        );
        let people = Unit::new("人数", "人数", Decimal::ONE, Decimal::ONE, None).unwrap();
        assert!(quantity.convert_to(&people).is_err());
    }

    #[test]
    fn tax_and_currency_conversions_require_explicit_positive_rates() {
        let before = Unit::new(
            "不含税元",
            "货币",
            Decimal::ONE,
            Decimal::ONE,
            Some("CNY:未税".into()),
        )
        .unwrap();
        let after = Unit::new(
            "含税元",
            "货币",
            Decimal::ONE,
            Decimal::ONE,
            Some("CNY:含税".into()),
        )
        .unwrap();
        let amount = Quantity {
            value: Decimal::from(100),
            unit: before,
        };
        assert_eq!(
            tax_convert(&amount, &after, Some(Decimal::new(13, 2)))
                .unwrap()
                .value,
            Decimal::from(113)
        );
        assert_eq!(
            convert_with_rate(&amount, &after, Some(Decimal::new(2, 0)))
                .unwrap()
                .value,
            Decimal::from(200)
        );
        assert!(tax_convert(&amount, &after, None).is_err());
        assert!(tax_convert(&amount, &after, Some(Decimal::ZERO)).is_err());
        assert!(convert_with_rate(&amount, &after, Some(Decimal::ZERO)).is_err());
    }
}
