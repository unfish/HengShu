use crate::runtime::Interpreter;
use crate::value::*;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

mod aggregates;
mod io;
mod operations;
mod streaming_summary;

pub use aggregates::grouped_method;
use aggregates::{
    checked_div, checked_sum, correlation_table, describe_table, group_table, table_value,
};
pub use io::table_native;
#[cfg(test)]
use io::{read_xlsx, write_csv, write_partitions, write_xlsx};
use operations::{
    compare_table_values, join_tables_limited, pivot_table, rank_table, resample_table,
    rolling_table, unpivot_table_limited, validate_table,
};
#[cfg(test)]
use operations::{join_tables, unpivot_table};
use streaming_summary::streaming_multi_summary;

pub fn table_attr(table: &Table, name: &str) -> Option<Value> {
    match name {
        "行数" => Some(Value::Number(Decimal::from(table.rows.len()))),
        "列数" => Some(Value::Number(Decimal::from(
            table.rows.first().map_or(0, |r| r.len()),
        ))),
        "表头" => Some(Value::list(
            table
                .rows
                .first()
                .map(|r| r.keys().cloned().map(Value::Text).collect())
                .unwrap_or_default(),
        )),
        "单位" => Some(Value::record(
            table
                .column_metadata
                .iter()
                .filter_map(|(column, metadata)| {
                    metadata.unit.as_ref().map(|unit| {
                        (
                            column.clone(),
                            Value::record(BTreeMap::from([
                                ("名称".into(), Value::Text(unit.name.clone())),
                                ("维度".into(), Value::Text(unit.dimension.clone())),
                                (
                                    "比例".into(),
                                    Value::Number(
                                        Decimal::from(unit.scale_numerator)
                                            / Decimal::from(unit.scale_denominator),
                                    ),
                                ),
                                (
                                    "口径".into(),
                                    unit.basis.clone().map_or(Value::Null, Value::Text),
                                ),
                            ])),
                        )
                    })
                })
                .collect(),
        )),
        _ => {
            if table.rows.first().is_some_and(|r| r.contains_key(name)) {
                Some(Value::list(
                    table
                        .rows
                        .iter()
                        .map(|r| r.get(name).cloned().unwrap_or(Value::Null))
                        .collect(),
                ))
            } else {
                None
            }
        }
    }
}
pub fn table_method(
    interpreter: &mut Interpreter,
    table: &Table,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    line: usize,
) -> Result<Value, String> {
    let collection_limit = interpreter.permissions.collection_limit();
    interpreter.check_collection_growth(0, table.rows.len())?;
    let other_metadata = args.first().and_then(|value| match value {
        Value::Table(table) => Some((
            table.borrow().metadata.clone(),
            table.borrow().column_metadata.clone(),
        )),
        _ => None,
    });
    let pivot_value_column = (name == "透视" && args.len() >= 3).then(|| args[2].to_string());
    let result = match name {
        "设置列单位" => {
            let column = args[0].to_string();
            if !table
                .rows
                .first()
                .is_some_and(|row| row.contains_key(&column))
            {
                return Err(format!("表格不存在列“{column}”"));
            }
            let Value::Record(quantity) = &args[1] else {
                return Err("设置列单位需要单位 创建返回的单位数量".into());
            };
            let quantity = quantity.borrow();
            let unit = crate::execution::UnitIdentity::from_decimal_scale(
                quantity
                    .get("单位")
                    .ok_or("单位数量缺少“单位”")?
                    .to_string(),
                quantity
                    .get("维度")
                    .ok_or("单位数量缺少“维度”")?
                    .to_string(),
                quantity
                    .get("比例")
                    .ok_or("单位数量缺少“比例”")?
                    .as_decimal()?,
                quantity
                    .get("口径")
                    .filter(|value| !matches!(value, Value::Null))
                    .map(ToString::to_string),
            )?;
            let mut output = table.clone();
            output
                .column_metadata
                .entry(column.clone())
                .or_default()
                .unit = Some(unit.clone());
            output.metadata.units.insert(column, unit);
            Ok(Value::Table(Rc::new(std::cell::RefCell::new(output))))
        }
        "列单位" => {
            let column = args[0].to_string();
            let unit = table
                .column_metadata
                .get(&column)
                .and_then(|metadata| metadata.unit.as_ref())
                .ok_or_else(|| format!("表格列“{column}”没有单位/口径元数据"))?;
            Ok(Value::record(BTreeMap::from([
                ("单位".into(), Value::Text(unit.name.clone())),
                ("维度".into(), Value::Text(unit.dimension.clone())),
                (
                    "比例分子".into(),
                    Value::Number(Decimal::from(unit.scale_numerator)),
                ),
                (
                    "比例分母".into(),
                    Value::Number(Decimal::from(unit.scale_denominator)),
                ),
                (
                    "口径".into(),
                    unit.basis.clone().map_or(Value::Null, Value::Text),
                ),
            ])))
        }
        "设置列敏感" => {
            let column = args[0].to_string();
            require_column(table, &column, "设置列敏感")?;
            let mut output = table.clone();
            let metadata = output.column_metadata.entry(column).or_default();
            for label in &args[1..] {
                metadata
                    .sensitivity
                    .labels
                    .insert(sensitive_label(&label.to_string()));
            }
            metadata.sensitivity.masked = false;
            metadata.sensitivity.pseudonymized = false;
            Ok(Value::Table(Rc::new(std::cell::RefCell::new(output))))
        }
        "标记列脱敏" | "标记列假名" => {
            let column = args[0].to_string();
            require_column(table, &column, name)?;
            let mut output = table.clone();
            let metadata = output.column_metadata.entry(column).or_default();
            if name == "标记列脱敏" {
                metadata.sensitivity.masked = true;
            } else {
                metadata.sensitivity.pseudonymized = true;
            }
            Ok(Value::Table(Rc::new(std::cell::RefCell::new(output))))
        }
        "列敏感" => {
            let column = args[0].to_string();
            require_column(table, &column, "列敏感")?;
            let sensitivity = table
                .column_metadata
                .get(&column)
                .map(|metadata| metadata.sensitivity.clone())
                .unwrap_or_default();
            Ok(Value::record(BTreeMap::from([
                (
                    "类别".into(),
                    Value::list(
                        sensitivity
                            .labels
                            .iter()
                            .map(|label| Value::Text(sensitive_label_name(label)))
                            .collect(),
                    ),
                ),
                ("已脱敏".into(), Value::Bool(sensitivity.masked)),
                ("已假名化".into(), Value::Bool(sensitivity.pseudonymized)),
            ])))
        }
        "筛选" => {
            let function = args[0].clone();
            let mut rows = Vec::new();
            for row in &table.rows {
                if interpreter
                    .invoke_function(function.clone(), vec![Value::record(row.clone())], line)?
                    .truth()
                {
                    rows.push(row.clone())
                }
            }
            Ok(table_value(rows))
        }
        "排序" => {
            let col = args[0].to_string();
            let desc = named.get("顺序").is_some_and(|v| v.to_string() == "降序");
            let mut rows = table.rows.clone();
            rows.sort_by(|a, b| compare_table_values(a.get(&col), b.get(&col)));
            if desc {
                rows.reverse()
            }
            Ok(table_value(rows))
        }
        "取前" => {
            let count = crate::contracts::decimal_to_usize(args[0].as_decimal()?, "取前数量")?;
            Ok(table_value(
                table.rows.iter().take(count).cloned().collect(),
            ))
        }
        "分组" => Ok(Value::Grouped(Rc::new(Grouped {
            rows: table.rows.clone(),
            column: args[0].to_string(),
        }))),
        "选择列" => {
            let cols = list_or_single(&args[0], collection_limit)?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            Ok(table_value(
                table
                    .rows
                    .iter()
                    .map(|r| {
                        cols.iter()
                            .map(|c| (c.clone(), r.get(c).cloned().unwrap_or(Value::Null)))
                            .collect()
                    })
                    .collect(),
            ))
        }
        "去重" => {
            let cols = list_or_single(&args[0], collection_limit)?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            let mut rows = Vec::new();
            for row in &table.rows {
                let key = cols
                    .iter()
                    .map(|c| row.get(c).map(ToString::to_string).unwrap_or_default())
                    .collect::<Vec<_>>();
                if seen.insert(key) {
                    rows.push(row.clone())
                }
            }
            Ok(table_value(rows))
        }
        "转记录列表" => Ok(Value::list(
            table.rows.iter().cloned().map(Value::record).collect(),
        )),
        "填补缺失" => {
            let replacement = args
                .first()
                .or_else(|| named.get("值"))
                .cloned()
                .unwrap_or(Value::Number(Decimal::ZERO));
            Ok(table_value(
                table
                    .rows
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    if matches!(v, Value::Null) {
                                        replacement.clone()
                                    } else {
                                        v.clone()
                                    },
                                )
                            })
                            .collect()
                    })
                    .collect(),
            ))
        }
        "删除缺失" => Ok(table_value(
            table
                .rows
                .iter()
                .filter(|r| r.values().all(|v| !matches!(v, Value::Null)))
                .cloned()
                .collect(),
        )),
        "分组统计" | "分组汇总" => {
            let columns = args
                .first()
                .ok_or_else(|| "分组统计缺少分组列".to_string())?;
            let cols = list_or_single(columns, collection_limit)?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let operation = named
                .get("聚合")
                .map(ToString::to_string)
                .unwrap_or_else(|| "平均值".into());
            group_table(table, &cols, &operation)
        }
        "相关矩阵" | "相关系数矩阵" => correlation_table(table),
        "描述" | "描述统计" => describe_table(table),
        "连接" => {
            let Value::Table(other) = &args[0] else {
                return Err("连接需要另一个表格".to_string());
            };
            let keys = list_or_single(&args[1], collection_limit)?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let kind = named
                .get("方式")
                .map(ToString::to_string)
                .unwrap_or_else(|| "内连接".into());
            interpreter.check_collection_growth(0, other.borrow().rows.len())?;
            join_tables_limited(table, &other.borrow(), &keys, &kind, collection_limit)
        }
        "纵向合并" => {
            let Value::Table(other) = &args[0] else {
                return Err("纵向合并需要另一个表格".to_string());
            };
            interpreter.check_collection_growth(table.rows.len(), other.borrow().rows.len())?;
            let mut rows = table.rows.clone();
            rows.extend(other.borrow().rows.clone());
            Ok(table_value(rows))
        }
        "透视" => pivot_table(table, &args, &named),
        "反透视" => unpivot_table_limited(table, &args, collection_limit),
        "排名" => rank_table(table, &args, &named),
        "滚动统计" => rolling_table(table, &args, &named),
        "时间重采样" => resample_table(table, &args, &named),
        "校验" => validate_table(table, &args),
        _ => Err(format!("表格不支持动作“{name}”。")),
    }?;
    if matches!(
        name,
        "设置列单位" | "设置列敏感" | "标记列脱敏" | "标记列假名"
    ) {
        return Ok(result);
    }
    attach_table_metadata(result, table, other_metadata, pivot_value_column.as_deref())
}

fn attach_table_metadata(
    value: Value,
    source: &Table,
    other: Option<(
        crate::execution::Metadata,
        BTreeMap<String, crate::execution::Metadata>,
    )>,
    pivot_value_column: Option<&str>,
) -> Result<Value, String> {
    let Value::Table(output) = &value else {
        return Ok(value);
    };
    let mut output = output.borrow_mut();
    output.metadata = if let Some((metadata, _)) = &other {
        source.metadata.try_union(metadata)?
    } else {
        source.metadata.clone()
    };
    let headers = output
        .rows
        .first()
        .map(|row| row.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for header in headers {
        let mut metadata = source.column_metadata.get(&header).cloned();
        if let Some((_, columns)) = &other {
            let right_name = header.strip_suffix("_右").unwrap_or(&header);
            if let Some(right) = columns.get(right_name) {
                metadata = Some(match metadata {
                    Some(left) => left.try_union(right)?,
                    None => right.clone(),
                });
            }
        }
        if metadata.is_none() {
            if let Some(source_column) = pivot_value_column {
                metadata = source.column_metadata.get(source_column).cloned();
            }
        }
        if let Some(metadata) = metadata {
            output.column_metadata.insert(header, metadata);
        }
    }
    drop(output);
    Ok(value)
}

fn require_column(table: &Table, column: &str, method: &str) -> Result<(), String> {
    if table
        .rows
        .first()
        .is_some_and(|row| row.contains_key(column))
    {
        Ok(())
    } else {
        Err(format!("{method}：表格不存在列“{column}”"))
    }
}

fn sensitive_label(name: &str) -> crate::execution::SensitiveLabel {
    use crate::execution::SensitiveLabel;
    match name {
        "身份" => SensitiveLabel::Identity,
        "联系" => SensitiveLabel::Contact,
        "财务" => SensitiveLabel::Financial,
        "医疗" => SensitiveLabel::Medical,
        "教育" => SensitiveLabel::Education,
        other => SensitiveLabel::Custom(other.to_string()),
    }
}

pub(crate) fn sensitive_label_name(label: &crate::execution::SensitiveLabel) -> String {
    use crate::execution::SensitiveLabel;
    match label {
        SensitiveLabel::Identity => "身份".into(),
        SensitiveLabel::Contact => "联系".into(),
        SensitiveLabel::Financial => "财务".into(),
        SensitiveLabel::Medical => "医疗".into(),
        SensitiveLabel::Education => "教育".into(),
        SensitiveLabel::Custom(name) => name.clone(),
    }
}

fn list_or_single(value: &Value, collection_limit: usize) -> Result<Vec<Value>, String> {
    match value {
        Value::List(_) | Value::Set(_) | Value::NumericArray(_) | Value::Range(_) => {
            value.iter_values_limited(collection_limit)
        }
        _ => Ok(vec![value.clone()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_row_stream_pulls_records_without_materializing_a_table() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("数据.csv");
        std::fs::write(&path, "编号,名称\n1,甲\n2,乙\n").unwrap();
        let mut interpreter = Interpreter::new(directory.path(), false);
        let Value::Stream(stream) = table_native(
            &mut interpreter,
            "读取CSV流",
            vec![Value::Text("数据.csv".into())],
            BTreeMap::new(),
        )
        .unwrap() else {
            panic!()
        };
        let first = match &stream.borrow().source {
            StreamSource::Decoded(rows) => rows.borrow_mut().next_value().unwrap().unwrap(),
            _ => panic!(),
        };
        let Value::Record(first) = first else {
            panic!()
        };
        assert_eq!(first.borrow()["编号"].to_string(), "1");
        let second = match &stream.borrow().source {
            StreamSource::Decoded(rows) => rows.borrow_mut().next_value().unwrap().unwrap(),
            _ => panic!(),
        };
        let Value::Record(second) = second else {
            panic!()
        };
        assert_eq!(second.borrow()["名称"].to_string(), "乙");
    }

    #[test]
    fn table_read_can_stop_after_a_bounded_number_of_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("排名.csv");
        std::fs::write(&path, "编号,金额\n1,30\n2,20\n3,10\n").unwrap();
        let mut interpreter = Interpreter::new(directory.path(), false);
        let Value::Table(table) = table_native(
            &mut interpreter,
            "读取表格",
            vec![Value::Text("排名.csv".into())],
            BTreeMap::from([("最大行数".into(), Value::Number(Decimal::from(2)))]),
        )
        .unwrap() else {
            panic!("预期表格")
        };
        assert_eq!(table.borrow().rows.len(), 2);
        assert_eq!(table.borrow().rows[1]["编号"].to_string(), "2");
    }

    #[test]
    fn table_read_options_select_sheets_and_header_rows() {
        use rust_xlsxwriter::Workbook;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("多表.xlsx");
        let mut workbook = Workbook::new();
        let first = workbook.add_worksheet();
        first.set_name("一月").unwrap();
        for (row, values) in [["金额", "小计"], ["10", "20"]].iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                first
                    .write_string(row as u32, column as u16, *value)
                    .unwrap();
            }
        }
        let second = workbook.add_worksheet();
        second.set_name("二月").unwrap();
        for (row, values) in [["部门", "金额"], ["销售", "金额"], ["华东", "70"]]
            .iter()
            .enumerate()
        {
            for (column, value) in values.iter().enumerate() {
                second
                    .write_string(row as u32, column as u16, *value)
                    .unwrap();
            }
        }
        workbook.save(&path).unwrap();
        let mut interpreter = Interpreter::new(directory.path(), false);
        let read = |interpreter: &mut Interpreter, named: BTreeMap<String, Value>| {
            table_native(
                interpreter,
                "读取表格",
                vec![Value::Text("多表.xlsx".into())],
                named,
            )
        };
        let Value::Table(by_name) = read(
            &mut interpreter,
            BTreeMap::from([("工作表".into(), Value::Text("二月".into()))]),
        )
        .unwrap() else {
            panic!("预期表格")
        };
        assert_eq!(by_name.borrow().rows.len(), 2);
        assert_eq!(by_name.borrow().rows[1]["金额"].to_string(), "70");
        let Value::Table(multi_header) = read(
            &mut interpreter,
            BTreeMap::from([
                ("工作表序号".into(), Value::Number(Decimal::from(2))),
                ("表头行数".into(), Value::Number(Decimal::from(2))),
            ]),
        )
        .unwrap() else {
            panic!("预期表格")
        };
        assert_eq!(multi_header.borrow().rows.len(), 1);
        assert_eq!(multi_header.borrow().rows[0]["金额_金额"].to_string(), "70");
        assert!(read(
            &mut interpreter,
            BTreeMap::from([("工作表序号".into(), Value::Number(Decimal::ZERO))]),
        )
        .is_err());
        assert!(read(
            &mut interpreter,
            BTreeMap::from([("模式".into(), Value::Text("严".into()))]),
        )
        .is_err());
    }

    #[test]
    fn table_read_strict_mode_rejects_repaired_cells() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("脏账.csv");
        std::fs::write(&path, "占比\n 50% \n").unwrap();
        let mut interpreter = Interpreter::new(directory.path(), false);
        let arguments = vec![Value::Text("脏账.csv".into())];
        assert!(table_native(
            &mut interpreter,
            "读取表格",
            arguments.clone(),
            BTreeMap::from([("模式".into(), Value::Text("严格".into()))]),
        )
        .is_err());
        let Value::Table(lenient) = table_native(
            &mut interpreter,
            "读取表格",
            arguments,
            BTreeMap::from([("模式".into(), Value::Text("宽松".into()))]),
        )
        .unwrap() else {
            panic!("预期表格")
        };
        assert_eq!(lenient.borrow().rows[0]["占比"].to_string(), "0.5");
    }

    #[test]
    fn table_write_serializes_json_rows_and_an_explicit_unit_schema() {
        let directory = tempfile::tempdir().unwrap();
        let mut interpreter = Interpreter::new(directory.path(), false);
        let table = Value::Table(Rc::new(std::cell::RefCell::new(Table {
            rows: vec![BTreeMap::from([
                ("部门".into(), Value::Text("华东".into())),
                ("金额".into(), Value::Number(Decimal::from(70))),
            ])],
            ..Default::default()
        })));
        let quantity = Value::record(BTreeMap::from([
            ("值".into(), Value::Number(Decimal::from(1))),
            ("单位".into(), Value::Text("万元".into())),
            ("维度".into(), Value::Text("货币".into())),
            ("比例".into(), Value::Number(Decimal::from(10_000))),
            ("口径".into(), Value::Text("CNY:含税".into())),
        ]));
        let Value::Table(source) = &table else {
            panic!("预期表格")
        };
        let with_unit = table_method(
            &mut interpreter,
            &source.borrow().clone(),
            "设置列单位",
            vec![Value::Text("金额".into()), quantity],
            BTreeMap::new(),
            1,
        )
        .unwrap();
        let write = |interpreter: &mut Interpreter, data: &str| {
            table_native(
                interpreter,
                "写入表格",
                vec![with_unit.clone(), Value::Text(data.into())],
                BTreeMap::from([("单位schema".into(), Value::Text("结果.schema.json".into()))]),
            )
            .unwrap();
            let schema: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(directory.path().join("结果.schema.json")).unwrap(),
            )
            .unwrap();
            schema
        };
        let schema = write(&mut interpreter, "结果.json");
        assert_eq!(schema["version"], 1);
        assert_eq!(schema["data_format"], "JSON");
        assert_eq!(schema["columns"]["金额"]["unit"]["name"], "万元");
        assert_eq!(schema["columns"]["金额"]["unit"]["basis"], "CNY:含税");
        assert_eq!(schema["table"]["units"]["金额"]["dimension"], "货币");
        let rows: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join("结果.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(rows[0]["金额"], 70);
        assert_eq!(rows[0]["部门"], "华东");
        assert_eq!(write(&mut interpreter, "结果.csv")["data_format"], "CSV");
    }

    #[test]
    fn records_convert_to_table_and_write_stable_chinese_csv() {
        let directory = tempfile::tempdir().unwrap();
        let mut interpreter = Interpreter::new(directory.path(), false);
        let records = Value::list(vec![
            Value::record(BTreeMap::from([
                ("商品名称".into(), Value::Text("气缸".into())),
                ("优惠价".into(), Value::Null),
                ("订货编码".into(), Value::Text("A001".into())),
            ])),
            Value::record(BTreeMap::from([
                ("商品名称".into(), Value::Text("接头".into())),
                ("优惠价".into(), Value::Number(Decimal::from(12))),
                ("订货编码".into(), Value::Text("A002".into())),
            ])),
        ]);
        let Value::Table(table) = table_native(
            &mut interpreter,
            "把记录整理成表格",
            vec![records],
            BTreeMap::new(),
        )
        .unwrap() else {
            panic!("预期表格")
        };
        assert_eq!(table.borrow().rows.len(), 2);
        table_native(
            &mut interpreter,
            "写入表格",
            vec![Value::Table(table), Value::Text("商品.csv".into())],
            BTreeMap::new(),
        )
        .unwrap();
        let csv = std::fs::read_to_string(directory.path().join("商品.csv")).unwrap();
        assert_eq!(csv, "优惠价,商品名称,订货编码\n,气缸,A001\n12,接头,A002\n");

        let error = table_native(
            &mut interpreter,
            "把记录整理成表格",
            vec![Value::list(vec![Value::Text("错误".into())])],
            BTreeMap::new(),
        )
        .unwrap_err();
        assert!(error.contains("第 1 项不是记录"));

        let Value::Table(empty) = table_native(
            &mut interpreter,
            "把记录整理成表格",
            vec![Value::list(vec![])],
            BTreeMap::new(),
        )
        .unwrap() else {
            panic!("预期空表格")
        };
        assert!(empty.borrow().rows.is_empty());
    }

    #[test]
    fn xlsx_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("数据.xlsx");
        let rows = vec![BTreeMap::from([
            ("部门".into(), Value::Text("甲".into())),
            ("金额".into(), Value::Number(Decimal::from(12))),
        ])];
        write_xlsx(&rows, path.to_str().unwrap()).unwrap();
        let table = read_xlsx(path.to_str().unwrap()).unwrap();
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0]["部门"].to_string(), "甲");
        assert_eq!(table.rows[0]["金额"].to_string(), "12");
    }

    #[test]
    fn csv_round_trip_preserves_null_as_a_missing_value() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("缺失值.csv");
        let rows = vec![BTreeMap::from([
            ("名称".into(), Value::Text("甲".into())),
            ("备注".into(), Value::Null),
        ])];
        write_csv(&rows, path.to_str().unwrap()).unwrap();

        let mut interpreter = Interpreter::new(directory.path(), false);
        let Value::Table(table) = table_native(
            &mut interpreter,
            "读取表格",
            vec![Value::Text("缺失值.csv".into())],
            BTreeMap::new(),
        )
        .unwrap() else {
            panic!("预期表格")
        };
        assert!(matches!(table.borrow().rows[0]["备注"], Value::Null));
    }

    #[test]
    fn group_and_correlation_analysis() {
        let table = Table {
            rows: vec![
                BTreeMap::from([
                    ("地区".into(), Value::Text("华东".into())),
                    ("销售额".into(), Value::Float(100.0)),
                    ("成本".into(), Value::Float(60.0)),
                ]),
                BTreeMap::from([
                    ("地区".into(), Value::Text("华东".into())),
                    ("销售额".into(), Value::Float(200.0)),
                    ("成本".into(), Value::Float(120.0)),
                ]),
                BTreeMap::from([
                    ("地区".into(), Value::Text("华南".into())),
                    ("销售额".into(), Value::Float(80.0)),
                    ("成本".into(), Value::Float(48.0)),
                ]),
            ],
            ..Default::default()
        };
        let Value::Table(grouped) = group_table(&table, &["地区".into()], "求和").unwrap()
        else {
            panic!()
        };
        assert_eq!(grouped.borrow().rows.len(), 2);
        let Value::Table(correlation) = correlation_table(&table).unwrap() else {
            panic!()
        };
        assert_eq!(correlation.borrow().rows.len(), 2);
    }

    #[test]
    fn table_sort_uses_numeric_values_before_text_fallback() {
        let table = Table {
            rows: vec![
                BTreeMap::from([("值".into(), Value::Number(Decimal::from(2)))]),
                BTreeMap::from([("值".into(), Value::Number(Decimal::from(10)))]),
            ],
            ..Default::default()
        };
        let mut interpreter = Interpreter::new(".", false);
        let Value::Table(sorted) = table_method(
            &mut interpreter,
            &table,
            "排序",
            vec![Value::Text("值".into())],
            BTreeMap::new(),
            1,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(sorted.borrow().rows[0]["值"].to_string(), "2");
        assert_eq!(sorted.borrow().rows[1]["值"].to_string(), "10");
    }

    #[test]
    fn table_operations_preserve_column_units_and_reject_join_conflicts() {
        let yuan = crate::execution::Metadata {
            unit: Some(crate::execution::UnitIdentity {
                name: "元".into(),
                dimension: "货币".into(),
                scale_numerator: 1,
                scale_denominator: 1,
                basis: Some("CNY".into()),
            }),
            ..Default::default()
        };
        let table = Table {
            rows: vec![BTreeMap::from([(
                "金额".into(),
                Value::Number(Decimal::ONE),
            )])],
            column_metadata: BTreeMap::from([("金额".into(), yuan.clone())]),
            ..Default::default()
        };
        let mut interpreter = Interpreter::new(".", false);
        let Value::Table(sorted) = table_method(
            &mut interpreter,
            &table,
            "排序",
            vec![Value::Text("金额".into())],
            BTreeMap::new(),
            1,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(sorted.borrow().column_metadata["金额"], yuan);

        let other = Value::Table(Rc::new(std::cell::RefCell::new(Table {
            rows: table.rows.clone(),
            column_metadata: BTreeMap::from([(
                "金额".into(),
                crate::execution::Metadata {
                    unit: Some(crate::execution::UnitIdentity {
                        name: "万元".into(),
                        dimension: "货币".into(),
                        scale_numerator: 10000,
                        scale_denominator: 1,
                        basis: Some("CNY".into()),
                    }),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        })));
        assert!(table_method(
            &mut interpreter,
            &table,
            "连接",
            vec![other, Value::Text("金额".into())],
            BTreeMap::new(),
            1
        )
        .is_err());
    }

    #[test]
    fn table_unit_methods_match_in_source_and_vm() {
        let source = "引入 表格处理\n引入 单位\n定义 表 为 表格处理 把记录整理成表格([{金额: 2}])\n定义 数量 为 单位 创建(2, \"万元\", \"货币\", 10000, \"CNY\")\n定义 有单位表 为 表 设置列单位(\"金额\", 数量)\n输出(有单位表 列单位(\"金额\") 的 单位)";
        let source_result =
            crate::api::run_source(source, crate::api::RunOptions::new(".")).unwrap();
        let vm_result = crate::api::run_compiled(source, crate::api::RunOptions::new(".")).unwrap();
        assert_eq!(source_result.output, ["万元"]);
        assert_eq!(source_result.output, vm_result.output);
    }

    #[test]
    fn table_sort_orders_money_and_floats_by_value_not_rendered_text() {
        let money = |amount: i64, currency: &str| {
            Value::Money(Rc::new(crate::value::MoneyValue {
                amount: Decimal::from(amount),
                currency: currency.into(),
                scale: 2,
                rounding: "银行家".into(),
                audit: Vec::new(),
            }))
        };
        let table = Table {
            rows: vec![
                BTreeMap::from([("金额".into(), money(10, "CNY"))]),
                BTreeMap::from([("金额".into(), money(2, "USD"))]),
                BTreeMap::from([("金额".into(), money(9, "CNY"))]),
            ],
            ..Default::default()
        };
        let mut interpreter = Interpreter::new(".", false);
        let Value::Table(sorted) = table_method(
            &mut interpreter,
            &table,
            "排序",
            vec![Value::Text("金额".into())],
            BTreeMap::new(),
            1,
        )
        .unwrap() else {
            panic!()
        };
        let order = sorted
            .borrow()
            .rows
            .iter()
            .map(|row| row["金额"].to_string())
            .collect::<Vec<_>>();
        assert_eq!(order, ["CNY 9", "CNY 10", "USD 2"]);

        let floats = Table {
            rows: vec![
                BTreeMap::from([("值".into(), Value::Float(1.0 + 2e-13))]),
                BTreeMap::from([("值".into(), Value::Float(1.0))]),
                BTreeMap::from([("值".into(), Value::Float(1.0 + 1e-13))]),
            ],
            ..Default::default()
        };
        let Value::Table(sorted) = table_method(
            &mut interpreter,
            &floats,
            "排序",
            vec![Value::Text("值".into())],
            BTreeMap::new(),
            1,
        )
        .unwrap() else {
            panic!()
        };
        let values = sorted
            .borrow()
            .rows
            .iter()
            .map(|row| match row["值"] {
                Value::Float(value) => value,
                _ => panic!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(values, [1.0, 1.0 + 1e-13, 1.0 + 2e-13]);
    }

    #[test]
    fn table_methods_obey_configured_collection_limits_before_expansion() {
        let mut interpreter = Interpreter::new(".", false);
        interpreter.permissions.max_collection_items = Some(2);
        let table = Table {
            rows: vec![BTreeMap::from([(
                "编号".into(),
                Value::Number(Decimal::ONE),
            )])],
            ..Default::default()
        };
        let range = Value::Range(Rc::new(RangeValue {
            start: 1,
            end: 4,
            inclusive: false,
            step: 1,
        }));
        let error = table_method(
            &mut interpreter,
            &table,
            "选择列",
            vec![range],
            BTreeMap::new(),
            1,
        )
        .unwrap_err();
        assert!(error.contains("集合项数"), "{error}");

        let duplicate_rows = vec![
            BTreeMap::from([("编号".into(), Value::Number(Decimal::ONE))]),
            BTreeMap::from([("编号".into(), Value::Number(Decimal::ONE))]),
        ];
        let left = Table {
            rows: duplicate_rows.clone(),
            ..Default::default()
        };
        let right = Value::Table(Rc::new(std::cell::RefCell::new(Table {
            rows: duplicate_rows,
            ..Default::default()
        })));
        let error = table_method(
            &mut interpreter,
            &left,
            "连接",
            vec![right, Value::Text("编号".into())],
            BTreeMap::new(),
            1,
        )
        .unwrap_err();
        assert!(error.contains("连接结果超过"), "{error}");
    }

    #[test]
    fn table_sort_has_deterministic_mixed_type_and_null_ordering() {
        let table = Table {
            rows: vec![
                BTreeMap::from([("值".into(), Value::Text("2".into()))]),
                BTreeMap::from([("值".into(), Value::Null)]),
                BTreeMap::from([("值".into(), Value::Float(3.0))]),
                BTreeMap::from([("值".into(), Value::Number(Decimal::from(10)))]),
                BTreeMap::from([("值".into(), Value::Bool(false))]),
            ],
            ..Default::default()
        };
        let mut interpreter = Interpreter::new(".", false);
        let Value::Table(sorted) = table_method(
            &mut interpreter,
            &table,
            "排序",
            vec![Value::Text("值".into())],
            BTreeMap::new(),
            1,
        )
        .unwrap() else {
            panic!()
        };
        let values = sorted
            .borrow()
            .rows
            .iter()
            .map(|row| row["值"].to_string())
            .collect::<Vec<_>>();
        assert_eq!(values, ["3", "10", "2", "否", "空"]);
    }

    #[test]
    fn partition_writes_csv_and_parquet_layout() {
        let directory = tempfile::tempdir().unwrap();
        let rows = vec![
            BTreeMap::from([
                ("地区".into(), Value::Text("华东".into())),
                ("金额".into(), Value::Number(Decimal::from(1))),
            ]),
            BTreeMap::from([
                ("地区".into(), Value::Text("华南".into())),
                ("金额".into(), Value::Number(Decimal::from(2))),
            ]),
        ];
        let csv = write_partitions(&rows, "地区", directory.path(), "CSV").unwrap();
        assert_eq!(csv.len(), 2);
        assert!(csv.iter().all(|path| path.exists()));
        let parquet_dir = directory.path().join("parquet");
        let parquet = write_partitions(&rows, "地区", &parquet_dir, "PARQUET").unwrap();
        assert_eq!(parquet.len(), 2);
        assert!(parquet
            .iter()
            .all(|path| path.extension().is_some_and(|v| v == "parquet")));
    }

    fn operation_fixture() -> Table {
        Table {
            rows: vec![
                BTreeMap::from([
                    ("编号".into(), Value::Number(Decimal::ONE)),
                    ("地区".into(), Value::Text("甲".into())),
                    ("类别".into(), Value::Text("一".into())),
                    ("日期".into(), Value::Text("2024-01-02".into())),
                    ("金额".into(), Value::Number(Decimal::from(10))),
                ]),
                BTreeMap::from([
                    ("编号".into(), Value::Number(Decimal::from(2))),
                    ("地区".into(), Value::Text("甲".into())),
                    ("类别".into(), Value::Text("二".into())),
                    ("日期".into(), Value::Text("2024-02-03".into())),
                    ("金额".into(), Value::Number(Decimal::from(20))),
                ]),
                BTreeMap::from([
                    ("编号".into(), Value::Number(Decimal::from(2))),
                    ("地区".into(), Value::Text("乙".into())),
                    ("类别".into(), Value::Text("一".into())),
                    ("日期".into(), Value::Text("2025-03-04".into())),
                    ("金额".into(), Value::Null),
                ]),
            ],
            ..Default::default()
        }
    }

    fn table_rows(value: Value) -> Rows {
        let Value::Table(table) = value else {
            panic!("预期表格")
        };
        let rows = table.borrow().rows.clone();
        rows
    }

    #[test]
    fn relation_window_and_schema_operations_cover_boundaries() {
        let table = operation_fixture();
        let right = Table {
            rows: vec![
                BTreeMap::from([
                    ("编号".into(), Value::Number(Decimal::ONE)),
                    ("地区".into(), Value::Text("右侧".into())),
                    ("备注".into(), Value::Text("命中".into())),
                ]),
                BTreeMap::from([
                    ("编号".into(), Value::Number(Decimal::from(4))),
                    ("备注".into(), Value::Text("新增".into())),
                ]),
            ],
            ..Default::default()
        };

        assert!(join_tables(&table, &right, &[], "内连接").is_err());
        let joined = table_rows(join_tables(&table, &right, &["编号".into()], "全连接").unwrap());
        assert_eq!(joined.len(), 4);
        assert_eq!(joined[0]["地区_右"].to_string(), "右侧");
        assert_eq!(joined[3]["备注"].to_string(), "新增");

        let pivoted = table_rows(
            pivot_table(
                &table,
                &[
                    Value::Text("地区".into()),
                    Value::Text("类别".into()),
                    Value::Text("金额".into()),
                ],
                &BTreeMap::from([("聚合".into(), Value::Text("平均值".into()))]),
            )
            .unwrap(),
        );
        assert_eq!(pivoted.len(), 1);
        assert_eq!(pivoted[0]["一"].to_string(), "10");
        assert!(pivot_table(&table, &[], &BTreeMap::new()).is_err());

        let unpivoted = table_rows(
            unpivot_table(
                &table,
                &[
                    Value::list(vec![Value::Text("编号".into())]),
                    Value::list(vec![Value::Text("金额".into())]),
                ],
            )
            .unwrap(),
        );
        assert_eq!(unpivoted.len(), 3);
        assert!(unpivot_table(&table, &[]).is_err());

        let ranked = table_rows(
            rank_table(
                &table,
                &[Value::Text("金额".into())],
                &BTreeMap::from([
                    ("顺序".into(), Value::Text("升序".into())),
                    ("结果列".into(), Value::Text("名次".into())),
                ]),
            )
            .unwrap(),
        );
        assert!(ranked.iter().all(|row| row.contains_key("名次")));
        assert!(rank_table(&table, &[], &BTreeMap::new()).is_err());

        let rolling = table_rows(
            rolling_table(
                &table,
                &[Value::Text("金额".into()), Value::Number(Decimal::from(2))],
                &BTreeMap::from([("聚合".into(), Value::Text("求和".into()))]),
            )
            .unwrap(),
        );
        assert_eq!(rolling[1]["金额滚动求和"].to_string(), "30");
        assert!(rolling_table(&table, &[], &BTreeMap::new()).is_err());

        let resampled = table_rows(
            resample_table(
                &table,
                &[Value::Text("日期".into()), Value::Text("金额".into())],
                &BTreeMap::from([("频率".into(), Value::Text("年".into()))]),
            )
            .unwrap(),
        );
        assert_eq!(resampled.len(), 1);
        assert_eq!(resampled[0]["金额"].to_string(), "30");
        assert!(resample_table(&table, &[], &BTreeMap::new()).is_err());

        let rules = Value::record(BTreeMap::from([
            ("非空".into(), Value::Bool(true)),
            ("类型".into(), Value::Text("数字".into())),
            ("唯一".into(), Value::Bool(true)),
        ]));
        let schema = Value::record(BTreeMap::from([("金额".into(), rules)]));
        let Value::Record(validation) = validate_table(&table, &[schema]).unwrap() else {
            panic!("预期校验记录")
        };
        assert!(!validation.borrow()["通过"].truth());
        assert!(validate_table(&table, &[]).is_err());
    }

    #[test]
    fn table_method_transformations_and_type_errors_are_explicit() {
        let table = operation_fixture();
        let mut interpreter = Interpreter::new(".", false);

        let selected = table_rows(
            table_method(
                &mut interpreter,
                &table,
                "选择列",
                vec![Value::list(vec![Value::Text("编号".into())])],
                BTreeMap::new(),
                1,
            )
            .unwrap(),
        );
        assert_eq!(selected[0].len(), 1);

        let unique = table_rows(
            table_method(
                &mut interpreter,
                &table,
                "去重",
                vec![Value::list(vec![Value::Text("编号".into())])],
                BTreeMap::new(),
                1,
            )
            .unwrap(),
        );
        assert_eq!(unique.len(), 2);

        let filled = table_rows(
            table_method(
                &mut interpreter,
                &table,
                "填补缺失",
                vec![Value::Number(Decimal::from(99))],
                BTreeMap::new(),
                1,
            )
            .unwrap(),
        );
        assert_eq!(filled[2]["金额"].to_string(), "99");
        let complete = table_rows(
            table_method(
                &mut interpreter,
                &table,
                "删除缺失",
                vec![],
                BTreeMap::new(),
                1,
            )
            .unwrap(),
        );
        assert_eq!(complete.len(), 2);

        let records = table_method(
            &mut interpreter,
            &table,
            "转记录列表",
            vec![],
            BTreeMap::new(),
            1,
        )
        .unwrap();
        assert_eq!(records.iter_values().unwrap().len(), 3);

        let appended = table_rows(
            table_method(
                &mut interpreter,
                &table,
                "纵向合并",
                vec![table_value(vec![BTreeMap::new()])],
                BTreeMap::new(),
                1,
            )
            .unwrap(),
        );
        assert_eq!(appended.len(), 4);

        assert!(table_method(
            &mut interpreter,
            &table,
            "连接",
            vec![Value::Null, Value::Text("编号".into())],
            BTreeMap::new(),
            1,
        )
        .is_err());
        assert!(table_method(
            &mut interpreter,
            &table,
            "纵向合并",
            vec![Value::Null],
            BTreeMap::new(),
            1,
        )
        .is_err());
        assert!(table_method(
            &mut interpreter,
            &table,
            "不存在",
            vec![],
            BTreeMap::new(),
            1,
        )
        .is_err());
    }
}
