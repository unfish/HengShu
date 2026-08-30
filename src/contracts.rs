use crate::value::Value;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

pub(crate) type Arity = (usize, Option<usize>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallShapeError {
    pub message: String,
    pub parameter: Option<String>,
}

impl CallShapeError {
    fn new(message: String, parameter: Option<&str>) -> Self {
        Self {
            message,
            parameter: parameter.map(str::to_owned),
        }
    }
}

pub(crate) fn validate_call_shape<'a>(
    function: &str,
    parameters: impl IntoIterator<Item = (&'a str, bool)>,
    positional: usize,
    named: impl IntoIterator<Item = &'a str>,
    _line: usize,
) -> Result<(), CallShapeError> {
    let parameters = parameters.into_iter().collect::<Vec<_>>();
    if positional > parameters.len() {
        return Err(CallShapeError::new(
            format!("功能“{function}”参数过多。"),
            None,
        ));
    }
    let named = named.into_iter().collect::<Vec<_>>();
    if let Some(extra) = named
        .iter()
        .find(|name| !parameters.iter().any(|(parameter, _)| parameter == *name))
    {
        return Err(CallShapeError::new(
            format!("功能“{function}”没有参数“{extra}”。"),
            None,
        ));
    }
    if let Some(duplicate) = named.iter().find(|name| {
        parameters
            .iter()
            .take(positional)
            .any(|(parameter, _)| parameter == *name)
    }) {
        return Err(CallShapeError::new(
            format!("参数“{duplicate}”同时由位置实参和具名实参提供。"),
            Some(duplicate),
        ));
    }
    if let Some((missing, _)) =
        parameters
            .iter()
            .skip(positional)
            .find(|(parameter, has_default)| {
                !*has_default && !named.iter().any(|name| name == parameter)
            })
    {
        return Err(CallShapeError::new(
            format!("功能“{function}”缺少参数“{missing}”。"),
            Some(missing),
        ));
    }
    Ok(())
}

pub(crate) fn validate_arity(
    name: &str,
    actual: usize,
    expected: Option<Arity>,
) -> Result<(), String> {
    let Some((minimum, maximum)) = expected else {
        return Ok(());
    };
    if actual < minimum {
        return Err(format!(
            "功能“{name}”至少需要 {minimum} 个位置参数，实际收到 {actual} 个。"
        ));
    }
    if let Some(maximum) = maximum {
        if actual > maximum {
            return Err(format!(
                "功能“{name}”最多接受 {maximum} 个位置参数，实际收到 {actual} 个。"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_named_args(
    name: &str,
    named: &BTreeMap<String, Value>,
    allowed: &[&str],
) -> Result<(), String> {
    if let Some(extra) = named
        .keys()
        .find(|argument| !allowed.contains(&argument.as_str()))
    {
        Err(format!("功能“{name}”没有命名参数“{extra}”。"))
    } else {
        Ok(())
    }
}

fn integer_decimal(value: Decimal, name: &str) -> Result<Decimal, String> {
    if !value.fract().is_zero() {
        return Err(format!("{name}必须是整数。"));
    }
    Ok(value)
}

pub(crate) fn decimal_to_usize(value: Decimal, name: &str) -> Result<usize, String> {
    integer_decimal(value, name)?
        .to_usize()
        .ok_or_else(|| format!("{name}超出支持范围。"))
}

pub(crate) fn decimal_to_one_based_index(value: Decimal) -> Result<usize, String> {
    let index = decimal_to_usize(value, "索引")?;
    index
        .checked_sub(1)
        .ok_or_else(|| "索引必须从 1 开始，不能使用 0。".to_string())
}

pub(crate) fn decimal_to_u64(value: Decimal, name: &str) -> Result<u64, String> {
    integer_decimal(value, name)?
        .to_u64()
        .ok_or_else(|| format!("{name}超出支持范围。"))
}

pub(crate) fn decimal_to_u32(value: Decimal, name: &str) -> Result<u32, String> {
    integer_decimal(value, name)?
        .to_u32()
        .ok_or_else(|| format!("{name}超出支持范围。"))
}

pub(crate) fn decimal_to_i64(value: Decimal, name: &str) -> Result<i64, String> {
    integer_decimal(value, name)?
        .to_i64()
        .ok_or_else(|| format!("{name}超出支持范围。"))
}

pub(crate) fn decimal_to_u8(value: Decimal, name: &str) -> Result<u8, String> {
    integer_decimal(value, name)?
        .to_u8()
        .ok_or_else(|| format!("{name}必须在 0 到 255 之间。"))
}

pub(crate) fn parse_duration_milliseconds(text: &str, name: &str) -> Result<i64, String> {
    parse_duration_milliseconds_message(text, name).map_err(|message| message.to_string())
}

pub(crate) fn parse_duration_milliseconds_message(text: &str, name: &str) -> Result<i64, String> {
    let compact = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let (number, multiplier) = [
        ("毫秒", Decimal::ONE),
        ("秒", Decimal::from(1_000)),
        ("分钟", Decimal::from(60_000)),
        ("小时", Decimal::from(3_600_000)),
        ("天", Decimal::from(86_400_000)),
    ]
    .into_iter()
    .find_map(|(unit, multiplier)| compact.strip_suffix(unit).map(|value| (value, multiplier)))
    .ok_or_else(|| format!("{name}必须写成带单位的时间，例如“500毫秒”、“1.2秒”或“3分钟”。"))?;
    if number.is_empty() {
        return Err(format!("{name}缺少数值。"));
    }
    let value = Decimal::from_str(number).map_err(|_| format!("{name}的数值无效。"))?;
    let milliseconds = value
        .checked_mul(multiplier)
        .ok_or_else(|| format!("{name}超出支持范围。"))?;
    if !milliseconds.fract().is_zero() {
        return Err(format!("{name}（精确到毫秒）必须是整数。"));
    }
    milliseconds
        .to_i64()
        .ok_or_else(|| format!("{name}（精确到毫秒）超出支持范围。"))
}

pub(crate) fn duration_milliseconds(value: &Value, name: &str) -> Result<i64, String> {
    match value {
        Value::Duration(value) => Ok(value.num_milliseconds()),
        Value::Text(value) => parse_duration_milliseconds(value, name),
        Value::Number(value) => decimal_to_i64(*value, &format!("{name}毫秒数")),
        _ => Err(format!("{name}必须是持续时间，例如 持续时间(\"30秒\")。")),
    }
}

pub(crate) fn timeout_duration(value: &Value, name: &str) -> Result<Duration, String> {
    let milliseconds = duration_milliseconds(value, name)?;
    let milliseconds = u64::try_from(milliseconds).map_err(|_| format!("{name}不能是负数。"))?;
    Ok(Duration::from_millis(milliseconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arity_contracts_cover_minimum_maximum_and_variadic_calls() {
        assert!(validate_arity("功能", 2, Some((1, Some(2)))).is_ok());
        assert!(validate_arity("功能", 3, Some((1, None))).is_ok());
        assert!(validate_arity("功能", 0, Some((1, Some(2))))
            .unwrap_err()
            .contains("至少需要 1"));
        assert!(validate_arity("功能", 3, Some((1, Some(2))))
            .unwrap_err()
            .contains("最多接受 2"));
    }

    #[test]
    fn named_contracts_reject_unknown_keys() {
        let named = BTreeMap::from([("拼错".into(), Value::Null)]);
        let error = validate_named_args("功能", &named, &["正确"]).unwrap_err();
        assert!(error.contains("拼错"));
        assert!(validate_named_args("功能", &BTreeMap::new(), &["正确"]).is_ok());
    }

    #[test]
    fn integer_conversions_reject_fractional_negative_and_oversized_values() {
        assert_eq!(decimal_to_usize(Decimal::from(3), "数量").unwrap(), 3);
        assert!(decimal_to_usize(Decimal::new(15, 1), "数量")
            .unwrap_err()
            .contains("必须是整数"));
        assert!(decimal_to_usize(Decimal::NEGATIVE_ONE, "数量").is_err());
        assert!(decimal_to_u8(Decimal::from(256), "字节").is_err());
    }

    #[test]
    fn duration_text_supports_natural_chinese_units() {
        assert_eq!(
            parse_duration_milliseconds("500毫秒", "等待时间").unwrap(),
            500
        );
        assert_eq!(
            parse_duration_milliseconds("1.2 秒", "等待时间").unwrap(),
            1_200
        );
        assert_eq!(
            parse_duration_milliseconds("3分钟", "等待时间").unwrap(),
            180_000
        );
        assert_eq!(
            parse_duration_milliseconds("1小时", "等待时间").unwrap(),
            3_600_000
        );
        assert_eq!(
            parse_duration_milliseconds("1天", "等待时间").unwrap(),
            86_400_000
        );
        assert!(parse_duration_milliseconds("30", "等待时间")
            .unwrap_err()
            .contains("带单位"));
        assert!(parse_duration_milliseconds("0.0001秒", "等待时间")
            .unwrap_err()
            .contains("精确到毫秒"));
    }

    #[test]
    fn timeout_values_accept_duration_text_and_reject_negative_values() {
        assert_eq!(
            timeout_duration(&Value::Text("2.5秒".into()), "最长等待").unwrap(),
            Duration::from_millis(2_500)
        );
        assert!(timeout_duration(&Value::Text("-1秒".into()), "最长等待").is_err());
    }
}
