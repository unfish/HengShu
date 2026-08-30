use crate::ast::{BindingId, BindingOperand};
use crate::bytecode::{BytecodeProgram, CompletionReason, Constant, Instruction};
use crate::concurrency::{decode_portable, encode_portable};
use crate::execution::{ObservationBudget, ObservationMode};
mod machine;
use crate::runtime::Interpreter;
use crate::runtime::RuntimeError;
use crate::security::PermissionSet;
use crate::value::{
    BytecodeClosure, Env, EnvRef, Module, PortableCodec, PortableValue, StructType, Value,
};
use machine::{CallFrame, Machine, ReturnTarget};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[cfg(test)]
pub fn execute(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
) -> Result<Interpreter, String> {
    execute_structured(
        bytecode,
        modules,
        base_dir,
        print,
        program_args,
        permissions,
    )
    .map_err(|error| error.message)
}

pub(crate) fn execute_structured(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
) -> Result<Interpreter, RuntimeError> {
    match execute_run(
        bytecode,
        modules,
        base_dir,
        print,
        program_args,
        permissions,
    ) {
        (interpreter, None) => Ok(interpreter),
        (_, Some(error)) => Err(error),
    }
}

/// GUI and other hosts use this entry point to keep the UI responsive while
/// retaining the same VM path and cancellation checks as CLI execution.
#[cfg(feature = "gui")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_run_with_observation_and_cancellation(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
    observation: ObservationMode,
    budget: ObservationBudget,
    cancellation: Arc<AtomicBool>,
) -> (Interpreter, Option<RuntimeError>) {
    execute_run_inner(
        bytecode,
        modules,
        base_dir,
        print,
        program_args,
        permissions,
        observation,
        budget,
        Some(cancellation),
    )
}

/// 无论成败都返回解释器，使失败前的输出与失败原因同时可见。
pub(crate) fn execute_run(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
) -> (Interpreter, Option<RuntimeError>) {
    execute_run_inner(
        bytecode,
        modules,
        base_dir,
        print,
        program_args,
        permissions,
        ObservationMode::Off,
        ObservationBudget::default(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_run_with_observation(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
    observation: ObservationMode,
    budget: ObservationBudget,
) -> (Interpreter, Option<RuntimeError>) {
    execute_run_inner(
        bytecode,
        modules,
        base_dir,
        print,
        program_args,
        permissions,
        observation,
        budget,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_run_inner(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
    observation: ObservationMode,
    budget: ObservationBudget,
    cancellation: Option<Arc<AtomicBool>>,
) -> (Interpreter, Option<RuntimeError>) {
    // 端点策略来自权限上限，字节码路径必须与源码解释器共享同一敏感外发边界。
    crate::privacy::install_endpoint_policy(permissions.endpoint_policy());
    let outcome = execute_run_bounded(
        bytecode,
        modules,
        base_dir,
        print,
        program_args,
        permissions,
        observation,
        budget,
        cancellation,
    );
    crate::privacy::clear_endpoint_policy();
    outcome
}

#[allow(clippy::too_many_arguments)]
fn execute_run_bounded(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
    observation: ObservationMode,
    budget: ObservationBudget,
    cancellation: Option<Arc<AtomicBool>>,
) -> (Interpreter, Option<RuntimeError>) {
    let mut interpreter =
        Interpreter::prepare_embedded(BTreeMap::new(), base_dir, print, program_args, permissions);
    if let Some(cancellation) = cancellation {
        interpreter.set_cancellation(cancellation);
    }
    interpreter.set_observation(observation, budget);
    if let Err(message) = bytecode.verify() {
        return (interpreter, Some(RuntimeError::new(0, message)));
    }
    interpreter.begin_execution();
    let root = Rc::new(bytecode.clone());
    let env = interpreter.env.clone();
    let state = Rc::new(VmModuleState {
        programs: modules
            .into_iter()
            .map(|(name, module)| (name, Rc::new(module)))
            .collect(),
        loaded: RefCell::new(BTreeMap::new()),
    });
    interpreter.vm_modules = Some(state.clone());
    // 顶层同样是一个任务组，与源码解释器一致：游离任务在程序结束前统一等待或取消回收。
    interpreter.begin_task_group();
    let outcome = {
        let mut machine = Machine {
            interpreter: &mut interpreter,
            modules: state,
            frames: vec![CallFrame::main(root, env)],
            result: None,
        };
        machine.run()
    };
    if let Err(mut error) = outcome {
        if let Some(cleanup_error) = interpreter.vm_cancel_all_task_groups(error.span.start.line) {
            error.append_cause(cleanup_error);
        }
        return (interpreter, Some(error));
    }
    let last_line = bytecode
        .functions
        .first()
        .and_then(|main| main.source_lines.last())
        .map_or(1, |line| *line as usize);
    let error = interpreter.vm_finish_task_group(last_line).err();
    (interpreter, error)
}

/// Embedded module programs plus the cache of modules already executed, shared
/// by every machine that runs against one interpreter.
pub(crate) struct VmModuleState {
    programs: BTreeMap<String, Rc<BytecodeProgram>>,
    loaded: RefCell<BTreeMap<String, Rc<Module>>>,
}

impl Default for VmModuleState {
    fn default() -> Self {
        Self {
            programs: BTreeMap::new(),
            loaded: RefCell::new(BTreeMap::new()),
        }
    }
}

/// Runs a bytecode closure on a nested machine so native higher-order builtins
/// can call back into user functions on the bytecode path.
pub(crate) fn call_bytecode_closure(
    interpreter: &mut Interpreter,
    closure: Rc<BytecodeClosure>,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    line: usize,
) -> Result<Value, RuntimeError> {
    let modules = interpreter.vm_modules.clone().unwrap_or_default();
    let name = closure.name.clone();
    let env = interpreter.new_env(Some(closure.closure.clone()), true);
    let frame = CallFrame::closure(closure, env, args, named, line, ReturnTarget::Root)?;
    let mut machine = Machine {
        interpreter,
        modules,
        frames: Vec::new(),
        result: None,
    };
    machine.push_frame(frame, name, line)?;
    machine.run()?;
    Ok(machine.result.take().unwrap_or(Value::Null))
}

pub(crate) struct BytecodeInvocation {
    pub program: BytecodeProgram,
    pub function: u32,
    pub name: String,
    pub captured: BTreeMap<BindingId, (String, PortableValue)>,
    pub args: Vec<PortableValue>,
    pub base_dir: PathBuf,
    pub permissions: PermissionSet,
    pub cancelled: Arc<AtomicBool>,
    pub call_line: usize,
}

pub(crate) fn execute_function(
    invocation: BytecodeInvocation,
) -> Result<PortableValue, RuntimeError> {
    let BytecodeInvocation {
        program,
        function,
        name,
        captured,
        args,
        base_dir,
        permissions,
        cancelled,
        call_line,
    } = invocation;
    let codec = PortableCodec::new(
        permissions.collection_limit(),
        permissions.value_nesting_limit(),
    );
    program
        .verify()
        .map_err(|message| RuntimeError::new(call_line, message))?;
    let mut interpreter = Interpreter::new(base_dir, false);
    interpreter.permissions = permissions;
    interpreter.set_cancellation(cancelled);
    for (binding, (name, value)) in captured {
        let value = decode_portable(codec, value, call_line, "任务捕获值")?;
        interpreter
            .env
            .borrow_mut()
            .resolved_values
            .insert(binding, (name, value));
    }
    let self_binding = program
        .functions
        .get(function as usize)
        .and_then(|prototype| prototype.self_binding.clone());
    let program = Rc::new(program);
    let closure = Rc::new(BytecodeClosure {
        name: name.clone(),
        function,
        program: program.clone(),
        closure: interpreter.env.clone(),
    });
    if let Some(binding) = self_binding {
        interpreter.env.borrow_mut().resolved_values.insert(
            binding.id,
            (binding.name, Value::BytecodeFunction(closure.clone())),
        );
    }
    let frame_env = interpreter.new_env(Some(closure.closure.clone()), true);
    let args = args
        .into_iter()
        .map(|value| decode_portable(codec, value, call_line, "任务参数"))
        .collect::<Result<Vec<_>, _>>()?;
    let frame = CallFrame::closure(
        closure,
        frame_env,
        args,
        BTreeMap::new(),
        call_line,
        ReturnTarget::Root,
    )?;
    let mut machine = Machine {
        interpreter: &mut interpreter,
        modules: Rc::new(VmModuleState::default()),
        frames: Vec::new(),
        result: None,
    };
    let mut outcome = machine.push_frame(frame, name, call_line);
    if outcome.is_ok() {
        outcome = machine.run();
    }
    let result = machine.result.take();
    let result = match outcome {
        Ok(()) => result,
        Err(mut error) => {
            if let Some(cleanup_error) =
                interpreter.vm_cancel_all_task_groups(error.span.start.line)
            {
                error.append_cause(cleanup_error);
            }
            return Err(error);
        }
    };
    encode_portable(
        codec,
        &result.unwrap_or(Value::Null),
        call_line,
        "任务返回值",
    )
}

fn bind_module(
    env: &EnvRef,
    module_name: &str,
    bindings: &[BindingOperand],
    selective: bool,
    module: Rc<Module>,
) -> Result<(), String> {
    if selective {
        for binding in bindings {
            let value = module
                .public_value(&binding.name)
                .cloned()
                .ok_or_else(|| vm_error(&format!("模块“{module_name}”没有“{}”", binding.name)))?;
            Env::define_resolved(env, binding, value, None)?;
        }
    } else {
        let binding = bindings
            .first()
            .ok_or_else(|| vm_error("模块导入缺少绑定身份"))?;
        Env::define_resolved(env, binding, Value::Module(module), None)?;
    }
    Ok(())
}

fn constant_value(constant: &Constant) -> Value {
    match constant {
        Constant::Null => Value::Null,
        Constant::Bool(value) => Value::Bool(*value),
        Constant::Number(value) => Value::Number(*value),
        Constant::Text(value) => Value::Text(value.clone()),
    }
}

fn vm_error(message: &str) -> String {
    message.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler;
    use crate::parser::parse;

    fn run(source: &str) -> Result<Vec<String>, String> {
        let permissions = PermissionSet::for_source(std::path::Path::new("."));
        Ok(execute(
            &compiler::compile(&parse(source)?)?,
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            permissions,
        )?
        .output)
    }

    /// 取消带观察的 VM 运行：报告取消错误，并且仍然能收尾出 Notebook 供宿主展示。
    #[test]
    fn cancelled_observed_run_reports_cancellation_and_finishes_the_notebook() {
        let source = "定义 甲 为 范围(1, 20000)\n输出(长度(甲))";
        let cancellation = Arc::new(AtomicBool::new(true));
        let (mut interpreter, error) = execute_run_inner(
            &compiler::compile(&parse(source).unwrap()).unwrap(),
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(std::path::Path::new(".")),
            ObservationMode::Snapshots,
            ObservationBudget::default(),
            Some(cancellation),
        );
        let error = error.expect("取消必须返回错误");
        assert!(error.message.contains("取消"), "{}", error.message);
        assert!(interpreter.finish_observation().is_some());
    }

    /// 字节码路径的原生集合循环与源码解释器共用预算检查，长循环同样可被打断。
    #[test]
    fn vm_native_collection_loops_honor_the_execution_budget() {
        let mut permissions = PermissionSet::for_source(std::path::Path::new("."));
        permissions.execution_timeout_ms = Some(200);
        let started = std::time::Instant::now();
        let source =
            "定义 甲 为 集合([1, 2, 3])\n定义 结果 为 甲 并集(范围(1, 40000))\n输出(长度(结果))";
        let error = execute(
            &compiler::compile(&parse(source).unwrap()).unwrap(),
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(error.contains("时间限制"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn vm_uses_recursive_call_frames_and_defaults() {
        let source = "定义功能 阶乘（值）：\n如果 值 <= 1：\n返回 1\n结束\n返回 值 * 阶乘(值 - 1)\n结束\n定义功能 加（值，增量 为 2）：\n返回 值 + 增量\n结束\n输出(阶乘(6), 加(3))";
        assert_eq!(run(source).unwrap(), vec!["720 5"]);
    }

    #[test]
    fn vm_closures_capture_independent_environments() {
        let source = "定义功能 制作（起点）：\n返回 定义功能（增量）为 起点 + 增量\n结束\n定义 甲 为 制作(10)\n定义 乙 为 制作(20)\n输出(甲(1), 乙(2))";
        assert_eq!(run(source).unwrap(), vec!["11 22"]);
    }

    #[test]
    fn vm_exception_table_unwinds_frames_and_runs_finally() {
        let source = "定义功能 引爆（）：\n抛出 \"内部错误\"\n结束\n尝试：\n引爆()\n捕获 异常：\n输出(异常 的 信息)\n最终：\n输出(\"已清理\")\n结束";
        assert_eq!(run(source).unwrap(), vec!["内部错误", "已清理"]);
    }

    #[test]
    fn vm_nested_closures_keep_all_lexical_levels() {
        let source = "定义功能 外层（甲）：\n返回 定义功能（乙）为 定义功能（丙）为 甲 + 乙 + 丙\n结束\n定义 中层 为 外层(10)\n定义 内层 为 中层(20)\n输出(内层(3))";
        assert_eq!(run(source).unwrap(), vec!["33"]);
    }

    #[test]
    fn vm_finally_runs_before_return() {
        let source =
            "定义功能 计算（）：\n尝试：\n返回 42\n最终：\n输出(\"清理\")\n结束\n结束\n输出(计算())";
        assert_eq!(run(source).unwrap(), vec!["清理", "42"]);
    }

    #[test]
    fn vm_finally_completion_overrides_without_recursive_compilation() {
        let source =
            "定义功能 计算（）：\n尝试：\n返回 1\n最终：\n返回 2\n结束\n结束\n输出(计算())";
        assert_eq!(run(source).unwrap(), vec!["2"]);
    }

    #[test]
    fn vm_nested_shared_finally_preserves_completion_through_local_catch() {
        let source = "定义功能 计算（）：\n尝试：\n尝试：\n返回 1\n最终：\n尝试：\n抛出 \"已处理\"\n捕获 内错：\n输出(内错 的 信息)\n结束\n输出(\"内层\")\n结束\n最终：\n输出(\"外层\")\n结束\n结束\n输出(计算())";
        assert_eq!(run(source).unwrap(), vec!["已处理", "内层", "外层", "1"]);
    }

    #[test]
    fn vm_same_level_catch_never_catches_its_finally_error() {
        let source = "定义功能 计算（）：\n尝试：\n返回 1\n捕获 内错：\n输出(\"错误地捕获\")\n最终：\n输出(\"最终\")\n抛出 \"最终错误\"\n结束\n结束\n尝试：\n计算()\n捕获 外错：\n输出(外错 的 信息)\n结束";
        assert_eq!(run(source).unwrap(), vec!["最终", "最终错误"]);
    }

    #[test]
    fn vm_rethrow_reaches_outer_exception_table() {
        let source = "尝试：\n尝试：\n抛出 \"原始错误\"\n捕获 内层错误：\n抛出 内层错误\n最终：\n输出(\"内部清理\")\n结束\n捕获 外层错误：\n输出(外层错误 的 信息)\n结束";
        assert_eq!(run(source).unwrap(), vec!["内部清理", "原始错误"]);
    }

    #[test]
    fn vm_rethrow_preserves_structured_category_and_stack() {
        let source = "定义功能 引爆（）：\n抛出 错误(\"失败\"，类别 为 \"自定义\")\n结束\n定义功能 转发（）：\n引爆()\n结束\n转发()";
        let program = compiler::compile(&parse(source).unwrap()).unwrap();
        let error = match execute_structured(
            &program,
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(std::path::Path::new(".")),
        ) {
            Ok(_) => panic!("重抛应该返回运行错误"),
            Err(error) => error,
        };
        assert_eq!(error.category, "自定义");
        assert_eq!(error.message, "失败");
        assert_eq!(error.span.start.line, 2);
        assert_eq!(error.stack.as_ref(), ["转发", "引爆"]);
    }

    #[test]
    fn vm_finally_error_keeps_the_overridden_error_as_its_cause() {
        let source = "尝试：\n尝试：\n抛出 错误(\"原始\"，类别 为 \"原始类别\")\n最终：\n抛出 错误(\"覆盖\"，类别 为 \"覆盖类别\")\n结束\n捕获 已捕获：\n抛出 已捕获\n结束";
        let program = compiler::compile(&parse(source).unwrap()).unwrap();
        let error = match execute_structured(
            &program,
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(std::path::Path::new(".")),
        ) {
            Ok(_) => panic!("最终错误应该覆盖原错误"),
            Err(error) => error,
        };
        assert_eq!(error.category, "覆盖类别");
        assert_eq!(error.message, "覆盖");
        let cause = error.cause.expect("最终错误应该保留原错误原因");
        assert_eq!(cause.category, "原始类别");
        assert_eq!(cause.message, "原始");
    }

    #[test]
    fn vm_error_uses_function_source_mapping() {
        let source = "定义功能 计算（值）：\n定义 零 为 0\n返回 值 / 零\n结束\n计算(1)";
        let program = compiler::compile(&parse(source).unwrap()).unwrap();
        let error = execute_structured(
            &program,
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(std::path::Path::new(".")),
        )
        .err()
        .unwrap();
        assert_eq!(error.span.start.line, 3);
        assert_eq!(error.message, "不能除以零。");
    }

    #[test]
    fn vm_parameter_errors_match_call_and_definition_spans() {
        for source in [
            "定义功能 计算（必需）：\n返回 必需\n结束\n计算()",
            "定义功能 计算（值 为 1 / 0）：\n返回 值\n结束\n计算()",
        ] {
            let program = compiler::compile(&parse(source).unwrap()).unwrap();
            let error = match execute_structured(
                &program,
                BTreeMap::new(),
                ".",
                false,
                Vec::new(),
                PermissionSet::for_source(std::path::Path::new(".")),
            ) {
                Ok(_) => panic!("参数错误应该失败"),
                Err(error) => error,
            };
            assert_eq!(error.span.start.line, 4);
            assert_eq!(
                error.definition_span.expect("应该记录参数定义").start.line,
                1
            );
        }
    }

    #[test]
    fn vm_structured_error_keeps_source_line_separate_from_message() {
        let source = "定义 值 为 1\n抛出 \"无定位前缀\"";
        let program = compiler::compile(&parse(source).unwrap()).unwrap();
        let error = execute_structured(
            &program,
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(std::path::Path::new(".")),
        )
        .err()
        .unwrap();
        assert_eq!(error.span.start.line, 2);
        assert_eq!(error.message, "无定位前缀");
    }

    #[test]
    fn vm_caught_error_keeps_original_source_line() {
        let source = "定义功能 引爆（）：\n抛出 \"内部错误\"\n结束\n尝试：\n引爆()\n捕获 异常：\n输出(异常 的 行号)\n结束";
        assert_eq!(run(source).unwrap(), vec!["2"]);
    }

    #[test]
    fn vm_matches_reference_for_core_control_flow() {
        let source = "定义 总计 为 0\n当 总计 < 3 时重复：\n总计 为 总计 + 1\n结束\n输出(总计)";
        let reference = Interpreter::execute_source(source, ".", false).unwrap();
        assert_eq!(run(source).unwrap(), reference.output);
    }

    #[test]
    fn vm_match_arms_have_independent_scopes() {
        let source = "匹配 1：\n当 记作 值，并且满足 否：\n输出(0)\n当 记作 值：\n输出(值)\n结束";
        assert_eq!(run(source).unwrap(), vec!["1"]);
    }

    #[test]
    fn vm_can_start_and_wait_for_bytecode_functions() {
        let source = "引入 并发\n定义功能 工作（值）：\n返回 值 * 2\n结束\n定义 任务 为 并发 启动(工作, [21])\n输出(任务 等待())";
        assert_eq!(run(source).unwrap(), vec!["42"]);
    }

    #[test]
    fn vm_rejects_cycles_in_preconstructed_embedded_modules() {
        let mut main = compiler::compile(&parse("引入 甲").unwrap()).unwrap();
        main.module_imports.insert("甲".into(), "模块:甲".into());
        let mut module_a = compiler::compile_module(&parse("引入 乙").unwrap()).unwrap();
        module_a
            .module_imports
            .insert("乙".into(), "模块:乙".into());
        let mut module_b = compiler::compile_module(&parse("引入 甲").unwrap()).unwrap();
        module_b
            .module_imports
            .insert("甲".into(), "模块:甲".into());
        let modules = BTreeMap::from([("模块:甲".into(), module_a), ("模块:乙".into(), module_b)]);
        let error = execute(
            &main,
            modules,
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(std::path::Path::new(".")),
        )
        .err()
        .unwrap();
        assert!(error.contains("模块循环依赖"), "{error}");
        assert!(error.contains("甲 -> 乙 -> 甲"), "{error}");
    }

    #[test]
    fn vm_enforces_module_depth_budget_like_source_runtime() {
        let mut main = compiler::compile(&parse("引入 甲").unwrap()).unwrap();
        main.module_imports.insert("甲".into(), "模块:甲".into());
        let mut module_a = compiler::compile_module(&parse("引入 乙").unwrap()).unwrap();
        module_a
            .module_imports
            .insert("乙".into(), "模块:乙".into());
        let module_b = compiler::compile_module(&parse("输出(1)").unwrap()).unwrap();
        let modules = BTreeMap::from([("模块:甲".into(), module_a), ("模块:乙".into(), module_b)]);
        let mut permissions = PermissionSet::for_source(std::path::Path::new("."));
        permissions.max_module_depth = Some(1);
        let error = execute(&main, modules, ".", false, Vec::new(), permissions)
            .err()
            .unwrap();
        assert!(error.contains("模块加载深度超过资源限制"), "{error}");
    }

    #[test]
    fn vm_task_groups_cleanup_on_errors_returns_and_outer_catches() {
        let source = r#"
引入 并发
定义功能 工作（）：
    并发 休眠(1000)
    返回 1
结束
尝试：
    任务组：
        并发 启动(工作)
        抛出 "组内失败"
    结束
捕获 异常：
    输出("已捕获")
结束
定义功能 提前返回（）：
    任务组：
        定义 任务 为 并发 启动(定义功能（）为 1)
        返回 42
    结束
结束
输出(提前返回())
"#;
        let started = std::time::Instant::now();
        let interpreter = execute(
            &compiler::compile(&parse(source).unwrap()).unwrap(),
            BTreeMap::new(),
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(std::path::Path::new(".")),
        )
        .unwrap();
        assert_eq!(interpreter.output, vec!["已捕获", "42"]);
        assert_eq!(interpreter.active_task_group_count(), 0);
        assert!(started.elapsed() < std::time::Duration::from_millis(300));
    }

    #[test]
    fn vm_task_group_cancels_cpu_bound_bytecode_tasks() {
        let source = r#"
引入 并发
定义功能 工作（）：
    当 是 时重复：
    结束
结束
尝试：
    任务组：
        并发 启动(工作)
        抛出 "停止"
    结束
捕获 异常：
    输出("已取消")
结束
"#;
        let started = std::time::Instant::now();
        assert_eq!(run(source).unwrap(), vec!["已取消"]);
        assert!(started.elapsed() < std::time::Duration::from_millis(300));
    }

    #[test]
    fn vm_finally_cleanup_respects_loop_boundaries() {
        let source = r#"
尝试：
    当 是 时重复：
        跳出循环
    结束
    输出("正文")
最终：
    输出("最终")
结束
"#;
        assert_eq!(run(source).unwrap(), vec!["正文", "最终"]);
    }
}
