use super::error;
use crate::security::PermissionSet;
use crate::value::Value;
use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Serialize)]
pub(super) struct FieldRule {
    selector: String,
    attribute: Option<String>,
    html: bool,
    reject_private_use: bool,
}

pub(super) fn select_values(
    html: &Value,
    selector: &Value,
    attribute: Option<String>,
    inner_html: bool,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    let document = Html::parse_document(&html.to_string());
    let selector = parse_selector(&selector.to_string())?;
    let mut values = Vec::new();
    for element in document.select(&selector) {
        if values.len() >= permissions.collection_limit() {
            return Err(error("网页选择结果超过集合项数限制"));
        }
        let value = if let Some(attribute) = &attribute {
            element
                .value()
                .attr(attribute)
                .unwrap_or_default()
                .to_string()
        } else if inner_html {
            element.inner_html()
        } else {
            element_text(element)
        };
        values.push(Value::Text(value));
    }
    Ok(Value::list(values))
}

pub(super) fn extract_records(
    html: &str,
    container_selector: &str,
    fields: &Value,
    collection_limit: usize,
) -> Result<Value, String> {
    let rules = parse_field_rules(fields)?;
    let compiled = compile_field_rules(&rules)?;
    let document = Html::parse_document(html);
    let container = parse_selector(container_selector)?;
    let mut records = Vec::new();
    for element in document.select(&container) {
        if records.len() >= collection_limit {
            return Err(error("网页采集结果超过集合项数限制"));
        }
        let mut record = BTreeMap::new();
        for (name, rule, selector) in &compiled {
            let selected = selector
                .as_ref()
                .and_then(|selector| element.select(selector).next());
            let selected = if rule.selector.is_empty() {
                Some(element)
            } else {
                selected
            };
            let value = selected.map_or_else(String::new, |selected| {
                if let Some(attribute) = &rule.attribute {
                    selected
                        .value()
                        .attr(attribute)
                        .unwrap_or_default()
                        .to_string()
                } else if rule.html {
                    selected.inner_html()
                } else {
                    element_text(selected)
                }
            });
            let value = if rule.reject_private_use && contains_private_use(&value) {
                String::new()
            } else {
                value
            };
            record.insert(name.clone(), Value::Text(value));
        }
        records.push(Value::record(record));
    }
    Ok(Value::list(records))
}

pub(super) fn parse_field_rules(fields: &Value) -> Result<BTreeMap<String, FieldRule>, String> {
    let Value::Record(fields) = fields else {
        return Err(error("网页字段配置必须是记录"));
    };
    fields
        .borrow()
        .iter()
        .map(|(name, value)| {
            let rule = match value {
                Value::Text(selector) => FieldRule {
                    selector: selector.clone(),
                    attribute: None,
                    html: false,
                    reject_private_use: false,
                },
                Value::Record(config) => {
                    let config = config.borrow();
                    FieldRule {
                        selector: config
                            .get("选择器")
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        attribute: config.get("属性").map(ToString::to_string),
                        html: config.get("HTML").is_some_and(Value::truth),
                        reject_private_use: config.get("拒绝私用字符").is_some_and(Value::truth),
                    }
                }
                _ => {
                    return Err(error(format!("网页字段“{name}”应为选择器文本或配置记录")));
                }
            };
            if rule.attribute.is_some() && rule.html {
                return Err(error(format!("网页字段“{name}”不能同时读取属性和 HTML")));
            }
            Ok((name.clone(), rule))
        })
        .collect()
}

type CompiledField = (String, FieldRule, Option<Selector>);

pub(super) fn compile_field_rules(
    rules: &BTreeMap<String, FieldRule>,
) -> Result<Vec<CompiledField>, String> {
    rules
        .iter()
        .map(|(name, rule)| {
            let selector = if rule.selector.is_empty() {
                None
            } else {
                Some(parse_selector(&rule.selector)?)
            };
            Ok((name.clone(), rule.clone(), selector))
        })
        .collect()
}

pub(super) fn parse_selector(selector: &str) -> Result<Selector, String> {
    Selector::parse(selector).map_err(|_| error(format!("CSS 选择器“{selector}”无效")))
}

fn element_text(element: ElementRef<'_>) -> String {
    element.text().collect::<String>().trim().to_string()
}

pub(super) fn contains_private_use(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character as u32,
            0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
        )
    })
}
