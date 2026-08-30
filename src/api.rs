//! 面向 Rust 程序的最小嵌入 API。

use crate::bytecode::BytecodeProgram;
pub use crate::diagnostic::{
    DiagnosticReport, SourcePosition, SourceSpan, DIAGNOSTIC_FILE_VARIABLE,
};
use crate::execution::{InvocationRequest, RunOutcome, RunTermination};
use crate::execution::{ObservationBudget, ObservationMode};
use crate::resource::MAX_SOURCE_BYTES;
use crate::{checker, parser, runtime, PermissionSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

const MAX_BYTECODE_BYTES: usize = 64 * 1024 * 1024;

/// 命令行与独立包在任何失败上都以此退出码结束进程。
pub const FAILURE_EXIT_CODE: i32 = 1;

/// 衡枢处理失败的阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    Parse,
    Check,
    Runtime,
    Bytecode,
    Serialization,
    Resource,
}

/// 可由嵌入方检查的衡枢错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HengshuError {
    kind: ErrorKind,
    category: Option<String>,
    span: Option<Box<SourceSpan>>,
    message: String,
    stack: Box<[String]>,
    cause: Option<Box<HengshuError>>,
    definition_span: Option<Box<SourceSpan>>,
    output: Box<[String]>,
    observation: Option<Box<crate::provenance::Notebook>>,
    termination: RunTermination,
}

impl HengshuError {
    fn new(kind: ErrorKind, message: String) -> Self {
        Self {
            kind,
            category: None,
            span: None,
            message,
            stack: Box::new([]),
            cause: None,
            definition_span: None,
            output: Box::new([]),
            observation: None,
            termination: RunTermination::Failed,
        }
    }

    fn from_diagnostic(kind: ErrorKind, diagnostic: crate::diagnostic::Diagnostic) -> Self {
        Self {
            kind,
            category: Some(diagnostic.stage.category().into()),
            span: Some(Box::new(diagnostic.span)),
            message: diagnostic.message,
            stack: Box::new([]),
            cause: None,
            definition_span: None,
            output: Box::new([]),
            observation: None,
            termination: RunTermination::Failed,
        }
    }

    fn from_runtime(error: runtime::RuntimeError) -> Self {
        let termination = error.termination;
        let cause = error
            .cause
            .map(|cause| Box::new(Self::from_runtime(*cause)));
        Self {
            kind: ErrorKind::Runtime,
            category: Some(error.category),
            span: Some(Box::new(error.span)),
            message: error.message,
            stack: error.stack,
            cause,
            definition_span: error.definition_span,
            output: Box::new([]),
            observation: None,
            termination,
        }
    }

    fn with_output(mut self, output: Vec<String>) -> Self {
        self.output = output.into_boxed_slice();
        self
    }

    fn with_observation(mut self, observation: Option<crate::provenance::Notebook>) -> Self {
        self.observation = observation.map(Box::new);
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn line(&self) -> Option<usize> {
        self.span.as_deref().map(|span| span.start.line)
    }

    pub fn column(&self) -> Option<usize> {
        self.span.as_deref().map(|span| span.start.column)
    }

    pub fn span(&self) -> Option<SourceSpan> {
        self.span.as_deref().copied()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }

    pub fn stack(&self) -> &[String] {
        &self.stack
    }

    pub fn cause(&self) -> Option<&HengshuError> {
        self.cause.as_deref()
    }

    /// 参数调用错误所关联的参数定义范围。
    pub fn definition_span(&self) -> Option<SourceSpan> {
        self.definition_span.as_deref().copied()
    }

    /// 程序在失败之前已经产生的输出。失败同样是一次执行，之前打印过的内容仍然可观察。
    pub fn output(&self) -> &[String] {
        &self.output
    }

    pub fn observation(&self) -> Option<&crate::provenance::Notebook> {
        self.observation.as_deref()
    }

    /// 与命令行、独立包写出的结构化诊断同一形状的失败身份。
    pub fn report(&self) -> DiagnosticReport {
        DiagnosticReport {
            category: self.category.clone(),
            span: self.span.as_deref().copied(),
            message: self.message.clone(),
            stack: self.stack.to_vec(),
            definition_span: self.definition_span.as_deref().copied(),
            cause: self.cause.as_deref().map(|cause| Box::new(cause.report())),
        }
    }
}

impl fmt::Display for HengshuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HengshuError {}

/// 解释执行选项。
#[derive(Clone, Debug)]
pub struct RunOptions {
    pub base_dir: PathBuf,
    pub print_output: bool,
    pub program_args: Vec<String>,
    pub permissions: PermissionSet,
    pub allow_interactive_input: bool,
    pub observation: ObservationMode,
    pub observation_budget: ObservationBudget,
}

impl RunOptions {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        let base_dir = base_dir.into();
        Self {
            permissions: PermissionSet::for_source(&base_dir),
            base_dir,
            print_output: false,
            program_args: Vec::new(),
            allow_interactive_input: false,
            observation: ObservationMode::Off,
            observation_budget: ObservationBudget::default(),
        }
    }
}

/// 一次解释执行产生的可观察结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunResult {
    pub output: Vec<String>,
    pub observation: Option<Box<crate::provenance::Notebook>>,
}

impl RunResult {
    pub fn into_outcome(self) -> RunOutcome {
        let observation = self.observation;
        let events = observation
            .map(|notebook| notebook.events)
            .unwrap_or_default();
        let peak_events = events.len();
        let output_bytes = self.output.iter().map(|line| line.len()).sum();
        RunOutcome {
            success: true,
            output: self.output,
            events,
            resources: crate::execution::ResourceSummary {
                output_bytes,
                peak_events,
                ..Default::default()
            },
            ..RunOutcome::default()
        }
    }
}

/// 已检查并编译的衡枢字节码，包含随主程序一起编译的相对导入模块。
#[derive(Debug)]
pub struct CompiledProgram {
    program: BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
}

#[derive(Serialize, Deserialize)]
struct SerializedProgram {
    program: BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
}

impl CompiledProgram {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, HengshuError> {
        if bytes.len() > MAX_BYTECODE_BYTES {
            return Err(HengshuError::new(
                ErrorKind::Serialization,
                format!("字节码超过 {MAX_BYTECODE_BYTES} 字节限制"),
            ));
        }
        let (serialized, remaining): (SerializedProgram, _) = postcard::take_from_bytes(bytes)
            .map_err(|error| HengshuError::new(ErrorKind::Serialization, error.to_string()))?;
        if !remaining.is_empty() {
            return Err(HengshuError::new(
                ErrorKind::Serialization,
                "字节码数据包含未使用的尾随字节".into(),
            ));
        }
        let compiled = Self {
            program: serialized.program,
            modules: serialized.modules,
        };
        compiled.verify()?;
        Ok(compiled)
    }

    pub fn verify(&self) -> Result<(), HengshuError> {
        self.program
            .verify()
            .and_then(|_| self.modules.values().try_for_each(BytecodeProgram::verify))
            .map_err(|message| HengshuError::new(ErrorKind::Bytecode, message))
    }

    pub fn bytecode_version(&self) -> u32 {
        self.program.version
    }

    pub fn instruction_count(&self) -> usize {
        std::iter::once(&self.program)
            .chain(self.modules.values())
            .flat_map(|program| &program.functions)
            .map(|function| function.instructions.len())
            .sum()
    }

    /// 使用内存字节码 VM 执行已编译程序。
    pub fn run(&self, options: RunOptions) -> Result<RunResult, HengshuError> {
        self.verify()?;
        let (mut interpreter, error) = crate::vm::execute_run_with_observation(
            &self.program,
            self.modules.clone(),
            options.base_dir,
            options.print_output,
            options.program_args,
            options.permissions,
            options.observation,
            options.observation_budget,
        );
        let notebook = interpreter.finish_observation();
        let output = interpreter.output;
        match error {
            None => Ok(RunResult {
                output,
                observation: notebook.map(Box::new),
            }),
            Some(error) => Err(HengshuError::from_runtime(error)
                .with_output(output)
                .with_observation(notebook)),
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, HengshuError> {
        let bytes = postcard::to_allocvec(&SerializedProgram {
            program: self.program.clone(),
            modules: self.modules.clone(),
        })
        .map_err(|error| HengshuError::new(ErrorKind::Serialization, error.to_string()))?;
        if bytes.len() > MAX_BYTECODE_BYTES {
            return Err(HengshuError::new(
                ErrorKind::Serialization,
                format!("字节码超过 {MAX_BYTECODE_BYTES} 字节限制"),
            ));
        }
        Ok(bytes)
    }
}

/// 解析并静态检查源码。
pub fn check_source(source: &str) -> Result<(), HengshuError> {
    check_source_limit(source)?;
    on_compile_stack(|| {
        let program = parser::parse_diagnostic(source)
            .map_err(|diagnostic| HengshuError::from_diagnostic(ErrorKind::Parse, diagnostic))?;
        checker::check_diagnostic(&program)
            .map_err(|diagnostic| HengshuError::from_diagnostic(ErrorKind::Check, diagnostic))
    })
}

/// 解释执行源码。
///
/// ```
/// use hengshu::api::{run_source, RunOptions};
///
/// let result = run_source("输出(1 + 2)", RunOptions::new("."))?;
/// assert_eq!(result.output, ["3"]);
/// # Ok::<(), hengshu::api::HengshuError>(())
/// ```
pub fn run_source(source: &str, options: RunOptions) -> Result<RunResult, HengshuError> {
    check_source_limit(source)?;
    check_source(source)?;
    let report = runtime::execute_output_with_observation_report(
        source.to_string(),
        options.base_dir,
        options.print_output,
        options.program_args,
        options.permissions,
        options.allow_interactive_input,
        options.observation,
        options.observation_budget,
    )
    .map_err(|failure| {
        HengshuError::from_runtime(*failure.error)
            .with_output(failure.output)
            .with_observation(failure.notebook)
    })?;
    Ok(RunResult {
        output: report.output,
        observation: report.notebook.map(Box::new),
    })
}

/// Execute through the versioned cross-entry contract used by GUI, CLI and
/// automation hosts. The source/VM semantics remain the same as `run_source`;
/// this adapter only validates and translates the portable request.
pub fn run_invocation(source: &str, request: InvocationRequest) -> RunOutcome {
    if let Err(message) = request
        .validate()
        .and_then(|_| request.encode_program_args().map(|_| ()))
    {
        return RunOutcome {
            success: false,
            diagnostics: vec![DiagnosticReport {
                category: Some("执行契约".into()),
                span: None,
                message,
                stack: Vec::new(),
                definition_span: None,
                cause: None,
            }],
            ..RunOutcome::default()
        };
    }
    let base_dir = Path::new(&request.program)
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut options = RunOptions::new(base_dir);
    options.program_args = request.encode_program_args().unwrap_or_default();
    options.permissions = request.permissions;
    options.observation = request.observation;
    options.observation_budget = request.observation_budget;
    run_source_outcome(source, options)
}

pub fn run_source_outcome(source: &str, options: RunOptions) -> RunOutcome {
    observed_outcome(|| run_source(source, options))
}

pub fn run_compiled_outcome(source: &str, options: RunOptions) -> RunOutcome {
    observed_outcome(|| run_compiled(source, options))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_embedded_outcome(
    bytecode: &BytecodeProgram,
    modules: BTreeMap<String, BytecodeProgram>,
    base_dir: impl Into<PathBuf>,
    print_output: bool,
    program_args: Vec<String>,
    permissions: PermissionSet,
    observation: ObservationMode,
    observation_budget: ObservationBudget,
) -> (RunOutcome, Option<runtime::RuntimeError>) {
    let base_dir = base_dir.into();
    let mut runtime_error = None;
    let outcome = observed_outcome(|| {
        let (mut interpreter, error) = crate::vm::execute_run_with_observation(
            bytecode,
            modules,
            base_dir,
            print_output,
            program_args,
            permissions,
            observation,
            observation_budget,
        );
        let notebook = interpreter.finish_observation();
        let output = interpreter.output;
        runtime_error = error.clone();
        match error {
            None => Ok(RunResult {
                output,
                observation: notebook.map(Box::new),
            }),
            Some(error) => Err(HengshuError::from_runtime(error)
                .with_output(output)
                .with_observation(notebook)),
        }
    });
    (outcome, runtime_error)
}

fn observed_outcome(run: impl FnOnce() -> Result<RunResult, HengshuError>) -> RunOutcome {
    let owns_session = crate::evidence::current_session().is_none();
    if owns_session {
        crate::evidence::start_observing();
    }
    let started = std::time::Instant::now();
    let mut outcome = match run() {
        Ok(result) => result.into_outcome(),
        Err(error) => RunOutcome {
            success: false,
            termination: error.termination,
            output: error.output().to_vec(),
            diagnostics: vec![error.report()],
            events: error
                .observation()
                .map(|notebook| notebook.events.clone())
                .unwrap_or_default(),
            ..RunOutcome::default()
        },
    };
    crate::evidence::observe_standard_output(&outcome.output);
    let observed = crate::evidence::observations();
    let metadata = crate::evidence::metadata_from_observations(&observed);
    outcome.artifacts = observed
        .output_paths
        .iter()
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let mut artifact =
                crate::execution::Artifact::observe_file(path, artifact_kind(path)).ok()?;
            artifact.purpose = "运行输出".into();
            artifact.metadata = metadata.clone();
            Some(artifact)
        })
        .collect();
    outcome.resources.elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    outcome.resources.observation_bytes = observed
        .inputs
        .iter()
        .map(|input| usize::try_from(input.bytes).unwrap_or(usize::MAX))
        .fold(0usize, usize::saturating_add);
    if owns_session {
        crate::evidence::stop_observing();
    }
    outcome
}

fn artifact_kind(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("pdf") => "application/pdf",
        Some("csv") => "text/csv",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("html") => "text/html",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// 将源码检查并编译为可验证、可序列化的字节码，相对模块从当前目录解析。
pub fn compile_source(source: &str) -> Result<CompiledProgram, HengshuError> {
    compile_source_in(source, Path::new("."))
}

/// 将源码检查并编译为字节码，相对模块导入从 `base_dir` 解析。
pub fn compile_source_in(source: &str, base_dir: &Path) -> Result<CompiledProgram, HengshuError> {
    check_source_limit(source)?;
    let (program, modules) = on_compile_stack(|| {
        let program = parser::parse_diagnostic(source)
            .map_err(|diagnostic| HengshuError::from_diagnostic(ErrorKind::Parse, diagnostic))?;
        checker::check_diagnostic(&program)
            .map_err(|diagnostic| HengshuError::from_diagnostic(ErrorKind::Check, diagnostic))?;
        crate::pack::compile_with_modules(&program, base_dir)
            .map_err(|message| HengshuError::new(ErrorKind::Bytecode, message))
    })?;
    let compiled = CompiledProgram { program, modules };
    compiled.verify()?;
    Ok(compiled)
}

/// 编译并使用内存字节码 VM 执行源码。
pub fn run_compiled(source: &str, options: RunOptions) -> Result<RunResult, HengshuError> {
    let compiled = compile_source_in(source, &options.base_dir)?;
    compiled.run(options)
}

/// 格式化衡枢源码。格式化器不要求源码已经通过静态检查。
pub fn format_source(source: &str) -> Result<String, HengshuError> {
    check_source_limit(source)?;
    on_compile_stack(|| Ok(crate::formatter::format_source(source)))
}

/// 嵌入方的解析、检查、编译与格式化都在大栈线程上运行，
/// 使语法嵌套深度预算先于宿主栈耗尽触发，与命令行入口保持同一行为。
fn on_compile_stack<T: Send>(
    body: impl FnOnce() -> Result<T, HengshuError> + Send,
) -> Result<T, HengshuError> {
    crate::resource::on_compile_stack_scoped(body).map_err(|error| {
        HengshuError::new(ErrorKind::Resource, format!("无法创建执行线程：{error}"))
    })?
}

fn check_source_limit(source: &str) -> Result<(), HengshuError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(HengshuError::new(
            ErrorKind::Resource,
            format!("源码超过 {MAX_SOURCE_BYTES} 字节限制"),
        ));
    }
    Ok(())
}
