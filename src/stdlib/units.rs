use crate::units::{convert_with_rate, tax_convert, Quantity, Rounding, Unit};
use crate::value::Value;
use rust_decimal::Decimal;
use std::collections::BTreeMap;

pub(super) fn unit_call(name: &str, args: Vec<Value>) -> Result<Value, String> {
    match name {
        "创建" => {
            let value = args[0].as_decimal()?;
            let unit_name = args[1].to_string();
            let dimension = args[2].to_string();
            let scale = args[3].as_decimal()?;
            let basis = optional_basis(args.get(4));
            quantity_value(Quantity {
                value,
                unit: Unit::new(unit_name, dimension, scale, Decimal::ONE, basis)
                    .map_err(|error| error.to_string())?,
            })
        }
        "换算" => {
            let quantity = parse_quantity(&args[0])?;
            let target = Unit::new(
                args[1].to_string(),
                args[2].to_string(),
                args[3].as_decimal()?,
                Decimal::ONE,
                optional_basis(args.get(4)),
            )
            .map_err(|error| error.to_string())?;
            let rate = args.get(5).map(Value::as_decimal).transpose()?;
            let converted =
                convert_with_rate(&quantity, &target, rate).map_err(|error| error.to_string())?;
            quantity_value(converted)
        }
        "税转换" => {
            let quantity = parse_quantity(&args[0])?;
            let target = Unit::new(
                args[1].to_string(),
                args[2].to_string(),
                args[3].as_decimal()?,
                Decimal::ONE,
                optional_basis(args.get(4)),
            )
            .map_err(|error| error.to_string())?;
            let converted = tax_convert(&quantity, &target, Some(args[5].as_decimal()?))
                .map_err(|error| error.to_string())?;
            quantity_value(converted)
        }
        "舍入" => {
            let quantity = parse_quantity(&args[0])?;
            let precision = args[1]
                .as_decimal()?
                .to_u32()
                .ok_or("单位舍入精度必须是非负整数")?;
            let mode = match args.get(2).map(ToString::to_string).as_deref() {
                Some("银行家") | Some("HalfEven") => Rounding::HalfEven,
                Some("向下") | Some("Down") => Rounding::Down,
                Some("向上") | Some("Up") => Rounding::Up,
                _ => Rounding::HalfUp,
            };
            quantity_value(
                quantity
                    .round(precision, mode)
                    .map_err(|error| error.to_string())?,
            )
        }
        "值" => Ok(Value::Number(parse_quantity(&args[0])?.value)),
        "名称" => Ok(Value::Text(parse_quantity(&args[0])?.unit.name)),
        "维度" => Ok(Value::Text(parse_quantity(&args[0])?.unit.dimension)),
        _ => Err(format!("单位模块不支持“{name}”")),
    }
}

/// 缺省与 `空` 都表示“没有口径”，否则同维度无口径换算会被当成跨口径。
fn optional_basis(value: Option<&Value>) -> Option<String> {
    value
        .filter(|value| !matches!(value, Value::Null))
        .map(ToString::to_string)
}

fn parse_quantity(value: &Value) -> Result<Quantity, String> {
    let Value::Record(record) = value else {
        return Err("单位运算需要单位数量记录".into());
    };
    let record = record.borrow();
    let value = record.get("值").ok_or("单位数量缺少“值”")?.as_decimal()?;
    let name = record.get("单位").ok_or("单位数量缺少“单位”")?.to_string();
    let dimension = record.get("维度").ok_or("单位数量缺少“维度”")?.to_string();
    let scale = record
        .get("比例")
        .ok_or("单位数量缺少“比例”")?
        .as_decimal()?;
    let basis = record
        .get("口径")
        .filter(|value| !matches!(value, Value::Null))
        .map(ToString::to_string);
    Ok(Quantity {
        value,
        unit: Unit::new(name, dimension, scale, Decimal::ONE, basis)
            .map_err(|error| error.to_string())?,
    })
}

fn quantity_value(quantity: Quantity) -> Result<Value, String> {
    Ok(Value::record(BTreeMap::from([
        ("值".into(), Value::Number(quantity.value)),
        ("单位".into(), Value::Text(quantity.unit.name)),
        ("维度".into(), Value::Text(quantity.unit.dimension)),
        ("比例".into(), Value::Number(quantity.unit.scale_numerator)),
        (
            "口径".into(),
            quantity.unit.basis.map_or(Value::Null, Value::Text),
        ),
    ])))
}

use rust_decimal::prelude::ToPrimitive;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_module_creates_converts_and_rounds_quantities() {
        let quantity = unit_call(
            "创建",
            vec![
                Value::Number(Decimal::from(2)),
                Value::Text("万元".into()),
                Value::Text("货币".into()),
                Value::Number(Decimal::from(10_000)),
                Value::Text("CNY".into()),
            ],
        )
        .unwrap();
        let converted = unit_call(
            "换算",
            vec![
                quantity,
                Value::Text("元".into()),
                Value::Text("货币".into()),
                Value::Number(Decimal::ONE),
                Value::Text("CNY".into()),
                Value::Number(Decimal::ONE),
            ],
        )
        .unwrap();
        assert_eq!(
            unit_call("值", vec![converted]).unwrap().to_string(),
            "20000"
        );
    }

    #[test]
    fn unit_module_rejects_incompatible_dimensions() {
        let quantity = unit_call(
            "创建",
            vec![
                Value::Number(Decimal::ONE),
                Value::Text("元".into()),
                Value::Text("货币".into()),
                Value::Number(Decimal::ONE),
            ],
        )
        .unwrap();
        assert!(unit_call(
            "换算",
            vec![
                quantity,
                Value::Text("人".into()),
                Value::Text("人数".into()),
                Value::Number(Decimal::ONE),
            ],
        )
        .is_err());
    }

    #[test]
    fn unit_module_is_available_through_language_imports() {
        let result = crate::runtime::execute(
            "引入 单位\n定义 金额 为 单位 创建(2, \"万元\", \"货币\", 10000, \"CNY\")\n定义 元金额 为 单位 换算(金额, \"元\", \"货币\", 1, \"CNY\", 1)\n输出(单位 值(元金额))\n",
            ".",
            false,
        )
        .unwrap();
        assert_eq!(result.output, ["20000"]);
    }
}
