//! 并发矩阵：任务组、并行映射、通道通信、任务取消、任务组异常聚合
//! 在三路径上的一致行为。

mod common;

use common::{assert_three_paths_failure, assert_three_paths_output};

#[test]
fn concurrency_matrix_task_group_waits_all() {
    let output = assert_three_paths_output(
        r#"引入 并发
引入 文件系统
文件系统 写入文本("产出甲.txt", "未完成")
文件系统 写入文本("产出乙.txt", "未完成")
任务组：
    并发 启动(定义功能（）：
        引入 并发
        引入 文件系统
        并发 休眠(200)
        文件系统 写入文本("产出甲.txt", "已完成")
    结束)
    并发 启动(定义功能（）：
        引入 并发
        引入 文件系统
        并发 休眠(200)
        文件系统 写入文本("产出乙.txt", "已完成")
    结束)
结束
输出(文件系统 读取文本("产出甲.txt"), 文件系统 读取文本("产出乙.txt"))"#,
    );
    assert_eq!(output, ["已完成 已完成"]);
}

/// 显式任务组接口必须把每个任务的结果按加入顺序收集回来，而不是丢弃结果。
#[test]
fn concurrency_matrix_explicit_task_group_collects_results_in_order() {
    let output = assert_three_paths_output(
        r#"引入 并发
定义 组 为 并发 任务组()
并发 加入任务组(组, 并发 启动(定义功能（）：
    引入 并发
    并发 休眠(150)
    返回 "甲"
结束))
并发 加入任务组(组, 并发 启动(定义功能（）：返回 "乙" 结束))
定义 结果 为 并发 结束任务组(组)
输出(长度(结果), 结果[1], 结果[2])"#,
    );
    assert_eq!(output, ["2 甲 乙"]);
}

#[test]
fn concurrency_matrix_parallel_map_preserves_order() {
    let output = assert_three_paths_output(
        r#"引入 并发
定义功能 加一（甲）：
    返回 甲 + 1
结束
定义 结果 为 并发 并行映射([10, 20, 30], 加一)
输出(结果[1], 结果[2], 结果[3])"#,
    );
    assert_eq!(output, ["11 21 31"]);
}

#[test]
fn concurrency_matrix_parallel_map_propagates_error() {
    let outcome = assert_three_paths_failure(
        r#"引入 并发
定义功能 炸（甲）：
    抛出 "失败"
结束
并发 并行映射([1], 炸)"#,
    );
    assert!(outcome.message().contains("失败"));
}

#[test]
fn concurrency_matrix_channel_send_receive() {
    let output = assert_three_paths_output(
        r#"引入 并发
定义 通 为 并发 通道(1)
通 发送("消息")
输出(通 接收())"#,
    );
    assert_eq!(output, ["消息"]);
}

#[test]
fn concurrency_matrix_task_wait_returns_result() {
    let output = assert_three_paths_output(
        r#"引入 并发
定义 任 为 并发 启动(定义功能（）：
    返回 42
结束)
输出(任 等待())"#,
    );
    assert_eq!(output, ["42"]);
}

#[test]
fn concurrency_matrix_task_group_exception_propagates() {
    let outcome = assert_three_paths_failure(
        r#"引入 并发
任务组：
    并发 启动(定义功能（）：
        抛出 "子任务出错"
    结束)
结束"#,
    );
    assert!(outcome.message().contains("子任务出错"));
}

#[test]
fn concurrency_matrix_detached_task_failure_is_reclaimed_at_program_end() {
    let outcome = assert_three_paths_failure(
        r#"引入 并发
并发 启动(定义功能（）：
    抛出 "游离任务出错"
结束)"#,
    );
    assert!(outcome.message().contains("游离任务出错"));
}

#[test]
fn concurrency_matrix_set_roundtrips_through_task() {
    let output = assert_three_paths_output(
        r#"引入 并发
定义功能 原样（值）：
    返回 值
结束
定义 结果 为 并发 并行映射([集合([3, 1, 2])], 原样)
输出(类型是(结果[1], "集合"), 长度(结果[1]))"#,
    );
    assert_eq!(output, ["是 3"]);
}
