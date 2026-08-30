use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

use crate::ast::{BindingOperand, CapturedBinding, LoopId, MethodOperand, TypeId, TypeOperand};
use crate::diagnostic::SourceSpan;

pub const BYTECODE_VERSION: u32 = 13;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Constant {
    Null,
    Bool(bool),
    Number(#[serde(with = "rust_decimal::serde::str")] Decimal),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub binding: BindingOperand,
    pub name_span: SourceSpan,
    pub default_function: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HandlerKind {
    Catch,
    Finally,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExceptionHandler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    pub kind: HandlerKind,
    pub environment_depth: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CleanupBlock {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopTarget {
    pub id: LoopId,
    pub continue_target: u32,
    pub break_target: u32,
    pub environment_depth: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CompletionReason {
    Normal,
    Return,
    Break(LoopId),
    Continue(LoopId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Instruction {
    PushConstant(u32),
    LoadName(Option<BindingOperand>, String),
    LoadInternal(u32),
    StoreInternal(u32),
    Define {
        binding: BindingOperand,
        type_constraint: Option<TypeOperand>,
    },
    AssignName(Option<BindingOperand>, String),
    Pop,
    Dup,
    BuildList(u32),
    BuildRecord(Vec<String>),
    Unary(String),
    Binary(String),
    ToBool,
    IsType(TypeOperand),
    GetAttr(String),
    GetIndex,
    SetAttr(String),
    SetIndex,
    Jump(u32),
    Break {
        loop_id: LoopId,
        target: u32,
    },
    Continue {
        loop_id: LoopId,
        target: u32,
    },
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    StartIterator,
    NextIterator {
        binding: BindingOperand,
        end: u32,
    },
    EndIterator,
    EnterScope,
    ExitScope,
    MakeClosure(u32),
    Call {
        method: Option<MethodOperand>,
        positional: u32,
        named: Vec<String>,
    },
    Return,
    DefineMethod {
        owner: TypeOperand,
        method: MethodOperand,
        public: bool,
    },
    DefineStruct {
        binding: BindingOperand,
        type_id: TypeId,
        fields: Vec<String>,
    },
    Import {
        module: String,
        bindings: Vec<BindingOperand>,
        selective: bool,
    },
    DeclareGlobal(Vec<BindingOperand>),
    Require {
        type_constraint: Option<TypeOperand>,
        has_message: bool,
    },
    Throw,
    BindException(Option<BindingOperand>),
    CallCleanup {
        target: u32,
        reason: CompletionReason,
    },
    FinishCleanup,
    Rethrow,
    BeginTaskGroup,
    FinishTaskGroup,
    CancelTaskGroup,
    Halt,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionPrototype {
    pub name: String,
    pub self_binding: Option<BindingOperand>,
    pub params: Vec<Parameter>,
    pub captures: Vec<CapturedBinding>,
    pub instructions: Vec<Instruction>,
    pub source_lines: Vec<u32>,
    pub exception_handlers: Vec<ExceptionHandler>,
    pub cleanup_blocks: Vec<CleanupBlock>,
    pub loop_targets: Vec<LoopTarget>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BytecodeProgram {
    pub version: u32,
    pub constants: Vec<Constant>,
    pub functions: Vec<FunctionPrototype>,
    pub exports: Vec<BindingOperand>,
    /// Maps the import spelling used by this program to its package-local module ID.
    pub module_imports: BTreeMap<String, String>,
}

impl BytecodeProgram {
    pub fn verify(&self) -> Result<(), String> {
        if self.version != BYTECODE_VERSION {
            return Err(format!(
                "不支持的字节码版本 {}，当前版本为 {BYTECODE_VERSION}",
                self.version
            ));
        }
        if self.functions.is_empty() {
            return Err("字节码没有主函数".into());
        }
        if self
            .module_imports
            .iter()
            .any(|(name, module_id)| name.is_empty() || module_id.is_empty())
        {
            return Err("字节码模块导入映射包含空名称".into());
        }
        for (index, function) in self.functions.iter().enumerate() {
            self.verify_function(index, function)?;
        }
        Ok(())
    }

    fn verify_function(
        &self,
        function_index: usize,
        function: &FunctionPrototype,
    ) -> Result<(), String> {
        let length = function.instructions.len();
        if length == 0 {
            return Err(format!("字节码函数 {function_index} 没有指令"));
        }
        if function.source_lines.len() != length {
            return Err(format!("字节码函数 {function_index} 源码映射长度不一致"));
        }
        for parameter in &function.params {
            if parameter
                .default_function
                .is_some_and(|index| index as usize >= self.functions.len())
            {
                return Err(format!("字节码函数 {function_index} 默认参数函数索引越界"));
            }
        }
        let maximum_environment_depth = function
            .instructions
            .iter()
            .filter(|instruction| {
                matches!(
                    instruction,
                    Instruction::EnterScope | Instruction::NextIterator { .. }
                )
            })
            .count() as u32;
        for handler in &function.exception_handlers {
            if handler.start >= handler.end
                || handler.end as usize > length
                || handler.target as usize >= length
            {
                return Err(format!("字节码函数 {function_index} 异常区间无效"));
            }
            if handler.environment_depth > maximum_environment_depth {
                return Err(format!(
                    "字节码函数 {function_index} 异常处理作用域深度无效"
                ));
            }
        }
        for block in &function.cleanup_blocks {
            if block.start >= block.end || block.end as usize > length {
                return Err(format!("字节码函数 {function_index} 清理块区间无效"));
            }
            if !matches!(
                function.instructions.get(block.end as usize - 1),
                Some(Instruction::FinishCleanup)
            ) {
                return Err(format!("字节码函数 {function_index} 清理块缺少完成指令"));
            }
        }
        for (index, block) in function.cleanup_blocks.iter().enumerate() {
            if function
                .cleanup_blocks
                .iter()
                .skip(index + 1)
                .filter(|other| block.start < other.end && other.start < block.end)
                .any(|other| {
                    let nested = (block.start <= other.start && other.end <= block.end)
                        || (other.start <= block.start && block.end <= other.end);
                    !nested || (block.start, block.end) == (other.start, other.end)
                })
            {
                return Err(format!("字节码函数 {function_index} 清理块区间部分重叠"));
            }
        }
        for (ip, instruction) in function.instructions.iter().enumerate() {
            if matches!(instruction, Instruction::FinishCleanup)
                && !function
                    .cleanup_blocks
                    .iter()
                    .any(|block| block.end as usize == ip + 1)
            {
                return Err(format!(
                    "字节码函数 {function_index} 在位置 {ip} 的清理完成指令没有完成原因槽"
                ));
            }
        }
        for handler in &function.exception_handlers {
            let target_is_cleanup = function
                .cleanup_blocks
                .iter()
                .any(|block| block.start == handler.target);
            if matches!(handler.kind, HandlerKind::Finally)
                && !target_is_cleanup
                && !matches!(
                    function.instructions.get(handler.target as usize),
                    Some(Instruction::CancelTaskGroup)
                )
            {
                return Err(format!("字节码函数 {function_index} 最终处理目标无效"));
            }
            if matches!(handler.kind, HandlerKind::Catch) && target_is_cleanup {
                return Err(format!("字节码函数 {function_index} 捕获目标不能是清理块"));
            }
        }
        let mut loop_ids = std::collections::BTreeSet::new();
        for target in &function.loop_targets {
            if !loop_ids.insert(target.id)
                || target.continue_target as usize >= length
                || target.break_target as usize >= length
                || target.environment_depth > maximum_environment_depth
            {
                return Err(format!("字节码函数 {function_index} 循环目标无效"));
            }
        }

        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        struct State {
            stack: usize,
            environment: usize,
            task_groups: Option<usize>,
        }
        let mut states = BTreeMap::new();
        let mut pending = VecDeque::from([(
            0usize,
            State {
                stack: 0,
                environment: 0,
                task_groups: Some(0),
            },
        )]);
        for handler in &function.exception_handlers {
            pending.push_back((
                handler.target as usize,
                State {
                    stack: 0,
                    environment: handler.environment_depth as usize,
                    task_groups: None,
                },
            ));
        }
        while let Some((ip, state)) = pending.pop_front() {
            if ip >= length {
                return Err(format!("字节码函数 {function_index} 跳转越界"));
            }
            if let Some(existing) = states.insert(ip, state) {
                let group_conflict = existing.task_groups.is_some()
                    && state.task_groups.is_some()
                    && existing.task_groups != state.task_groups;
                let environment_conflict = existing.environment != state.environment
                    && !matches!(
                        function.instructions.get(ip),
                        Some(Instruction::NextIterator { .. } | Instruction::EndIterator)
                    );
                if existing.stack != state.stack || environment_conflict || group_conflict {
                    return Err(format!(
                        "字节码函数 {function_index} 在位置 {ip} 的栈、作用域或任务组深度不一致：{existing:?} 与 {state:?}"
                    ));
                }
                continue;
            }
            let instruction = &function.instructions[ip];
            let (pop, push) = stack_effect(instruction);
            let next_stack = state
                .stack
                .checked_sub(pop)
                .ok_or_else(|| format!("字节码函数 {function_index} 在位置 {ip} 栈下溢"))?
                + push;
            let next_environment = match instruction {
                Instruction::EnterScope => state.environment + 1,
                Instruction::ExitScope => state.environment.checked_sub(1).ok_or_else(|| {
                    format!("字节码函数 {function_index} 在位置 {ip} 作用域栈下溢")
                })?,
                Instruction::EndIterator => function
                    .loop_targets
                    .iter()
                    .find(|target| target.break_target as usize == ip)
                    .map(|target| target.environment_depth as usize)
                    .ok_or_else(|| {
                        format!("字节码函数 {function_index} 在位置 {ip} 的遍历结束缺少循环目标")
                    })?,
                _ => state.environment,
            };
            let next_task_groups = match (instruction, state.task_groups) {
                (Instruction::BeginTaskGroup, Some(depth)) => Some(depth + 1),
                (Instruction::FinishTaskGroup | Instruction::CancelTaskGroup, Some(depth)) => {
                    Some(depth.checked_sub(1).ok_or_else(|| {
                        format!("字节码函数 {function_index} 在位置 {ip} 任务组栈下溢")
                    })?)
                }
                (_, depth) => depth,
            };
            let next_state = State {
                stack: next_stack,
                environment: next_environment,
                task_groups: next_task_groups,
            };
            let target = |value: u32| -> Result<usize, String> {
                let value = value as usize;
                if value >= length {
                    Err(format!("字节码函数 {function_index} 在位置 {ip} 跳转越界"))
                } else {
                    Ok(value)
                }
            };
            let validate_jump = |destination: usize| -> Result<usize, String> {
                // 清理块严格嵌套：只要目标所在清理块同时包含当前位置，跳转就仍在该块内部，
                // 只有跳进不包含当前位置的清理块中部才违反“清理块只能从入口进入”。
                if function.cleanup_blocks.iter().any(|block| {
                    (block.start as usize) < destination
                        && destination < block.end as usize
                        && !(block.start as usize <= ip && ip < block.end as usize)
                }) {
                    Err(format!(
                        "字节码函数 {function_index} 在位置 {ip} 跳入清理块中部"
                    ))
                } else {
                    Ok(destination)
                }
            };
            match instruction {
                Instruction::PushConstant(index) if *index as usize >= self.constants.len() => {
                    return Err(format!(
                        "字节码函数 {function_index} 在位置 {ip} 常量索引越界"
                    ));
                }
                Instruction::MakeClosure(index) if *index as usize >= self.functions.len() => {
                    return Err(format!(
                        "字节码函数 {function_index} 在位置 {ip} 函数索引越界"
                    ));
                }
                Instruction::Jump(destination) => {
                    pending.push_back((validate_jump(target(*destination)?)?, next_state))
                }
                Instruction::Break {
                    loop_id,
                    target: destination,
                }
                | Instruction::Continue {
                    loop_id,
                    target: destination,
                } => {
                    let Some(loop_target) = function
                        .loop_targets
                        .iter()
                        .find(|target| target.id == *loop_id)
                    else {
                        return Err(format!(
                            "字节码函数 {function_index} 在位置 {ip} 循环身份无效"
                        ));
                    };
                    let expected = if matches!(instruction, Instruction::Break { .. }) {
                        loop_target.break_target
                    } else {
                        loop_target.continue_target
                    };
                    if *destination != expected {
                        return Err(format!(
                            "字节码函数 {function_index} 在位置 {ip} 循环跳转目标无效"
                        ));
                    }
                    pending.push_back((validate_jump(target(*destination)?)?, next_state));
                }
                Instruction::JumpIfFalse(destination) | Instruction::JumpIfTrue(destination) => {
                    pending.push_back((validate_jump(target(*destination)?)?, next_state));
                    if ip + 1 < length {
                        pending.push_back((ip + 1, next_state));
                    }
                }
                Instruction::NextIterator { end, .. } => {
                    let base_environment = function
                        .loop_targets
                        .iter()
                        .find(|target| target.continue_target as usize == ip)
                        .map(|target| target.environment_depth as usize)
                        .ok_or_else(|| {
                            format!("字节码函数 {function_index} 在位置 {ip} 的遍历缺少循环目标")
                        })?;
                    if state.environment < base_environment {
                        return Err(format!(
                            "字节码函数 {function_index} 在位置 {ip} 的遍历作用域深度无效"
                        ));
                    }
                    pending.push_back((
                        validate_jump(target(*end)?)?,
                        State {
                            environment: base_environment,
                            ..next_state
                        },
                    ));
                    if ip + 1 < length {
                        pending.push_back((
                            ip + 1,
                            State {
                                environment: base_environment + 1,
                                ..next_state
                            },
                        ));
                    }
                }
                Instruction::CallCleanup {
                    target: cleanup,
                    reason,
                } => {
                    let cleanup = target(*cleanup)?;
                    if !function
                        .cleanup_blocks
                        .iter()
                        .any(|block| block.start as usize == cleanup)
                    {
                        return Err(format!(
                            "字节码函数 {function_index} 在位置 {ip} 清理目标无效"
                        ));
                    }
                    if let CompletionReason::Break(loop_id) | CompletionReason::Continue(loop_id) =
                        reason
                    {
                        if !function
                            .loop_targets
                            .iter()
                            .any(|target| target.id == *loop_id)
                        {
                            return Err(format!(
                                "字节码函数 {function_index} 在位置 {ip} 的完成原因循环身份无效"
                            ));
                        }
                    }
                    pending.push_back((cleanup, next_state));
                    if ip + 1 < length {
                        pending.push_back((ip + 1, state));
                    }
                }
                Instruction::Return | Instruction::Halt => {
                    if next_environment != 0 || next_task_groups.is_some_and(|depth| depth != 0) {
                        return Err(format!(
                            "字节码函数 {function_index} 在位置 {ip} 终止时仍有活动作用域或任务组"
                        ));
                    }
                }
                Instruction::Throw | Instruction::FinishCleanup | Instruction::Rethrow => {}
                _ if ip + 1 < length => pending.push_back((ip + 1, next_state)),
                _ => return Err(format!("字节码函数 {function_index} 缺少终止指令")),
            }
        }
        if !function
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Halt | Instruction::Return))
        {
            return Err(format!("字节码函数 {function_index} 缺少终止指令"));
        }
        Ok(())
    }
}

fn stack_effect(instruction: &Instruction) -> (usize, usize) {
    match instruction {
        Instruction::PushConstant(_)
        | Instruction::LoadName(..)
        | Instruction::LoadInternal(_)
        | Instruction::MakeClosure(_) => (0, 1),
        Instruction::Define { .. }
        | Instruction::AssignName(..)
        | Instruction::StoreInternal(_)
        | Instruction::Pop => (1, 0),
        Instruction::Dup => (1, 2),
        Instruction::BuildList(count) => (*count as usize, 1),
        Instruction::BuildRecord(keys) => (keys.len(), 1),
        Instruction::Unary(_)
        | Instruction::ToBool
        | Instruction::IsType(_)
        | Instruction::GetAttr(_) => (1, 1),
        Instruction::Binary(_) | Instruction::GetIndex => (2, 1),
        Instruction::SetAttr(_) => (2, 0),
        Instruction::SetIndex => (3, 0),
        Instruction::Jump(_)
        | Instruction::Break { .. }
        | Instruction::Continue { .. }
        | Instruction::NextIterator { .. }
        | Instruction::EndIterator => (0, 0),
        Instruction::CallCleanup { reason, .. } => {
            (usize::from(matches!(reason, CompletionReason::Return)), 0)
        }
        Instruction::JumpIfFalse(_) | Instruction::JumpIfTrue(_) | Instruction::StartIterator => {
            (1, 0)
        }
        Instruction::Call {
            positional, named, ..
        } => (1 + *positional as usize + named.len(), 1),
        Instruction::Return | Instruction::Throw => (1, 0),
        Instruction::DefineMethod { .. } => (1, 0),
        Instruction::Require { has_message, .. } => (1 + usize::from(*has_message), 0),
        Instruction::EnterScope
        | Instruction::ExitScope
        | Instruction::DefineStruct { .. }
        | Instruction::Import { .. }
        | Instruction::DeclareGlobal(_)
        | Instruction::BindException(_)
        | Instruction::FinishCleanup
        | Instruction::Rethrow
        | Instruction::BeginTaskGroup
        | Instruction::FinishTaskGroup
        | Instruction::CancelTaskGroup
        | Instruction::Halt => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(instructions: Vec<Instruction>) -> BytecodeProgram {
        BytecodeProgram {
            version: BYTECODE_VERSION,
            constants: vec![Constant::Null],
            functions: vec![FunctionPrototype {
                name: "主程序".into(),
                self_binding: None,
                params: Vec::new(),
                captures: Vec::new(),
                source_lines: vec![1; instructions.len()],
                instructions,
                exception_handlers: Vec::new(),
                cleanup_blocks: Vec::new(),
                loop_targets: Vec::new(),
            }],
            exports: Vec::new(),
            module_imports: BTreeMap::new(),
        }
    }

    #[test]
    fn verifier_rejects_bad_constant_jump_handler_and_stack_underflow() {
        assert!(
            program(vec![Instruction::PushConstant(2), Instruction::Halt])
                .verify()
                .unwrap_err()
                .contains("常量索引")
        );
        assert!(program(vec![Instruction::Jump(8), Instruction::Halt])
            .verify()
            .unwrap_err()
            .contains("跳转越界"));
        assert!(
            program(vec![Instruction::Binary("+".into()), Instruction::Halt])
                .verify()
                .unwrap_err()
                .contains("栈下溢")
        );
        let mut bad = program(vec![Instruction::Halt]);
        bad.functions[0].exception_handlers.push(ExceptionHandler {
            start: 0,
            end: 2,
            target: 0,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        });
        assert!(bad.verify().unwrap_err().contains("异常区间"));

        let mut bad_depth = program(vec![Instruction::PushConstant(0), Instruction::Halt]);
        bad_depth.constants.push(Constant::Null);
        bad_depth.functions[0]
            .exception_handlers
            .push(ExceptionHandler {
                start: 0,
                end: 1,
                target: 1,
                kind: HandlerKind::Catch,
                environment_depth: 1,
            });
        assert!(bad_depth.verify().unwrap_err().contains("作用域深度"));

        let mut missing_finish = program(vec![Instruction::Halt]);
        missing_finish.functions[0]
            .cleanup_blocks
            .push(CleanupBlock { start: 0, end: 1 });
        assert!(missing_finish
            .verify()
            .unwrap_err()
            .contains("清理块缺少完成指令"));

        let invalid_cleanup_target = program(vec![
            Instruction::CallCleanup {
                target: 1,
                reason: CompletionReason::Normal,
            },
            Instruction::Halt,
        ]);
        assert!(invalid_cleanup_target
            .verify()
            .unwrap_err()
            .contains("清理目标无效"));

        let mut jump_into_cleanup = program(vec![
            Instruction::Jump(2),
            Instruction::PushConstant(0),
            Instruction::FinishCleanup,
            Instruction::Halt,
        ]);
        jump_into_cleanup.functions[0]
            .cleanup_blocks
            .push(CleanupBlock { start: 1, end: 3 });
        assert!(jump_into_cleanup
            .verify()
            .unwrap_err()
            .contains("跳入清理块中部"));

        let bad_scope = program(vec![Instruction::ExitScope, Instruction::Halt]);
        assert!(bad_scope.verify().unwrap_err().contains("作用域栈下溢"));

        let active_scope = program(vec![Instruction::EnterScope, Instruction::Halt]);
        assert!(active_scope.verify().unwrap_err().contains("活动作用域"));

        let bad_task_group = program(vec![Instruction::FinishTaskGroup, Instruction::Halt]);
        assert!(bad_task_group
            .verify()
            .unwrap_err()
            .contains("任务组栈下溢"));

        let mut bad_loop = program(vec![
            Instruction::Break {
                loop_id: LoopId(7),
                target: 1,
            },
            Instruction::Halt,
        ]);
        bad_loop.functions[0].loop_targets.push(LoopTarget {
            id: LoopId(8),
            continue_target: 0,
            break_target: 1,
            environment_depth: 0,
        });
        assert!(bad_loop.verify().unwrap_err().contains("循环身份无效"));

        let stray_finish = program(vec![Instruction::FinishCleanup, Instruction::Halt]);
        assert!(stray_finish
            .verify()
            .unwrap_err()
            .contains("没有完成原因槽"));
    }
}
