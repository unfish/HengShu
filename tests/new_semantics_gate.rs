//! 本轮新增语义的三路径差分：隐私、对账、表格敏感标签和原格式模板替换在源码
//! 解释器、字节码 VM 和独立打包程序上必须给出相同输出、诊断、交付物和观察摘要。

mod common;

use common::{
    assert_three_paths_failure, assert_three_paths_output, assert_three_paths_with_artifacts,
    ExecutionStatus,
};
use std::io::Write;
use zip::write::{SimpleFileOptions, ZipWriter};

fn xlsx_template() -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        let mut archive = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        for (name, content) in [
            ("[Content_Types].xml", "<Types/>".as_bytes()),
            ("xl/workbook.xml", "<workbook/>".as_bytes()),
            (
                "xl/worksheets/汇总.xml",
                r#"<worksheet><sheetData><row r="1"><c r="B1"><v>0</v></c></row></sheetData></worksheet>"#.as_bytes(),
            ),
        ] {
            archive.start_file(name, SimpleFileOptions::default()).unwrap();
            archive.write_all(content).unwrap();
        }
        archive.finish().unwrap();
    }
    buffer
}

#[test]
fn privacy_functions_agree_across_all_three_paths() {
    let output = assert_three_paths_output(
        "引入 隐私\n\
输出(隐私 掩码(\"110101199001011234\"))\n\
输出(隐私 掩码(\"13800001111\", 保留后 为 4))\n\
输出(隐私 假名化(\"张三\", \"盐值\"))\n\
输出(隐私 分箱(37.4, 5))\n",
    );
    assert_eq!(output[0], "******************");
    assert_eq!(output[1], "*******1111");
    assert_eq!(output[3], "35");
}

#[test]
fn privacy_mask_rejects_keeping_both_ends_on_all_three_paths() {
    let outcome = assert_three_paths_failure(
        "引入 隐私\n输出(隐私 掩码(\"13800001111\", 保留前 为 3, 保留后 为 4))\n",
    );
    assert!(outcome.message().contains("只能保留前缀或后缀之一"));
}

#[test]
fn reconciliation_and_sensitivity_labels_agree_across_all_three_paths() {
    let output = assert_three_paths_output(
        "引入 表格处理\n\
引入 对账\n\
定义 流水 为 表格处理 把记录整理成表格([{凭证号: \"A1\", 金额: 100}, {凭证号: \"A2\", 金额: 200}])\n\
定义 台账 为 表格处理 把记录整理成表格([{凭证号: \"A1\", 金额: 100}, {凭证号: \"A3\", 金额: 300}])\n\
定义 标注 为 流水 设置列敏感(\"凭证号\", \"财务\") 标记列脱敏(\"凭证号\")\n\
定义 结果 为 对账 比对(标注, 台账, {编号字段: \"凭证号\", 精确键: [\"凭证号\"], 比较字段: [\"金额\"], 金额字段: [\"金额\"]})\n\
定义 统计 为 对账 统计(结果)\n\
输出(结果 的 行数)\n\
输出(统计 的 已匹配, 统计 的 仅左侧, 统计 的 仅右侧)\n\
输出(标注 列敏感(\"凭证号\") 的 类别, 标注 列敏感(\"凭证号\") 的 已脱敏)\n",
    );
    assert_eq!(output[0], "3");
    assert_eq!(output[1], "1 1 1");
    assert_eq!(output[2], "[财务] 是");
}

#[test]
fn reconciliation_rejects_a_missing_key_field_on_all_three_paths() {
    let outcome = assert_three_paths_failure(
        "引入 对账\n定义 结果 为 对账 比对([{甲: 1}], [{甲: 1}], {精确键: [\"凭证号\"]})\n输出(结果 的 行数)\n",
    );
    assert!(outcome.message().contains("对账字段不存在"));
}

#[test]
fn template_replacement_and_report_artifacts_are_identical_across_all_three_paths() {
    let template = xlsx_template();
    let outcome = assert_three_paths_with_artifacts(
        "引入 表格处理\n\
引入 交付\n\
引入 报告\n\
定义 表 为 表格处理 把记录整理成表格([{科目: \"货款\", 金额: 100}, {科目: \"运费\", 金额: 20}])\n\
表格处理 写入表格(表, \"结果/明细.csv\")\n\
定义 单页 为 报告 创建(\"月度交付\", [{标题: \"明细\", 类型: \"表格\", 内容: 表}])\n\
报告 导出(单页, \"HTML\", \"结果/月度交付.html\")\n\
定义 替换 为 交付 模板替换(\"模板.xlsx\", \"结果/月报.xlsx\", {\"汇总!B1\": \"120\"})\n\
输出(替换 的 替换数)\n",
        &[("模板.xlsx", &template)],
        &["结果/明细.csv", "结果/月度交付.html", "结果/月报.xlsx"],
    );
    assert_eq!(outcome.status, ExecutionStatus::Success);
    assert_eq!(outcome.output, ["1"]);
}
