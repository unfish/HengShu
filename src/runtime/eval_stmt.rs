//! 语句执行：控制流完成状态与 `Stmt` 的求值。
//!
//! 完成状态在这里定义，因为只有语句层能产生返回、跳出和继续；表达式层只会产生值或错误。

use super::{Interpreter, RuntimeError};
use crate::ast::*;
use crate::value::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub(super) enum Completion {
    Normal,
    Return(Value),
    Break(LoopId),
    Continue(LoopId),
    Throw(RuntimeError),
}

impl Interpreter {
    pub(super) fn exec_block(
        &mut self,
        body: &[Stmt],
        env: &EnvRef,
    ) -> Result<Completion, RuntimeError> {
        for stmt in body {
            self.record_event(stmt.line(), "statement", "执行语句")?;
            self.check_operation_limits().map_err(|message| {
                RuntimeError::new(stmt.line(), message).with_category("资源错误")
            })?;
            let flow = self.exec_stmt(stmt, env)?;
            if !matches!(flow, Completion::Normal) {
                return Ok(flow);
            }
        }
        Ok(Completion::Normal)
    }
    fn exec_stmt(&mut self, stmt: &Stmt, env: &EnvRef) -> Result<Completion, RuntimeError> {
        match self.exec_stmt_inner(stmt, env) {
            Ok(completion) => Ok(completion),
            Err(error) => Ok(Completion::Throw(error.with_fallback_line(stmt.line()))),
        }
    }

    fn exec_stmt_inner(&mut self, stmt: &Stmt, env: &EnvRef) -> Result<Completion, RuntimeError> {
        match stmt {
            Stmt::Export {
                public, statement, ..
            } => {
                if let Stmt::Function {
                    name,
                    name_span,
                    params,
                    body,
                    type_name: Some(type_name),
                    line,
                    ..
                } = statement.as_ref()
                {
                    let struct_type = Env::get_struct_type(env, &self.type_operand(type_name))?;
                    let function = Rc::new(UserFunction {
                        name: name.clone(),
                        params: params.clone(),
                        body: body.clone(),
                        closure: env.clone(),
                        captures: self.function_captures(*name_span),
                        semantic_bindings: self.semantic_bindings.clone(),
                        semantic_types: self.semantic_types.clone(),
                        semantic_methods: self.semantic_methods.clone(),
                        self_binding: None,
                    });
                    let method = self.method_operand(name);
                    let method_id = method
                        .candidates
                        .iter()
                        .find(|id| id.owner == struct_type.id)
                        .copied()
                        .ok_or_else(|| {
                            RuntimeError::new(*line, format!("方法“{name}”缺少语义身份"))
                        })?;
                    struct_type
                        .method_names
                        .borrow_mut()
                        .insert(name.clone(), method_id);
                    struct_type.methods.borrow_mut().insert(
                        method_id,
                        UserMethod {
                            value: Value::Function(function),
                            public: *public,
                        },
                    );
                    return Ok(Completion::Normal);
                }
                return self.exec_stmt(statement, env);
            }
            Stmt::TaskGroup { body, line } => {
                let group_env = self.new_env(Some(env.clone()), false);
                self.begin_task_group();
                let mut completion = match self.exec_block(body, &group_env) {
                    Ok(completion) => completion,
                    Err(error) => Completion::Throw(error),
                };
                let exit = if matches!(completion, Completion::Normal) {
                    crate::concurrency::TaskGroupExit::Normal
                } else {
                    crate::concurrency::TaskGroupExit::Cancel
                };
                if let Some(cleanup_error) = self.finish_task_group(exit, *line) {
                    if let Completion::Throw(primary) = &mut completion {
                        primary.append_cause(cleanup_error);
                    } else if matches!(completion, Completion::Normal) {
                        completion = Completion::Throw(cleanup_error);
                    }
                }
                return Ok(completion);
            }
            Stmt::Define {
                name,
                name_span,
                expr,
                type_constraint,
                line,
                ..
            } => {
                let value = self.eval(expr, env)?;
                let binding = self.binding(name, *name_span, true).ok_or_else(|| {
                    RuntimeError::new(*line, format!("名称“{name}”缺少已解析的语义绑定身份"))
                })?;
                let type_constraint = type_constraint
                    .as_ref()
                    .map(|constraint| self.type_operand(constraint));
                Env::define_resolved(env, &binding, value, type_constraint.as_ref())
                    .map_err(|message| RuntimeError::from(message).with_fallback_line(*line))?;
            }
            Stmt::Assign { target, expr, line } => {
                let value = self.eval(expr, env)?;
                self.assign_target(target, value, env, *line)?;
            }
            Stmt::Expr { expr, .. } => {
                self.eval(expr, env)?;
            }
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                for (condition, body) in branches {
                    if self.eval(condition, env)?.truth() {
                        let branch_env = self.new_env(Some(env.clone()), false);
                        return self.exec_block(body, &branch_env);
                    }
                }
                let branch_env = self.new_env(Some(env.clone()), false);
                return self.exec_block(otherwise, &branch_env);
            }
            Stmt::While {
                loop_id,
                condition,
                body,
                line,
            } => {
                while self.eval(condition, env)?.truth() {
                    self.check_limits().map_err(|message| {
                        RuntimeError::new(*line, message).with_category("资源错误")
                    })?;
                    let loop_env = self.new_env(Some(env.clone()), false);
                    match self.exec_block(body, &loop_env)? {
                        Completion::Normal => {}
                        Completion::Continue(target) if target == *loop_id => {}
                        Completion::Break(target) if target == *loop_id => break,
                        flow => return Ok(flow),
                    }
                }
            }
            Stmt::For {
                loop_id,
                iterable,
                name,
                name_span,
                body,
                line,
                ..
            } => {
                let iterable = self.eval(iterable, env)?;
                let values = if let Value::Stream(stream) = iterable {
                    let mut values = Vec::new();
                    while let Some(value) = self.stream_next(&stream, *line)? {
                        self.check_collection_growth(values.len(), 1)?;
                        values.push(value);
                    }
                    values
                } else {
                    iterable.iter_values_limited(self.permissions.collection_limit())?
                };
                for value in values {
                    let loop_env = self.new_env(Some(env.clone()), false);
                    let binding = self.binding(name, *name_span, true).ok_or_else(|| {
                        RuntimeError::new(*line, format!("名称“{name}”缺少已解析的语义绑定身份"))
                    })?;
                    Env::define_resolved(&loop_env, &binding, value, None)?;
                    match self.exec_block(body, &loop_env)? {
                        Completion::Normal => {}
                        Completion::Continue(target) if target == *loop_id => {}
                        Completion::Break(target) if target == *loop_id => break,
                        flow => return Ok(flow),
                    }
                }
            }
            Stmt::Function {
                name,
                name_span,
                params,
                body,
                type_name,
                line,
                ..
            } => {
                let function = Rc::new(UserFunction {
                    name: name.clone(),
                    params: params.clone(),
                    body: body.clone(),
                    closure: env.clone(),
                    captures: self.function_captures(*name_span),
                    semantic_bindings: self.semantic_bindings.clone(),
                    semantic_types: self.semantic_types.clone(),
                    semantic_methods: self.semantic_methods.clone(),
                    self_binding: self.binding(name, *name_span, true),
                });
                if let Some(ty) = type_name {
                    let struct_type = Env::get_struct_type(env, &self.type_operand(ty))?;
                    let method = self.method_operand(name);
                    let method_id = method
                        .candidates
                        .iter()
                        .find(|id| id.owner == struct_type.id)
                        .copied()
                        .ok_or_else(|| {
                            RuntimeError::new(*line, format!("方法“{name}”缺少语义身份"))
                        })?;
                    struct_type
                        .method_names
                        .borrow_mut()
                        .insert(name.clone(), method_id);
                    struct_type.methods.borrow_mut().insert(
                        method_id,
                        UserMethod {
                            value: Value::Function(function.clone()),
                            public: true,
                        },
                    );
                } else {
                    let binding = self.binding(name, *name_span, true).ok_or_else(|| {
                        RuntimeError::new(*line, format!("名称“{name}”缺少已解析的语义绑定身份"))
                    })?;
                    Env::define_resolved(env, &binding, Value::Function(function), None)?;
                }
            }
            Stmt::Return { expr, .. } => {
                return Ok(Completion::Return(match expr {
                    Some(v) => self.eval(v, env)?,
                    None => Value::Null,
                }))
            }
            Stmt::Struct {
                name,
                name_span,
                fields,
                line,
                ..
            } => {
                let module_id = env.borrow().module_id;
                let ty = Rc::new(StructType {
                    id: self
                        .semantic_types
                        .iter()
                        .find(|semantic_type| semantic_type.span == *name_span)
                        .map(|semantic_type| semantic_type.id)
                        .ok_or_else(|| {
                            RuntimeError::new(*line, format!("结构“{name}”缺少语义类型身份"))
                        })?,
                    name: name.clone(),
                    fields: fields.clone(),
                    methods: RefCell::new(BTreeMap::new()),
                    method_names: RefCell::new(BTreeMap::new()),
                    module_id,
                });
                let binding = self.binding(name, *name_span, true).ok_or_else(|| {
                    RuntimeError::new(*line, format!("名称“{name}”缺少已解析的语义绑定身份"))
                })?;
                Env::define_resolved(env, &binding, Value::StructType(ty), None)?;
            }
            Stmt::Import {
                module,
                names,
                binding_spans,
                line,
                ..
            } => self.do_import(module, names.as_deref(), binding_spans, env, *line)?,
            Stmt::Global {
                names, name_spans, ..
            } => {
                let bindings = names
                    .iter()
                    .zip(name_spans)
                    .filter_map(|(name, span)| self.binding(name, *span, false))
                    .collect::<Vec<_>>();
                Env::declare_global_bindings(env, &bindings);
            }
            Stmt::Break { target, line } => {
                let target = target.ok_or_else(|| {
                    RuntimeError::new(*line, "非法跨功能循环控制：跳出循环缺少语义循环目标".into())
                })?;
                return Ok(Completion::Break(target));
            }
            Stmt::Continue { target, line } => {
                let target = target.ok_or_else(|| {
                    RuntimeError::new(
                        *line,
                        "非法跨功能循环控制：继续下一轮缺少语义循环目标".into(),
                    )
                })?;
                return Ok(Completion::Continue(target));
            }
            Stmt::Try {
                body,
                error_name,
                error_span,
                catch_body,
                finally_body,
                line,
                ..
            } => {
                let try_env = self.new_env(Some(env.clone()), false);
                let attempted = self.exec_block(body, &try_env);
                let resolved = match attempted {
                    Ok(Completion::Throw(error))
                        if error_name.is_some() || !catch_body.is_empty() =>
                    {
                        let catch_env = self.new_env(Some(env.clone()), false);
                        if let (Some(name), Some(span)) = (error_name, error_span) {
                            let binding = self.binding(name, *span, true).ok_or_else(|| {
                                RuntimeError::new(
                                    *line,
                                    format!("名称“{name}”缺少已解析的语义绑定身份"),
                                )
                            })?;
                            Env::define_resolved(
                                &catch_env,
                                &binding,
                                Value::Error(Rc::new(error.clone().into_issue(Vec::new()))),
                                None,
                            )?;
                        }
                        self.exec_block(catch_body, &catch_env)
                    }
                    Ok(completion) => Ok(completion),
                    Err(error) if error_name.is_some() || !catch_body.is_empty() => {
                        let catch_env = self.new_env(Some(env.clone()), false);
                        if let (Some(name), Some(span)) = (error_name, error_span) {
                            let binding = self.binding(name, *span, true).ok_or_else(|| {
                                RuntimeError::new(
                                    *line,
                                    format!("名称“{name}”缺少已解析的语义绑定身份"),
                                )
                            })?;
                            Env::define_resolved(
                                &catch_env,
                                &binding,
                                Value::Error(Rc::new(error.into_issue(Vec::new()))),
                                None,
                            )?;
                        }
                        self.exec_block(catch_body, &catch_env)
                    }
                    Err(error) => Err(error),
                };
                if !finally_body.is_empty() {
                    let finally_env = self.new_env(Some(env.clone()), false);
                    let final_flow = self.exec_block(finally_body, &finally_env)?;
                    if !matches!(final_flow, Completion::Normal) {
                        if let (
                            Completion::Throw(final_error),
                            Ok(Completion::Throw(original_error)),
                        ) = (&final_flow, &resolved)
                        {
                            return Ok(Completion::Throw(
                                final_error.clone().with_cause(original_error.clone()),
                            ));
                        }
                        return Ok(final_flow);
                    }
                }
                return resolved;
            }
            Stmt::Throw { expr, line } => {
                let value = self.eval(expr, env)?;
                let error = match value {
                    Value::Error(issue) => RuntimeError::from_issue(issue.as_ref(), *line),
                    other => RuntimeError::new(*line, other.to_string()),
                };
                return Ok(Completion::Throw(error));
            }
            Stmt::Require {
                expr,
                type_constraint,
                message,
                ..
            } => {
                let value = self.eval(expr, env)?;
                let passed = type_constraint
                    .as_ref()
                    .map(|expected| {
                        Env::value_matches_type(env, &value, &self.type_operand(expected))
                    })
                    .transpose()?
                    .unwrap_or_else(|| value.truth());
                if !passed {
                    let detail = match message {
                        Some(message) => self.eval(message, env)?.to_string(),
                        None => match type_constraint {
                            Some(expected) => {
                                format!(
                                    "要求值是{expected}，实际是{}。",
                                    value.actual_type_display(expected)
                                )
                            }
                            None => "要求的条件没有满足。".into(),
                        },
                    };
                    return Err(self.err(detail).into());
                }
            }
            Stmt::Match {
                expr,
                arms,
                otherwise,
                line,
            } => {
                let target = self.eval(expr, env)?;
                for arm in arms {
                    let arm_env = self.new_env(Some(env.clone()), false);
                    let implicit_span = arm.pattern.implicit_binding_span();
                    let current = self.binding("当前值", implicit_span, true).ok_or_else(|| {
                        RuntimeError::new(*line, "隐式名称“当前值”缺少已解析的语义绑定身份".into())
                    })?;
                    Env::define_resolved(&arm_env, &current, target.clone(), None)?;
                    let matched = match &arm.pattern {
                        MatchPattern::Value(pattern) => {
                            values_equal(&target, &self.eval(pattern, env)?)
                        }
                        MatchPattern::Type(expected) => {
                            Env::value_matches_type(env, &target, &self.type_operand(expected))?
                        }
                        MatchPattern::Bind { name, span } => {
                            let binding = self.binding(name, *span, true).ok_or_else(|| {
                                RuntimeError::new(
                                    *line,
                                    format!("名称“{name}”缺少已解析的语义绑定身份"),
                                )
                            })?;
                            Env::define_resolved(&arm_env, &binding, target.clone(), None)?;
                            true
                        }
                    };
                    if matched
                        && arm
                            .guard
                            .as_ref()
                            .map(|guard| self.eval(guard, &arm_env).map(|v| v.truth()))
                            .transpose()?
                            .unwrap_or(true)
                    {
                        return self.exec_block(&arm.body, &arm_env);
                    }
                }
                let fallback_env = self.new_env(Some(env.clone()), false);
                return self.exec_block(otherwise, &fallback_env);
            }
            Stmt::Test { .. } | Stmt::TestHook { .. } => {}
        }
        Ok(Completion::Normal)
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::interpreter::execute;
    use crate::runtime::test_support::run;

    #[test]
    fn variables_decimal_and_control() {
        assert_eq!(
            run("定义 总计 为 0\n当 总计 < 3 时重复：\n总计 为 总计 + 1\n结束\n输出(总计)"),
            vec!["3"]
        )
    }

    #[test]
    fn function_and_list() {
        assert_eq!(run("定义功能 加倍（数值）：\n返回 数值 * 2\n结束\n定义 数组 为 [1, 2]\n遍历 数组，记作 项：\n输出(加倍(项))\n结束"),vec!["2","4"])
    }

    #[test]
    fn contracts_try_match_and_general_values() {
        let source = r#"
定义 金额 为 12.5，类型限定为 数字
要求 金额 的类型是 数字
尝试：
    定义 结果 为 1 / 0
捕获 异常：
    输出(异常 的 类别)
最终：
    输出("已清理")
结束
匹配 金额：
    当 类型是 数字，并且满足 当前值 > 10：
        输出("较大金额")
    其他：
        输出("其他")
结束
定义 唯一值 为 集合([1, 1, 2])
输出(唯一值 的 数量)
"#;
        assert_eq!(run(source), vec!["运行错误", "已清理", "较大金额", "2"]);
        let error = execute(
            "定义 金额 为 1，类型限定为 数字\n金额 为 \"错误\"",
            ".",
            false,
        )
        .err()
        .unwrap();
        assert!(error.contains("要求是数字"));
    }

    #[test]
    fn concurrent_tasks_channels_and_parallel_map() {
        let source = r#"
引入 并发
定义功能 加倍（数值）：
    返回 数值 * 2
结束
定义 任务 为 并发 启动(加倍, [21])
输出(任务 等待())
定义 通道 为 并发 通道(1)
通道 发送("消息")
输出(通道 接收())
定义 结果 为 并发 并行映射([1, 2, 3], 加倍, 工作者 为 2)
输出(结果)
"#;
        assert_eq!(run(source), vec!["42", "消息", "[2, 4, 6]"]);
    }

    #[test]
    fn lexical_task_group_waits_and_does_not_leak_bindings() {
        let source = "引入 并发\n定义功能 加倍（数值）：\n返回 数值 * 2\n结束\n任务组：\n定义 任务 为 并发 启动(加倍, [21])\n结束\n输出(\"任务组已结束\")";
        assert_eq!(run(source), vec!["任务组已结束"]);
        let leaked = execute("任务组：\n定义 临时值 为 1\n结束\n输出(临时值)", ".", false);
        assert!(leaked.is_err());
    }

    #[test]
    fn lexical_task_group_cancels_tasks_before_propagating_scope_error() {
        let result = execute(
            "引入 并发\n定义功能 工作（）：\n返回 1\n结束\n任务组：\n定义 任务 为 并发 启动(工作)\n要求 否，否则说明 \"组内失败\"\n结束",
            ".",
            false,
        );
        let error = result.err().expect("任务组应该传播块内错误");
        assert!(error.contains("组内失败"));
    }

    #[test]
    fn lexical_task_group_tracks_tasks_stored_inside_collections() {
        let source = r#"
引入 并发
定义功能 工作（）：
    定义 次数 为 0
    当 次数 < 10000 时重复：
        次数 为 次数 + 1
    结束
    返回 次数
结束
定义 任务列表 为 []
任务组：
    任务列表 加入(并发 启动(工作))
结束
输出(任务列表[1] 的 状态)
"#;
        assert_eq!(run(source), vec!["已完成"]);
    }
}
