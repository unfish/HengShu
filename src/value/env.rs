//! 绑定环境：作用域链、绑定解析、全局声明与结构类型约束。
//!
//! 值本身是不可变数据，环境才是可变状态。两者分开，`value/mod.rs` 就只描述“值是什么”，
//! 本模块只描述“名字在哪一层解析成哪个值”。

use super::equality::value_matches_type;
use super::{StructType, Value, BUILTIN_TYPE_NAMES};
use crate::ast::{BindingId, BindingOperand, TypeId, TypeOperand};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

pub type EnvRef = Rc<RefCell<Env>>;

pub struct Env {
    pub parent: Option<EnvRef>,
    pub function_scope: bool,
    /// 模块或程序的根绑定层，`全局`只解析到最近的根绑定层。
    pub module_root: bool,
    pub values: BTreeMap<String, Value>,
    pub resolved_values: BTreeMap<BindingId, (String, Value)>,
    pub global_bindings: BTreeSet<BindingId>,
    pub resolved_type_constraints: BTreeMap<BindingId, TypeConstraint>,
    pub module_id: usize,
}

static NEXT_MODULE_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
pub enum TypeConstraint {
    Named(String),
    Struct(TypeId, String),
}

impl TypeConstraint {
    fn matches(&self, value: &Value) -> bool {
        match self {
            Self::Named(expected) => value_matches_type(value, expected),
            Self::Struct(expected, _) => matches!(
                value,
                Value::StructInstance(instance)
                    if instance.borrow().struct_type.id == *expected
            ),
        }
    }

    fn name(&self) -> String {
        match self {
            Self::Named(expected) => expected.clone(),
            Self::Struct(_, name) => name.clone(),
        }
    }
}

impl Env {
    pub fn new(parent: Option<EnvRef>) -> EnvRef {
        Self::new_with_kind(parent, false, false)
    }
    pub fn new_function(parent: Option<EnvRef>) -> EnvRef {
        Self::new_with_kind(parent, true, false)
    }
    pub fn new_module(parent: Option<EnvRef>) -> EnvRef {
        Self::new_with_kind(parent, false, true)
    }
    fn new_with_kind(parent: Option<EnvRef>, function_scope: bool, new_module: bool) -> EnvRef {
        let module_id = if new_module {
            NEXT_MODULE_ID.fetch_add(1, AtomicOrdering::Relaxed)
        } else {
            parent.as_ref().map_or_else(
                || NEXT_MODULE_ID.fetch_add(1, AtomicOrdering::Relaxed),
                |parent| parent.borrow().module_id,
            )
        };
        Rc::new(RefCell::new(Self {
            module_root: new_module || parent.is_none(),
            parent,
            function_scope,
            values: BTreeMap::new(),
            resolved_values: BTreeMap::new(),
            global_bindings: BTreeSet::new(),
            resolved_type_constraints: BTreeMap::new(),
            module_id,
        }))
    }
    pub fn get(env: &EnvRef, name: &str) -> Result<Value, String> {
        if let Some(v) = env.borrow().values.get(name) {
            return Ok(v.clone());
        }
        if let Some((_, value)) = env
            .borrow()
            .resolved_values
            .values()
            .find(|(binding_name, _)| binding_name == name)
        {
            return Ok(value.clone());
        }
        if let Some(parent) = env.borrow().parent.clone() {
            return Self::get(&parent, name);
        }
        Err(format!(
            "变量“{name}”尚未定义。\n建议：先使用「定义 {name} 为 ...」定义它。"
        ))
    }

    pub fn get_resolved(
        env: &EnvRef,
        binding: Option<&BindingOperand>,
        name: &str,
    ) -> Result<Value, String> {
        let Some(binding) = binding else {
            return Self::get(env, name);
        };
        if let Some((_, value)) = env.borrow().resolved_values.get(&binding.id) {
            return Ok(value.clone());
        }
        if let Some(parent) = env.borrow().parent.clone() {
            return Self::get_resolved(&parent, Some(binding), name);
        }
        Err(format!(
            "变量“{name}”尚未定义。\n建议：先使用「定义 {name} 为 ...」定义它。"
        ))
    }
    pub fn define_resolved(
        env: &EnvRef,
        binding: &BindingOperand,
        value: Value,
        type_constraint: Option<&TypeOperand>,
    ) -> Result<(), String> {
        if env.borrow().resolved_values.contains_key(&binding.id) {
            return Err(format!(
                "变量“{}”已经定义过了。\n建议：修改变量请直接写「{} 为 ...」。",
                binding.name, binding.name
            ));
        }
        if let Some(expected) = type_constraint {
            let resolved = Self::resolve_type_constraint(env, expected)?;
            if !resolved.matches(&value) {
                return Err(format!(
                    "变量“{}”要求是{}，实际是{}。",
                    binding.name,
                    expected.name,
                    value.actual_type_display(&expected.name)
                ));
            }
            env.borrow_mut()
                .resolved_type_constraints
                .insert(binding.id, resolved);
        }
        env.borrow_mut()
            .resolved_values
            .insert(binding.id, (binding.name.clone(), value));
        Ok(())
    }
    pub fn assign_resolved(
        env: &EnvRef,
        binding: Option<&BindingOperand>,
        name: &str,
        value: Value,
    ) -> Result<(), String> {
        let Some(binding) = binding else {
            return Err(format!(
                "变量“{name}”还没有定义。\n建议：先使用「定义 {name} 为 ...」定义它。"
            ));
        };
        if env.borrow().resolved_values.contains_key(&binding.id) {
            Self::check_resolved_assignment(env, binding, &value)?;
            env.borrow_mut()
                .resolved_values
                .insert(binding.id, (binding.name.clone(), value));
            return Ok(());
        }
        let (parent, function_scope, global) = {
            let current = env.borrow();
            (
                current.parent.clone(),
                current.function_scope,
                current.global_bindings.contains(&binding.id),
            )
        };
        if global {
            return Self::assign_root_resolved(env, binding, value);
        }
        if let Some(parent) = parent {
            if !function_scope {
                return Self::assign_resolved(&parent, Some(binding), name, value);
            }
            if Self::has_resolved(&parent, binding.id) {
                return Err(format!(
                    "功能内部不能直接修改外部变量“{name}”。\n建议：请先写「全局 {name}」。"
                ));
            }
        }
        Err(format!(
            "变量“{name}”还没有定义。\n建议：先使用「定义 {name} 为 ...」定义它。"
        ))
    }

    fn has_resolved(env: &EnvRef, binding: BindingId) -> bool {
        if env.borrow().resolved_values.contains_key(&binding) {
            true
        } else {
            env.borrow()
                .parent
                .as_ref()
                .is_some_and(|parent| Self::has_resolved(parent, binding))
        }
    }

    fn check_resolved_assignment(
        env: &EnvRef,
        binding: &BindingOperand,
        value: &Value,
    ) -> Result<(), String> {
        if let Some(expected) = env
            .borrow()
            .resolved_type_constraints
            .get(&binding.id)
            .cloned()
        {
            if !expected.matches(value) {
                let expected_name = expected.name();
                return Err(format!(
                    "变量“{}”要求是{}，实际是{}。",
                    binding.name,
                    expected_name,
                    value.actual_type_display(&expected_name)
                ));
            }
        }
        Ok(())
    }

    pub fn declare_global_bindings(env: &EnvRef, bindings: &[BindingOperand]) {
        let mut current = env.clone();
        loop {
            let (function_scope, parent) = {
                let current = current.borrow();
                (current.function_scope, current.parent.clone())
            };
            if function_scope || parent.is_none() {
                current
                    .borrow_mut()
                    .global_bindings
                    .extend(bindings.iter().map(|binding| binding.id));
                return;
            }
            if let Some(parent) = parent {
                current = parent;
            } else {
                return;
            }
        }
    }

    fn assign_root_resolved(
        env: &EnvRef,
        binding: &BindingOperand,
        value: Value,
    ) -> Result<(), String> {
        let mut root = env.clone();
        loop {
            let (module_root, parent) = {
                let current = root.borrow();
                (current.module_root, current.parent.clone())
            };
            if module_root {
                break;
            }
            let Some(parent) = parent else {
                break;
            };
            root = parent;
        }
        if !root.borrow().resolved_values.contains_key(&binding.id) {
            return Err(format!("全局变量“{}”尚未定义。", binding.name));
        }
        Self::check_resolved_assignment(&root, binding, &value)?;
        root.borrow_mut()
            .resolved_values
            .insert(binding.id, (binding.name.clone(), value));
        Ok(())
    }

    pub fn value_matches_type(
        env: &EnvRef,
        value: &Value,
        expected: &TypeOperand,
    ) -> Result<bool, String> {
        Ok(Self::resolve_type_constraint(env, expected)?.matches(value))
    }

    fn resolve_type_constraint(
        env: &EnvRef,
        expected: &TypeOperand,
    ) -> Result<TypeConstraint, String> {
        if expected.id.is_none()
            && (expected.name == "任意" || BUILTIN_TYPE_NAMES.contains(&expected.name.as_str()))
        {
            return Ok(TypeConstraint::Named(expected.name.clone()));
        }
        let value = if let Some(id) = expected.id {
            Self::find_struct_type(env, id)
                .map(Value::StructType)
                .ok_or_else(|| format!("类型“{}”的语义身份未注册。", expected.name))?
        } else if let Some((module_name, type_name)) = expected.name.split_once('.') {
            match Self::get(env, module_name)? {
                Value::Module(module) => module
                    .public_value(type_name)
                    .cloned()
                    .ok_or_else(|| format!("模块“{module_name}”没有类型“{type_name}”。"))?,
                _ => {
                    return Err(format!("“{module_name}”不是模块。"));
                }
            }
        } else {
            Self::get(env, &expected.name)?
        };
        match value {
            Value::StructType(struct_type) => Ok(TypeConstraint::Struct(
                struct_type.id,
                struct_type.name.clone(),
            )),
            _ => Err(format!("“{}”不是结构类型。", expected.name)),
        }
    }

    fn find_struct_type(env: &EnvRef, id: TypeId) -> Option<Rc<StructType>> {
        let found = env
            .borrow()
            .resolved_values
            .values()
            .find_map(|(_, value)| match value {
                Value::StructType(struct_type) if struct_type.id == id => Some(struct_type.clone()),
                _ => None,
            });
        found.or_else(|| {
            env.borrow()
                .parent
                .as_ref()
                .and_then(|parent| Self::find_struct_type(parent, id))
        })
    }

    pub fn get_struct_type(env: &EnvRef, expected: &TypeOperand) -> Result<Rc<StructType>, String> {
        match Self::resolve_type_constraint(env, expected)? {
            TypeConstraint::Struct(id, _) => Self::find_struct_type(env, id)
                .ok_or_else(|| format!("类型“{}”的语义身份未注册。", expected.name)),
            TypeConstraint::Named(_) => Err(format!("“{}”不是结构类型。", expected.name)),
        }
    }

    pub fn value_matches_dynamic_type(
        env: &EnvRef,
        value: &Value,
        expected: &str,
    ) -> Result<bool, String> {
        Self::value_matches_type(
            env,
            value,
            &TypeOperand {
                id: None,
                name: expected.to_string(),
            },
        )
    }
}
