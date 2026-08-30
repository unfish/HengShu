use super::{Interpreter, RuntimeError};
use crate::ast::MethodOperand;
use crate::value::*;
use std::collections::BTreeMap;
use std::rc::Rc;

impl Interpreter {
    pub fn invoke_function(
        &mut self,
        function: Value,
        args: Vec<Value>,
        line: usize,
    ) -> Result<Value, String> {
        self.call(function, args, BTreeMap::new(), line)
            .map_err(|error| error.message)
    }

    pub(crate) fn invoke_function_structured(
        &mut self,
        function: Value,
        args: Vec<Value>,
        line: usize,
    ) -> Result<Value, super::RuntimeError> {
        self.call(function, args, BTreeMap::new(), line)
    }

    pub(crate) fn vm_unary(
        &self,
        op: &str,
        value: Value,
        line: usize,
    ) -> Result<Value, RuntimeError> {
        self.unary(op, value, line)
    }

    pub(crate) fn vm_binary(&self, op: &str, left: Value, right: Value) -> Result<Value, String> {
        self.binary(op, left, right)
    }

    pub(crate) fn vm_get_attr(&self, value: &Value, name: &str) -> Result<Value, String> {
        self.get_attr(value, name)
    }

    pub(crate) fn vm_get_index(&self, value: &Value, index: &Value) -> Result<Value, String> {
        self.get_index(value, index)
    }

    pub(crate) fn vm_set_attr(
        &self,
        object: Value,
        name: &str,
        value: Value,
    ) -> Result<(), String> {
        self.set_attr_value(object, name, value)
    }

    pub(crate) fn vm_set_index(
        &self,
        object: Value,
        index: Value,
        value: Value,
    ) -> Result<(), String> {
        self.set_index_value(object, index, value)
    }

    pub(crate) fn vm_call_value_in_env(
        &mut self,
        value: Value,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
        env: &EnvRef,
    ) -> Result<Value, RuntimeError> {
        self.call_in_env(value, args, named, line, env)
    }

    pub(crate) fn vm_call_method(
        &mut self,
        value: Value,
        method: &MethodOperand,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
    ) -> Result<Value, RuntimeError> {
        self.call_method(value, method, args, named, line)
    }

    pub(crate) fn vm_builtin_module(&self, name: &str) -> Option<Rc<Module>> {
        self.builtin_module(name)
    }

    pub(crate) fn vm_issue_from_runtime_error(
        &self,
        error: super::RuntimeError,
        stack: Vec<String>,
    ) -> RuntimeIssue {
        error.into_issue(stack)
    }

    pub(crate) fn begin_task_group(&mut self) {
        self.task_groups.push(Vec::new());
    }

    pub(crate) fn new_env(&mut self, parent: Option<EnvRef>, function_scope: bool) -> EnvRef {
        let env = if function_scope {
            Env::new_function(parent)
        } else {
            Env::new(parent)
        };
        self.environments.track(&env);
        env
    }

    pub(crate) fn new_module_env(&mut self, parent: Option<EnvRef>) -> EnvRef {
        let env = Env::new_module(parent);
        self.environments.track(&env);
        env
    }

    pub(crate) fn register_task(&mut self, task: Rc<TaskValue>) {
        if let Some(group) = self.task_groups.last_mut() {
            group.push(task);
        }
    }

    pub(crate) fn finish_task_group(
        &mut self,
        exit: crate::concurrency::TaskGroupExit,
        line: usize,
    ) -> Option<RuntimeError> {
        let tasks = self.task_groups.pop().unwrap_or_default();
        crate::concurrency::settle_task_group(self, &tasks, exit, line)
    }

    pub(crate) fn vm_finish_task_group(&mut self, line: usize) -> Result<(), RuntimeError> {
        self.finish_task_group(crate::concurrency::TaskGroupExit::Normal, line)
            .map_or(Ok(()), Err)
    }

    pub(crate) fn vm_cancel_task_group(&mut self, line: usize) -> Option<RuntimeError> {
        self.finish_task_group(crate::concurrency::TaskGroupExit::Cancel, line)
    }

    pub(crate) fn vm_cancel_all_task_groups(&mut self, line: usize) -> Option<RuntimeError> {
        let mut primary: Option<RuntimeError> = None;
        while !self.task_groups.is_empty() {
            if let Some(error) = self.vm_cancel_task_group(line) {
                if let Some(primary) = &mut primary {
                    primary.append_cause(error);
                } else {
                    primary = Some(error);
                }
            }
        }
        primary
    }

    #[cfg(test)]
    pub(crate) fn active_task_group_count(&self) -> usize {
        self.task_groups.len()
    }
}
