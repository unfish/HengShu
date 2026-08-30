//! Bytecode machine, frames, and instruction execution.

use super::*;
pub(super) struct Machine<'a> {
    pub(super) interpreter: &'a mut Interpreter,
    pub(super) modules: Rc<VmModuleState>,
    pub(super) frames: Vec<CallFrame>,
    pub(super) result: Option<Value>,
}

pub(super) struct CallFrame {
    program: Rc<BytecodeProgram>,
    function: u32,
    ip: usize,
    stack: Vec<Value>,
    internals: BTreeMap<u32, Value>,
    env: EnvRef,
    iterators: Vec<IteratorState>,
    supplied: Vec<Value>,
    named: BTreeMap<String, Value>,
    next_param: usize,
    return_target: ReturnTarget,
    pending_completions: Vec<PendingCompletion>,
    environment_depth: usize,
    call_line: usize,
    depth_registered: bool,
}

enum PendingCompletion {
    Resume {
        return_ip: usize,
        value: Option<Value>,
        block_start: usize,
        reason: CompletionReason,
    },
    Throw(RuntimeError),
}

struct IteratorState {
    values: Vec<Value>,
    index: usize,
    base_env: EnvRef,
    base_environment_depth: usize,
}

pub(super) enum ReturnTarget {
    Root,
    Caller,
    BindDefault {
        binding: BindingOperand,
        name_span: crate::diagnostic::SourceSpan,
        call_line: usize,
    },
    Module {
        module_id: String,
        display_name: String,
        bindings: Vec<BindingOperand>,
        selective: bool,
        importer: EnvRef,
        import_line: usize,
    },
}

impl CallFrame {
    pub(super) fn main(program: Rc<BytecodeProgram>, env: EnvRef) -> Self {
        Self {
            program,
            function: 0,
            ip: 0,
            stack: Vec::new(),
            internals: BTreeMap::new(),
            env,
            iterators: Vec::new(),
            supplied: Vec::new(),
            named: BTreeMap::new(),
            next_param: 0,
            return_target: ReturnTarget::Root,
            pending_completions: Vec::new(),
            environment_depth: 0,
            call_line: 0,
            depth_registered: false,
        }
    }

    pub(super) fn closure(
        closure: Rc<BytecodeClosure>,
        env: EnvRef,
        supplied: Vec<Value>,
        named: BTreeMap<String, Value>,
        call_line: usize,
        return_target: ReturnTarget,
    ) -> Result<Self, RuntimeError> {
        let prototype = closure
            .program
            .functions
            .get(closure.function as usize)
            .ok_or_else(|| RuntimeError::new(call_line, "字节码功能索引越界".into()))?;
        crate::contracts::validate_call_shape(
            &closure.name,
            prototype.params.iter().map(|parameter| {
                (
                    parameter.binding.name.as_str(),
                    parameter.default_function.is_some(),
                )
            }),
            supplied.len(),
            named.keys().map(String::as_str),
            call_line,
        )
        .map_err(|error| {
            let definition_span = error.parameter.as_deref().and_then(|name| {
                prototype
                    .params
                    .iter()
                    .find(|parameter| parameter.binding.name == name)
                    .map(|parameter| parameter.name_span)
            });
            RuntimeError::new(call_line, error.message).with_definition_span(definition_span)
        })?;
        Ok(Self {
            program: closure.program.clone(),
            function: closure.function,
            ip: 0,
            stack: Vec::new(),
            internals: BTreeMap::new(),
            env,
            iterators: Vec::new(),
            supplied,
            named,
            next_param: 0,
            return_target,
            pending_completions: Vec::new(),
            environment_depth: 0,
            call_line,
            depth_registered: false,
        })
    }

    fn prototype(&self) -> &crate::bytecode::FunctionPrototype {
        &self.program.functions[self.function as usize]
    }
}

impl Machine<'_> {
    pub(super) fn run(&mut self) -> Result<(), RuntimeError> {
        while !self.frames.is_empty() {
            if let Err(error) = self.initialize_frame() {
                let fault = self.frames.last().map_or(0, |frame| frame.ip);
                self.handle_error(error, fault)?;
                continue;
            }
            if self
                .frames
                .last()
                .is_some_and(|frame| frame.next_param < frame.prototype().params.len())
            {
                continue;
            }

            let (instruction, line, fault_ip) = {
                let frame = self.frames.last_mut().ok_or("调用帧栈下溢".to_string())?;
                let prototype = frame.prototype();
                if frame.ip >= prototype.instructions.len() {
                    let line = prototype
                        .source_lines
                        .last()
                        .copied()
                        .map(|line| line as usize)
                        .unwrap_or(frame.call_line);
                    return Err(RuntimeError::new(line, "字节码指令指针越界".into()));
                }
                let fault_ip = frame.ip;
                let instruction = prototype.instructions[fault_ip].clone();
                let line = prototype.source_lines[fault_ip] as usize;
                frame.ip += 1;
                (instruction, line, fault_ip)
            };
            self.interpreter
                .record_event(line, "instruction", "执行指令")?;
            if let Err(error) = self.step(instruction, line) {
                self.handle_error(error.with_fallback_line(line), fault_ip)?;
            }
        }
        Ok(())
    }

    pub(super) fn push_frame(
        &mut self,
        mut frame: CallFrame,
        name: String,
        line: usize,
    ) -> Result<(), RuntimeError> {
        self.interpreter.enter_call(name, line)?;
        frame.depth_registered = true;
        self.frames.push(frame);
        Ok(())
    }

    fn pop_frame(&mut self) -> Option<CallFrame> {
        let frame = self.frames.pop()?;
        if frame.depth_registered {
            self.interpreter.leave_call();
        }
        Some(frame)
    }

    fn initialize_frame(&mut self) -> Result<(), RuntimeError> {
        loop {
            let Some(frame) = self.frames.last_mut() else {
                return Ok(());
            };
            let function_index = frame.function as usize;
            let prototype = frame.program.functions[function_index].clone();
            if frame.next_param >= prototype.params.len() {
                return Ok(());
            }
            let parameter = prototype.params[frame.next_param].clone();
            let value = frame.supplied.get(frame.next_param).cloned();
            let value = value.or_else(|| frame.named.remove(&parameter.binding.name));
            frame.next_param += 1;
            if let Some(value) = value {
                frame
                    .env
                    .borrow_mut()
                    .resolved_values
                    .insert(parameter.binding.id, (parameter.binding.name, value));
                continue;
            }
            let Some(default_function) = parameter.default_function else {
                return Err(RuntimeError::new(
                    frame.call_line,
                    format!(
                        "功能“{}”缺少参数“{}”。",
                        prototype.name, parameter.binding.name
                    ),
                )
                .with_definition_span(Some(parameter.name_span)));
            };
            let closure = Rc::new(BytecodeClosure {
                name: format!("{} 的默认参数 {}", prototype.name, parameter.binding.name),
                function: default_function,
                program: frame.program.clone(),
                closure: frame.env.clone(),
            });
            let call_line = frame.call_line;
            let env = self
                .interpreter
                .new_env(Some(closure.closure.clone()), true);
            self.frames.push(CallFrame::closure(
                closure,
                env,
                Vec::new(),
                BTreeMap::new(),
                call_line,
                ReturnTarget::BindDefault {
                    binding: parameter.binding,
                    name_span: parameter.name_span,
                    call_line,
                },
            )?);
            return Ok(());
        }
    }

    fn step(&mut self, instruction: Instruction, line: usize) -> Result<(), RuntimeError> {
        self.interpreter
            .check_operation_limits()
            .map_err(|message| RuntimeError::new(line, message).with_category("资源错误"))?;
        match instruction {
            Instruction::PushConstant(index) => {
                let constant = self
                    .frames
                    .last()
                    .and_then(|frame| frame.program.constants.get(index as usize))
                    .cloned()
                    .ok_or_else(|| vm_error("常量索引越界"))?;
                self.push(constant_value(&constant))?;
            }
            Instruction::LoadName(binding, name) => {
                let env = self.current_env()?;
                self.push(Env::get_resolved(&env, binding.as_ref(), &name)?)?;
            }
            Instruction::LoadInternal(slot) => {
                let value = self
                    .frames
                    .last()
                    .and_then(|frame| frame.internals.get(&slot))
                    .cloned()
                    .ok_or_else(|| vm_error("内部临时槽尚未初始化"))?;
                self.push(value)?;
            }
            Instruction::StoreInternal(slot) => {
                let value = self.pop()?;
                self.current_frame_mut()?.internals.insert(slot, value);
            }
            Instruction::Define {
                binding,
                type_constraint,
            } => {
                let value = self.pop()?;
                let env = self.current_env()?;
                Env::define_resolved(&env, &binding, value, type_constraint.as_ref())?;
            }
            Instruction::AssignName(binding, name) => {
                let value = self.pop()?;
                let env = self.current_env()?;
                Env::assign_resolved(&env, binding.as_ref(), &name, value)?;
            }
            Instruction::Pop => {
                self.pop()?;
            }
            Instruction::Dup => {
                let value = self.pop()?;
                self.push(value.clone())?;
                self.push(value)?;
            }
            Instruction::BuildList(count) => {
                let values = self.pop_values(count as usize)?;
                let value = Value::list(values);
                self.interpreter.check_value_limit(&value, line)?;
                self.push(value)?;
            }
            Instruction::BuildRecord(keys) => {
                let values = self.pop_values(keys.len())?;
                let value = Value::record(keys.into_iter().zip(values).collect());
                self.interpreter.check_value_limit(&value, line)?;
                self.push(value)?;
            }
            Instruction::Unary(op) => {
                let value = self.pop()?;
                let result = self.interpreter.vm_unary(&op, value, line)?;
                self.push(result)?;
            }
            Instruction::Binary(op) => {
                let right = self.pop()?;
                let left = self.pop()?;
                let result = self.interpreter.vm_binary(&op, left, right)?;
                self.push(result)?;
            }
            Instruction::ToBool => {
                let value = self.pop()?;
                self.push(Value::Bool(value.truth()))?;
            }
            Instruction::IsType(expected) => {
                let value = self.pop()?;
                let env = self.current_env()?;
                self.push(Value::Bool(Env::value_matches_type(
                    &env, &value, &expected,
                )?))?;
            }
            Instruction::GetAttr(name) => {
                let value = self.pop()?;
                let result = self.interpreter.vm_get_attr(&value, &name)?;
                self.push(result)?;
            }
            Instruction::GetIndex => {
                let index = self.pop()?;
                let value = self.pop()?;
                let result = self.interpreter.vm_get_index(&value, &index)?;
                self.push(result)?;
            }
            Instruction::SetAttr(name) => {
                let object = self.pop()?;
                let value = self.pop()?;
                self.interpreter.vm_set_attr(object, &name, value)?;
            }
            Instruction::SetIndex => {
                let index = self.pop()?;
                let object = self.pop()?;
                let value = self.pop()?;
                self.interpreter.vm_set_index(object, index, value)?;
            }
            Instruction::Jump(target) => self.current_frame_mut()?.ip = target as usize,
            Instruction::Break { target, .. } | Instruction::Continue { target, .. } => {
                let target = target as usize;
                let frame = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?;
                // 跳出或继续会放弃仍在执行的清理块，其恢复点不再有效；留下来会被外层
                // 清理块的完成指令误当成自己的待恢复完成原因。
                let abandoned = frame
                    .prototype()
                    .cleanup_blocks
                    .iter()
                    .filter(|block| {
                        !(block.start as usize <= target && target < block.end as usize)
                    })
                    .map(|block| block.start as usize)
                    .collect::<Vec<_>>();
                frame
                    .pending_completions
                    .retain(|completion| match completion {
                        PendingCompletion::Resume { block_start, .. } => {
                            !abandoned.contains(block_start)
                        }
                        PendingCompletion::Throw(_) => true,
                    });
                frame.ip = target;
            }
            Instruction::JumpIfFalse(target) => {
                if !self.pop()?.truth() {
                    self.current_frame_mut()?.ip = target as usize;
                }
            }
            Instruction::JumpIfTrue(target) => {
                if self.pop()?.truth() {
                    self.current_frame_mut()?.ip = target as usize;
                }
            }
            Instruction::StartIterator => {
                let values = self
                    .pop()?
                    .iter_values_limited(self.interpreter.permissions.collection_limit())?;
                let frame = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?;
                frame.iterators.push(IteratorState {
                    values,
                    index: 0,
                    base_env: frame.env.clone(),
                    base_environment_depth: frame.environment_depth,
                });
            }
            Instruction::NextIterator { binding, end } => {
                let frame = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?;
                let iterator = frame
                    .iterators
                    .last_mut()
                    .ok_or_else(|| vm_error("迭代器栈下溢"))?;
                frame.env = iterator.base_env.clone();
                frame.environment_depth = iterator.base_environment_depth;
                if let Some(value) = iterator.values.get(iterator.index).cloned() {
                    iterator.index += 1;
                    let env = self
                        .interpreter
                        .new_env(Some(iterator.base_env.clone()), false);
                    Env::define_resolved(&env, &binding, value, None)?;
                    frame.env = env;
                    frame.environment_depth = iterator.base_environment_depth + 1;
                } else {
                    frame.ip = end as usize;
                }
            }
            Instruction::EndIterator => {
                let frame = self.current_frame_mut()?;
                let iterator = frame
                    .iterators
                    .pop()
                    .ok_or_else(|| vm_error("迭代器栈下溢"))?;
                frame.env = iterator.base_env;
                frame.environment_depth = iterator.base_environment_depth;
            }
            Instruction::EnterScope => {
                let parent = self.current_env()?;
                let env = self.interpreter.new_env(Some(parent), false);
                let frame = self.current_frame_mut()?;
                frame.env = env;
                frame.environment_depth += 1;
            }
            Instruction::ExitScope => {
                let parent = self
                    .frames
                    .last()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?
                    .env
                    .borrow()
                    .parent
                    .clone()
                    .ok_or_else(|| vm_error("作用域栈下溢"))?;
                let frame = self.current_frame_mut()?;
                frame.env = parent;
                frame.environment_depth = frame
                    .environment_depth
                    .checked_sub(1)
                    .ok_or_else(|| vm_error("作用域深度下溢"))?;
            }
            Instruction::MakeClosure(function) => {
                let (name, program, closure) = {
                    let frame = self.frames.last().ok_or_else(|| vm_error("调用帧栈下溢"))?;
                    let prototype = frame
                        .program
                        .functions
                        .get(function as usize)
                        .ok_or_else(|| vm_error("功能索引越界"))?;
                    (
                        prototype.name.clone(),
                        frame.program.clone(),
                        frame.env.clone(),
                    )
                };
                self.push(Value::BytecodeFunction(Rc::new(BytecodeClosure {
                    name,
                    function,
                    program,
                    closure,
                })))?;
            }
            Instruction::Call {
                method,
                positional,
                named,
            } => {
                let named_values = self.pop_values(named.len())?;
                let named = named.into_iter().zip(named_values).collect();
                let args = self.pop_values(positional as usize)?;
                let receiver = self.pop()?;
                self.call(receiver, method.as_ref(), args, named, line)?;
            }
            Instruction::Return => {
                let value = self.pop()?;
                self.finish_frame(value)?;
            }
            Instruction::DefineMethod {
                owner,
                method,
                public,
            } => {
                let value = self.pop()?;
                let env = self.current_env()?;
                let struct_type = Env::get_struct_type(&env, &owner)?;
                let method_id = method
                    .candidates
                    .iter()
                    .find(|id| id.owner == struct_type.id)
                    .copied()
                    .ok_or_else(|| vm_error("方法缺少与所属类型一致的语义身份"))?;
                struct_type
                    .method_names
                    .borrow_mut()
                    .insert(method.name, method_id);
                struct_type.methods.borrow_mut().insert(
                    method_id,
                    crate::value::UserMethod {
                        value: value.clone(),
                        public,
                    },
                );
            }
            Instruction::DefineStruct {
                binding,
                type_id,
                fields,
            } => {
                let env = self.current_env()?;
                let module_id = env.borrow().module_id;
                Env::define_resolved(
                    &env,
                    &binding,
                    Value::StructType(Rc::new(StructType {
                        id: type_id,
                        name: binding.name.clone(),
                        fields,
                        methods: std::cell::RefCell::new(BTreeMap::new()),
                        method_names: std::cell::RefCell::new(BTreeMap::new()),
                        module_id,
                    })),
                    None,
                )?;
            }
            Instruction::Import {
                module,
                bindings,
                selective,
            } => self.import(module, bindings, selective, line)?,
            Instruction::DeclareGlobal(bindings) => {
                let env = self
                    .frames
                    .last()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?
                    .env
                    .clone();
                Env::declare_global_bindings(&env, &bindings);
            }
            Instruction::Require {
                type_constraint,
                has_message,
            } => {
                let message = has_message.then(|| self.pop()).transpose()?;
                let value = self.pop()?;
                let passed = type_constraint
                    .as_ref()
                    .map(|expected| {
                        let env = self.current_env()?;
                        Env::value_matches_type(&env, &value, expected)
                    })
                    .transpose()?
                    .unwrap_or_else(|| value.truth());
                if !passed {
                    let detail = message.map(|value| value.to_string()).unwrap_or_else(|| {
                        type_constraint
                            .map(|expected| {
                                format!(
                                    "要求值是{}，实际是{}。",
                                    expected.name,
                                    value.actual_type_display(&expected.name)
                                )
                            })
                            .unwrap_or_else(|| "要求的条件没有满足。".into())
                    });
                    return Err(vm_error(&detail).into());
                }
            }
            Instruction::Throw => {
                let value = self.pop()?;
                return Err(match value {
                    Value::Error(issue) => RuntimeError::from_issue(issue.as_ref(), line),
                    value => RuntimeError::new(line, value.to_string()),
                });
            }
            Instruction::BindException(name) => {
                let completion = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?
                    .pending_completions
                    .pop()
                    .ok_or_else(|| vm_error("没有待处理异常"))?;
                let PendingCompletion::Throw(error) = completion else {
                    return Err(vm_error("捕获目标不是抛出完成原因").into());
                };
                if let Some(binding) = name {
                    let stack = self
                        .frames
                        .iter()
                        .map(|frame| frame.prototype().name.clone())
                        .collect();
                    let issue = self.interpreter.vm_issue_from_runtime_error(error, stack);
                    let env = self.current_env()?;
                    Env::define_resolved(&env, &binding, Value::Error(Rc::new(issue)), None)?;
                }
            }
            Instruction::CallCleanup { target, reason } => {
                let value = matches!(reason, CompletionReason::Return)
                    .then(|| self.pop())
                    .transpose()?;
                let frame = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?;
                frame.pending_completions.push(PendingCompletion::Resume {
                    return_ip: frame.ip,
                    value,
                    block_start: target as usize,
                    reason,
                });
                frame.ip = target as usize;
            }
            Instruction::FinishCleanup => {
                let completion = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?
                    .pending_completions
                    .pop()
                    .ok_or_else(|| vm_error("最终块没有待恢复的完成原因"))?;
                if let PendingCompletion::Resume {
                    return_ip,
                    value,
                    reason,
                    ..
                } = completion
                {
                    let frame = self
                        .frames
                        .last_mut()
                        .ok_or_else(|| vm_error("调用帧栈下溢"))?;
                    frame.ip = return_ip;
                    if matches!(reason, CompletionReason::Return) {
                        let value = value.ok_or_else(|| vm_error("返回完成原因缺少返回值"))?;
                        self.push(value)?;
                    }
                } else if let PendingCompletion::Throw(error) = completion {
                    return Err(error);
                }
            }
            Instruction::Rethrow => {
                let completion = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| vm_error("调用帧栈下溢"))?
                    .pending_completions
                    .pop()
                    .ok_or_else(|| vm_error("最终块没有待传播异常"))?;
                let PendingCompletion::Throw(error) = completion else {
                    return Err(vm_error("重抛目标不是抛出完成原因").into());
                };
                return Err(error);
            }
            Instruction::BeginTaskGroup => self.interpreter.begin_task_group(),
            Instruction::FinishTaskGroup => {
                self.interpreter.vm_finish_task_group(line)?;
            }
            Instruction::CancelTaskGroup => {
                if let Some(cleanup_error) = self.interpreter.vm_cancel_task_group(line) {
                    let frame = self
                        .frames
                        .last_mut()
                        .ok_or_else(|| vm_error("调用帧栈下溢"))?;
                    if let Some(primary) =
                        frame
                            .pending_completions
                            .iter_mut()
                            .rev()
                            .find_map(|completion| match completion {
                                PendingCompletion::Throw(error) => Some(error),
                                PendingCompletion::Resume { .. } => None,
                            })
                    {
                        primary.append_cause(cleanup_error);
                    }
                }
            }
            Instruction::Halt => self.finish_frame(Value::Null)?,
        }
        Ok(())
    }

    fn call(
        &mut self,
        receiver: Value,
        method: Option<&crate::ast::MethodOperand>,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
    ) -> Result<(), RuntimeError> {
        if let Some(method_operand) = method {
            let name = method_operand.name.as_str();
            if let Value::StructInstance(instance) = &receiver {
                let method_id = {
                    let instance = instance.borrow();
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
                    instance
                        .borrow()
                        .struct_type
                        .methods
                        .borrow()
                        .get(&method_id)
                        .cloned()
                });
                if let Some(method) = method {
                    let caller_module = self.current_env()?.borrow().module_id;
                    if !method.public && instance.borrow().struct_type.module_id != caller_module {
                        return Err(
                            vm_error(&format!("结构方法“{name}”仅限所属模块内部访问。")).into()
                        );
                    }
                    let mut with_self = vec![receiver];
                    with_self.extend(args);
                    return self.call(method.value, None, with_self, named, line);
                }
            }
            if let Value::Module(module) = &receiver {
                let function = module
                    .exports
                    .get(name)
                    .cloned()
                    .ok_or_else(|| vm_error(&format!("模块“{}”没有“{name}”。", module.name)))?;
                return self.call(function, None, args, named, line);
            }
            let result =
                self.interpreter
                    .vm_call_method(receiver, method_operand, args, named, line)?;
            self.push(result)?;
            return Ok(());
        }

        match receiver {
            Value::BytecodeFunction(closure) => {
                let name = closure.name.clone();
                let env = self
                    .interpreter
                    .new_env(Some(closure.closure.clone()), true);
                let frame =
                    CallFrame::closure(closure, env, args, named, line, ReturnTarget::Caller)?;
                self.push_frame(frame, name, line)?;
            }
            value => {
                let env = self.current_env()?;
                let result = self
                    .interpreter
                    .vm_call_value_in_env(value, args, named, line, &env)?;
                self.push(result)?;
            }
        }
        Ok(())
    }

    fn import(
        &mut self,
        name: String,
        bindings: Vec<BindingOperand>,
        selective: bool,
        line: usize,
    ) -> Result<(), RuntimeError> {
        let module_id = self
            .frames
            .last()
            .and_then(|frame| frame.program.module_imports.get(&name))
            .cloned();
        if let Some(module) = module_id
            .as_ref()
            .and_then(|id| self.modules.loaded.borrow().get(id).cloned())
            .or_else(|| self.interpreter.vm_builtin_module(&name))
        {
            let env = self.current_env()?;
            bind_module(&env, &name, &bindings, selective, module)?;
            return Ok(());
        }
        let module_id =
            module_id.ok_or_else(|| vm_error(&format!("字节码缺少模块“{name}”的身份映射。")))?;
        let loading = self
            .frames
            .iter()
            .filter_map(|frame| match &frame.return_target {
                ReturnTarget::Module {
                    module_id,
                    display_name,
                    ..
                } => Some((module_id.clone(), display_name.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        if let Some(index) = loading.iter().position(|(id, _)| id == &module_id) {
            let mut cycle = loading[index..]
                .iter()
                .map(|(_, display)| display.clone())
                .collect::<Vec<_>>();
            cycle.push(name.clone());
            return Err(
                RuntimeError::new(line, format!("模块循环依赖：{}", cycle.join(" -> ")))
                    .with_category("模块错误"),
            );
        }
        if loading.len() >= self.interpreter.permissions.module_depth_limit() {
            return Err(RuntimeError::new(line, "模块加载深度超过资源限制".into())
                .with_category("资源错误"));
        }
        let program = self
            .modules
            .programs
            .get(&module_id)
            .cloned()
            .ok_or_else(|| RuntimeError::new(line, format!("找不到内嵌模块“{name}”。")))?;
        program.verify().map_err(|error| {
            RuntimeError::new(line, format!("模块“{name}”字节码无效：{error}"))
                .with_category("模块错误")
        })?;
        let importer = self.current_env()?;
        let module_env = self
            .interpreter
            .new_module_env(Some(self.interpreter.env.clone()));
        let mut frame = CallFrame::main(program, module_env);
        frame.return_target = ReturnTarget::Module {
            module_id,
            display_name: name,
            bindings,
            selective,
            importer,
            import_line: line,
        };
        self.frames.push(frame);
        Ok(())
    }

    fn finish_frame(&mut self, value: Value) -> Result<(), RuntimeError> {
        let frame = self.pop_frame().ok_or("调用帧栈下溢")?;
        match frame.return_target {
            ReturnTarget::Root => self.result = Some(value),
            ReturnTarget::Caller => self.push(value)?,
            ReturnTarget::BindDefault { binding, .. } => {
                let parent = self.frames.last().ok_or("默认参数缺少调用帧")?;
                parent
                    .env
                    .borrow_mut()
                    .resolved_values
                    .insert(binding.id, (binding.name, value));
            }
            ReturnTarget::Module {
                module_id,
                display_name,
                bindings,
                selective,
                importer,
                import_line,
            } => {
                let exports = frame
                    .program
                    .exports
                    .iter()
                    .filter_map(|binding| {
                        frame
                            .env
                            .borrow()
                            .resolved_values
                            .get(&binding.id)
                            .map(|(_, value)| (binding.name.clone(), value.clone()))
                    })
                    .collect();
                let module = Rc::new(Module::new(
                    display_name.clone(),
                    exports,
                    Some(frame.env.clone()),
                ));
                self.modules
                    .loaded
                    .borrow_mut()
                    .insert(module_id, module.clone());
                bind_module(&importer, &display_name, &bindings, selective, module).map_err(
                    |message| RuntimeError::from(message).with_fallback_line(import_line),
                )?;
            }
        }
        Ok(())
    }

    fn handle_error(
        &mut self,
        mut error: RuntimeError,
        mut fault_ip: usize,
    ) -> Result<(), RuntimeError> {
        if error.stack.is_empty() {
            error.stack = self
                .frames
                .iter()
                .filter_map(|frame| {
                    let name = &frame.prototype().name;
                    (name != "主程序").then(|| name.clone())
                })
                .collect();
        }
        loop {
            let Some(frame) = self.frames.last_mut() else {
                return Err(error);
            };
            let handler = frame
                .prototype()
                .exception_handlers
                .iter()
                .filter(|handler| {
                    handler.start as usize <= fault_ip && fault_ip < handler.end as usize
                })
                .min_by_key(|handler| handler.end - handler.start)
                .cloned();
            if let Some(handler) = handler {
                let handler_inside_active_cleanup = frame
                    .pending_completions
                    .iter()
                    .rev()
                    .find_map(|completion| match completion {
                        PendingCompletion::Resume { block_start, .. } => Some(*block_start),
                        PendingCompletion::Throw(_) => None,
                    })
                    .is_some_and(|block_start| {
                        frame.prototype().cleanup_blocks.iter().any(|block| {
                            block.start as usize == block_start
                                && block.start <= handler.target
                                && handler.target < block.end
                        })
                    });
                if !handler_inside_active_cleanup {
                    frame
                        .pending_completions
                        .retain(|completion| matches!(completion, PendingCompletion::Throw(_)));
                }
                let target_depth = handler.environment_depth as usize;
                while frame.environment_depth > target_depth {
                    let parent = frame
                        .env
                        .borrow()
                        .parent
                        .clone()
                        .ok_or_else(|| vm_error("异常处理时作用域栈下溢"))?;
                    frame.env = parent;
                    frame.environment_depth -= 1;
                }
                while frame
                    .iterators
                    .last()
                    .is_some_and(|iterator| iterator.base_environment_depth + 1 > target_depth)
                {
                    frame.iterators.pop();
                }
                frame.stack.clear();
                frame
                    .pending_completions
                    .push(PendingCompletion::Throw(error));
                frame.ip = handler.target as usize;
                return Ok(());
            }
            frame
                .pending_completions
                .retain(|completion| matches!(completion, PendingCompletion::Throw(_)));
            if error.cause.is_none() {
                if let Some(PendingCompletion::Throw(original)) = frame.pending_completions.pop() {
                    error = error.with_cause(original);
                }
            }
            if let ReturnTarget::BindDefault {
                name_span,
                call_line,
                ..
            } = frame.return_target
            {
                let mut stack = error.stack.into_vec();
                stack.retain(|name| !name.contains(" 的默认参数 "));
                error.stack = stack.into_boxed_slice();
                error = error.at_parameter_call_site(call_line, name_span);
            }
            self.pop_frame();
            let Some(caller) = self.frames.last() else {
                return Err(error);
            };
            fault_ip = caller.ip.saturating_sub(1);
        }
    }

    fn current_frame_mut(&mut self) -> Result<&mut CallFrame, String> {
        self.frames
            .last_mut()
            .ok_or_else(|| vm_error("调用帧栈下溢"))
    }

    fn current_env(&self) -> Result<EnvRef, String> {
        self.frames
            .last()
            .map(|frame| frame.env.clone())
            .ok_or_else(|| vm_error("调用帧栈下溢"))
    }

    fn push(&mut self, value: Value) -> Result<(), String> {
        self.current_frame_mut()?.stack.push(value);
        Ok(())
    }

    fn pop(&mut self) -> Result<Value, String> {
        self.frames
            .last_mut()
            .and_then(|frame| frame.stack.pop())
            .ok_or_else(|| vm_error("栈下溢"))
    }

    fn pop_values(&mut self, count: usize) -> Result<Vec<Value>, String> {
        let frame = self
            .frames
            .last_mut()
            .ok_or_else(|| vm_error("调用帧栈下溢"))?;
        if frame.stack.len() < count {
            return Err(vm_error("栈下溢"));
        }
        Ok(frame.stack.split_off(frame.stack.len() - count))
    }
}
