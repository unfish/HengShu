//! 发布前人工复审：按语义矩阵各维度补充探测，寻找三路径差异。
mod common;

use common::{assert_three_paths, assert_three_paths_output, ExecutionStatus};

#[test]
fn review_probes_output_semantics_across_paths() {
    let cases: &[(&str, &[&str])] = &[
        // 一基索引读写
        ("定义 值 为 [1, 2, 3]\n值[1] 为 9\n输出(值[1], 值[3])", &["9 3"]),
        // 短路求值不得求值右侧
        ("定义功能 爆（）：\n抛出 \"不该求值\"\n结束\n输出(否 且 爆(), 是 或 爆())", &["否 是"]),
        // 要求语句延迟求值说明
        ("要求 是，否则说明 1 / 0\n输出(\"通过\")", &["通过"]),
        // finally 在提前返回时只执行一次
        (
            "定义功能 甲（）：\n尝试：\n返回 1\n最终：\n输出(\"清理\")\n结束\n结束\n输出(甲())",
            &["清理", "1"],
        ),
        // 块级遮蔽离开后恢复
        ("定义 值 为 1\n遍历 [9]，记作 值：\n输出(值)\n结束\n输出(值)", &["9", "1"]),
        // 幂运算右结合
        ("输出(2 ^ 3 ^ 2)", &["512"]),
        // 参数化输出格式化
        ("定义 金额 为 1234.5\n参数化输出(\"{金额:千分位}\")", &["1,234.5"]),
        // 匹配语句与守卫
        (
            "定义 值 为 5\n匹配 值：\n当 类型是 文本：\n输出(\"文本\")\n当 类型是 数字，并且满足 当前值 > 3：\n输出(\"大\")\n其他：\n输出(\"小\")\n结束",
            &["大"],
        ),
        // 守卫不成立时落到兜底分支
        (
            "定义 值 为 1\n匹配 值：\n当 类型是 数字，并且满足 当前值 > 3：\n输出(\"大\")\n其他：\n输出(\"小\")\n结束",
            &["小"],
        ),
        // 跳出循环仍执行本轮 finally
        (
            "遍历 [1, 2, 3]，记作 项：\n尝试：\n如果 项 == 2：\n跳出循环\n结束\n输出(项)\n最终：\n输出(\"清理\")\n结束\n结束\n输出(\"完\")",
            &["1", "清理", "清理", "完"],
        ),
        // 继续下一轮每轮执行一次 finally
        (
            "遍历 [1, 2]，记作 项：\n尝试：\n继续下一轮\n最终：\n输出(\"清理\", 项)\n结束\n结束\n输出(\"完\")",
            &["清理 1", "清理 2", "完"],
        ),
        // finally 中的返回覆盖尝试块的返回
        (
            "定义功能 甲（）：\n尝试：\n返回 \"尝试\"\n最终：\n返回 \"最终\"\n结束\n结束\n输出(甲())",
            &["最终"],
        ),
        // 精确十进制算术与取模符号
        ("输出(0.1 + 0.2)\n输出(10 % 3, -10 % 3)\n输出(2 ^ -1)", &["0.3", "1 -1", "0.5"]),
        // 跨类型相等保持严格
        ("输出(1 == 1.0, \"1\" == 1)", &["是 否"]),
    ];
    for (source, expected) in cases {
        assert_eq!(assert_three_paths_output(source), *expected, "{source}");
    }
}

#[test]
fn review_probes_failure_semantics_across_paths() {
    let cases: &[(&str, &str, usize, &str)] = &[
        ("输出(1 / 0)", "运行错误", 1, "不能除以零"),
        ("定义 值 为 [1]\n输出(值[2])", "运行错误", 2, "下标越界"),
        (
            "定义功能 甲（必需）：\n返回 必需\n结束\n甲()",
            "运行错误",
            4,
            "必需",
        ),
        (
            "定义功能 甲（值）：\n返回 值\n结束\n甲(1, 值 为 2)",
            "运行错误",
            4,
            "值",
        ),
        ("抛出 \"自定义\"", "运行错误", 1, "自定义"),
        (
            "定义 表 为 {\"键\": 1}\n输出(表 的 缺失)",
            "运行错误",
            2,
            "缺失",
        ),
        (
            "定义功能 无限（）：\n返回 无限()\n结束\n无限()",
            "资源错误",
            2,
            "功能调用深度超过资源限制",
        ),
    ];
    for (source, category, line, fragment) in cases {
        let outcome = assert_three_paths(source);
        assert_eq!(outcome.status, ExecutionStatus::Failure, "{source}");
        assert_eq!(outcome.error_category(), Some(*category), "{source}");
        assert_eq!(outcome.line(), Some(*line), "{source}");
        assert!(
            outcome.message().contains(fragment),
            "{source}：{}",
            outcome.message()
        );
        assert!(
            !outcome.message().contains("行发生"),
            "{source}：消息不得内嵌定位前缀：{}",
            outcome.message()
        );
    }
}
