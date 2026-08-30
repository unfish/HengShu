//! Bounded ingestion of messy CSV/XLSX workbooks.

use calamine::Data;
use csv::StringRecord;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod xlsx;
use xlsx::ingest_xlsx;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum IngestMode {
    Strict,
    Lenient,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IngestOptions {
    pub mode: IngestMode,
    pub sheet_names: Vec<String>,
    pub sheet_indices: Vec<usize>,
    pub header_rows: usize,
    pub max_sheets: usize,
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_bytes: u64,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            mode: IngestMode::Lenient,
            sheet_names: Vec::new(),
            sheet_indices: Vec::new(),
            header_rows: 1,
            max_sheets: 64,
            max_rows: 1_000_000,
            max_columns: 16_384,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum IssueLevel {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellIssue {
    pub sheet: String,
    pub row: usize,
    pub column: usize,
    pub level: IssueLevel,
    pub message: String,
    pub raw: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SheetResult {
    pub name: String,
    pub index: usize,
    pub hidden: bool,
    pub columns: Vec<String>,
    pub rows: Vec<BTreeMap<String, String>>,
    pub merged_ranges: Vec<String>,
    #[serde(default)]
    pub hidden_columns: Vec<usize>,
    #[serde(default)]
    pub hidden_rows: Vec<usize>,
    #[serde(default)]
    pub source_rows: Vec<usize>,
    #[serde(default)]
    pub source_columns: Vec<usize>,
    #[serde(default)]
    pub date_system_1904: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkbookResult {
    pub path: PathBuf,
    pub sheets: Vec<SheetResult>,
    pub issues: Vec<CellIssue>,
}

type XlsxStructure = (
    Vec<bool>,
    Vec<Vec<String>>,
    Vec<Vec<usize>>,
    Vec<Vec<usize>>,
    bool,
);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MappingMode {
    Exact,
    NormalizedExact,
    Manual,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MappingOptions {
    pub mode: MappingMode,
    pub source_field: String,
    pub master_field: String,
    pub manual: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MappingIssueKind {
    Unmatched,
    DuplicateMaster,
    DuplicateSource,
    OneToMany,
    InvalidManual,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MappingIssue {
    pub kind: MappingIssueKind,
    pub source_index: Option<usize>,
    pub source_value: String,
    pub candidates: Vec<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MappingResult {
    pub source_to_master: Vec<Option<usize>>,
    pub issues: Vec<MappingIssue>,
}

pub fn map_master_data(
    source: &[BTreeMap<String, String>],
    master: &[BTreeMap<String, String>],
    options: &MappingOptions,
) -> Result<MappingResult, String> {
    if options.source_field.is_empty() || options.master_field.is_empty() {
        return Err("主数据映射字段不能为空".into());
    }
    let mut issues = Vec::new();
    let mut index: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (master_index, record) in master.iter().enumerate() {
        let Some(value) = record.get(&options.master_field) else {
            issues.push(MappingIssue {
                kind: MappingIssueKind::DuplicateMaster,
                source_index: None,
                source_value: String::new(),
                candidates: Vec::new(),
                message: format!(
                    "主数据第 {} 行缺少字段“{}”",
                    master_index + 1,
                    options.master_field
                ),
            });
            continue;
        };
        let key = match options.mode {
            MappingMode::Exact | MappingMode::Manual => value.clone(),
            MappingMode::NormalizedExact => normalize_key(value),
        };
        index.entry(key).or_default().push(master_index);
    }
    for (key, candidates) in &index {
        if candidates.len() > 1 {
            issues.push(MappingIssue {
                kind: MappingIssueKind::DuplicateMaster,
                source_index: None,
                source_value: key.clone(),
                candidates: candidates.iter().map(|index| index.to_string()).collect(),
                message: "主数据键重复，不能自动选择".into(),
            });
        }
    }
    let mut source_to_master = Vec::with_capacity(source.len());
    let mut claimed = BTreeMap::<usize, usize>::new();
    for (source_index, record) in source.iter().enumerate() {
        let value = record
            .get(&options.source_field)
            .cloned()
            .unwrap_or_default();
        let key = match options.mode {
            MappingMode::Exact | MappingMode::Manual => value.clone(),
            MappingMode::NormalizedExact => normalize_key(&value),
        };
        if options.mode == MappingMode::Manual {
            if let Some(mapped) = options.manual.get(&value) {
                if let Ok(master_index) = mapped.parse::<usize>() {
                    if master_index < master.len() {
                        source_to_master.push(Some(master_index));
                        *claimed.entry(master_index).or_default() += 1;
                        continue;
                    }
                }
            }
            issues.push(MappingIssue {
                kind: MappingIssueKind::InvalidManual,
                source_index: Some(source_index),
                source_value: value,
                candidates: Vec::new(),
                message: "人工映射目标无效".into(),
            });
            source_to_master.push(None);
            continue;
        }
        let candidates = index.get(&key).cloned().unwrap_or_default();
        if candidates.len() == 1 {
            source_to_master.push(Some(candidates[0]));
            *claimed.entry(candidates[0]).or_default() += 1;
        } else {
            let kind = if candidates.is_empty() {
                MappingIssueKind::Unmatched
            } else {
                MappingIssueKind::DuplicateSource
            };
            issues.push(MappingIssue {
                kind,
                source_index: Some(source_index),
                source_value: value,
                candidates: candidates.iter().map(|index| index.to_string()).collect(),
                message: if candidates.is_empty() {
                    "主数据没有匹配项".into()
                } else {
                    "主数据有多个候选项，未自动选择".into()
                },
            });
            source_to_master.push(None);
        }
    }
    for (master_index, count) in claimed {
        if count > 1 {
            issues.push(MappingIssue {
                kind: MappingIssueKind::OneToMany,
                source_index: None,
                source_value: master_index.to_string(),
                candidates: vec![count.to_string()],
                message: "多个源记录映射到同一主数据记录".into(),
            });
        }
    }
    Ok(MappingResult {
        source_to_master,
        issues,
    })
}

fn normalize_key(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| {
            !character.is_whitespace() && !matches!(character, ',' | '，' | '-' | '－' | '_' | '＿')
        })
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn ingest(path: impl AsRef<Path>, options: &IngestOptions) -> Result<WorkbookResult, String> {
    let path = path.as_ref();
    let size = std::fs::metadata(path)
        .map_err(|error| format!("读取表格失败：{error}"))?
        .len();
    if size > options.max_bytes {
        return Err("表格文件超过字节预算".into());
    }
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
    {
        return ingest_csv(path, options);
    }
    ingest_xlsx(path, options)
}

fn ingest_csv(path: &Path, options: &IngestOptions) -> Result<WorkbookResult, String> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(path)
        .map_err(|error| error.to_string())?;
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let mut issues = Vec::new();
    let columns = unique_columns(
        headers.iter().map(str::to_owned).collect(),
        "CSV",
        &mut issues,
    );
    let mut rows = Vec::new();
    for (row_index, record) in reader.records().enumerate() {
        if row_index >= options.max_rows {
            return Err("CSV 行数超过预算".into());
        }
        let record = record.map_err(|error| error.to_string())?;
        rows.push(parse_record(
            "CSV",
            row_index + 2,
            &columns,
            &record,
            options,
            &mut issues,
        )?);
    }
    let source_rows = (2..=rows.len() + 1).collect();
    let source_columns = (1..=columns.len()).collect();
    Ok(WorkbookResult {
        path: path.to_path_buf(),
        sheets: vec![SheetResult {
            name: "CSV".into(),
            index: 0,
            hidden: false,
            columns,
            rows,
            merged_ranges: Vec::new(),
            hidden_columns: Vec::new(),
            hidden_rows: Vec::new(),
            source_rows,
            source_columns,
            date_system_1904: false,
        }],
        issues,
    })
}

fn parse_record(
    sheet: &str,
    row: usize,
    columns: &[String],
    values: &StringRecord,
    options: &IngestOptions,
    issues: &mut Vec<CellIssue>,
) -> Result<BTreeMap<String, String>, String> {
    if values.len() > columns.len() {
        issues.push(CellIssue {
            sheet: sheet.into(),
            row,
            column: columns.len() + 1,
            level: IssueLevel::Warning,
            message: "数据列超过表头，已截断".into(),
            raw: String::new(),
        });
        if options.mode == IngestMode::Strict {
            return Err(format!("严格模式拒绝多余列：{sheet}!{row}"));
        }
    }
    let mut record = BTreeMap::new();
    for (column, name) in columns.iter().enumerate() {
        let raw = values.get(column).unwrap_or_default().trim().to_owned();
        let normalized = normalize_cell(&raw);
        if raw != normalized && !raw.is_empty() {
            issues.push(CellIssue {
                sheet: sheet.into(),
                row,
                column: column + 1,
                level: IssueLevel::Warning,
                message: "已标准化单元格文本".into(),
                raw: raw.clone(),
            });
        }
        if options.mode == IngestMode::Strict && raw != normalized && !raw.is_empty() {
            return Err(format!("严格模式拒绝修复：{sheet}!{}", column + 1));
        }
        if ambiguous_date(&normalized) {
            issues.push(CellIssue {
                sheet: sheet.into(),
                row,
                column: column + 1,
                level: IssueLevel::Warning,
                message: "日期存在日/月歧义，保留原文不自动解释".into(),
                raw: normalized.clone(),
            });
            if options.mode == IngestMode::Strict {
                return Err(format!("严格模式拒绝歧义日期：{sheet}!{}", column + 1));
            }
        }
        record.insert(name.clone(), normalized);
    }
    Ok(record)
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::DateTimeIso(value) | Data::DurationIso(value) | Data::String(value) => value.clone(),
        Data::Int(value) => value.to_string(),
        Data::Float(value) => value.to_string(),
        Data::Bool(value) => value.to_string(),
        Data::Error(value) => format!("错误:{value:?}"),
        Data::DateTime(value) => {
            let (year, month, day, hour, minute, second, milli) = value.to_ymd_hms_milli();
            if hour == 0 && minute == 0 && second == 0 && milli == 0 {
                format!("{year:04}-{month:02}-{day:02}")
            } else {
                format!(
                    "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}"
                )
            }
        }
        Data::Empty => String::new(),
    }
}

/// 形如 3/4/2026 的日期在日先与月先两种读法下含义不同，属于必须人工确认的歧义。
fn ambiguous_date(text: &str) -> bool {
    let parts = text
        .split(['/', '-'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|part| part.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    let year_length = parts[2].len();
    if year_length != 2 && year_length != 4 {
        return false;
    }
    let first = parts[0].parse::<u32>().unwrap_or(0);
    let second = parts[1].parse::<u32>().unwrap_or(0);
    (1..=12).contains(&first) && (1..=12).contains(&second) && first != second
}

fn normalize_cell(raw: &str) -> String {
    let text = raw.trim();
    if let Some(date) = normalize_explicit_date(text) {
        return date;
    }
    if let Some(number) = normalize_number(text) {
        return number;
    }
    text.to_string()
}

fn normalize_explicit_date(text: &str) -> Option<String> {
    let normalized = if let Some(chinese) = text.strip_suffix('日') {
        chinese.replace(['年', '月'], "-")
    } else if text
        .split(['/', '-'])
        .next()
        .is_some_and(|year| year.len() == 4)
    {
        text.replace('/', "-")
    } else {
        return None;
    };
    let date = chrono::NaiveDate::parse_from_str(&normalized, "%Y-%m-%d").ok()?;
    Some(date.format("%Y-%m-%d").to_string())
}

fn normalize_number(text: &str) -> Option<String> {
    let compact = text.replace([' ', '\u{3000}'], "");
    let (negative, compact) = compact
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .map_or((false, compact.as_str()), |value| (true, value));
    let compact = compact
        .strip_prefix("人民币")
        .or_else(|| compact.strip_prefix("CNY"))
        .unwrap_or(compact)
        .trim_start_matches(['¥', '￥', '$']);
    let (number, multiplier, percent) = [
        ("亿元", Decimal::from(100_000_000), false),
        ("万元", Decimal::from(10_000), false),
        ("千元", Decimal::from(1_000), false),
        ("元", Decimal::ONE, false),
        ("亿", Decimal::from(100_000_000), false),
        ("万", Decimal::from(10_000), false),
        ("%", Decimal::ONE, true),
        ("％", Decimal::ONE, true),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier, percent)| {
        compact
            .strip_suffix(suffix)
            .map(|number| (number, multiplier, percent))
    })
    .unwrap_or((compact, Decimal::ONE, false));
    let number = strip_valid_grouping(number)?;
    let mut value = Decimal::from_str_exact(&number).ok()?;
    if negative {
        value = -value;
    }
    value *= multiplier;
    if percent {
        value /= Decimal::from(100);
    }
    Some(value.normalize().to_string())
}

fn strip_valid_grouping(number: &str) -> Option<String> {
    let normalized = number.replace('，', ",");
    let unsigned = normalized.strip_prefix(['+', '-']).unwrap_or(&normalized);
    let integer = unsigned.split('.').next().unwrap_or_default();
    if !integer.contains(',') {
        return Some(normalized);
    }
    let groups = integer.split(',').collect::<Vec<_>>();
    if groups.is_empty()
        || groups[0].is_empty()
        || groups[0].len() > 3
        || groups[1..]
            .iter()
            .any(|group| group.len() != 3 || !group.chars().all(|c| c.is_ascii_digit()))
        || !groups[0].chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(normalized.replace(',', ""))
}

fn unique_columns(
    mut columns: Vec<String>,
    sheet: &str,
    issues: &mut Vec<CellIssue>,
) -> Vec<String> {
    let mut seen = BTreeMap::<String, usize>::new();
    for (index, column) in columns.iter_mut().enumerate() {
        let base = if column.is_empty() {
            format!("列{}", index + 1)
        } else {
            column.clone()
        };
        let count = seen.entry(base.clone()).or_default();
        *count += 1;
        if column.is_empty() {
            *column = base.clone();
        }
        if *count > 1 {
            *column = format!("{}_{}", base, count);
            issues.push(CellIssue {
                sheet: sheet.into(),
                row: 1,
                column: index + 1,
                level: IssueLevel::Warning,
                message: "重名列已编号".into(),
                raw: base,
            });
        }
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn csv_ingestion_is_stable_and_strict_mode_rejects_repairs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dirty.csv");
        std::fs::write(&path, "金额,金额\n 1,234 ,50%\n").unwrap();
        let result = ingest(&path, &IngestOptions::default()).unwrap();
        assert_eq!(result.sheets[0].columns, vec!["金额", "金额_2"]);
        assert!(!result.issues.is_empty());
        let strict = IngestOptions {
            mode: IngestMode::Strict,
            ..Default::default()
        };
        assert!(ingest(&path, &strict).is_err());
    }

    #[test]
    fn master_mapping_supports_exact_normalized_and_manual_modes_without_fuzzy_fallback() {
        let source = vec![
            BTreeMap::from([("名称".into(), " 上海-银行 ".into())]),
            BTreeMap::from([("名称".into(), "不存在".into())]),
            BTreeMap::from([("名称".into(), "上海银行".into())]),
        ];
        let master = vec![
            BTreeMap::from([("名称".into(), "上海银行".into())]),
            BTreeMap::from([("名称".into(), "上海银行".into())]),
        ];
        let options = MappingOptions {
            mode: MappingMode::NormalizedExact,
            source_field: "名称".into(),
            master_field: "名称".into(),
            manual: BTreeMap::new(),
        };
        let result = map_master_data(&source, &master, &options).unwrap();
        assert_eq!(result.source_to_master, vec![None, None, None]);
        assert!(result
            .issues
            .iter()
            .any(|issue| issue.kind == MappingIssueKind::DuplicateMaster));
        assert!(result
            .issues
            .iter()
            .any(|issue| issue.kind == MappingIssueKind::Unmatched));
        let manual = MappingOptions {
            mode: MappingMode::Manual,
            source_field: "名称".into(),
            master_field: "名称".into(),
            manual: BTreeMap::from([("不存在".into(), "0".into())]),
        };
        let manual_result = map_master_data(&source, &master, &manual).unwrap();
        assert_eq!(manual_result.source_to_master[1], Some(0));
    }

    #[test]
    fn csv_and_xlsx_ingestion_share_a_stable_basic_schema() {
        let directory = tempfile::tempdir().unwrap();
        let csv_path = directory.path().join("data.csv");
        std::fs::write(&csv_path, "名称,金额\n甲,1,200\n乙,2\n").unwrap();
        let csv = ingest(&csv_path, &IngestOptions::default()).unwrap();
        let xlsx_path = directory.path().join("data.xlsx");
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet.write_string(0, 0, "名称").unwrap();
        sheet.write_string(0, 1, "金额").unwrap();
        sheet.write_string(1, 0, "甲").unwrap();
        sheet.write_number(1, 1, 1200.0).unwrap();
        sheet.write_string(2, 0, "乙").unwrap();
        sheet.write_number(2, 1, 2.0).unwrap();
        workbook.save(&xlsx_path).unwrap();
        let xlsx = ingest(&xlsx_path, &IngestOptions::default()).unwrap();
        assert_eq!(csv.sheets[0].columns, xlsx.sheets[0].columns);
        assert_eq!(csv.sheets[0].rows.len(), xlsx.sheets[0].rows.len());
        assert_eq!(xlsx.sheets[0].rows[0]["名称"], "甲");
    }

    #[test]
    fn xlsx_ingestion_excludes_hidden_rows_and_columns_but_keeps_source_coordinates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hidden.xlsx");
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet
            .merge_range(0, 0, 0, 1, "编号", &rust_xlsxwriter::Format::new())
            .unwrap();
        sheet.write_string(1, 0, "甲").unwrap();
        sheet.write_string(1, 1, "内部").unwrap();
        sheet.write_string(2, 0, "乙").unwrap();
        sheet.write_string(2, 1, "公开").unwrap();
        sheet.set_row_hidden(1).unwrap();
        sheet.set_column_hidden(1).unwrap();
        workbook.save(&path).unwrap();
        let result = ingest(&path, &IngestOptions::default()).unwrap();
        let sheet = &result.sheets[0];
        assert_eq!(sheet.hidden_rows, vec![2]);
        assert_eq!(sheet.hidden_columns, vec![2]);
        assert_eq!(sheet.columns, vec!["编号"]);
        assert_eq!(sheet.rows.len(), 1);
        assert_eq!(sheet.rows[0]["编号"], "乙");
        assert_eq!(sheet.source_rows, vec![3]);
        assert_eq!(sheet.source_columns, vec![1]);
    }

    #[test]
    fn strict_xlsx_ingestion_rejects_formula_without_cached_result() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("formula.xlsx");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(
                "xl/worksheets/sheet1.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        std::io::Write::write_all(
            &mut writer,
            br#"<worksheet><sheetData><row><c r="A1"><f>1+1</f></c></row></sheetData></worksheet>"#,
        )
        .unwrap();
        writer.finish().unwrap();
        let options = IngestOptions {
            mode: IngestMode::Strict,
            ..Default::default()
        };
        let result = ingest(&path, &options);
        assert!(result.is_err());
    }

    #[test]
    fn xlsx_structure_keeps_1904_date_system_and_hidden_columns() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("structure.xlsx");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes) in [
            (
                "xl/workbook.xml",
                br#"<workbook xmlns:r="relationships"><workbookPr date1904="1"/><sheets><sheet name="Sheet1" state="visible" r:id="rId5"/></sheets></workbook>"#.as_slice(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                br#"<Relationships><Relationship Id="rId5" Target="worksheets/custom.xml"/></Relationships>"#.as_slice(),
            ),
            (
                "xl/worksheets/custom.xml",
                br#"<worksheet><cols><col min="2" max="3" hidden="1"/></cols><mergeCells><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#.as_slice(),
            ),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, bytes).unwrap();
        }
        writer.finish().unwrap();
        let (_, merged, hidden_columns, hidden_rows, date_system) =
            xlsx::xlsx_structure(&path, &IngestOptions::default()).unwrap();
        assert!(date_system);
        assert_eq!(hidden_columns[0], vec![2, 3]);
        assert!(hidden_rows[0].is_empty());
        assert_eq!(merged[0], vec!["A1:B1"]);
    }

    #[test]
    fn dirty_headers_ambiguous_dates_and_budgets_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dirty.csv");
        std::fs::write(&path, "日期,,日期\n3/4/2026,x,2026-03-04\n").unwrap();
        let result = ingest(&path, &IngestOptions::default()).unwrap();
        assert_eq!(result.sheets[0].columns, vec!["日期", "列2", "日期_2"]);
        assert_eq!(result.sheets[0].rows[0]["日期"], "3/4/2026");
        assert_eq!(result.sheets[0].rows[0]["日期_2"], "2026-03-04");
        assert_eq!(
            result
                .issues
                .iter()
                .filter(|issue| issue.message.contains("歧义"))
                .count(),
            1,
            "{:?}",
            result.issues
        );

        let strict = IngestOptions {
            mode: IngestMode::Strict,
            ..Default::default()
        };
        assert!(ingest(&path, &strict)
            .unwrap_err()
            .contains("严格模式拒绝歧义日期"));

        assert!(ingest(
            &path,
            &IngestOptions {
                max_rows: 0,
                ..Default::default()
            }
        )
        .is_err());
        assert!(ingest(
            &path,
            &IngestOptions {
                max_bytes: 1,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("字节预算"));
    }

    #[test]
    fn amounts_percentages_units_and_excel_dates_normalize_without_corrupting_text() {
        assert_eq!(normalize_cell("￥1,234.50元"), "1234.5");
        assert_eq!(normalize_cell("(2.5万元)"), "-25000");
        assert_eq!(normalize_cell("12.5％"), "0.125");
        assert_eq!(normalize_cell("2026年3月4日"), "2026-03-04");
        assert_eq!(normalize_cell("2026/3/4"), "2026-03-04");
        assert_eq!(normalize_cell("ACME, Inc"), "ACME, Inc");
        assert_eq!(normalize_cell("12,34"), "12,34");

        let date_1900 = Data::DateTime(calamine::ExcelDateTime::new(
            61.0,
            calamine::ExcelDateTimeType::DateTime,
            false,
        ));
        let date_1904 = Data::DateTime(calamine::ExcelDateTime::new(
            0.0,
            calamine::ExcelDateTimeType::DateTime,
            true,
        ));
        assert_eq!(cell_text(&date_1900), "1900-03-01");
        assert_eq!(cell_text(&date_1904), "1904-01-01");
        assert!(xlsx::validate_sheet_dimensions(
            r#"<worksheet><sheetData><row r="100"><c r="ZZ100"/></row></sheetData></worksheet>"#,
            "xl/worksheets/sheet1.xml",
            &IngestOptions {
                max_rows: 10,
                max_columns: 10,
                ..IngestOptions::default()
            },
        )
        .is_err());
    }
}
