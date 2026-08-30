use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SourcePosition {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SourceSpan {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

impl SourceSpan {
    pub const fn point(line: usize, column: usize) -> Self {
        Self {
            start: SourcePosition { line, column },
            end: SourcePosition {
                line,
                column: column + 1,
            },
        }
    }

    pub const fn line(line: usize) -> Self {
        Self {
            start: SourcePosition { line, column: 1 },
            end: SourcePosition { line, column: 1 },
        }
    }

    /// 折叠到单个位置的空范围。隐式声明没有对应源码文本，用空范围锚定它所属的语法位置：
    /// 按光标位置查找永远不会选中它，因此不会与真正写在这里的名字争夺同一范围。
    pub const fn collapsed(start: SourcePosition) -> Self {
        Self { start, end: start }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticStage {
    Lex,
    Parse,
    Check,
    Compile,
}

impl DiagnosticStage {
    pub(crate) fn category(self) -> &'static str {
        match self {
            Self::Lex => "词法错误",
            Self::Parse => "解析错误",
            Self::Check => "检查错误",
            Self::Compile => "编译错误",
        }
    }
}

/// 位置只在这里渲染一次：命令行的`检查`、`打包`、`运行`和嵌入接口共用同一段文本。
/// 只有携带列信息的范围才输出列号，整行范围保持“第 N 行发生X：”。
pub(crate) fn render_location(span: SourceSpan, category: &str, message: &str) -> String {
    let line = span.start.line;
    if span.start.column > 1 || span.end.column > span.start.column + 1 {
        format!(
            "第 {line} 行第 {} 列发生{category}：\n{message}",
            span.start.column
        )
    } else {
        format!("第 {line} 行发生{category}：\n{message}")
    }
}

/// 设置此环境变量为一个文件路径后，命令行与独立包在失败时把结构化诊断写入该文件。
/// 渲染文本只面向人阅读，工具链和差分门禁需要不经反解就能读到位置与原因链。
pub const DIAGNOSTIC_FILE_VARIABLE: &str = "HENGSHU_DIAGNOSTIC_FILE";

/// 一次失败的结构化身份：类别、精确范围、消息、调用栈、参数定义范围和原因链。
/// 进程内与跨进程共用同一结构，因此比较错误时不需要从渲染文本反解，也不需要伪造列号。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticReport {
    pub category: Option<String>,
    pub span: Option<SourceSpan>,
    pub message: String,
    pub stack: Vec<String>,
    pub definition_span: Option<SourceSpan>,
    pub cause: Option<Box<DiagnosticReport>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub stage: DiagnosticStage,
    pub span: SourceSpan,
    pub message: String,
}

impl Diagnostic {
    pub fn new(stage: DiagnosticStage, span: SourceSpan, message: impl Into<String>) -> Self {
        Self {
            stage,
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&render_location(
            self.span,
            self.stage.category(),
            &self.message,
        ))
    }
}

impl std::error::Error for Diagnostic {}
