use crate::resource::{check_file_size, DEFAULT_MAX_BUFFER_BYTES};
use crate::runtime::Interpreter;
use crate::stream::coerce;
use crate::value::{Rows, StreamValue, Table, Value};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::streaming_multi_summary;

pub fn table_native(
    interpreter: &mut Interpreter,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
) -> Result<Value, String> {
    crate::contracts::validate_arity(name, args.len(), table_native_arity(name))?;
    match name {
        "把记录整理成表格" => {
            let Value::List(values) = &args[0] else {
                return Err("把记录整理成表格需要一组记录".to_string());
            };
            let values = values.borrow();
            if values.len() > interpreter.permissions.collection_limit() {
                return Err("记录数量超过集合项数限制".to_string());
            }
            let mut rows = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let Value::Record(record) = value else {
                    return Err(format!("待整理内容的第 {} 项不是记录", index + 1));
                };
                rows.push(record.borrow().clone());
            }
            Ok(Value::Table(Rc::new(std::cell::RefCell::new(Table {
                rows,
                ..Default::default()
            }))))
        }
        "读取CSV流" => {
            let path = resolve(&interpreter.base_dir, &args[0]);
            interpreter.permissions.check_file(&path)?;
            Ok(Value::Stream(Rc::new(std::cell::RefCell::new(
                StreamValue {
                    source: crate::stream::open_csv_rows(&path)?,
                    operations: Vec::new(),
                },
            ))))
        }
        "读取表格" => {
            let path = resolve(&interpreter.base_dir, &args[0]);
            interpreter.permissions.check_file(&path)?;
            let maximum_rows = named
                .get("最大行数")
                .map(|value| {
                    crate::contracts::decimal_to_usize(value.as_decimal()?, "读取表格最大行数")
                })
                .transpose()?
                .unwrap_or_else(|| interpreter.permissions.collection_limit());
            if maximum_rows > interpreter.permissions.collection_limit() {
                return Err("读取表格最大行数超过集合项数限制".to_string());
            }
            if named
                .keys()
                .any(|key| INGEST_OPTIONS.contains(&key.as_str()))
            {
                return read_selected_sheet(&path, &named, maximum_rows);
            }
            read_table(&path.to_string_lossy(), maximum_rows)
                .map(|t| Value::Table(Rc::new(std::cell::RefCell::new(t))))
                .map_err(|e| e.to_string())
        }
        "流式多维汇总" => {
            let path = resolve(&interpreter.base_dir, &args[0]);
            interpreter.permissions.check_file(&path)?;
            let collection_limit = interpreter.permissions.collection_limit();
            let mut source = crate::stream::open_csv_records(&path)?;
            streaming_multi_summary(
                &mut source,
                &args[1],
                &args[2],
                args.get(3),
                collection_limit,
                || interpreter.check_operation_limits(),
            )
        }
        "写入表格" => {
            let (rows, sensitivity, metadata, column_metadata) = match &args[0] {
                Value::Table(v) => {
                    let table = v.borrow();
                    (
                        table.rows.clone(),
                        table.sensitivity(),
                        table.metadata.clone(),
                        table.column_metadata.clone(),
                    )
                }
                _ => return Err("写入表格需要表格对象".to_string()),
            };
            let path = resolve(&interpreter.base_dir, &args[1]);
            interpreter.permissions.check_file(&path)?;
            crate::privacy::guard_endpoint(crate::privacy::Endpoint::LocalFile, &sensitivity)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
            }
            let format = data_format(&path);
            let text = path.to_string_lossy();
            match format {
                "XLSX" => write_xlsx(&rows, &text)?,
                "JSON" => write_json(&rows, &path)?,
                _ => write_csv(&rows, &text)?,
            }
            if let Some(target) = named.get("单位schema") {
                let target = resolve(&interpreter.base_dir, target);
                interpreter.permissions.check_file(&target)?;
                write_unit_schema(&target, format, &metadata, &column_metadata)?;
            }
            Ok(Value::Null)
        }
        "分区写出" => {
            let Value::Table(table) = &args[0] else {
                return Err("分区写出需要表格对象".to_string());
            };
            let column = args
                .get(1)
                .or_else(|| named.get("分区列"))
                .map(ToString::to_string)
                .ok_or_else(|| "分区写出缺少分区列".to_string())?;
            let directory = resolve(&interpreter.base_dir, &args[2]);
            interpreter.permissions.check_file(&directory)?;
            let format = named
                .get("格式")
                .map(ToString::to_string)
                .unwrap_or_else(|| "CSV".into())
                .to_uppercase();
            let paths = write_partitions(&table.borrow().rows, &column, &directory, &format)
                .map_err(|e| e.to_string())?;
            Ok(Value::list(
                paths
                    .into_iter()
                    .map(|path| Value::Path(Rc::new(path)))
                    .collect(),
            ))
        }
        _ => Err(format!("未知表格功能“{name}”")),
    }
}

fn table_native_arity(name: &str) -> Option<(usize, Option<usize>)> {
    Some(match name {
        "读取CSV流" | "读取表格" | "把记录整理成表格" => (1, Some(1)),
        "写入表格" => (2, Some(2)),
        "流式多维汇总" => (3, Some(4)),
        "分区写出" => (3, Some(3)),
        _ => return None,
    })
}

fn resolve(base_dir: &Path, value: &Value) -> PathBuf {
    let path = PathBuf::from(value.to_string());
    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}
/// 出现这些命名选项时改用 ingest 选项模型；缺省仍走旧的单 sheet 读取。
const INGEST_OPTIONS: [&str; 4] = ["工作表", "工作表序号", "表头行数", "模式"];

fn read_selected_sheet(
    path: &Path,
    named: &BTreeMap<String, Value>,
    maximum_rows: usize,
) -> Result<Value, String> {
    let mut options = crate::ingest::IngestOptions {
        max_rows: maximum_rows,
        ..Default::default()
    };
    if let Some(name) = named.get("工作表") {
        options.sheet_names = vec![name.to_string()];
    }
    if let Some(index) = named.get("工作表序号") {
        let index = crate::contracts::decimal_to_usize(index.as_decimal()?, "读取表格工作表序号")?;
        if index == 0 {
            return Err("读取表格工作表序号从 1 开始".to_string());
        }
        options.sheet_indices = vec![index - 1];
    }
    if let Some(rows) = named.get("表头行数") {
        options.header_rows =
            crate::contracts::decimal_to_usize(rows.as_decimal()?, "读取表格表头行数")?;
    }
    if let Some(mode) = named.get("模式") {
        options.mode = match mode.to_string().as_str() {
            "严格" => crate::ingest::IngestMode::Strict,
            "宽松" => crate::ingest::IngestMode::Lenient,
            other => return Err(format!("读取表格模式只支持 严格 或 宽松，收到“{other}”")),
        };
    }
    let workbook = crate::ingest::ingest(path, &options)?;
    let sheet = workbook
        .sheets
        .into_iter()
        .next()
        .ok_or("工作簿没有匹配的工作表")?;
    let rows = sheet
        .rows
        .into_iter()
        .take(maximum_rows)
        .map(|row| {
            row.into_iter()
                .map(|(column, cell)| (column, coerce(&cell)))
                .collect()
        })
        .collect();
    Ok(Value::Table(Rc::new(std::cell::RefCell::new(Table {
        rows,
        ..Default::default()
    }))))
}

fn read_table(path: &str, maximum_rows: usize) -> Result<Table, String> {
    check_file_size(std::path::Path::new(path), DEFAULT_MAX_BUFFER_BYTES)
        .map_err(|e| e.to_string())?;
    if path.to_lowercase().ends_with(".xlsx") {
        let mut table = read_xlsx(path)?;
        table.rows.truncate(maximum_rows);
        Ok(table)
    } else {
        read_csv(path, maximum_rows)
    }
}
fn read_csv(path: &str, maximum_rows: usize) -> Result<Table, String> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(path)
        .map_err(|e| e.to_string())?;
    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    let mut rows = Vec::new();
    for result in reader.records().take(maximum_rows) {
        let record = result.map_err(|e| e.to_string())?;
        rows.push(
            headers
                .iter()
                .zip(record.iter())
                .map(|(k, v)| (k.into(), coerce(v)))
                .collect(),
        );
    }
    Ok(Table {
        rows,
        ..Default::default()
    })
}
/// 数据文件格式只按扩展名判定，schema 文件里也记录同一个名字，便于下游核对。
fn data_format(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("xlsx") => "XLSX",
        Some("json") => "JSON",
        _ => "CSV",
    }
}

fn write_json(rows: &Rows, path: &Path) -> Result<(), String> {
    let records = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|(column, value)| {
                    let json = match value {
                        Value::Null => serde_json::Value::Null,
                        Value::Bool(value) => serde_json::Value::Bool(*value),
                        Value::Number(value) => value
                            .normalize()
                            .to_string()
                            .parse::<serde_json::Number>()
                            .map_or_else(
                                |_| serde_json::Value::String(value.to_string()),
                                serde_json::Value::Number,
                            ),
                        Value::Float(value) => serde_json::Number::from_f64(*value)
                            .map_or(serde_json::Value::Null, Into::into),
                        other => serde_json::Value::String(other.to_string()),
                    };
                    (column.clone(), json)
                })
                .collect::<serde_json::Map<_, _>>()
        })
        .collect::<Vec<_>>();
    let text = serde_json::to_string_pretty(&records).map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| error.to_string())
}

fn write_unit_schema(
    path: &Path,
    format: &str,
    metadata: &crate::execution::Metadata,
    columns: &BTreeMap<String, crate::execution::Metadata>,
) -> Result<(), String> {
    #[derive(serde::Serialize)]
    struct UnitSchema<'a> {
        version: u32,
        data_format: &'a str,
        table: &'a crate::execution::Metadata,
        columns: &'a BTreeMap<String, crate::execution::Metadata>,
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(&UnitSchema {
        version: 1,
        data_format: format,
        table: metadata,
        columns,
    })
    .map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| error.to_string())
}

pub(crate) fn write_csv(rows: &Rows, path: &str) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path).map_err(|e| e.to_string())?;
    if let Some(row) = rows.first() {
        let headers: Vec<_> = row.keys().cloned().collect();
        writer.write_record(&headers).map_err(|e| e.to_string())?;
        for row in rows {
            writer
                .write_record(headers.iter().map(|header| match row.get(header) {
                    None | Some(Value::Null) => String::new(),
                    Some(value) => value.to_string(),
                }))
                .map_err(|e| e.to_string())?;
        }
    }
    writer.flush().map_err(|e| e.to_string())
}
pub(crate) fn write_xlsx(rows: &Rows, path: &str) -> Result<(), String> {
    use rust_xlsxwriter::Workbook;
    let mut workbook = Workbook::new();
    {
        let sheet = workbook.add_worksheet();
        if let Some(row) = rows.first() {
            let headers = row.keys().cloned().collect::<Vec<_>>();
            for (c, h) in headers.iter().enumerate() {
                sheet
                    .write_string(0, c as u16, h)
                    .map_err(|e| e.to_string())?;
            }
            for (r, row) in rows.iter().enumerate() {
                for (c, h) in headers.iter().enumerate() {
                    match row.get(h).unwrap_or(&Value::Null) {
                        Value::Number(v) => {
                            sheet
                                .write_number(
                                    (r + 1) as u32,
                                    c as u16,
                                    v.to_string().parse::<f64>().unwrap_or(f64::NAN),
                                )
                                .map_err(|e| e.to_string())?;
                        }
                        Value::Float(v) => {
                            sheet
                                .write_number((r + 1) as u32, c as u16, *v)
                                .map_err(|e| e.to_string())?;
                        }
                        Value::Bool(v) => {
                            sheet
                                .write_boolean((r + 1) as u32, c as u16, *v)
                                .map_err(|e| e.to_string())?;
                        }
                        Value::Null => {}
                        v => {
                            sheet
                                .write_string((r + 1) as u32, c as u16, v.to_string())
                                .map_err(|e| e.to_string())?;
                        }
                    }
                }
            }
        }
    }
    workbook.save(path).map_err(|e| e.to_string())
}

pub(super) fn write_partitions(
    rows: &Rows,
    column: &str,
    directory: &Path,
    format: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut partitions: BTreeMap<String, Rows> = BTreeMap::new();
    for row in rows {
        let value = row
            .get(column)
            .ok_or_else(|| format!("表格不存在分区列“{column}”"))?
            .to_string();
        partitions
            .entry(safe_partition(&value))
            .or_default()
            .push(row.clone());
    }
    let mut paths = Vec::new();
    for (value, rows) in partitions {
        let partition_dir = directory.join(format!("{column}={value}"));
        std::fs::create_dir_all(&partition_dir).map_err(|e| e.to_string())?;
        let path = if format == "PARQUET" {
            partition_dir.join("part-00000.parquet")
        } else {
            partition_dir.join("part-00000.csv")
        };
        if format == "PARQUET" {
            write_parquet_strings(&rows, &path)?;
        } else if format == "CSV" {
            write_csv(&rows, &path.to_string_lossy())?;
        } else {
            return Err(format!("不支持的分区格式“{format}”"));
        }
        paths.push(path);
    }
    Ok(paths)
}

fn safe_partition(value: &str) -> String {
    let value = value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if value.is_empty() {
        "空".into()
    } else {
        value
    }
}

fn write_parquet_strings(rows: &Rows, path: &Path) -> Result<(), String> {
    use arrow_array::{ArrayRef, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    let headers = rows
        .first()
        .map(|row| row.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let schema = Arc::new(Schema::new(
        headers
            .iter()
            .map(|header| Field::new(header, DataType::Utf8, true))
            .collect::<Vec<_>>(),
    ));
    let arrays = headers
        .iter()
        .map(|header| {
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.get(header).map(ToString::to_string))
                    .collect::<Vec<_>>(),
            )) as ArrayRef
        })
        .collect::<Vec<_>>();
    let batch = RecordBatch::try_new(schema.clone(), arrays).map_err(|e| e.to_string())?;
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut writer = ArrowWriter::try_new(file, schema, None).map_err(|e| e.to_string())?;
    writer.write(&batch).map_err(|e| e.to_string())?;
    writer.close().map_err(|e| e.to_string())?;
    Ok(())
}
pub(super) fn read_xlsx(path: &str) -> Result<Table, String> {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut workbook = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let range = workbook
        .worksheet_range_at(0)
        .ok_or("工作簿没有工作表")?
        .map_err(|e| e.to_string())?;
    let mut iter = range.rows();
    let headers: Vec<String> = iter
        .next()
        .unwrap_or(&[])
        .iter()
        .map(|c| c.to_string())
        .collect();
    let rows = iter
        .map(|row| {
            headers
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let v = match row.get(i) {
                        Some(Data::Float(v)) => Value::Float(*v),
                        Some(Data::Int(v)) => Value::Number(Decimal::from(*v)),
                        Some(Data::Bool(v)) => Value::Bool(*v),
                        Some(Data::Empty) | None => Value::Null,
                        Some(v) => coerce(&v.to_string()),
                    };
                    (h.clone(), v)
                })
                .collect()
        })
        .collect();
    Ok(Table {
        rows,
        ..Default::default()
    })
}
