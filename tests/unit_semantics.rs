//! 单位/口径语义的三路径差分：含税与不含税、元与万元、人与人次、币种、税率、
//! 汇率、精度以及不使用单位的旧程序，在解释器、字节码 VM 和独立包上结果一致。

mod common;

use common::{assert_three_paths_failure, assert_three_paths_output};

#[test]
fn currency_scales_and_rounding_precision_are_explicit() {
    let output = assert_three_paths_output(
        r#"引入 单位
定义 万 为 单位 创建(2.345, "万元", "货币", 10000, "CNY")
定义 元 为 单位 换算(万, "元", "货币", 1, "CNY", 1)
输出(单位 值(元))
输出(单位 名称(元))
定义 分 为 单位 创建(2.345, "元", "货币", 1, "CNY")
输出(单位 值(单位 舍入(分, 2)))
输出(单位 值(单位 舍入(分, 2, "银行家")))
输出(单位 值(单位 舍入(分, 0, "向下")))
"#,
    );
    assert_eq!(output, ["23450", "元", "2.35", "2.34", "2"]);
}

#[test]
fn rounding_precision_beyond_the_decimal_range_is_rejected() {
    let error = hengshu::api::check_source(
        r#"引入 单位
定义 金额 为 单位 创建(1, "元", "货币", 1, "CNY")
输出(单位 舍入(金额, 29))
"#,
    )
    .unwrap_err();
    assert!(error.message().contains("精度"), "{}", error.message());
    let outcome = assert_three_paths_failure(
        r#"引入 单位
定义 精度 为 20 + 9
定义 金额 为 单位 创建(1, "元", "货币", 1, "CNY")
输出(单位 舍入(金额, 精度))
"#,
    );
    let error = outcome.error.unwrap().message;
    assert!(error.contains("精度"), "{error}");
}

#[test]
fn tax_basis_conversion_requires_an_explicit_rate() {
    let output = assert_three_paths_output(
        r#"引入 单位
定义 不含税 为 单位 创建(100, "不含税元", "货币", 1, "CNY:未税")
定义 含税 为 单位 税转换(不含税, "含税元", "货币", 1, "CNY:含税", 0.13)
输出(单位 值(含税))
输出(单位 值(单位 税转换(含税, "含税元", "货币", 1, "CNY:含税", 0.13)))
"#,
    );
    assert_eq!(output, ["113", "113"]);
    let error = hengshu::api::check_source(
        r#"引入 单位
定义 不含税 为 单位 创建(100, "不含税元", "货币", 1, "CNY:未税")
输出(单位 值(单位 换算(不含税, "含税元", "货币", 1, "CNY:含税")))
"#,
    )
    .unwrap_err();
    assert!(error.message().contains("单位/口径"), "{}", error.message());
    let outcome = assert_three_paths_failure(
        r#"引入 单位
定义 不含税 为 单位 创建(100, "不含税元", "货币", 1, "CNY:未税")
定义 目标口径 为 "CNY:含税"
输出(单位 值(单位 换算(不含税, "含税元", "货币", 1, 目标口径)))
"#,
    );
    assert!(outcome.error.unwrap().message.contains("税率或汇率"));
}

#[test]
fn cross_currency_conversion_uses_the_supplied_exchange_rate() {
    let output = assert_three_paths_output(
        r#"引入 单位
定义 人民币 为 单位 创建(700, "元", "货币", 1, "CNY")
定义 美元 为 单位 换算(人民币, "美元", "货币", 1, "USD", 0.14)
输出(单位 值(美元))
输出(单位 名称(美元))
"#,
    );
    assert_eq!(output, ["98", "美元"]);
    let error = hengshu::api::check_source(
        r#"引入 单位
定义 人民币 为 单位 创建(700, "元", "货币", 1, "CNY")
输出(单位 值(单位 换算(人民币, "美元", "货币", 1, "USD", 0)))
"#,
    )
    .unwrap_err();
    assert!(error.message().contains("有限正数"), "{}", error.message());
    let outcome = assert_three_paths_failure(
        r#"引入 单位
定义 人民币 为 单位 创建(700, "元", "货币", 1, "CNY")
定义 汇率 为 0
输出(单位 值(单位 换算(人民币, "美元", "货币", 1, "USD", 汇率)))
"#,
    );
    assert!(outcome.error.unwrap().message.contains("有限正数"));
}

#[test]
fn head_counts_and_visit_counts_are_different_dimensions() {
    let output = assert_three_paths_output(
        r#"引入 单位
定义 人 为 单位 创建(120, "人", "人数", 1)
输出(单位 维度(人))
输出(单位 值(单位 换算(人, "百人", "人数", 100, 空, 1)))
"#,
    );
    assert_eq!(output, ["人数", "1.2"]);
    let error = hengshu::api::check_source(
        r#"引入 单位
定义 人 为 单位 创建(120, "人", "人数", 1)
输出(单位 值(单位 换算(人, "人次", "人次", 1, 空, 1)))
"#,
    )
    .unwrap_err();
    assert!(error.message().contains("单位/口径"), "{}", error.message());
}

#[test]
fn programs_without_units_keep_plain_number_semantics() {
    let output = assert_three_paths_output(
        r#"定义 单价 为 2.5
定义 数量 为 4
输出(单价 * 数量)
输出(1 / 8)
"#,
    );
    assert_eq!(output, ["10", "0.125"]);
}
