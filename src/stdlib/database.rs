use super::codec::bytes;
use super::{error, resolve};
use crate::value::{DatabaseValue, Rows, StreamValue, Table, Value};
use rusqlite::limits::Limit;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params_from_iter, Connection, OpenFlags};
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;

/// Stable boundary for future PostgreSQL/MySQL drivers. The interpreter only
/// depends on these capabilities and keeps SQL text in the external standard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseCapabilities {
    pub parameterized_sql: bool,
    pub transactions: bool,
    pub batch_parameters: bool,
    pub streaming_queries: bool,
}

pub trait DatabaseDriver {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> DatabaseCapabilities;
}

pub struct SqliteDriver;

impl DatabaseDriver for SqliteDriver {
    fn name(&self) -> &'static str {
        "SQLite"
    }

    fn capabilities(&self) -> DatabaseCapabilities {
        DatabaseCapabilities {
            parameterized_sql: true,
            transactions: true,
            batch_parameters: true,
            streaming_queries: true,
        }
    }
}

pub(super) fn database_call(
    name: &str,
    args: Vec<Value>,
    base_dir: &Path,
) -> Result<Value, String> {
    if name == "驱动能力" {
        let driver = SqliteDriver;
        let capabilities = driver.capabilities();
        return Ok(Value::record(BTreeMap::from([
            ("驱动".into(), Value::Text(driver.name().into())),
            (
                "参数化SQL".into(),
                Value::Bool(capabilities.parameterized_sql),
            ),
            ("事务".into(), Value::Bool(capabilities.transactions)),
            (
                "批量参数".into(),
                Value::Bool(capabilities.batch_parameters),
            ),
            (
                "流式查询".into(),
                Value::Bool(capabilities.streaming_queries),
            ),
        ])));
    }
    if name != "打开SQLite" {
        return Err(error(format!("数据库模块不支持“{name}”")));
    }
    let path = if args[0].to_string() == ":memory:" {
        PathBuf::from(":memory:")
    } else {
        resolve(base_dir, &args[0])
    };
    // 不启用 SQLITE_OPEN_URI：URI 会让实际打开的文件与通过权限校验的路径不是同一个值。
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Connection::open_with_flags(path, flags)
        .map(|connection| {
            // ATTACH 会绑定未经文件权限校验的路径，直接把可附加库数量降为零。
            connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
            Value::Database(Rc::new(DatabaseValue {
                driver: "SQLite".into(),
                connection: RefCell::new(connection),
            }))
        })
        .map_err(|error_value| error(error_value.to_string()))
}

#[cfg(test)]
pub(super) fn database_method(
    database: &Rc<DatabaseValue>,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
) -> Result<Value, String> {
    database_method_limited(
        database,
        name,
        args,
        named,
        crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
    )
}

pub(super) fn database_method_limited(
    database: &Rc<DatabaseValue>,
    name: &str,
    args: Vec<Value>,
    _named: BTreeMap<String, Value>,
    collection_limit: usize,
) -> Result<Value, String> {
    let parameters = if name == "批量执行" {
        Vec::new()
    } else {
        args.get(1)
            .map(|value| value.iter_values_limited(collection_limit))
            .transpose()?
            .unwrap_or_default()
            .iter()
            .map(to_sql)
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut connection = database.connection.borrow_mut();
    match name {
        "执行" => connection
            .execute(&args[0].to_string(), params_from_iter(parameters))
            .map(|count| Value::Number(Decimal::from(count)))
            .map_err(|error_value| error(error_value.to_string())),
        "批量执行" => {
            if let Some(rows) = args.get(1) {
                let rows = rows.iter_values_limited(collection_limit)?;
                let transaction = connection
                    .transaction()
                    .map_err(|error_value| error(error_value.to_string()))?;
                for row in rows {
                    let values = row
                        .iter_values_limited(collection_limit)?
                        .iter()
                        .map(to_sql)
                        .collect::<Result<Vec<_>, _>>()?;
                    transaction
                        .execute(&args[0].to_string(), params_from_iter(values))
                        .map_err(|error_value| error(error_value.to_string()))?;
                }
                transaction
                    .commit()
                    .map(|_| Value::Null)
                    .map_err(|error_value| error(error_value.to_string()))
            } else {
                connection
                    .execute_batch(&args[0].to_string())
                    .map(|_| Value::Null)
                    .map_err(|error_value| error(error_value.to_string()))
            }
        }
        "查询" => {
            let mut statement = connection
                .prepare(&args[0].to_string())
                .map_err(|error_value| error(error_value.to_string()))?;
            let columns = statement
                .column_names()
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>();
            let rows = statement
                .query_map(params_from_iter(parameters), |row| {
                    let mut result = BTreeMap::new();
                    for (index, column) in columns.iter().enumerate() {
                        result.insert(column.clone(), from_sql(row.get_ref(index)?));
                    }
                    Ok(result)
                })
                .map_err(|error_value| error(error_value.to_string()))?
                .collect::<Result<Rows, _>>()
                .map_err(|error_value| error(error_value.to_string()))?;
            Ok(Value::Table(Rc::new(RefCell::new(Table {
                rows,
                ..Default::default()
            }))))
        }
        "流式查询" => {
            drop(connection);
            let parameters = args
                .get(1)
                .map(|value| value.iter_values_limited(collection_limit))
                .transpose()?
                .unwrap_or_default()
                .iter()
                .map(to_sql)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Stream(Rc::new(RefCell::new(StreamValue {
                source: crate::stream::database_rows(
                    database.clone(),
                    args[0].to_string(),
                    parameters,
                ),
                operations: Vec::new(),
            }))))
        }
        "开始事务" => connection
            .execute_batch("BEGIN IMMEDIATE")
            .map(|_| Value::Null)
            .map_err(|error_value| error(error_value.to_string())),
        "提交" => connection
            .execute_batch("COMMIT")
            .map(|_| Value::Null)
            .map_err(|error_value| error(error_value.to_string())),
        "回滚" => connection
            .execute_batch("ROLLBACK")
            .map(|_| Value::Null)
            .map_err(|error_value| error(error_value.to_string())),
        _ => Err(error(format!("数据库连接不支持“{name}”"))),
    }
}

fn to_sql(value: &Value) -> Result<SqlValue, String> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Number(value) => SqlValue::Text(value.to_string()),
        Value::Float(value) => SqlValue::Real(*value),
        Value::Text(value) => SqlValue::Text(value.clone()),
        Value::Bytes(value) => SqlValue::Blob(value.borrow().clone()),
        Value::DateTime(value) => SqlValue::Text(value.to_rfc3339()),
        Value::Money(value) => SqlValue::Text(value.amount.to_string()),
        _ => return Err(format!("{}不能作为 SQL 参数", value.type_name())),
    })
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
        ValueRef::Blob(value) => bytes(value.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::StreamSource;

    #[test]
    fn sqlite_uses_standard_sql_and_parameters() {
        let driver = SqliteDriver;
        assert_eq!(driver.name(), "SQLite");
        assert!(driver.capabilities().streaming_queries);
        let database = database_call(
            "打开SQLite",
            vec![Value::Text(":memory:".into())],
            Path::new("."),
        )
        .unwrap();
        let Value::Database(database) = database else {
            panic!("需要数据库");
        };
        database_method(
            &database,
            "批量执行",
            vec![Value::Text(
                "CREATE TABLE sales(region TEXT, amount TEXT);".into(),
            )],
            BTreeMap::new(),
        )
        .unwrap();
        database_method(
            &database,
            "执行",
            vec![
                Value::Text("INSERT INTO sales VALUES (?, ?)".into()),
                Value::list(vec![
                    Value::Text("华东".into()),
                    Value::Number(Decimal::from(10)),
                ]),
            ],
            BTreeMap::new(),
        )
        .unwrap();
        let result = database_method(
            &database,
            "查询",
            vec![
                Value::Text("SELECT * FROM sales WHERE region = ?".into()),
                Value::list(vec![Value::Text("华东".into())]),
            ],
            BTreeMap::new(),
        )
        .unwrap();
        let Value::Table(table) = result else {
            panic!("需要表格");
        };
        assert_eq!(table.borrow().rows.len(), 1);
    }

    #[test]
    fn sqlite_cannot_attach_files_outside_the_permission_check() {
        let directory = tempfile::tempdir().unwrap();
        let escaped = directory.path().join("越权.db");
        let database = database_call(
            "打开SQLite",
            vec![Value::Text(":memory:".into())],
            Path::new("."),
        )
        .unwrap();
        let Value::Database(database) = database else {
            panic!("需要数据库");
        };
        let error = database_method(
            &database,
            "批量执行",
            vec![Value::Text(format!(
                "ATTACH DATABASE '{}' AS 越权;",
                escaped.display()
            ))],
            BTreeMap::new(),
        )
        .unwrap_err();
        assert!(!escaped.exists(), "{error}");

        let uri = directory.path().join("file:内存.db?mode=memory");
        let opened = database_call(
            "打开SQLite",
            vec![Value::Path(std::rc::Rc::new(uri.clone()))],
            directory.path(),
        );
        assert!(opened.is_ok());
        assert!(uri.exists(), "URI 文本应作为普通文件名打开");
    }

    #[test]
    fn sqlite_batch_parameters_are_atomic() {
        let database = database_call(
            "打开SQLite",
            vec![Value::Text(":memory:".into())],
            Path::new("."),
        )
        .unwrap();
        let Value::Database(database) = database else {
            panic!("需要数据库");
        };
        database_method(
            &database,
            "执行",
            vec![Value::Text(
                "CREATE TABLE items(name TEXT, amount INTEGER)".into(),
            )],
            BTreeMap::new(),
        )
        .unwrap();
        database_method(
            &database,
            "批量执行",
            vec![
                Value::Text("INSERT INTO items VALUES (?, ?)".into()),
                Value::list(vec![
                    Value::list(vec![
                        Value::Text("甲".into()),
                        Value::Number(Decimal::from(1)),
                    ]),
                    Value::list(vec![
                        Value::Text("乙".into()),
                        Value::Number(Decimal::from(2)),
                    ]),
                ]),
            ],
            BTreeMap::new(),
        )
        .unwrap();
        let result = database_method(
            &database,
            "查询",
            vec![Value::Text("SELECT COUNT(*) AS count FROM items".into())],
            BTreeMap::new(),
        )
        .unwrap();
        let Value::Table(table) = result else {
            panic!("需要表格");
        };
        assert_eq!(table.borrow().rows[0]["count"].to_string(), "2");
    }

    #[test]
    fn sqlite_stream_reads_one_row_per_pull() {
        let database = database_call(
            "打开SQLite",
            vec![Value::Text(":memory:".into())],
            Path::new("."),
        )
        .unwrap();
        let Value::Database(database) = database else {
            panic!("需要数据库");
        };
        database_method(
            &database,
            "批量执行",
            vec![Value::Text(
                "CREATE TABLE items(id INTEGER, name TEXT)".into(),
            )],
            BTreeMap::new(),
        )
        .unwrap();
        database_method(
            &database,
            "批量执行",
            vec![
                Value::Text("INSERT INTO items VALUES (?, ?)".into()),
                Value::list(vec![
                    Value::list(vec![
                        Value::Number(Decimal::from(1)),
                        Value::Text("甲".into()),
                    ]),
                    Value::list(vec![
                        Value::Number(Decimal::from(2)),
                        Value::Text("乙".into()),
                    ]),
                ]),
            ],
            BTreeMap::new(),
        )
        .unwrap();
        let Value::Stream(stream) = database_method(
            &database,
            "流式查询",
            vec![Value::Text("SELECT id, name FROM items ORDER BY id".into())],
            BTreeMap::new(),
        )
        .unwrap() else {
            panic!("需要流");
        };
        let first = match &stream.borrow().source {
            StreamSource::Decoded(cursor) => cursor.borrow_mut().next_value().unwrap().unwrap(),
            _ => panic!("需要数据库流"),
        };
        let Value::Record(first) = first else {
            panic!("需要记录")
        };
        assert_eq!(first.borrow()["id"].to_string(), "1");
        assert_eq!(first.borrow()["name"].to_string(), "甲");
        let second = match &stream.borrow().source {
            StreamSource::Decoded(cursor) => cursor.borrow_mut().next_value().unwrap().unwrap(),
            _ => panic!("需要数据库流"),
        };
        let Value::Record(second) = second else {
            panic!("需要记录")
        };
        assert_eq!(second.borrow()["id"].to_string(), "2");
        assert_eq!(second.borrow()["name"].to_string(), "乙");
        let end = match &stream.borrow().source {
            StreamSource::Decoded(cursor) => cursor.borrow_mut().next_value().unwrap(),
            _ => panic!("需要数据库流"),
        };
        assert!(end.is_none());
    }

    #[test]
    fn sqlite_batch_rolls_back_on_constraint_error() {
        let database = database_call(
            "打开SQLite",
            vec![Value::Text(":memory:".into())],
            Path::new("."),
        )
        .unwrap();
        let Value::Database(database) = database else {
            panic!()
        };
        database_method(
            &database,
            "执行",
            vec![Value::Text(
                "CREATE TABLE x (id INTEGER PRIMARY KEY)".into(),
            )],
            BTreeMap::new(),
        )
        .unwrap();
        let result = database_method(
            &database,
            "批量执行",
            vec![
                Value::Text("INSERT INTO x VALUES (?)".into()),
                Value::list(vec![
                    Value::list(vec![Value::Number(Decimal::from(1))]),
                    Value::list(vec![Value::Number(Decimal::from(1))]),
                ]),
            ],
            BTreeMap::new(),
        );
        assert!(result.is_err());
        let Value::Table(table) = database_method(
            &database,
            "查询",
            vec![Value::Text("SELECT COUNT(*) AS n FROM x".into())],
            BTreeMap::new(),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(table.borrow().rows[0]["n"].to_string(), "0");
    }

    #[test]
    fn sqlite_parameters_obey_the_configured_collection_limit() {
        let database = database_call(
            "打开SQLite",
            vec![Value::Text(":memory:".into())],
            Path::new("."),
        )
        .unwrap();
        let Value::Database(database) = database else {
            panic!()
        };
        let parameters = Value::Range(Rc::new(crate::value::RangeValue {
            start: 1,
            end: 4,
            inclusive: false,
            step: 1,
        }));
        let error = database_method_limited(
            &database,
            "查询",
            vec![Value::Text("SELECT 1".into()), parameters],
            BTreeMap::new(),
            2,
        )
        .unwrap_err();
        assert!(error.contains("集合项数"), "{error}");
    }
}
