use super::*;

/// 浮点数相等：按位精确判断，只把所有`NaN`视为同一类值、`0.0`与`-0.0`视为相等，
/// 因此相等是自反、对称且可传递的，排序、去重和集合成员判断可以复用同一关系。
pub fn float_equal(x: f64, y: f64) -> bool {
    x == y || (x.is_nan() && y.is_nan())
}

/// 与`float_equal`一致的全序：相等的值给出`Equal`，其余按IEEE全序排列（`NaN`排在末端）。
pub fn float_order(x: f64, y: f64) -> Ordering {
    if float_equal(x, y) {
        Ordering::Equal
    } else {
        x.total_cmp(&y)
    }
}

/// 金额顺序：同币种按数额比较，跨币种没有语言级顺序。
pub fn money_order(x: &MoneyValue, y: &MoneyValue) -> Option<Ordering> {
    (x.currency == y.currency).then(|| x.amount.cmp(&y.amount))
}

/// `数字`与`浮点数`是彼此独立的类型，没有隐式转换：算术、大小比较和相等判断
/// 遇到这一对操作数时用同一条诊断拒绝，不允许一处报“不能比较”另一处静默返回“不相等”。
pub fn mixed_numeric_message(a: &Value, b: &Value) -> Option<&'static str> {
    matches!(
        (a, b),
        (Value::Number(_), Value::Float(_)) | (Value::Float(_), Value::Number(_))
    )
    .then_some("数字和浮点数不能参与同一次运算或比较，请先用“转数字”把浮点数转换为数字。")
}

pub fn values_equal(a: &Value, b: &Value) -> bool {
    values_equal_inner(a, b, &mut HashSet::new(), 0)
}

fn values_equal_inner(
    a: &Value,
    b: &Value,
    seen: &mut HashSet<(usize, usize, u8)>,
    depth: usize,
) -> bool {
    if depth >= MAX_VALUE_NESTING {
        return false;
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => float_equal(*x, *y),
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::Bytes(x), Value::Bytes(y)) => *x.borrow() == *y.borrow(),
        (Value::Path(x), Value::Path(y)) => x == y,
        (Value::DateTime(x), Value::DateTime(y)) => x == y,
        (Value::Duration(x), Value::Duration(y)) => x == y,
        (Value::Money(x), Value::Money(y)) => x.currency == y.currency && x.amount == y.amount,
        (Value::List(x), Value::List(y)) => sequence_equal(x, y, seen, 1, depth + 1),
        (Value::Set(x), Value::Set(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let key = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize, 2);
            if !seen.insert(key) {
                return true;
            }
            let x = x.borrow();
            let y = y.borrow();
            x.len() == y.len()
                && x.iter().all(|left| {
                    y.iter()
                        .any(|right| values_equal_inner(left, right, seen, depth + 1))
                })
        }
        (Value::Record(x), Value::Record(y)) => map_equal(x, y, seen, 3, depth + 1),
        (Value::StructInstance(x), Value::StructInstance(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let key = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize, 4);
            if !seen.insert(key) {
                return true;
            }
            let x = x.borrow();
            let y = y.borrow();
            Rc::ptr_eq(&x.struct_type, &y.struct_type)
                && x.fields.len() == y.fields.len()
                && x.fields.iter().all(|(name, left)| {
                    y.fields
                        .get(name)
                        .is_some_and(|right| values_equal_inner(left, right, seen, depth + 1))
                })
        }
        (Value::Table(x), Value::Table(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let x = x.borrow();
            let y = y.borrow();
            x.rows.len() == y.rows.len()
                && x.rows.iter().zip(&y.rows).all(|(left, right)| {
                    left.len() == right.len()
                        && left.iter().all(|(name, value)| {
                            right.get(name).is_some_and(|other| {
                                values_equal_inner(value, other, seen, depth + 1)
                            })
                        })
                })
        }
        (Value::Range(x), Value::Range(y)) => {
            x.start == y.start && x.end == y.end && x.inclusive == y.inclusive && x.step == y.step
        }
        _ => false,
    }
}

fn sequence_equal(
    x: &Rc<RefCell<Vec<Value>>>,
    y: &Rc<RefCell<Vec<Value>>>,
    seen: &mut HashSet<(usize, usize, u8)>,
    kind: u8,
    depth: usize,
) -> bool {
    if Rc::ptr_eq(x, y) {
        return true;
    }
    let key = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize, kind);
    if !seen.insert(key) {
        return true;
    }
    let x = x.borrow();
    let y = y.borrow();
    x.len() == y.len()
        && x.iter()
            .zip(y.iter())
            .all(|(left, right)| values_equal_inner(left, right, seen, depth))
}

fn map_equal(
    x: &Rc<RefCell<BTreeMap<String, Value>>>,
    y: &Rc<RefCell<BTreeMap<String, Value>>>,
    seen: &mut HashSet<(usize, usize, u8)>,
    kind: u8,
    depth: usize,
) -> bool {
    if Rc::ptr_eq(x, y) {
        return true;
    }
    let key = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize, kind);
    if !seen.insert(key) {
        return true;
    }
    let x = x.borrow();
    let y = y.borrow();
    x.len() == y.len()
        && x.iter().all(|(name, left)| {
            y.get(name)
                .is_some_and(|right| values_equal_inner(left, right, seen, depth))
        })
}

pub fn value_matches_type(value: &Value, expected: &str) -> bool {
    if expected == "任意" {
        return true;
    }
    match value {
        Value::Float(_) if expected == "浮点数" => true,
        Value::StructInstance(_) if expected == "结构实例" => true,
        // Concrete structure identity is resolved through `TypeConstraint::Struct`.
        // This fallback only handles built-in names and must not compare user names.
        Value::StructInstance(_) => false,
        _ => value.type_name() == expected,
    }
}

pub fn ensure_acyclic_insertion(target: &Value, candidate: &Value) -> Result<(), String> {
    let Some(target) = container_key(target) else {
        return Ok(());
    };
    if value_reaches_container(candidate, target, &mut HashSet::new(), 0)? {
        Err("容器不能直接或间接包含自身".into())
    } else {
        Ok(())
    }
}

fn value_reaches_container(
    value: &Value,
    target: ContainerKey,
    active: &mut HashSet<ContainerKey>,
    depth: usize,
) -> Result<bool, String> {
    if depth >= MAX_VALUE_NESTING {
        return Err("值的嵌套层级超过限制".into());
    }
    let Some(key) = container_key(value) else {
        return match value {
            Value::Outcome(value) => match value.as_ref() {
                OutcomeValue::Success(value) => {
                    value_reaches_container(value, target, active, depth + 1)
                }
                OutcomeValue::Failure(_) => Ok(false),
            },
            _ => Ok(false),
        };
    };
    if key == target {
        return Ok(true);
    }
    if !active.insert(key) {
        return Err("值中存在循环引用".into());
    }
    let reaches = match value {
        Value::List(values) | Value::Set(values) => {
            let values = values.borrow().clone();
            values.iter().try_fold(false, |found, value| {
                if found {
                    Ok(true)
                } else {
                    value_reaches_container(value, target, active, depth + 1)
                }
            })?
        }
        Value::Record(values) => {
            let values = values.borrow().values().cloned().collect::<Vec<_>>();
            values.iter().try_fold(false, |found, value| {
                if found {
                    Ok(true)
                } else {
                    value_reaches_container(value, target, active, depth + 1)
                }
            })?
        }
        Value::StructInstance(value) => {
            let values = value.borrow().fields.values().cloned().collect::<Vec<_>>();
            values.iter().try_fold(false, |found, value| {
                if found {
                    Ok(true)
                } else {
                    value_reaches_container(value, target, active, depth + 1)
                }
            })?
        }
        Value::Table(value) => {
            let rows = value.borrow().rows.clone();
            rows.iter().try_fold(false, |found, row| {
                if found {
                    Ok(true)
                } else {
                    row.values().try_fold(false, |found, value| {
                        if found {
                            Ok(true)
                        } else {
                            value_reaches_container(value, target, active, depth + 1)
                        }
                    })
                }
            })?
        }
        _ => false,
    };
    active.remove(&key);
    Ok(reaches)
}

pub(super) fn container_key(value: &Value) -> Option<ContainerKey> {
    match value {
        Value::List(value) => Some((Rc::as_ptr(value) as usize, 1)),
        Value::Record(value) => Some((Rc::as_ptr(value) as usize, 2)),
        Value::StructInstance(value) => Some((Rc::as_ptr(value) as usize, 3)),
        Value::Set(value) => Some((Rc::as_ptr(value) as usize, 4)),
        Value::Table(value) => Some((Rc::as_ptr(value) as usize, 5)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::PortableCodec;

    fn portable_codec() -> PortableCodec {
        PortableCodec::new(
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
            crate::resource::DEFAULT_MAX_VALUE_NESTING,
        )
    }

    #[test]
    fn cyclic_values_are_rejected_or_rendered_without_recursion() {
        let direct = Value::list(Vec::new());
        let Value::List(direct_values) = &direct else {
            unreachable!()
        };
        direct_values.borrow_mut().push(direct.clone());
        assert!(direct.to_string().contains("循环引用"));
        assert!(portable_codec()
            .encode(&direct)
            .unwrap_err()
            .contains("循环引用"));
        direct_values.borrow_mut().clear();

        let list = Value::list(Vec::new());
        let record = Value::record(BTreeMap::from([("列表".into(), list.clone())]));
        let Value::List(values) = &list else {
            unreachable!()
        };
        values.borrow_mut().push(record.clone());
        assert!(record.to_string().contains("循环引用"));
        assert!(portable_codec()
            .encode(&record)
            .unwrap_err()
            .contains("循环引用"));
        values.borrow_mut().clear();

        let target = Value::list(Vec::new());
        let candidate = Value::record(BTreeMap::from([("目标".into(), target.clone())]));
        assert!(ensure_acyclic_insertion(&target, &candidate)
            .unwrap_err()
            .contains("包含自身"));
    }

    /// 深层相等比较必须在嵌套预算处返回结果：递归无预算时同一对值会让宿主栈溢出。
    /// 断言用超过预算的深度，否则预算本身形同虚设也不会被发现。
    #[test]
    fn deep_equality_stops_at_the_nesting_budget_instead_of_overflowing() {
        fn nest(depth: usize) -> Value {
            let mut value = Value::Number(Decimal::ONE);
            for _ in 0..depth {
                value = Value::list(vec![value]);
            }
            value
        }

        let within = MAX_VALUE_NESTING - 2;
        assert!(values_equal(&nest(within), &nest(within)));

        let beyond = MAX_VALUE_NESTING * 4;
        assert!(!values_equal(&nest(beyond), &nest(beyond)));
    }

    #[test]
    fn float_and_money_relations_are_consistent_equivalences() {
        assert!(float_equal(f64::NAN, f64::NAN));
        assert!(float_equal(0.0, -0.0));
        assert!(float_equal(f64::INFINITY, f64::INFINITY));
        assert!(!float_equal(1.0, 1.0 + f64::EPSILON));
        for &(x, y) in &[
            (f64::NAN, f64::NAN),
            (0.0, -0.0),
            (1.5, 1.5),
            (1.5, 2.5),
            (f64::NAN, 1.5),
        ] {
            assert_eq!(float_equal(x, y), float_order(x, y) == Ordering::Equal);
            assert_eq!(float_order(x, y), float_order(y, x).reverse());
            assert_eq!(
                float_equal(x, y),
                values_equal(&Value::Float(x), &Value::Float(y))
            );
        }
        // 传递性：`1e-13`级差值以前被当作相等，使排序与去重给出互相矛盾的答案。
        let near = [1.0, 1.0 + 1e-13, 1.0 + 2e-13];
        assert!(!float_equal(near[0], near[1]));
        assert!(!float_equal(near[1], near[2]));
        assert!(!float_equal(near[0], near[2]));

        let cny = MoneyValue {
            amount: Decimal::from(9),
            currency: "CNY".into(),
            scale: 2,
            rounding: "银行家".into(),
            audit: Vec::new(),
        };
        let bigger = MoneyValue {
            amount: Decimal::from(10),
            ..cny.clone()
        };
        let usd = MoneyValue {
            currency: "USD".into(),
            ..cny.clone()
        };
        assert_eq!(money_order(&cny, &bigger), Some(Ordering::Less));
        assert_eq!(money_order(&cny, &cny), Some(Ordering::Equal));
        assert_eq!(money_order(&cny, &usd), None);

        let float = Value::Float(1.5);
        let number = Value::Number(Decimal::new(15, 1));
        assert!(mixed_numeric_message(&float, &number).is_some());
        assert_eq!(
            mixed_numeric_message(&float, &number),
            mixed_numeric_message(&number, &float)
        );
        assert!(mixed_numeric_message(&float, &float).is_none());
        assert!(!values_equal(&float, &number));
    }
}
