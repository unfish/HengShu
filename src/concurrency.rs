use crate::ast::{BindingId, BindingOperand, CapturedBinding, Param, Stmt};
use crate::bytecode::BytecodeProgram;
use crate::contracts::{decimal_to_usize, timeout_duration};
use crate::runtime::{Interpreter, RuntimeError};
use crate::security::PermissionSet;
use crate::value::{
    ChannelValue, EnvRef, PortableCodec, PortableValue, StreamSource, StreamValue, TaskValue,
    UserFunction, Value,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskGroupExit {
    Normal,
    Cancel,
}

fn portable_error(line: usize, context: &str, message: String) -> RuntimeError {
    RuntimeError::new(line, format!("{context}不可传递：{message}")).with_category("并发错误")
}

pub(crate) fn encode_portable(
    codec: PortableCodec,
    value: &Value,
    line: usize,
    context: &str,
) -> Result<PortableValue, RuntimeError> {
    codec
        .encode(value)
        .map_err(|message| portable_error(line, context, message))
}

pub(crate) fn decode_portable(
    codec: PortableCodec,
    value: PortableValue,
    line: usize,
    context: &str,
) -> Result<Value, RuntimeError> {
    codec
        .decode(value)
        .map_err(|message| portable_error(line, context, message))
}

#[derive(Clone)]
enum TaskFunction {
    Source {
        name: String,
        params: Vec<Param>,
        body: Vec<Stmt>,
        captures: Vec<CapturedBinding>,
        semantic_bindings: Vec<(crate::diagnostic::SourceSpan, bool, BindingOperand)>,
        semantic_types: Vec<crate::ast::SemanticType>,
        semantic_methods: Vec<crate::ast::SemanticMethod>,
        self_binding: Option<BindingOperand>,
        captured: BTreeMap<BindingId, (String, PortableValue)>,
    },
    Bytecode {
        name: String,
        function: u32,
        program: BytecodeProgram,
        captured: BTreeMap<BindingId, (String, PortableValue)>,
    },
}

pub fn module_functions(name: &str) -> Option<&'static [&'static str]> {
    (name == "并发").then_some(&[
        "启动",
        "等待全部",
        "通道",
        "通道转流",
        "任务组",
        "加入任务组",
        "结束任务组",
        "并行映射",
        "休眠",
    ])
}

pub fn call(
    interpreter: &mut Interpreter,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    line: usize,
) -> Result<Value, RuntimeError> {
    crate::contracts::validate_arity(name, args.len(), concurrency_arity(name))?;
    match name {
        "启动" => Ok(start(interpreter, args, line)?),
        "等待全部" => {
            let mut values = Vec::new();
            for task in args[0].iter_values_limited(interpreter.permissions.collection_limit())? {
                let Value::Task(task) = task else {
                    return Err(error("等待全部只接受任务列表").into());
                };
                values.push(wait_task(interpreter, &task, None, line)?);
            }
            Ok(Value::list(values))
        }
        "通道" => {
            let capacity = args
                .first()
                .map(Value::as_decimal)
                .transpose()?
                .map(|value| decimal_to_usize(value, "通道容量"))
                .transpose()?;
            let limit = interpreter.permissions.channel_capacity_limit();
            if capacity.is_some_and(|capacity| capacity > limit) {
                return Err(
                    RuntimeError::new(line, format!("通道容量超过资源上限 {limit}"))
                        .with_category("资源错误"),
                );
            }
            let (sender, receiver) = match capacity {
                Some(capacity) => crossbeam_channel::bounded(capacity),
                None => crossbeam_channel::unbounded(),
            };
            Ok(Value::Channel(Rc::new(ChannelValue {
                sender: RefCell::new(Some(sender)),
                receiver,
            })))
        }
        "通道转流" => {
            let Value::Channel(channel) = &args[0] else {
                return Err(error("通道转流需要通道对象").into());
            };
            let receiver = channel.receiver.clone();
            Ok(Value::Stream(Rc::new(RefCell::new(StreamValue {
                source: StreamSource::Channel(receiver),
                operations: Vec::new(),
            }))))
        }
        "任务组" => Ok(Value::list(Vec::new())),
        "加入任务组" => {
            let Value::List(group) = &args[0] else {
                return Err(error("加入任务组需要任务组").into());
            };
            if !matches!(args[1], Value::Task(_)) {
                return Err(error("任务组只能加入任务").into());
            }
            interpreter.check_collection_growth(group.borrow().len(), 1)?;
            group.borrow_mut().push(args[1].clone());
            Ok(Value::Null)
        }
        "结束任务组" => {
            let Value::List(group) = &args[0] else {
                return Err(error("结束任务组需要任务组").into());
            };
            let cancel = named.get("取消").is_some_and(Value::truth);
            let mut results = Vec::new();
            for value in group.borrow().iter() {
                let Value::Task(task) = value else { continue };
                if cancel && task.status.load(Ordering::Relaxed) == 0 {
                    task.cancelled.store(true, Ordering::Relaxed);
                    task.status.store(2, Ordering::Relaxed);
                }
                results.push(wait_task(interpreter, task, None, line)?);
            }
            Ok(Value::list(results))
        }
        "并行映射" => parallel_map(interpreter, args, named, line),
        "休眠" => {
            let milliseconds = timeout_duration(&args[0], "休眠时间")?.as_millis() as u64;
            interpreter.blocking_sleep(milliseconds)?;
            Ok(Value::Null)
        }
        _ => Err(error(format!("并发模块不支持“{name}”")).into()),
    }
}

fn concurrency_arity(name: &str) -> Option<(usize, Option<usize>)> {
    Some(match name {
        "启动" => (1, Some(2)),
        "等待全部" | "通道转流" | "结束任务组" | "休眠" => (1, Some(1)),
        "通道" | "任务组" => (0, Some(1)),
        "加入任务组" | "并行映射" => (2, Some(2)),
        _ => return None,
    })
}

pub fn attr(value: &Value, name: &str) -> Option<Value> {
    match value {
        Value::Task(task) if name == "状态" => Some(Value::Text(
            match task.status.load(Ordering::Relaxed) {
                0 => "运行中",
                1 => "已完成",
                2 => "已取消",
                _ => "失败",
            }
            .into(),
        )),
        _ => None,
    }
}

pub fn method(
    interpreter: &Interpreter,
    value: &Value,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    line: usize,
) -> Option<Result<Value, RuntimeError>> {
    match value {
        Value::Task(task) if matches!(name, "等待" | "取消") => {
            Some(task_method(interpreter, task, name, args, named, line))
        }
        Value::Channel(channel) if matches!(name, "发送" | "接收" | "尝试接收" | "关闭") => {
            Some(channel_method(
                interpreter,
                channel,
                name,
                args,
                named,
                line,
            ))
        }
        _ => None,
    }
}

fn task_method(
    interpreter: &Interpreter,
    task: &TaskValue,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    line: usize,
) -> Result<Value, RuntimeError> {
    match name {
        "等待" => {
            let timeout = named
                .get("最长等待")
                .or_else(|| args.first())
                .map(|value| timeout_duration(value, "任务最长等待"))
                .transpose()?
                .map(|value| value.as_millis() as u64);
            wait_task(interpreter, task, timeout, line)
        }
        "取消" => {
            task.cancelled.store(true, Ordering::Relaxed);
            task.status.store(2, Ordering::Relaxed);
            Ok(Value::Null)
        }
        _ => Err(error("任务动作无效").into()),
    }
}

fn channel_method(
    interpreter: &Interpreter,
    channel: &ChannelValue,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    line: usize,
) -> Result<Value, RuntimeError> {
    let codec = interpreter.portable_codec();
    match name {
        "发送" => match channel.sender.borrow().as_ref() {
            Some(sender) => {
                let value = encode_portable(codec, &args[0], line, "通道发送值")?;
                match interpreter.blocking_timeout(None, line)? {
                    Some(timeout) => sender
                        .send_timeout(value, timeout)
                        .map(|_| Value::Null)
                        .map_err(|_| interpreter.blocking_timeout_error(line, "通道发送超时")),
                    None => sender
                        .send(value)
                        .map(|_| Value::Null)
                        .map_err(|e| RuntimeError::new(line, e.to_string())),
                }
            }
            None => Err(RuntimeError::new(line, "通道已经关闭".into())),
        },
        "接收" => {
            let timeout = named
                .get("最长等待")
                .map(|value| timeout_duration(value, "通道接收最长等待"))
                .transpose()?
                .map(|value| value.as_millis() as u64);
            let result = match interpreter.blocking_timeout(timeout, line)? {
                Some(timeout) => channel
                    .receiver
                    .recv_timeout(timeout)
                    .map_err(|_| interpreter.blocking_timeout_error(line, "通道接收超时")),
                None => channel
                    .receiver
                    .recv()
                    .map_err(|e| RuntimeError::new(line, e.to_string())),
            };
            decode_portable(codec, result?, line, "通道接收值")
        }
        "尝试接收" => match channel.receiver.try_recv() {
            Ok(value) => decode_portable(codec, value, line, "通道接收值"),
            Err(_) => Ok(Value::Null),
        },
        "关闭" => {
            channel.sender.borrow_mut().take();
            Ok(Value::Null)
        }
        _ => Err(RuntimeError::new(line, "通道动作无效".into())),
    }
}

fn start(
    interpreter: &mut Interpreter,
    args: Vec<Value>,
    line: usize,
) -> Result<Value, RuntimeError> {
    let codec = interpreter.portable_codec();
    let spec = match &args[0] {
        Value::Function(function) => snapshot_function(function, codec, line)?,
        Value::BytecodeFunction(function) => snapshot_bytecode_function(function, codec, line)?,
        _ => return Err(RuntimeError::new(line, "启动任务需要一个功能".into())),
    };
    let task_args = args
        .get(1)
        .map(|value| value.iter_values_limited(interpreter.permissions.collection_limit()))
        .transpose()?
        .unwrap_or_default()
        .iter()
        .map(|value| encode_portable(codec, value, line, "任务参数"))
        .collect::<Result<Vec<_>, _>>()?;
    let base_dir = interpreter.base_dir.clone();
    let mut permissions = interpreter.permissions.clone();
    permissions.execution_timeout_ms = interpreter
        .remaining_execution_time(line)?
        .map(|remaining| remaining.as_millis().min(u128::from(u64::MAX)) as u64);
    let cancelled = Arc::new(AtomicBool::new(false));
    let status = Arc::new(AtomicU8::new(0));
    let (sender, receiver) = crossbeam_channel::bounded(1);
    let thread_cancelled = cancelled.clone();
    let thread_status = status.clone();
    let observation_session = crate::evidence::current_session();
    crate::resource::execution_thread()
        .spawn(move || {
            crate::evidence::with_session(observation_session, || {
                let result = run_spec(
                    spec,
                    task_args,
                    base_dir,
                    permissions,
                    thread_cancelled.clone(),
                    line,
                );
                thread_status.store(
                    if thread_cancelled.load(Ordering::Relaxed) {
                        2
                    } else if result.is_ok() {
                        1
                    } else {
                        3
                    },
                    Ordering::Relaxed,
                );
                let _ = sender.send(result);
            });
        })
        .map_err(|error| {
            RuntimeError::new(line, format!("无法创建任务线程：{error}")).with_category("资源错误")
        })?;
    let task = Rc::new(TaskValue {
        receiver,
        result: RefCell::new(None),
        cancelled,
        status,
    });
    interpreter.register_task(task.clone());
    Ok(Value::Task(task))
}

fn wait_task(
    interpreter: &Interpreter,
    task: &TaskValue,
    timeout: Option<u64>,
    line: usize,
) -> Result<Value, RuntimeError> {
    if let Some(result) = task.result.borrow().clone() {
        return decode_portable(interpreter.portable_codec(), result?, line, "任务返回值");
    }
    let result = match interpreter.blocking_timeout(timeout, line)? {
        Some(timeout) => task
            .receiver
            .recv_timeout(timeout)
            .map_err(|_| interpreter.blocking_timeout_error(line, "任务等待超时"))?,
        None => task.receiver.recv().map_err(|e| error(e.to_string()))?,
    };
    *task.result.borrow_mut() = Some(result.clone());
    decode_portable(interpreter.portable_codec(), result?, line, "任务返回值")
}

fn cancel_task(task: &TaskValue) {
    task.cancelled.store(true, Ordering::Relaxed);
    task.status.store(2, Ordering::Relaxed);
}

fn wait_task_reclaimed(
    interpreter: &Interpreter,
    task: &TaskValue,
    line: usize,
) -> Result<Value, RuntimeError> {
    if let Some(result) = task.result.borrow().clone() {
        return decode_portable(interpreter.portable_codec(), result?, line, "任务返回值");
    }
    let result = task
        .receiver
        .recv()
        .map_err(|error| RuntimeError::new(line, error.to_string()))?;
    *task.result.borrow_mut() = Some(result.clone());
    decode_portable(interpreter.portable_codec(), result?, line, "任务返回值")
}

pub(crate) fn settle_task_group(
    interpreter: &Interpreter,
    tasks: &[Rc<TaskValue>],
    exit: TaskGroupExit,
    line: usize,
) -> Option<RuntimeError> {
    let mut cancelling = exit == TaskGroupExit::Cancel;
    let mut primary: Option<RuntimeError> = None;
    if cancelling {
        for task in tasks {
            cancel_task(task);
        }
    }

    for task in tasks {
        let waited = if cancelling {
            wait_task_reclaimed(interpreter, task, line)
        } else {
            wait_task(interpreter, task, None, line)
        };
        if let Err(error) = waited {
            let expected_cancellation = cancelling && error.message.contains("任务已经取消");
            if !expected_cancellation {
                if let Some(primary) = &mut primary {
                    primary.append_cause(error);
                } else {
                    primary = Some(error);
                }
            }
            if !cancelling {
                cancelling = true;
                for pending in tasks {
                    cancel_task(pending);
                }
                if task.result.borrow().is_none() {
                    if let Err(reclaim_error) = wait_task_reclaimed(interpreter, task, line) {
                        if !reclaim_error.message.contains("任务已经取消") {
                            if let Some(primary) = &mut primary {
                                primary.append_cause(reclaim_error);
                            } else {
                                primary = Some(reclaim_error);
                            }
                        }
                    }
                }
            }
        }
    }
    primary
}

fn parallel_map(
    interpreter: &Interpreter,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    line: usize,
) -> Result<Value, RuntimeError> {
    let codec = interpreter.portable_codec();
    let values = args[0]
        .iter_values_limited(interpreter.permissions.collection_limit())?
        .iter()
        .map(|value| encode_portable(codec, value, line, "并行映射输入"))
        .collect::<Result<Vec<_>, _>>()?;
    let spec = match &args[1] {
        Value::Function(function) => snapshot_function(function, codec, line)?,
        Value::BytecodeFunction(function) => snapshot_bytecode_function(function, codec, line)?,
        _ => return Err(error("并行映射需要一个功能").into()),
    };
    let workers = named
        .get("工作者")
        .map(Value::as_decimal)
        .transpose()?
        .map(|value| decimal_to_usize(value, "工作者数量"))
        .transpose()?
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, usize::from))
        .max(1)
        .min(values.len().max(1))
        .min(interpreter.permissions.parallel_worker_limit());
    let (work_sender, work_receiver) = crossbeam_channel::unbounded::<(usize, PortableValue)>();
    let (result_sender, result_receiver) = crossbeam_channel::unbounded();
    let mut permissions = interpreter.permissions.clone();
    permissions.execution_timeout_ms = interpreter
        .remaining_execution_time(line)?
        .map(|remaining| remaining.as_millis().min(u128::from(u64::MAX)) as u64);
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    let mut spawn_failure = None;
    for _ in 0..workers {
        let work_receiver = work_receiver.clone();
        let result_sender = result_sender.clone();
        let spec = spec.clone();
        let base_dir = interpreter.base_dir.clone();
        let permissions = permissions.clone();
        let worker_cancelled = cancelled.clone();
        let observation_session = crate::evidence::current_session();
        match crate::resource::execution_thread().spawn(move || {
            crate::evidence::with_session(observation_session, || {
                for (index, value) in work_receiver {
                    let result = run_spec(
                        spec.clone(),
                        vec![value],
                        base_dir.clone(),
                        permissions.clone(),
                        worker_cancelled.clone(),
                        line,
                    );
                    let _ = result_sender.send((index, result));
                }
            });
        }) {
            Ok(handle) => handles.push(handle),
            Err(error_value) => {
                spawn_failure = Some(error(format!("无法创建并行映射线程：{error_value}")));
                break;
            }
        }
    }
    drop(result_sender);
    if let Some(failure) = spawn_failure {
        cancelled.store(true, Ordering::Relaxed);
        drop(work_sender);
        for handle in handles {
            handle.join().map_err(|_| error("并行工作者异常结束"))?;
        }
        return Err(failure.into());
    }
    // 分发与收集拥有发送端，任何失败路径都会断开工作队列。
    let dispatch = move || -> Result<Vec<Value>, RuntimeError> {
        let total = values.len();
        for (index, value) in values.into_iter().enumerate() {
            work_sender
                .send((index, value))
                .map_err(|error_value| error(error_value.to_string()))?;
        }
        drop(work_sender);
        let mut results = vec![None; total];
        for _ in 0..total {
            let (index, result) = match interpreter.blocking_timeout(None, line)? {
                Some(timeout) => result_receiver
                    .recv_timeout(timeout)
                    .map_err(|_| interpreter.blocking_timeout_error(line, "并行映射等待超时")),
                None => result_receiver
                    .recv()
                    .map_err(|error_value| RuntimeError::new(line, error_value.to_string())),
            }?;
            results[index] = Some(decode_portable(codec, result?, line, "并行映射输出")?);
        }
        Ok(results.into_iter().flatten().collect())
    };
    let outcome = dispatch();
    if outcome.is_err() {
        cancelled.store(true, Ordering::Relaxed);
    }
    for handle in handles {
        handle.join().map_err(|_| error("并行工作者异常结束"))?;
    }
    Ok(Value::list(outcome?))
}

fn snapshot_function(
    function: &UserFunction,
    codec: PortableCodec,
    line: usize,
) -> Result<TaskFunction, RuntimeError> {
    Ok(TaskFunction::Source {
        name: function.name.clone(),
        params: function.params.clone(),
        body: function.body.clone(),
        captures: function.captures.clone(),
        semantic_bindings: function.semantic_bindings.clone(),
        semantic_types: function.semantic_types.clone(),
        semantic_methods: function.semantic_methods.clone(),
        self_binding: function.self_binding.clone(),
        captured: snapshot_env(&function.closure, &function.captures, codec, line)?,
    })
}

fn snapshot_bytecode_function(
    function: &crate::value::BytecodeClosure,
    codec: PortableCodec,
    line: usize,
) -> Result<TaskFunction, RuntimeError> {
    let captures = function
        .program
        .functions
        .get(function.function as usize)
        .ok_or_else(|| RuntimeError::new(line, "任务功能字节码索引越界".into()))?
        .captures
        .clone();
    Ok(TaskFunction::Bytecode {
        name: function.name.clone(),
        function: function.function,
        program: function.program.as_ref().clone(),
        captured: snapshot_env(&function.closure, &captures, codec, line)?,
    })
}

fn snapshot_env(
    env: &EnvRef,
    captures: &[CapturedBinding],
    codec: PortableCodec,
    line: usize,
) -> Result<BTreeMap<BindingId, (String, PortableValue)>, RuntimeError> {
    let mut values = if let Some(parent) = env.borrow().parent.clone() {
        snapshot_env(&parent, captures, codec, line)?
    } else {
        BTreeMap::new()
    };
    for (binding, (name, value)) in &env.borrow().resolved_values {
        if !captures.iter().any(|capture| capture.id == *binding) {
            continue;
        }
        match value {
            Value::Native(_) => {}
            value => {
                let value = encode_portable(codec, value, line, &format!("任务捕获变量“{name}”"))?;
                values.insert(*binding, (name.clone(), value));
            }
        }
    }
    Ok(values)
}

fn run_spec(
    spec: TaskFunction,
    args: Vec<PortableValue>,
    base_dir: PathBuf,
    permissions: PermissionSet,
    cancelled: Arc<AtomicBool>,
    call_line: usize,
) -> Result<PortableValue, RuntimeError> {
    // Worker threads do not inherit thread-local authority from the parent.
    // Install the cloned run policy for the duration of this task only.
    crate::privacy::install_endpoint_policy(permissions.endpoint_policy());
    let codec = PortableCodec::new(
        permissions.collection_limit(),
        permissions.value_nesting_limit(),
    );
    if let TaskFunction::Bytecode {
        name,
        function,
        program,
        captured,
    } = spec
    {
        let result = crate::vm::execute_function(crate::vm::BytecodeInvocation {
            program,
            function,
            name,
            captured,
            args,
            base_dir,
            permissions,
            cancelled,
            call_line,
        });
        crate::privacy::clear_endpoint_policy();
        return result;
    }
    let TaskFunction::Source {
        name,
        params,
        body,
        captures,
        semantic_bindings,
        semantic_types,
        semantic_methods,
        self_binding,
        captured,
    } = spec
    else {
        return Err(RuntimeError::new(call_line, "任务功能类型无效".into()));
    };
    let mut interpreter = Interpreter::new(base_dir, false);
    interpreter.permissions = permissions;
    interpreter.set_cancellation(cancelled);
    interpreter.semantic_bindings = semantic_bindings.clone();
    interpreter.semantic_types = semantic_types.clone();
    interpreter.semantic_methods = semantic_methods.clone();
    for (binding, (name, value)) in captured {
        let value = decode_portable(codec, value, call_line, "任务捕获值")?;
        interpreter
            .env
            .borrow_mut()
            .resolved_values
            .insert(binding, (name, value));
    }
    let function = Rc::new(UserFunction {
        name: name.clone(),
        params,
        body,
        closure: interpreter.env.clone(),
        captures,
        semantic_bindings,
        semantic_types,
        semantic_methods,
        self_binding: self_binding.clone(),
    });
    if let Some(binding) = self_binding {
        interpreter.env.borrow_mut().resolved_values.insert(
            binding.id,
            (binding.name, Value::Function(function.clone())),
        );
    }
    let args = args
        .into_iter()
        .map(|value| decode_portable(codec, value, call_line, "任务参数"))
        .collect::<Result<Vec<_>, _>>()?;
    let result =
        interpreter.invoke_function_structured(Value::Function(function), args, call_line)?;
    let encoded = encode_portable(codec, &result, call_line, "任务返回值");
    crate::privacy::clear_endpoint_policy();
    encoded
}

fn error(message: impl AsRef<str>) -> String {
    message.as_ref().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{StructInstance, StructType};

    #[test]
    fn every_concurrency_boundary_uses_the_same_portable_failure() {
        let value = Value::StructInstance(Rc::new(RefCell::new(StructInstance {
            struct_type: Rc::new(StructType {
                id: crate::ast::TypeId {
                    module: 1,
                    local: 1,
                },
                name: "客户".into(),
                fields: vec!["姓名".into()],
                methods: RefCell::new(BTreeMap::new()),
                method_names: RefCell::new(BTreeMap::new()),
                module_id: 1,
            }),
            fields: BTreeMap::from([("姓名".into(), Value::Text("甲".into()))]),
        })));
        let codec = PortableCodec::new(32, 8);
        let contexts = ["通道发送值", "任务参数", "任务返回值", "并行映射输入"];
        let errors = contexts.map(|context| {
            encode_portable(codec, &value, 7, context).expect_err("结构实例应被拒绝")
        });

        for error in &errors {
            assert_eq!(error.category, "并发错误");
            assert_eq!(error.span.start.line, 7);
            assert!(error.message.contains("结构实例暂不支持跨任务传递"));
        }
    }
}
