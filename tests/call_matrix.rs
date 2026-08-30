//! 调用矩阵：位置/具名/默认/重复/缺失/多余/副作用顺序、方法`自己`和结构构造。

mod common;

use common::{assert_three_paths_failure, assert_three_paths_output};

#[test]
fn call_matrix_positional_and_named_arguments() {
    let output = assert_three_paths_output(
        r#"定义功能 合计（甲，乙，丙）：
    返回 甲 + 乙 + 丙
结束
输出(合计(1, 2, 丙 为 3))"#,
    );
    assert_eq!(output, ["6"]);
}

#[test]
fn call_matrix_default_parameters() {
    let output = assert_three_paths_output(
        r#"定义功能 加（基础，增量 为 10）：
    返回 基础 + 增量
结束
输出(加(1), 加(1, 增量 为 5))"#,
    );
    assert_eq!(output, ["11 6"]);
}

#[test]
fn call_matrix_missing_argument_error() {
    let outcome = assert_three_paths_failure(
        r#"定义功能 需两个（甲，乙）：
    返回 甲 + 乙
结束
需两个(1)"#,
    );
    assert!(outcome.message().contains("参数"));
}

#[test]
fn call_matrix_extra_argument_error() {
    let outcome = assert_three_paths_failure(
        r#"定义功能 需一个（甲）：
    返回 甲
结束
需一个(1, 2)"#,
    );
    assert!(outcome.message().contains("参数"));
}

#[test]
fn call_matrix_argument_evaluation_order() {
    let output = assert_three_paths_output(
        r#"定义功能 标记（标签）：
    输出(标签)
    返回 标签
结束
定义功能 合计（甲，乙，丙）：
    返回 甲 + 乙 + 丙
结束
输出(合计(标记("一"), 标记("二"), 标记("三")))"#,
    );
    assert_eq!(output, ["一", "二", "三", "一二三"]);
}

#[test]
fn call_matrix_struct_construction_requires_all_fields() {
    let outcome = assert_three_paths_failure(
        r#"结构 点：
    横
    纵
结束
点(横 为 1)"#,
    );
    assert!(outcome.message().contains("参数") || outcome.message().contains("字段"));
}

#[test]
fn call_matrix_struct_rejects_unknown_field() {
    let outcome = assert_three_paths_failure(
        r#"结构 点：
    横
结束
点(横 为 1, 未知 为 2)"#,
    );
    assert!(outcome.message().contains("参数") || outcome.message().contains("未知"));
}

#[test]
fn call_matrix_method_self_binding() {
    let output = assert_three_paths_output(
        r#"结构 盒：
    值
结束
定义功能 盒 倍增（自己，倍数）：
    返回 自己 的 值 * 倍数
结束
定义 实例 为 盒(值 为 5)
输出(实例 倍增(3))"#,
    );
    assert_eq!(output, ["15"]);
}

#[test]
fn call_matrix_struct_rejects_positional_arguments() {
    let outcome = assert_three_paths_failure(
        r#"结构 单：
    值
结束
单(1)"#,
    );
    assert!(outcome.message().contains("参数") || outcome.message().contains("字段"));
}

#[test]
fn call_matrix_native_higher_order_callback_runs_user_function() {
    let output = assert_three_paths_output(
        r#"定义 列 为 [1, 2, 3]
定义功能 加倍（项）：
    返回 项 * 2
结束
定义功能 是偶数（项）：
    返回 项 % 2 == 0
结束
定义功能 累加（左，右）：
    返回 左 + 右
结束
输出(列 映射(加倍))
输出(列 筛选(是偶数))
输出(列 归并(累加, 0))"#,
    );
    assert_eq!(output, ["[2, 4, 6]", "[2]", "6"]);
}

#[test]
fn call_matrix_native_higher_order_callback_propagates_errors() {
    let outcome = assert_three_paths_failure(
        r#"定义 列 为 [1, 2, 3]
定义功能 出错（项）：
    返回 项 / 0
结束
输出(列 映射(出错))"#,
    );
    assert!(outcome.message().contains("零") || outcome.message().contains("除"));
}

#[test]
fn call_matrix_nested_higher_order_callbacks() {
    let output = assert_three_paths_output(
        r#"定义功能 加倍（项）：
    返回 项 * 2
结束
定义功能 处理（组）：
    返回 组 映射(加倍)
结束
输出([[1, 2], [3]] 映射(处理))"#,
    );
    assert_eq!(output, ["[[2, 4], [6]]"]);
}

#[test]
fn call_matrix_call_depth_limit_is_identical_on_every_path() {
    let outcome = assert_three_paths_failure(
        r#"定义功能 递归（数）：
    如果 数 <= 0：
        返回 0
    结束
    返回 1 + 递归(数 - 1)
结束
输出(递归(2000))"#,
    );
    assert_eq!(outcome.error_category(), Some("资源错误"));
    assert_eq!(outcome.message(), "功能调用深度超过资源限制");
}

#[test]
fn call_matrix_stream_callbacks_run_user_functions() {
    let output = assert_three_paths_output(
        r#"引入 文件系统
文件系统 写入文本("流回调.txt", "1\n2\n3\n4")
定义功能 转数（行）：
    返回 转数字(行)
结束
定义功能 是偶数（项）：
    返回 项 % 2 == 0
结束
定义 流水 为 文件系统 逐行读取("流回调.txt")
输出(流水 映射(转数) 筛选(是偶数) 收集())"#,
    );
    assert_eq!(output, ["[2, 4]"]);
}

#[test]
fn call_matrix_table_row_callback_runs_user_function() {
    let output = assert_three_paths_output(
        r#"引入 表格处理
定义 表 为 表格处理 把记录整理成表格([{"名称": "甲", "数量": 1}, {"名称": "乙", "数量": 2}])
定义功能 数量大于一（行）：
    返回 行["数量"] > 1
结束
输出(表 筛选(数量大于一) 转记录列表())"#,
    );
    assert_eq!(output, [r#"[{"名称": 乙, "数量": 2}]"#]);
}
