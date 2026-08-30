use crate::security::PermissionSet;
use crate::value::{Rows, Value};
use rust_decimal::prelude::ToPrimitive;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

mod chart;
mod html;
mod spec;

pub(crate) use chart::chart_svg;
use chart::{chart_data, render_chart_svg, render_special_chart, ChartOptions};
use html::{render_html, resolve};
use spec::{normalize_report, Alignment, ColumnSpec, FormatSpec, TableSpec};

pub fn module_functions(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "图表" => &[
            "折线图",
            "柱状图",
            "散点图",
            "直方图",
            "箱线图",
            "相关热力图",
            "堆叠柱状图",
            "瀑布图",
        ],
        "报告" => &["创建", "导出", "生成HTML"],
        _ => return None,
    })
}

pub fn call(
    module: &str,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    crate::contracts::validate_arity(
        &format!("{module}.{name}"),
        args.len(),
        report_arity(module, name),
    )?;
    match (module, name) {
        ("报告", "创建") => {
            let title = args
                .first()
                .map(ToString::to_string)
                .unwrap_or_else(|| "衡枢分析报告".into());
            let sections = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| Value::list(Vec::new()));
            let subtitle = named.get("副标题").map(ToString::to_string);
            let theme = named
                .get("主题")
                .map(ToString::to_string)
                .unwrap_or_else(|| "分析报告".into());
            normalize_report(title, sections, subtitle, &theme)
        }
        ("报告", "导出") => {
            let Value::Record(report) = &args[0] else {
                return Err(error("报告导出需要报告对象"));
            };
            let format = args
                .get(1)
                .map(ToString::to_string)
                .unwrap_or_else(|| "HTML".into())
                .to_uppercase();
            let path = resolve(base_dir, &args[2]);
            permissions.check_file(&path)?;
            let title = report
                .borrow()
                .get("标题")
                .map(ToString::to_string)
                .unwrap_or_else(|| "衡枢分析报告".into());
            let sections = report
                .borrow()
                .get("章节")
                .cloned()
                .unwrap_or_else(|| Value::list(Vec::new()));
            let metadata = report_metadata(&sections, permissions.collection_limit())?;
            crate::privacy::guard_endpoint(
                if format == "PDF" {
                    crate::privacy::Endpoint::Pdf
                } else {
                    crate::privacy::Endpoint::LocalFile
                },
                &metadata.sensitivity,
            )?;
            let rows: Vec<BTreeMap<String, Value>> = sections
                .iter_values_limited(permissions.collection_limit())?
                .into_iter()
                .map(|section| {
                    if let Value::Record(section) = section {
                        let section = section.borrow();
                        BTreeMap::from([
                            (
                                "标题".into(),
                                section.get("标题").cloned().unwrap_or(Value::Null),
                            ),
                            (
                                "内容".into(),
                                section.get("内容").cloned().unwrap_or(Value::Null),
                            ),
                        ])
                    } else {
                        BTreeMap::from([("标题".into(), Value::Null), ("内容".into(), section)])
                    }
                })
                .collect::<Vec<_>>();
            match format.as_str() {
                "CSV" => write_report_csv(&sections, &path, permissions.collection_limit())?,
                "XLSX" => write_report_xlsx(&sections, &path, permissions.collection_limit())?,
                "SVG" => {
                    let values = rows
                        .iter()
                        .filter_map(|row| row.get("内容").and_then(|v| v.as_f64().ok()))
                        .collect::<Vec<_>>();
                    if values.is_empty() {
                        return Err(error("SVG 报告需要可转为数字的章节内容"));
                    }
                    crate::security::atomic_write_file(
                        &path,
                        chart_svg(&values, false, &title)?.as_bytes(),
                        true,
                    )
                    .map_err(error)?;
                }
                "HTML" => {
                    let sections = inline_report_charts(
                        &sections,
                        base_dir,
                        permissions,
                        permissions.collection_limit(),
                    )?;
                    let html = render_html(&title, &sections, permissions.collection_limit())?;
                    crate::security::atomic_write_file(&path, html.as_bytes(), true)
                        .map_err(error)?;
                }
                "PDF" => {
                    let document = pdf_document(
                        &title,
                        &sections,
                        permissions.collection_limit(),
                        &path,
                        base_dir,
                        permissions,
                    )?;
                    document
                        .write_artifact(
                            &path,
                            &crate::delivery::PdfBudget {
                                max_pages: permissions.collection_limit().min(100),
                                max_elements: permissions.collection_limit(),
                                max_text_bytes: permissions.buffer_limit(),
                                max_image_bytes: permissions.buffer_limit(),
                                max_output_bytes: permissions.output_limit(),
                            },
                        )
                        .map_err(|delivery_error| error(delivery_error.to_string()))?;
                }
                _ => return Err(error(format!("不支持的报告格式“{format}”"))),
            }
            Ok(Value::Path(std::rc::Rc::new(path)))
        }
        (
            "图表",
            "折线图" | "柱状图" | "散点图" | "直方图" | "箱线图" | "相关热力图" | "堆叠柱状图"
            | "瀑布图",
        ) => {
            let chart = chart_data(&args[0], &named, permissions.collection_limit(), name)?;
            let path = resolve(base_dir, &args[1]);
            permissions.check_file(&path)?;
            let title = named
                .get("标题")
                .map(ToString::to_string)
                .unwrap_or_else(|| "衡枢数据图表".into());
            let options = ChartOptions {
                title,
                x_axis_name: named.get("横轴名称").map(ToString::to_string),
                y_axis_name: named.get("纵轴名称").map(ToString::to_string),
                unit: named.get("单位").map(ToString::to_string),
                decimal_places: named
                    .get("小数位")
                    .map(|value| {
                        crate::contracts::decimal_to_usize(value.as_decimal()?, "图表小数位")
                    })
                    .transpose()?
                    .unwrap_or(2)
                    .min(8),
                show_values: named.get("显示数值").is_some_and(Value::truth),
                reference_line: named
                    .get("参考线")
                    .map(Value::as_decimal)
                    .transpose()?
                    .map(|value| value.to_f64().unwrap_or(0.0)),
                interval_lower: named
                    .get("区间下限")
                    .map(|value| {
                        value
                            .iter_values_limited(permissions.collection_limit())
                            .map(|values| values.iter().filter_map(|v| v.as_f64().ok()).collect())
                    })
                    .transpose()?,
                interval_upper: named
                    .get("区间上限")
                    .map(|value| {
                        value
                            .iter_values_limited(permissions.collection_limit())
                            .map(|values| values.iter().filter_map(|v| v.as_f64().ok()).collect())
                    })
                    .transpose()?,
                annotation: named.get("注释").map(ToString::to_string),
            };
            let svg = if matches!(
                name,
                "散点图" | "直方图" | "箱线图" | "相关热力图" | "堆叠柱状图" | "瀑布图"
            ) {
                render_special_chart(&chart, name, &options)?
            } else {
                render_chart_svg(&chart, name == "柱状图", &options)?
            };
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| error(e.to_string()))?;
            }
            crate::security::atomic_write_file(&path, svg.as_bytes(), true).map_err(error)?;
            Ok(Value::Path(std::rc::Rc::new(path)))
        }
        ("报告", "生成HTML") => {
            let title = args[0].to_string();
            let sections = args[1].iter_values_limited(permissions.collection_limit())?;
            let path = resolve(base_dir, &args[2]);
            permissions.check_file(&path)?;
            let html = render_html(
                &title,
                &inline_report_charts(
                    &Value::list(sections),
                    base_dir,
                    permissions,
                    permissions.collection_limit(),
                )?,
                permissions.collection_limit(),
            )?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| error(e.to_string()))?;
            }
            crate::security::atomic_write_file(&path, html.as_bytes(), true).map_err(error)?;
            Ok(Value::Path(std::rc::Rc::new(path)))
        }
        _ => Err(error(format!("{module}模块不支持“{name}”"))),
    }
}

fn report_arity(module: &str, name: &str) -> Option<(usize, Option<usize>)> {
    Some(match (module, name) {
        ("报告", "创建") => (0, Some(2)),
        ("报告", "导出" | "生成HTML") => (3, Some(3)),
        (
            "图表",
            "折线图" | "柱状图" | "散点图" | "直方图" | "箱线图" | "相关热力图" | "堆叠柱状图"
            | "瀑布图",
        ) => (2, Some(2)),
        _ => return None,
    })
}

fn error(message: impl AsRef<str>) -> String {
    message.as_ref().to_string()
}

/// 把章节值拆成标题、类型、内容和列规范；列顺序与格式由 `TableSpec` 决定。
fn section_parts(
    section: Value,
    index: usize,
    limit: usize,
) -> Result<(String, String, Value, TableSpec), String> {
    match section {
        Value::Record(record) => {
            let record = record.borrow();
            let spec = TableSpec::from_section(&record, limit)?;
            Ok((
                record
                    .get("标题")
                    .map(ToString::to_string)
                    .unwrap_or_else(|| format!("章节{}", index + 1)),
                record
                    .get("类型")
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "文字".into()),
                record.get("内容").cloned().unwrap_or(Value::Null),
                spec,
            ))
        }
        value => Ok((
            format!("章节{}", index + 1),
            "文字".into(),
            value,
            TableSpec::default(),
        )),
    }
}

fn flatten_value(value: &Value, path: &str, rows: &mut Vec<(String, Value)>, limit: usize) {
    if rows.len() >= limit {
        return;
    }
    match value {
        Value::Record(record) => {
            for (key, child) in record.borrow().iter() {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                flatten_value(child, &child_path, rows, limit);
            }
        }
        Value::List(values) => {
            let values = values.borrow();
            if values.iter().all(|item| matches!(item, Value::Record(_))) {
                for (index, item) in values.iter().enumerate() {
                    flatten_value(item, &format!("{path}[{}]", index + 1), rows, limit);
                }
            } else {
                rows.push((path.to_string(), value.clone()));
            }
        }
        Value::Table(table) => rows.push((
            path.to_string(),
            Value::Text(format!("表格（{}行）", table.borrow().rows.len())),
        )),
        Value::NumericArray(array) if array.data.len() == 1 => {
            rows.push((path.to_string(), Value::Float(array.data[0])))
        }
        _ => rows.push((path.to_string(), value.clone())),
    }
}

fn record_rows(record: &BTreeMap<String, Value>, limit: usize) -> Rows {
    let mut flattened = Vec::new();
    for (key, value) in record {
        flatten_value(value, key, &mut flattened, limit);
    }
    flattened
        .into_iter()
        .map(|(key, value)| {
            BTreeMap::from([("指标".into(), Value::Text(key)), ("数值".into(), value)])
        })
        .collect()
}

fn content_rows(content: &Value, limit: usize) -> Rows {
    match content {
        Value::Table(table) => table.borrow().rows.iter().take(limit).cloned().collect(),
        Value::Record(record) => record_rows(&record.borrow(), limit),
        Value::List(values) => {
            let values = values.borrow();
            if values.iter().all(|value| matches!(value, Value::Record(_))) {
                values
                    .iter()
                    .take(limit)
                    .filter_map(|value| match value {
                        Value::Record(record) => Some(record.borrow().clone()),
                        _ => None,
                    })
                    .collect()
            } else {
                values
                    .iter()
                    .take(limit)
                    .enumerate()
                    .map(|(index, value)| {
                        BTreeMap::from([
                            ("序号".into(), Value::Number((index + 1).into())),
                            ("值".into(), value.clone()),
                        ])
                    })
                    .collect()
            }
        }
        value => vec![BTreeMap::from([("值".into(), value.clone())])],
    }
}

fn write_report_xlsx(sections: &Value, path: &Path, limit: usize) -> Result<(), String> {
    use rust_xlsxwriter::{
        ConditionalFormatCell, ConditionalFormatCellRule, DocProperties, ExcelDateTime, Format,
        FormatAlign, Workbook,
    };
    let mut workbook = Workbook::new();
    // rust_xlsxwriter 默认把当前时钟写进 docProps/core.xml，同一份报告导出两次字节就不同，
    // 解释器/VM/独立包也会因此产出不同交付物。固定创建时间让 XLSX 可复现。
    let created = ExcelDateTime::from_ymd(1980, 1, 1)
        .and_then(|date| date.and_hms(0, 0, 0))
        .map_err(|reason| error(reason.to_string()))?;
    workbook.set_properties(&DocProperties::new().set_creation_datetime(&created));
    let caption_format = Format::new().set_italic().set_font_color("595959");
    let note_format = Format::new().set_font_color("595959");
    let note_title_format = Format::new().set_bold().set_font_color("595959");
    let mut used_names = BTreeSet::new();
    for (index, section) in sections.iter_values_limited(limit)?.into_iter().enumerate() {
        let (title, _, content, spec) = section_parts(section, index, limit)?;
        let base = title.chars().take(31).collect::<String>();
        let mut name = if base.is_empty() {
            format!("章节{}", index + 1)
        } else {
            base.clone()
        };
        let mut suffix = 2usize;
        while !used_names.insert(name.clone()) {
            let suffix_text = format!("_{suffix}");
            name = format!(
                "{}{}",
                base.chars()
                    .take(31 - suffix_text.chars().count())
                    .collect::<String>(),
                suffix_text
            );
            suffix += 1;
        }
        let sheet = workbook
            .add_worksheet()
            .set_name(name)
            .map_err(|e| e.to_string())?;
        let rows = content_rows(&content, limit);
        if rows.is_empty() {
            continue;
        }
        let columns = spec.resolve(&rows);
        let mut row_cursor = 0u32;
        if let Some(caption) = spec.caption.as_deref() {
            sheet
                .write_string_with_format(row_cursor, 0, caption, &caption_format)
                .map_err(|e| e.to_string())?;
            row_cursor += 1;
        }
        let header_row = row_cursor;
        for (position, column) in columns.iter().enumerate() {
            let position = position as u16;
            let alignment = match column.alignment() {
                Alignment::Left => FormatAlign::Left,
                Alignment::Center => FormatAlign::Center,
                Alignment::Right => FormatAlign::Right,
            };
            let header_format = Format::new()
                .set_bold()
                .set_background_color("D9EDE5")
                .set_align(alignment);
            let header = column.header();
            sheet
                .write_string_with_format(header_row, position, &header, &header_format)
                .map_err(|e| e.to_string())?;
            let width = column
                .width
                .map(f64::from)
                .unwrap_or_else(|| header.chars().count().clamp(10, 24) as f64);
            sheet
                .set_column_width(position, width)
                .map_err(|e| e.to_string())?;
        }
        for (offset, row) in rows.iter().enumerate() {
            let target = header_row + 1 + offset as u32;
            for (position, column) in columns.iter().enumerate() {
                write_xlsx_cell(
                    sheet,
                    target,
                    position as u16,
                    column,
                    row.get(&column.name),
                )?;
            }
        }
        let last_data_row = header_row + rows.len() as u32;
        // 有限的主题条件格式只作用于实际数据区，不覆盖表头、汇总或指标解释。
        // 规则仅提供正负值的非颜色语义补充，原生数值和列数字格式保持不变。
        for (position, column) in columns.iter().enumerate() {
            if !rows
                .iter()
                .filter_map(|row| row.get(&column.name))
                .any(|value| ColumnSpec::sample(value).is_some())
            {
                continue;
            }
            let positive = ConditionalFormatCell::new()
                .set_rule(ConditionalFormatCellRule::GreaterThanOrEqualTo(0.0))
                .set_format(
                    Format::new()
                        .set_background_color("E2F0D9")
                        .set_font_color("375623"),
                );
            let negative = ConditionalFormatCell::new()
                .set_rule(ConditionalFormatCellRule::LessThan(0.0))
                .set_format(
                    Format::new()
                        .set_background_color("FCE4D6")
                        .set_font_color("9C0006"),
                );
            sheet
                .add_conditional_format(
                    header_row + 1,
                    position as u16,
                    last_data_row,
                    position as u16,
                    &positive,
                )
                .map_err(|e| e.to_string())?;
            sheet
                .add_conditional_format(
                    header_row + 1,
                    position as u16,
                    last_data_row,
                    position as u16,
                    &negative,
                )
                .map_err(|e| e.to_string())?;
        }
        let total_label = spec.total_label();
        if let Some(label) = total_label {
            let total_row = last_data_row + 1;
            let mut labelled = false;
            for (position, column) in columns.iter().enumerate() {
                let position = position as u16;
                let total_format = Format::new()
                    .set_bold()
                    .set_background_color("F1F5F3")
                    .set_num_format_index(0);
                match column.total_value(&rows) {
                    Some(number) => {
                        let format = match column.format.xlsx_number_format() {
                            Some(pattern) => Format::new()
                                .set_bold()
                                .set_background_color("F1F5F3")
                                .set_num_format(pattern),
                            None => total_format,
                        };
                        sheet.write_number_with_format(total_row, position, number, &format)
                    }
                    None if !labelled => {
                        labelled = true;
                        sheet.write_string_with_format(total_row, position, label, &total_format)
                    }
                    None => sheet.write_blank(total_row, position, &total_format),
                }
                .map_err(|e| e.to_string())?;
            }
        }
        sheet
            .set_freeze_panes(header_row + 1, 0)
            .map_err(|e| e.to_string())?;
        sheet
            .autofilter(
                header_row,
                0,
                last_data_row,
                (columns.len() - 1).min(u16::MAX as usize) as u16,
            )
            .map_err(|e| e.to_string())?;
        let descriptions = spec.descriptions();
        if !descriptions.is_empty() {
            let mut note_row = last_data_row + if total_label.is_some() { 3 } else { 2 };
            sheet
                .write_string_with_format(note_row, 0, "指标解释", &note_title_format)
                .map_err(|e| e.to_string())?;
            for (header, description) in descriptions {
                note_row += 1;
                sheet
                    .write_string_with_format(note_row, 0, &header, &note_format)
                    .map_err(|e| e.to_string())?;
                sheet
                    .write_string_with_format(note_row, 1, &description, &note_format)
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    workbook.save(path).map_err(|e| e.to_string())
}

/// XLSX 保留原生数值和列格式；只有非数值才退化为列格式渲染的文本。
fn write_xlsx_cell(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    position: u16,
    column: &ColumnSpec,
    value: Option<&Value>,
) -> Result<(), String> {
    use rust_xlsxwriter::Format;
    let value = value.unwrap_or(&Value::Null);
    if matches!(value, Value::Null) {
        return Ok(());
    }
    let number_format = column
        .format
        .xlsx_number_format()
        .map(|pattern| Format::new().set_num_format(pattern));
    let number = match value {
        Value::Number(number) => Some(
            number
                .to_string()
                .parse::<f64>()
                .map_err(|_| "XLSX 数字超出范围".to_string())?,
        ),
        Value::Money(money) => Some(
            money
                .amount
                .to_string()
                .parse::<f64>()
                .map_err(|_| "XLSX 数字超出范围".to_string())?,
        ),
        Value::Float(number) if number.is_finite() => Some(*number),
        Value::NumericArray(array) if array.data.len() == 1 => Some(array.data[0]),
        _ => None,
    };
    let result = match (number, number_format) {
        (Some(number), Some(format)) => {
            sheet.write_number_with_format(row, position, number, &format)
        }
        (Some(number), None) => sheet.write_number(row, position, number),
        (None, _) => match value {
            Value::Bool(flag) => sheet.write_boolean(row, position, *flag),
            other => sheet.write_string(row, position, column.render(other)),
        },
    };
    result.map(|_| ()).map_err(|e| e.to_string())
}

/// CSV 的一行：章节定位信息加上该章节解析出的列规范和原始行数据。
struct CsvRecord {
    section_index: usize,
    title: String,
    kind: String,
    columns: Vec<ColumnSpec>,
    row: BTreeMap<String, Value>,
}

/// CSV 是数据交换格式：列顺序由列规范决定，单元格保留原始标量，嵌套内容显式报错。
fn write_report_csv(sections: &Value, path: &Path, limit: usize) -> Result<(), String> {
    let mut headers = vec![
        "章节序号".to_string(),
        "章节".to_string(),
        "类型".to_string(),
    ];
    let mut records: Vec<CsvRecord> = Vec::new();
    for (index, section) in sections.iter_values_limited(limit)?.into_iter().enumerate() {
        let (title, kind, content, spec) = section_parts(section, index, limit)?;
        let rows = content_rows(&content, limit);
        let columns = spec.resolve(&rows);
        for column in &columns {
            let header = column.header();
            if !headers.contains(&header) {
                headers.push(header);
            }
        }
        for row in rows {
            for column in &columns {
                let value = row.get(&column.name).unwrap_or(&Value::Null);
                if !csv_expressible(value) {
                    return Err(error(format!(
                        "报告章节“{title}”的列“{}”是嵌套结构，CSV 无法无损表达；请先转换为规范化表格或改用 HTML/XLSX/PDF",
                        column.name
                    )));
                }
            }
            records.push(CsvRecord {
                section_index: index + 1,
                title: title.clone(),
                kind: kind.clone(),
                columns: columns.clone(),
                row,
            });
            if records.len() >= limit {
                break;
            }
        }
        if records.len() >= limit {
            break;
        }
    }
    let mut writer = csv::Writer::from_path(path).map_err(|e| error(e.to_string()))?;
    writer
        .write_record(&headers)
        .map_err(|e| error(e.to_string()))?;
    for record in &records {
        let cells = headers
            .iter()
            .map(|header| match header.as_str() {
                "章节序号" => record.section_index.to_string(),
                "章节" => record.title.clone(),
                "类型" => record.kind.clone(),
                header => record
                    .columns
                    .iter()
                    .find(|column| column.header() == header)
                    .and_then(|column| record.row.get(&column.name))
                    .map(csv_cell)
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        writer
            .write_record(&cells)
            .map_err(|e| error(e.to_string()))?;
    }
    writer.flush().map_err(|e| error(e.to_string()))
}

fn csv_expressible(value: &Value) -> bool {
    match value {
        Value::Record(_) | Value::Table(_) => false,
        Value::List(values) => values.borrow().len() <= 1,
        Value::NumericArray(array) => array.data.len() <= 1,
        _ => true,
    }
}

fn csv_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::List(values) if values.borrow().len() == 1 => csv_cell(&values.borrow()[0]),
        Value::NumericArray(array) if array.data.len() == 1 => array.data[0].to_string(),
        Value::List(_) | Value::NumericArray(_) => String::new(),
        other => other.to_string(),
    }
}

fn inline_report_charts(
    sections: &Value,
    base_dir: &Path,
    permissions: &PermissionSet,
    limit: usize,
) -> Result<Value, String> {
    let values = sections.iter_values_limited(limit)?;
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let Value::Record(section) = &value else {
            output.push(value);
            continue;
        };
        let mut section = section.borrow().clone();
        if section.get("类型").map(ToString::to_string).as_deref() == Some("图表") {
            if let Some(content) = section.get("内容").cloned() {
                let source = match content {
                    Value::Path(path) => path.as_ref().clone(),
                    Value::Text(path) => std::path::PathBuf::from(path),
                    _ => {
                        output.push(Value::Record(std::rc::Rc::new(std::cell::RefCell::new(
                            section,
                        ))));
                        continue;
                    }
                };
                let path = if source.is_absolute() {
                    source
                } else {
                    base_dir.join(source)
                };
                permissions.check_file(&path)?;
                let bytes =
                    fs::read(&path).map_err(|error| format!("读取 HTML 图表失败：{error}"))?;
                validate_report_svg(&bytes, limit)?;
                let encoded =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
                section.insert(
                    "内容".into(),
                    Value::Text(format!("data:image/svg+xml;base64,{encoded}")),
                );
            }
        }
        output.push(Value::Record(std::rc::Rc::new(std::cell::RefCell::new(
            section,
        ))));
    }
    Ok(Value::list(output))
}

fn pdf_document(
    title: &str,
    sections: &Value,
    collection_limit: usize,
    output: &Path,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<crate::delivery::ReportDocument, String> {
    use crate::delivery::{ReportDocument, ReportElement};
    let mut elements = vec![ReportElement::Paragraph {
        text: title.to_owned(),
        font_size: 18,
    }];
    for (index, section) in sections
        .iter_values_limited(collection_limit)?
        .into_iter()
        .enumerate()
    {
        let is_record = matches!(section, Value::Record(_));
        let (heading, kind, content, spec) = section_parts(section, index, collection_limit)?;
        if is_record && !heading.is_empty() {
            elements.push(ReportElement::Paragraph {
                text: heading,
                font_size: 14,
            });
        }
        if let Some(caption) = spec.caption.as_deref() {
            elements.push(ReportElement::Paragraph {
                text: caption.to_owned(),
                font_size: 9,
            });
        }
        match kind.as_str() {
            "图表" => append_pdf_chart(
                &mut elements,
                &content,
                output,
                base_dir,
                permissions,
                collection_limit,
            )?,
            "指标" => append_pdf_metrics(&mut elements, &content, &spec, collection_limit)?,
            _ => append_pdf_content(&mut elements, &content, &spec, collection_limit)?,
        }
        for (header, description) in spec.descriptions() {
            elements.push(ReportElement::Paragraph {
                text: format!("{header}：{description}"),
                font_size: 9,
            });
        }
        if elements.len() > collection_limit {
            return Err("PDF 报告元素超过集合预算".into());
        }
    }
    let metadata = report_metadata(sections, collection_limit)?;
    Ok(ReportDocument {
        version: Default::default(),
        page_size: crate::delivery::PageSize::A4,
        margin: 36,
        font: crate::delivery_template::basic_docx_pdf_font(),
        header: None,
        footer: None,
        elements,
        metadata,
    })
}

fn append_pdf_metrics(
    elements: &mut Vec<crate::delivery::ReportElement>,
    content: &Value,
    spec: &TableSpec,
    collection_limit: usize,
) -> Result<(), String> {
    let values = match content {
        Value::Table(table) => table.borrow().rows.first().cloned().unwrap_or_default(),
        Value::Record(record) => record.borrow().clone(),
        _ => return append_pdf_content(elements, content, spec, collection_limit),
    };
    let rows = record_rows(&values, collection_limit)
        .into_iter()
        .map(|row| {
            let name = row.get("指标").map(ToString::to_string).unwrap_or_default();
            let column = spec.column_for(&name);
            vec![
                column.header(),
                column.render(row.get("数值").unwrap_or(&Value::Null)),
            ]
        })
        .collect();
    elements.push(crate::delivery::ReportElement::Table {
        headers: vec!["指标".into(), "数值".into()],
        rows,
    });
    Ok(())
}

fn append_pdf_chart(
    elements: &mut Vec<crate::delivery::ReportElement>,
    content: &Value,
    output: &Path,
    base_dir: &Path,
    permissions: &PermissionSet,
    collection_limit: usize,
) -> Result<(), String> {
    let source = match content {
        Value::Path(path) => path.as_ref().clone(),
        Value::Text(path) => std::path::PathBuf::from(path),
        _ => return Err("PDF 图表章节的内容必须是 SVG 路径".into()),
    };
    if source.extension().and_then(|value| value.to_str()) != Some("svg") {
        return Err("PDF 图表章节只支持 SVG 文件".into());
    }
    let output_relative = output.parent().unwrap_or(base_dir).join(&source);
    let base_relative = base_dir.join(&source);
    let path = if output_relative.is_file() {
        output_relative
    } else {
        base_relative
    };
    permissions.check_file(&path)?;
    let metadata = fs::metadata(&path).map_err(|error| format!("读取 PDF 图表失败：{error}"))?;
    if !metadata.is_file() || metadata.len() > permissions.buffer_limit() as u64 {
        return Err("PDF 图表文件无效或超过缓冲预算".into());
    }
    let bytes = fs::read(&path).map_err(|error| format!("读取 PDF 图表失败：{error}"))?;
    validate_report_svg(&bytes, collection_limit)?;
    elements.push(crate::delivery::ReportElement::Image {
        bytes,
        width: 523,
        height: 314,
        mime: "image/svg+xml".into(),
    });
    Ok(())
}

fn validate_report_svg(bytes: &[u8], collection_limit: usize) -> Result<(), String> {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut buffer = Vec::new();
    let mut elements = 0usize;
    let mut root_seen = false;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("PDF 图表 SVG 无效：{error}"))?
        {
            quick_xml::events::Event::Start(start) | quick_xml::events::Event::Empty(start) => {
                elements = elements.saturating_add(1);
                if elements > collection_limit {
                    return Err("PDF 图表 SVG 元素超过集合预算".into());
                }
                if !root_seen {
                    root_seen = true;
                    if start.local_name().as_ref() != b"svg" {
                        return Err("PDF 图表文件不是 SVG".into());
                    }
                }
                for attribute in start.attributes().with_checks(true) {
                    let attribute =
                        attribute.map_err(|error| format!("PDF 图表 SVG 属性无效：{error}"))?;
                    let name = attribute.key.local_name();
                    if matches!(name.as_ref(), b"href" | b"src") {
                        return Err("PDF 图表 SVG 不能引用外部资源".into());
                    }
                }
            }
            quick_xml::events::Event::DocType(_) | quick_xml::events::Event::PI(_) => {
                return Err("PDF 图表 SVG 不能包含主动内容".into())
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !root_seen {
        return Err("PDF 图表文件不是 SVG".into());
    }
    Ok(())
}

fn report_metadata(sections: &Value, limit: usize) -> Result<crate::execution::Metadata, String> {
    let mut metadata = crate::execution::Metadata::default();
    for section in sections.iter_values_limited(limit)? {
        let content = match section {
            Value::Record(record) => record.borrow().get("内容").cloned().unwrap_or(Value::Null),
            other => other,
        };
        if let Value::Table(table) = content {
            let table = table.borrow();
            metadata = metadata.union(&table.metadata);
            metadata.sensitivity = metadata.sensitivity.union(&table.sensitivity());
            for column in table.column_metadata.values() {
                metadata = metadata.union(column);
            }
        }
    }
    Ok(metadata)
}

fn append_pdf_content(
    elements: &mut Vec<crate::delivery::ReportElement>,
    content: &Value,
    spec: &TableSpec,
    collection_limit: usize,
) -> Result<(), String> {
    use crate::delivery::ReportElement;
    match content {
        Value::Table(table) => append_pdf_rows(elements, &table.borrow().rows, spec),
        Value::List(values)
            if values
                .borrow()
                .iter()
                .all(|value| matches!(value, Value::Record(_))) =>
        {
            let rows = values
                .borrow()
                .iter()
                .filter_map(|value| match value {
                    Value::Record(row) => Some(row.borrow().clone()),
                    _ => None,
                })
                .take(collection_limit)
                .collect::<Vec<_>>();
            append_pdf_rows(elements, &rows, spec)
        }
        Value::Record(metrics) => {
            for (label, value) in metrics.borrow().iter().take(collection_limit) {
                let column = spec.column_for(label);
                elements.push(ReportElement::Paragraph {
                    text: format!("{}: {}", column.header(), column.render(value)),
                    font_size: 11,
                });
            }
        }
        Value::Null => {}
        value => elements.push(ReportElement::Paragraph {
            text: FormatSpec::default().render(value),
            font_size: 10,
        }),
    }
    Ok(())
}

fn append_pdf_rows(
    elements: &mut Vec<crate::delivery::ReportElement>,
    rows: &[BTreeMap<String, Value>],
    spec: &TableSpec,
) {
    if rows.is_empty() {
        return;
    }
    let columns = spec.resolve(rows);
    if columns.len() > 7 {
        elements.push(crate::delivery::ReportElement::Paragraph {
            text: "宽表已按页面宽度切分，首列在各页重复".into(),
            font_size: 9,
        });
    }
    let mut values = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| column.render(row.get(&column.name).unwrap_or(&Value::Null)))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if let Some(label) = spec.total_label() {
        let mut labelled = false;
        values.push(
            columns
                .iter()
                .map(|column| match column.total_text(rows) {
                    Some(text) => text,
                    None if !labelled => {
                        labelled = true;
                        label.to_string()
                    }
                    None => String::new(),
                })
                .collect(),
        );
    }
    elements.push(crate::delivery::ReportElement::Table {
        headers: columns.iter().map(ColumnSpec::header).collect(),
        rows: values,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn unified_report_exports_csv_xlsx_html_and_svg() {
        let directory = tempfile::tempdir().unwrap();
        let report = Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("销售报告".into())),
            (
                "章节".into(),
                Value::list(vec![Value::record(BTreeMap::from([
                    ("标题".into(), Value::Text("总额".into())),
                    ("内容".into(), Value::Float(12.0)),
                ]))]),
            ),
        ]));
        for format in ["CSV", "XLSX", "HTML", "SVG", "PDF"] {
            let path = directory.path().join(format!("报告.{format}"));
            call(
                "报告",
                "导出",
                vec![
                    report.clone(),
                    Value::Text(format.into()),
                    Value::Path(std::rc::Rc::new(path.clone())),
                ],
                BTreeMap::new(),
                directory.path(),
                &PermissionSet::for_source(directory.path()),
            )
            .unwrap();
            assert!(path.exists(), "{format} 没有生成");
        }
    }

    #[test]
    fn pdf_renders_table_grid_and_embeds_chart_instead_of_its_filename() {
        let directory = tempfile::tempdir().unwrap();
        let chart_path = directory.path().join("趋势.svg");
        fs::write(
            &chart_path,
            chart_svg(&[1.0, 3.0, 2.0], false, "月度趋势").unwrap(),
        )
        .unwrap();
        let table = Value::Table(std::rc::Rc::new(std::cell::RefCell::new(
            crate::value::Table {
                rows: vec![BTreeMap::from([
                    ("月份".into(), Value::Text("2026-01".into())),
                    ("销售额".into(), Value::Number(Decimal::new(12_345_678, 5))),
                ])],
                ..Default::default()
            },
        )));
        let report = Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("经营报告".into())),
            (
                "章节".into(),
                Value::list(vec![
                    Value::record(BTreeMap::from([
                        ("标题".into(), Value::Text("明细".into())),
                        ("类型".into(), Value::Text("表格".into())),
                        ("内容".into(), table),
                        (
                            "列规范".into(),
                            Value::list(vec![
                                Value::record(BTreeMap::from([
                                    ("名称".into(), Value::Text("月份".into())),
                                    ("格式".into(), Value::Text("文本".into())),
                                ])),
                                Value::record(BTreeMap::from([
                                    ("名称".into(), Value::Text("销售额".into())),
                                    (
                                        "格式".into(),
                                        Value::record(BTreeMap::from([
                                            ("类型".into(), Value::Text("金额".into())),
                                            ("小数位".into(), Value::Number(Decimal::from(2))),
                                        ])),
                                    ),
                                    ("汇总".into(), Value::Text("求和".into())),
                                ])),
                            ]),
                        ),
                    ])),
                    Value::record(BTreeMap::from([
                        ("标题".into(), Value::Text("趋势".into())),
                        ("类型".into(), Value::Text("图表".into())),
                        ("内容".into(), Value::Text("趋势.svg".into())),
                    ])),
                ]),
            ),
        ]));
        let output = directory.path().join("报告.pdf");
        call(
            "报告",
            "导出",
            vec![
                report,
                Value::Text("PDF".into()),
                Value::Path(std::rc::Rc::new(output.clone())),
            ],
            BTreeMap::new(),
            directory.path(),
            &PermissionSet::for_source(directory.path()),
        )
        .unwrap();

        let bytes = fs::read(output).unwrap();
        let parsed = printpdf::PdfDocument::parse(
            &bytes,
            &printpdf::PdfParseOptions::default(),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(parsed
            .pages
            .iter()
            .flat_map(|page| &page.ops)
            .any(|op| { matches!(op, printpdf::Op::DrawPolygon { .. }) }));
        assert!(parsed
            .pages
            .iter()
            .flat_map(|page| &page.ops)
            .any(|op| { matches!(op, printpdf::Op::UseXobject { .. }) }));
        let text = parsed
            .extract_text()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("趋势.svg"));
        assert!(text.contains("123.46"));
        assert!(!text.contains("123.45678"));
        assert!(text.contains("合计"));
        assert_eq!(text.matches("经营报告").count(), 1);
    }

    #[test]
    fn pdf_explains_wide_table_splitting() {
        let directory = tempfile::tempdir().unwrap();
        let rows = Value::list(vec![Value::record(
            (0..9)
                .map(|index| (format!("列{index}"), Value::Float(index as f64)))
                .collect(),
        )]);
        let report = Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("宽表".into())),
            (
                "章节".into(),
                Value::list(vec![Value::record(BTreeMap::from([
                    ("标题".into(), Value::Text("明细".into())),
                    ("类型".into(), Value::Text("表格".into())),
                    ("内容".into(), rows),
                ]))]),
            ),
        ]));
        let pdf = fs::read(export(&report, "PDF", directory.path())).unwrap();
        let text = printpdf::PdfDocument::parse(
            &pdf,
            &printpdf::PdfParseOptions::default(),
            &mut Vec::new(),
        )
        .unwrap()
        .extract_text()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        assert!(text.contains("宽表已按页面宽度切分"), "{text}");
    }

    #[test]
    fn html_report_inlines_validated_svg_chart_data() {
        let directory = tempfile::tempdir().unwrap();
        let chart_path = directory.path().join("趋势.svg");
        fs::write(&chart_path, chart_svg(&[1.0, 2.0], false, "趋势").unwrap()).unwrap();
        let report = Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("经营报告".into())),
            (
                "章节".into(),
                Value::list(vec![Value::record(BTreeMap::from([
                    ("标题".into(), Value::Text("趋势".into())),
                    ("类型".into(), Value::Text("图表".into())),
                    ("内容".into(), Value::Text("趋势.svg".into())),
                ]))]),
            ),
        ]));
        let output = directory.path().join("报告.html");
        call(
            "报告",
            "导出",
            vec![
                report,
                Value::Text("HTML".into()),
                Value::Path(std::rc::Rc::new(output.clone())),
            ],
            BTreeMap::new(),
            directory.path(),
            &PermissionSet::for_source(directory.path()),
        )
        .unwrap();
        let html = fs::read_to_string(output).unwrap();
        assert!(html.contains("data:image/svg+xml;base64,"));
        assert!(!html.contains("src=\"趋势.svg\""));
    }
    #[test]
    fn table_chart_contains_categories_axes_units_and_ticks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("趋势.svg");
        let table = Value::Table(std::rc::Rc::new(std::cell::RefCell::new(
            crate::value::Table {
                rows: vec![
                    BTreeMap::from([
                        ("月份".into(), Value::Text("2026-01".into())),
                        ("销售额".into(), Value::Number(Decimal::from(85_000_000))),
                    ]),
                    BTreeMap::from([
                        ("月份".into(), Value::Text("2026-02".into())),
                        ("销售额".into(), Value::Number(Decimal::from(32_000_000))),
                    ]),
                ],
                ..Default::default()
            },
        )));
        call(
            "图表",
            "折线图",
            vec![table, Value::Text("趋势.svg".into())],
            BTreeMap::from([
                ("标题".into(), Value::Text("月度销售额".into())),
                ("横轴".into(), Value::Text("月份".into())),
                ("纵轴".into(), Value::Text("销售额".into())),
                ("横轴名称".into(), Value::Text("月份".into())),
                ("纵轴名称".into(), Value::Text("销售额".into())),
                ("单位".into(), Value::Text("万元".into())),
            ]),
            directory.path(),
            &PermissionSet::for_source(directory.path()),
        )
        .unwrap();
        let svg = fs::read_to_string(path).unwrap();
        assert!(svg.contains("2026-01"));
        assert!(svg.contains("2026-02"));
        assert!(svg.contains("销售额（万元）"));
        // Plotters 输出的颜色是大写十六进制，断言不应绑定大小写。
        assert!(
            svg.to_ascii_lowercase().contains("stroke=\"#d7ddd9\""),
            "缺少网格线"
        );
        assert!(svg.contains("role=\"img\""));
        assert!(svg.contains("<desc>"));
    }

    fn column(entries: Vec<(&str, Value)>) -> Value {
        Value::record(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// 同一份报告在四种格式里必须给出同一套列顺序、列标题、数值、空值和汇总。
    fn semantic_snapshot_report() -> Value {
        let rows = Value::list(vec![
            Value::record(BTreeMap::from([
                ("月份".into(), Value::Text("2026-01".into())),
                ("销售额".into(), Value::Number(Decimal::new(123_456_789, 3))),
                ("毛利率".into(), Value::Float(12.5)),
            ])),
            Value::record(BTreeMap::from([
                ("月份".into(), Value::Text("2026-02".into())),
                ("销售额".into(), Value::Number(Decimal::from(2_000))),
                ("毛利率".into(), Value::Null),
            ])),
        ]);
        Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("经营月报".into())),
            (
                "章节".into(),
                Value::list(vec![Value::record(BTreeMap::from([
                    ("标题".into(), Value::Text("月度明细".into())),
                    ("类型".into(), Value::Text("表格".into())),
                    ("说明".into(), Value::Text("口径：含税收入".into())),
                    ("内容".into(), rows),
                    (
                        "列规范".into(),
                        Value::list(vec![
                            column(vec![
                                ("名称", Value::Text("月份".into())),
                                ("格式", Value::Text("文本".into())),
                            ]),
                            column(vec![
                                ("名称", Value::Text("销售额".into())),
                                ("标题", Value::Text("销售金额".into())),
                                (
                                    "格式",
                                    column(vec![
                                        ("类型", Value::Text("金额".into())),
                                        ("单位", Value::Text("元".into())),
                                        ("小数位", Value::Number(Decimal::from(2))),
                                    ]),
                                ),
                                ("汇总", Value::Text("求和".into())),
                                ("说明", Value::Text("不含退货".into())),
                            ]),
                            column(vec![
                                ("名称", Value::Text("毛利率".into())),
                                (
                                    "格式",
                                    column(vec![
                                        ("类型", Value::Text("百分比".into())),
                                        ("小数位", Value::Number(Decimal::from(1))),
                                        ("空值", Value::Text("缺数据".into())),
                                    ]),
                                ),
                            ]),
                        ]),
                    ),
                ]))]),
            ),
        ]))
    }

    fn export(report: &Value, format: &str, directory: &Path) -> std::path::PathBuf {
        let path = directory.join(format!("报告.{}", format.to_lowercase()));
        call(
            "报告",
            "导出",
            vec![
                report.clone(),
                Value::Text(format.into()),
                Value::Path(std::rc::Rc::new(path.clone())),
            ],
            BTreeMap::new(),
            directory,
            &PermissionSet::for_source(directory),
        )
        .unwrap();
        path
    }

    #[test]
    fn one_report_keeps_the_same_column_contract_in_html_pdf_xlsx_and_csv() {
        let directory = tempfile::tempdir().unwrap();
        let report = semantic_snapshot_report();

        let html = fs::read_to_string(export(&report, "HTML", directory.path())).unwrap();
        let month = html.find("月份</th>").unwrap();
        let sales = html.find("销售金额（元）</th>").unwrap();
        let margin = html.find("毛利率</th>").unwrap();
        assert!(month < sales && sales < margin, "HTML 列顺序必须来自列规范");
        assert!(html.contains("123,456.79"), "{html}");
        assert!(html.contains("12.5%"));
        assert!(html.contains("缺数据"));
        assert!(html.contains("125,456.79"), "HTML 缺少求和汇总");
        assert!(html.contains("不含退货"));
        assert!(html.contains("口径：含税收入"));

        let pdf = fs::read(export(&report, "PDF", directory.path())).unwrap();
        let text = printpdf::PdfDocument::parse(
            &pdf,
            &printpdf::PdfParseOptions::default(),
            &mut Vec::new(),
        )
        .unwrap()
        .extract_text()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        let month = text.find("月份").unwrap();
        let sales = text.find("销售金额（元）").unwrap();
        let margin = text.find("毛利率").unwrap();
        assert!(month < sales && sales < margin, "PDF 列顺序必须来自列规范");
        assert!(text.contains("123,456.79"), "{text}");
        assert!(text.contains("12.5%"));
        assert!(text.contains("缺数据"));
        assert!(text.contains("125,456.79"), "PDF 缺少求和汇总");
        assert!(text.contains("不含退货"));
        assert!(text.contains("口径：含税收入"));

        let xlsx_path = export(&report, "XLSX", directory.path());
        let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(&xlsx_path).unwrap();
        let sheet = calamine::Reader::worksheet_range(&mut workbook, "月度明细").unwrap();
        let cells = sheet
            .rows()
            .map(|row| row.iter().map(ToString::to_string).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(cells[0][0], "口径：含税收入", "{cells:?}");
        assert_eq!(cells[1], vec!["月份", "销售金额（元）", "毛利率"]);
        assert_eq!(cells[2][0], "2026-01");
        // XLSX 保留原生数值，展示格式由列格式承担。
        assert_eq!(cells[2][1].parse::<f64>().unwrap(), 123_456.789);
        assert_eq!(cells[2][2].parse::<f64>().unwrap(), 12.5);
        assert!(cells[3][2].is_empty(), "空值在 XLSX 里保持空单元格");
        let totals = cells.iter().find(|row| row[0] == "合计").unwrap();
        assert_eq!(totals[1].parse::<f64>().unwrap(), 125_456.789);
        assert!(cells.iter().any(|row| row[0] == "指标解释"));
        assert!(cells
            .iter()
            .any(|row| row.len() > 1 && row[1] == "不含退货"));
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&xlsx_path).unwrap()).unwrap();
        let mut sheet_xml = String::new();
        use std::io::Read;
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet_xml)
            .unwrap();
        assert!(sheet_xml.contains("<conditionalFormatting sqref=\"B3:B4\""));
        assert!(sheet_xml.contains("operator=\"greaterThanOrEqual\""));
        assert!(sheet_xml.contains("operator=\"lessThan\""));
        assert!(
            !sheet_xml.contains("sqref=\"B2:B4\""),
            "条件格式不得覆盖表头"
        );

        let csv = fs::read_to_string(export(&report, "CSV", directory.path())).unwrap();
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "章节序号,章节,类型,月份,销售金额（元）,毛利率"
        );
        // CSV 是数据交换格式：保留原始精度，不写展示文本。
        assert_eq!(
            lines.next().unwrap(),
            "1,月度明细,表格,2026-01,123456.789,12.5"
        );
        assert_eq!(lines.next().unwrap(), "1,月度明细,表格,2026-02,2000,");
    }

    #[test]
    fn csv_export_rejects_nested_cells_instead_of_writing_display_text() {
        let directory = tempfile::tempdir().unwrap();
        let report = Value::record(BTreeMap::from([
            ("标题".into(), Value::Text("检验结果".into())),
            (
                "章节".into(),
                Value::list(vec![Value::record(BTreeMap::from([
                    ("标题".into(), Value::Text("配对检验".into())),
                    ("类型".into(), Value::Text("表格".into())),
                    (
                        "内容".into(),
                        Value::list(vec![
                            Value::record(BTreeMap::from([
                                ("方法".into(), Value::Text("t 检验".into())),
                                (
                                    "区间".into(),
                                    Value::list(vec![Value::Float(0.1), Value::Float(0.9)]),
                                ),
                            ])),
                            Value::record(BTreeMap::from([
                                ("方法".into(), Value::Text("秩和检验".into())),
                                (
                                    "区间".into(),
                                    Value::list(vec![Value::Float(0.2), Value::Float(0.8)]),
                                ),
                            ])),
                        ]),
                    ),
                ]))]),
            ),
        ]));
        let path = directory.path().join("报告.csv");
        let message = call(
            "报告",
            "导出",
            vec![
                report,
                Value::Text("CSV".into()),
                Value::Path(std::rc::Rc::new(path.clone())),
            ],
            BTreeMap::new(),
            directory.path(),
            &PermissionSet::for_source(directory.path()),
        )
        .unwrap_err();
        assert!(message.contains("CSV 无法无损表达"), "{message}");
        assert!(message.contains("区间"), "{message}");
        assert!(!path.exists(), "失败时不应留下半个 CSV");
    }
}
