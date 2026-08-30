use super::{cancelled_error, resource_error, timed_out_error, Interpreter, RuntimeError};
use crate::value::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

impl Interpreter {
    pub fn begin_execution(&mut self) {
        self.started_at = Instant::now();
        self.output_bytes = 0;
    }

    pub(super) fn check_limits(&self) -> Result<(), String> {
        if self
            .permissions
            .execution_timeout_ms
            .is_some_and(|limit| self.started_at.elapsed().as_millis() > u128::from(limit))
        {
            Err(timed_out_error("程序执行超过时间限制"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn check_value_limit(&self, value: &Value, line: usize) -> Result<(), RuntimeError> {
        crate::value::ensure_collection_limit_with_depth(
            value,
            self.permissions.collection_limit(),
            self.permissions.value_nesting_limit(),
        )
        .map_err(|message| RuntimeError::new(line, message).with_category("资源错误"))
    }

    pub(crate) fn check_collection_growth(
        &self,
        current: usize,
        additional: usize,
    ) -> Result<(), String> {
        current
            .checked_add(additional)
            .filter(|size| *size <= self.permissions.collection_limit())
            .map(|_| ())
            .ok_or_else(|| resource_error("集合项数超过资源限制"))
    }

    pub(crate) fn check_operation_limits(&self) -> Result<(), String> {
        self.check_limits()?;
        if self
            .cancellation
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Relaxed))
        {
            Err(cancelled_error("任务已经取消"))
        } else {
            Ok(())
        }
    }

    /// 原生集合循环不经过语句执行，必须自行按固定间隔检查执行预算与取消令牌，
    /// 否则百万项的比较或去重会在预算耗尽后继续独占CPU。
    pub(crate) fn check_native_loop(&self, processed: usize) -> Result<(), String> {
        const NATIVE_LOOP_CHECK_INTERVAL: usize = 1024;
        if processed.is_multiple_of(NATIVE_LOOP_CHECK_INTERVAL) {
            self.check_operation_limits()?;
        }
        Ok(())
    }

    pub(crate) fn check_call_depth(&self, depth: usize, line: usize) -> Result<(), RuntimeError> {
        if depth >= self.permissions.call_depth_limit() {
            Err(RuntimeError::new(line, "功能调用深度超过资源限制".into())
                .with_category("资源错误"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn enter_call(&mut self, name: String, line: usize) -> Result<(), RuntimeError> {
        self.check_call_depth(self.call_stack.len(), line)?;
        self.call_stack.push(name);
        Ok(())
    }

    pub(crate) fn leave_call(&mut self) {
        self.call_stack.pop();
    }

    pub fn set_cancellation(&mut self, cancellation: Arc<AtomicBool>) {
        self.cancellation = Some(cancellation);
    }

    pub(crate) fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
        self.cancellation.clone()
    }

    pub(crate) fn blocking_sleep(&self, milliseconds: u64) -> Result<(), String> {
        let requested = Duration::from_millis(milliseconds);
        let deadline = self
            .permissions
            .execution_timeout_ms
            .map(|limit| {
                self.started_at
                    .checked_add(Duration::from_millis(limit))
                    .ok_or_else(|| self.err("程序执行时间限制超出支持范围"))
            })
            .transpose()?;
        let target = Instant::now()
            .checked_add(requested)
            .ok_or_else(|| self.err("休眠时间超出支持范围"))?;
        loop {
            self.check_limits()?;
            if self
                .cancellation
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(Ordering::Relaxed))
            {
                return Err(cancelled_error("任务已经取消"));
            }
            let now = Instant::now();
            if now >= target {
                return Ok(());
            }
            let wake = deadline.map_or(target, |deadline| deadline.min(target));
            if now >= wake {
                return Err(timed_out_error("程序执行超过时间限制"));
            }
            std::thread::sleep((wake - now).min(Duration::from_millis(10)));
        }
    }

    pub(crate) fn remaining_execution_time(
        &self,
        line: usize,
    ) -> Result<Option<Duration>, RuntimeError> {
        let Some(limit) = self.permissions.execution_timeout_ms else {
            return Ok(None);
        };
        let total = Duration::from_millis(limit);
        total
            .checked_sub(self.started_at.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .map(Some)
            .ok_or_else(|| {
                RuntimeError::new(line, "程序执行超过时间限制".into())
                    .with_category("资源错误")
                    .with_termination(crate::execution::RunTermination::TimedOut)
            })
    }

    pub(crate) fn blocking_timeout(
        &self,
        requested_ms: Option<u64>,
        line: usize,
    ) -> Result<Option<Duration>, RuntimeError> {
        let requested = requested_ms.map(Duration::from_millis);
        let remaining = self.remaining_execution_time(line)?;
        Ok(match (requested, remaining) {
            (Some(requested), Some(remaining)) => Some(requested.min(remaining)),
            (Some(requested), None) => Some(requested),
            (None, remaining) => remaining,
        })
    }

    /// 阻塞等待用总预算的剩余时间作为超时，超时后必须区分两种原因：
    /// 总预算耗尽时原样传播结构化资源错误，只有局部等待超时才用局部说明。
    pub(crate) fn blocking_timeout_error(&self, line: usize, local: &str) -> RuntimeError {
        self.remaining_execution_time(line)
            .err()
            .unwrap_or_else(|| RuntimeError::new(line, local.into()).with_category("资源错误"))
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::interpreter::{execute, execute_with_permissions};
    use crate::security::PermissionSet;
    use std::fs;
    use std::path::Path;
    use std::time::Instant;

    #[test]
    fn recursive_calls_stop_at_language_depth_budget() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.max_call_depth = Some(8);
        let error = execute_with_permissions(
            "定义功能 无限（）：\n返回 无限()\n结束\n无限()",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .expect("递归应该触发调用深度限制");
        assert!(error.contains("功能调用深度超过资源限制"), "{error}");
    }

    #[test]
    fn value_nesting_budget_is_shared_by_source_runtime_checks() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.max_value_nesting = Some(1);
        let error =
            execute_with_permissions("定义 值 为 [[1]]", ".", false, Vec::new(), permissions)
                .err()
                .expect("嵌套值应该触发资源限制");
        assert!(error.contains("值的嵌套层级超过限制"), "{error}");
    }

    #[test]
    fn task_workers_inherit_call_and_value_resource_budgets() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.max_call_depth = Some(6);
        permissions.max_value_nesting = Some(2);
        let error = execute_with_permissions(
            "引入 并发\n定义功能 无限（）：\n返回 无限()\n结束\n定义 任务 为 并发 启动(无限)\n任务 等待()",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .expect("任务应继承调用深度预算");
        assert!(error.contains("功能调用深度超过资源限制"), "{error}");
    }

    #[test]
    fn resource_limits_stop_time_output_and_large_collections() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.max_output_bytes = Some(4);
        let output_error = execute_with_permissions(
            "输出(\"1234\")",
            ".",
            false,
            Vec::new(),
            permissions.clone(),
        )
        .err()
        .unwrap();
        assert!(output_error.contains("最大输出字节"));

        permissions.max_output_bytes = None;
        permissions.max_collection_items = Some(2);
        let collection_error = execute_with_permissions(
            "定义 数据 为 [1, 2, 3]",
            ".",
            false,
            Vec::new(),
            permissions.clone(),
        )
        .err()
        .unwrap();
        assert!(collection_error.contains("集合项数"));

        let range_error = execute_with_permissions(
            "遍历 范围(1, 4)，记作 值：\n结束",
            ".",
            false,
            Vec::new(),
            permissions.clone(),
        )
        .err()
        .unwrap();
        assert!(range_error.contains("集合项数"), "{range_error}");

        for source in [
            "引入 财务\n财务 敏感性分析(100, 范围(1, 4))",
            "引入 统计分析\n统计分析 数组(范围(1, 4))",
            "引入 编码\n编码 转字节(范围(1, 4))",
        ] {
            let error =
                execute_with_permissions(source, ".", false, Vec::new(), permissions.clone())
                    .err()
                    .unwrap();
            assert!(error.contains("集合项数"), "源码：{source}\n错误：{error}");
        }

        permissions.max_collection_items = None;
        permissions.execution_timeout_ms = Some(1);
        let timeout_error =
            execute_with_permissions("当 是 时重复：\n结束", ".", false, Vec::new(), permissions)
                .err()
                .unwrap();
        assert!(timeout_error.contains("时间限制"));
    }

    #[test]
    fn imported_modules_inherit_buffer_limits() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("模块.代码"), "定义 数值 为 123\n").unwrap();
        let mut permissions = PermissionSet::for_source(directory.path());
        permissions.max_buffer_bytes = Some(4);
        let error = execute_with_permissions(
            "引入 模块",
            directory.path(),
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(error.contains("缓冲上限"));
    }

    #[test]
    fn blocking_sleep_obeys_total_execution_timeout() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.execution_timeout_ms = Some(10);
        let started = Instant::now();
        let error = execute_with_permissions(
            "引入 并发\n并发 休眠(200)",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(error.contains("时间限制"));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn blocking_channel_receive_obeys_total_execution_timeout() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.execution_timeout_ms = Some(10);
        let started = Instant::now();
        let error = execute_with_permissions(
            "引入 并发\n定义 通道 为 并发 通道()\n通道 接收()",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(
            error.contains("超时") || error.contains("时间限制"),
            "{error}"
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(100));

        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.execution_timeout_ms = Some(10);
        let send_error = execute_with_permissions(
            "引入 并发\n定义 通道 为 并发 通道(1)\n通道 发送(1)\n通道 发送(2)",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(
            send_error.contains("超时") || send_error.contains("时间限制"),
            "{send_error}"
        );

        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.execution_timeout_ms = Some(10);
        let stream_error = execute_with_permissions(
            "引入 并发\n定义 通道 为 并发 通道()\n定义 数据流 为 并发 通道转流(通道)\n数据流 下一个()",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(
            stream_error.contains("超时") || stream_error.contains("时间限制"),
            "{stream_error}"
        );
    }

    #[test]
    fn channel_capacity_and_parallel_workers_stay_within_the_budget() {
        let over_limit = execute("引入 并发\n并发 通道(65537)", ".", false)
            .err()
            .unwrap();
        assert!(
            over_limit.contains("通道容量超过资源上限 65536"),
            "{over_limit}"
        );

        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.max_channel_capacity = Some(4);
        let tightened = execute_with_permissions(
            "引入 并发\n并发 通道(8)",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(tightened.contains("通道容量超过资源上限 4"), "{tightened}");

        // 工作者上限只收紧线程数，不改变结果：请求百万工作者仍按预算封顶后正常完成。
        let interpreter = execute(
            "引入 并发\n定义功能 加倍（值）：\n返回 值 * 2\n结束\n输出(并发 并行映射([1, 2, 3], 加倍, 工作者 为 1000000))",
            ".",
            false,
        )
        .unwrap();
        assert_eq!(interpreter.output, vec!["[2, 4, 6]"]);
    }

    #[test]
    fn native_collection_loops_honor_the_execution_budget() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.execution_timeout_ms = Some(200);
        let started = Instant::now();
        let error = execute_with_permissions(
            "定义 结果 为 去重(范围(1, 40000))\n输出(长度(结果))",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(error.contains("时间限制"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn native_set_algebra_loops_honor_the_execution_budget() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.execution_timeout_ms = Some(200);
        let started = Instant::now();
        let error = execute_with_permissions(
            "定义 甲 为 集合([1, 2, 3])\n定义 结果 为 甲 并集(范围(1, 40000))\n输出(长度(结果))",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap();
        assert!(error.contains("时间限制"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn extreme_blocking_and_allocation_sizes_return_errors() {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.execution_timeout_ms = Some(10);
        assert!(execute_with_permissions(
            "引入 并发\n并发 休眠(持续时间(\"9223372036854775807毫秒\"))",
            ".",
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .unwrap()
        .contains("时间限制"));
        assert!(execute(
            "引入 统计分析\n定义 数据 为 统计分析 数组([1, 2])\n数据 直方图(18446744073709551615)",
            ".",
            false
        )
        .err()
        .unwrap()
        .contains("分组数"));
        assert!(execute(
            "引入 密码\n密码 安全随机字节(18446744073709551615)",
            ".",
            false
        )
        .err()
        .unwrap()
        .contains("随机字节长度"));
    }
}
