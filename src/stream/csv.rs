use super::{RecordStream, StreamRecord, StreamSource, ValueStreamSource};
use crate::value::Value;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

struct CsvRowStream {
    records: CsvRecordStream,
}

pub(crate) struct CsvRecordStream {
    reader: csv::Reader<File>,
    columns: Vec<String>,
}

pub(crate) fn open_csv_rows(path: &Path) -> Result<StreamSource, String> {
    Ok(StreamSource::decoded(CsvRowStream {
        records: open_csv_records(path)?,
    }))
}

pub(crate) fn open_csv_records(path: &Path) -> Result<CsvRecordStream, String> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(path)
        .map_err(|error| error.to_string())?;
    let columns = reader
        .headers()
        .map_err(|error| error.to_string())?
        .iter()
        .map(str::to_string)
        .collect();
    Ok(CsvRecordStream { reader, columns })
}

impl ValueStreamSource for CsvRowStream {
    fn next_value(&mut self) -> Result<Option<Value>, String> {
        let Some(record) = self.records.next_record()? else {
            return Ok(None);
        };
        Ok(Some(Value::record(
            self.records
                .columns
                .iter()
                .zip(record.fields())
                .map(|(name, value)| (name.clone(), coerce(value)))
                .collect::<BTreeMap<_, _>>(),
        )))
    }
}

impl RecordStream for CsvRecordStream {
    fn columns(&self) -> &[String] {
        &self.columns
    }

    fn next_record(&mut self) -> Result<Option<StreamRecord>, String> {
        let mut record = csv::StringRecord::new();
        if !self
            .reader
            .read_record(&mut record)
            .map_err(|error| error.to_string())?
        {
            return Ok(None);
        }
        Ok(Some(StreamRecord {
            values: record.iter().map(str::to_string).collect(),
        }))
    }
}

pub(crate) fn coerce(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else if let Ok(number) = value.parse::<Decimal>() {
        Value::Number(number)
    } else {
        Value::Text(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn write(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("数据.csv");
        std::fs::write(&path, contents).unwrap();
        (directory, path)
    }

    fn rows(contents: &str) -> Vec<BTreeMap<String, Value>> {
        let (_directory, path) = write(contents);
        let StreamSource::Decoded(source) = open_csv_rows(&path).unwrap() else {
            panic!("预期解码流")
        };
        let mut collected = Vec::new();
        while let Some(value) = source.borrow_mut().next_value().unwrap() {
            let Value::Record(record) = value else {
                panic!("预期记录")
            };
            collected.push(record.borrow().clone());
        }
        collected
    }

    #[test]
    fn a_header_only_file_is_an_empty_record_stream_but_still_reports_columns() {
        let (_directory, path) = write("甲,乙\n");
        let mut source = open_csv_records(&path).unwrap();
        assert_eq!(source.columns(), ["甲".to_string(), "乙".to_string()]);
        assert!(source.next_record().unwrap().is_none());
        assert!(source.next_record().unwrap().is_none());
    }

    #[test]
    fn a_leading_byte_order_mark_is_stripped_from_the_first_column() {
        let (_directory, path) = write("\u{feff}日期,额\n2026-01-01,1\n");
        let source = open_csv_records(&path).unwrap();
        assert_eq!(source.columns()[0], "日期");
    }

    #[test]
    fn a_single_record_is_decoded_with_numeric_and_null_coercion() {
        let decoded = rows("名称,额,备注\n甲,12.50,\n");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0]["名称"].to_string(), "甲");
        assert!(matches!(decoded[0]["额"], Value::Number(_)));
        assert_eq!(decoded[0]["额"].to_string(), "12.5");
        assert!(matches!(decoded[0]["备注"], Value::Null));
    }

    #[test]
    fn multiple_records_are_decoded_lazily_in_file_order() {
        let decoded = rows("名称\n甲\n乙\n丙\n");
        let names = decoded
            .iter()
            .map(|row| row["名称"].to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["甲", "乙", "丙"]);
    }

    #[test]
    fn a_short_row_yields_fewer_keys_rather_than_null_padding() {
        let decoded = rows("甲,乙,丙\n1,2\n");
        assert_eq!(decoded[0].len(), 2);
        assert!(!decoded[0].contains_key("丙"));
    }

    #[test]
    fn a_long_row_drops_the_fields_beyond_the_header() {
        let decoded = rows("甲,乙\n1,2,3\n");
        assert_eq!(decoded[0].len(), 2);
        assert_eq!(decoded[0]["乙"].to_string(), "2");
    }

    #[test]
    fn record_streams_expose_raw_untrimmed_fields_by_index() {
        let (_directory, path) = write("甲,乙\n 1 ,文本\n");
        let mut source = open_csv_records(&path).unwrap();
        let record = source.next_record().unwrap().unwrap();
        assert_eq!(record.get(0), Some(" 1 "));
        assert_eq!(record.get(1), Some("文本"));
        assert_eq!(record.get(2), None);
    }

    #[test]
    fn a_missing_file_is_reported_as_an_error() {
        assert!(open_csv_records(std::path::Path::new("不存在的文件.csv")).is_err());
    }

    #[test]
    fn coercion_treats_whitespace_only_cells_as_null_and_keeps_non_numeric_text() {
        assert!(matches!(coerce("   "), Value::Null));
        assert!(matches!(coerce(""), Value::Null));
        assert!(matches!(coerce("-3.5"), Value::Number(_)));
        assert!(matches!(coerce("1e5"), Value::Number(_)));
        assert!(matches!(coerce("1,000"), Value::Text(_)));
        assert!(matches!(coerce("甲"), Value::Text(_)));
    }
}
