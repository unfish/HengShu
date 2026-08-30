//! Project-local configuration loaded from an `环境.配置` file.
//!
//! Values are intentionally read on demand from the source/package directory;
//! they are never copied into process-wide environment state or diagnostics.

use crate::resource::read_string_limited;
use std::path::Path;

const MAX_ENV_BYTES: usize = 128 * 1024;
const MAX_ENV_LINE_BYTES: usize = 16 * 1024;

/// Read a configuration value from `<base_dir>/环境.配置`, falling back to the
/// process environment for backwards compatibility with existing deployments.
pub(crate) fn file_value(base_dir: &Path, name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    parse_file(base_dir)
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        })
        .filter(|value| !value.is_empty())
}

pub(crate) fn value(base_dir: &Path, name: &str) -> Option<String> {
    file_value(base_dir, name)
        .or_else(|| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
}

pub(crate) fn values(base_dir: &Path) -> Vec<(String, String)> {
    let mut entries = parse_file(base_dir).unwrap_or_default();
    for (name, value) in std::env::vars() {
        if !entries.iter().any(|(key, _)| key == &name) {
            entries.push((name, value));
        }
    }
    entries
}

fn parse_file(base_dir: &Path) -> Result<Vec<(String, String)>, String> {
    let path = base_dir.join("环境.配置");
    let source = read_string_limited(&path, MAX_ENV_BYTES).map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for line in source.lines() {
        if line.len() > MAX_ENV_LINE_BYTES {
            return Err("环境.配置单行超过资源限制".into());
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((name, raw_value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !valid_name(name) {
            continue;
        }
        entries.push((name.to_string(), parse_value(raw_value.trim())));
    }
    Ok(entries)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        && !name.as_bytes()[0].is_ascii_digit()
}

fn parse_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            let inner = &value[1..value.len() - 1];
            if bytes[0] == b'"' {
                return inner
                    .replace("\\n", "\n")
                    .replace("\\r", "\r")
                    .replace("\\t", "\t")
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\");
            }
            return inner.to_string();
        }
    }
    value.split_once(" #").map_or_else(
        || value.to_string(),
        |(value, _)| value.trim_end().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_common_dotenv_forms() {
        let directory = tempdir().unwrap();
        std::fs::write(
            directory.path().join("环境.配置"),
            "# comment\nexport API_KEY=abc\nQUOTED=\"a\\nb\"\nSINGLE='x y'\n",
        )
        .unwrap();
        assert_eq!(value(directory.path(), "API_KEY").as_deref(), Some("abc"));
        assert_eq!(value(directory.path(), "QUOTED").as_deref(), Some("a\nb"));
        assert_eq!(value(directory.path(), "SINGLE").as_deref(), Some("x y"));
    }

    #[test]
    fn ignores_invalid_names_and_uses_environment_fallback() {
        let directory = tempdir().unwrap();
        std::fs::write(directory.path().join("环境.配置"), "BAD-NAME=x\n").unwrap();
        assert_eq!(value(directory.path(), "BAD-NAME"), None);
    }
}
