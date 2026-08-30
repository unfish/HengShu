use crate::ast::{
    BindingOperand, Expr, Literal, LoopId, MatchPattern, MethodOperand, Param, Program,
    SemanticFunction, SemanticMethod, SemanticType, Stmt, SymbolId, TypeOperand,
};
use crate::bytecode::{
    BytecodeProgram, CleanupBlock, CompletionReason, Constant, ExceptionHandler, FunctionPrototype,
    HandlerKind, Instruction, LoopTarget, Parameter, BYTECODE_VERSION,
};
use crate::diagnostic::{Diagnostic, DiagnosticStage, SourceSpan};
use std::collections::BTreeMap;

pub fn compile(program: &Program) -> Result<BytecodeProgram, String> {
    compile_diagnostic(program).map_err(|diagnostic| diagnostic.to_string())
}

pub fn compile_module(program: &Program) -> Result<BytecodeProgram, String> {
    compile_module_diagnostic(program).map_err(|diagnostic| diagnostic.to_string())
}

pub fn compile_diagnostic(program: &Program) -> Result<BytecodeProgram, Diagnostic> {
    Compiler::new().finish(program, false)
}

pub fn compile_module_diagnostic(program: &Program) -> Result<BytecodeProgram, Diagnostic> {
    Compiler::new().finish(program, true)
}

struct Compiler {
    constants: Vec<Constant>,
    functions: Vec<FunctionPrototype>,
    temporary: usize,
    next_cleanup: u32,
    semantic_functions: Vec<SemanticFunction>,
    semantic_bindings: Vec<(SourceSpan, bool, BindingOperand)>,
    semantic_types: Vec<SemanticType>,
    semantic_methods: Vec<SemanticMethod>,
    error: Option<Diagnostic>,
}

#[derive(Default)]
struct FunctionCompiler {
    name: String,
    self_binding: Option<BindingOperand>,
    params: Vec<Parameter>,
    captures: Vec<crate::ast::CapturedBinding>,
    instructions: Vec<Instruction>,
    lines: Vec<u32>,
    handlers: Vec<ExceptionHandler>,
    cleanup_blocks: Vec<CleanupBlock>,
    loop_targets: Vec<LoopTarget>,
    loops: Vec<LoopContext>,
    cleanups: Vec<Cleanup>,
    cleanup_patches: BTreeMap<u32, Vec<usize>>,
    environment_depth: usize,
}

#[derive(Clone)]
enum Cleanup {
    Finally { id: u32 },
    TaskGroup,
    Scope,
}

struct LoopContext {
    id: LoopId,
    break_jumps: Vec<usize>,
    continue_target: usize,
    cleanup_depth: usize,
}

impl Compiler {
    fn new() -> Self {
        Self {
            constants: Vec::new(),
            functions: vec![FunctionPrototype {
                name: String::new(),
                self_binding: None,
                params: Vec::new(),
                captures: Vec::new(),
                instructions: Vec::new(),
                source_lines: Vec::new(),
                exception_handlers: Vec::new(),
                cleanup_blocks: Vec::new(),
                loop_targets: Vec::new(),
            }],
            temporary: 0,
            next_cleanup: 1,
            semantic_functions: Vec::new(),
            semantic_bindings: Vec::new(),
            semantic_types: Vec::new(),
            semantic_methods: Vec::new(),
            error: None,
        }
    }

    fn finish(mut self, program: &Program, module: bool) -> Result<BytecodeProgram, Diagnostic> {
        self.semantic_functions = program.symbols.functions.clone();
        self.semantic_bindings = program.binding_operands();
        self.semantic_types = program.symbols.types.clone();
        self.semantic_methods = program.symbols.methods.clone();
        let mut main = FunctionCompiler {
            name: "主程序".into(),
            ..Default::default()
        };
        self.statements(&mut main, program);
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        main.emit(Instruction::Halt, program.last().map_or(0, Stmt::line));
        self.functions[0] = main.finish();
        let exports = if module {
            program
                .symbols
                .occurrences
                .iter()
                .filter_map(|occurrence| match occurrence.identity {
                    SymbolId::Binding(id)
                        if occurrence.declaration
                            && program.symbols.interface.public_values.contains(&id) =>
                    {
                        Some(BindingOperand {
                            id,
                            name: occurrence.name.clone(),
                        })
                    }
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(BytecodeProgram {
            version: BYTECODE_VERSION,
            constants: self.constants,
            functions: self.functions,
            exports,
            module_imports: BTreeMap::new(),
        })
    }

    fn fail(&mut self, line: usize, message: &str) {
        if self.error.is_none() {
            self.error = Some(Diagnostic::new(
                DiagnosticStage::Compile,
                SourceSpan::line(line),
                message,
            ));
        }
    }

    fn binding(
        &mut self,
        name: &str,
        span: SourceSpan,
        declaration: bool,
    ) -> Option<BindingOperand> {
        let binding = self
            .semantic_bindings
            .iter()
            .find(|(candidate_span, candidate_declaration, candidate)| {
                *candidate_span == span
                    && *candidate_declaration == declaration
                    && candidate.name == name
            })
            .map(|(_, _, binding)| binding.clone());
        if binding.is_none() {
            self.fail(
                span.start.line,
                &format!("名称“{name}”缺少已解析的语义绑定身份"),
            );
        }
        binding
    }

    fn type_operand(&self, reference: &crate::ast::TypeRef) -> TypeOperand {
        let id = self
            .semantic_types
            .iter()
            .find(|semantic_type| semantic_type.span == reference.span)
            .map(|semantic_type| semantic_type.id)
            .or_else(|| {
                self.semantic_types
                    .iter()
                    .find(|semantic_type| semantic_type.name == reference.name)
                    .map(|semantic_type| semantic_type.id)
            });
        TypeOperand {
            id,
            name: reference.name.clone(),
        }
    }

    fn method_operand(&self, name: &str) -> MethodOperand {
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

    fn statements(&mut self, function: &mut FunctionCompiler, statements: &[Stmt]) {
        for statement in statements {
            self.statement(function, statement);
        }
    }

    fn scoped_statements(
        &mut self,
        function: &mut FunctionCompiler,
        statements: &[Stmt],
        line: usize,
    ) {
        function.enter_scope(line);
        function.cleanups.push(Cleanup::Scope);
        self.statements(function, statements);
        function.cleanups.pop();
        function.exit_scope(line);
    }

    fn statement(&mut self, function: &mut FunctionCompiler, statement: &Stmt) {
        let line = statement.line();
        match statement {
            Stmt::Export {
                public, statement, ..
            } => {
                if let Stmt::Function {
                    name,
                    name_span,
                    params,
                    body,
                    type_name: Some(type_name),
                    ..
                } = statement.as_ref()
                {
                    let index = self.compile_function(name, *name_span, params, body);
                    function.emit(Instruction::MakeClosure(index), line);
                    function.emit(
                        Instruction::DefineMethod {
                            owner: self.type_operand(type_name),
                            method: self.method_operand(name),
                            public: *public,
                        },
                        line,
                    );
                } else {
                    self.statement(function, statement);
                }
            }
            Stmt::TaskGroup { body, .. } => {
                function.enter_scope(line);
                let group_environment_depth = function.environment_depth;
                function.emit(Instruction::BeginTaskGroup, line);
                let body_start = function.position();
                function.cleanups.push(Cleanup::TaskGroup);
                self.statements(function, body);
                function.cleanups.pop();
                function.emit(Instruction::FinishTaskGroup, line);
                let body_end = function.position();
                function.exit_scope(line);
                let normal_exit = function.emit(Instruction::Jump(0), line);
                let cancel_target = function.position();
                function.emit(Instruction::CancelTaskGroup, line);
                function.emit(Instruction::ExitScope, line);
                function.emit(Instruction::Rethrow, line);
                let after = function.position();
                function.patch(normal_exit, after);
                if body_start < body_end {
                    function.handlers.push(ExceptionHandler {
                        start: body_start as u32,
                        end: body_end as u32,
                        target: cancel_target as u32,
                        kind: HandlerKind::Finally,
                        environment_depth: group_environment_depth as u32,
                    });
                }
            }
            Stmt::Define {
                name,
                name_span,
                expr,
                type_constraint,
                ..
            } => {
                self.expression(function, expr);
                let Some(binding) = self.binding(name, *name_span, true) else {
                    return;
                };
                function.emit(
                    Instruction::Define {
                        binding,
                        type_constraint: type_constraint
                            .as_ref()
                            .map(|value| self.type_operand(value)),
                    },
                    line,
                );
            }
            Stmt::Assign { target, expr, .. } => match target {
                Expr::Name(name, span) => {
                    self.expression(function, expr);
                    let binding = self.binding(name, *span, false);
                    function.emit(Instruction::AssignName(binding, name.clone()), line);
                }
                Expr::Attr { receiver, name, .. } => {
                    self.expression(function, expr);
                    self.expression(function, receiver);
                    function.emit(Instruction::SetAttr(name.clone()), line);
                }
                Expr::Index {
                    receiver, index, ..
                } => {
                    self.expression(function, expr);
                    self.expression(function, receiver);
                    self.expression(function, index);
                    function.emit(Instruction::SetIndex, line);
                }
                _ => (),
            },
            Stmt::Expr { expr, .. } => {
                self.expression(function, expr);
                function.emit(Instruction::Pop, line);
            }
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                let mut ends = Vec::new();
                for (condition, body) in branches {
                    self.expression(function, condition);
                    let skip = function.emit(Instruction::JumpIfFalse(0), condition.line());
                    self.scoped_statements(function, body, line);
                    ends.push(function.emit(Instruction::Jump(0), line));
                    function.patch(skip, function.position());
                }
                self.scoped_statements(function, otherwise, line);
                let end = function.position();
                for jump in ends {
                    function.patch(jump, end);
                }
            }
            Stmt::While {
                loop_id,
                condition,
                body,
                ..
            } => {
                let start = function.position();
                self.expression(function, condition);
                let exit = function.emit(Instruction::JumpIfFalse(0), condition.line());
                function.loops.push(LoopContext {
                    id: *loop_id,
                    break_jumps: Vec::new(),
                    continue_target: start,
                    cleanup_depth: function.cleanups.len(),
                });
                function.enter_scope(line);
                function.cleanups.push(Cleanup::Scope);
                self.statements(function, body);
                function.cleanups.pop();
                function.exit_scope(line);
                function.emit(Instruction::Jump(start as u32), line);
                let end = function.position();
                function.patch(exit, end);
                let Some(context) = function.loops.pop() else {
                    self.fail(line, "循环编译上下文栈下溢");
                    return;
                };
                for jump in context.break_jumps {
                    function.patch(jump, end);
                }
                function.loop_targets.push(LoopTarget {
                    id: context.id,
                    continue_target: context.continue_target as u32,
                    break_target: end as u32,
                    environment_depth: function.environment_depth as u32,
                });
            }
            Stmt::For {
                loop_id,
                iterable,
                name,
                name_span,
                body,
                ..
            } => {
                self.expression(function, iterable);
                function.emit(Instruction::StartIterator, line);
                let next = function.position();
                let Some(binding) = self.binding(name, *name_span, true) else {
                    return;
                };
                let exit = function.emit(Instruction::NextIterator { binding, end: 0 }, line);
                function.loops.push(LoopContext {
                    id: *loop_id,
                    break_jumps: Vec::new(),
                    continue_target: next,
                    cleanup_depth: function.cleanups.len(),
                });
                function.environment_depth += 1;
                self.statements(function, body);
                function.environment_depth -= 1;
                function.emit(Instruction::Jump(next as u32), line);
                let cleanup = function.position();
                function.patch(exit, cleanup);
                function.emit(Instruction::EndIterator, line);
                let end = function.position();
                let Some(context) = function.loops.pop() else {
                    self.fail(line, "遍历编译上下文栈下溢");
                    return;
                };
                for jump in context.break_jumps {
                    function.patch(jump, cleanup);
                }
                function.loop_targets.push(LoopTarget {
                    id: context.id,
                    continue_target: context.continue_target as u32,
                    break_target: cleanup as u32,
                    environment_depth: function.environment_depth as u32,
                });
                debug_assert!(end >= cleanup);
            }
            Stmt::Function {
                name,
                name_span,
                params,
                body,
                type_name,
                ..
            } => {
                let index = self.compile_function(name, *name_span, params, body);
                function.emit(Instruction::MakeClosure(index), line);
                if let Some(type_name) = type_name {
                    function.emit(
                        Instruction::DefineMethod {
                            owner: self.type_operand(type_name),
                            method: self.method_operand(name),
                            public: true,
                        },
                        line,
                    );
                } else {
                    let Some(binding) = self.binding(name, *name_span, true) else {
                        return;
                    };
                    function.emit(
                        Instruction::Define {
                            binding,
                            type_constraint: None,
                        },
                        line,
                    );
                }
            }
            Stmt::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expression(function, expr);
                } else {
                    self.literal(function, &Literal::Null, line);
                }
                self.emit_cleanups(function, 0, line, CompletionReason::Return);
                function.emit(Instruction::Return, line);
            }
            Stmt::Struct {
                name,
                name_span,
                fields,
                ..
            } => {
                let Some(binding) = self.binding(name, *name_span, true) else {
                    return;
                };
                let Some(type_id) = self
                    .semantic_types
                    .iter()
                    .find(|semantic_type| semantic_type.span == *name_span)
                    .map(|semantic_type| semantic_type.id)
                else {
                    self.fail(line, &format!("结构“{name}”缺少语义类型身份"));
                    return;
                };
                function.emit(
                    Instruction::DefineStruct {
                        binding,
                        type_id,
                        fields: fields.clone(),
                    },
                    line,
                );
            }
            Stmt::Import {
                module,
                names,
                binding_spans,
                ..
            } => {
                let binding_names = names.clone().unwrap_or_else(|| {
                    vec![module.rsplit('/').next().unwrap_or(module).to_string()]
                });
                let mut bindings = Vec::new();
                for (name, span) in binding_names.iter().zip(binding_spans) {
                    let Some(binding) = self.binding(name, *span, true) else {
                        return;
                    };
                    bindings.push(binding);
                }
                function.emit(
                    Instruction::Import {
                        module: module.clone(),
                        bindings,
                        selective: names.is_some(),
                    },
                    line,
                );
            }
            Stmt::Global {
                names, name_spans, ..
            } => {
                let mut bindings = Vec::new();
                for (name, span) in names.iter().zip(name_spans) {
                    let Some(binding) = self.binding(name, *span, false) else {
                        return;
                    };
                    bindings.push(binding);
                }
                function.emit(Instruction::DeclareGlobal(bindings), line);
            }
            Stmt::Break { target, .. } => {
                let Some(target) = target else {
                    self.fail(line, "跳出循环缺少语义循环目标");
                    return;
                };
                let Some(cleanup_depth) = function
                    .loops
                    .iter()
                    .rev()
                    .find(|context| context.id == *target)
                    .map(|context| context.cleanup_depth)
                else {
                    self.fail(line, "跳出循环引用了当前功能之外的循环目标");
                    return;
                };
                self.emit_cleanups(
                    function,
                    cleanup_depth,
                    line,
                    CompletionReason::Break(*target),
                );
                let jump = function.emit(
                    Instruction::Break {
                        loop_id: *target,
                        target: 0,
                    },
                    line,
                );
                if let Some(context) = function
                    .loops
                    .iter_mut()
                    .rev()
                    .find(|context| context.id == *target)
                {
                    context.break_jumps.push(jump);
                }
            }
            Stmt::Continue { target, .. } => {
                let Some(target) = target else {
                    self.fail(line, "继续下一轮缺少语义循环目标");
                    return;
                };
                let Some(context) = function
                    .loops
                    .iter()
                    .rev()
                    .find(|context| context.id == *target)
                else {
                    self.fail(line, "继续下一轮引用了当前功能之外的循环目标");
                    return;
                };
                let cleanup_depth = context.cleanup_depth;
                let continue_target = context.continue_target;
                self.emit_cleanups(
                    function,
                    cleanup_depth,
                    line,
                    CompletionReason::Continue(*target),
                );
                function.emit(
                    Instruction::Continue {
                        loop_id: *target,
                        target: continue_target as u32,
                    },
                    line,
                );
            }
            Stmt::Try { .. } => self.try_statement(function, statement),
            Stmt::Throw { expr, .. } => {
                self.expression(function, expr);
                function.emit(Instruction::Throw, line);
            }
            Stmt::Require {
                expr,
                type_constraint,
                message,
                ..
            } => {
                self.expression(function, expr);
                function.emit(Instruction::Dup, line);
                if let Some(expected) = type_constraint {
                    function.emit(Instruction::IsType(self.type_operand(expected)), line);
                }
                let success = function.emit(Instruction::JumpIfTrue(0), line);
                if let Some(message) = message {
                    self.expression(function, message);
                }
                function.emit(
                    Instruction::Require {
                        type_constraint: type_constraint
                            .as_ref()
                            .map(|value| self.type_operand(value)),
                        has_message: message.is_some(),
                    },
                    line,
                );
                let failure_exit = function.emit(Instruction::Jump(0), line);
                function.patch(success, function.position());
                function.emit(Instruction::Pop, line);
                function.patch(failure_exit, function.position());
            }
            Stmt::Match {
                expr,
                arms,
                otherwise,
                ..
            } => {
                function.enter_scope(line);
                function.cleanups.push(Cleanup::Scope);
                let temp = self.temp_name();
                self.expression(function, expr);
                function.emit(Instruction::StoreInternal(temp), line);
                let mut ends = Vec::new();
                for arm in arms {
                    function.enter_scope(line);
                    function.cleanups.push(Cleanup::Scope);
                    match &arm.pattern {
                        MatchPattern::Value(pattern) => {
                            function.emit(Instruction::LoadInternal(temp), line);
                            self.expression(function, pattern);
                            function.emit(Instruction::Binary("=".into()), line);
                        }
                        MatchPattern::Type(expected) => {
                            function.emit(Instruction::LoadInternal(temp), line);
                            function.emit(Instruction::IsType(self.type_operand(expected)), line);
                        }
                        MatchPattern::Bind { name, span } => {
                            function.emit(Instruction::LoadInternal(temp), line);
                            let Some(binding) = self.binding(name, *span, true) else {
                                return;
                            };
                            function.emit(
                                Instruction::Define {
                                    binding,
                                    type_constraint: None,
                                },
                                line,
                            );
                            self.literal(function, &Literal::Bool(true), line);
                        }
                    }
                    let skip_pattern = function.emit(Instruction::JumpIfFalse(0), line);
                    function.emit(Instruction::LoadInternal(temp), line);
                    let implicit_span = arm.pattern.implicit_binding_span();
                    let Some(binding) = self.binding("当前值", implicit_span, true) else {
                        return;
                    };
                    function.emit(
                        Instruction::Define {
                            binding,
                            type_constraint: None,
                        },
                        line,
                    );
                    let skip_guard = if let Some(guard) = &arm.guard {
                        self.expression(function, guard);
                        Some(function.emit(Instruction::JumpIfFalse(0), guard.line()))
                    } else {
                        None
                    };
                    self.statements(function, &arm.body);
                    function.cleanups.pop();
                    function.exit_scope(line);
                    ends.push(function.emit(Instruction::Jump(0), line));
                    let cleanup = function.position();
                    function.patch(skip_pattern, cleanup);
                    if let Some(skip) = skip_guard {
                        function.patch(skip, cleanup);
                    }
                    function.emit(Instruction::ExitScope, line);
                }
                function.enter_scope(line);
                function.cleanups.push(Cleanup::Scope);
                self.statements(function, otherwise);
                function.cleanups.pop();
                function.exit_scope(line);
                let end = function.position();
                for jump in ends {
                    function.patch(jump, end);
                }
                function.cleanups.pop();
                function.exit_scope(line);
            }
            Stmt::Test { .. } | Stmt::TestHook { .. } => {}
        }
    }

    fn try_statement(&mut self, function: &mut FunctionCompiler, statement: &Stmt) {
        let Stmt::Try {
            body,
            error_name,
            error_span,
            catch_body,
            finally_body,
            line,
        } = statement
        else {
            self.fail(statement.line(), "尝试编译入口收到非尝试语句");
            return;
        };
        let line = *line;
        let finally_id = if finally_body.is_empty() {
            None
        } else {
            let id = self.next_cleanup;
            self.next_cleanup += 1;
            Some(id)
        };
        let handler_environment_depth = function.environment_depth;
        function.enter_scope(line);
        if let Some(id) = finally_id {
            function.cleanups.push(Cleanup::Finally { id });
        }
        function.cleanups.push(Cleanup::Scope);
        let body_start = function.position();
        self.statements(function, body);
        let body_end = function.position();
        function.cleanups.pop();
        if !finally_body.is_empty() {
            function.cleanups.pop();
        }
        function.exit_scope(line);
        if let Some(id) = finally_id {
            function.emit_cleanup_call(id, CompletionReason::Normal, line);
        }
        let normal_exit = function.emit(Instruction::Jump(0), line);

        let has_catch = error_name.is_some() || !catch_body.is_empty();
        let catch_target = function.position();
        let mut catch_end = catch_target;
        let mut catch_exit = None;
        if has_catch {
            function.enter_scope(line);
            if let Some(id) = finally_id {
                function.cleanups.push(Cleanup::Finally { id });
            }
            function.cleanups.push(Cleanup::Scope);
            let error_binding = match (error_name, error_span) {
                (Some(name), Some(span)) => self.binding(name, *span, true),
                _ => None,
            };
            function.emit(Instruction::BindException(error_binding), line);
            self.statements(function, catch_body);
            catch_end = function.position();
            function.cleanups.pop();
            if !finally_body.is_empty() {
                function.cleanups.pop();
            }
            function.exit_scope(line);
            if let Some(id) = finally_id {
                function.emit_cleanup_call(id, CompletionReason::Normal, line);
            }
            catch_exit = Some(function.emit(Instruction::Jump(0), line));
        }

        let finally_target = function.position();
        if !finally_body.is_empty() {
            if let Some(id) = finally_id {
                function.patch_cleanup(id, finally_target);
            }
            self.scoped_statements(function, finally_body, line);
            function.emit(Instruction::FinishCleanup, line);
            function.cleanup_blocks.push(CleanupBlock {
                start: finally_target as u32,
                end: function.position() as u32,
            });
        }
        let after = function.position();
        function.patch(normal_exit, after);
        if let Some(exit) = catch_exit {
            function.patch(exit, after);
        }
        if has_catch {
            self.push_handler_regions(
                function,
                body_start,
                body_end,
                catch_target,
                HandlerKind::Catch,
                handler_environment_depth,
            );
            if !finally_body.is_empty() && catch_target < catch_end {
                self.push_handler_regions(
                    function,
                    catch_target,
                    catch_end,
                    finally_target,
                    HandlerKind::Finally,
                    handler_environment_depth,
                );
            }
        } else if !finally_body.is_empty() {
            function.handlers.push(ExceptionHandler {
                start: body_start as u32,
                end: body_end as u32,
                target: finally_target as u32,
                kind: HandlerKind::Finally,
                environment_depth: handler_environment_depth as u32,
            });
        }
    }

    fn compile_function(
        &mut self,
        name: &str,
        declaration_span: SourceSpan,
        params: &[Param],
        body: &[Stmt],
    ) -> u32 {
        let index = self.functions.len() as u32;
        self.functions.push(FunctionPrototype {
            name: String::new(),
            self_binding: None,
            params: Vec::new(),
            captures: Vec::new(),
            instructions: Vec::new(),
            source_lines: Vec::new(),
            exception_handlers: Vec::new(),
            cleanup_blocks: Vec::new(),
            loop_targets: Vec::new(),
        });
        let mut compiled_params = Vec::new();
        for param in params {
            let default_function = param.default.as_ref().map(|default| {
                let default_name = format!("{name} 的默认参数 {}", param.name);
                let mut function = FunctionCompiler {
                    name: default_name,
                    ..Default::default()
                };
                self.expression(&mut function, default);
                function.emit(Instruction::Return, default.line());
                let default_index = self.functions.len() as u32;
                self.functions.push(function.finish());
                default_index
            });
            compiled_params.push(Parameter {
                binding: match self.binding(&param.name, param.name_span, true) {
                    Some(binding) => binding,
                    None => continue,
                },
                name_span: param.name_span,
                default_function,
            });
        }
        let mut function = FunctionCompiler {
            name: name.into(),
            self_binding: self
                .semantic_bindings
                .iter()
                .find(|(span, declaration, binding)| {
                    *span == declaration_span && *declaration && binding.name == name
                })
                .map(|(_, _, binding)| binding.clone()),
            params: compiled_params,
            captures: self
                .semantic_functions
                .iter()
                .find(|function| function.declaration_span == declaration_span)
                .map(|function| function.captures.clone())
                .unwrap_or_default(),
            ..Default::default()
        };
        self.statements(&mut function, body);
        self.literal(
            &mut function,
            &Literal::Null,
            body.last().map_or(0, Stmt::line),
        );
        function.emit(Instruction::Return, body.last().map_or(0, Stmt::line));
        self.functions[index as usize] = function.finish();
        index
    }

    fn emit_cleanups(
        &mut self,
        function: &mut FunctionCompiler,
        depth: usize,
        line: usize,
        reason: CompletionReason,
    ) {
        let cleanup_len = function.cleanups.len();
        let original_environment_depth = function.environment_depth;
        for index in (depth..cleanup_len).rev() {
            let cleanup = function.cleanups[index].clone();
            match cleanup {
                Cleanup::Finally { id } => function.emit_cleanup_call(id, reason.clone(), line),
                Cleanup::TaskGroup => {
                    function.emit(Instruction::CancelTaskGroup, line);
                    function.emit(Instruction::ExitScope, line);
                    function.environment_depth = function.environment_depth.saturating_sub(1);
                }
                Cleanup::Scope => {
                    function.emit(Instruction::ExitScope, line);
                    function.environment_depth = function.environment_depth.saturating_sub(1);
                }
            }
        }
        function.environment_depth = original_environment_depth;
    }

    fn push_handler_regions(
        &self,
        function: &mut FunctionCompiler,
        start: usize,
        end: usize,
        target: usize,
        kind: HandlerKind,
        environment_depth: usize,
    ) {
        if start < end {
            function.handlers.push(ExceptionHandler {
                start: start as u32,
                end: end as u32,
                target: target as u32,
                kind,
                environment_depth: environment_depth as u32,
            });
        }
    }

    fn expression(&mut self, function: &mut FunctionCompiler, expression: &Expr) {
        let line = expression.line();
        match expression {
            Expr::Literal(value, _) => self.literal(function, value, line),
            Expr::Name(name, span) => {
                let binding = self
                    .semantic_bindings
                    .iter()
                    .find(|(candidate_span, declaration, candidate)| {
                        *candidate_span == *span && !*declaration && candidate.name == *name
                    })
                    .map(|(_, _, binding)| binding.clone());
                function.emit(Instruction::LoadName(binding, name.clone()), line);
            }
            Expr::List(values, _) => {
                for value in values {
                    self.expression(function, value);
                }
                function.emit(Instruction::BuildList(values.len() as u32), line);
            }
            Expr::Record(values, _) => {
                for (_, value) in values {
                    self.expression(function, value);
                }
                function.emit(
                    Instruction::BuildRecord(values.iter().map(|(key, _)| key.clone()).collect()),
                    line,
                );
            }
            Expr::Unary { op, expr, .. } => {
                self.expression(function, expr);
                function.emit(Instruction::Unary(op.clone()), line);
            }
            Expr::Binary {
                left, op, right, ..
            } if op == "且" || op == "或" => {
                self.expression(function, left);
                function.emit(Instruction::ToBool, line);
                function.emit(Instruction::Dup, line);
                let jump = function.emit(
                    if op == "且" {
                        Instruction::JumpIfFalse(0)
                    } else {
                        Instruction::JumpIfTrue(0)
                    },
                    line,
                );
                function.emit(Instruction::Pop, line);
                self.expression(function, right);
                function.emit(Instruction::ToBool, line);
                function.patch(jump, function.position());
            }
            Expr::Binary {
                left, op, right, ..
            } => {
                self.expression(function, left);
                self.expression(function, right);
                function.emit(Instruction::Binary(op.clone()), line);
            }
            Expr::Call {
                receiver,
                name,
                args,
                named,
                ..
            } => {
                self.expression(function, receiver);
                for arg in args {
                    self.expression(function, arg);
                }
                for argument in named {
                    self.expression(function, &argument.value);
                }
                function.emit(
                    Instruction::Call {
                        method: name.as_ref().map(|name| self.method_operand(name)),
                        positional: args.len() as u32,
                        named: named.iter().map(|argument| argument.name.clone()).collect(),
                    },
                    line,
                );
            }
            Expr::Attr { receiver, name, .. } => {
                self.expression(function, receiver);
                function.emit(Instruction::GetAttr(name.clone()), line);
            }
            Expr::Index {
                receiver, index, ..
            } => {
                self.expression(function, receiver);
                self.expression(function, index);
                function.emit(Instruction::GetIndex, line);
            }
            Expr::Function {
                name,
                declaration_span,
                params,
                body,
                ..
            } => {
                let display = name.as_deref().unwrap_or("匿名功能");
                let index = self.compile_function(display, *declaration_span, params, body);
                function.emit(Instruction::MakeClosure(index), line);
            }
        }
    }

    fn literal(&mut self, function: &mut FunctionCompiler, literal: &Literal, line: usize) {
        let constant = match literal {
            Literal::Null => Constant::Null,
            Literal::Bool(value) => Constant::Bool(*value),
            Literal::Number(value) => Constant::Number(*value),
            Literal::Text(value) => Constant::Text(value.clone()),
        };
        let index = self
            .constants
            .iter()
            .position(|item| item == &constant)
            .unwrap_or_else(|| {
                self.constants.push(constant);
                self.constants.len() - 1
            });
        function.emit(Instruction::PushConstant(index as u32), line);
    }

    fn temp_name(&mut self) -> u32 {
        self.temporary += 1;
        self.temporary as u32
    }
}

impl FunctionCompiler {
    fn emit(&mut self, instruction: Instruction, line: usize) -> usize {
        let position = self.instructions.len();
        self.instructions.push(instruction);
        self.lines.push(line as u32);
        position
    }

    fn position(&self) -> usize {
        self.instructions.len()
    }

    fn emit_cleanup_call(&mut self, id: u32, reason: CompletionReason, line: usize) {
        let position = self.emit(Instruction::CallCleanup { target: 0, reason }, line);
        self.cleanup_patches.entry(id).or_default().push(position);
    }

    fn patch_cleanup(&mut self, id: u32, target: usize) {
        for position in self.cleanup_patches.remove(&id).unwrap_or_default() {
            if let Instruction::CallCleanup {
                target: destination,
                ..
            } = &mut self.instructions[position]
            {
                *destination = target as u32;
            }
        }
    }

    fn enter_scope(&mut self, line: usize) {
        self.emit(Instruction::EnterScope, line);
        self.environment_depth += 1;
    }

    fn exit_scope(&mut self, line: usize) {
        self.emit(Instruction::ExitScope, line);
        self.environment_depth = self.environment_depth.saturating_sub(1);
    }

    fn patch(&mut self, position: usize, target: usize) {
        match &mut self.instructions[position] {
            Instruction::Jump(value)
            | Instruction::JumpIfFalse(value)
            | Instruction::JumpIfTrue(value) => *value = target as u32,
            Instruction::Break {
                target: destination,
                ..
            } => *destination = target as u32,
            Instruction::NextIterator { end, .. } => *end = target as u32,
            _ => (),
        }
    }

    fn finish(self) -> FunctionPrototype {
        FunctionPrototype {
            name: self.name,
            self_binding: self.self_binding,
            params: self.params,
            captures: self.captures,
            instructions: self.instructions,
            source_lines: self.lines,
            exception_handlers: self.handlers,
            cleanup_blocks: self.cleanup_blocks,
            loop_targets: self.loop_targets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn compiler_emits_only_serializable_vm_instructions() {
        let bytecode =
            compile(&parse("定义功能 加一（值）：\n返回 值 + 1\n结束\n输出(加一(2))").unwrap())
                .unwrap();
        bytecode.verify().unwrap();
        assert!(bytecode.functions.len() >= 2);
        let encoded = postcard::to_allocvec(&bytecode).unwrap();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn compiler_emits_static_exception_handlers() {
        let bytecode = compile(&parse("尝试：\n抛出 \"错\"\n捕获 异常：\n输出(异常 的 信息)\n最终：\n输出(\"清理\")\n结束").unwrap()).unwrap();
        bytecode.verify().unwrap();
        assert_eq!(bytecode.functions[0].exception_handlers.len(), 2);
        assert!(bytecode.functions[0]
            .exception_handlers
            .iter()
            .any(|handler| handler.kind == HandlerKind::Catch));
    }

    #[test]
    fn compiler_emits_each_finally_as_one_shared_cleanup_block() {
        let program = parse(
            "定义功能 计算（值）：\n尝试：\n如果 值 > 0：\n返回 值\n结束\n输出(\"正文\")\n最终：\n输出(\"清理\")\n结束\n返回 0\n结束",
        )
        .unwrap();
        let bytecode = compile(&program).unwrap();
        let function = bytecode
            .functions
            .iter()
            .find(|function| function.name == "计算")
            .unwrap();
        assert_eq!(function.cleanup_blocks.len(), 1);
        assert_eq!(
            function
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::FinishCleanup))
                .count(),
            1
        );
        assert!(function.instructions.iter().any(|instruction| matches!(
            instruction,
            Instruction::CallCleanup {
                reason: CompletionReason::Return,
                ..
            }
        )));
    }

    #[test]
    fn module_exports_are_driven_by_public_binding_identities() {
        let bytecode = compile_module(
            &parse("内部 引入 甲\n内部 从 乙 引入 丙，丁\n公开 定义 名称 为 \"模块\"").unwrap(),
        )
        .unwrap();
        assert_eq!(
            bytecode
                .exports
                .iter()
                .map(|binding| binding.name.as_str())
                .collect::<Vec<_>>(),
            ["名称"]
        );
    }

    #[test]
    fn bytecode_uses_binding_identities_for_shadowing_and_global_assignment() {
        let bytecode = compile(
            &parse(
                "定义 值 为 1\n定义功能 修改（）：\n全局 值\n值 为 2\n结束\n如果 是：\n定义 值 为 3\n输出(值)\n结束\n修改()\n输出(值)",
            )
            .unwrap(),
        )
        .unwrap();
        let root = bytecode.functions[0]
            .instructions
            .iter()
            .find_map(|instruction| match instruction {
                Instruction::Define { binding, .. } if binding.name == "值" => Some(binding.id),
                _ => None,
            })
            .unwrap();
        let shadow = bytecode.functions[0]
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                Instruction::Define { binding, .. } if binding.name == "值" => Some(binding.id),
                _ => None,
            })
            .find(|binding| *binding != root)
            .unwrap();
        assert_ne!(root, shadow);
        let function = bytecode
            .functions
            .iter()
            .find(|function| function.name == "修改")
            .unwrap();
        assert!(function.instructions.iter().any(|instruction| matches!(
            instruction,
            Instruction::DeclareGlobal(bindings) if bindings.iter().any(|binding| binding.id == root)
        )));
        assert!(function.instructions.iter().any(|instruction| matches!(
            instruction,
            Instruction::AssignName(Some(binding), name) if binding.id == root && name == "值"
        )));
    }

    #[test]
    fn bytecode_uses_type_and_method_identities_for_user_structures() {
        let bytecode = compile(
            &parse(
                "结构 客户：\n姓名\n结束\n定义功能 客户 标签（自己）：\n返回 自己 的 姓名\n结束\n定义 客户甲 为 客户(姓名 为 \"甲\")\n要求 客户甲 的类型是 客户\n输出(客户甲 标签())",
            )
            .unwrap(),
        )
        .unwrap();
        let instructions = &bytecode.functions[0].instructions;
        let type_id = instructions
            .iter()
            .find_map(|instruction| match instruction {
                Instruction::DefineStruct { type_id, .. } => Some(*type_id),
                _ => None,
            })
            .unwrap();
        let method_id = instructions
            .iter()
            .find_map(|instruction| match instruction {
                Instruction::DefineMethod { owner, method, .. } => {
                    assert_eq!(owner.id, Some(type_id));
                    method.candidates.first().copied()
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(method_id.owner, type_id);
        assert!(
            instructions.iter().any(|instruction| matches!(
                instruction,
                Instruction::IsType(expected) if expected.id == Some(type_id)
            )),
            "{instructions:#?}"
        );
        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            Instruction::Call { method: Some(method), .. }
                if method.candidates.contains(&method_id)
        )));
    }

    #[test]
    fn compiler_returns_errors_for_missing_or_cross_function_loop_targets() {
        let top_level = parse("跳出循环").unwrap();
        assert!(compile(&top_level)
            .unwrap_err()
            .contains("缺少语义循环目标"));

        let nested = parse("当 是 时重复：\n定义功能 非法（）：\n继续下一轮\n结束\n结束").unwrap();
        assert!(compile(&nested).unwrap_err().contains("缺少语义循环目标"));
    }

    #[test]
    fn compiler_records_loop_identities_and_targets_for_verification() {
        let bytecode = compile(
            &parse("当 是 时重复：\n跳出循环\n结束\n遍历 [1]，记作 项：\n继续下一轮\n结束")
                .unwrap(),
        )
        .unwrap();
        bytecode.verify().unwrap();
        let function = &bytecode.functions[0];
        assert_eq!(function.loop_targets.len(), 2);
        for instruction in &function.instructions {
            match instruction {
                Instruction::Break { loop_id, target } => {
                    let declared = function
                        .loop_targets
                        .iter()
                        .find(|declared| declared.id == *loop_id)
                        .unwrap();
                    assert_eq!(*target, declared.break_target);
                }
                Instruction::Continue { loop_id, target } => {
                    let declared = function
                        .loop_targets
                        .iter()
                        .find(|declared| declared.id == *loop_id)
                        .unwrap();
                    assert_eq!(*target, declared.continue_target);
                }
                _ => {}
            }
        }
    }
}
