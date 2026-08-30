use crate::runtime;
use std::fs;
use std::path::Path;
use std::time::Instant;
use walkdir::WalkDir;

pub fn document(root: &Path, output: &Path) -> Result<(), String> {
    let mut sections = String::new();
    for entry in WalkDir::new(root.join("src")) {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "代码")
        {
            continue;
        }
        let source =
            crate::resource::read_string_limited(entry.path(), crate::resource::MAX_SOURCE_BYTES)
                .map_err(|e| e.to_string())?;
        let comments = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("注："))
            .map(str::trim)
            .collect::<Vec<_>>();
        sections.push_str(&format!(
            "<section><h2>{}</h2><p>{}</p></section>",
            escape(
                &entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(entry.path())
                    .display()
                    .to_string()
            ),
            escape(&comments.join(" "))
        ));
    }
    let html = format!(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><title>衡枢项目文档</title><body><h1>衡枢项目文档</h1>{sections}</body></html>"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::security::atomic_write_file(output, html.as_bytes(), true)
}

pub fn benchmark(source: &Path, iterations: usize) -> Result<(f64, f64), String> {
    let text = crate::resource::read_string_limited(source, crate::resource::MAX_SOURCE_BYTES)
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    for _ in 0..iterations {
        runtime::execute_with_args(
            &text,
            source.parent().unwrap_or(Path::new(".")),
            false,
            Vec::new(),
        )?;
    }
    let total = start.elapsed().as_secs_f64();
    Ok((total, total / iterations.max(1) as f64))
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
