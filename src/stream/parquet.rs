use super::{StreamSource, StreamValue, ValueStreamSource};
use crate::value::Value;
use arrow_array::{Array, BooleanArray, Float64Array, Int64Array, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::fs::File;
use std::rc::Rc;

struct ParquetRowStream {
    reader: Box<dyn Iterator<Item = Result<arrow_array::RecordBatch, arrow_schema::ArrowError>>>,
    current: Option<arrow_array::RecordBatch>,
    row_index: usize,
}

pub(crate) fn open_parquet_stream(path: &str) -> Result<Value, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    Ok(Value::Stream(Rc::new(std::cell::RefCell::new(
        StreamValue {
            source: StreamSource::decoded(ParquetRowStream {
                reader: Box::new(reader),
                current: None,
                row_index: 0,
            }),
            operations: Vec::new(),
        },
    ))))
}

impl ValueStreamSource for ParquetRowStream {
    fn next_value(&mut self) -> Result<Option<Value>, String> {
        loop {
            let needs_batch = self
                .current
                .as_ref()
                .is_none_or(|batch| self.row_index >= batch.num_rows());
            if needs_batch {
                self.current = self
                    .reader
                    .next()
                    .transpose()
                    .map_err(|error| error.to_string())?;
                self.row_index = 0;
                if self.current.is_none() {
                    return Ok(None);
                }
                if self
                    .current
                    .as_ref()
                    .is_some_and(|batch| batch.num_rows() == 0)
                {
                    self.current = None;
                    continue;
                }
            }
            let row_index = self.row_index;
            let batch = self
                .current
                .as_ref()
                .ok_or_else(|| "Parquet 流批次状态无效".to_string())?;
            let mut row = BTreeMap::new();
            for (column_index, field) in batch.schema().fields().iter().enumerate() {
                let array = batch.column(column_index);
                let value = if array.is_null(row_index) {
                    Value::Null
                } else if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
                    Value::Float(values.value(row_index))
                } else if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
                    Value::Number(Decimal::from(values.value(row_index)))
                } else if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
                    Value::Text(values.value(row_index).into())
                } else if let Some(values) = array.as_any().downcast_ref::<BooleanArray>() {
                    Value::Bool(values.value(row_index))
                } else {
                    Value::Text(format!("{array:?}"))
                };
                row.insert(field.name().clone(), value);
            }
            self.row_index += 1;
            return Ok(Some(Value::record(row)));
        }
    }
}
