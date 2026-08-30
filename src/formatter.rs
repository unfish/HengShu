use std::path::Path;

pub fn format_source(source: &str) -> String {
    let mut output = Vec::new();
    let mut indent = 0_usize;
    for original in source.lines() {
        let line = original.trim();
        if line.is_empty() {
            if output.last().is_some_and(|line: &String| !line.is_empty()) {
                output.push(String::new());
            }
            continue;
        }
        let previous_is_match = output.last().is_some_and(|previous: &String| {
            previous.trim_start().starts_with("匹配 ")
                && (previous.ends_with(':') || previous.ends_with('：'))
        });
        let closes = line == "结束"
            || line.starts_with("否则")
            || line.starts_with("捕获")
            || line.starts_with("最终")
            || line.starts_with("其他")
            || (line.starts_with("当 ") && !previous_is_match);
        if closes {
            indent = indent.saturating_sub(1);
        }
        output.push(format!(
            "{}{}",
            "    ".repeat(indent),
            normalize_punctuation(line)
        ));
        let opens = line.ends_with(':')
            || line.ends_with('：')
            || line.ends_with("就一直进行:")
            || line.ends_with("就一直进行：");
        if opens {
            indent += 1;
        }
    }
    while output.last().is_some_and(String::is_empty) {
        output.pop();
    }
    let mut result = output.join("\n");
    result.push('\n');
    result
}

pub fn format_file(path: &Path, check: bool) -> Result<bool, String> {
    let source = crate::resource::read_string_limited(path, crate::resource::MAX_SOURCE_BYTES)
        .map_err(|e| e.to_string())?;
    let formatted = format_source(&source);
    let changed = source != formatted;
    if changed && !check {
        crate::security::atomic_write_file(path, formatted.as_bytes(), true)
            .map_err(|e| e.to_string())?;
    }
    Ok(changed)
}

fn normalize_punctuation(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut quote = None;
    let mut backslashes = 0_usize;
    for character in line.chars() {
        if matches!(character, '"' | '\'') && backslashes.is_multiple_of(2) {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        }
        output.push(if quote.is_none() {
            match character {
                '(' => '（',
                ')' => '）',
                ',' => '，',
                ':' => '：',
                character => character,
            }
        } else {
            character
        });
        if character == '\\' {
            backslashes += 1;
        } else {
            backslashes = 0;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatter_is_idempotent_and_keeps_chinese_structure() {
        let source = "如果 是:\n输出(\"好\",1)\n否则:\n输出(\"否\")\n结束";
        let formatted = format_source(source);
        assert_eq!(formatted, format_source(&formatted));
        assert!(formatted.contains("    输出（\"好\"，1）"));
    }

    #[test]
    fn formatter_keeps_punctuation_after_escaped_quotes_inside_strings() {
        let source = r#"输出("a\"(b")"#;
        let formatted = format_source(source);
        assert!(formatted.contains(r#""a\"(b""#));
        assert_eq!(formatted, format_source(&formatted));
    }

    #[test]
    fn formatter_normalizes_else_if_and_new_loops_idempotently() {
        let source = "如果 否:\n输出(0)\n否则,如果 是:\n遍历 [1,2],记作 值:\n当 值 < 2 时重复:\n值 为 值 + 1\n结束\n结束\n结束";
        let formatted = format_source(source);
        assert!(formatted.contains("否则，如果 是："));
        assert!(formatted.contains("遍历 [1，2]，记作 值："));
        assert!(formatted.contains("当 值 < 2 时重复："));
        assert_eq!(formatted, format_source(&formatted));
    }

    #[test]
    fn formatter_indents_test_lifecycle_blocks() {
        let source = "测试准备:\n定义 数据 为 [1]\n结束\n测试 \"示例\":\n断言不为空(数据)\n结束";
        let formatted = format_source(source);
        assert!(formatted.contains("测试准备：\n    定义 数据 为 [1]\n结束"));
        assert!(formatted.contains("测试 \"示例\"：\n    断言不为空（数据）\n结束"));
        assert_eq!(formatted, format_source(&formatted));
    }
}
