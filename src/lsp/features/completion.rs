//! 补全：LSP 关键字触发的静态补全项。

use crate::parser;

pub(in crate::lsp) fn keywords() -> Vec<&'static str> {
    let mut keywords = parser::CURRENT_KEYWORDS.to_vec();
    keywords.extend([
        "文件系统",
        "网络",
        "网页",
        "数据库",
        "并发",
        "财务",
        "统计分析",
    ]);
    keywords
}
