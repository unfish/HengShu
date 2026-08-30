//! 解释器本体：状态、构造、程序入口与公共执行入口。
//!
//! 语句求值在 `eval_stmt`，表达式求值在 `eval_expr`；本模块只负责把解释器建起来、
//! 把程序喂进去，并把结果交回调用方。

use super::eval_stmt::Completion;
use super::{environment::EnvironmentTracker, modules, RuntimeError};
use crate::ast::*;
#[cfg(test)]
use crate::checker;
use crate::diagnostic::SourceSpan;
use crate::execution::{ExecutionEvent, ObservationBudget, ObservationMode};
#[cfg(test)]
use crate::parser::parse;
use crate::provenance::EventCollector;
use crate::security::PermissionSet;
use crate::value::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

pub(crate) struct Interpreter {
    pub(crate) base_dir: PathBuf,
    pub(crate) env: EnvRef,
    pub(crate) output: Vec<String>,
    pub(crate) print_output: bool,
    pub(super) modules: BTreeMap<String, Rc<Module>>,
    pub(super) virtual_modules: BTreeMap<String, Program>,
    pub(super) module_registry: Rc<RefCell<modules::ModuleRegistry>>,
    pub(super) call_stack: Vec<String>,
    pub(super) call_module_stack: Vec<usize>,
    pub(super) program_args: Vec<String>,
    pub(super) cancellation: Option<Arc<AtomicBool>>,
    pub(crate) permissions: PermissionSet,
    pub(super) started_at: Instant,
    pub(super) output_bytes: usize,
    pub(super) task_groups: Vec<Vec<Rc<TaskValue>>>,
    pub(super) environments: EnvironmentTracker,
    pub(super) interactive_input: bool,
    pub(super) semantic_functions: Vec<SemanticFunction>,
    pub(crate) semantic_bindings: Vec<(SourceSpan, bool, BindingOperand)>,
    pub(crate) semantic_types: Vec<SemanticType>,
    pub(crate) semantic_methods: Vec<SemanticMethod>,
    pub(crate) vm_modules: Option<Rc<crate::vm::VmModuleState>>,
    pub(crate) observer: Option<EventCollector>,
    pub(crate) observation_step: u64,
}

impl Interpreter {
    pub fn new(base_dir: impl Into<PathBuf>, print_output: bool) -> Self {
        let base_dir = base_dir.into();
        let env = Env::new(None);
        let environments = EnvironmentTracker::new(&env);
        let mut this = Self {
            permissions: PermissionSet::for_source(&base_dir),
            base_dir,
            env,
            output: Vec::new(),
            print_output,
            modules: BTreeMap::new(),
            virtual_modules: BTreeMap::new(),
            module_registry: Rc::new(RefCell::new(modules::ModuleRegistry::default())),
            call_stack: Vec::new(),
            call_module_stack: Vec::new(),
            program_args: Vec::new(),
            cancellation: None,
            started_at: Instant::now(),
            output_bytes: 0,
            task_groups: Vec::new(),
            environments,
            interactive_input: true,
            semantic_functions: Vec::new(),
            semantic_bindings: Vec::new(),
            semantic_types: Vec::new(),
            semantic_methods: Vec::new(),
            vm_modules: None,
            observer: None,
            observation_step: 0,
        };
        this.install_builtins();
        this
    }
    pub(super) fn install_builtins(&mut self) {
        for name in [
            "输出",
            "参数化输出",
            "输入",
            "长度",
            "四舍五入",
            "绝对值",
            "最大值",
            "最小值",
            "求和",
            "平均值",
            "排序",
            "去重",
            "包含",
            "转数字",
            "转文本",
            "转逻辑",
            "类型是",
            "字节",
            "集合",
            "路径",
            "持续时间",
            "范围",
            "成功",
            "失败",
            "错误",
            "流",
            "断言",
            "断言相等",
            "断言不相等",
            "断言包含",
            "断言为空",
            "断言不为空",
            "断言接近",
            "断言会失败",
        ] {
            self.env
                .borrow_mut()
                .values
                .insert(name.into(), Value::Native(name.into()));
        }
    }
    #[cfg(test)]
    pub fn execute_source(
        source: &str,
        base_dir: impl Into<PathBuf>,
        print: bool,
    ) -> Result<Self, String> {
        let base_dir = base_dir.into();
        let permissions = PermissionSet::for_source(&base_dir);
        Self::execute_source_with_args(source, base_dir, print, Vec::new(), permissions)
    }
    #[cfg(test)]
    pub fn execute_source_with_args(
        source: &str,
        base_dir: impl Into<PathBuf>,
        print: bool,
        program_args: Vec<String>,
        permissions: PermissionSet,
    ) -> Result<Self, String> {
        Self::execute_source_with_options(source, base_dir, print, program_args, permissions, true)
    }
    #[cfg(test)]
    pub fn execute_source_with_options(
        source: &str,
        base_dir: impl Into<PathBuf>,
        print: bool,
        program_args: Vec<String>,
        permissions: PermissionSet,
        interactive_input: bool,
    ) -> Result<Self, String> {
        let program = parse(source)?;
        checker::check(&program)?;
        let mut interpreter = Self::new(base_dir, print);
        interpreter.program_args = program_args;
        interpreter.permissions = permissions;
        interpreter.interactive_input = interactive_input;
        interpreter.run_program(&program)?;
        Ok(interpreter)
    }
    pub fn prepare_embedded(
        modules: BTreeMap<String, Program>,
        base_dir: impl Into<PathBuf>,
        print: bool,
        program_args: Vec<String>,
        permissions: PermissionSet,
    ) -> Self {
        let mut interpreter = Self::new(base_dir, print);
        interpreter.virtual_modules = modules;
        interpreter.program_args = program_args;
        interpreter.permissions = permissions;
        interpreter
    }
    #[cfg(test)]
    pub(super) fn run_program(&mut self, program: &Program) -> Result<(), String> {
        self.semantic_functions = program.symbols.functions.clone();
        self.semantic_bindings = program.binding_operands();
        self.semantic_types = program.symbols.types.clone();
        self.semantic_methods = program.symbols.methods.clone();
        self.run(program)
    }

    pub(crate) fn set_observation(&mut self, mode: ObservationMode, budget: ObservationBudget) {
        self.observer = (mode != ObservationMode::Off).then(|| EventCollector::new(mode, budget));
        self.observation_step = 0;
    }

    pub(crate) fn record_event(
        &mut self,
        line: usize,
        stage: &str,
        description: &str,
    ) -> Result<(), RuntimeError> {
        if self.observer.is_none() {
            return Ok(());
        }
        self.observation_step = self.observation_step.saturating_add(1);
        let observer = self.observer.as_mut().expect("observer checked above");
        observer
            .record(ExecutionEvent {
                version: Default::default(),
                step: self.observation_step,
                source_span: Some(crate::diagnostic::SourceSpan::point(line, 1)),
                stage: stage.into(),
                description: description.into(),
                inputs: BTreeMap::new(),
                outputs: BTreeMap::new(),
                artifacts: Vec::new(),
                warnings: Vec::new(),
                provenance: Vec::new(),
            })
            .map_err(|error| RuntimeError::new(line, error.to_string()).with_category("资源错误"))
    }

    #[allow(dead_code)]
    pub(crate) fn finish_observation(&mut self) -> Option<crate::provenance::Notebook> {
        self.observer.take().map(EventCollector::finish)
    }

    #[cfg(test)]
    pub fn run(&mut self, program: &[Stmt]) -> Result<(), String> {
        self.begin_execution();
        self.run_statements(program)
    }

    pub(crate) fn run_structured(&mut self, program: &Program) -> Result<(), RuntimeError> {
        self.semantic_functions = program.symbols.functions.clone();
        self.semantic_bindings = program.binding_operands();
        self.semantic_types = program.symbols.types.clone();
        self.semantic_methods = program.symbols.methods.clone();
        self.begin_execution();
        let env = self.env.clone();
        // 顶层同样是一个任务组：`启动`出来的任务不再游离，程序结束前统一等待或取消回收。
        self.begin_task_group();
        let outcome = match self.exec_block(program, &env) {
            Ok(Completion::Normal) => Ok(()),
            Ok(Completion::Throw(error)) => Err(error),
            Ok(_) => Err(RuntimeError::new(
                program.last().map_or(1, Stmt::line),
                "顶层出现了无效的流程控制".into(),
            )),
            Err(error) => Err(error),
        };
        let exit = if outcome.is_ok() {
            crate::concurrency::TaskGroupExit::Normal
        } else {
            crate::concurrency::TaskGroupExit::Cancel
        };
        let line = program.last().map_or(1, Stmt::line);
        match (outcome, self.finish_task_group(exit, line)) {
            (Ok(()), None) => Ok(()),
            (Ok(()), Some(cleanup)) => Err(cleanup),
            (Err(mut error), cleanup) => {
                if let Some(cleanup) = cleanup {
                    error.append_cause(cleanup);
                }
                Err(error)
            }
        }
    }
    pub(super) fn function_captures(&self, declaration_span: SourceSpan) -> Vec<CapturedBinding> {
        self.semantic_functions
            .iter()
            .find(|function| function.declaration_span == declaration_span)
            .map(|function| function.captures.clone())
            .unwrap_or_default()
    }

    pub(super) fn binding(
        &self,
        name: &str,
        span: SourceSpan,
        declaration: bool,
    ) -> Option<BindingOperand> {
        self.semantic_bindings
            .iter()
            .find(|(candidate_span, candidate_declaration, candidate)| {
                *candidate_span == span
                    && *candidate_declaration == declaration
                    && candidate.name == name
            })
            .map(|(_, _, binding)| binding.clone())
    }

    pub(super) fn type_operand(&self, reference: &TypeRef) -> TypeOperand {
        TypeOperand {
            id: self
                .semantic_types
                .iter()
                .find(|semantic_type| semantic_type.span == reference.span)
                .map(|semantic_type| semantic_type.id),
            name: reference.name.clone(),
        }
    }

    pub(super) fn method_operand(&self, name: &str) -> MethodOperand {
        MethodOperand {
            candidates: self
                .semantic_methods
                .iter()
                .filter(|method| method.name == name)
                .map(|method| method.id)
                .collect(),
            name: name.to_string(),
        }
    }
    #[cfg(test)]
    pub fn run_statements(&mut self, program: &[Stmt]) -> Result<(), String> {
        let env = self.env.clone();
        match self.exec_block(program, &env) {
            Ok(Completion::Normal) => Ok(()),
            Ok(Completion::Throw(error)) => Err(error.message),
            Ok(_) => Err("顶层出现了无效的流程控制".into()),
            Err(error) => Err(error.message),
        }
    }
    pub(super) fn emit(&mut self, text: String) -> Result<(), String> {
        let next = self.output_bytes.saturating_add(text.len() + 1);
        if next > self.permissions.output_limit() {
            return Err(self.err("程序输出超过最大输出字节限制"));
        }
        self.output_bytes = next;
        if self.print_output {
            println!("{text}");
        }
        self.output.push(text);
        Ok(())
    }

    pub(super) fn err(&self, message: impl AsRef<str>) -> String {
        message.as_ref().to_string()
    }
    pub(crate) fn portable_codec(&self) -> PortableCodec {
        PortableCodec::new(
            self.permissions.collection_limit(),
            self.permissions.value_nesting_limit(),
        )
    }
}

#[cfg(test)]
pub fn execute(
    source: &str,
    base_dir: impl Into<PathBuf>,
    print: bool,
) -> Result<Interpreter, String> {
    Interpreter::execute_source(source, base_dir, print)
}

pub(crate) fn execute_with_args(
    source: &str,
    base_dir: impl Into<PathBuf>,
    print: bool,
    args: Vec<String>,
) -> Result<Vec<String>, String> {
    let base_dir = base_dir.into();
    let permissions = PermissionSet::for_source(&base_dir);
    execute_output_with_options(source.to_string(), base_dir, print, args, permissions, true)
        .map_err(|failure| failure.error.render())
}

#[cfg(test)]
pub(crate) fn execute_with_permissions(
    source: &str,
    base_dir: impl Into<PathBuf>,
    print: bool,
    args: Vec<String>,
    permissions: PermissionSet,
) -> Result<Interpreter, String> {
    Interpreter::execute_source_with_args(source, base_dir, print, args, permissions)
}

/// 一次失败执行的完整可观察结果。程序在失败前同样打印过东西，只返回错误会让嵌入方
/// 和差分门禁看不到失败前的可观察行为。
pub(crate) struct FailedRun {
    pub(crate) output: Vec<String>,
    pub(crate) error: Box<RuntimeError>,
    pub(crate) notebook: Option<crate::provenance::Notebook>,
}

impl From<RuntimeError> for FailedRun {
    fn from(error: RuntimeError) -> Self {
        Self {
            output: Vec::new(),
            error: Box::new(error),
            notebook: None,
        }
    }
}

/// 在独立的大栈执行线程上解释源码，只返回可跨线程传递的输出。
///
/// 源码解释器的用户功能调用会递归 Rust 栈，宿主默认栈在 debug 构建下装不下
/// `最大调用深度` 预算内的全部帧，语言级资源错误会退化成宿主栈溢出。
pub(crate) fn execute_output_with_options(
    source: String,
    base_dir: PathBuf,
    print: bool,
    args: Vec<String>,
    permissions: PermissionSet,
    interactive_input: bool,
) -> Result<Vec<String>, FailedRun> {
    execute_output_with_observation_report(
        source,
        base_dir,
        print,
        args,
        permissions,
        interactive_input,
        ObservationMode::Off,
        ObservationBudget::default(),
    )
    .map(|report| report.output)
}

pub(crate) struct ExecutionOutput {
    pub(crate) output: Vec<String>,
    pub(crate) notebook: Option<crate::provenance::Notebook>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_output_with_observation(
    source: String,
    base_dir: PathBuf,
    print: bool,
    args: Vec<String>,
    permissions: PermissionSet,
    interactive_input: bool,
    observation: ObservationMode,
    observation_budget: ObservationBudget,
) -> Result<ExecutionOutput, FailedRun> {
    let endpoint_policy = permissions.endpoint_policy();
    let observation_session = crate::evidence::current_session();
    crate::resource::on_execution_stack(move || {
        crate::evidence::with_session(observation_session, || {
            crate::privacy::install_endpoint_policy(endpoint_policy);
            let (mut interpreter, error) = execute_run_with_options(
                &source,
                base_dir,
                print,
                args,
                permissions,
                interactive_input,
                observation,
                observation_budget,
            );
            let notebook = interpreter.finish_observation();
            let output = interpreter.output;
            let result = match error {
                None => Ok(ExecutionOutput { output, notebook }),
                Some(error) => Err(FailedRun {
                    output,
                    error: Box::new(error),
                    notebook,
                }),
            };
            crate::privacy::clear_endpoint_policy();
            result
        })
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_output_with_observation_report(
    source: String,
    base_dir: PathBuf,
    print: bool,
    args: Vec<String>,
    permissions: PermissionSet,
    interactive_input: bool,
    observation: ObservationMode,
    observation_budget: ObservationBudget,
) -> Result<ExecutionOutput, FailedRun> {
    // 端点策略来自权限上限，运行期只能收紧。
    crate::privacy::install_endpoint_policy(permissions.endpoint_policy());
    let outcome = execute_output_with_observation(
        source,
        base_dir,
        print,
        args,
        permissions,
        interactive_input,
        observation,
        observation_budget,
    );
    crate::privacy::clear_endpoint_policy();
    outcome
}

#[cfg(test)]
pub(crate) fn execute_with_options_structured(
    source: &str,
    base_dir: impl Into<PathBuf>,
    print: bool,
    args: Vec<String>,
    permissions: PermissionSet,
    interactive_input: bool,
) -> Result<Interpreter, RuntimeError> {
    match execute_run_with_options(
        source,
        base_dir,
        print,
        args,
        permissions,
        interactive_input,
        ObservationMode::Off,
        ObservationBudget::default(),
    ) {
        (interpreter, None) => Ok(interpreter),
        (_, Some(error)) => Err(error),
    }
}

/// 无论成败都返回解释器，使失败前的输出与失败原因同时可见。
#[allow(clippy::too_many_arguments)]
fn execute_run_with_options(
    source: &str,
    base_dir: impl Into<PathBuf>,
    print: bool,
    args: Vec<String>,
    permissions: PermissionSet,
    interactive_input: bool,
    observation: ObservationMode,
    observation_budget: ObservationBudget,
) -> (Interpreter, Option<RuntimeError>) {
    let mut interpreter = Interpreter::new(base_dir, print);
    interpreter.program_args = args;
    interpreter.permissions = permissions;
    interpreter.interactive_input = interactive_input;
    interpreter.set_observation(observation, observation_budget);
    let program = crate::parser::parse_diagnostic(source)
        .map_err(RuntimeError::from_diagnostic)
        .and_then(|program| {
            crate::checker::check_diagnostic(&program).map_err(RuntimeError::from_diagnostic)?;
            Ok(program)
        });
    match program {
        Err(error) => (interpreter, Some(error)),
        Ok(program) => {
            let error = interpreter.run_structured(&program).err();
            (interpreter, error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::execute;
    use std::rc::Rc;

    #[test]
    fn interpreter_drop_releases_named_function_environment_cycles() {
        let weak = {
            let interpreter = execute("定义功能 自身（）：\n返回 自身\n结束", ".", false).unwrap();
            Rc::downgrade(&interpreter.env)
        };
        assert!(weak.upgrade().is_none());
    }
}
