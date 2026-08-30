//! AI offline/replay behavior must remain deterministic across the interpreter,
//! bytecode VM and packaged executable. No network or API key is needed.

mod common;

use common::{
    assert_three_paths_failure, assert_three_paths_output, assert_three_paths_with_artifacts,
};

#[test]
fn offline_ai_request_matches_across_all_three_paths() {
    let output = assert_three_paths_output(
        "引入 人工智能\n定义 结果 为 人工智能 请求(\"季度销售\", 模式 为 \"离线\")\n输出(结果 的 状态, 结果 的 模式, 结果 的 响应ID)\n",
    );
    assert_eq!(output, ["成功 离线 offline-response"]);
}

#[test]
fn malformed_ai_mode_is_a_structured_failure_on_all_three_paths() {
    let outcome =
        assert_three_paths_failure("引入 人工智能\n人工智能 请求(\"提示\", 模式 为 \"未知\")\n");
    assert!(outcome.message().contains("模式只支持"));
}

#[test]
fn replayed_responses_match_across_all_three_paths() {
    let outcome = assert_three_paths_with_artifacts(
        "引入 人工智能\n定义 结果 为 人工智能 请求(\"提示\", 模式 为 \"重放\", 响应文件 为 \"响应.json\")\n输出(结果 的 文本, 结果 的 响应ID)\n",
        &[("响应.json", br#"{"id":"resp_fixture","output_text":"cached"}"#)],
        &[],
    );
    assert_eq!(outcome.output, ["cached resp_fixture"]);
}

#[test]
fn structured_extraction_returns_a_table_across_all_three_paths() {
    let outcome = assert_three_paths_with_artifacts(
        r#"引入 人工智能
引入 表格处理
定义 结果 为 人工智能 提取结构化("订单 A01，客户星河，金额 128.50；订单 A02，客户远帆，金额 90", {"订单号": "文本", "客户": "文本", "金额": "数字"}, 模式 为 "重放", 响应文件 为 "提取响应.json")
定义 订单表 为 结果 的 数据
输出(订单表 的 行数, 订单表 的 表头, (订单表 转记录列表())[1] 的 金额)
表格处理 写入表格(订单表, "订单.csv")
"#,
        &[(
            "提取响应.json",
            r#"{"id":"resp_extract","output_text":"{\"数据\":[{\"订单号\":\"A01\",\"客户\":\"星河\",\"金额\":128.5},{\"订单号\":\"A02\",\"客户\":\"远帆\",\"金额\":90}]}"}"#.as_bytes(),
        )],
        &["订单.csv"],
    );
    assert_eq!(outcome.output, ["2 [客户, 订单号, 金额] 128.5"]);
}

#[test]
fn structured_conclusion_keeps_references_across_all_three_paths() {
    let output = assert_three_paths_output(
        "引入 人工智能\n定义 结果 为 人工智能 分析结论({销售额: 120, 毛利率: 0.25}, \"解释趋势\", \"管理层\", 模式 为 \"模拟\")\n输出(结果 的 模式, 结果 的 引用字段, 结果 的 事实, 结果 的 模型解释)\n",
    );
    assert!(output[0].starts_with("模拟"));
    assert!(output[0].contains("销售额") && output[0].contains("毛利率"));
    assert!(output[0].contains("引用字段"));
    assert!(output[0].contains("统计显著性"));
}

#[test]
fn generated_report_is_escaped_and_keeps_a_report_spec_fallback() {
    let output = assert_three_paths_output(
        "引入 人工智能\n定义 草稿 为 人工智能 生成报告({销售额: 120, 备注: \"<script>alert(1)</script>\"}, \"解释趋势\", 模式 为 \"模拟\")\n输出(草稿 的 状态, 草稿 的 可复现级别, 草稿 的 HTML, 草稿 的 报告 的 版本)\n",
    );
    assert!(output[0].starts_with("成功"));
    assert!(output[0].contains("完全可复现"));
    assert!(output[0].contains("&lt;script&gt;"));
    assert!(output[0].ends_with("1"));
}

#[test]
fn failed_ai_generation_keeps_a_standard_report_on_all_three_paths() {
    let output = assert_three_paths_output(
        "引入 人工智能\n定义 草稿 为 人工智能 生成报告({销售额: 120}, \"解释趋势\", 模式 为 \"在线\")\n输出(草稿 的 状态, 草稿 的 报告 的 版本, 草稿 的 警告)\n",
    );
    assert!(output[0].starts_with("部分成功 1"), "{}", output[0]);
    assert!(output[0].contains("API Key"), "{}", output[0]);
}

#[test]
fn generated_report_accepts_editable_template_theme_and_section_order() {
    let output = assert_three_paths_output(
        "引入 人工智能\n定义 草稿 为 人工智能 生成报告({销售额: 120}, \"解释趋势\", 模式 为 \"模拟\", 主题 为 \"经营简报\", HTML模板 为 \"<main data-theme='exec'>{{标题}}|{{结论}}|{{引用}}</main>\")\n输出(草稿 的 HTML, 草稿 的 报告 的 主题)\n",
    );
    assert!(output[0].contains("经营简报") && output[0].contains("AI 增强分析报告"));
    assert!(output[0].contains("<main data-theme='exec'>"));
}

#[test]
fn custom_ai_html_is_rejected_to_standard_draft_and_citations_stay_input_bound() {
    let output = assert_three_paths_output(
        "引入 人工智能\n定义 草稿 为 人工智能 生成报告({销售额: 120}, \"解释趋势\", 模式 为 \"模拟\", HTML模板 为 \"<script>伪造</script>{{结论}}\")\n输出(草稿 的 状态, 草稿 的 警告, 草稿 的 HTML, 草稿 的 引用)\n",
    );
    assert!(output[0].starts_with("部分成功"), "{}", output[0]);
    assert!(output[0].contains("不支持") && !output[0].contains("<script>"));
    assert!(output[0].contains("结果.销售额"));
}
