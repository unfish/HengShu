//! XLSX target resolution, row expansion, formula translation, and cell replacement.

use super::*;

pub(super) fn replace_xlsx(
    input: &Path,
    output: &Path,
    plans: &[ReplacementPlan],
    preflight_report: TemplatePreflight,
    mut metadata: Metadata,
) -> Result<ReplacementReport, String> {
    let mut cells = BTreeMap::new();
    let mut sample_rows = BTreeMap::<(String, usize), usize>::new();
    let date_system_1904 = xlsx_date_system_1904(input)?;
    for plan in plans {
        let (sheet_part, cell_ref) = match &plan.target {
            TemplateTarget::XlsxCell {
                sheet_part,
                cell_ref,
            } => (sheet_part.clone(), cell_ref.clone()),
            TemplateTarget::XlsxNamedRange { name } => resolve_named_range(input, name)?
                .ok_or_else(|| format!("XLSX 命名区域不存在或不是单元格引用：{name}"))?,
            TemplateTarget::XlsxTableAnchor {
                table_name,
                column_name,
                row_offset,
            } => resolve_table_anchor(input, table_name, column_name, *row_offset)?.ok_or_else(
                || format!("XLSX 表格锚点不存在：{table_name}.{column_name}[{row_offset}]"),
            )?,
            TemplateTarget::XlsxSampleRow {
                sheet_part,
                sample_row,
                row_offset,
                column,
            } => {
                if *sample_row == 0 || *column == 0 {
                    return Err("XLSX 样板行和列必须从 1 开始".into());
                }
                let copies = row_offset.checked_add(1).ok_or("XLSX 样板行偏移过大")?;
                sample_rows
                    .entry((sheet_part.clone(), *sample_row))
                    .and_modify(|current| *current = (*current).max(copies))
                    .or_insert(copies);
                (
                    sheet_part.clone(),
                    cell_reference(*column, sample_row.saturating_add(*row_offset)),
                )
            }
            _ => return Err("XLSX 模板计划包含非 XLSX 目标".into()),
        };
        if !sheet_part.starts_with("xl/worksheets/") || !sheet_part.ends_with(".xml") {
            return Err(format!("XLSX 工作表部件路径无效：{sheet_part}"));
        }
        let key = (sheet_part.clone(), cell_ref.clone());
        if cells.insert(key, plan.value.clone()).is_some() {
            return Err(format!("XLSX 替换目标重复：{sheet_part}!{cell_ref}"));
        }
        plan.value.validate()?;
        if let Replacement::Quantity { unit, .. } = &plan.value {
            let label = format!("{sheet_part}!{cell_ref}");
            if metadata.units.insert(label.clone(), unit.clone()).is_some() {
                return Err(format!("XLSX 替换单位元数据重复：{label}"));
            }
        }
    }
    let table_expansions = xlsx_sample_table_expansions(input, &sample_rows)?;
    let replaced = rewrite_package(input, output, |name, bytes| {
        let applicable = cells
            .iter()
            .filter(|((part, _), _)| part == name)
            .collect::<Vec<_>>();
        if applicable.is_empty()
            && !sample_rows.keys().any(|(part, _)| part == name)
            && !table_expansions.contains_key(name)
        {
            if name == "xl/workbook.xml" && !sample_rows.is_empty() {
                return force_xlsx_recalculation_xml(bytes);
            }
            return Ok(bytes.to_vec());
        }
        let mut result = bytes.to_vec();
        if let Some(expansions) = table_expansions.get(name) {
            for (sample_row, copies) in expansions {
                result = expand_xlsx_table_xml(&result, *sample_row, *copies)?;
            }
        }
        for ((part, sample_row), copies) in &sample_rows {
            if part == name {
                result = repeat_xlsx_sample_row_xml(&result, *sample_row, *copies)?;
            }
        }
        for ((_, cell_ref), value) in applicable {
            result = replace_xlsx_cell_xml(&result, cell_ref, value, date_system_1904)?;
        }
        Ok(result)
    })?;
    let artifact = Artifact::observe_file(
        output,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    )
    .map_err(|error| error.to_string())
    .map(|mut artifact| {
        artifact.metadata = metadata.clone();
        artifact.purpose = "模板文件交付".into();
        artifact
    })?;
    Ok(ReplacementReport {
        preflight: preflight_report,
        replaced,
        output: output.to_path_buf(),
        artifact,
        diagnostics: Vec::new(),
    })
}

fn resolve_named_range(input: &Path, wanted: &str) -> Result<Option<(String, String)>, String> {
    let file = File::open(input).map_err(|error| format!("读取 XLSX 命名区域失败：{error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let workbook = read_zip_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_zip_entry(&mut archive, "xl/_rels/workbook.xml.rels")?;
    let mut sheet_ids = BTreeMap::new();
    let mut reader = Reader::from_str(&workbook);
    let mut buffer = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| error.to_string())?
        {
            Event::Empty(start) | Event::Start(start) if start.name().as_ref() == b"sheet" => {
                let mut name = None;
                let mut id = None;
                for attribute in start.attributes().flatten() {
                    match attribute.key.as_ref() {
                        b"name" => {
                            name = Some(String::from_utf8_lossy(&attribute.value).into_owned())
                        }
                        b"r:id" => {
                            id = Some(String::from_utf8_lossy(&attribute.value).into_owned())
                        }
                        _ => {}
                    }
                }
                if let (Some(name), Some(id)) = (name, id) {
                    sheet_ids.insert(name, id);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    let mut relationships = BTreeMap::new();
    let mut reader = Reader::from_str(&rels);
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| error.to_string())?
        {
            Event::Empty(start) | Event::Start(start)
                if start.name().as_ref() == b"Relationship" =>
            {
                let mut id = None;
                let mut target = None;
                for attribute in start.attributes().flatten() {
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
    let mut reader = Reader::from_str(&workbook);
    let mut current_name = None;
    let mut current_formula = String::new();
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| error.to_string())?
        {
            Event::Start(start) if start.name().as_ref() == b"definedName" => {
                current_name = start.attributes().flatten().find_map(|attribute| {
                    (attribute.key.as_ref() == b"name")
                        .then(|| String::from_utf8_lossy(&attribute.value).into_owned())
                });
                current_formula.clear();
            }
            Event::Text(text) if current_name.is_some() => {
                current_formula.push_str(&String::from_utf8_lossy(&text));
            }
            Event::End(end) if end.name().as_ref() == b"definedName" => {
                if current_name.as_deref() == Some(wanted) {
                    if let Some((sheet, cell)) = current_formula.rsplit_once('!') {
                        let sheet = sheet.trim_matches('\'').to_string();
                        let cell = cell.replace('$', "");
                        if let Some(id) = sheet_ids.get(&sheet) {
                            if let Some(target) = relationships.get(id) {
                                let part = if target.starts_with('/') {
                                    target.trim_start_matches('/').to_string()
                                } else {
                                    format!("xl/{target}")
                                };
                                return Ok(Some((part, cell)));
                            }
                        }
                    }
                }
                current_name = None;
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(None)
}

pub(super) fn read_zip_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String, String> {
    let mut entry = archive
        .by_name(name)
        .map_err(|error| format!("XLSX 缺少 {name}：{error}"))?;
    crate::ooxml::read_string_limited(&mut entry, name, 64 * 1024 * 1024)
}

fn resolve_table_anchor(
    input: &Path,
    table_name: &str,
    column_name: &str,
    row_offset: usize,
) -> Result<Option<(String, String)>, String> {
    let file = File::open(input).map_err(|error| format!("读取 XLSX 表格失败：{error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    validate_template_archive(&mut archive)?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().to_string();
        if name.starts_with("xl/worksheets/") && name.ends_with(".xml") {
            let xml = crate::ooxml::read_string_limited(&mut entry, &name, 64 * 1024 * 1024)?;
            names.push((name, xml));
        }
    }
    for (sheet_part, sheet_xml) in names {
        let rels_name = sheet_part
            .strip_prefix("xl/worksheets/")
            .map(|name| format!("xl/worksheets/_rels/{name}.rels"));
        let Some(rels_name) = rels_name else { continue };
        let Ok(rels) = read_zip_entry(&mut archive, &rels_name) else {
            continue;
        };
        let table_ids = xml_attributes(&sheet_xml, "tablePart", b"r:id");
        let relationships = relationship_targets(&rels);
        for id in table_ids {
            let Some(target) = relationships.get(&id) else {
                continue;
            };
            let table_part = if target.starts_with('/') {
                target.trim_start_matches('/').to_string()
            } else if target.starts_with("../") {
                format!("xl/{}", target.trim_start_matches("../"))
            } else {
                format!("xl/tables/{target}")
            };
            let Ok(table_xml) = read_zip_entry(&mut archive, &table_part) else {
                continue;
            };
            let table = parse_table_definition(&table_xml)?;
            if table.name != table_name {
                continue;
            }
            let Some(column_index) = table.columns.iter().position(|name| name == column_name)
            else {
                return Ok(None);
            };
            let (start_col, start_row, end_col, end_row) = parse_cell_range(&table.reference)?;
            let row = start_row.saturating_add(1).saturating_add(row_offset);
            let col = start_col.saturating_add(column_index);
            if row > end_row || col > end_col {
                return Ok(None);
            }
            return Ok(Some((sheet_part, cell_reference(col, row))));
        }
    }
    Ok(None)
}

#[derive(Default)]
struct TableDefinition {
    name: String,
    reference: String,
    columns: Vec<String>,
}

fn parse_table_definition(xml: &str) -> Result<TableDefinition, String> {
    let mut result = TableDefinition::default();
    let mut reader = Reader::from_str(xml);
    let mut buffer = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| error.to_string())?
        {
            Event::Empty(start) | Event::Start(start) if start.name().as_ref() == b"table" => {
                for attribute in start.attributes().flatten() {
                    match attribute.key.as_ref() {
                        b"name" => {
                            result.name = String::from_utf8_lossy(&attribute.value).into_owned()
                        }
                        b"ref" => {
                            result.reference =
                                String::from_utf8_lossy(&attribute.value).into_owned()
                        }
                        _ => {}
                    }
                }
            }
            Event::Empty(start) | Event::Start(start)
                if start.name().as_ref() == b"tableColumn" =>
            {
                if let Some(name) = start.attributes().flatten().find_map(|attribute| {
                    (attribute.key.as_ref() == b"name")
                        .then(|| String::from_utf8_lossy(&attribute.value).into_owned())
                }) {
                    result.columns.push(name);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(result)
}

fn xlsx_sample_table_expansions(
    input: &Path,
    sample_rows: &BTreeMap<(String, usize), usize>,
) -> Result<BTreeMap<String, Vec<(usize, usize)>>, String> {
    let file = File::open(input).map_err(|error| format!("读取 XLSX 表格关系失败：{error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut expansions = BTreeMap::<String, Vec<(usize, usize)>>::new();
    for ((sheet_part, sample_row), copies) in sample_rows {
        let sheet_xml = read_zip_entry(&mut archive, sheet_part)?;
        let Some(sheet_name) = sheet_part.strip_prefix("xl/worksheets/") else {
            continue;
        };
        let rels_name = format!("xl/worksheets/_rels/{sheet_name}.rels");
        let Ok(rels) = read_zip_entry(&mut archive, &rels_name) else {
            continue;
        };
        let relationships = relationship_targets(&rels);
        for id in xml_attributes(&sheet_xml, "tablePart", b"r:id") {
            let Some(target) = relationships.get(&id) else {
                continue;
            };
            let table_part = if target.starts_with('/') {
                target.trim_start_matches('/').to_string()
            } else if target.starts_with("../") {
                format!("xl/{}", target.trim_start_matches("../"))
            } else {
                format!("xl/tables/{target}")
            };
            let table_xml = read_zip_entry(&mut archive, &table_part)?;
            let table = parse_table_definition(&table_xml)?;
            let (_, start_row, _, end_row) = parse_cell_range(&table.reference)?;
            if *sample_row >= start_row && *sample_row <= end_row {
                expansions
                    .entry(table_part)
                    .or_default()
                    .push((*sample_row, *copies));
            }
        }
    }
    Ok(expansions)
}

pub(super) fn expand_xlsx_table_xml(
    bytes: &[u8],
    sample_row: usize,
    copies: usize,
) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| error.to_string())?
        {
            Event::Start(start) if matches!(start.name().as_ref(), b"table" | b"autoFilter") => {
                writer
                    .write_event(Event::Start(expand_range_attribute(
                        start, sample_row, copies,
                    )?))
                    .map_err(|error| error.to_string())?;
            }
            Event::Empty(start) if matches!(start.name().as_ref(), b"table" | b"autoFilter") => {
                writer
                    .write_event(Event::Empty(expand_range_attribute(
                        start, sample_row, copies,
                    )?))
                    .map_err(|error| error.to_string())?;
            }
            Event::Eof => break,
            event => writer
                .write_event(event.into_owned())
                .map_err(|error| error.to_string())?,
        }
        buffer.clear();
    }
    Ok(writer.into_inner())
}

fn expand_range_attribute(
    start: BytesStart<'_>,
    sample_row: usize,
    copies: usize,
) -> Result<BytesStart<'static>, String> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut output = BytesStart::new(name);
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if attribute.key.as_ref() == b"ref" {
            let reference = String::from_utf8_lossy(&attribute.value);
            let (start_col, start_row, end_col, mut end_row) = parse_cell_range(&reference)?;
            if sample_row >= start_row && sample_row <= end_row {
                end_row = end_row.saturating_add(copies.saturating_sub(1));
            }
            let value = format!(
                "{}:{}",
                cell_reference(start_col, start_row),
                cell_reference(end_col, end_row)
            );
            output.push_attribute((attribute.key.as_ref(), value.as_bytes()));
        } else {
            output.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    Ok(output.into_owned())
}

fn xml_attributes(xml: &str, element: &str, attribute_name: &[u8]) -> Vec<String> {
    let mut values = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buffer = Vec::new();
    while let Ok(event) = reader.read_event_into(&mut buffer) {
        match event {
            Event::Empty(start) | Event::Start(start)
                if start.name().as_ref() == element.as_bytes() =>
            {
                if let Some(value) = start.attributes().flatten().find_map(|attribute| {
                    (attribute.key.as_ref() == attribute_name)
                        .then(|| String::from_utf8_lossy(&attribute.value).into_owned())
                }) {
                    values.push(value);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    values
}

fn relationship_targets(xml: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut reader = Reader::from_str(xml);
    let mut buffer = Vec::new();
    while let Ok(event) = reader.read_event_into(&mut buffer) {
        match event {
            Event::Empty(start) | Event::Start(start)
                if start.name().as_ref() == b"Relationship" =>
            {
                let mut id = None;
                let mut target = None;
                for attribute in start.attributes().flatten() {
                    match attribute.key.as_ref() {
                        b"Id" => id = Some(String::from_utf8_lossy(&attribute.value).into_owned()),
                        b"Target" => {
                            target = Some(String::from_utf8_lossy(&attribute.value).into_owned())
                        }
                        _ => {}
                    }
                }
                if let (Some(id), Some(target)) = (id, target) {
                    values.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    values
}

fn parse_cell_range(range: &str) -> Result<(usize, usize, usize, usize), String> {
    let (left, right) = range.split_once(':').ok_or("XLSX 表格引用必须是矩形范围")?;
    let (start_col, start_row) = parse_cell_reference(left)?;
    let (end_col, end_row) = parse_cell_reference(right)?;
    if start_col > end_col || start_row > end_row {
        return Err("XLSX 表格引用范围无效".into());
    }
    Ok((start_col, start_row, end_col, end_row))
}

fn parse_cell_reference(value: &str) -> Result<(usize, usize), String> {
    let value = value.trim().trim_matches('$');
    let split = value
        .find(|character: char| character.is_ascii_digit())
        .ok_or("XLSX 单元格引用无效")?;
    let (letters, digits) = value.split_at(split);
    let mut column = 0usize;
    for character in letters.chars() {
        if !character.is_ascii_alphabetic() {
            return Err("XLSX 单元格列引用无效".into());
        }
        column = column
            .checked_mul(26)
            .and_then(|value| {
                value.checked_add(character.to_ascii_uppercase() as usize - 'A' as usize + 1)
            })
            .ok_or("XLSX 单元格列引用过大")?;
    }
    let row = digits
        .parse::<usize>()
        .map_err(|_| "XLSX 单元格行引用无效")?;
    if column == 0 || row == 0 {
        return Err("XLSX 单元格引用必须从 1 开始".into());
    }
    Ok((column, row))
}

pub(super) fn cell_reference(column: usize, row: usize) -> String {
    let mut column = column;
    let mut letters = String::new();
    while column > 0 {
        let remainder = (column - 1) % 26;
        letters.push((b'A' + remainder as u8) as char);
        column = (column - 1) / 26;
    }
    format!("{}{}", letters.chars().rev().collect::<String>(), row)
}

fn xlsx_date_system_1904(input: &Path) -> Result<bool, String> {
    let file = File::open(input).map_err(|error| format!("读取 XLSX 日期系统失败：{error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let Ok(workbook) = read_zip_entry(&mut archive, "xl/workbook.xml") else {
        return Ok(false);
    };
    Ok(workbook.contains("date1904=\"1\"") || workbook.contains("date1904=\"true\""))
}

fn excel_date_serial(value: &str, date_system_1904: bool) -> Result<String, String> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "日期替换必须是 YYYY-MM-DD".to_string())?;
    let epoch = NaiveDate::from_ymd_opt(
        if date_system_1904 { 1904 } else { 1899 },
        if date_system_1904 { 1 } else { 12 },
        if date_system_1904 { 1 } else { 31 },
    )
    .ok_or("Excel 日期纪元无效")?;
    if date < epoch {
        return Err("日期早于 Excel 日期系统支持范围".into());
    }
    let mut serial = (date - epoch).num_days();
    if !date_system_1904 && date >= NaiveDate::from_ymd_opt(1900, 3, 1).unwrap() {
        serial += 1;
    }
    Ok(serial.to_string())
}

fn repeat_xlsx_sample_row_xml(
    bytes: &[u8],
    sample_row: usize,
    copies: usize,
) -> Result<Vec<u8>, String> {
    if copies == 0 {
        return Err("XLSX 样板行复制数量必须大于零".into());
    }
    let extra = copies - 1;
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut found = false;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("解析 XLSX 样板行失败：{error}"))?
        {
            Event::Start(start) if start.name().as_ref() == b"row" => {
                let row_number =
                    xml_start_usize_attribute(&start, b"r").ok_or("XLSX 样板行缺少行号")?;
                let mut row_writer = Writer::new(Vec::new());
                row_writer
                    .write_event(Event::Start(start.into_owned()))
                    .map_err(|error| error.to_string())?;
                let mut depth = 1usize;
                while depth > 0 {
                    let event = reader
                        .read_event_into(&mut buffer)
                        .map_err(|error| format!("解析 XLSX 样板行失败：{error}"))?;
                    match &event {
                        Event::Start(_) => depth += 1,
                        Event::End(_) => depth -= 1,
                        Event::Eof => return Err("XLSX 样板行 XML 不完整".into()),
                        _ => {}
                    }
                    row_writer
                        .write_event(event.into_owned())
                        .map_err(|error| error.to_string())?;
                    buffer.clear();
                }
                let row = row_writer.into_inner();
                if row_number == sample_row {
                    found = true;
                    for offset in 0..copies {
                        writer.get_mut().extend_from_slice(&rewrite_xlsx_row(
                            &row,
                            sample_row + offset,
                            offset as isize,
                        )?);
                    }
                } else {
                    let shift = if row_number > sample_row { extra } else { 0 };
                    writer.get_mut().extend_from_slice(&rewrite_xlsx_row(
                        &row,
                        row_number + shift,
                        shift as isize,
                    )?);
                }
            }
            Event::Eof => break,
            event => writer
                .write_event(event.into_owned())
                .map_err(|error| error.to_string())?,
        }
        buffer.clear();
    }
    if !found {
        return Err(format!("XLSX 样板行不存在：{sample_row}"));
    }
    Ok(writer.into_inner())
}

fn xml_start_usize_attribute(start: &BytesStart<'_>, key: &[u8]) -> Option<usize> {
    start.attributes().flatten().find_map(|attribute| {
        (attribute.key.as_ref() == key)
            .then(|| String::from_utf8_lossy(&attribute.value).parse().ok())
            .flatten()
    })
}

pub(super) fn rewrite_xlsx_row(
    bytes: &[u8],
    row_number: usize,
    formula_delta: isize,
) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut in_formula = false;
    let mut formula_cell = false;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| error.to_string())?
        {
            Event::Start(start)
                if start.name().as_ref() == b"row" || start.name().as_ref() == b"c" =>
            {
                let is_row = start.name().as_ref() == b"row";
                writer
                    .write_event(Event::Start(rewrite_xlsx_reference_start(
                        start, row_number, is_row,
                    )?))
                    .map_err(|error| error.to_string())?;
            }
            Event::Empty(start) if start.name().as_ref() == b"c" => {
                writer
                    .write_event(Event::Empty(rewrite_xlsx_reference_start(
                        start, row_number, false,
                    )?))
                    .map_err(|error| error.to_string())?;
            }
            Event::Start(start) if start.name().as_ref() == b"f" => {
                in_formula = true;
                formula_cell = true;
                writer
                    .write_event(Event::Start(start.into_owned()))
                    .map_err(|error| error.to_string())?;
            }
            Event::End(end) if end.name().as_ref() == b"f" => {
                in_formula = false;
                writer
                    .write_event(Event::End(end.into_owned()))
                    .map_err(|error| error.to_string())?;
            }
            Event::Start(start)
                if start.name().as_ref() == b"v" && formula_cell && formula_delta != 0 =>
            {
                let mut depth = 1usize;
                while depth > 0 {
                    match reader
                        .read_event_into(&mut buffer)
                        .map_err(|error| error.to_string())?
                    {
                        Event::Start(_) => depth += 1,
                        Event::End(_) => depth -= 1,
                        Event::Eof => return Err("XLSX 公式缓存 XML 不完整".into()),
                        _ => {}
                    }
                    buffer.clear();
                }
            }
            Event::End(end) if end.name().as_ref() == b"c" => {
                formula_cell = false;
                writer
                    .write_event(Event::End(end.into_owned()))
                    .map_err(|error| error.to_string())?;
            }
            Event::Text(text) if in_formula && formula_delta != 0 => {
                let formula = text.decode().map_err(|error| error.to_string())?;
                writer
                    .write_event(Event::Text(BytesText::new(&translate_formula_rows(
                        &formula,
                        formula_delta,
                    )?)))
                    .map_err(|error| error.to_string())?;
            }
            Event::Eof => break,
            event => writer
                .write_event(event.into_owned())
                .map_err(|error| error.to_string())?,
        }
        buffer.clear();
    }
    Ok(writer.into_inner())
}

pub(super) fn force_xlsx_recalculation_xml(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut found = false;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| error.to_string())?
        {
            Event::Start(start) if start.name().as_ref() == b"calcPr" => {
                found = true;
                writer
                    .write_event(Event::Empty(recalculation_start(&start)))
                    .map_err(|error| error.to_string())?;
                let mut depth = 1usize;
                while depth > 0 {
                    match reader
                        .read_event_into(&mut buffer)
                        .map_err(|error| error.to_string())?
                    {
                        Event::Start(_) => depth += 1,
                        Event::End(_) => depth -= 1,
                        Event::Eof => return Err("XLSX calcPr XML 不完整".into()),
                        _ => {}
                    }
                    buffer.clear();
                }
            }
            Event::Empty(start) if start.name().as_ref() == b"calcPr" => {
                found = true;
                writer
                    .write_event(Event::Empty(recalculation_start(&start)))
                    .map_err(|error| error.to_string())?;
            }
            Event::Empty(start) if start.name().as_ref() == b"workbook" => {
                writer
                    .write_event(Event::Start(start.into_owned()))
                    .map_err(|error| error.to_string())?;
                if !found {
                    let mut calc = BytesStart::new("calcPr");
                    calc.push_attribute(("calcMode", "auto"));
                    calc.push_attribute(("fullCalcOnLoad", "1"));
                    calc.push_attribute(("forceFullCalc", "1"));
                    writer
                        .write_event(Event::Empty(calc))
                        .map_err(|error| error.to_string())?;
                }
                writer
                    .write_event(Event::End(BytesEnd::new("workbook")))
                    .map_err(|error| error.to_string())?;
            }
            Event::End(end) if end.name().as_ref() == b"workbook" => {
                if !found {
                    let mut calc = BytesStart::new("calcPr");
                    calc.push_attribute(("calcMode", "auto"));
                    calc.push_attribute(("fullCalcOnLoad", "1"));
                    calc.push_attribute(("forceFullCalc", "1"));
                    writer
                        .write_event(Event::Empty(calc))
                        .map_err(|error| error.to_string())?;
                }
                writer
                    .write_event(Event::End(end.into_owned()))
                    .map_err(|error| error.to_string())?;
            }
            Event::Eof => break,
            event => writer
                .write_event(event.into_owned())
                .map_err(|error| error.to_string())?,
        }
        buffer.clear();
    }
    Ok(writer.into_inner())
}

fn recalculation_start(start: &BytesStart<'_>) -> BytesStart<'static> {
    let mut output = BytesStart::new("calcPr");
    for attribute in start.attributes().with_checks(false).flatten() {
        if !matches!(
            attribute.key.as_ref(),
            b"calcMode" | b"fullCalcOnLoad" | b"forceFullCalc"
        ) {
            output.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    output.push_attribute(("calcMode", "auto"));
    output.push_attribute(("fullCalcOnLoad", "1"));
    output.push_attribute(("forceFullCalc", "1"));
    output.into_owned()
}

fn rewrite_xlsx_reference_start(
    start: BytesStart<'_>,
    row_number: usize,
    is_row: bool,
) -> Result<BytesStart<'static>, String> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut output = BytesStart::new(name);
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if attribute.key.as_ref() == b"r" {
            let value = if is_row {
                row_number.to_string()
            } else {
                let reference = String::from_utf8_lossy(&attribute.value);
                let (column, _) = parse_cell_reference(&reference)?;
                cell_reference(column, row_number)
            };
            output.push_attribute((attribute.key.as_ref(), value.as_bytes()));
        } else {
            output.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    Ok(output.into_owned())
}

pub(super) fn translate_formula_rows(formula: &str, delta: isize) -> Result<String, String> {
    static CELL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let cell =
        CELL.get_or_init(|| Regex::new(r"(?P<column>\$?[A-Za-z]{1,3})(?P<row>\$?\d+)").unwrap());
    let mut output = String::new();
    for (index, segment) in formula.split('"').enumerate() {
        if index > 0 {
            output.push('"');
        }
        if index % 2 == 1 {
            output.push_str(segment);
            continue;
        }
        let mut cursor = 0usize;
        for captures in cell.captures_iter(segment) {
            let matched = captures.get(0).expect("formula cell regex has full match");
            let before = segment[..matched.start()].chars().next_back();
            let after = segment[matched.end()..].chars().next();
            if before.is_some_and(|value| value.is_ascii_alphanumeric() || value == '_')
                || after.is_some_and(|value| {
                    value.is_ascii_alphanumeric() || matches!(value, '_' | '(')
                })
            {
                continue;
            }
            let normalized = matched.as_str().replace('$', "");
            let Ok((column_number, _)) = parse_cell_reference(&normalized) else {
                continue;
            };
            if column_number > 16_384 {
                continue;
            }
            output.push_str(&segment[cursor..matched.start()]);
            let row = &captures["row"];
            if row.starts_with('$') {
                output.push_str(matched.as_str());
            } else {
                let parsed = row.parse::<isize>().map_err(|_| "XLSX 公式行号无效")?;
                let shifted = parsed.checked_add(delta).ok_or("XLSX 公式行号溢出")?;
                if !(1..=1_048_576).contains(&shifted) {
                    return Err("XLSX 公式平移超出工作表行范围".into());
                }
                output.push_str(&format!("{}{shifted}", &captures["column"]));
            }
            cursor = matched.end();
        }
        output.push_str(&segment[cursor..]);
    }
    Ok(output)
}

fn replace_xlsx_cell_xml(
    bytes: &[u8],
    cell_ref: &str,
    value: &Replacement,
    date_system_1904: bool,
) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut in_target = false;
    let mut found = false;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("解析工作表 XML 失败：{error}"))?
        {
            Event::Start(start)
                if start.name().as_ref() == b"c"
                    && start.attributes().flatten().any(|attribute| {
                        attribute.key.as_ref() == b"r"
                            && attribute.value.as_ref() == cell_ref.as_bytes()
                    }) =>
            {
                found = true;
                in_target = true;
                let mut replacement_start = BytesStart::new("c");
                for attribute in start.attributes().flatten() {
                    if attribute.key.as_ref() != b"t" {
                        replacement_start
                            .push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
                    }
                }
                let value_type = match value {
                    Replacement::Number(_)
                    | Replacement::Date(_)
                    | Replacement::Quantity { .. } => Some(b"n".as_slice()),
                    Replacement::Boolean(_) => Some(b"b".as_slice()),
                    Replacement::Text(_) => Some(b"inlineStr".as_slice()),
                };
                if let Some(value_type) = value_type {
                    replacement_start.push_attribute((b"t".as_slice(), value_type));
                }
                writer
                    .write_event(Event::Start(replacement_start))
                    .map_err(|error| error.to_string())?;
            }
            Event::End(end) if in_target && end.name().as_ref() == b"c" => {
                let text = match value {
                    Replacement::Date(value) => excel_date_serial(value, date_system_1904)?,
                    Replacement::Quantity { value, .. } => value.clone(),
                    value => value.as_text(),
                };
                if matches!(value, Replacement::Text(_)) {
                    writer
                        .write_event(Event::Start(BytesStart::new("is")))
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_event(Event::Start(BytesStart::new("t")))
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_event(Event::Text(BytesText::new(&text)))
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_event(Event::End(BytesEnd::new("t")))
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_event(Event::End(BytesEnd::new("is")))
                        .map_err(|error| error.to_string())?;
                } else {
                    writer
                        .write_event(Event::Start(BytesStart::new("v")))
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_event(Event::Text(BytesText::new(&text)))
                        .map_err(|error| error.to_string())?;
                    writer
                        .write_event(Event::End(BytesEnd::new("v")))
                        .map_err(|error| error.to_string())?;
                }
                writer
                    .write_event(Event::End(end.into_owned()))
                    .map_err(|error| error.to_string())?;
                in_target = false;
            }
            Event::Start(_) if in_target => {}
            Event::End(_) if in_target => {}
            Event::Text(_) if in_target => {}
            Event::Eof => break,
            event if !in_target => writer
                .write_event(event.into_owned())
                .map_err(|error| error.to_string())?,
            _ => {}
        }
        buffer.clear();
    }
    if !found {
        return Err(format!("XLSX 单元格不存在：{cell_ref}"));
    }
    Ok(writer.into_inner())
}
