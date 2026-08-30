//! 结构门禁：用源码扫描守住模块分层，防止重构过程中出现跨层反向依赖。
//!
//! 这些断言不检查行为，只检查依赖方向。它们存在的意义是：通用层一旦引用业务层，
//! 编译仍会通过，但分层就已经失效，只有显式检查才能拦住。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 读取一个模块目录（含同名 `foo.rs`）下的全部 Rust 源码，按文件路径归档。
fn module_sources(module: &str) -> BTreeMap<String, String> {
    let root = source_root();
    let mut files = BTreeMap::new();
    let single = root.join(format!("{module}.rs"));
    if single.is_file() {
        files.insert(
            format!("src/{module}.rs"),
            std::fs::read_to_string(&single).unwrap(),
        );
    }
    let directory = root.join(module);
    if directory.is_dir() {
        collect(&directory, &root, &mut files);
    }
    assert!(
        !files.is_empty(),
        "模块“{module}”没有找到任何源码文件，结构门禁需要更新"
    );
    files
}

fn collect(directory: &Path, root: &Path, files: &mut BTreeMap<String, String>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, root, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let label = format!("src/{}", path.strip_prefix(root).unwrap().display());
            files.insert(label, std::fs::read_to_string(&path).unwrap());
        }
    }
}

/// 只保留能表达依赖的代码行：跳过 `//` 注释和 `//!` 模块文档，避免文字描述触发断言。
fn code_lines(contents: &str) -> impl Iterator<Item = (usize, &str)> {
    contents
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.starts_with("//"))
}

fn find_violation(
    sources: &BTreeMap<String, String>,
    module: &str,
    forbidden: &[&str],
) -> Option<String> {
    for (file, contents) in sources {
        for (number, line) in code_lines(contents) {
            for target in forbidden {
                for path in [
                    format!("crate::{target}"),
                    format!("super::super::{target}"),
                ] {
                    if line.contains(&path) {
                        return Some(format!(
                            "{file}:{number} 违反分层：{module} 不得依赖 {target}\n    {line}"
                        ));
                    }
                }
            }
        }
    }
    None
}

fn assert_no_dependency(module: &str, forbidden: &[&str]) {
    if let Some(violation) = find_violation(&module_sources(module), module, forbidden) {
        panic!("{violation}");
    }
}

#[test]
fn the_generic_stream_layer_does_not_depend_on_business_modules() {
    assert_no_dependency(
        "stream",
        &[
            "table", "analysis", "report", "finance", "project", "stdlib", "runtime", "vm",
            "compiler", "parser", "checker", "lsp", "cli", "pack",
        ],
    );
}

#[test]
fn the_stream_layer_only_reaches_for_values_and_resource_budgets() {
    let allowed = ["crate::value", "crate::resource", "crate::stream"];
    for (file, contents) in module_sources("stream") {
        for (number, line) in code_lines(&contents) {
            let Some(rest) = line.split_once("crate::") else {
                continue;
            };
            let referenced = format!("crate::{}", rest.1);
            assert!(
                allowed.iter().any(|prefix| referenced.starts_with(prefix)),
                "{file}:{number} 通用流层只允许依赖 {allowed:?}\n    {line}"
            );
        }
    }
}

#[test]
fn business_modules_reach_the_stream_layer_only_through_its_public_surface() {
    for module in ["table", "analysis", "stdlib"] {
        for (file, contents) in module_sources(module) {
            for (number, line) in code_lines(&contents) {
                for private in ["crate::stream::csv::", "crate::stream::bytes::"] {
                    assert!(
                        !line.contains(private),
                        "{file}:{number} 必须经由 crate::stream 的导出项访问流层\n    {line}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_detector_fires_on_a_violation_and_ignores_commented_mentions() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "src/stream/probe.rs".to_string(),
        "use crate::value::Value;\nfn probe() { crate::table::summarize(); }\n".to_string(),
    );
    let violation = find_violation(&sources, "stream", &["table"]).expect("检测器应当报告违规");
    assert!(violation.contains("src/stream/probe.rs:2"), "{violation}");
    assert!(violation.contains("不得依赖 table"), "{violation}");

    let mut commented = BTreeMap::new();
    commented.insert(
        "src/stream/probe.rs".to_string(),
        "//! 本层不得依赖 crate::table\n// 见 crate::table 的说明\nuse crate::value::Value;\n"
            .to_string(),
    );
    assert!(find_violation(&commented, "stream", &["table"]).is_none());
}

#[test]
fn the_removed_streaming_module_path_is_gone_everywhere() {
    let root = source_root();
    assert!(
        !root.join("table/streaming.rs").exists(),
        "table/streaming.rs 已重命名为 table/streaming_summary.rs"
    );
    let mut files = BTreeMap::new();
    collect(&root, &root, &mut files);
    for (file, contents) in files {
        for (number, line) in code_lines(&contents) {
            assert!(
                !line.contains("table::streaming::") && !line.contains("mod streaming;"),
                "{file}:{number} 仍引用旧模块路径 table::streaming\n    {line}"
            );
        }
    }
}
