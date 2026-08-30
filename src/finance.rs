use crate::value::{AuditEntry, MoneyValue, Table, Value};
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use rust_decimal::{Decimal, RoundingStrategy};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub fn module_functions(name: &str) -> Option<&'static [&'static str]> {
    (name == "财务").then_some(&[
        "金额",
        "税额",
        "换算",
        "会计期间",
        "是否工作日",
        "月末",
        "敏感性分析",
    ])
}

pub fn call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    collection_limit: usize,
) -> Result<Value, String> {
    crate::contracts::validate_arity(name, args.len(), finance_arity(name))?;
    match name {
        "金额" => {
            let amount = args[0].as_decimal()?;
            let currency = args
                .get(1)
                .or_else(|| named.get("币种"))
                .map(ToString::to_string)
                .unwrap_or_else(|| "CNY".into());
            let scale = named
                .get("小数位")
                .map(Value::as_decimal)
                .transpose()?
                .and_then(|value| value.try_into().ok())
                .unwrap_or(2);
            let rounding = named
                .get("舍入")
                .map(ToString::to_string)
                .unwrap_or_else(|| "银行家".into());
            Ok(money(
                round(amount, scale, &rounding),
                currency,
                scale,
                rounding,
                vec![AuditEntry::new("创建金额")],
            ))
        }
        "税额" => {
            let rate = args[1].as_decimal()?;
            match &args[0] {
                Value::Money(value) => Ok(money(
                    round(
                        checked(value.amount.checked_mul(rate))?,
                        value.scale,
                        &value.rounding,
                    ),
                    value.currency.clone(),
                    value.scale,
                    value.rounding.clone(),
                    appended_audit(&value.audit, format!("按税率 {rate} 计算税额")),
                )),
                value => Ok(Value::Number(checked(
                    value.as_decimal()?.checked_mul(rate),
                )?)),
            }
        }
        "换算" => {
            let Value::Money(value) = &args[0] else {
                return Err(error("货币换算需要金额值"));
            };
            let rate = args[1].as_decimal()?;
            let currency = args[2].to_string();
            let audit = appended_audit(
                &value.audit,
                format!("{} 按汇率 {} 换算为 {}", value.currency, rate, currency),
            );
            Ok(money(
                round(
                    checked(value.amount.checked_mul(rate))?,
                    value.scale,
                    &value.rounding,
                ),
                currency,
                value.scale,
                value.rounding.clone(),
                audit,
            ))
        }
        "会计期间" => {
            let date = parse_date(&args[0].to_string())?;
            let start_month = named
                .get("年度开始月")
                .map(Value::as_decimal)
                .transpose()?
                .and_then(|value| value.try_into().ok())
                .unwrap_or(1_u32);
            if !(1..=12).contains(&start_month) {
                return Err(error("年度开始月必须在 1 到 12 之间"));
            }
            let fiscal_year = if date.month() >= start_month {
                date.year()
            } else {
                date.year() - 1
            };
            let period = (date.month() + 12 - start_month) % 12 + 1;
            Ok(Value::record(BTreeMap::from([
                ("财年".into(), Value::Number(Decimal::from(fiscal_year))),
                ("期间".into(), Value::Number(Decimal::from(period))),
            ])))
        }
        "是否工作日" => {
            let date = parse_date(&args[0].to_string())?;
            let holidays = args
                .get(1)
                .map(Value::iter_values)
                .transpose()?
                .unwrap_or_default()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            Ok(Value::Bool(
                !matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
                    && !holidays.contains(&date.to_string()),
            ))
        }
        "月末" => {
            let date = parse_date(&args[0].to_string())?;
            let (year, month) = if date.month() == 12 {
                (
                    date.year()
                        .checked_add(1)
                        .ok_or_else(|| error("日期超出支持范围"))?,
                    1,
                )
            } else {
                (date.year(), date.month() + 1)
            };
            let first_next =
                NaiveDate::from_ymd_opt(year, month, 1).ok_or_else(|| error("日期超出支持范围"))?;
            Ok(Value::Text((first_next - Duration::days(1)).to_string()))
        }
        "敏感性分析" => {
            let base = args[0].as_decimal()?;
            let rates = args[1].iter_values_limited(collection_limit)?;
            let rows = rates
                .into_iter()
                .map(|rate| {
                    let rate = rate.as_decimal()?;
                    Ok(BTreeMap::from([
                        ("变化率".into(), Value::Number(rate)),
                        (
                            "结果".into(),
                            Value::Number(checked(
                                base.checked_mul(checked(Decimal::ONE.checked_add(rate))?),
                            )?),
                        ),
                    ]))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Value::Table(Rc::new(RefCell::new(Table {
                rows,
                ..Default::default()
            }))))
        }
        _ => Err(error(format!("财务模块不支持“{name}”"))),
    }
}

pub fn attr(value: &Value, name: &str) -> Option<Value> {
    let Value::Money(value) = value else {
        return None;
    };
    Some(match name {
        "数额" => Value::Number(value.amount),
        "币种" => Value::Text(value.currency.clone()),
        "小数位" => Value::Number(Decimal::from(value.scale)),
        "舍入方式" => Value::Text(value.rounding.clone()),
        "审计记录" => Value::list(
            value
                .audit
                .iter()
                .map(|entry| Value::Text(entry.to_string()))
                .collect(),
        ),
        _ => return None,
    })
}

pub fn method(value: &Value, name: &str, args: Vec<Value>) -> Option<Result<Value, String>> {
    let Value::Money(value) = value else {
        return None;
    };
    Some(match name {
        "格式化" => Ok(Value::Text(format!(
            "{} {}",
            value.currency,
            value.amount.round_dp(value.scale)
        ))),
        "按精度舍入" => {
            let scale = args[0]
                .as_decimal()
                .ok()
                .and_then(|value| value.try_into().ok())
                .unwrap_or(value.scale);
            Ok(money(
                round(value.amount, scale, &value.rounding),
                value.currency.clone(),
                scale,
                value.rounding.clone(),
                value.audit.clone(),
            ))
        }
        _ => Err(error(format!("金额不支持动作“{name}”"))),
    })
}

pub fn add(left: &MoneyValue, right: &MoneyValue) -> Result<Value, String> {
    if left.currency != right.currency {
        return Err(error("不同币种金额不能直接相加"));
    }
    Ok(money(
        round(
            checked(left.amount.checked_add(right.amount))?,
            left.scale.max(right.scale),
            &left.rounding,
        ),
        left.currency.clone(),
        left.scale.max(right.scale),
        left.rounding.clone(),
        merged_audit(&left.audit, &right.audit, "金额相加"),
    ))
}

pub fn subtract(left: &MoneyValue, right: &MoneyValue) -> Result<Value, String> {
    if left.currency != right.currency {
        return Err(error("不同币种金额不能直接相减"));
    }
    Ok(money(
        round(
            checked(left.amount.checked_sub(right.amount))?,
            left.scale.max(right.scale),
            &left.rounding,
        ),
        left.currency.clone(),
        left.scale.max(right.scale),
        left.rounding.clone(),
        merged_audit(&left.audit, &right.audit, "金额相减"),
    ))
}

pub fn scale(value: &MoneyValue, factor: Decimal, divide: bool) -> Result<Value, String> {
    if divide && factor.is_zero() {
        return Err(error("金额不能除以零"));
    }
    let amount = if divide {
        checked(value.amount.checked_div(factor))?
    } else {
        checked(value.amount.checked_mul(factor))?
    };
    let audit = appended_audit(
        &value.audit,
        format!("金额{} {}", if divide { "除以" } else { "乘以" }, factor),
    );
    Ok(money(
        round(amount, value.scale, &value.rounding),
        value.currency.clone(),
        value.scale,
        value.rounding.clone(),
        audit,
    ))
}

const OMITTED_AUDIT: &str = "更早的审计记录已省略";

/// 审计链是折叠且有上限的表示：同一动作只累加次数，条数达到上限时把最早的记录
/// 汇总成一条省略说明。因此循环内反复累加金额的内存与克隆成本保持常量级。
fn push_audit(audit: &mut Vec<AuditEntry>, action: String, count: usize) {
    if let Some(last) = audit.last_mut() {
        if last.action == action {
            last.count = last.count.saturating_add(count);
            return;
        }
    }
    let limit = crate::resource::DEFAULT_MAX_MONEY_AUDIT_ENTRIES;
    if audit.len() >= limit {
        let mut omitted = 0_usize;
        while audit.len() + 2 > limit {
            omitted = omitted.saturating_add(audit.remove(0).count);
        }
        audit.insert(
            0,
            AuditEntry {
                action: OMITTED_AUDIT.into(),
                count: omitted,
            },
        );
    }
    audit.push(AuditEntry { action, count });
}

fn appended_audit(base: &[AuditEntry], action: String) -> Vec<AuditEntry> {
    let mut audit = base.to_vec();
    push_audit(&mut audit, action, 1);
    audit
}

fn merged_audit(left: &[AuditEntry], right: &[AuditEntry], action: &str) -> Vec<AuditEntry> {
    let mut audit = left.to_vec();
    for entry in right {
        push_audit(&mut audit, entry.action.clone(), entry.count);
    }
    push_audit(&mut audit, action.into(), 1);
    audit
}

fn money(
    amount: Decimal,
    currency: String,
    scale: u32,
    rounding: String,
    audit: Vec<AuditEntry>,
) -> Value {
    Value::Money(Rc::new(MoneyValue {
        amount,
        currency,
        scale,
        rounding,
        audit,
    }))
}

fn round(value: Decimal, scale: u32, strategy: &str) -> Decimal {
    let strategy = match strategy {
        "远离零" => RoundingStrategy::MidpointAwayFromZero,
        "趋向零" => RoundingStrategy::ToZero,
        "向上" => RoundingStrategy::ToPositiveInfinity,
        "向下" => RoundingStrategy::ToNegativeInfinity,
        _ => RoundingStrategy::MidpointNearestEven,
    };
    value.round_dp_with_strategy(scale, strategy)
}

fn parse_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|e| error(e.to_string()))
}

fn error(message: impl AsRef<str>) -> String {
    message.as_ref().to_string()
}

fn checked(value: Option<Decimal>) -> Result<Decimal, String> {
    value.ok_or_else(|| error("财务数字运算溢出"))
}

fn finance_arity(name: &str) -> Option<(usize, Option<usize>)> {
    Some(match name {
        "金额" => (1, Some(2)),
        "税额" => (2, Some(2)),
        "换算" => (3, Some(3)),
        "会计期间" | "月末" => (1, Some(1)),
        "是否工作日" => (1, Some(2)),
        "敏感性分析" => (2, Some(2)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_rounding_tax_and_currency_rules() {
        let amount = call(
            "金额",
            vec![
                Value::Number(Decimal::new(10005, 3)),
                Value::Text("CNY".into()),
            ],
            BTreeMap::new(),
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        assert_eq!(amount.to_string(), "CNY 10.00");
        let tax = call(
            "税额",
            vec![amount.clone(), Value::Number(Decimal::new(13, 2))],
            BTreeMap::new(),
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        assert_eq!(tax.to_string(), "CNY 1.30");
        let usd = call(
            "金额",
            vec![Value::Number(Decimal::ONE), Value::Text("USD".into())],
            BTreeMap::new(),
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        let (Value::Money(cny), Value::Money(usd)) = (amount, usd) else {
            panic!("需要金额")
        };
        assert!(add(&cny, &usd).is_err());
    }

    #[test]
    fn money_audit_chains_fold_repeats_and_stay_within_the_budget() {
        let limit = crate::resource::DEFAULT_MAX_MONEY_AUDIT_ENTRIES;
        let one = MoneyValue {
            amount: Decimal::ONE,
            currency: "CNY".into(),
            scale: 2,
            rounding: "银行家".into(),
            audit: vec![AuditEntry::new("创建金额")],
        };
        let mut total = Rc::new(one.clone());
        for _ in 0..2_000 {
            let Value::Money(sum) = add(&total, &one).unwrap() else {
                panic!("金额相加应返回金额")
            };
            total = sum;
        }
        assert_eq!(total.amount, Decimal::from(2_001));
        assert!(total.audit.len() <= limit, "{}", total.audit.len());
        assert_eq!(total.audit[0].action, OMITTED_AUDIT);
        assert!(total.audit[0].count > 0);
        assert!(total.audit.iter().any(|entry| entry.count > 1));

        let Some(Value::List(records)) = attr(&Value::Money(total), "审计记录") else {
            panic!("金额应提供审计记录")
        };
        assert!(records.borrow().len() <= limit);
        assert!(records
            .borrow()
            .iter()
            .any(|record| record.to_string().contains("次）")));
    }
}
