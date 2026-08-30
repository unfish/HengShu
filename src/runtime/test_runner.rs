use super::{Completion, Interpreter, RuntimeError};
use crate::ast::{Stmt, TestHookStage};
use crate::checker;
use crate::parser::parse;
use crate::value::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

pub(crate) struct TestReport {
    pub(crate) passed: Vec<String>,
    pub(crate) failed: Vec<(String, String)>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct TestRunOptions<'a> {
    pub(crate) filter: Option<&'a str>,
    pub(crate) fail_fast: bool,
}

pub(crate) fn list_tests(source: &str) -> Result<Vec<String>, String> {
    let program = parse(source)?;
    checker::check(&program)?;
    Ok(program
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Test { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect())
}

/// 在独立的大栈执行线程上运行测试文件，理由同`execute_output_with_options`。
pub(crate) fn execute_tests_on_execution_stack(
    source: String,
    base_dir: PathBuf,
    filter: Option<String>,
    fail_fast: bool,
) -> Result<TestReport, RuntimeError> {
    crate::resource::on_execution_stack(move || {
        execute_tests(
            &source,
            base_dir,
            TestRunOptions {
                filter: filter.as_deref(),
                fail_fast,
            },
        )
    })
}

pub(crate) fn execute_tests(
    source: &str,
    base_dir: impl Into<PathBuf>,
    options: TestRunOptions<'_>,
) -> Result<TestReport, RuntimeError> {
    let program = crate::parser::parse_diagnostic(source).map_err(RuntimeError::from_diagnostic)?;
    crate::checker::check_diagnostic(&program).map_err(RuntimeError::from_diagnostic)?;
    let tests = program
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Test { name, body, .. }
                if options.filter.is_none_or(|filter| name.contains(filter)) =>
            {
                Some((name, body))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if tests.is_empty() {
        return Ok(TestReport {
            passed: Vec::new(),
            failed: Vec::new(),
        });
    }

    let hook = |stage| {
        program.iter().find_map(|statement| match statement {
            Stmt::TestHook {
                stage: actual,
                body,
                ..
            } if *actual == stage => Some(body.as_slice()),
            _ => None,
        })
    };
    let mut interpreter = Interpreter::new(base_dir, false);
    interpreter.semantic_functions = program.symbols.functions.clone();
    interpreter.semantic_bindings = program.binding_operands();
    interpreter.semantic_types = program.symbols.types.clone();
    interpreter.semantic_methods = program.symbols.methods.clone();
    let setup = program
        .iter()
        .filter(|statement| !matches!(statement, Stmt::Test { .. } | Stmt::TestHook { .. }))
        .cloned()
        .collect::<Vec<_>>();
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    interpreter.begin_execution();
    let root_env = interpreter.env.clone();
    let base_result = execute_test_block(&mut interpreter, &setup, &root_env).and_then(|_| {
        let env = interpreter.env.clone();
        hook(TestHookStage::SuiteSetup)
            .map(|body| execute_scoped_test_hook(&mut interpreter, body, &env))
            .unwrap_or(Ok(()))
    });

    if let Err(error) = base_result {
        failed.push(("测试准备".into(), error.render()));
    } else {
        for (name, body) in tests {
            let env = clone_test_environment(&mut interpreter);
            let before = hook(TestHookStage::BeforeEach)
                .map(|body| execute_scoped_test_hook(&mut interpreter, body, &env))
                .unwrap_or(Ok(()));
            let body_result = before.and_then(|_| execute_test_block(&mut interpreter, body, &env));
            let after = hook(TestHookStage::AfterEach)
                .map(|body| execute_scoped_test_hook(&mut interpreter, body, &env))
                .unwrap_or(Ok(()));

            let result: Result<(), RuntimeError> = match (body_result, after) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) => Err(error),
                (Ok(()), Err(error)) => Err(error),
                (Err(mut error), Err(cleanup)) => {
                    error.append_cause(cleanup);
                    Err(error)
                }
            };
            match result {
                Ok(()) => passed.push(name.clone()),
                Err(error) => {
                    failed.push((name.clone(), error.render()));
                    if options.fail_fast {
                        break;
                    }
                }
            }
        }
    }

    if let Some(body) = hook(TestHookStage::SuiteTeardown) {
        let env = interpreter.env.clone();
        if let Err(error) = execute_scoped_test_hook(&mut interpreter, body, &env) {
            failed.push(("测试收尾".into(), error.render()));
        }
    }
    Ok(TestReport { passed, failed })
}

fn execute_test_block(
    interpreter: &mut Interpreter,
    body: &[Stmt],
    env: &EnvRef,
) -> Result<(), RuntimeError> {
    match interpreter.exec_block(body, env) {
        Ok(Completion::Normal) => Ok(()),
        Ok(Completion::Throw(error)) | Err(error) => Err(error),
        Ok(_) => Err(RuntimeError::new(
            body.last().map_or(1, Stmt::line),
            "测试中出现无效流程控制".into(),
        )),
    }
}

fn execute_scoped_test_hook(
    interpreter: &mut Interpreter,
    body: &[Stmt],
    parent: &EnvRef,
) -> Result<(), RuntimeError> {
    let env = interpreter.new_env(Some(parent.clone()), false);
    execute_test_block(interpreter, body, &env)
}

fn clone_test_environment(interpreter: &mut Interpreter) -> EnvRef {
    let source = interpreter.env.clone();
    let target = interpreter.new_env(None, false);
    let (values, resolved_values, global_bindings, resolved_constraints) = {
        let source = source.borrow();
        (
            source.values.clone(),
            source.resolved_values.clone(),
            source.global_bindings.clone(),
            source.resolved_type_constraints.clone(),
        )
    };
    {
        let mut target_env = target.borrow_mut();
        target_env.global_bindings = global_bindings;
        target_env.resolved_type_constraints = resolved_constraints;
    }
    let mut memo = BTreeMap::new();
    for (name, value) in values {
        let value = clone_test_value(&value, &source, &target, &mut memo);
        target.borrow_mut().values.insert(name, value);
    }
    for (binding, (name, value)) in resolved_values {
        let value = clone_test_value(&value, &source, &target, &mut memo);
        target
            .borrow_mut()
            .resolved_values
            .insert(binding, (name, value));
    }
    target
}

fn clone_test_value(
    value: &Value,
    source: &EnvRef,
    target: &EnvRef,
    memo: &mut BTreeMap<(usize, u8), Value>,
) -> Value {
    match value {
        Value::List(values) => {
            let key = (Rc::as_ptr(values) as usize, 1);
            if let Some(value) = memo.get(&key) {
                return value.clone();
            }
            let cloned = Value::list(Vec::new());
            memo.insert(key, cloned.clone());
            let Value::List(output) = &cloned else {
                return cloned;
            };
            output.borrow_mut().extend(
                values
                    .borrow()
                    .iter()
                    .map(|value| clone_test_value(value, source, target, memo)),
            );
            cloned
        }
        Value::Record(values) => {
            let key = (Rc::as_ptr(values) as usize, 2);
            if let Some(value) = memo.get(&key) {
                return value.clone();
            }
            let cloned = Value::record(BTreeMap::new());
            memo.insert(key, cloned.clone());
            let Value::Record(output) = &cloned else {
                return cloned;
            };
            for (name, value) in values.borrow().iter() {
                output
                    .borrow_mut()
                    .insert(name.clone(), clone_test_value(value, source, target, memo));
            }
            cloned
        }
        Value::Set(values) => {
            let key = (Rc::as_ptr(values) as usize, 3);
            if let Some(value) = memo.get(&key) {
                return value.clone();
            }
            let cloned = Value::Set(Rc::new(RefCell::new(Vec::new())));
            memo.insert(key, cloned.clone());
            let Value::Set(output) = &cloned else {
                return cloned;
            };
            output.borrow_mut().extend(
                values
                    .borrow()
                    .iter()
                    .map(|value| clone_test_value(value, source, target, memo)),
            );
            cloned
        }
        Value::Bytes(values) => Value::Bytes(Rc::new(RefCell::new(values.borrow().clone()))),
        Value::Table(table) => Value::Table(Rc::new(RefCell::new(Table {
            rows: table
                .borrow()
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|(name, value)| {
                            (name.clone(), clone_test_value(value, source, target, memo))
                        })
                        .collect()
                })
                .collect(),
            metadata: table.borrow().metadata.clone(),
            column_metadata: table.borrow().column_metadata.clone(),
        }))),
        Value::StructInstance(instance) => {
            let struct_type = match clone_test_value(
                &Value::StructType(instance.borrow().struct_type.clone()),
                source,
                target,
                memo,
            ) {
                Value::StructType(struct_type) => struct_type,
                _ => instance.borrow().struct_type.clone(),
            };
            Value::StructInstance(Rc::new(RefCell::new(StructInstance {
                struct_type,
                fields: instance
                    .borrow()
                    .fields
                    .iter()
                    .map(|(name, value)| {
                        (name.clone(), clone_test_value(value, source, target, memo))
                    })
                    .collect(),
            })))
        }
        Value::StructType(struct_type) => {
            let key = (Rc::as_ptr(struct_type) as usize, 4);
            if let Some(value) = memo.get(&key) {
                return value.clone();
            }
            let cloned_type = Rc::new(StructType {
                id: struct_type.id,
                name: struct_type.name.clone(),
                fields: struct_type.fields.clone(),
                methods: RefCell::new(BTreeMap::new()),
                method_names: RefCell::new(struct_type.method_names.borrow().clone()),
                module_id: target.borrow().module_id,
            });
            let cloned = Value::StructType(cloned_type.clone());
            memo.insert(key, cloned.clone());
            for (name, method) in struct_type.methods.borrow().iter() {
                cloned_type.methods.borrow_mut().insert(
                    *name,
                    UserMethod {
                        value: clone_test_value(&method.value, source, target, memo),
                        public: method.public,
                    },
                );
            }
            cloned
        }
        Value::Function(function) => {
            let mut function = function.as_ref().clone();
            if Rc::ptr_eq(&function.closure, source) {
                function.closure = target.clone();
            }
            Value::Function(Rc::new(function))
        }
        Value::BytecodeFunction(function) => {
            let mut function = function.as_ref().clone();
            if Rc::ptr_eq(&function.closure, source) {
                function.closure = target.clone();
            }
            Value::BytecodeFunction(Rc::new(function))
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{execute_tests, TestRunOptions};

    #[test]
    fn chinese_assertion_family_covers_values_collections_numbers_and_errors() {
        let source = r#"
测试 "完整断言"：
    断言（是，"逻辑断言"）
    断言相等（1 + 1，2，"相等断言"）
    断言不相等（1，2，"不相等断言"）
    断言包含（[1，2，3]，2）
    断言包含（"衡枢数据分析"，"数据"）
    断言为空（[]）
    断言为空（空）
    断言不为空（{"名称": "衡枢"}）
    断言接近（0.1 + 0.2，0.3，允许误差 为 0.000001）
    断言会失败（
        定义功能（）：
            抛出 "目标错误"
        结束，
        信息包含 为 "目标"
    ）
结束
"#;
        let report = execute_tests(source, ".", TestRunOptions::default()).unwrap();
        assert_eq!(report.passed, ["完整断言"]);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn chinese_assertions_report_each_failure_shape() {
        for (statement, expected) in [
            ("断言（否，\"自定义说明\"）", "自定义说明"),
            ("断言相等（1，2）", "实际为 1，预期为 2"),
            ("断言不相等（1，1）", "实际值与不应值均为 1"),
            ("断言包含（[1]，2）", "中没有 2"),
            ("断言为空（[1]）", "实际为 [1]"),
            ("断言不为空（[]）", "实际值为空"),
            ("断言接近（1，2，允许误差 为 0.01）", "允许误差为 0.01"),
            (
                "断言会失败（定义功能（）：\n返回 1\n结束）",
                "功能正常返回了 1",
            ),
        ] {
            let source = format!("测试 \"失败形状\"：\n{statement}\n结束");
            let report = execute_tests(&source, ".", TestRunOptions::default()).unwrap();
            assert_eq!(report.failed.len(), 1, "源码：{source}");
            assert!(
                report.failed[0].1.contains(expected),
                "源码：{source}\n错误：{}",
                report.failed[0].1
            );
        }

        for statement in [
            "断言接近（1，1，允许误差 为 -1）",
            "断言会失败（定义功能（）：\n抛出 \"甲\"\n结束，信息包含 为 \"乙\"）",
        ] {
            let source = format!("测试 \"参数边界\"：\n{statement}\n结束");
            let report = execute_tests(&source, ".", TestRunOptions::default()).unwrap();
            assert_eq!(report.failed.len(), 1, "源码：{source}");
        }
    }

    #[test]
    fn test_lifecycle_is_ordered_and_mutable_state_is_isolated() {
        let source = r#"
定义 基准 为 [0]
定义 基准记录 为 {"值": 1}
定义功能 添加标记（）：
    全局 基准
    基准 加入（3）
结束
测试准备：
    基准 加入（1）
结束
每项测试前：
    断言相等（长度（基准），2）
    基准 加入（2）
结束
测试 "第一项"：
    断言相等（长度（基准），3）
    添加标记（）
    基准记录 的 值 为 2
结束
测试 "第二项"：
    断言相等（长度（基准），3）
    断言相等（基准记录 的 值，1）
结束
每项测试后：
    断言不为空（基准）
结束
测试收尾：
    断言相等（长度（基准），2）
    断言相等（基准记录 的 值，1）
结束
"#;
        let report = execute_tests(source, ".", TestRunOptions::default()).unwrap();
        assert_eq!(report.passed, ["第一项", "第二项"]);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn test_hook_definitions_are_local_but_assignments_reach_the_test_state() {
        let source = r#"
定义 数值 为 1
定义 变化 为 1
测试准备：
    定义 数值 为 99
    断言相等（数值，99）
    变化 为 2
结束
测试 "作用域"：
    断言相等（数值，1）
    断言相等（变化，2）
结束
"#;
        let report = execute_tests(source, ".", TestRunOptions::default()).unwrap();
        assert_eq!(report.passed, ["作用域"]);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn cleanup_failures_are_kept_and_fail_fast_still_runs_suite_teardown() {
        let source = r#"
测试 "正文失败"：
    断言（否，"正文错误"）
结束
测试 "不应执行"：
    断言（否）
结束
每项测试后：
    断言（否，"逐项清理错误"）
结束
测试收尾：
    断言（否，"文件收尾错误"）
结束
"#;
        let report = execute_tests(
            source,
            ".",
            TestRunOptions {
                fail_fast: true,
                ..TestRunOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.failed.len(), 2);
        assert_eq!(report.failed[0].0, "正文失败");
        assert!(report.failed[0].1.contains("正文错误"));
        assert!(report.failed[0].1.contains("逐项清理错误"));
        assert_eq!(report.failed[1].0, "测试收尾");
        assert!(report.failed[1].1.contains("文件收尾错误"));
    }

    #[test]
    fn filtering_happens_before_test_execution() {
        let source = "测试 \"排除的失败\"：\n断言(否)\n结束\n测试 \"保留的通过\"：\n断言(是)\n结束";
        let report = execute_tests(
            source,
            ".",
            TestRunOptions {
                filter: Some("保留"),
                fail_fast: false,
            },
        )
        .unwrap();
        assert_eq!(report.passed, ["保留的通过"]);
        assert!(report.failed.is_empty());

        let no_match = "测试准备：\n断言(否)\n结束\n测试 \"唯一测试\"：\n结束";
        let report = execute_tests(
            no_match,
            ".",
            TestRunOptions {
                filter: Some("不存在"),
                fail_fast: false,
            },
        )
        .unwrap();
        assert!(report.passed.is_empty());
        assert!(report.failed.is_empty());
    }
}
