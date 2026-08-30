use crate::delivery_template::{
    replace_docx_template_to_pdf, Replacement, ReplacementPlan, TemplateTarget,
};
use crate::security::PermissionSet;
use crate::template::TemplateBudget;
use crate::value::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

pub(super) fn delivery_call(
    name: &str,
    args: Vec<Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    match name {
        "Word转PDF" => {
            let input = super::path::resolve(base_dir, &args[0]);
            let output = super::path::resolve(base_dir, &args[1]);
            let plans = args
                .get(2)
                .map(parse_replacements)
                .transpose()?
                .unwrap_or_default();
            let buffer_limit = permissions.buffer_limit();
            let collection_limit = permissions.collection_limit();
            let report = replace_docx_template_to_pdf(
                &input,
                &output,
                &plans,
                TemplateBudget {
                    max_entries: collection_limit.min(TemplateBudget::default().max_entries),
                    max_compressed_bytes: buffer_limit as u64,
                    max_uncompressed_bytes: buffer_limit as u64,
                    ..TemplateBudget::default()
                },
                crate::delivery::PdfBudget {
                    max_pages: collection_limit.min(100),
                    max_elements: collection_limit,
                    max_text_bytes: buffer_limit,
                    max_image_bytes: buffer_limit,
                    max_output_bytes: permissions.output_limit(),
                },
                crate::evidence::observed_metadata(),
            )?;
            Ok(Value::record(BTreeMap::from([
                ("路径".into(), Value::Path(Rc::new(report.output))),
                ("字节".into(), Value::Number(report.artifact.bytes.into())),
                ("摘要".into(), Value::Text(report.artifact.sha256)),
                ("替换数".into(), Value::Number(report.replaced.into())),
                (
                    "诊断".into(),
                    Value::list(report.diagnostics.into_iter().map(Value::Text).collect()),
                ),
            ])))
        }
        "模板替换" => {
            let input = super::path::resolve(base_dir, &args[0]);
            let output = super::path::resolve(base_dir, &args[1]);
            let plans = parse_template_replacements(&args[2])?;
            let buffer_limit = permissions.buffer_limit();
            let collection_limit = permissions.collection_limit();
            let report = crate::delivery_template::replace_template_with_metadata(
                &input,
                &output,
                &plans,
                TemplateBudget {
                    max_entries: collection_limit.min(TemplateBudget::default().max_entries),
                    max_compressed_bytes: buffer_limit as u64,
                    max_uncompressed_bytes: buffer_limit as u64,
                    ..TemplateBudget::default()
                },
                crate::evidence::observed_metadata(),
            )?;
            Ok(Value::record(BTreeMap::from([
                ("路径".into(), Value::Path(Rc::new(report.output))),
                ("字节".into(), Value::Number(report.artifact.bytes.into())),
                ("摘要".into(), Value::Text(report.artifact.sha256)),
                ("替换数".into(), Value::Number(report.replaced.into())),
                (
                    "诊断".into(),
                    Value::list(report.diagnostics.into_iter().map(Value::Text).collect()),
                ),
            ])))
        }
        _ => Err(format!("交付模块不支持“{name}”")),
    }
}

/// 模板替换的键既可能指 DOCX 占位符，也可能指 XLSX 单元格，按 `工作表!单元格` 形状区分。
fn parse_template_replacements(value: &Value) -> Result<Vec<ReplacementPlan>, String> {
    let mut plans = parse_replacements(value)?;
    for plan in &mut plans {
        let TemplateTarget::DocxPlaceholder { placeholder } = &plan.target else {
            continue;
        };
        if let Some((sheet, cell)) = placeholder.split_once('!') {
            if sheet.is_empty() || cell.is_empty() {
                return Err("模板替换的单元格键必须写成“工作表!单元格”".into());
            }
            plan.target = TemplateTarget::XlsxCell {
                sheet_part: format!("xl/worksheets/{sheet}.xml"),
                cell_ref: cell.to_uppercase(),
            };
        }
    }
    Ok(plans)
}

fn parse_replacements(value: &Value) -> Result<Vec<ReplacementPlan>, String> {
    let Value::Record(values) = value else {
        return Err("Word转PDF 的变量替换必须是记录".into());
    };
    values
        .borrow()
        .iter()
        .map(|(placeholder, value)| {
            if placeholder.trim().is_empty() {
                return Err("Word转PDF 的占位符不能为空".into());
            }
            let replacement = match value {
                Value::Bool(value) => Replacement::Boolean(*value),
                Value::Number(value) => Replacement::Number(value.to_string()),
                Value::Float(value) if value.is_finite() => Replacement::Number(value.to_string()),
                Value::Float(_) => return Err("Word转PDF 不能填充非有限浮点数".into()),
                Value::Record(record) if is_quantity_record(&record.borrow()) => {
                    let record = record.borrow();
                    Replacement::Quantity {
                        value: record["值"].as_decimal()?.to_string(),
                        unit: quantity_unit_identity(&record)?,
                    }
                }
                Value::List(_) | Value::Record(_) | Value::Table(_) => {
                    return Err(format!("Word转PDF 的占位符“{placeholder}”需要标量值"));
                }
                value => Replacement::Text(value.to_string()),
            };
            Ok(ReplacementPlan {
                target: TemplateTarget::DocxPlaceholder {
                    placeholder: placeholder.clone(),
                },
                value: replacement,
            })
        })
        .collect()
}

fn is_quantity_record(record: &BTreeMap<String, Value>) -> bool {
    record.len() == 5
        && ["值", "单位", "维度", "比例", "口径"]
            .iter()
            .all(|key| record.contains_key(*key))
}

fn quantity_unit_identity(
    record: &BTreeMap<String, Value>,
) -> Result<crate::execution::UnitIdentity, String> {
    crate::execution::UnitIdentity::from_decimal_scale(
        record["单位"].to_string(),
        record["维度"].to_string(),
        record["比例"].as_decimal()?,
        record
            .get("口径")
            .filter(|value| !matches!(value, Value::Null))
            .map(ToString::to_string),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    #[test]
    fn replacement_values_preserve_scalar_types() {
        let values = Value::record(BTreeMap::from([
            ("name".into(), Value::Text("Alice".into())),
            ("amount".into(), Value::Number(42.into())),
        ]));
        let plans = parse_replacements(&values).unwrap();
        assert_eq!(plans.len(), 2);
        assert!(plans
            .iter()
            .any(|plan| matches!(plan.value, Replacement::Number(_))));
    }

    #[test]
    fn word_pdf_delivery_is_identical_through_interpreter_and_vm() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("template.docx");
        let file = File::create(&input).unwrap();
        let mut package = ZipWriter::new(file);
        package
            .start_file("[Content_Types].xml", SimpleFileOptions::default())
            .unwrap();
        package.write_all(b"<Types/>").unwrap();
        package
            .start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        package
            .write_all(b"<w:document><w:body><w:p><w:r><w:t>{{name}}</w:t></w:r></w:p></w:body></w:document>")
            .unwrap();
        package.finish().unwrap();

        for (compiled, output) in [(false, "source.pdf"), (true, "vm.pdf")] {
            let source = format!(
                "引入 交付\n定义 结果 为 交付 Word转PDF(\"template.docx\", \"{output}\", {{name: \"Alice\"}})\n输出(结果 的 替换数)"
            );
            let options = crate::api::RunOptions::new(directory.path());
            let result = if compiled {
                crate::api::run_compiled(&source, options)
            } else {
                crate::api::run_source(&source, options)
            }
            .unwrap();
            assert_eq!(result.output, ["1"]);
            assert!(std::fs::read(directory.path().join(output))
                .unwrap()
                .starts_with(b"%PDF-"));
        }
    }

    #[test]
    fn template_replacement_keeps_the_original_package_on_both_paths() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("template.docx");
        let file = File::create(&input).unwrap();
        let mut package = ZipWriter::new(file);
        package
            .start_file("[Content_Types].xml", SimpleFileOptions::default())
            .unwrap();
        package.write_all(b"<Types/>").unwrap();
        package
            .start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        package
            .write_all(b"<w:document><w:body><w:p><w:r><w:t>{{name}}</w:t></w:r></w:p></w:body></w:document>")
            .unwrap();
        package.finish().unwrap();

        for (compiled, output) in [(false, "source.docx"), (true, "vm.docx")] {
            let source = format!(
                "引入 交付\n定义 结果 为 交付 模板替换(\"template.docx\", \"{output}\", {{name: \"Alice\"}})\n输出(结果 的 替换数)"
            );
            let options = crate::api::RunOptions::new(directory.path());
            let result = if compiled {
                crate::api::run_compiled(&source, options)
            } else {
                crate::api::run_source(&source, options)
            }
            .unwrap();
            assert_eq!(result.output, ["1"]);
            assert!(
                std::fs::read(directory.path().join(output))
                    .unwrap()
                    .starts_with(b"PK"),
                "模板替换必须保留原始 OOXML 包"
            );
        }

        assert!(parse_template_replacements(&Value::record(BTreeMap::from([(
            "!A1".into(),
            Value::Text("x".into())
        )])))
        .is_err());
    }
}
