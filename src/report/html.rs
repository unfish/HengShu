use super::spec::{ColumnSpec, FormatSpec, TableSpec};
use crate::value::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) fn render_html(
    title: &str,
    sections: &Value,
    collection_limit: usize,
) -> Result<String, String> {
    let mut body = String::new();
    let mut navigation = String::new();
    let mut section_index = 0usize;
    for section in sections.iter_values_limited(collection_limit)? {
        match section {
            Value::Record(section) => {
                let section = section.borrow();
                let heading = section
                    .get("标题")
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let kind = section.get("类型").map(ToString::to_string);
                let content = section.get("内容").cloned().unwrap_or(Value::Null);
                let spec = TableSpec::from_section(&section, collection_limit)?;
                let rendered = render_content(&content, kind.as_deref(), &heading, &spec)?;
                section_index += 1;
                let id = format!("section-{section_index}");
                navigation.push_str(&format!(
                    "<li><a href=\"#{id}\">{}. {}</a></li>",
                    section_index,
                    escape(&heading)
                ));
                body.push_str(&format!(
                    "<section id=\"{id}\"><h2>{}. {}</h2>{}</section>",
                    section_index,
                    escape(&heading),
                    rendered,
                ));
            }
            value => body.push_str(&render_content(&value, None, "", &TableSpec::default())?),
        }
    }
    let body = if navigation.is_empty() {
        body
    } else {
        format!("<nav class=\"toc\" aria-label=\"报告目录\"><strong>报告目录</strong><ul>{navigation}</ul></nav>{body}")
    };
    Ok(format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{0}</title><style>{2}</style></head><body><h1>{0}</h1>{1}</body></html>",
        escape(title),
        body,
        STYLE
    ))
}

const STYLE: &str = ":root{color-scheme:light;--ink:#17221d;--muted:#66716c;--line:#d9dfdc;--accent:#1f6f54;--soft:#f4f7f5}*{box-sizing:border-box}body{font-family:system-ui,-apple-system,\"Segoe UI\",sans-serif;max-width:1240px;margin:32px auto;padding:0 24px 48px;color:var(--ink);background:#fff;line-height:1.55}h1{border-bottom:3px solid var(--accent);padding-bottom:12px;margin-bottom:32px}h2{font-size:19px;margin:0 0 14px}section{margin:34px 0}.table-wrap{overflow:auto;border:1px solid var(--line)}table{width:100%;border-collapse:collapse;font-size:13px;white-space:nowrap}caption{caption-side:top;text-align:left;padding:9px 11px;color:var(--muted);font-size:12px}th,td{padding:9px 11px;border-bottom:1px solid var(--line)}.cell-left{text-align:left}.cell-center{text-align:center}.cell-right{text-align:right}th{position:sticky;top:0;background:#edf3f0;color:#26332d;font-weight:650}tbody tr:nth-child(even){background:#fafcfb}tfoot td{font-weight:650;background:#f1f5f3;border-top:2px solid var(--line)}.table-note{margin:8px 0 0;color:var(--muted);font-size:12px}.column-notes{margin:8px 0 0;color:var(--muted);font-size:12px}.column-notes dt{font-weight:650;display:inline}.column-notes dd{display:inline;margin:0 0 0 4px}.column-notes div{margin:2px 0}.metrics{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:1px;background:var(--line);border:1px solid var(--line)}.metric{background:#fff;padding:14px 16px;min-width:0}.metric dt{color:var(--muted);font-size:12px;margin-bottom:5px}.metric dd{margin:0;font-size:18px;font-weight:650;overflow-wrap:anywhere}ul{padding-left:22px}p{margin:8px 0}figure{margin:0}figure img{display:block;width:100%;height:auto;border:1px solid var(--line)}code{background:var(--soft);padding:2px 5px}@media print{body{max-width:none;margin:0;padding:0}.toc{display:none}section{break-inside:avoid}thead{display:table-header-group}table{white-space:normal}}@media(max-width:640px){body{padding:0 12px;margin-top:20px}th,td{padding:7px 8px}.metrics{grid-template-columns:repeat(2,minmax(0,1fr))}}";

const HTML_TABLE_ROW_LIMIT: usize = 100;

fn render_content(
    value: &Value,
    kind: Option<&str>,
    heading: &str,
    spec: &TableSpec,
) -> Result<String, String> {
    if kind == Some("图表") {
        return Ok(format!(
            "<figure><img src=\"{}\" alt=\"{}\">{}</figure>",
            escape_attribute(&value.to_string()),
            escape_attribute(heading),
            spec.caption
                .as_deref()
                .map(|caption| format!(
                    "<figcaption class=\"table-note\">{}</figcaption>",
                    escape(caption)
                ))
                .unwrap_or_default(),
        ));
    }
    match value {
        Value::Table(table) => render_rows(&table.borrow().rows, kind == Some("指标"), spec),
        Value::Record(record) => Ok(render_metrics(&record.borrow(), spec)),
        Value::List(values) => {
            let values = values.borrow();
            if values.iter().all(|value| matches!(value, Value::Record(_))) {
                let rows = values
                    .iter()
                    .take(HTML_TABLE_ROW_LIMIT + 1)
                    .filter_map(|value| match value {
                        Value::Record(record) => Some(record.borrow().clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if kind == Some("指标") || (rows.len() == 1 && !spec.has_declared_columns()) {
                    render_leading_metrics(&rows, spec)
                } else {
                    render_rows_with_total(&rows, values.len(), spec)
                }
            } else if values.iter().all(|value| matches!(value, Value::Text(_))) {
                Ok(format!(
                    "<ul>{}</ul>",
                    values
                        .iter()
                        .map(|value| format!("<li>{}</li>", escape(&value.to_string())))
                        .collect::<String>()
                ))
            } else {
                let format = FormatSpec::default();
                Ok(format!(
                    "<ul>{}</ul>",
                    values
                        .iter()
                        .map(|value| format!("<li>{}</li>", escape(&format.render(value))))
                        .collect::<String>()
                ))
            }
        }
        Value::Text(text) => Ok(text
            .lines()
            .map(|line| format!("<p>{}</p>", escape(line)))
            .collect()),
        Value::Null => Ok("<p class=\"table-note\">无数据</p>".into()),
        value => Ok(format!(
            "<p>{}</p>",
            escape(&FormatSpec::default().render(value))
        )),
    }
}

fn render_rows(
    rows: &[BTreeMap<String, Value>],
    metrics: bool,
    spec: &TableSpec,
) -> Result<String, String> {
    if metrics || (rows.len() == 1 && !spec.has_declared_columns()) {
        return render_leading_metrics(rows, spec);
    }
    render_rows_with_total(rows, rows.len(), spec)
}

fn render_leading_metrics(
    rows: &[BTreeMap<String, Value>],
    spec: &TableSpec,
) -> Result<String, String> {
    rows.first()
        .map(|row| render_metrics(row, spec))
        .ok_or_else(|| "报告小节没有数据".to_string())
}

fn render_rows_with_total(
    rows: &[BTreeMap<String, Value>],
    total: usize,
    spec: &TableSpec,
) -> Result<String, String> {
    if rows.is_empty() {
        return Ok("<p class=\"table-note\">无数据</p>".into());
    }
    let columns = spec.resolve(rows);
    let caption = spec
        .caption
        .as_deref()
        .map(|caption| format!("<caption>{}</caption>", escape(caption)))
        .unwrap_or_default();
    let colgroup = if columns.iter().any(|column| column.width.is_some()) {
        format!(
            "<colgroup>{}</colgroup>",
            columns
                .iter()
                .map(|column| match column.width {
                    Some(width) => format!("<col style=\"width:{width}ch\">"),
                    None => "<col>".to_string(),
                })
                .collect::<String>()
        )
    } else {
        String::new()
    };
    let header = columns
        .iter()
        .map(|column| {
            format!(
                "<th scope=\"col\" class=\"{}\">{}</th>",
                column.alignment().css_class(),
                escape(&column.header())
            )
        })
        .collect::<String>();
    let body = rows
        .iter()
        .take(HTML_TABLE_ROW_LIMIT)
        .map(|row| {
            let cells = columns
                .iter()
                .map(|column| {
                    let value = row.get(&column.name).unwrap_or(&Value::Null);
                    format!(
                        "<td class=\"{}\">{}</td>",
                        column.alignment().css_class(),
                        escape(&column.render(value))
                    )
                })
                .collect::<String>();
            format!("<tr>{cells}</tr>")
        })
        .collect::<String>();
    let footer = render_totals(&columns, rows, spec);
    let note = if total > HTML_TABLE_ROW_LIMIT {
        format!(
            "<p class=\"table-note\">共 {total} 行，报告中展示前 {HTML_TABLE_ROW_LIMIT} 行；完整数据请查看对应 CSV 文件。</p>"
        )
    } else {
        format!("<p class=\"table-note\">共 {total} 行</p>")
    };
    let wide = if columns.len() > 8 {
        format!(
            "<p class=\"table-note\">本表共 {} 列，窄屏可横向滚动查看，打印时按纸张宽度换行。</p>",
            columns.len()
        )
    } else {
        String::new()
    };
    Ok(format!(
        "<div class=\"table-wrap\"><table>{caption}{colgroup}<thead><tr>{header}</tr></thead><tbody>{body}</tbody>{footer}</table></div>{note}{wide}{}",
        render_descriptions(spec)
    ))
}

fn render_totals(
    columns: &[ColumnSpec],
    rows: &[BTreeMap<String, Value>],
    spec: &TableSpec,
) -> String {
    let Some(label) = spec.total_label() else {
        return String::new();
    };
    let mut labelled = false;
    let cells = columns
        .iter()
        .map(|column| {
            let alignment = column.alignment().css_class();
            match column.total_text(rows) {
                Some(text) => format!("<td class=\"{alignment}\">{}</td>", escape(&text)),
                None if !labelled => {
                    labelled = true;
                    format!("<td class=\"cell-left\">{label}</td>")
                }
                None => format!("<td class=\"{alignment}\"></td>"),
            }
        })
        .collect::<String>();
    format!("<tfoot><tr>{cells}</tr></tfoot>")
}

fn render_descriptions(spec: &TableSpec) -> String {
    let descriptions = spec.descriptions();
    if descriptions.is_empty() {
        return String::new();
    }
    format!(
        "<dl class=\"column-notes\"><div><strong>指标解释</strong></div>{}</dl>",
        descriptions
            .into_iter()
            .map(|(header, description)| format!(
                "<div><dt>{}</dt><dd>{}</dd></div>",
                escape(&header),
                escape(&description)
            ))
            .collect::<String>()
    )
}

fn render_metrics(record: &BTreeMap<String, Value>, spec: &TableSpec) -> String {
    let mut flattened = Vec::new();
    for (name, value) in record {
        flatten_metric(name, value, &mut flattened);
    }
    let items = flattened
        .into_iter()
        .map(|(name, value)| {
            let column = spec.column_for(&name);
            format!(
                "<div class=\"metric\"><dt>{}</dt><dd>{}</dd></div>",
                escape(&column.header()),
                escape(&column.render(&value)),
            )
        })
        .collect::<String>();
    format!(
        "<dl class=\"metrics\">{items}</dl>{}",
        render_descriptions(spec)
    )
}

fn flatten_metric(prefix: &str, value: &Value, output: &mut Vec<(String, Value)>) {
    match value {
        Value::Record(record) => {
            for (name, child) in record.borrow().iter() {
                flatten_metric(&format!("{prefix}.{name}"), child, output);
            }
        }
        Value::List(values)
            if values
                .borrow()
                .iter()
                .all(|item| matches!(item, Value::Record(_))) =>
        {
            for (index, child) in values.borrow().iter().enumerate() {
                flatten_metric(&format!("{prefix}[{}]", index + 1), child, output);
            }
        }
        _ => output.push((prefix.to_string(), value.clone())),
    }
}

pub(super) fn resolve(base_dir: &Path, value: &Value) -> PathBuf {
    let path = match value {
        Value::Path(path) => path.as_ref().clone(),
        value => PathBuf::from(value.to_string()),
    };
    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_attribute(value: &str) -> String {
    escape(value).replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn html_report_renders_structured_values_semantically() {
        let rows = Value::list(vec![
            Value::record(BTreeMap::from([
                ("月份".into(), Value::Text("2026-01".into())),
                ("销售额".into(), Value::Number(Decimal::new(12_345_678, 2))),
            ])),
            Value::record(BTreeMap::from([
                ("月份".into(), Value::Text("2026-02".into())),
                ("销售额".into(), Value::Number(Decimal::from(9_000))),
            ])),
        ]);
        let sections = Value::list(vec![
            Value::record(BTreeMap::from([
                ("标题".into(), Value::Text("月度趋势".into())),
                ("类型".into(), Value::Text("表格".into())),
                ("内容".into(), rows),
            ])),
            Value::record(BTreeMap::from([
                ("标题".into(), Value::Text("判断".into())),
                ("类型".into(), Value::Text("列表".into())),
                (
                    "内容".into(),
                    Value::list(vec![Value::Text("需要关注复购".into())]),
                ),
            ])),
            Value::record(BTreeMap::from([
                ("标题".into(), Value::Text("图形".into())),
                ("类型".into(), Value::Text("图表".into())),
                ("内容".into(), Value::Text("趋势.svg".into())),
            ])),
        ]);
        let html = render_html(
            "经营报告",
            &sections,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        assert!(html.contains("<table>"));
        assert!(html.contains("月份</th>"));
        assert!(html.contains("123,456.78"));
        assert!(html.contains("<li>需要关注复购</li>"));
        assert!(html.contains("<img src=\"趋势.svg\""));
        assert!(!html.contains("<pre>"));
    }

    #[test]
    fn column_spec_drives_html_header_order_alignment_totals_and_notes() {
        let rows = Value::list(vec![
            Value::record(BTreeMap::from([
                ("月份".into(), Value::Text("2026-01".into())),
                ("销售额".into(), Value::Number(Decimal::from(1_000))),
                ("毛利率".into(), Value::Float(12.5)),
            ])),
            Value::record(BTreeMap::from([
                ("月份".into(), Value::Text("2026-02".into())),
                ("销售额".into(), Value::Number(Decimal::from(2_500))),
                ("毛利率".into(), Value::Null),
            ])),
        ]);
        let sections = Value::list(vec![Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("月度趋势".into())),
            ("类型".into(), Value::Text("表格".into())),
            ("说明".into(), Value::Text("口径：含税".into())),
            ("内容".into(), rows),
            (
                "列规范".into(),
                Value::list(vec![
                    Value::record(BTreeMap::from([
                        ("名称".into(), Value::Text("月份".into())),
                        ("格式".into(), Value::Text("文本".into())),
                    ])),
                    Value::record(BTreeMap::from([
                        ("名称".into(), Value::Text("销售额".into())),
                        ("标题".into(), Value::Text("销售金额".into())),
                        (
                            "格式".into(),
                            Value::record(BTreeMap::from([
                                ("类型".into(), Value::Text("金额".into())),
                                ("单位".into(), Value::Text("元".into())),
                                ("小数位".into(), Value::Number(Decimal::from(2))),
                            ])),
                        ),
                        ("汇总".into(), Value::Text("求和".into())),
                        ("说明".into(), Value::Text("不含退货".into())),
                    ])),
                    Value::record(BTreeMap::from([
                        ("名称".into(), Value::Text("毛利率".into())),
                        (
                            "格式".into(),
                            Value::record(BTreeMap::from([
                                ("类型".into(), Value::Text("百分比".into())),
                                ("小数位".into(), Value::Number(Decimal::from(1))),
                                ("空值".into(), Value::Text("缺数据".into())),
                            ])),
                        ),
                    ])),
                ]),
            ),
        ]))]);
        let html = render_html(
            "经营报告",
            &sections,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        let month = html.find("月份</th>").unwrap();
        let sales = html.find("销售金额（元）</th>").unwrap();
        let margin = html.find("毛利率</th>").unwrap();
        assert!(month < sales && sales < margin, "列顺序必须由列规范决定");
        assert!(html.contains("<caption>口径：含税</caption>"));
        assert!(html.contains("1,000.00"));
        assert!(html.contains("12.5%"));
        assert!(html.contains("缺数据"));
        assert!(html.contains("<tfoot>"));
        assert!(html.contains("3,500.00"));
        assert!(html.contains("指标解释"));
        assert!(html.contains("不含退货"));
        assert!(html.contains("class=\"cell-left\">2026-01</td>"));
    }

    #[test]
    fn html_no_longer_guesses_percent_from_column_name() {
        let sections = Value::list(vec![Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("占比".into())),
            ("类型".into(), Value::Text("表格".into())),
            (
                "内容".into(),
                Value::list(vec![
                    Value::record(BTreeMap::from([
                        ("分类".into(), Value::Text("华东".into())),
                        ("增长率".into(), Value::Number(Decimal::new(125, 1))),
                    ])),
                    Value::record(BTreeMap::from([
                        ("分类".into(), Value::Text("华南".into())),
                        ("增长率".into(), Value::Number(Decimal::new(88, 1))),
                    ])),
                ]),
            ),
        ]))]);
        let html = render_html(
            "经营报告",
            &sections,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        assert!(html.contains("12.5"));
        assert!(!html.contains("12.5%"), "未声明百分比格式时不得补 % 号");
    }

    #[test]
    fn empty_metrics_section_returns_a_language_error_instead_of_panicking() {
        let sections = Value::list(vec![Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("关键指标".into())),
            ("类型".into(), Value::Text("指标".into())),
            ("内容".into(), Value::list(Vec::new())),
        ]))]);
        let error = render_html(
            "经营报告",
            &sections,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap_err();
        assert_eq!(error, "报告小节没有数据");

        let sections = Value::list(vec![Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("明细".into())),
            ("类型".into(), Value::Text("表格".into())),
            ("内容".into(), Value::list(Vec::new())),
        ]))]);
        let html = render_html(
            "经营报告",
            &sections,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
        )
        .unwrap();
        assert!(html.contains("无数据"), "{html}");
    }
}
