use super::{Interpreter, RuntimeError};
use crate::ast::*;
use crate::contracts::decimal_to_i64;
use crate::value::*;
use rust_decimal::{Decimal, MathematicalOps};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;

impl Interpreter {
    pub(super) fn eval(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, RuntimeError> {
        self.eval_inner(expr, env).map_err(|error| {
            if error.span.start.line == 0 {
                error.with_fallback_line(expr.line())
            } else {
                error
            }
        })
    }

    fn eval_inner(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, RuntimeError> {
        match expr {
            Expr::Literal(v, _) => Ok(match v {
                Literal::Number(v) => Value::Number(*v),
                Literal::Text(v) => Value::Text(v.clone()),
                Literal::Bool(v) => Value::Bool(*v),
                Literal::Null => Value::Null,
            }),
            Expr::Name(name, span) => Ok(Env::get_resolved(
                env,
                self.binding(name, *span, false).as_ref(),
                name,
            )?),
            Expr::List(items, line) => {
                let value = Value::list(
                    items
                        .iter()
                        .map(|x| self.eval(x, env))
                        .collect::<Result<_, _>>()?,
                );
                self.check_value_limit(&value, *line)?;
                Ok(value)
            }
            Expr::Record(items, line) => {
                let value = Value::record(
                    items
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), self.eval(v, env)?)))
                        .collect::<Result<_, RuntimeError>>()?,
                );
                self.check_value_limit(&value, *line)?;
                Ok(value)
            }
            Expr::Unary { op, expr, line } => {
                let v = self.eval(expr, env)?;
                self.unary(op, v, *line)
            }
            Expr::Binary {
                left, op, right, ..
            } => {
                let a = self.eval(left, env)?;
                if op == "且" && !a.truth() {
                    return Ok(Value::Bool(false));
                }
                if op == "或" && a.truth() {
                    return Ok(Value::Bool(true));
                }
                let b = self.eval(right, env)?;
                Ok(self.binary(op, a, b)?)
            }
            Expr::Attr { receiver, name, .. } => {
                let obj = self.eval(receiver, env)?;
                Ok(self.get_attr(&obj, name)?)
            }
            Expr::Index {
                receiver, index, ..
            } => {
                let obj = self.eval(receiver, env)?;
                let idx = self.eval(index, env)?;
                Ok(self.get_index(&obj, &idx)?)
            }
            Expr::Call {
                receiver,
                name,
                args,
                named,
                line,
                ..
            } => {
                let recv = self.eval(receiver, env)?;
                let args = args
                    .iter()
                    .map(|x| self.eval(x, env))
                    .collect::<Result<Vec<_>, _>>()?;
                let named = named
                    .iter()
                    .map(|argument| Ok((argument.name.clone(), self.eval(&argument.value, env)?)))
                    .collect::<Result<BTreeMap<_, _>, RuntimeError>>()?;
                if let Some(name) = name {
                    Ok(self.call_method(recv, &self.method_operand(name), args, named, *line)?)
                } else {
                    Ok(self.call_in_env(recv, args, named, *line, env)?)
                }
            }
            Expr::Function {
                name,
                declaration_span,
                params,
                body,
                type_name: _,
                line: _,
            } => Ok(Value::Function(Rc::new(UserFunction {
                name: name.clone().unwrap_or_else(|| "匿名功能".into()),
                params: params.clone(),
                body: body.clone(),
                closure: env.clone(),
                captures: self.function_captures(*declaration_span),
                semantic_bindings: self.semantic_bindings.clone(),
                semantic_types: self.semantic_types.clone(),
                semantic_methods: self.semantic_methods.clone(),
                self_binding: name
                    .as_ref()
                    .and_then(|name| self.binding(name, *declaration_span, true)),
            }))),
        }
    }

    pub(super) fn unary(&self, op: &str, value: Value, line: usize) -> Result<Value, RuntimeError> {
        match op {
            "非" => Ok(Value::Bool(!value.truth())),
            "-" => match value {
                Value::Number(value) => Ok(Value::Number(
                    checked_decimal(Decimal::ZERO.checked_sub(value), "取负")
                        .map_err(|message| RuntimeError::new(line, message))?,
                )),
                Value::Float(value) => Ok(Value::Float(
                    checked_float(-value, "取负")
                        .map_err(|message| RuntimeError::new(line, message))?,
                )),
                value => Err(RuntimeError::new(
                    line,
                    format!("不能对{}执行取负。", value.type_name()),
                )),
            },
            _ => Err(RuntimeError::new(line, "未知单目运算".into())),
        }
    }

    pub(super) fn binary(&self, op: &str, a: Value, b: Value) -> Result<Value, String> {
        if matches!(
            op,
            "+" | "-" | "*" | "/" | "%" | "^" | "=" | "==" | "!=" | ">" | "<" | ">=" | "<="
        ) {
            if let Some(message) = crate::value::mixed_numeric_message(&a, &b) {
                return Err(self.err(message));
            }
        }
        match (op, &a, &b) {
            ("+", Value::Money(left), Value::Money(right)) => {
                return crate::finance::add(left, right)
            }
            ("-", Value::Money(left), Value::Money(right)) => {
                return crate::finance::subtract(left, right)
            }
            ("*", Value::Money(value), Value::Number(factor)) => {
                return crate::finance::scale(value, *factor, false)
            }
            ("*", Value::Number(factor), Value::Money(value)) => {
                return crate::finance::scale(value, *factor, false)
            }
            ("/", Value::Money(value), Value::Number(factor)) => {
                return crate::finance::scale(value, *factor, true)
            }
            _ => {}
        }
        let invalid = || self.err(format!("无法对 {a} 和 {b} 进行“{op}”运算。"));
        match op {
            "+" | "-" | "*" | "/" => match (&a, &b) {
                (Value::Number(left), Value::Number(right)) => {
                    if op == "/" && right.is_zero() {
                        return Err(self.err("不能除以零。"));
                    }
                    let value = match op {
                        "+" => left.checked_add(*right),
                        "-" => left.checked_sub(*right),
                        "*" => left.checked_mul(*right),
                        _ => left.checked_div(*right),
                    };
                    Ok(Value::Number(checked_decimal(value, op)?))
                }
                (Value::Float(left), Value::Float(right)) => {
                    if op == "/" && *right == 0.0 {
                        return Err(self.err("不能除以零。"));
                    }
                    let value = match op {
                        "+" => left + right,
                        "-" => left - right,
                        "*" => left * right,
                        _ => left / right,
                    };
                    Ok(Value::Float(checked_float(value, op)?))
                }
                (Value::Text(left), Value::Text(right)) if op == "+" => {
                    Ok(Value::Text(format!("{left}{right}")))
                }
                _ => Err(invalid()),
            },
            "%" => {
                let (Value::Number(left), Value::Number(right)) = (&a, &b) else {
                    return Err(invalid());
                };
                if right.is_zero() {
                    return Err(self.err("不能对零取模。"));
                }
                Ok(Value::Number(checked_decimal(
                    left.checked_rem(*right),
                    op,
                )?))
            }
            "^" => {
                let (Value::Number(left), Value::Number(right)) = (&a, &b) else {
                    return Err(invalid());
                };
                let exponent = decimal_to_i64(*right, "指数")?;
                Ok(Value::Number(checked_decimal(
                    left.checked_powi(exponent),
                    op,
                )?))
            }
            "=" | "==" => Ok(Value::Bool(values_equal(&a, &b))),
            "!=" => Ok(Value::Bool(!values_equal(&a, &b))),
            ">" | "<" | ">=" | "<=" => {
                let ord = self.compare(&a, &b)?;
                Ok(Value::Bool(match op {
                    ">" => ord == Ordering::Greater,
                    "<" => ord == Ordering::Less,
                    ">=" => ord != Ordering::Less,
                    _ => ord != Ordering::Greater,
                }))
            }
            "属于" | "不属于" => {
                let found = b
                    .iter_values_limited(self.permissions.collection_limit())?
                    .iter()
                    .any(|v| values_equal(&a, v));
                Ok(Value::Bool(if op == "属于" { found } else { !found }))
            }
            "且" => Ok(Value::Bool(a.truth() && b.truth())),
            "或" => Ok(Value::Bool(a.truth() || b.truth())),
            _ => Err(self.err(format!("未知运算符 {op}"))),
        }
    }

    pub(super) fn compare(&self, a: &Value, b: &Value) -> Result<Ordering, String> {
        if let Some(message) = crate::value::mixed_numeric_message(a, b) {
            return Err(self.err(message));
        }
        match (a, b) {
            (Value::Text(x), Value::Text(y)) => Ok(x.cmp(y)),
            (Value::Number(x), Value::Number(y)) => Ok(x.cmp(y)),
            (Value::Float(x), Value::Float(y)) => Ok(crate::value::float_order(*x, *y)),
            (Value::Money(x), Value::Money(y)) => {
                crate::value::money_order(x, y).ok_or_else(|| self.err("不同币种金额不能直接比较"))
            }
            (Value::DateTime(x), Value::DateTime(y)) => Ok(x.cmp(y)),
            (Value::Duration(x), Value::Duration(y)) => Ok(x.cmp(y)),
            _ => Err(self.err(format!("不能比较{}和{}。", a.type_name(), b.type_name()))),
        }
    }
}

pub(super) fn checked_decimal(value: Option<Decimal>, operation: &str) -> Result<Decimal, String> {
    value.ok_or_else(|| format!("数字“{operation}”运算溢出"))
}

pub(super) fn checked_float(value: f64, operation: &str) -> Result<f64, String> {
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| format!("浮点数“{operation}”运算产生非有限结果"))
}
