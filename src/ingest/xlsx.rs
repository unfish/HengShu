//! Bounded XLSX archive inspection and worksheet ingestion.

use super::*;
use calamine::{open_workbook_auto, Reader};
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use std::fs::File;
use std::io::Read;
use zip::ZipArchive;

pub(super) fn ingest_xlsx(path: &Path, options: &IngestOptions) -> Result<WorkbookResult, String> {
    check_xlsx_zip_budget(path, options.max_bytes)?;
    let (hidden_sheets, merged_ranges, hidden_columns, hidden_rows, date_system_1904) =
        xlsx_structure(path, options)?;
    let formula_issues = xlsx_formula_cache_issues(path)?;
    if options.mode == IngestMode::Strict && !formula_issues.is_empty() {
        return Err(format!(
            "严格模式拒绝无缓存公式：{}",
            formula_issues.join(", ")
        ));
    }
    let mut workbook =
        open_workbook_auto(path).map_err(|error| format!("打开工作簿失败：{error}"))?;
    let names = workbook.sheet_names().to_vec();
    if names.len() > options.max_sheets {
        return Err("工作表数量超过预算".into());
    }
    let selected = names
        .iter()
        .enumerate()
        .filter(|(index, name)| {
            (options.sheet_names.is_empty() && options.sheet_indices.is_empty())
                || options.sheet_names.contains(name)
                || options.sheet_indices.contains(index)
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("没有匹配的工作表".into());
    }
    let mut result = WorkbookResult {
        path: path.to_path_buf(),
        sheets: Vec::new(),
        issues: Vec::new(),
    };
    result
        .issues
        .extend(formula_issues.into_iter().map(|sheet| CellIssue {
            sheet,
            row: 0,
            column: 0,
            level: IssueLevel::Warning,
            message: "工作表包含没有缓存结果的公式，已保留为问题诊断".into(),
            raw: String::new(),
        }));
    for (index, name) in selected {
        let range = workbook
            .worksheet_range(name)
            .map_err(|error| format!("读取工作表失败：{error}"))?;
        let (origin_row, origin_column) = range.start().unwrap_or((0, 0));
        let mut values = range
            .rows()
            .map(|row| row.iter().map(cell_text).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        propagate_merged_cells(
            &mut values,
            merged_ranges.get(index).map(Vec::as_slice).unwrap_or(&[]),
            origin_row as usize,
            origin_column as usize,
        );
        let hidden_sheet_rows = hidden_rows.get(index).cloned().unwrap_or_default();
        let mut source_rows = (0..values.len())
            .map(|row| origin_row as usize + row + 1)
            .collect::<Vec<_>>();
        let mut visible_values = Vec::with_capacity(values.len());
        let mut visible_rows = Vec::with_capacity(values.len());
        for (row, coordinate) in values.into_iter().zip(source_rows.drain(..)) {
            if !hidden_sheet_rows.contains(&coordinate) {
                visible_values.push(row);
                visible_rows.push(coordinate);
            }
        }
        let values = visible_values;
        if values.len() > options.max_rows.saturating_add(options.header_rows) {
            return Err(format!("工作表“{name}”行数超过预算"));
        }
        let width = values.iter().map(|row| row.len()).max().unwrap_or(0);
        if width > options.max_columns {
            return Err(format!("工作表“{name}”列数超过预算"));
        }
        let hidden_sheet_columns = hidden_columns.get(index).cloned().unwrap_or_default();
        let source_columns = (0..width)
            .map(|column| origin_column as usize + column + 1)
            .filter(|column| !hidden_sheet_columns.contains(column))
            .collect::<Vec<_>>();
        let visible_offsets = source_columns
            .iter()
            .map(|column| column - origin_column as usize - 1)
            .collect::<Vec<_>>();
        let header_rows = options.header_rows.min(values.len());
        let mut headers = Vec::new();
        for &column in &visible_offsets {
            let parts = (0..header_rows)
                .filter_map(|row| values.get(row).and_then(|cells| cells.get(column)))
                .filter(|text| !text.is_empty())
                .cloned()
                .collect::<Vec<_>>();
            headers.push(if parts.is_empty() {
                format!("列{}", column + 1)
            } else {
                parts.join("_")
            });
        }
        headers = unique_columns(headers, name, &mut result.issues);
        let mut rows = Vec::new();
        for (row_offset, cells) in values.iter().skip(header_rows).enumerate() {
            let record = StringRecord::from(
                visible_offsets
                    .iter()
                    .map(|column| cells.get(*column).cloned().unwrap_or_default())
                    .collect::<Vec<_>>(),
            );
            rows.push(parse_record(
                name,
                visible_rows[header_rows + row_offset],
                &headers,
                &record,
                options,
                &mut result.issues,
            )?);
        }
        result.sheets.push(SheetResult {
            name: name.clone(),
            index,
            hidden: hidden_sheets.get(index).copied().unwrap_or(false),
            columns: headers,
            rows,
            merged_ranges: merged_ranges.get(index).cloned().unwrap_or_default(),
            hidden_columns: hidden_sheet_columns,
            hidden_rows: hidden_sheet_rows,
            source_rows: visible_rows.into_iter().skip(header_rows).collect(),
            source_columns,
            date_system_1904,
        });
    }
    Ok(result)
}

fn propagate_merged_cells(
    values: &mut [Vec<String>],
    ranges: &[String],
    origin_row: usize,
    origin_column: usize,
) {
    for range in ranges {
        let Some((start, end)) = range.split_once(':') else {
            continue;
        };
        let Some((start_row, start_col)) = cell_coordinate(start) else {
            continue;
        };
        let Some((end_row, end_col)) = cell_coordinate(end) else {
            continue;
        };
        let Some(start_row) = start_row.checked_sub(origin_row) else {
            continue;
        };
        let Some(start_col) = start_col.checked_sub(origin_column) else {
            continue;
        };
        let Some(end_row) = end_row.checked_sub(origin_row) else {
            continue;
        };
        let Some(end_col) = end_col.checked_sub(origin_column) else {
            continue;
        };
        let source = values
            .get(start_row)
            .and_then(|row| row.get(start_col))
            .cloned()
            .unwrap_or_default();
        for row in start_row..=end_row {
            for column in start_col..=end_col {
                if let Some(cell) = values.get_mut(row).and_then(|cells| cells.get_mut(column)) {
                    if cell.is_empty() {
                        *cell = source.clone();
                    }
                }
            }
        }
    }
}

fn cell_coordinate(text: &str) -> Option<(usize, usize)> {
    let split = text
        .char_indices()
        .find(|(_, character)| character.is_ascii_digit())
        .map(|(index, _)| index)?;
    let (letters, digits) = text.split_at(split);
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let mut column = 0usize;
    for character in letters.chars() {
        if !character.is_ascii_alphabetic() {
            return None;
        }
        column = column * 26 + (character.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    let row = digits.parse::<usize>().ok()?.checked_sub(1)?;
    Some((row, column.checked_sub(1)?))
}

fn check_xlsx_zip_budget(path: &Path, max_bytes: u64) -> Result<(), String> {
    let file = File::open(path).map_err(|error| format!("读取 XLSX 压缩包失败：{error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("XLSX 压缩包无效：{error}"))?;
    crate::ooxml::validate_archive(
        &mut archive,
        crate::ooxml::OoxmlBudget {
            max_entries: 10_000,
            max_entry_bytes: max_bytes,
            max_total_bytes: max_bytes.saturating_mul(8),
        },
    )
}

fn xlsx_formula_cache_issues(path: &Path) -> Result<Vec<String>, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut issues = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().to_string();
        if !name.starts_with("xl/worksheets/") || !name.ends_with(".xml") {
            continue;
        }
        let xml = read_zip_entry_bounded(&mut entry)?;
        let mut offset = 0usize;
        let mut missing = false;
        while let Some(relative) = xml[offset..].find("<f") {
            let start = offset + relative;
            let Some(formula_end) = xml[start..].find("</f>").map(|value| start + value + 4) else {
                missing = true;
                break;
            };
            let cell_end = xml[formula_end..]
                .find("</c>")
                .map(|value| formula_end + value)
                .unwrap_or(xml.len());
            let cached = xml[formula_end..cell_end].find("<v>").and_then(|value| {
                let value_start = formula_end + value + 3;
                xml[value_start..cell_end]
                    .find("</v>")
                    .map(|end| xml[value_start..value_start + end].trim())
            });
            if cached.is_none_or(str::is_empty) {
                missing = true;
                break;
            }
            offset = formula_end;
        }
        if missing {
            issues.push(name);
        }
    }
    Ok(issues)
}

pub(super) fn xlsx_structure(
    path: &Path,
    options: &IngestOptions,
) -> Result<XlsxStructure, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut hidden = Vec::new();
    let mut sheet_ids = Vec::new();
    let mut date_system_1904 = false;
    if let Ok(mut workbook) = archive.by_name("xl/workbook.xml") {
        let xml = read_zip_entry_bounded(&mut workbook)?;
        (hidden, sheet_ids, date_system_1904) = parse_workbook_structure(&xml)?;
    }
    let relationships = if let Ok(mut rels) = archive.by_name("xl/_rels/workbook.xml.rels") {
        let xml = read_zip_entry_bounded(&mut rels)?;
        parse_workbook_relationships(&xml)?
    } else {
        BTreeMap::new()
    };
    let sheet_parts = sheet_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            relationships
                .get(id)
                .and_then(|target| normalize_workbook_target(target))
                .unwrap_or_else(|| format!("xl/worksheets/sheet{}.xml", index + 1))
        })
        .collect::<Vec<_>>();
    let mut merged = Vec::new();
    let mut hidden_columns = Vec::new();
    let mut hidden_rows = Vec::new();
    let fallback = ["xl/worksheets/sheet1.xml".to_string()];
    for name in if sheet_parts.is_empty() {
        fallback.as_slice()
    } else {
        sheet_parts.as_slice()
    } {
        let mut ranges = Vec::new();
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        if let Ok(mut sheet) = archive.by_name(name) {
            let xml = read_zip_entry_bounded(&mut sheet)?;
            validate_sheet_dimensions(&xml, name, options)?;
            let mut offset = 0usize;
            while let Some(relative) = xml[offset..].find("<col") {
                let start = offset + relative;
                let end = xml[start..]
                    .find('>')
                    .map(|value| start + value)
                    .unwrap_or(xml.len());
                let tag = &xml[start..=end.min(xml.len().saturating_sub(1))];
                if tag.contains("hidden=\"1\"") || tag.contains("hidden=\"true\"") {
                    let min = tag
                        .split("min=\"")
                        .nth(1)
                        .and_then(|part| part.split('"').next())
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(1);
                    let max = tag
                        .split("max=\"")
                        .nth(1)
                        .and_then(|part| part.split('"').next())
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(min);
                    columns.extend(min..=max);
                }
                offset = end.saturating_add(1);
            }
            let mut offset = 0usize;
            while let Some(relative) = xml[offset..].find("<row") {
                let start = offset + relative;
                let end = xml[start..]
                    .find('>')
                    .map(|value| start + value)
                    .unwrap_or(xml.len());
                let tag = &xml[start..=end.min(xml.len().saturating_sub(1))];
                if tag.contains("hidden=\"1\"") || tag.contains("hidden=\"true\"") {
                    if let Some(row) = tag
                        .split("r=\"")
                        .nth(1)
                        .and_then(|part| part.split('"').next())
                        .and_then(|value| value.parse::<usize>().ok())
                    {
                        rows.push(row);
                    }
                }
                offset = end.saturating_add(1);
            }
            let mut offset = 0usize;
            while let Some(relative) = xml[offset..].find("<mergeCell") {
                let start = offset + relative;
                let end = xml[start..]
                    .find('>')
                    .map(|value| start + value)
                    .unwrap_or(xml.len());
                let tag = &xml[start..=end.min(xml.len().saturating_sub(1))];
                if let Some(value) = tag
                    .split("ref=\"")
                    .nth(1)
                    .and_then(|part| part.split('"').next())
                {
                    ranges.push(value.to_string());
                }
                offset = end.saturating_add(1);
            }
        }
        merged.push(ranges);
        columns.sort_unstable();
        columns.dedup();
        hidden_columns.push(columns);
        rows.sort_unstable();
        rows.dedup();
        hidden_rows.push(rows);
    }
    Ok((
        hidden,
        merged,
        hidden_columns,
        hidden_rows,
        date_system_1904,
    ))
}

pub(super) fn validate_sheet_dimensions(
    xml: &str,
    name: &str,
    options: &IngestOptions,
) -> Result<(), String> {
    let mut reader = XmlReader::from_str(xml);
    let mut buffer = Vec::new();
    let max_rows = options.max_rows.saturating_add(options.header_rows);
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("解析工作表维度失败：{error}"))?
        {
            Event::Start(start) | Event::Empty(start) if start.name().as_ref() == b"row" => {
                for attribute in start.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    if attribute.key.as_ref() == b"r" {
                        let row = String::from_utf8_lossy(&attribute.value)
                            .parse::<usize>()
                            .map_err(|_| "XLSX 行坐标无效")?;
                        if row > max_rows {
                            return Err(format!("工作表“{name}”行数超过预算"));
                        }
                    }
                }
            }
            Event::Start(start) | Event::Empty(start) if start.name().as_ref() == b"c" => {
                for attribute in start.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    if attribute.key.as_ref() == b"r" {
                        let reference = String::from_utf8_lossy(&attribute.value);
                        let (_, column) =
                            cell_coordinate(&reference).ok_or("XLSX 单元格坐标无效")?;
                        if column.saturating_add(1) > options.max_columns {
                            return Err(format!("工作表“{name}”列数超过预算"));
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(())
}

fn parse_workbook_structure(xml: &str) -> Result<(Vec<bool>, Vec<String>, bool), String> {
    let mut hidden = Vec::new();
    let mut sheet_ids = Vec::new();
    let mut date_system_1904 = false;
    let mut reader = XmlReader::from_str(xml);
    let mut buffer = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("解析 workbook.xml 失败：{error}"))?
        {
            Event::Start(start) | Event::Empty(start) if start.name().as_ref() == b"workbookPr" => {
                for attribute in start.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    if attribute.key.as_ref() == b"date1904" {
                        date_system_1904 = matches!(attribute.value.as_ref(), b"1" | b"true");
                    }
                }
            }
            Event::Start(start) | Event::Empty(start) if start.name().as_ref() == b"sheet" => {
                let mut id = None;
                let mut state = None;
                for attribute in start.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    match attribute.key.as_ref() {
                        b"r:id" => {
                            id = Some(String::from_utf8_lossy(&attribute.value).into_owned())
                        }
                        b"state" => {
                            state = Some(String::from_utf8_lossy(&attribute.value).into_owned())
                        }
                        _ => {}
                    }
                }
                hidden.push(matches!(state.as_deref(), Some("hidden" | "veryHidden")));
                sheet_ids.push(id.unwrap_or_default());
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((hidden, sheet_ids, date_system_1904))
}

fn parse_workbook_relationships(xml: &str) -> Result<BTreeMap<String, String>, String> {
    let mut relationships = BTreeMap::new();
    let mut reader = XmlReader::from_str(xml);
    let mut buffer = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("解析 workbook 关系失败：{error}"))?
        {
            Event::Start(start) | Event::Empty(start)
                if start.name().as_ref() == b"Relationship" =>
            {
                let mut id = None;
                let mut target = None;
                for attribute in start.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    match attribute.key.as_ref() {
                        b"Id" => id = Some(String::from_utf8_lossy(&attribute.value).into_owned()),
                        b"Target" => {
                            target = Some(String::from_utf8_lossy(&attribute.value).into_owned())
                        }
                        _ => {}
                    }
                }
                if let (Some(id), Some(target)) = (id, target) {
                    relationships.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(relationships)
}

fn normalize_workbook_target(target: &str) -> Option<String> {
    let normalized = target.replace('\\', "/");
    if normalized.split('/').any(|part| part == "..") {
        return None;
    }
    if normalized.starts_with('/') {
        Some(normalized.trim_start_matches('/').to_string())
    } else if normalized.starts_with("xl/") {
        Some(normalized)
    } else {
        Some(format!("xl/{normalized}"))
    }
}

const MAX_XLSX_XML_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

fn read_zip_entry_bounded<R: Read>(entry: &mut R) -> Result<String, String> {
    let mut bytes = Vec::new();
    entry
        .take(MAX_XLSX_XML_ENTRY_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_XLSX_XML_ENTRY_BYTES {
        return Err("XLSX XML 部件超过字节预算".into());
    }
    String::from_utf8(bytes).map_err(|_| "XLSX XML 部件不是 UTF-8".into())
}
