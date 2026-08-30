use super::equality::container_key;
use super::limits::ensure_length;
use super::{ContainerKey, MoneyValue, Value, MAX_VALUE_NESTING};
use crate::execution::Metadata;
use chrono::{DateTime, Duration, FixedOffset};
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum PortableValue {
    Null,
    Bool(bool),
    Number(Decimal),
    Float(f64),
    Text(String),
    List(Vec<PortableValue>),
    Set(Vec<PortableValue>),
    Record(BTreeMap<String, PortableValue>),
    Bytes(Vec<u8>),
    Path(PathBuf),
    DateTime(DateTime<FixedOffset>),
    Duration(Duration),
    Money(MoneyValue),
    Quantity(crate::units::Quantity),
    Table {
        rows: Vec<BTreeMap<String, PortableValue>>,
        metadata: Metadata,
        column_metadata: BTreeMap<String, Metadata>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct PortableCodec {
    collection_limit: usize,
    nesting_limit: usize,
}

impl PortableCodec {
    pub fn new(collection_limit: usize, nesting_limit: usize) -> Self {
        Self {
            collection_limit,
            nesting_limit: nesting_limit.clamp(1, MAX_VALUE_NESTING),
        }
    }

    pub fn encode(&self, value: &Value) -> Result<PortableValue, String> {
        to_portable_inner(value, &mut HashSet::new(), 0, *self)
    }

    pub fn decode(&self, value: PortableValue) -> Result<Value, String> {
        from_portable_inner(value, 0, *self)
    }
}

fn to_portable_inner(
    value: &Value,
    active: &mut HashSet<ContainerKey>,
    depth: usize,
    codec: PortableCodec,
) -> Result<PortableValue, String> {
    if depth >= codec.nesting_limit {
        return Err("跨任务值的嵌套层级超过限制".into());
    }
    let key = container_key(value);
    if key.is_some_and(|key| !active.insert(key)) {
        return Err("含循环引用的值不能跨任务传递".into());
    }
    let result = Ok(match value {
        Value::Null => PortableValue::Null,
        Value::Bool(value) => PortableValue::Bool(*value),
        Value::Number(value) => PortableValue::Number(*value),
        Value::Float(value) => PortableValue::Float(*value),
        Value::Text(value) => PortableValue::Text(value.clone()),
        Value::List(values) => {
            let values = values.borrow();
            ensure_length(values.len(), codec.collection_limit)?;
            PortableValue::List(
                values
                    .iter()
                    .map(|value| to_portable_inner(value, active, depth + 1, codec))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        Value::Set(values) => {
            let values = values.borrow();
            ensure_length(values.len(), codec.collection_limit)?;
            PortableValue::Set(
                values
                    .iter()
                    .map(|value| to_portable_inner(value, active, depth + 1, codec))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        Value::Record(values) if quantity_record(values).is_some() => {
            PortableValue::Quantity(quantity_record(values).expect("guarded quantity record"))
        }
        Value::Record(values) => {
            let values = values.borrow();
            ensure_length(values.len(), codec.collection_limit)?;
            PortableValue::Record(
                values
                    .iter()
                    .map(|(key, value)| {
                        Ok((
                            key.clone(),
                            to_portable_inner(value, active, depth + 1, codec)?,
                        ))
                    })
                    .collect::<Result<_, String>>()?,
            )
        }
        Value::StructInstance(_) => return Err("结构实例暂不支持跨任务传递，已拒绝启动任务".into()),
        Value::Bytes(value) => {
            let value = value.borrow();
            ensure_length(value.len(), codec.collection_limit)?;
            PortableValue::Bytes(value.clone())
        }
        Value::Path(value) => PortableValue::Path(value.as_ref().clone()),
        Value::DateTime(value) => PortableValue::DateTime(*value),
        Value::Duration(value) => PortableValue::Duration(*value),
        Value::Money(value) => PortableValue::Money(value.as_ref().clone()),
        Value::Table(table) => {
            let table = table.borrow();
            ensure_length(table.rows.len(), codec.collection_limit)?;
            PortableValue::Table {
                rows: table
                    .rows
                    .iter()
                    .map(|row| {
                        ensure_length(row.len(), codec.collection_limit)?;
                        row.iter()
                            .map(|(key, value)| {
                                Ok((
                                    key.clone(),
                                    to_portable_inner(value, active, depth + 1, codec)?,
                                ))
                            })
                            .collect::<Result<_, String>>()
                    })
                    .collect::<Result<_, String>>()?,
                metadata: table.metadata.clone(),
                column_metadata: table.column_metadata.clone(),
            }
        }
        _ => return Err(format!("{}不能跨任务传递", value.type_name())),
    });
    if let Some(key) = key {
        active.remove(&key);
    }
    result
}

fn from_portable_inner(
    value: PortableValue,
    depth: usize,
    codec: PortableCodec,
) -> Result<Value, String> {
    if depth >= codec.nesting_limit {
        return Err("跨任务值的嵌套层级超过限制".into());
    }
    match value {
        PortableValue::Null => Ok(Value::Null),
        PortableValue::Bool(value) => Ok(Value::Bool(value)),
        PortableValue::Number(value) => Ok(Value::Number(value)),
        PortableValue::Float(value) => Ok(Value::Float(value)),
        PortableValue::Text(value) => Ok(Value::Text(value)),
        PortableValue::List(values) => {
            ensure_length(values.len(), codec.collection_limit)?;
            Ok(Value::list(
                values
                    .into_iter()
                    .map(|value| from_portable_inner(value, depth + 1, codec))
                    .collect::<Result<_, _>>()?,
            ))
        }
        PortableValue::Set(values) => {
            ensure_length(values.len(), codec.collection_limit)?;
            Ok(Value::Set(Rc::new(RefCell::new(
                values
                    .into_iter()
                    .map(|value| from_portable_inner(value, depth + 1, codec))
                    .collect::<Result<_, _>>()?,
            ))))
        }
        PortableValue::Record(values) => {
            ensure_length(values.len(), codec.collection_limit)?;
            Ok(Value::record(
                values
                    .into_iter()
                    .map(|(key, value)| Ok((key, from_portable_inner(value, depth + 1, codec)?)))
                    .collect::<Result<_, String>>()?,
            ))
        }
        PortableValue::Bytes(value) => {
            ensure_length(value.len(), codec.collection_limit)?;
            Ok(Value::Bytes(Rc::new(RefCell::new(value))))
        }
        PortableValue::Path(value) => Ok(Value::Path(Rc::new(value))),
        PortableValue::DateTime(value) => Ok(Value::DateTime(value)),
        PortableValue::Duration(value) => Ok(Value::Duration(value)),
        PortableValue::Money(value) => Ok(Value::Money(Rc::new(value))),
        PortableValue::Quantity(quantity) => Ok(quantity_value(quantity)),
        PortableValue::Table {
            rows,
            metadata,
            column_metadata,
        } => {
            ensure_length(rows.len(), codec.collection_limit)?;
            let rows = rows
                .into_iter()
                .map(|row| {
                    ensure_length(row.len(), codec.collection_limit)?;
                    row.into_iter()
                        .map(|(key, value)| {
                            Ok((key, from_portable_inner(value, depth + 1, codec)?))
                        })
                        .collect::<Result<_, String>>()
                })
                .collect::<Result<_, String>>()?;
            Ok(Value::Table(Rc::new(RefCell::new(super::Table {
                rows,
                metadata,
                column_metadata,
            }))))
        }
    }
}

fn quantity_record(
    values: &Rc<RefCell<BTreeMap<String, Value>>>,
) -> Option<crate::units::Quantity> {
    let values = values.borrow();
    if values.len() != 5 {
        return None;
    }
    let value = values.get("值")?.as_decimal().ok()?;
    let name = match values.get("单位")? {
        Value::Text(value) => value.clone(),
        _ => return None,
    };
    let dimension = match values.get("维度")? {
        Value::Text(value) => value.clone(),
        _ => return None,
    };
    let scale = values.get("比例")?.as_decimal().ok()?;
    let basis = match values.get("口径")? {
        Value::Null => None,
        Value::Text(value) => Some(value.clone()),
        _ => return None,
    };
    Some(crate::units::Quantity {
        value,
        unit: crate::units::Unit::new(name, dimension, scale, Decimal::ONE, basis).ok()?,
    })
}

fn quantity_value(quantity: crate::units::Quantity) -> Value {
    Value::record(BTreeMap::from([
        ("值".into(), Value::Number(quantity.value)),
        ("单位".into(), Value::Text(quantity.unit.name)),
        ("维度".into(), Value::Text(quantity.unit.dimension)),
        (
            "比例".into(),
            Value::Number(quantity.unit.scale_numerator / quantity.unit.scale_denominator),
        ),
        (
            "口径".into(),
            quantity.unit.basis.map_or(Value::Null, Value::Text),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::super::{StructInstance, StructType, MAX_VALUE_NESTING};
    use super::*;
    use crate::ast::TypeId;

    fn portable_codec() -> PortableCodec {
        PortableCodec::new(
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
            crate::resource::DEFAULT_MAX_VALUE_NESTING,
        )
    }

    #[test]
    fn deeply_nested_portable_values_fail_with_a_depth_error() {
        let mut value = Value::Null;
        for _ in 0..=MAX_VALUE_NESTING {
            value = Value::list(vec![value]);
        }
        assert!(portable_codec()
            .encode(&value)
            .unwrap_err()
            .contains("嵌套层级"));
    }

    #[test]
    fn portable_sets_keep_set_semantics_and_structs_are_rejected() {
        let set = Value::Set(Rc::new(RefCell::new(vec![Value::Number(1.into())])));
        let portable = portable_codec().encode(&set).unwrap();
        assert!(matches!(portable, PortableValue::Set(_)));
        assert!(matches!(
            portable_codec().decode(portable),
            Ok(Value::Set(_))
        ));

        let structure = Value::StructInstance(Rc::new(RefCell::new(StructInstance {
            struct_type: Rc::new(StructType {
                id: TypeId {
                    module: 1,
                    local: 1,
                },
                name: "客户".into(),
                fields: vec!["姓名".into()],
                methods: RefCell::new(BTreeMap::new()),
                method_names: RefCell::new(BTreeMap::new()),
                module_id: 1,
            }),
            fields: BTreeMap::from([("姓名".into(), Value::Text("甲".into()))]),
        })));
        assert!(portable_codec()
            .encode(&structure)
            .unwrap_err()
            .contains("结构实例暂不支持跨任务传递"));
    }

    #[test]
    fn portable_codec_enforces_collection_and_depth_budgets_both_ways() {
        let collection_codec = PortableCodec::new(1, 8);
        let list = Value::list(vec![Value::Null, Value::Null]);
        assert!(collection_codec
            .encode(&list)
            .unwrap_err()
            .contains("集合项数"));
        assert!(collection_codec
            .decode(PortableValue::List(vec![
                PortableValue::Null,
                PortableValue::Null,
            ]))
            .unwrap_err()
            .contains("集合项数"));

        let depth_codec = PortableCodec::new(8, 2);
        let nested = Value::list(vec![Value::list(vec![Value::Null])]);
        assert!(depth_codec
            .encode(&nested)
            .unwrap_err()
            .contains("嵌套层级"));
        assert!(depth_codec
            .decode(PortableValue::List(vec![PortableValue::List(vec![
                PortableValue::Null,
            ])]))
            .unwrap_err()
            .contains("嵌套层级"));
    }

    #[test]
    fn portable_quantity_preserves_unit_identity() {
        let quantity = Value::record(BTreeMap::from([
            ("值".into(), Value::Number(Decimal::from(2))),
            ("单位".into(), Value::Text("万元".into())),
            ("维度".into(), Value::Text("货币".into())),
            ("比例".into(), Value::Number(Decimal::from(10_000))),
            ("口径".into(), Value::Text("CNY".into())),
        ]));
        let portable = portable_codec().encode(&quantity).unwrap();
        assert!(matches!(portable, PortableValue::Quantity(_)));
        let restored = portable_codec().decode(portable).unwrap();
        assert_eq!(restored.to_string(), quantity.to_string());
    }
}
