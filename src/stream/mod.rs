use crate::value::{PortableValue, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

mod binary;
mod bytes;
mod csv;
mod database;
mod parquet;
mod text;

pub(crate) use bytes::reader_chunks;
pub(crate) use csv::{coerce, open_csv_records, open_csv_rows};
pub(crate) use database::database_rows;
pub(crate) use parquet::open_parquet_stream;
pub(crate) use text::lines;

pub struct StreamValue {
    pub source: StreamSource,
    pub operations: Vec<StreamOperation>,
}

#[allow(private_interfaces)]
pub enum StreamSource {
    Values(VecDeque<Value>),
    Decoded(Rc<RefCell<dyn ValueStreamSource>>),
    Channel(crossbeam_channel::Receiver<PortableValue>),
}

impl StreamSource {
    pub(crate) fn decoded(source: impl ValueStreamSource + 'static) -> Self {
        Self::Decoded(Rc::new(RefCell::new(source)))
    }
}

pub(crate) trait ValueStreamSource {
    fn next_value(&mut self) -> Result<Option<Value>, String>;
}

pub(crate) struct StreamRecord {
    values: Vec<String>,
}

impl StreamRecord {
    pub(crate) fn get(&self, index: usize) -> Option<&str> {
        self.values.get(index).map(String::as_str)
    }

    pub(crate) fn fields(&self) -> impl Iterator<Item = &str> {
        self.values.iter().map(String::as_str)
    }

    #[cfg(test)]
    pub(crate) fn from_fields(values: impl IntoIterator<Item = String>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }
}

pub(crate) trait RecordStream {
    fn columns(&self) -> &[String];
    fn next_record(&mut self) -> Result<Option<StreamRecord>, String>;
}

#[derive(Clone)]
pub enum StreamOperation {
    Map(Value),
    Filter(Value),
}
