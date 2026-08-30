use super::{method_arity, method_named_args, Interpreter, RuntimeError};
use crate::ast::MethodOperand;
use crate::contracts::{decimal_to_usize, validate_arity, validate_named_args};
use crate::value::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

impl Interpreter {
    pub(super) fn call_method(
        &mut self,
        obj: Value,
        method: &MethodOperand,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
    ) -> Result<Value, RuntimeError> {
        let result = self.call_method_inner(obj, method, args, named, line)?;
        self.check_value_limit(&result, line)?;
        Ok(result)
    }

    fn call_method_inner(
        &mut self,
        obj: Value,
        method_operand: &MethodOperand,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
    ) -> Result<Value, RuntimeError> {
        let name = method_operand.name.as_str();
        validate_arity(name, args.len(), method_arity(&obj, name))?;
        if let Some(allowed) = method_named_args(&obj, name) {
            validate_named_args(name, &named, allowed)?;
        }
        if let Some(result) = crate::finance::method(&obj, name, args.clone()) {
            return result.map_err(Into::into);
        }
        if let Some(result) =
            crate::concurrency::method(self, &obj, name, args.clone(), named.clone(), line)
        {
            return result;
        }
        if let Some(result) = crate::stdlib::method(
            &obj,
            name,
            args.clone(),
            named.clone(),
            crate::stdlib::ExecutionContext {
                base_dir: &self.base_dir,
                program_args: &self.program_args,
                permissions: &self.permissions,
                execution_budget: self.remaining_execution_time(line)?,
            },
        ) {
            return result.map_err(Into::into);
        }
        match &obj {
            Value::StructInstance(v) => {
                let method_id = {
                    let instance = v.borrow();
                    method_operand
                        .candidates
                        .iter()
                        .find(|id| instance.struct_type.methods.borrow().contains_key(id))
                        .copied()
                        .or_else(|| {
                            instance
                                .struct_type
                                .method_names
                                .borrow()
                                .get(name)
                                .copied()
                        })
                };
                let method = method_id.and_then(|method_id| {
                    v.borrow()
                        .struct_type
                        .methods
                        .borrow()
                        .get(&method_id)
                        .cloned()
                });
                if let Some(UserMethod {
                    value: Value::Function(function),
                    public,
                }) = method
                {
                    let owner_module = v.borrow().struct_type.module_id;
                    let caller_module = self
                        .call_module_stack
                        .last()
                        .copied()
                        .unwrap_or_else(|| self.env.borrow().module_id);
                    if !public && owner_module != caller_module {
                        return Err(self
                            .err(format!("结构方法“{name}”仅限所属模块内部访问。"))
                            .into());
                    }
                    let mut with_self = vec![obj.clone()];
                    with_self.extend(args);
                    return self.call_user(function, with_self, named, line);
                }
            }
            Value::Module(v) => {
                return self.call(
                    v.public_value(name)
                        .cloned()
                        .ok_or_else(|| self.err(format!("模块“{}”没有“{name}”。", v.name)))?,
                    args,
                    named,
                    line,
                )
            }
            Value::Stream(stream) => match name {
                "映射" => {
                    self.check_collection_growth(stream.borrow().operations.len(), 1)?;
                    stream
                        .borrow_mut()
                        .operations
                        .push(StreamOperation::Map(args[0].clone()));
                    return Ok(obj);
                }
                "筛选" => {
                    self.check_collection_growth(stream.borrow().operations.len(), 1)?;
                    stream
                        .borrow_mut()
                        .operations
                        .push(StreamOperation::Filter(args[0].clone()));
                    return Ok(obj);
                }
                "下一个" => return Ok(self.stream_next(stream, line)?.unwrap_or(Value::Null)),
                "收集" => {
                    let mut values = Vec::new();
                    while let Some(value) = self.stream_next(stream, line)? {
                        self.check_native_loop(values.len())?;
                        self.check_collection_growth(values.len(), 1)?;
                        values.push(value);
                    }
                    return Ok(Value::list(values));
                }
                "取前" => {
                    let count = decimal_to_usize(args[0].as_decimal()?, "取前数量")?;
                    self.check_collection_growth(0, count)?;
                    let mut values = Vec::new();
                    for taken in 0..count {
                        self.check_native_loop(taken)?;
                        let Some(value) = self.stream_next(stream, line)? else {
                            break;
                        };
                        values.push(value);
                    }
                    return Ok(Value::list(values));
                }
                "合并" => {
                    let Value::Stream(other) = &args[0] else {
                        return Err(self.err("合并需要另一个流").into());
                    };
                    let mut values = Vec::new();
                    while let Some(value) = self.stream_next(stream, line)? {
                        self.check_native_loop(values.len())?;
                        self.check_collection_growth(values.len(), 1)?;
                        values.push(value);
                    }
                    while let Some(value) = self.stream_next(other, line)? {
                        self.check_native_loop(values.len())?;
                        self.check_collection_growth(values.len(), 1)?;
                        values.push(value);
                    }
                    return Ok(Value::Stream(Rc::new(RefCell::new(StreamValue {
                        source: StreamSource::Values(values.into()),
                        operations: Vec::new(),
                    }))));
                }
                _ => {}
            },
            Value::List(v) => match name {
                "加入" => {
                    ensure_acyclic_insertion(&obj, &args[0]).map_err(|error| self.err(error))?;
                    self.check_collection_growth(v.borrow().len(), 1)?;
                    v.borrow_mut().push(args[0].clone());
                    return Ok(Value::Null);
                }
                "插入" => {
                    let index = decimal_to_usize(args[0].as_decimal()?, "插入位置")?;
                    ensure_acyclic_insertion(&obj, &args[1]).map_err(|error| self.err(error))?;
                    let mut values = v.borrow_mut();
                    if index == 0 || index > values.len() + 1 {
                        return Err(self.err("插入位置越界").into());
                    }
                    self.check_collection_growth(values.len(), 1)?;
                    values.insert(index - 1, args[1].clone());
                    return Ok(Value::Null);
                }
                "删除" => {
                    let index = decimal_to_usize(args[0].as_decimal()?, "删除位置")?;
                    let mut values = v.borrow_mut();
                    if index == 0 || index > values.len() {
                        return Err(self.err("删除位置越界").into());
                    }
                    return Ok(values.remove(index - 1));
                }
                "清空" => {
                    v.borrow_mut().clear();
                    return Ok(Value::Null);
                }
                "映射" => {
                    let function = args[0].clone();
                    let values = v.borrow().clone();
                    self.check_collection_growth(0, values.len())?;
                    let mut mapped = Vec::with_capacity(values.len());
                    for (processed, value) in values.into_iter().enumerate() {
                        self.check_native_loop(processed)?;
                        mapped.push(self.invoke_function(function.clone(), vec![value], line)?);
                    }
                    return Ok(Value::list(mapped));
                }
                "筛选" => {
                    let function = args[0].clone();
                    let values = v.borrow().clone();
                    let mut selected = Vec::new();
                    for (processed, value) in values.into_iter().enumerate() {
                        self.check_native_loop(processed)?;
                        if self
                            .invoke_function(function.clone(), vec![value.clone()], line)?
                            .truth()
                        {
                            self.check_collection_growth(selected.len(), 1)?;
                            selected.push(value);
                        }
                    }
                    return Ok(Value::list(selected));
                }
                "归并" => {
                    let function = args[0].clone();
                    let values = v.borrow().clone();
                    let mut current = args.get(1).cloned().unwrap_or(Value::Null);
                    for (processed, value) in values.into_iter().enumerate() {
                        self.check_native_loop(processed)?;
                        current =
                            self.invoke_function(function.clone(), vec![current, value], line)?;
                    }
                    return Ok(current);
                }
                _ => {}
            },
            Value::Record(v) => match name {
                "转记录" => return Ok(obj),
                "键列表" => {
                    return Ok(Value::list(
                        v.borrow().keys().cloned().map(Value::Text).collect(),
                    ))
                }
                "值列表" => return Ok(Value::list(v.borrow().values().cloned().collect())),
                "删除" => {
                    return Ok(v
                        .borrow_mut()
                        .remove(&args[0].to_string())
                        .unwrap_or(Value::Null))
                }
                "包含键" => {
                    return Ok(Value::Bool(v.borrow().contains_key(&args[0].to_string())))
                }
                _ => {}
            },
            Value::Set(v) => match name {
                "加入" => {
                    ensure_acyclic_insertion(&obj, &args[0]).map_err(|error| self.err(error))?;
                    if !v
                        .borrow()
                        .iter()
                        .any(|current| values_equal(current, &args[0]))
                    {
                        self.check_collection_growth(v.borrow().len(), 1)?;
                        v.borrow_mut().push(args[0].clone());
                    }
                    return Ok(Value::Null);
                }
                "移除" => {
                    let mut values = v.borrow_mut();
                    if let Some(index) = values
                        .iter()
                        .position(|current| values_equal(current, &args[0]))
                    {
                        return Ok(values.remove(index));
                    }
                    return Ok(Value::Null);
                }
                "包含" => {
                    return Ok(Value::Bool(
                        v.borrow()
                            .iter()
                            .any(|current| values_equal(current, &args[0])),
                    ))
                }
                "并集" | "交集" | "差集" => {
                    let other = args[0].iter_values_limited(self.permissions.collection_limit())?;
                    let left = v.borrow().clone();
                    let result = match name {
                        "并集" => {
                            let mut result = left;
                            for (processed, value) in other.into_iter().enumerate() {
                                self.check_native_loop(processed)?;
                                if !result.iter().any(|current| values_equal(current, &value)) {
                                    self.check_collection_growth(result.len(), 1)?;
                                    result.push(value);
                                }
                            }
                            result
                        }
                        _ => {
                            let keep = name == "交集";
                            let mut result = Vec::new();
                            for (processed, value) in left.into_iter().enumerate() {
                                self.check_native_loop(processed)?;
                                if other.iter().any(|current| values_equal(current, &value)) == keep
                                {
                                    result.push(value);
                                }
                            }
                            result
                        }
                    };
                    return Ok(Value::Set(Rc::new(RefCell::new(result))));
                }
                _ => {}
            },
            Value::Outcome(v) => match (v.as_ref(), name) {
                (OutcomeValue::Success(value), "取值") => return Ok(value.clone()),
                (OutcomeValue::Failure(issue), "取值") => {
                    return Err(self
                        .err(format!("{}：{}", issue.category, issue.message))
                        .into())
                }
                (OutcomeValue::Success(value), "失败时使用") => return Ok(value.clone()),
                (OutcomeValue::Failure(_), "失败时使用") => return Ok(args[0].clone()),
                _ => {}
            },
            Value::Table(v) => {
                return Ok(crate::table::table_method(
                    self,
                    &v.borrow(),
                    name,
                    args,
                    named,
                    line,
                )?)
            }
            Value::Grouped(v) => return Ok(crate::table::grouped_method(v, name, args)?),
            Value::NumericArray(v) => {
                let collection_limit = self.permissions.collection_limit();
                for value in args.iter().chain(named.values()) {
                    ensure_collection_limit(value, collection_limit)
                        .map_err(|message| self.err(message))?;
                }
                return Ok(crate::analysis::array_method(v, name, args, named)?);
            }
            Value::Regression(v) => return Ok(crate::analysis::regression_method(v, name, args)?),
            _ => {}
        }
        Err(self.err(format!("对象不支持动作“{name}”。")).into())
    }

    pub(super) fn stream_next(
        &mut self,
        stream: &Rc<RefCell<StreamValue>>,
        line: usize,
    ) -> Result<Option<Value>, RuntimeError> {
        self.stream_next_inner(stream, line)
            .map_err(|error| error.with_fallback_line(line))
    }

    fn stream_next_inner(
        &mut self,
        stream: &Rc<RefCell<StreamValue>>,
        line: usize,
    ) -> Result<Option<Value>, RuntimeError> {
        loop {
            let (next, operations) = {
                let mut stream = stream.borrow_mut();
                let next = match &mut stream.source {
                    StreamSource::Values(values) => values.pop_front(),
                    StreamSource::Decoded(source) => source.borrow_mut().next_value()?,
                    StreamSource::Channel(receiver) => {
                        let portable = match self.remaining_execution_time(line)? {
                            Some(timeout) => receiver
                                .recv_timeout(timeout)
                                .map_err(|_| self.blocking_timeout_error(line, "通道流读取超时"))?,
                            None => match receiver.recv() {
                                Ok(value) => value,
                                Err(_) => return Ok(None),
                            },
                        };
                        Some(self.portable_codec().decode(portable).map_err(|message| {
                            self.err(format!("通道接收值不可跨任务传递：{message}"))
                        })?)
                    }
                };
                (next, stream.operations.clone())
            };
            let Some(mut value) = next else {
                return Ok(None);
            };
            let mut keep = true;
            for operation in operations {
                match operation {
                    StreamOperation::Map(function) => {
                        value = self.invoke_function(function, vec![value], line)?;
                    }
                    StreamOperation::Filter(function) => {
                        if !self
                            .invoke_function(function, vec![value.clone()], line)?
                            .truth()
                        {
                            keep = false;
                            break;
                        }
                    }
                }
            }
            if keep {
                return Ok(Some(value));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::test_support::run;

    #[test]
    fn structure_method_and_type_check() {
        assert_eq!(run("结构 客户：\n姓名\n等级\n累计消费\n结束\n定义功能 客户 升级（自己）：\n如果 自己 的 累计消费 >= 10000：\n自己 的 等级 为 \"钻石\"\n结束\n结束\n定义 甲 为 客户(姓名 为 \"张三\", 等级 为 \"黄金\", 累计消费 为 12000)\n甲 升级()\n输出(甲 的 等级, 类型是(甲, \"客户\"))"),vec!["钻石 是"])
    }
}
