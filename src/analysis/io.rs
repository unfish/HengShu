use super::error;
use crate::resource::{check_file_size, DEFAULT_MAX_BUFFER_BYTES};
use crate::value::{Table, Value};
use rayon::prelude::*;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::fs::File;
use std::rc::Rc;

pub(super) fn read_csv_chunks(path: &str, size: usize) -> Result<Value, String> {
    check_file_size(std::path::Path::new(path), DEFAULT_MAX_BUFFER_BYTES)
        .map_err(|e| error(e.to_string()))?;
    let mut reader = csv::Reader::from_path(path).map_err(|e| error(e.to_string()))?;
    let headers = reader.headers().map_err(|e| error(e.to_string()))?.clone();
    let mut chunks = Vec::new();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| error(e.to_string()))?;
        rows.push(
            headers
                .iter()
                .zip(record.iter())
                .map(|(k, v)| {
                    (
                        k.into(),
                        if v.is_empty() {
                            Value::Null
                        } else if let Ok(n) = v.parse::<Decimal>() {
                            Value::Number(n)
                        } else {
                            Value::Text(v.into())
                        },
                    )
                })
                .collect(),
        );
        if rows.len() >= size {
            chunks.push(Value::Table(Rc::new(std::cell::RefCell::new(Table {
                rows: std::mem::take(&mut rows),
                ..Default::default()
            }))))
        }
    }
    if !rows.is_empty() {
        chunks.push(Value::Table(Rc::new(std::cell::RefCell::new(Table {
            rows,
            ..Default::default()
        }))))
    }
    Ok(Value::list(chunks))
}
pub(super) fn chunk_aggregate(
    path: &str,
    column: &str,
    op: &str,
    size: usize,
    workers: usize,
) -> Result<Value, String> {
    check_file_size(std::path::Path::new(path), DEFAULT_MAX_BUFFER_BYTES)
        .map_err(|e| error(e.to_string()))?;
    let size = size.max(1);
    let mut reader = csv::Reader::from_path(path).map_err(|e| error(e.to_string()))?;
    let headers = reader.headers().map_err(|e| error(e.to_string()))?.clone();
    let index = headers
        .iter()
        .position(|h| h == column)
        .ok_or_else(|| error(format!("CSV 不存在列“{column}”")))?;
    let workers = workers.max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|e| error(e.to_string()))?;
    let summarize = |chunk: &Vec<f64>| {
        (
            chunk.iter().sum::<f64>(),
            chunk.len(),
            chunk.iter().copied().fold(f64::INFINITY, f64::min),
            chunk.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let mut pending = Vec::new();
    let mut partial = Vec::new();
    let mut current = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| error(e.to_string()))?;
        if let Ok(v) = record.get(index).unwrap_or("").parse::<f64>() {
            current.push(v)
        }
        if current.len() >= size {
            pending.push(std::mem::take(&mut current));
            if pending.len() >= workers {
                partial.extend(
                    pool.install(|| pending.par_iter().map(&summarize).collect::<Vec<_>>()),
                );
                pending.clear();
            }
        }
    }
    if !current.is_empty() {
        pending.push(current)
    }
    partial.extend(pool.install(|| pending.par_iter().map(summarize).collect::<Vec<_>>()));
    let sum = partial.iter().map(|p| p.0).sum::<f64>();
    let count = partial.iter().map(|p| p.1).sum::<usize>();
    Ok(Value::Float(match op {
        "求和" | "总和" => sum,
        "计数" | "数量" => count as f64,
        "最小值" => partial.iter().map(|p| p.2).fold(f64::INFINITY, f64::min),
        "最大值" => partial
            .iter()
            .map(|p| p.3)
            .fold(f64::NEG_INFINITY, f64::max),
        _ => sum / count as f64,
    }))
}
pub(super) fn read_parquet(path: &str) -> Result<Value, String> {
    check_file_size(std::path::Path::new(path), DEFAULT_MAX_BUFFER_BYTES)
        .map_err(|e| error(e.to_string()))?;
    use arrow_array::{Array, BooleanArray, Float64Array, Int64Array, StringArray};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let file = File::open(path).map_err(|e| error(e.to_string()))?;
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| error(e.to_string()))?;
    let schema = builder.schema().clone();
    let reader = builder.build().map_err(|e| error(e.to_string()))?;
    let mut chunks = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| error(e.to_string()))?;
        let mut rows = Vec::new();
        for r in 0..batch.num_rows() {
            let mut row = BTreeMap::new();
            for (c, field) in schema.fields().iter().enumerate() {
                let a = batch.column(c);
                let value = if a.is_null(r) {
                    Value::Null
                } else if let Some(v) = a.as_any().downcast_ref::<Float64Array>() {
                    Value::Float(v.value(r))
                } else if let Some(v) = a.as_any().downcast_ref::<Int64Array>() {
                    Value::Number(Decimal::from(v.value(r)))
                } else if let Some(v) = a.as_any().downcast_ref::<StringArray>() {
                    Value::Text(v.value(r).into())
                } else if let Some(v) = a.as_any().downcast_ref::<BooleanArray>() {
                    Value::Bool(v.value(r))
                } else {
                    Value::Text(format!("{a:?}"))
                };
                row.insert(field.name().clone(), value);
            }
            rows.push(row)
        }
        chunks.push(Value::Table(Rc::new(std::cell::RefCell::new(Table {
            rows,
            ..Default::default()
        }))));
    }
    Ok(Value::list(chunks))
}

pub(super) fn open_parquet_stream(path: &str) -> Result<Value, String> {
    crate::stream::open_parquet_stream(path).map_err(error)
}
