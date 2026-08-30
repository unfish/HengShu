use super::{Completion, Interpreter, RuntimeError};
use crate::value::{Env, StructInstance, UserFunction, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

impl Interpreter {
    pub(super) fn call(
        &mut self,
        function_value: Value,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
    ) -> Result<Value, RuntimeError> {
        let env = self.env.clone();
        self.call_in_env(function_value, args, named, line, &env)
    }

    pub(super) fn call_in_env(
        &mut self,
        function_value: Value,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
        env: &crate::value::EnvRef,
    ) -> Result<Value, RuntimeError> {
        let result = match function_value {
            Value::StructType(struct_type) => {
                if !args.is_empty() {
                    return Err(self
                        .err(format!(
                            "结构“{}”只能使用字段具名实参构造。",
                            struct_type.name
                        ))
                        .into());
                }
                if let Some(extra) = named.keys().find(|name| !struct_type.fields.contains(name)) {
                    return Err(self
                        .err(format!("结构“{}”不存在字段“{extra}”。", struct_type.name))
                        .into());
                }
                let mut fields = BTreeMap::new();
                for field in &struct_type.fields {
                    fields.insert(
                        field.clone(),
                        named.get(field).cloned().ok_or_else(|| {
                            RuntimeError::from(
                                self.err(format!(
                                    "创建“{}”时缺少字段“{field}”。",
                                    struct_type.name
                                )),
                            )
                        })?,
                    );
                }
                Ok(Value::StructInstance(Rc::new(RefCell::new(
                    StructInstance {
                        struct_type,
                        fields,
                    },
                ))))
            }
            Value::Function(function) => self.call_user(function, args, named, line),
            Value::BytecodeFunction(closure) => {
                crate::vm::call_bytecode_closure(self, closure, args, named, line)
            }
            Value::Native(name) => self.call_native(&name, args, named, line, env),
            _ => Err(self
                .err(format!("“{function_value}”不是可调用功能。"))
                .into()),
        }?;
        self.check_value_limit(&result, line)?;
        Ok(result)
    }

    pub(super) fn call_user(
        &mut self,
        function: Rc<UserFunction>,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
    ) -> Result<Value, RuntimeError> {
        self.check_call_depth(self.call_stack.len(), line)?;
        crate::contracts::validate_call_shape(
            &function.name,
            function
                .params
                .iter()
                .map(|parameter| (parameter.name.as_str(), parameter.default.is_some())),
            args.len(),
            named.keys().map(String::as_str),
            line,
        )
        .map_err(|error| {
            let definition_span = error.parameter.as_deref().and_then(|name| {
                function
                    .params
                    .iter()
                    .find(|parameter| parameter.name == name)
                    .map(|parameter| parameter.name_span)
            });
            RuntimeError::new(line, error.message).with_definition_span(definition_span)
        })?;
        let previous_bindings = std::mem::replace(
            &mut self.semantic_bindings,
            function.semantic_bindings.clone(),
        );
        let previous_types =
            std::mem::replace(&mut self.semantic_types, function.semantic_types.clone());
        let previous_methods = std::mem::replace(
            &mut self.semantic_methods,
            function.semantic_methods.clone(),
        );
        let result = self.call_user_body(&function, &args, &named, line);
        self.semantic_bindings = previous_bindings;
        self.semantic_types = previous_types;
        self.semantic_methods = previous_methods;
        result
    }

    fn call_user_body(
        &mut self,
        function: &UserFunction,
        args: &[Value],
        named: &BTreeMap<String, Value>,
        line: usize,
    ) -> Result<Value, RuntimeError> {
        let local = self.new_env(Some(function.closure.clone()), true);
        for (index, parameter) in function.params.iter().enumerate() {
            let value = if let Some(value) = args.get(index) {
                value.clone()
            } else if let Some(value) = named.get(&parameter.name) {
                value.clone()
            } else if let Some(default) = &parameter.default {
                self.eval(default, &local)
                    .map_err(|error| error.at_parameter_call_site(line, parameter.name_span))?
            } else {
                return Err(self
                    .err(format!(
                        "功能“{}”缺少参数“{}”。",
                        function.name, parameter.name
                    ))
                    .into());
            };
            let binding = self
                .binding(&parameter.name, parameter.name_span, true)
                .ok_or_else(|| {
                    RuntimeError::new(
                        line,
                        format!("参数“{}”缺少已解析的语义绑定身份", parameter.name),
                    )
                })?;
            Env::define_resolved(&local, &binding, value, None)?;
        }
        self.call_stack.push(function.name.clone());
        self.call_module_stack
            .push(function.closure.borrow().module_id);
        let result = self.exec_block(&function.body, &local);
        let stack = self.call_stack.clone();
        self.call_stack.pop();
        self.call_module_stack.pop();
        let completion = result.map_err(|mut error| {
            if error.stack.is_empty() {
                error.stack = stack.clone().into_boxed_slice();
            }
            error
        })?;
        match completion {
            Completion::Normal => Ok(Value::Null),
            Completion::Return(value) => Ok(value),
            Completion::Throw(mut error) => {
                if error.stack.is_empty() {
                    error.stack = stack.into_boxed_slice();
                }
                Err(error)
            }
            Completion::Break(_) | Completion::Continue(_) => Err(RuntimeError::new(
                line,
                format!("功能“{}”产生了跨功能循环控制。", function.name),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;
    use crate::runtime::interpreter::{execute, execute_with_options_structured};
    use crate::runtime::test_support::run;
    use crate::runtime::Interpreter;
    use crate::security::PermissionSet;
    use std::path::Path;

    #[test]
    fn recursion_defaults_anonymous_and_one_based_index() {
        assert_eq!(run("定义功能 阶乘（数值）：\n如果 数值 <= 1：\n返回 1\n结束\n返回 数值 * 阶乘(数值 - 1)\n结束\n定义功能 加（甲, 乙 为 2）为 甲 + 乙\n定义 判断 为 定义功能（数值）为 数值 > 10\n定义 数据 为 [5, 20]\n输出(阶乘(5), 加(3), 判断(数据[2]))"),vec!["120 5 是"])
    }

    #[test]
    fn parameter_errors_use_call_site_and_keep_definition_span() {
        let permissions = PermissionSet::for_source(Path::new("."));
        let missing = execute_with_options_structured(
            "定义功能 计算（必需）：\n返回 必需\n结束\n计算()",
            ".",
            false,
            Vec::new(),
            permissions.clone(),
            false,
        )
        .err()
        .expect("缺少参数应该失败");
        assert_eq!(missing.span.start.line, 4);
        assert_eq!(
            missing
                .definition_span
                .expect("应该记录参数定义")
                .start
                .line,
            1
        );

        let default = execute_with_options_structured(
            "定义功能 计算（值 为 1 / 0）：\n返回 值\n结束\n计算()",
            ".",
            false,
            Vec::new(),
            permissions,
            false,
        )
        .err()
        .expect("默认参数求值应该失败");
        assert_eq!(default.span.start.line, 4);
        assert_eq!(
            default
                .definition_span
                .expect("应该记录参数定义")
                .start
                .line,
            1
        );
        assert_eq!(
            default
                .cause
                .expect("应该保留默认表达式错误")
                .span
                .start
                .line,
            1
        );
    }

    #[test]
    fn while_scopes_unknown_named_arguments_and_container_equality_are_consistent() {
        assert_eq!(
            run("定义 次数 为 0\n当 次数 < 2 时重复：\n定义 临时 为 次数\n次数 为 次数 + 1\n结束\n输出(次数)"),
            vec!["2"]
        );
        let error = execute(
            "定义功能 原样（值）：\n返回 值\n结束\n原样(1, 拼错 为 2)",
            ".",
            false,
        )
        .err()
        .unwrap();
        assert!(error.contains("没有参数“拼错”"));
        assert_eq!(
            run("定义 列表值 为 [1, 2]\n输出(列表值 = 列表值)"),
            vec!["是"]
        );
    }

    #[test]
    fn user_call_never_silently_converts_illegal_loop_completion_to_null() {
        let program = parse("定义功能 非法（）：\n跳出循环\n结束\n非法()").unwrap();
        let mut interpreter = Interpreter::new(".", false);
        let error = interpreter
            .run_program(&program)
            .expect_err("绕过检查器的跨功能循环控制必须失败");
        assert!(error.contains("跨功能循环控制"), "{error}");
    }
}
