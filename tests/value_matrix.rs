//! 值矩阵：索引边界、未知字段、自引用容器、深层值嵌套预算、相等、显示、
//! 任务往返和模块往返的三路径一致性。集合项数上限需要收紧配置，由
//! `tests/public_api.rs`覆盖。

mod common;

use common::{assert_three_paths_failure, assert_three_paths_output};

#[test]
fn value_matrix_one_based_index_rejects_zero() {
    let outcome = assert_three_paths_failure("定义 列 为 [10, 20]\n输出(列[0])");
    assert!(outcome.message().contains("索引必须从 1 开始"));
}

/// 一基索引的赋值形式与读取形式必须共用同一条边界：写入路径单独实现时曾漏掉零。
#[test]
fn value_matrix_one_based_index_write_rejects_zero() {
    let outcome = assert_three_paths_failure("定义 列 为 [10, 20]\n列[0] 为 9");
    assert!(
        outcome.message().contains("索引必须从 1 开始"),
        "{}",
        outcome.message()
    );
}

#[test]
fn value_matrix_one_based_index_rejects_negative() {
    let outcome = assert_three_paths_failure("定义 列 为 [10, 20]\n输出(列[-1])");
    assert!(outcome.message().contains("下标") || outcome.message().contains("索引"));
}

#[test]
fn value_matrix_one_based_index_rejects_out_of_bounds() {
    let outcome = assert_three_paths_failure("定义 列 为 [10, 20]\n输出(列[3])");
    assert!(outcome.message().contains("下标"));
}

#[test]
fn value_matrix_one_based_read_and_write() {
    let output = assert_three_paths_output(
        r#"定义 列 为 [10, 20, 30]
输出(列[1], 列[2], 列[3])
列[2] 为 99
输出(列[2])"#,
    );
    assert_eq!(output, ["10 20 30", "99"]);
}

#[test]
fn value_matrix_record_index_write_matches_index_read() {
    let output = assert_three_paths_output(
        r#"定义 记录 为 {"键": 1}
记录["键"] 为 9
记录["新键"] 为 2
输出(记录["键"], 记录["新键"], 记录 的 键)"#,
    );
    assert_eq!(output, ["9 2 9"]);
}

/// 规范规定赋值先求右值、再求目标接收者：两条路径各自实现赋值时曾出现相反顺序，
/// 只有带副作用的目标与右值才能观察到差异。
#[test]
fn value_matrix_attribute_assignment_evaluates_value_before_target() {
    let output = assert_three_paths_output(
        r#"定义 记 为 []
定义功能 目标（）：
    记 加入("目标")
    返回 {甲: 0}
结束
定义功能 右值（）：
    记 加入("右值")
    返回 1
结束
目标() 的 甲 为 右值()
输出(记)"#,
    );
    assert_eq!(output, ["[右值, 目标]"]);
}

#[test]
fn value_matrix_index_write_on_unsupported_object_reports_the_object() {
    let outcome = assert_three_paths_failure("定义 数 为 1\n数[\"键\"] 为 2");
    assert!(
        outcome.message().contains("对象不能修改索引"),
        "{}",
        outcome.message()
    );
}

#[test]
fn value_matrix_struct_rejects_unknown_field_write() {
    let outcome = assert_three_paths_failure(
        r#"结构 点：
    横
结束
定义 位 为 点(横 为 1)
位 的 未知 为 2"#,
    );
    assert!(outcome.message().contains("字段") || outcome.message().contains("未知"));
}

#[test]
fn value_matrix_equality_across_types() {
    let output = assert_three_paths_output(
        r#"输出(1 = 1, "甲" = "甲", [1, 2] = [1, 2])
输出(1 = "1", [1] = [1, 2])"#,
    );
    assert_eq!(output, ["是 是 是", "否 否"]);
}

#[test]
fn value_matrix_float_relations_agree_across_sorting_dedup_and_sets() {
    let output = assert_three_paths_output(
        r#"引入 统计分析
定义 甲 为 统计分析 数组([1, 2]) 平均值()
定义 乙 为 统计分析 数组([2, 4]) 平均值()
定义 丙 为 统计分析 数组([1, 2]) 平均值()
输出(长度(去重([甲, 乙, 丙])), 甲 = 丙, 甲 < 乙, 甲 属于 集合([乙, 丙]))
输出(排序([乙, 甲]))"#,
    );
    assert_eq!(output, ["2 是 是 是", "[1.5, 3]"]);

    for expression in ["甲 = 1.5", "甲 < 2", "甲 + 1"] {
        let outcome = assert_three_paths_failure(&format!(
            "引入 统计分析\n定义 甲 为 统计分析 数组([1, 2]) 平均值()\n输出({expression})"
        ));
        assert_eq!(
            outcome.message(),
            "数字和浮点数不能参与同一次运算或比较，请先用“转数字”把浮点数转换为数字。",
            "{expression}"
        );
    }
}

#[test]
fn value_matrix_money_orders_by_currency_and_amount() {
    let output = assert_three_paths_output(
        r#"引入 财务
定义 小 为 财务 金额(9)
定义 大 为 财务 金额(10)
定义 美 为 财务 金额(9, "USD")
输出(小 < 大, 大 > 小, 小 = 美, 小 = 财务 金额(9))
输出(排序([大, 小]))"#,
    );
    assert_eq!(output, ["是 是 否 是", "[CNY 9, CNY 10]"]);

    let outcome = assert_three_paths_failure(
        r#"引入 财务
输出(财务 金额(9) < 财务 金额(9, "USD"))"#,
    );
    assert_eq!(outcome.message(), "不同币种金额不能直接比较");
}

#[test]
fn value_matrix_record_dynamic_key() {
    let output = assert_three_paths_output(
        r#"定义 录 为 {甲: 1}
录 的 乙 为 2
输出(录 的 甲, 录 的 乙)"#,
    );
    assert_eq!(output, ["1 2"]);
}

#[test]
fn value_matrix_set_preserves_type_across_task() {
    let output = assert_three_paths_output(
        r#"引入 并发
定义功能 原样（值）：
    返回 值
结束
定义 结果 为 并发 并行映射([集合([1, 2, 3])], 原样)
输出(类型是(结果[1], "集合"), 长度(结果[1]))"#,
    );
    assert_eq!(output, ["是 3"]);
}

#[test]
fn value_matrix_display_list_and_record() {
    let output = assert_three_paths_output(
        r#"输出([1, "甲", 是])
输出({名: "乙"})"#,
    );
    assert_eq!(output, ["[1, 甲, 是]", "{\"名\": 乙}"]);
}

#[test]
fn value_matrix_text_length() {
    let output = assert_three_paths_output(
        r#"定义 文 为 "甲乙丙"
输出(长度(文))"#,
    );
    assert_eq!(output, ["3"]);
}

/// 属性赋值制造自引用容器在三条路径上都必须被拒绝：
/// VM 桥接层曾直接改容器而绕过解释器的循环引用检查。
#[test]
fn value_matrix_attribute_assignment_rejects_self_containment() {
    let outcome = assert_three_paths_failure(
        r#"结构 盒：
    内
结束
定义 甲 为 盒(内 为 空)
甲 的 内 为 甲"#,
    );
    assert!(
        outcome.message().contains("容器不能直接或间接包含自身"),
        "{}",
        outcome.message()
    );

    let list = assert_three_paths_failure("定义 列 为 [1]\n列[1] 为 列");
    assert!(
        list.message().contains("容器不能直接或间接包含自身"),
        "{}",
        list.message()
    );
}

/// 循环里反复包装同一个值必须在嵌套预算处返回资源错误，而不是无限增长或让宿主栈溢出。
#[test]
fn value_matrix_repeated_wrapping_hits_the_nesting_budget() {
    let outcome = assert_three_paths_failure(
        r#"定义 值 为 1
定义 次 为 1
当 次 <= 3000 时重复：
    值 为 [值]
    次 为 次 + 1
结束
输出("不该到这里")"#,
    );
    assert_eq!(outcome.message(), "值的嵌套层级超过限制");
    assert_eq!(outcome.error_category(), Some("资源错误"));
}
