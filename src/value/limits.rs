use super::equality::container_key;
use super::*;

pub fn ensure_collection_limit(value: &Value, limit: usize) -> Result<(), String> {
    ensure_collection_limit_with_depth(value, limit, crate::resource::DEFAULT_MAX_VALUE_NESTING)
}

pub fn ensure_collection_limit_with_depth(
    value: &Value,
    limit: usize,
    max_depth: usize,
) -> Result<(), String> {
    ensure_collection_limit_inner(value, limit, max_depth.max(1), &mut HashSet::new(), 0)
}

fn ensure_collection_limit_inner(
    value: &Value,
    limit: usize,
    max_depth: usize,
    active: &mut HashSet<ContainerKey>,
    depth: usize,
) -> Result<(), String> {
    if depth >= max_depth {
        return Err("值的嵌套层级超过限制".into());
    }
    let key = container_key(value);
    if key.is_some_and(|key| !active.insert(key)) {
        return Err("值中存在循环引用".into());
    }
    let result = match value {
        Value::List(values) | Value::Set(values) => {
            let values = values.borrow();
            ensure_length(values.len(), limit)?;
            values.iter().try_for_each(|value| {
                ensure_collection_limit_inner(value, limit, max_depth, active, depth + 1)
            })
        }
        Value::Record(values) => {
            let values = values.borrow();
            ensure_length(values.len(), limit)?;
            values.values().try_for_each(|value| {
                ensure_collection_limit_inner(value, limit, max_depth, active, depth + 1)
            })
        }
        Value::StructInstance(value) => {
            let value = value.borrow();
            ensure_length(value.fields.len(), limit)?;
            value.fields.values().try_for_each(|value| {
                ensure_collection_limit_inner(value, limit, max_depth, active, depth + 1)
            })
        }
        Value::Table(value) => {
            let value = value.borrow();
            ensure_length(value.rows.len(), limit)?;
            value.rows.iter().try_for_each(|row| {
                ensure_length(row.len(), limit)?;
                row.values().try_for_each(|value| {
                    ensure_collection_limit_inner(value, limit, max_depth, active, depth + 1)
                })
            })
        }
        Value::NumericArray(value) => ensure_length(value.data.len(), limit),
        Value::Bytes(value) => ensure_length(value.borrow().len(), limit),
        Value::Text(_) => Ok(()),
        Value::Range(_) => value.iter_values_limited(limit).map(|_| ()),
        Value::Outcome(value) => match value.as_ref() {
            OutcomeValue::Success(value) => {
                ensure_collection_limit_inner(value, limit, max_depth, active, depth + 1)
            }
            OutcomeValue::Failure(_) => Ok(()),
        },
        _ => Ok(()),
    };
    if let Some(key) = key {
        active.remove(&key);
    }
    result
}

pub(super) fn ensure_length(length: usize, limit: usize) -> Result<(), String> {
    if length > limit {
        Err("集合项数超过资源限制".into())
    } else {
        Ok(())
    }
}
