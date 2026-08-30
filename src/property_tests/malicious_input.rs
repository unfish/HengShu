//! 恶意输入：任意源码、任意字节与畸形字节码都必须被拒绝而不是崩溃。

use super::support::configuration;
use crate::api::CompiledProgram;
use crate::bytecode::{BytecodeProgram, FunctionPrototype, Instruction, BYTECODE_VERSION};
use crate::formatter::format_source;
use crate::parser;
use crate::runtime::Interpreter;
use crate::security::PermissionSet;
use crate::vm;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

proptest! {
    #![proptest_config(configuration())]

    #[test]
    fn formatter_is_idempotent_for_arbitrary_text(source in any::<String>()) {
        let formatted = format_source(&source);
        prop_assert_eq!(format_source(&formatted), formatted);
    }

    #[test]
    fn formatter_preserves_string_output(
        content in "[a-zA-Z0-9 (),:]{0,80}"
    ) {
        let source = format!("输出(\"{content}\")");
        let formatted = format_source(&source);
        let before = Interpreter::execute_source(&source, ".", false).unwrap().output;
        let after = Interpreter::execute_source(&formatted, ".", false).unwrap().output;
        prop_assert_eq!(after, before);
    }

    #[test]
    fn parser_never_panics_on_arbitrary_unicode(source in any::<String>()) {
        let result = catch_unwind(AssertUnwindSafe(|| parser::parse(&source)));
        prop_assert!(result.is_ok());
    }

    #[test]
    fn bytecode_loader_never_panics_on_arbitrary_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..4096)
    ) {
        let result = catch_unwind(AssertUnwindSafe(|| CompiledProgram::from_bytes(&bytes)));
        prop_assert!(result.is_ok());
    }
}

#[test]
fn malicious_source_corpus_does_not_panic() {
    let corpus = vec![
        String::new(),
        "\0\0\0".into(),
        "定义".into(),
        "输出(".repeat(128),
        "\"\\\\\\\"（，：".into(),
        "999999999999999999999999999999999999999999999999999999999999".into(),
        "匹配 1：\n当 [当前, ...其余]：\n结束".into(),
    ];
    for source in corpus {
        assert!(catch_unwind(AssertUnwindSafe(|| {
            crate::resource::on_compile_stack_scoped(|| parser::parse(&source)).unwrap()
        }))
        .is_ok());
    }
}

#[test]
fn malformed_bytecode_is_rejected_before_vm_execution_without_panicking() {
    let function = |instructions: Vec<Instruction>, source_lines: Vec<u32>| FunctionPrototype {
        name: "恶意功能".into(),
        self_binding: None,
        params: Vec::new(),
        captures: Vec::new(),
        instructions,
        source_lines,
        exception_handlers: Vec::new(),
        cleanup_blocks: Vec::new(),
        loop_targets: Vec::new(),
    };
    let programs = [
        BytecodeProgram {
            version: BYTECODE_VERSION,
            constants: Vec::new(),
            functions: Vec::new(),
            exports: Vec::new(),
            module_imports: BTreeMap::new(),
        },
        BytecodeProgram {
            version: BYTECODE_VERSION,
            constants: Vec::new(),
            functions: vec![function(
                vec![Instruction::PushConstant(99), Instruction::Halt],
                vec![1, 1],
            )],
            exports: Vec::new(),
            module_imports: BTreeMap::new(),
        },
        BytecodeProgram {
            version: BYTECODE_VERSION,
            constants: Vec::new(),
            functions: vec![function(vec![Instruction::Jump(99)], vec![1])],
            exports: Vec::new(),
            module_imports: BTreeMap::new(),
        },
        BytecodeProgram {
            version: BYTECODE_VERSION,
            constants: Vec::new(),
            functions: vec![function(
                vec![Instruction::Pop, Instruction::Halt],
                vec![1, 1],
            )],
            exports: Vec::new(),
            module_imports: BTreeMap::new(),
        },
    ];
    for program in programs {
        let result = catch_unwind(AssertUnwindSafe(|| {
            vm::execute(
                &program,
                BTreeMap::new(),
                ".",
                false,
                Vec::new(),
                PermissionSet::for_source(std::path::Path::new(".")),
            )
        }));
        assert!(result.is_ok(), "VM 不应因畸形字节码 panic");
        assert!(result.unwrap().is_err(), "畸形字节码必须被拒绝");
    }
}
