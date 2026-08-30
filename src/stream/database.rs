use super::{StreamSource, ValueStreamSource};
use crate::value::{DatabaseValue, Value};
use rusqlite::params_from_iter;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::str::FromStr;

struct DatabaseStream {
    database: Rc<DatabaseValue>,
    query: String,
    parameters: Vec<SqlValue>,
    columns: Option<Vec<String>>,
    offset: usize,
    finished: bool,
}

pub(crate) fn database_rows(
    database: Rc<DatabaseValue>,
    query: String,
    parameters: Vec<SqlValue>,
) -> StreamSource {
    StreamSource::decoded(DatabaseStream {
        database,
        query,
        parameters,
        columns: None,
        offset: 0,
        finished: false,
    })
}

impl ValueStreamSource for DatabaseStream {
    fn next_value(&mut self) -> Result<Option<Value>, String> {
        if self.finished {
            return Ok(None);
        }
        let query = self.query.trim().trim_end_matches(';').trim();
        let database = self.database.clone();
        let parameters = self.parameters.clone();
        let offset = self.offset;
        let known_columns = self.columns.clone();
        let sql = format!(
            "SELECT * FROM ({query}) AS __hengshu_stream LIMIT 1 OFFSET {}",
            offset
        );
        let fetched = {
            let connection = database.connection.borrow();
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| error.to_string())?;
            let columns = known_columns.unwrap_or_else(|| {
                statement
                    .column_names()
                    .iter()
                    .map(|name| name.to_string())
                    .collect()
            });
            let mut rows = statement
                .query(params_from_iter(parameters.iter()))
                .map_err(|error| error.to_string())?;
            match rows.next().map_err(|error| error.to_string())? {
                Some(row) => {
                    let mut result = BTreeMap::new();
                    for (index, column) in columns.iter().enumerate() {
                        result.insert(
                            column.clone(),
                            from_sql(row.get_ref(index).map_err(|error| error.to_string())?),
                        );
                    }
                    Some((columns, result))
                }
                None => None,
            }
        };
        let Some((columns, row)) = fetched else {
            self.finished = true;
            return Ok(None);
        };
        self.columns = Some(columns);
        self.offset += 1;
        Ok(Some(Value::record(row)))
    }
}

fn from_sql(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Number(Decimal::from(value)),
        ValueRef::Real(value) => Value::Float(value),
        ValueRef::Text(value) => {
            let text = String::from_utf8_lossy(value).into_owned();
            Decimal::from_str(&text)
                .map(Value::Number)
                .unwrap_or(Value::Text(text))
        }
        ValueRef::Blob(value) => Value::Bytes(Rc::new(std::cell::RefCell::new(value.to_vec()))),
    }
}
