use super::Interpreter;
use crate::ast::Expr;
use crate::contracts::decimal_to_one_based_index;
use crate::value::{ensure_acyclic_insertion, Env, EnvRef, OutcomeValue, Value};
use rust_decimal::Decimal;
use std::path::Path;
use std::rc::Rc;

impl Interpreter {
    pub(super) fn get_attr(&self, object: &Value, name: &str) -> Result<Value, String> {
        if let Some(value) = crate::finance::attr(object, name) {
            return Ok(value);
        }
        if let Some(value) = crate::concurrency::attr(object, name) {
            return Ok(value);
        }
        if let Some(result) = crate::stdlib::attr(object, name) {
            return result;
        }
        match object {
            Value::StructInstance(value) => value
                .borrow()
                .fields
                .get(name)
                .cloned()
                .ok_or_else(|| self.err(format!("结构没有字段“{name}”。"))),
            Value::Module(value) => value
                .exports
                .get(name)
                .cloned()
                .ok_or_else(|| self.err(format!("模块“{}”没有“{name}”。", value.name))),
            Value::Record(value) => value
                .borrow()
                .get(name)
                .cloned()
                .ok_or_else(|| self.err(format!("记录没有“{name}”。"))),
            Value::Table(value) => crate::table::table_attr(&value.borrow(), name)
                .ok_or_else(|| self.err(format!("表格没有属性或列“{name}”。"))),
            Value::NumericArray(value) => crate::analysis::array_attr(value, name)
                .ok_or_else(|| self.err(format!("数组没有属性“{name}”。"))),
            Value::Regression(value) => crate::analysis::regression_attr(value, name)
                .ok_or_else(|| self.err(format!("回归结果没有属性“{name}”。"))),
            Value::Bytes(value) => match name {
                "长度" => Ok(Value::Number(Decimal::from(value.borrow().len()))),
                _ => Err(self.err(format!("字节没有属性“{name}”。"))),
            },
            Value::Set(value) => match name {
                "数量" | "长度" => Ok(Value::Number(Decimal::from(value.borrow().len()))),
                _ => Err(self.err(format!("集合没有属性“{name}”。"))),
            },
            Value::Path(value) => match name {
                "文件名" => Ok(Value::Text(
                    value
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )),
                "扩展名" => Ok(Value::Text(
                    value
                        .extension()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )),
                "父目录" => Ok(Value::Path(Rc::new(
                    value.parent().unwrap_or(Path::new("")).to_path_buf(),
                ))),
                _ => Err(self.err(format!("路径没有属性“{name}”。"))),
            },
            Value::Error(value) => match name {
                "类别" => Ok(Value::Text(value.category.clone())),
                "信息" => Ok(Value::Text(value.message.clone())),
                "行号" => Ok(Value::Number(Decimal::from(value.line))),
                "调用栈" => Ok(Value::list(
                    value.stack.iter().cloned().map(Value::Text).collect(),
                )),
                _ => Err(self.err(format!("错误没有属性“{name}”。"))),
            },
            Value::Outcome(value) => match (value.as_ref(), name) {
                (OutcomeValue::Success(_), "成功") => Ok(Value::Bool(true)),
                (OutcomeValue::Failure(_), "成功") => Ok(Value::Bool(false)),
                (OutcomeValue::Success(value), "值") => Ok(value.clone()),
                (OutcomeValue::Failure(issue), "错误") => {
                    Ok(Value::Error(Rc::new(issue.clone())))
                }
                _ => Err(self.err(format!("结果没有属性“{name}”。"))),
            },
            _ => Err(self.err(format!("对象没有属性“{name}”。"))),
        }
    }

    pub(super) fn get_index(&self, object: &Value, index: &Value) -> Result<Value, String> {
        let one_based_index = || decimal_to_one_based_index(index.as_decimal()?);
        match object {
            Value::List(value) => value
                .borrow()
                .get(one_based_index()?)
                .cloned()
                .ok_or_else(|| self.err("下标越界。")),
            Value::Table(value) => value
                .borrow()
                .rows
                .get(one_based_index()?)
                .cloned()
                .map(Value::record)
                .ok_or_else(|| self.err("下标越界。")),
            Value::Record(value) => value
                .borrow()
                .get(&index.to_string())
                .cloned()
                .ok_or_else(|| self.err("记录键不存在。")),
            Value::NumericArray(value) => value
                .data
                .get(one_based_index()?)
                .copied()
                .map(Value::Float)
                .ok_or_else(|| self.err("下标越界。")),
            Value::Bytes(value) => value
                .borrow()
                .get(one_based_index()?)
                .copied()
                .map(|value| Value::Number(Decimal::from(value)))
                .ok_or_else(|| self.err("下标越界。")),
            Value::Set(value) => value
                .borrow()
                .get(one_based_index()?)
                .cloned()
                .ok_or_else(|| self.err("下标越界。")),
            _ => Err(self.err("对象不支持索引。")),
        }
    }

    pub(super) fn assign_target(
        &mut self,
        target: &Expr,
        value: Value,
        env: &EnvRef,
        line: usize,
    ) -> Result<(), super::RuntimeError> {
        match target {
            Expr::Name(name, span) => {
                let result: Result<(), String> = Env::assign_resolved(
                    env,
                    self.binding(name, *span, false).as_ref(),
                    name,
                    value,
                );
                Ok(
                    result
                        .map_err(|msg| super::RuntimeError::from(msg).with_fallback_line(line))?,
                )
            }
            Expr::Attr { receiver, name, .. } => {
                let object = self.eval(receiver, env)?;
                self.set_attr_value(object, name, value)
                    .map_err(|msg| super::RuntimeError::from(msg).with_fallback_line(line))
            }
            Expr::Index {
                receiver, index, ..
            } => {
                let object = self.eval(receiver, env)?;
                let index = self.eval(index, env)?;
                self.set_index_value(object, index, value)
                    .map_err(|msg| super::RuntimeError::from(msg).with_fallback_line(line))
            }
            _ => Err(self.err("左侧不是可赋值对象。").into()),
        }
    }

    pub(crate) fn set_attr_value(
        &self,
        object: Value,
        name: &str,
        value: Value,
    ) -> Result<(), String> {
        match object {
            Value::StructInstance(instance) => {
                if !instance
                    .borrow()
                    .struct_type
                    .fields
                    .iter()
                    .any(|field| field == name)
                {
                    return Err(self.err(format!("结构没有字段“{name}”。")));
                }
                ensure_acyclic_insertion(&Value::StructInstance(instance.clone()), &value)
                    .map_err(|error| self.err(error))?;
                instance.borrow_mut().fields.insert(name.into(), value);
                Ok(())
            }
            Value::Record(record) => {
                ensure_acyclic_insertion(&Value::Record(record.clone()), &value)
                    .map_err(|error| self.err(error))?;
                if !record.borrow().contains_key(name) {
                    self.check_collection_growth(record.borrow().len(), 1)?;
                }
                record.borrow_mut().insert(name.into(), value);
                Ok(())
            }
            _ => Err(self.err("对象不能设置属性。")),
        }
    }

    pub(crate) fn set_index_value(
        &self,
        object: Value,
        index: Value,
        value: Value,
    ) -> Result<(), String> {
        match object {
            Value::List(values) => {
                let index = decimal_to_one_based_index(index.as_decimal()?)?;
                ensure_acyclic_insertion(&Value::List(values.clone()), &value)
                    .map_err(|error| self.err(error))?;
                let mut values = values.borrow_mut();
                let slot = values
                    .get_mut(index)
                    .ok_or_else(|| self.err("下标越界。"))?;
                *slot = value;
                Ok(())
            }
            Value::Record(record) => {
                self.set_attr_value(Value::Record(record), &index.to_string(), value)
            }
            _ => Err(self.err("对象不能修改索引。")),
        }
    }
}
