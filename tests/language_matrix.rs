#![allow(non_snake_case)]

use std::collections::BTreeSet;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn run_case(source: &str) -> std::process::Output {
    let directory = tempdir().expect("创建测试目录");
    let path = directory.path().join("回归.代码");
    fs::write(&path, source).expect("写入测试源码");
    Command::new(env!("CARGO_BIN_EXE_衡枢"))
        .arg("运行")
        .arg(path)
        .output()
        .expect("启动衡枢")
}

type CaseBuilder = fn(usize) -> (String, String, bool);

macro_rules! language_cases {
    ($( $name:ident, $builder:ident, $number:expr ),+ $(,)?) => {
        $(
        #[test]
        fn $name() {
            let (source, expected, success) = $builder($number);
            let output = run_case(&source);
            assert_eq!(
                output.status.success(),
                success,
                "标准错误：{}\n源码：{}",
                String::from_utf8_lossy(&output.stderr),
                source
            );
            if success {
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    expected,
                    "源码：{}",
                    source
                );
            }
        }
        )+
    };
}

fn case_arithmetic(n: usize) -> (String, String, bool) {
    let cases = [
        ("输出(1 + 2)", "3"),
        ("输出(9 - 4)", "5"),
        ("输出(3 * 4)", "12"),
        ("输出(8 / 2)", "4"),
        ("输出(7 % 3)", "1"),
        ("输出(2 + 3 * 4)", "14"),
        ("输出((2 + 3) * 4)", "20"),
        ("输出(-5 + 8)", "3"),
        ("输出(1.25 + 2.75)", "4"),
        ("输出(3 = 3)", "是"),
        ("输出(3 != 4)", "是"),
        ("输出(5 > 2)", "是"),
        ("输出(2 < 5)", "是"),
        ("输出(5 >= 5)", "是"),
        ("输出(4 <= 5)", "是"),
        ("输出(是 且 否)", "否"),
        ("输出(是 或 否)", "是"),
        ("输出(非 否)", "是"),
        ("输出(2 属于 [1, 2, 3])", "是"),
        ("输出(4 不属于 [1, 2, 3])", "是"),
        ("输出(\"甲\" + \"乙\")", "甲乙"),
        ("输出(绝对值(-8))", "8"),
        ("输出(四舍五入(3.14159, 2))", "3.14"),
        ("输出(最大值([2, 9, 4]))", "9"),
        ("输出(最小值([2, 9, 4]))", "2"),
    ];
    let (source, expected) = cases[n - 1];
    (format!("{source}\n"), expected.into(), true)
}

fn case_definition(n: usize) -> (String, String, bool) {
    let n = n as i64;
    (
        format!("定义 原值 为 {n}\n定义 结果 为 原值 * 2\n结果 为 结果 + 1\n输出(结果)\n"),
        (n * 2 + 1).to_string(),
        true,
    )
}

fn case_condition(n: usize) -> (String, String, bool) {
    let n = n as i64;
    let expected = if n % 2 == 0 { "偶数" } else { "奇数" };
    (
        format!(
            "定义 数值 为 {n}\n如果 数值 % 2 = 0：\n输出(\"偶数\")\n否则：\n输出(\"奇数\")\n结束\n"
        ),
        expected.into(),
        true,
    )
}

fn case_list(n: usize) -> (String, String, bool) {
    let n = n as i64;
    let index = n % 3 + 1;
    let expected = n + index - 1;
    (
        format!(
            "定义 数据 为 [{n}, {}, {}]\n输出(数据[{index}])\n",
            n + 1,
            n + 2
        ),
        expected.to_string(),
        true,
    )
}

fn case_record(n: usize) -> (String, String, bool) {
    let n = n as i64;
    (
        format!(
            "定义 记录 为 {{\"编号\": {n}, \"金额\": {}}}\n输出(记录 的 金额)\n",
            n * 10
        ),
        (n * 10).to_string(),
        true,
    )
}

fn case_while(n: usize) -> (String, String, bool) {
    let target = n as i64;
    (
        format!(
            "定义 次数 为 0\n当 次数 < {target} 时重复：\n次数 为 次数 + 1\n结束\n输出(次数)\n"
        ),
        target.to_string(),
        true,
    )
}

fn case_for(n: usize) -> (String, String, bool) {
    let max = n as i64;
    let values = (1..=max)
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let expected = (1..=max)
        .map(|value| (value * 2).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    (
        format!("遍历 [{values}]，记作 数值：\n输出(数值 * 2)\n结束\n"),
        expected,
        true,
    )
}

fn case_function(n: usize) -> (String, String, bool) {
    let n = n as i64;
    match n % 3 {
        0 => (format!("定义功能 计算（数值）：\n返回 数值 + {n}\n结束\n输出(计算(2))\n"), (n + 2).to_string(), true),
        1 => (format!("定义功能 计算（数值, 增量 为 {n}）：\n返回 数值 * 增量\n结束\n输出(计算(2))\n"), (n * 2).to_string(), true),
        _ => (format!("定义功能 计算（数值）：\n如果 数值 > {n}：\n返回 \"大\"\n否则：\n返回 \"小\"\n结束\n结束\n输出(计算({}))\n", n + 1), "大".into(), true),
    }
}

fn case_closure(n: usize) -> (String, String, bool) {
    let n = n as i64;
    (
        format!("定义 起点 为 {n}\n定义 增加 为 定义功能（数值）为 数值 + 起点\n输出(增加(5))\n"),
        (n + 5).to_string(),
        true,
    )
}

fn case_match(n: usize) -> (String, String, bool) {
    let n = n as i64;
    let expected = match n {
        1 => "一",
        2 => "二",
        _ => "其他",
    };
    (format!("定义 值 为 {n}\n匹配 值：\n当 1：\n输出(\"一\")\n当 2：\n输出(\"二\")\n其他：\n输出(\"其他\")\n结束\n"), expected.into(), true)
}

fn case_text_encoding(n: usize) -> (String, String, bool) {
    let cases = [
        ("引入 文本处理\n输出(文本处理 大写(\"hengshu\"))", "HENGSHU"),
        ("引入 文本处理\n输出(文本处理 小写(\"HENGSHU\"))", "hengshu"),
        ("引入 文本处理\n输出(文本处理 替换(\"甲甲\", \"甲\", \"乙\"))", "乙乙"),
        ("引入 文本处理\n输出(文本处理 分割(\"甲,乙\", \",\"))", "[甲, 乙]"),
        ("引入 编码\n输出(编码 转文本(编码 转字节(\"衡枢\")))", "衡枢"),
        ("引入 编码\n输出(编码 Base64编码(\"hi\"))", "aGk="),
        ("引入 编码\n输出(编码 转文本(编码 Base64解码(\"aGk=\")))", "hi"),
        ("引入 编码\n输出(编码 十六进制编码(\"hi\"))", "6869"),
        ("引入 编码\n输出(编码 转文本(编码 十六进制解码(\"6869\")))", "hi"),
        ("引入 正则\n输出(正则 是否匹配(\"^衡\", \"衡枢\"))", "是"),
        ("引入 正则\n输出(正则 查找(\"枢\", \"衡枢\"))", "枢"),
        ("引入 正则\n输出(长度(正则 查找全部(\"甲\", \"甲乙甲\")))", "2"),
        ("引入 正则\n输出(正则 替换(\"甲+\", \"甲甲乙\", \"衡\"))", "衡乙"),
        ("引入 正则\n输出(长度(正则 分割(\",\", \"甲,乙,丙\")))", "3"),
        ("引入 压缩\n引入 编码\n输出(编码 转文本(压缩 Gzip解压(压缩 Gzip压缩(\"压缩\"))))", "压缩"),
        ("引入 TOML\n定义 配置 为 TOML 解析(\"name = \\\"衡枢\\\"\")\n输出(配置 的 name)", "衡枢"),
        ("引入 TOML\n输出(长度(TOML 生成({\"name\": \"衡枢\"})))", "12"),
        ("引入 日期时间\n定义 时间 为 日期时间 解析(\"2024-02-29T12:34:56+08:00\")\n输出(日期时间 格式化(时间, \"%Y-%m-%d\"))", "2024-02-29"),
        ("引入 JSON\n输出(JSON 解析(\"{\\\"名称\\\":\\\"衡枢\\\"}\") 的 名称)", "衡枢"),
        ("引入 JSON\n输出(JSON 生成([4, 5]))", "[4,5]"),
    ];
    let (source, expected) = cases[n - 1];
    (format!("{source}\n"), expected.into(), true)
}

fn case_statistics(n: usize) -> (String, String, bool) {
    let cases = [
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 平均值())", "2"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 总和())", "6"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 中位数())", "2"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 方差())", "0.6666666666666666"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 标准差())", "0.816496580927726"),
        ("定义 数据 为 统计分析 数组([3, 1, 2])\n输出(数据 最小值())", "1"),
        ("定义 数据 为 统计分析 数组([3, 1, 2])\n输出(数据 最大值())", "3"),
        ("定义 数据 为 统计分析 数组([1, 2, 3, 4])\n输出(数据 分位数([0.5]))", "[2.5]"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 累计和())", "[1, 3, 6]"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 累计积())", "[1, 2, 6]"),
        ("定义 数据 为 统计分析 数组([1, 空, 2])\n输出(数据 去除缺失())", "[1, 2]"),
        ("定义 数据 为 统计分析 数组([1, 空, 2])\n输出(数据 填补缺失(0))", "[1, 0, 2]"),
        ("定义 甲 为 统计分析 数组([1, 2, 3])\n定义 乙 为 统计分析 数组([2, 4, 6])\n输出(甲 相关系数(乙))", "1"),
        ("定义 甲 为 统计分析 数组([1, 2, 3])\n定义 乙 为 统计分析 数组([2, 4, 6])\n输出(甲 协方差(乙))", "2"),
        ("定义 甲 为 统计分析 数组([[1, 2], [3, 4]])\n定义 乙 为 统计分析 数组([[5, 6], [7, 8]])\n输出(甲 矩阵乘(乙))", "[19, 22, 43, 50]"),
        ("定义 甲 为 统计分析 数组([[2, 1], [1, 2]])\n定义 乙 为 统计分析 数组([3, 3])\n输出(甲 求解方程(乙))", "[1, 1]"),
        ("定义 数据 为 统计分析 数组([[2, 0], [0, 3]])\n输出(长度(数据 特征值()))", "2"),
        ("定义 数据 为 统计分析 数组([[2, 0], [0, 3]])\n输出(长度(数据 奇异值()))", "2"),
        ("定义 数据 为 统计分析 数组([1, 2, 3, 4])\n输出(长度((数据 直方统计(2)) 的 计数))", "2"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(数据 描述() 的 平均值)", "2"),
        ("定义 数据 为 统计分析 数组([1, 2, 3])\n输出(长度(数据 正态化()))", "3"),
        ("输出(统计分析 正态分布概率(0))", "0.5"),
        ("输出(统计分析 正态分布分位点(0.5))", "0"),
        ("定义 结果 为 统计分析 线性回归([[1], [2], [3]], [2, 4, 6])\n输出(结果 的 R平方)", "1"),
        ("输出(长度(统计分析 线性预测([1, 2, 3], 步数 为 2)))", "2"),
    ];
    let (source, expected) = cases[n - 1];
    (format!("引入 统计分析\n{source}\n"), expected.into(), true)
}

fn case_finance(n: usize) -> (String, String, bool) {
    let cases = [
        ("引入 财务\n输出(财务 税额(100, 0.13))", "13", true),
        ("引入 财务\n定义 金额值 为 财务 金额(12.345, 小数位 为 2)\n输出(金额值 的 数额)", "12.34", true),
        ("引入 财务\n定义 金额值 为 财务 金额(10, 币种 为 \"CNY\")\n输出(金额值 的 币种)", "CNY", true),
        ("引入 财务\n定义 金额值 为 财务 金额(10.5, 币种 为 \"CNY\")\n输出(金额值 格式化())", "CNY 10.5", true),
        ("引入 财务\n定义 金额值 为 财务 金额(100)\n定义 税 为 财务 税额(金额值, 0.13)\n输出(税 的 数额)", "13", true),
        ("引入 财务\n定义 金额值 为 财务 金额(100, 币种 为 \"CNY\")\n定义 结果 为 财务 换算(金额值, 0.14, \"USD\")\n输出(结果 的 币种)", "USD", true),
        ("引入 财务\n定义 金额值 为 财务 金额(100, 币种 为 \"CNY\")\n定义 结果 为 财务 换算(金额值, 0.14, \"USD\")\n输出(结果 的 数额)", "14", true),
        ("引入 财务\n定义 甲 为 财务 金额(1)\n定义 乙 为 财务 金额(2)\n输出((甲 + 乙) 的 数额)", "3", true),
        ("引入 财务\n定义 甲 为 财务 金额(5)\n定义 乙 为 财务 金额(2)\n输出((甲 - 乙) 的 数额)", "3", true),
        ("引入 财务\n定义 甲 为 财务 金额(5)\n输出((甲 * 2) 的 数额)", "10", true),
        ("引入 财务\n定义 甲 为 财务 金额(1, 币种 为 \"CNY\")\n定义 乙 为 财务 金额(1, 币种 为 \"USD\")\n输出(甲 + 乙)", "", false),
        ("引入 财务\n定义 期间 为 财务 会计期间(\"2024-02-01\", 年度开始月 为 1)\n输出(期间 的 财年)", "2024", true),
        ("引入 财务\n定义 期间 为 财务 会计期间(\"2024-04-01\", 年度开始月 为 4)\n输出(期间 的 期间)", "1", true),
        ("引入 财务\n输出(财务 是否工作日(\"2024-08-16\"))", "是", true),
        ("引入 财务\n输出(财务 月末(\"2024-02-10\"))", "2024-02-29", true),
    ];
    let (source, expected, success) = cases[n - 1];
    (format!("{source}\n"), expected.into(), success)
}

fn case_multiline(n: usize) -> (String, String, bool) {
    let n = n as i64;
    if n % 2 == 0 {
        (
            format!(
                "定义 数据 为 [\n    {n},\n    {},\n    {}\n]\n输出(长度(\n    数据\n))\n",
                n + 1,
                n + 2
            ),
            "3".into(),
            true,
        )
    } else {
        (
            format!(
                "定义 记录 为 {{\n    \"编号\": {n},\n    \"合计\": {}\n}}\n输出(记录 的 合计)\n",
                n * 2
            ),
            (n * 2).to_string(),
            true,
        )
    }
}

fn case_diagnostic(n: usize) -> (String, String, bool) {
    let n = n as i64;
    match n % 4 {
        0 => (
            format!("定义 重复{n} 为 1\n定义 重复{n} 为 2\n"),
            String::new(),
            false,
        ),
        1 => (format!("输出(未定义{n})\n"), String::new(), false),
        2 => (
            format!("定义 值{n} 为 \"文本\"，类型限定为 数字\n"),
            String::new(),
            false,
        ),
        _ => (
            format!("要求 否，否则说明 \"第 {n} 个契约失败\"\n"),
            String::new(),
            false,
        ),
    }
}

fn case_permission(n: usize) -> (String, String, bool) {
    (
        format!("引入 环境\n输出(环境 读取(\"衡枢测试拒绝_{n}\"))\n"),
        "空".into(),
        true,
    )
}

fn case_json(n: usize) -> (String, String, bool) {
    let cases = [
        ("定义 数据 为 JSON 解析(\"{\\\"编号\\\":1}\")\n输出(数据 的 编号)", "1"),
        ("定义 数据 为 JSON 解析(\"{\\\"对象\\\":{\\\"值\\\":2}}\")\n输出((数据 的 对象) 的 值)", "2"),
        ("定义 数据 为 JSON 解析(\"{\\\"项目\\\":[1,2,3]}\")\n输出(长度(数据 的 项目))", "3"),
        ("定义 数据 为 JSON 解析(\"{\\\"值\\\":null}\")\n输出(数据 的 值)", "空"),
        ("定义 数据 为 JSON 解析(\"{\\\"文字\\\":\\\"衡枢\\\"}\")\n输出(数据 的 文字)", "衡枢"),
        ("输出(JSON 生成(42))", "42"),
        ("输出(JSON 生成([1, 2, 3]))", "[1,2,3]"),
        ("输出(JSON 生成({\"编号\": 1, \"有效\": 是}))", "{\"有效\":true,\"编号\":1}"),
        ("输出(JSON 生成(空))", "null"),
        ("输出(JSON 生成(\"衡枢\"))", "\"衡枢\""),
        ("JSON 写入行(\"数据.jsonl\", [{\"编号\": 1}, {\"编号\": 2}])\n输出(长度(JSON 读取行(\"数据.jsonl\")))", "2"),
        ("JSON 写入行(\"数据.jsonl\", [{\"编号\": 7}])\n定义 行 为 JSON 读取行(\"数据.jsonl\")\n输出((行[1]) 的 编号)", "7"),
        ("JSON 写入行(\"数据.jsonl\", [1, 2, 3])\n输出(长度(JSON 读取行(\"数据.jsonl\")))", "3"),
        ("输出(JSON 生成({\"外层\": {\"内层\": 是}}))", "{\"外层\":{\"内层\":true}}"),
        ("定义 数据 为 JSON 解析(\"{\\\"数值\\\":1.5}\")\n输出(数据 的 数值)", "1.5"),
        ("输出(JSON 生成(否))", "false"),
        ("定义 数据 为 JSON 解析(\"{\\\"项目\\\":[4,5]}\")\n输出((数据 的 项目)[2])", "5"),
        ("输出(JSON 生成([]))", "[]"),
        ("JSON 写入行(\"数据.jsonl\", [\"甲\"])\n定义 行 为 JSON 读取行(\"数据.jsonl\")\n输出(行[1])", "甲"),
        ("定义 数据 为 JSON 解析(\"  { \\\"编号\\\" : 9 }  \")\n输出(数据 的 编号)", "9"),
    ];
    let (source, expected) = cases[n - 1];
    (format!("引入 JSON\n{source}\n"), expected.into(), true)
}

language_cases! {
    回归_算术_001, case_arithmetic, 1,
    回归_算术_002, case_arithmetic, 2,
    回归_算术_003, case_arithmetic, 3,
    回归_算术_004, case_arithmetic, 4,
    回归_算术_005, case_arithmetic, 5,
    回归_算术_006, case_arithmetic, 6,
    回归_算术_007, case_arithmetic, 7,
    回归_算术_008, case_arithmetic, 8,
    回归_算术_009, case_arithmetic, 9,
    回归_算术_010, case_arithmetic, 10,
    回归_算术_011, case_arithmetic, 11,
    回归_算术_012, case_arithmetic, 12,
    回归_算术_013, case_arithmetic, 13,
    回归_算术_014, case_arithmetic, 14,
    回归_算术_015, case_arithmetic, 15,
    回归_算术_016, case_arithmetic, 16,
    回归_算术_017, case_arithmetic, 17,
    回归_算术_018, case_arithmetic, 18,
    回归_算术_019, case_arithmetic, 19,
    回归_算术_020, case_arithmetic, 20,
    回归_算术_021, case_arithmetic, 21,
    回归_算术_022, case_arithmetic, 22,
    回归_算术_023, case_arithmetic, 23,
    回归_算术_024, case_arithmetic, 24,
    回归_算术_025, case_arithmetic, 25,
    回归_定义_001, case_definition, 1,
    回归_定义_002, case_definition, 2,
    回归_定义_003, case_definition, 3,
    回归_定义_004, case_definition, 4,
    回归_定义_005, case_definition, 5,
    回归_定义_006, case_definition, 6,
    回归_定义_007, case_definition, 7,
    回归_定义_008, case_definition, 8,
    回归_定义_009, case_definition, 9,
    回归_定义_010, case_definition, 10,
    回归_定义_011, case_definition, 11,
    回归_定义_012, case_definition, 12,
    回归_定义_013, case_definition, 13,
    回归_定义_014, case_definition, 14,
    回归_定义_015, case_definition, 15,
    回归_定义_016, case_definition, 16,
    回归_定义_017, case_definition, 17,
    回归_定义_018, case_definition, 18,
    回归_定义_019, case_definition, 19,
    回归_定义_020, case_definition, 20,
    回归_定义_021, case_definition, 21,
    回归_定义_022, case_definition, 22,
    回归_定义_023, case_definition, 23,
    回归_定义_024, case_definition, 24,
    回归_定义_025, case_definition, 25,
    回归_条件_001, case_condition, 1,
    回归_条件_002, case_condition, 2,
    回归_条件_003, case_condition, 3,
    回归_条件_004, case_condition, 4,
    回归_条件_005, case_condition, 5,
    回归_条件_006, case_condition, 6,
    回归_条件_007, case_condition, 7,
    回归_条件_008, case_condition, 8,
    回归_条件_009, case_condition, 9,
    回归_条件_010, case_condition, 10,
    回归_条件_011, case_condition, 11,
    回归_条件_012, case_condition, 12,
    回归_条件_013, case_condition, 13,
    回归_条件_014, case_condition, 14,
    回归_条件_015, case_condition, 15,
    回归_条件_016, case_condition, 16,
    回归_条件_017, case_condition, 17,
    回归_条件_018, case_condition, 18,
    回归_条件_019, case_condition, 19,
    回归_条件_020, case_condition, 20,
    回归_列表_001, case_list, 1,
    回归_列表_002, case_list, 2,
    回归_列表_003, case_list, 3,
    回归_列表_004, case_list, 4,
    回归_列表_005, case_list, 5,
    回归_列表_006, case_list, 6,
    回归_列表_007, case_list, 7,
    回归_列表_008, case_list, 8,
    回归_列表_009, case_list, 9,
    回归_列表_010, case_list, 10,
    回归_列表_011, case_list, 11,
    回归_列表_012, case_list, 12,
    回归_列表_013, case_list, 13,
    回归_列表_014, case_list, 14,
    回归_列表_015, case_list, 15,
    回归_列表_016, case_list, 16,
    回归_列表_017, case_list, 17,
    回归_列表_018, case_list, 18,
    回归_列表_019, case_list, 19,
    回归_列表_020, case_list, 20,
    回归_列表_021, case_list, 21,
    回归_列表_022, case_list, 22,
    回归_列表_023, case_list, 23,
    回归_列表_024, case_list, 24,
    回归_列表_025, case_list, 25,
    回归_记录_001, case_record, 1,
    回归_记录_002, case_record, 2,
    回归_记录_003, case_record, 3,
    回归_记录_004, case_record, 4,
    回归_记录_005, case_record, 5,
    回归_记录_006, case_record, 6,
    回归_记录_007, case_record, 7,
    回归_记录_008, case_record, 8,
    回归_记录_009, case_record, 9,
    回归_记录_010, case_record, 10,
    回归_记录_011, case_record, 11,
    回归_记录_012, case_record, 12,
    回归_记录_013, case_record, 13,
    回归_记录_014, case_record, 14,
    回归_记录_015, case_record, 15,
    回归_记录_016, case_record, 16,
    回归_记录_017, case_record, 17,
    回归_记录_018, case_record, 18,
    回归_记录_019, case_record, 19,
    回归_记录_020, case_record, 20,
    回归_循环_001, case_while, 1,
    回归_循环_002, case_while, 2,
    回归_循环_003, case_while, 3,
    回归_循环_004, case_while, 4,
    回归_循环_005, case_while, 5,
    回归_循环_006, case_while, 6,
    回归_循环_007, case_while, 7,
    回归_循环_008, case_while, 8,
    回归_循环_009, case_while, 9,
    回归_循环_010, case_while, 10,
    回归_循环_011, case_while, 11,
    回归_循环_012, case_while, 12,
    回归_循环_013, case_while, 13,
    回归_循环_014, case_while, 14,
    回归_循环_015, case_while, 15,
    回归_循环_016, case_while, 16,
    回归_循环_017, case_while, 17,
    回归_循环_018, case_while, 18,
    回归_循环_019, case_while, 19,
    回归_循环_020, case_while, 20,
    回归_遍历_001, case_for, 1,
    回归_遍历_002, case_for, 2,
    回归_遍历_003, case_for, 3,
    回归_遍历_004, case_for, 4,
    回归_遍历_005, case_for, 5,
    回归_遍历_006, case_for, 6,
    回归_遍历_007, case_for, 7,
    回归_遍历_008, case_for, 8,
    回归_遍历_009, case_for, 9,
    回归_遍历_010, case_for, 10,
    回归_遍历_011, case_for, 11,
    回归_遍历_012, case_for, 12,
    回归_遍历_013, case_for, 13,
    回归_遍历_014, case_for, 14,
    回归_遍历_015, case_for, 15,
    回归_遍历_016, case_for, 16,
    回归_遍历_017, case_for, 17,
    回归_遍历_018, case_for, 18,
    回归_遍历_019, case_for, 19,
    回归_遍历_020, case_for, 20,
    回归_功能_001, case_function, 1,
    回归_功能_002, case_function, 2,
    回归_功能_003, case_function, 3,
    回归_功能_004, case_function, 4,
    回归_功能_005, case_function, 5,
    回归_功能_006, case_function, 6,
    回归_功能_007, case_function, 7,
    回归_功能_008, case_function, 8,
    回归_功能_009, case_function, 9,
    回归_功能_010, case_function, 10,
    回归_功能_011, case_function, 11,
    回归_功能_012, case_function, 12,
    回归_功能_013, case_function, 13,
    回归_功能_014, case_function, 14,
    回归_功能_015, case_function, 15,
    回归_功能_016, case_function, 16,
    回归_功能_017, case_function, 17,
    回归_功能_018, case_function, 18,
    回归_功能_019, case_function, 19,
    回归_功能_020, case_function, 20,
    回归_功能_021, case_function, 21,
    回归_功能_022, case_function, 22,
    回归_功能_023, case_function, 23,
    回归_功能_024, case_function, 24,
    回归_功能_025, case_function, 25,
    回归_闭包_001, case_closure, 1,
    回归_闭包_002, case_closure, 2,
    回归_闭包_003, case_closure, 3,
    回归_闭包_004, case_closure, 4,
    回归_闭包_005, case_closure, 5,
    回归_闭包_006, case_closure, 6,
    回归_闭包_007, case_closure, 7,
    回归_闭包_008, case_closure, 8,
    回归_闭包_009, case_closure, 9,
    回归_闭包_010, case_closure, 10,
    回归_闭包_011, case_closure, 11,
    回归_闭包_012, case_closure, 12,
    回归_闭包_013, case_closure, 13,
    回归_闭包_014, case_closure, 14,
    回归_闭包_015, case_closure, 15,
    回归_匹配_001, case_match, 1,
    回归_匹配_002, case_match, 2,
    回归_匹配_003, case_match, 3,
    回归_匹配_004, case_match, 4,
    回归_匹配_005, case_match, 5,
    回归_匹配_006, case_match, 6,
    回归_匹配_007, case_match, 7,
    回归_匹配_008, case_match, 8,
    回归_匹配_009, case_match, 9,
    回归_匹配_010, case_match, 10,
    回归_匹配_011, case_match, 11,
    回归_匹配_012, case_match, 12,
    回归_匹配_013, case_match, 13,
    回归_匹配_014, case_match, 14,
    回归_匹配_015, case_match, 15,
    回归_文本编码_001, case_text_encoding, 1,
    回归_文本编码_002, case_text_encoding, 2,
    回归_文本编码_003, case_text_encoding, 3,
    回归_文本编码_004, case_text_encoding, 4,
    回归_文本编码_005, case_text_encoding, 5,
    回归_文本编码_006, case_text_encoding, 6,
    回归_文本编码_007, case_text_encoding, 7,
    回归_文本编码_008, case_text_encoding, 8,
    回归_文本编码_009, case_text_encoding, 9,
    回归_文本编码_010, case_text_encoding, 10,
    回归_文本编码_011, case_text_encoding, 11,
    回归_文本编码_012, case_text_encoding, 12,
    回归_文本编码_013, case_text_encoding, 13,
    回归_文本编码_014, case_text_encoding, 14,
    回归_文本编码_015, case_text_encoding, 15,
    回归_文本编码_016, case_text_encoding, 16,
    回归_文本编码_017, case_text_encoding, 17,
    回归_文本编码_018, case_text_encoding, 18,
    回归_文本编码_019, case_text_encoding, 19,
    回归_文本编码_020, case_text_encoding, 20,
    回归_统计_001, case_statistics, 1,
    回归_统计_002, case_statistics, 2,
    回归_统计_003, case_statistics, 3,
    回归_统计_004, case_statistics, 4,
    回归_统计_005, case_statistics, 5,
    回归_统计_006, case_statistics, 6,
    回归_统计_007, case_statistics, 7,
    回归_统计_008, case_statistics, 8,
    回归_统计_009, case_statistics, 9,
    回归_统计_010, case_statistics, 10,
    回归_统计_011, case_statistics, 11,
    回归_统计_012, case_statistics, 12,
    回归_统计_013, case_statistics, 13,
    回归_统计_014, case_statistics, 14,
    回归_统计_015, case_statistics, 15,
    回归_统计_016, case_statistics, 16,
    回归_统计_017, case_statistics, 17,
    回归_统计_018, case_statistics, 18,
    回归_统计_019, case_statistics, 19,
    回归_统计_020, case_statistics, 20,
    回归_统计_021, case_statistics, 21,
    回归_统计_022, case_statistics, 22,
    回归_统计_023, case_statistics, 23,
    回归_统计_024, case_statistics, 24,
    回归_统计_025, case_statistics, 25,
    回归_财务_001, case_finance, 1,
    回归_财务_002, case_finance, 2,
    回归_财务_003, case_finance, 3,
    回归_财务_004, case_finance, 4,
    回归_财务_005, case_finance, 5,
    回归_财务_006, case_finance, 6,
    回归_财务_007, case_finance, 7,
    回归_财务_008, case_finance, 8,
    回归_财务_009, case_finance, 9,
    回归_财务_010, case_finance, 10,
    回归_财务_011, case_finance, 11,
    回归_财务_012, case_finance, 12,
    回归_财务_013, case_finance, 13,
    回归_财务_014, case_finance, 14,
    回归_财务_015, case_finance, 15,
    回归_多行_001, case_multiline, 1,
    回归_多行_002, case_multiline, 2,
    回归_多行_003, case_multiline, 3,
    回归_多行_004, case_multiline, 4,
    回归_多行_005, case_multiline, 5,
    回归_多行_006, case_multiline, 6,
    回归_多行_007, case_multiline, 7,
    回归_多行_008, case_multiline, 8,
    回归_多行_009, case_multiline, 9,
    回归_多行_010, case_multiline, 10,
    回归_多行_011, case_multiline, 11,
    回归_多行_012, case_multiline, 12,
    回归_多行_013, case_multiline, 13,
    回归_多行_014, case_multiline, 14,
    回归_多行_015, case_multiline, 15,
    回归_多行_016, case_multiline, 16,
    回归_多行_017, case_multiline, 17,
    回归_多行_018, case_multiline, 18,
    回归_多行_019, case_multiline, 19,
    回归_多行_020, case_multiline, 20,
    回归_诊断_001, case_diagnostic, 1,
    回归_诊断_002, case_diagnostic, 2,
    回归_诊断_003, case_diagnostic, 3,
    回归_诊断_004, case_diagnostic, 4,
    回归_诊断_005, case_diagnostic, 5,
    回归_诊断_006, case_diagnostic, 6,
    回归_诊断_007, case_diagnostic, 7,
    回归_诊断_008, case_diagnostic, 8,
    回归_诊断_009, case_diagnostic, 9,
    回归_诊断_010, case_diagnostic, 10,
    回归_诊断_011, case_diagnostic, 11,
    回归_诊断_012, case_diagnostic, 12,
    回归_诊断_013, case_diagnostic, 13,
    回归_诊断_014, case_diagnostic, 14,
    回归_诊断_015, case_diagnostic, 15,
    回归_诊断_016, case_diagnostic, 16,
    回归_诊断_017, case_diagnostic, 17,
    回归_诊断_018, case_diagnostic, 18,
    回归_诊断_019, case_diagnostic, 19,
    回归_诊断_020, case_diagnostic, 20,
    回归_JSON_001, case_json, 1,
    回归_JSON_002, case_json, 2,
    回归_JSON_003, case_json, 3,
    回归_JSON_004, case_json, 4,
    回归_JSON_005, case_json, 5,
    回归_JSON_006, case_json, 6,
    回归_JSON_007, case_json, 7,
    回归_JSON_008, case_json, 8,
    回归_JSON_009, case_json, 9,
    回归_JSON_010, case_json, 10,
    回归_JSON_011, case_json, 11,
    回归_JSON_012, case_json, 12,
    回归_JSON_013, case_json, 13,
    回归_JSON_014, case_json, 14,
    回归_JSON_015, case_json, 15,
    回归_JSON_016, case_json, 16,
    回归_JSON_017, case_json, 17,
    回归_JSON_018, case_json, 18,
    回归_JSON_019, case_json, 19,
    回归_JSON_020, case_json, 20,
    回归_权限_001, case_permission, 1,
    回归_权限_002, case_permission, 2,
    回归_权限_003, case_permission, 3,
    回归_权限_004, case_permission, 4,
    回归_权限_005, case_permission, 5,
    回归_权限_006, case_permission, 6,
    回归_权限_007, case_permission, 7,
    回归_权限_008, case_permission, 8,
    回归_权限_009, case_permission, 9,
    回归_权限_010, case_permission, 10,
    回归_权限_011, case_permission, 11,
    回归_权限_012, case_permission, 12,
    回归_权限_013, case_permission, 13,
    回归_权限_014, case_permission, 14,
    回归_权限_015, case_permission, 15,
    回归_权限_016, case_permission, 16,
    回归_权限_017, case_permission, 17,
    回归_权限_018, case_permission, 18,
    回归_权限_019, case_permission, 19,
    回归_权限_020, case_permission, 20,
}

#[test]
fn matrix_catalog_audits_categories_and_semantic_shapes() {
    let categories: [(&str, usize, CaseBuilder); 17] = [
        ("算术", 25, case_arithmetic),
        ("定义", 25, case_definition),
        ("条件", 20, case_condition),
        ("列表", 25, case_list),
        ("记录", 20, case_record),
        ("循环", 20, case_while),
        ("遍历", 20, case_for),
        ("功能", 25, case_function),
        ("闭包", 15, case_closure),
        ("匹配", 15, case_match),
        ("文本编码", 20, case_text_encoding),
        ("统计", 25, case_statistics),
        ("财务", 15, case_finance),
        ("多行", 20, case_multiline),
        ("诊断", 20, case_diagnostic),
        ("权限", 20, case_permission),
        ("JSON", 20, case_json),
    ];
    let mut exact_sources = BTreeSet::new();
    let mut semantic_shapes = BTreeSet::new();
    for (_, count, builder) in categories {
        assert!(count > 0);
        for number in 1..=count {
            let (source, _, _) = builder(number);
            assert!(
                exact_sources.insert(source.clone()),
                "发现重复测试源码：{source}"
            );
            semantic_shapes.insert(normalize_numeric_shape(&source));
        }
    }
    assert_eq!(exact_sources.len(), 350, "每项必须拥有独立源码");
    assert!(
        semantic_shapes.len() >= 80,
        "语义形状过少，疑似仅修改数字凑数"
    );
}

fn normalize_numeric_shape(source: &str) -> String {
    let mut output = String::new();
    let mut in_number = false;
    for character in source.chars() {
        if character.is_ascii_digit() {
            if !in_number {
                output.push('#');
                in_number = true;
            }
        } else {
            in_number = false;
            output.push(character);
        }
    }
    output
}
