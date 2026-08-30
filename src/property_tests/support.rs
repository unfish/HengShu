//! 属性测试共享的执行入口与取值策略。

use crate::compiler;
use crate::parser;
use crate::security::PermissionSet;
use crate::value::Value;
use crate::vm;
use proptest::prelude::*;
use std::collections::BTreeMap;

pub(super) fn run_vm(source: &str) -> Result<Vec<String>, String> {
    let program = parser::parse(source)?;
    let bytecode = compiler::compile(&program)?;
    Ok(vm::execute(
        &bytecode,
        BTreeMap::new(),
        ".",
        false,
        Vec::new(),
        PermissionSet::for_source(std::path::Path::new(".")),
    )?
    .output)
}

pub(super) fn structured_errors(
    source: &str,
    permissions: PermissionSet,
) -> (crate::runtime::RuntimeError, crate::runtime::RuntimeError) {
    let interpreter = crate::runtime::execute_with_options_structured(
        source,
        ".",
        false,
        Vec::new(),
        permissions.clone(),
        false,
    )
    .err()
    .unwrap_or_else(|| panic!("源码解释器应该返回错误：\n{source}"));
    let program = parser::parse(source).expect("源码应该可解析");
    let vm = vm::execute_structured(
        &compiler::compile(&program).expect("源码应该可编译"),
        BTreeMap::new(),
        ".",
        false,
        Vec::new(),
        permissions,
    )
    .err()
    .unwrap_or_else(|| panic!("VM 应该返回错误：\n{source}"));
    (interpreter, vm)
}

pub(super) fn configuration() -> ProptestConfig {
    ProptestConfig {
        cases: 96,
        max_shrink_iters: 2_000,
        ..ProptestConfig::default()
    }
}

fn leaf_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|value| Value::Number(value.into())),
        "[a-zA-Z0-9_]{0,24}".prop_map(Value::Text),
    ]
}

pub(super) fn structured_value() -> impl Strategy<Value = Value> {
    leaf_value().prop_recursive(3, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::list),
            prop::collection::btree_map("[a-z]{1,8}", inner, 0..8).prop_map(Value::record),
        ]
    })
}

pub(super) fn restricted_semantic_program() -> impl Strategy<Value = String> {
    prop_oneof![
        (-100_i16..=100, -100_i16..=100).prop_map(|(outer, inner)| format!(
            "定义 值 为 {outer}\n如果 是：\n定义 值 为 {inner}\n输出(值)\n结束\n输出(值)"
        )),
        (0_u8..=12).prop_map(|limit| format!(
            "定义 编号 为 0\n定义 总和 为 0\n当 编号 < {limit} 时重复：\n编号 为 编号 + 1\n如果 编号 % 2 = 0：\n继续下一轮\n结束\n总和 为 总和 + 编号\n结束\n输出(总和)"
        )),
        (-20_i16..=20, -20_i16..=20, -20_i16..=20).prop_map(
            |(base, captured, argument)| format!(
                "定义 基数 为 {base}\n定义功能 制作（捕获值）：\n返回 定义功能（参数）为 基数 + 捕获值 + 参数\n结束\n定义 闭包 为 制作({captured})\n输出(闭包({argument}))"
            )
        ),
        (1_i16..=20, 1_i16..=20).prop_map(|(first, second)| format!(
            "定义 次数 为 0\n定义功能 下一个（增量）：\n全局 次数\n次数 为 次数 + 增量\n返回 次数\n结束\n定义功能 合并（左，右）：\n返回 左 * 100 + 右\n结束\n输出(合并(下一个({first}), 下一个({second})), 次数)"
        )),
    ]
}
