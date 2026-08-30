use crate::diagnostic::{Diagnostic, DiagnosticStage, SourcePosition, SourceSpan};
use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Number(Decimal),
    String(String),
    Word(String),
    Symbol(String),
    Newline,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub column: usize,
    pub span: SourceSpan,
}

impl Token {
    pub fn text(&self) -> String {
        match &self.kind {
            TokenKind::Number(v) => v.to_string(),
            TokenKind::String(v) | TokenKind::Word(v) | TokenKind::Symbol(v) => v.clone(),
            TokenKind::Newline => "\n".into(),
            TokenKind::Eof => "".into(),
        }
    }
}

fn is_word_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}
fn is_word_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

#[cfg(test)]
pub fn lex(source: &str) -> Result<Vec<Token>, String> {
    lex_diagnostic(source).map_err(|error| error.to_string())
}

pub fn lex_diagnostic(source: &str) -> Result<Vec<Token>, Diagnostic> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let mut col = 1;
    while i < chars.len() {
        let ch = chars[i];
        // U+3000 是中文输入法常见的全角空格，与半角空格等价。
        if matches!(ch, ' ' | '\t' | '\r' | '\u{3000}') {
            i += 1;
            col += 1;
            continue;
        }
        if ch == '\n' {
            out.push(Token {
                kind: TokenKind::Newline,
                line,
                column: col,
                span: token_span(line, col, line, col + 1),
            });
            i += 1;
            line += 1;
            col = 1;
            continue;
        }
        if ch == '注' && i + 1 < chars.len() && matches!(chars[i + 1], ':' | '：') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
                col += 1;
            }
            continue;
        }
        let punct = match ch {
            '（' => '(',
            '）' => ')',
            '【' => '[',
            '】' => ']',
            '｛' => '{',
            '｝' => '}',
            '，' => ',',
            '：' => ':',
            '；' => ';',
            c => c,
        };
        if "()[]{},:;.".contains(punct) {
            out.push(Token {
                kind: TokenKind::Symbol(punct.to_string()),
                line,
                column: col,
                span: token_span(line, col, line, col + 1),
            });
            i += 1;
            col += 1;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            let quote = ch;
            let start_line = line;
            let start_col = col;
            i += 1;
            col += 1;
            let mut text = String::new();
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    col += 1;
                    text.push(match chars[i] {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '\\' => '\\',
                        '"' => '"',
                        '\'' => '\'',
                        c => c,
                    });
                    i += 1;
                    col += 1;
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                        col = 1;
                    } else {
                        col += 1;
                    }
                    text.push(chars[i]);
                    i += 1;
                }
            }
            if i >= chars.len() {
                return Err(Diagnostic::new(
                    DiagnosticStage::Lex,
                    SourceSpan::point(start_line, start_col),
                    "字符串缺少结束引号",
                ));
            }
            i += 1;
            col += 1;
            out.push(Token {
                kind: TokenKind::String(text),
                line: start_line,
                column: start_col,
                span: token_span(start_line, start_col, line, col),
            });
            continue;
        }
        let two = if i + 1 < chars.len() {
            format!("{}{}", ch, chars[i + 1])
        } else {
            String::new()
        };
        if matches!(two.as_str(), ">=" | "<=" | "!=" | "==") {
            out.push(Token {
                kind: TokenKind::Symbol(two),
                line,
                column: col,
                span: token_span(line, col, line, col + 2),
            });
            i += 2;
            col += 2;
            continue;
        }
        if "+-*/%^=><".contains(ch) {
            out.push(Token {
                kind: TokenKind::Symbol(ch.to_string()),
                line,
                column: col,
                span: token_span(line, col, line, col + 1),
            });
            i += 1;
            col += 1;
            continue;
        }
        if ch.is_ascii_digit() {
            let start = i;
            let start_col = col;
            let mut dots = 0;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                if chars[i] == '.' {
                    dots += 1;
                }
                i += 1;
                col += 1;
            }
            let raw: String = chars[start..i].iter().collect();
            if dots > 1 {
                return Err(Diagnostic::new(
                    DiagnosticStage::Lex,
                    token_span(line, start_col, line, col),
                    format!("非法数字 {raw}"),
                ));
            }
            let number = Decimal::from_str(&raw).map_err(|_| {
                Diagnostic::new(
                    DiagnosticStage::Lex,
                    token_span(line, start_col, line, col),
                    format!("非法数字 {raw}"),
                )
            })?;
            out.push(Token {
                kind: TokenKind::Number(number),
                line,
                column: start_col,
                span: token_span(line, start_col, line, col),
            });
            continue;
        }
        if is_word_start(ch) {
            let start = i;
            let start_col = col;
            i += 1;
            col += 1;
            while i < chars.len() && is_word_continue(chars[i]) {
                i += 1;
                col += 1;
            }
            out.push(Token {
                kind: TokenKind::Word(chars[start..i].iter().collect()),
                line,
                column: start_col,
                span: token_span(line, start_col, line, col),
            });
            continue;
        }
        return Err(Diagnostic::new(
            DiagnosticStage::Lex,
            SourceSpan::point(line, col),
            format!("无法识别字符 {ch:?}"),
        ));
    }
    out.push(Token {
        kind: TokenKind::Eof,
        line,
        column: col,
        span: SourceSpan::point(line, col),
    });
    Ok(merge_phrases(out))
}

fn merge_phrases(tokens: Vec<Token>) -> Vec<Token> {
    let phrases: &[(&[&str], &str)] = &[
        (&["时", "重复"], "时重复"),
        (&["否则", "如果"], "否则如果"),
        (&["中的", "每个"], "中的每个"),
        (&["都", "进行"], "都进行"),
        (&["就一直", "进行"], "就一直进行"),
        (&["跳出", "循环"], "跳出循环"),
        (&["继续", "下一轮"], "继续下一轮"),
        (&["类型", "限定", "为"], "类型限定为"),
        (&["的", "类型", "是"], "的类型是"),
        (&["类型", "是"], "类型是"),
        (&["否则", "说明"], "否则说明"),
        (&["并且", "满足"], "并且满足"),
    ];
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let mut matched = false;
        for (parts, replacement) in phrases {
            if i + parts.len() <= tokens.len()
                && parts.iter().enumerate().all(|(k, p)| {
                    matches!(&tokens[i + k].kind, TokenKind::Word(text) if text == p)
                        && (k == 0 || tokens[i + k].line == tokens[i].line)
                })
            {
                out.push(Token {
                    kind: TokenKind::Word((*replacement).into()),
                    line: tokens[i].line,
                    column: tokens[i].column,
                    span: SourceSpan {
                        start: tokens[i].span.start,
                        end: tokens[i + parts.len() - 1].span.end,
                    },
                });
                i += parts.len();
                matched = true;
                break;
            }
        }
        if !matched {
            out.push(tokens[i].clone());
            i += 1;
        }
    }
    out
}

fn token_span(
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
) -> SourceSpan {
    SourceSpan {
        start: SourcePosition {
            line: start_line,
            column: start_column,
        },
        end: SourcePosition {
            line: end_line,
            column: end_column,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lexes_chinese_and_full_width_punctuation() {
        let t = lex("定义 金额 为 12.5\n如果 金额 >= 10：\n输出（\"好\"）\n结束").unwrap();
        assert!(t.iter().any(|x| x.text() == "金额"));
        assert!(t.iter().any(|x| x.text() == ">="));
        assert!(t.iter().any(|x| x.text() == "("));
    }

    #[test]
    fn keeps_syntax_like_string_contents_as_strings() {
        let tokens = lex("输出(\")\")").unwrap();
        assert!(matches!(
            tokens.get(2).map(|token| &token.kind),
            Some(TokenKind::String(value)) if value == ")"
        ));
    }

    #[test]
    fn treats_the_fullwidth_space_as_whitespace() {
        let tokens = lex("定义\u{3000}金额\u{3000}为\u{3000}1").unwrap();
        assert_eq!(
            tokens
                .iter()
                .map(Token::text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>(),
            ["定义", "金额", "为", "1"]
        );
    }
}
