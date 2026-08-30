use crate::value::{Env, EnvRef};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

pub(super) struct EnvironmentTracker {
    refs: RefCell<Vec<Weak<RefCell<Env>>>>,
    protected: RefCell<BTreeSet<usize>>,
}

impl EnvironmentTracker {
    pub(super) fn new(root: &EnvRef) -> Self {
        Self {
            refs: RefCell::new(vec![Rc::downgrade(root)]),
            protected: RefCell::new(BTreeSet::new()),
        }
    }

    pub(super) fn track(&self, env: &EnvRef) {
        self.refs.borrow_mut().push(Rc::downgrade(env));
    }

    pub(super) fn protect(&self, env: &EnvRef) {
        self.protected.borrow_mut().insert(Rc::as_ptr(env) as usize);
    }
}

impl Drop for EnvironmentTracker {
    fn drop(&mut self) {
        for env in self.refs.get_mut() {
            if let Some(env) = env.upgrade() {
                if self
                    .protected
                    .get_mut()
                    .contains(&(Rc::as_ptr(&env) as usize))
                {
                    continue;
                }
                let mut env = env.borrow_mut();
                env.values.clear();
                env.resolved_values.clear();
                env.parent = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::interpreter::execute;
    use crate::runtime::test_support::run;

    #[test]
    fn definition_errors_are_chinese() {
        let error = execute("定义 金额 为 1\n定义 金额 为 2", ".", false)
            .err()
            .unwrap();
        assert!(error.contains("已经定义过了"));
    }

    #[test]
    fn task_capture_respects_branch_scope_shadowing() {
        let source = "引入 并发\n定义 外部 为 7\n定义功能 工作（条件）：\n如果 条件：\n定义 外部 为 9\n结束\n返回 外部\n结束\n定义 任务 为 并发 启动(工作, [否])\n输出(任务 等待())";
        assert_eq!(run(source), vec!["7"]);
    }

    #[test]
    fn task_capture_errors_are_reported_before_worker_start() {
        let source = "引入 并发\n定义功能 辅助（）：\n返回 1\n结束\n定义功能 工作（）：\n返回 辅助()\n结束\n并发 启动(工作)";
        let error = execute(source, ".", false).err().unwrap();
        assert!(error.contains("任务捕获变量“辅助”不可传递"));
    }
}
