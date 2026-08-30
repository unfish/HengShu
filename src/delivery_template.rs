//! Explicit OOXML template replacement plans.

use crate::delivery::{FontResource, PdfBudget, ReportDocument, ReportElement};
use crate::execution::{Artifact, Metadata};
use crate::template::{preflight, SupportStatus, TemplateBudget, TemplateKind, TemplatePreflight};
use chrono::NaiveDate;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use regex::Regex;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Replacement {
    Text(String),
    Number(String),
    Boolean(bool),
    Date(String),
    Quantity {
        value: String,
        unit: crate::execution::UnitIdentity,
    },
}

impl Replacement {
    fn as_text(&self) -> String {
        match self {
            Self::Text(value) | Self::Number(value) | Self::Date(value) => value.clone(),
            Self::Boolean(value) => value.to_string(),
            Self::Quantity { value, unit } => format!("{value} {}", unit.name),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if let Self::Number(value) | Self::Quantity { value, .. } = self {
            Decimal::from_str(value.trim())
                .map_err(|_| "数值替换必须是合法数字，不能写入公式文本".to_string())?;
        }
        if let Self::Date(value) = self {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| "日期替换必须是 YYYY-MM-DD".to_string())?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateTarget {
    XlsxCell {
        sheet_part: String,
        cell_ref: String,
    },
    XlsxNamedRange {
        name: String,
    },
    /// A zero-based offset inside an OOXML table reference, resolved from the
    /// table's declared name and column metadata.
    XlsxTableAnchor {
        table_name: String,
        column_name: String,
        row_offset: usize,
    },
    /// Repeat a formatted sample row. `row_offset` is zero based and `column`
    /// is a one-based Excel column number.
    XlsxSampleRow {
        sheet_part: String,
        sample_row: usize,
        row_offset: usize,
        column: usize,
    },
    DocxPlaceholder {
        placeholder: String,
    },
    DocxContentControl {
        tag: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementPlan {
    pub target: TemplateTarget,
    pub value: Replacement,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementReport {
    pub preflight: TemplatePreflight,
    pub replaced: usize,
    pub output: PathBuf,
    pub artifact: Artifact,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocxPdfReport {
    pub preflight: TemplatePreflight,
    pub replaced: usize,
    pub output: PathBuf,
    pub artifact: Artifact,
    pub diagnostics: Vec<String>,
}

/// Fill a DOCX template and export its basic visible document structure as PDF.
///
/// This path is self-contained: it does not invoke Office, a print driver, or an
/// external converter. Paragraphs, page breaks, headers, footers, and tables are
/// preserved; unsupported complex Word objects are reported in `diagnostics`.
mod docx;
use docx::replace_docx;
pub use docx::{basic_docx_pdf_font, docx_to_report_document, replace_docx_template_to_pdf};
fn validate_template_archive(archive: &mut ZipArchive<File>) -> Result<(), String> {
    crate::ooxml::validate_archive(
        archive,
        crate::ooxml::OoxmlBudget {
            max_entries: 10_000,
            max_entry_bytes: 64 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
        },
    )
}

pub fn replace_template(
    input: &Path,
    output: &Path,
    plans: &[ReplacementPlan],
    budget: TemplateBudget,
) -> Result<ReplacementReport, String> {
    replace_template_with_metadata(input, output, plans, budget, Metadata::default())
}

/// 模板替换的专用来源节点：记录模板、输出和被写入的目标。
pub fn record_provenance(
    graph: &mut crate::provenance::ProvenanceGraph,
    input: &Path,
    report: &ReplacementReport,
    plans: &[ReplacementPlan],
    budget: crate::provenance::GraphBudget,
) -> Result<String, crate::provenance::ObservationError> {
    let targets = plans
        .iter()
        .map(|plan| format!("{:?}", plan.target))
        .collect::<Vec<_>>();
    graph.add_template(
        &input.display().to_string(),
        &report.output.display().to_string(),
        &targets,
        report
            .artifact
            .metadata
            .provenance
            .iter()
            .map(|reference| reference.source_id.clone()),
        report.artifact.metadata.clone(),
        budget,
    )
}

pub fn replace_template_with_metadata(
    input: &Path,
    output: &Path,
    plans: &[ReplacementPlan],
    budget: TemplateBudget,
    metadata: Metadata,
) -> Result<ReplacementReport, String> {
    let preflight_report = preflight(input, budget).map_err(|error| error.to_string())?;
    if plans.is_empty() {
        return Err("模板替换计划不能为空".into());
    }
    crate::privacy::guard_endpoint(crate::privacy::Endpoint::LocalFile, &metadata.sensitivity)?;
    match preflight_report.kind {
        TemplateKind::Xlsx => replace_xlsx(input, output, plans, preflight_report, metadata),
        TemplateKind::Docx => replace_docx(input, output, plans, preflight_report, metadata),
    }
}

mod xlsx;
use xlsx::{cell_reference, read_zip_entry, replace_xlsx};
#[cfg(test)]
use xlsx::{
    expand_xlsx_table_xml, force_xlsx_recalculation_xml, rewrite_xlsx_row, translate_formula_rows,
};
fn template_replacement_metadata(mut metadata: Metadata, plans: &[ReplacementPlan]) -> Metadata {
    for plan in plans {
        let Replacement::Quantity { unit, .. } = &plan.value else {
            continue;
        };
        let target = match &plan.target {
            TemplateTarget::XlsxCell {
                sheet_part,
                cell_ref,
            } => format!("{sheet_part}!{cell_ref}"),
            TemplateTarget::XlsxNamedRange { name } => format!("named:{name}"),
            TemplateTarget::XlsxTableAnchor {
                table_name,
                column_name,
                row_offset,
            } => format!("table:{table_name}.{column_name}[{row_offset}]"),
            TemplateTarget::XlsxSampleRow {
                sheet_part,
                sample_row,
                row_offset,
                column,
            } => format!(
                "{sheet_part}!{}",
                cell_reference(*column, sample_row.saturating_add(*row_offset))
            ),
            TemplateTarget::DocxPlaceholder { placeholder } => format!("placeholder:{placeholder}"),
            TemplateTarget::DocxContentControl { tag } => format!("control:{tag}"),
        };
        metadata.units.insert(target, unit.clone());
    }
    metadata
}

fn rewrite_package(
    input: &Path,
    output: &Path,
    mut transform: impl FnMut(&str, &[u8]) -> Result<Vec<u8>, String>,
) -> Result<usize, String> {
    let input_file = File::open(input).map_err(|error| format!("读取模板失败：{error}"))?;
    let mut archive =
        ZipArchive::new(input_file).map_err(|error| format!("读取模板压缩包失败：{error}"))?;
    validate_template_archive(&mut archive)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut output = crate::security::AtomicOutput::new(output)?;
    let mut writer = ZipWriter::new(output.file_mut());
    let mut replaced = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().to_owned();
        let bytes = crate::ooxml::read_bytes_limited(&mut entry, &name, 64 * 1024 * 1024)?;
        let transformed = transform(&name, &bytes)?;
        if transformed != bytes {
            replaced += 1;
        }
        writer
            .start_file(name, SimpleFileOptions::default())
            .map_err(|error| error.to_string())?;
        writer
            .write_all(&transformed)
            .map_err(|error| error.to_string())?;
    }
    writer.finish().map_err(|error| error.to_string())?;
    output.commit(true)?;
    Ok(replaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    fn package(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn xlsx_cell_replacement_keeps_unrelated_entries() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.xlsx");
        let output = directory.path().join("out.xlsx");
        package(&input, &[("[Content_Types].xml", b"<Types/>"), ("xl/workbook.xml", b"<workbook/>"), ("xl/worksheets/sheet1.xml", br#"<worksheet><sheetData><row><c r="B2" s="7" t="str"><v>old</v></c></row></sheetData></worksheet>"#), ("xl/styles.xml", b"styles")]);
        let report = replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::XlsxCell {
                    sheet_part: "xl/worksheets/sheet1.xml".into(),
                    cell_ref: "B2".into(),
                },
                value: Replacement::Text("new".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        assert_eq!(report.replaced, 1);
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("new"));
        assert!(sheet.contains(r#"s="7""#));
        let mut styles = String::new();
        archive
            .by_name("xl/styles.xml")
            .unwrap()
            .read_to_string(&mut styles)
            .unwrap();
        assert_eq!(styles, "styles");
    }

    #[test]
    fn xlsx_named_range_resolves_to_the_declared_cell() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.xlsx");
        let output = directory.path().join("out.xlsx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>") ,
                (
                    "xl/workbook.xml",
                    br#"<workbook><sheets><sheet name="Report" r:id="rId1"/></sheets><definedNames><definedName name="ReportDate">'Report'!$B$2</definedName></definedNames></workbook>"#,
                ),
                (
                    "xl/_rels/workbook.xml.rels",
                    br#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#,
                ),
                (
                    "xl/worksheets/sheet1.xml",
                    br#"<worksheet><c r="B2"><v>old</v></c></worksheet>"#,
                ),
            ],
        );
        replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::XlsxNamedRange {
                    name: "ReportDate".into(),
                },
                value: Replacement::Text("2026-08-23".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("2026-08-23"));
    }

    #[test]
    fn xlsx_table_anchor_resolves_named_column_and_data_row() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.xlsx");
        let output = directory.path().join("out.xlsx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "xl/worksheets/sheet1.xml",
                    br#"<worksheet><tableParts><tablePart r:id="rId1"/></tableParts><c r="B3"><v>old</v></c></worksheet>"#,
                ),
                (
                    "xl/worksheets/_rels/sheet1.xml.rels",
                    br#"<Relationships><Relationship Id="rId1" Target="../tables/table1.xml"/></Relationships>"#,
                ),
                (
                    "xl/tables/table1.xml",
                    br#"<table name="Sales" ref="A1:B4"><tableColumns><tableColumn id="1" name="Date"/><tableColumn id="2" name="Amount"/></tableColumns></table>"#,
                ),
            ],
        );
        replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::XlsxTableAnchor {
                    table_name: "Sales".into(),
                    column_name: "Amount".into(),
                    row_offset: 1,
                },
                value: Replacement::Number("42".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("r=\"B3\""));
        assert!(sheet.contains(">42<"));
    }

    #[test]
    fn xlsx_sample_row_repeats_styles_and_translates_relative_formulas() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("sample.xlsx");
        let output = directory.path().join("out.xlsx");
        package(&input, &[
            ("[Content_Types].xml", b"<Types/>"),
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/worksheets/sheet1.xml", br#"<worksheet><sheetData><row r="2" s="5"><c r="A2" s="7"><v>old</v></c><c r="C2" s="8"><f>A2*$C$1</f><v>0</v></c></row><row r="3"><c r="A3"><f>A2</f><v>0</v></c></row></sheetData></worksheet>"#),
        ]);
        replace_template(
            &input,
            &output,
            &[
                ReplacementPlan {
                    target: TemplateTarget::XlsxSampleRow {
                        sheet_part: "xl/worksheets/sheet1.xml".into(),
                        sample_row: 2,
                        row_offset: 0,
                        column: 1,
                    },
                    value: Replacement::Text("甲".into()),
                },
                ReplacementPlan {
                    target: TemplateTarget::XlsxSampleRow {
                        sheet_part: "xl/worksheets/sheet1.xml".into(),
                        sample_row: 2,
                        row_offset: 1,
                        column: 1,
                    },
                    value: Replacement::Text("乙".into()),
                },
            ],
            TemplateBudget::default(),
        )
        .unwrap();
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("r=\"3\" s=\"5\""));
        assert!(sheet.contains("A3*$C$1"));
        assert!(sheet.contains("<row r=\"4\""));
        assert!(sheet.contains(">甲<") && sheet.contains(">乙<"));
    }

    #[test]
    fn xlsx_date_replacement_obeys_1900_and_1904_boundaries() {
        for (date1904, date, expected) in [(false, "1900-03-01", "61"), (true, "1904-01-02", "1")] {
            let directory = tempdir().unwrap();
            let input = directory.path().join("date.xlsx");
            let output = directory.path().join("out.xlsx");
            let workbook = if date1904 {
                b"<workbook><workbookPr date1904=\"1\"/></workbook>".as_slice()
            } else {
                b"<workbook/>".as_slice()
            };
            package(&input, &[("[Content_Types].xml", b"<Types/>"), ("xl/workbook.xml", workbook), ("xl/worksheets/sheet1.xml", br#"<worksheet><sheetData><row r="2"><c r="A2" s="4"><v>0</v></c></row></sheetData></worksheet>"#)]);
            replace_template(
                &input,
                &output,
                &[ReplacementPlan {
                    target: TemplateTarget::XlsxCell {
                        sheet_part: "xl/worksheets/sheet1.xml".into(),
                        cell_ref: "A2".into(),
                    },
                    value: Replacement::Date(date.into()),
                }],
                TemplateBudget::default(),
            )
            .unwrap();
            let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
            let mut sheet = String::new();
            archive
                .by_name("xl/worksheets/sheet1.xml")
                .unwrap()
                .read_to_string(&mut sheet)
                .unwrap();
            assert!(sheet.contains(&format!(">{expected}<")), "{sheet}");
            assert!(sheet.contains("s=\"4\""));
        }
    }

    #[test]
    fn xlsx_table_reference_expands_with_sample_row_copy() {
        let xml = br#"<table ref="A1:C2"><autoFilter ref="A1:C2"/></table>"#;
        let expanded = expand_xlsx_table_xml(xml, 2, 3).unwrap();
        let text = String::from_utf8(expanded).unwrap();
        assert!(text.contains("ref=\"A1:C4\""));
    }

    #[test]
    fn xlsx_formula_copy_drops_cache_and_forces_recalculation() {
        let row = br#"<row r="2"><c r="A2"><f>A1+1</f><v>99</v></c></row>"#;
        let copied = String::from_utf8(rewrite_xlsx_row(row, 3, 1).unwrap()).unwrap();
        assert!(copied.contains("A2+1"));
        assert!(!copied.contains("99"));
        let workbook =
            force_xlsx_recalculation_xml(b"<workbook><calcPr calcId=\"1\"/></workbook>").unwrap();
        let workbook = String::from_utf8(workbook).unwrap();
        assert!(workbook.contains("calcMode=\"auto\""));
        assert!(workbook.contains("forceFullCalc=\"1\""));
        assert_eq!(
            translate_formula_rows("LOG10(A2)+Sheet1!B2+\"A2\"", 1).unwrap(),
            "LOG10(A3)+Sheet1!B3+\"A2\""
        );
    }

    #[test]
    fn quantity_replacements_are_recorded_on_docx_artifacts() {
        let unit = crate::execution::UnitIdentity {
            name: "万元".into(),
            dimension: "货币".into(),
            scale_numerator: 10000,
            scale_denominator: 1,
            basis: Some("CNY".into()),
        };
        let metadata = template_replacement_metadata(
            Metadata::default(),
            &[ReplacementPlan {
                target: TemplateTarget::DocxPlaceholder {
                    placeholder: "amount".into(),
                },
                value: Replacement::Quantity {
                    value: "2".into(),
                    unit: unit.clone(),
                },
            }],
        );
        assert_eq!(metadata.units["placeholder:amount"], unit);
    }

    #[test]
    fn docx_placeholder_replacement_is_explicit() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.docx");
        let output = directory.path().join("out.docx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "word/document.xml",
                    br#"<document><w:p><w:r><w:t>{{name}}</w:t></w:r></w:p></document>"#,
                ),
            ],
        );
        replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::DocxPlaceholder {
                    placeholder: "name".into(),
                },
                value: Replacement::Text("Alice".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut document = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut document)
            .unwrap();
        assert!(document.contains("Alice"));
    }

    #[test]
    fn docx_placeholder_can_span_multiple_text_runs() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.docx");
        let output = directory.path().join("out.docx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "word/document.xml",
                    br#"<document><w:r><w:t>{{na</w:t></w:r><w:r><w:t>me}}</w:t></w:r></document>"#,
                ),
            ],
        );
        replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::DocxPlaceholder {
                    placeholder: "name".into(),
                },
                value: Replacement::Text("Alice".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut document = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut document)
            .unwrap();
        assert!(document.contains("Alice"));
        assert!(!document.contains("{{na"));
    }

    #[test]
    fn docx_content_control_replacement_preserves_control_wrapper() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.docx");
        let output = directory.path().join("out.docx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "word/document.xml",
                    br#"<document><w:sdt><w:sdtPr><w:tag w:val="amount"/></w:sdtPr><w:sdtContent><w:r><w:t>old</w:t></w:r></w:sdtContent></w:sdt></document>"#,
                ),
            ],
        );
        replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::DocxContentControl {
                    tag: "amount".into(),
                },
                value: Replacement::Number("42".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut document = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut document)
            .unwrap();
        assert!(document.contains("w:tag w:val=\"amount\""));
        assert!(document.contains(">42</w:t>"));
    }

    #[test]
    fn filled_docx_exports_basic_paragraphs_tables_and_headers_to_pdf() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.docx");
        let output = directory.path().join("out.pdf");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "word/document.xml",
                    "<w:document><w:body><w:p><w:r><w:t>月报：{{month}}</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>项目</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>金额</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>收入</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:drawing/></w:r></w:p></w:body></w:document>".as_bytes(),
                ),
                (
                    "word/header1.xml",
                    "<w:hdr><w:p><w:r><w:t>经营报告</w:t></w:r></w:p></w:hdr>".as_bytes(),
                ),
            ],
        );
        let mut metadata = Metadata::default();
        metadata.provenance.push(crate::execution::ProvenanceRef {
            source_id: "ledger".into(),
            location: Some("sheet:1".into()),
            step_id: None,
        });
        let report = replace_docx_template_to_pdf(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::DocxPlaceholder {
                    placeholder: "month".into(),
                },
                value: Replacement::Text("2026-08".into()),
            }],
            TemplateBudget::default(),
            PdfBudget::default(),
            metadata.clone(),
        )
        .unwrap();
        let pdf = fs::read(&output).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert_eq!(report.artifact.metadata, metadata);
        assert_eq!(report.artifact.kind, "application/pdf");
        assert!(report
            .diagnostics
            .iter()
            .any(|message| message.contains("drawing")));

        let filled = directory.path().join("filled.docx");
        replace_template(
            &input,
            &filled,
            &[ReplacementPlan {
                target: TemplateTarget::DocxPlaceholder {
                    placeholder: "month".into(),
                },
                value: Replacement::Text("2026-08".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        let (document, _) =
            docx_to_report_document(&filled, basic_docx_pdf_font(), Metadata::default()).unwrap();
        assert_eq!(document.header.as_deref(), Some("经营报告"));
        assert!(matches!(
            document.elements[0],
            ReportElement::Paragraph { .. }
        ));
        assert!(matches!(document.elements[1], ReportElement::Table { .. }));
    }

    #[test]
    fn docx_pdf_export_rejects_non_pdf_destinations() {
        let error = replace_docx_template_to_pdf(
            Path::new("in.docx"),
            Path::new("out.docx"),
            &[],
            TemplateBudget::default(),
            PdfBudget::default(),
            Metadata::default(),
        )
        .unwrap_err();
        assert!(error.contains(".pdf"));
    }

    #[test]
    fn text_replacement_cannot_inject_formula_or_xml_markup() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.xlsx");
        let output = directory.path().join("out.xlsx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "xl/worksheets/sheet1.xml",
                    br#"<worksheet><c r="A1"><v>old</v></c></worksheet>"#,
                ),
            ],
        );
        replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::XlsxCell {
                    sheet_part: "xl/worksheets/sheet1.xml".into(),
                    cell_ref: "A1".into(),
                },
                value: Replacement::Text("=1+1 & <x>".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        let mut archive = ZipArchive::new(File::open(output).unwrap()).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("=1+1"));
        assert!(sheet.contains("&lt;x&gt;"));
        assert!(!sheet.contains("<x>"));
    }

    #[test]
    fn template_artifact_preserves_supplied_metadata() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.xlsx");
        let output = directory.path().join("out.xlsx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "xl/worksheets/sheet1.xml",
                    br#"<worksheet><c r="A1"><v>old</v></c></worksheet>"#,
                ),
            ],
        );
        let mut metadata = Metadata::default();
        metadata
            .sensitivity
            .labels
            .insert(crate::execution::SensitiveLabel::Financial);
        let report = replace_template_with_metadata(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::XlsxCell {
                    sheet_part: "xl/worksheets/sheet1.xml".into(),
                    cell_ref: "A1".into(),
                },
                value: Replacement::Text("new".into()),
            }],
            TemplateBudget::default(),
            metadata.clone(),
        )
        .unwrap();
        assert_eq!(report.artifact.metadata, metadata);
    }

    #[test]
    fn duplicate_template_targets_fail_and_duplicate_placeholders_are_reported() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("in.docx");
        let output = directory.path().join("out.docx");
        package(
            &input,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "word/document.xml",
                    br#"<document><w:t>{{name}}</w:t><w:t>{{name}}</w:t></document>"#,
                ),
            ],
        );
        let report = replace_template(
            &input,
            &output,
            &[ReplacementPlan {
                target: TemplateTarget::DocxPlaceholder {
                    placeholder: "name".into(),
                },
                value: Replacement::Text("A".into()),
            }],
            TemplateBudget::default(),
        )
        .unwrap();
        assert!(!report.diagnostics.is_empty());
        let xlsx = directory.path().join("in.xlsx");
        package(
            &xlsx,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                (
                    "xl/worksheets/sheet1.xml",
                    br#"<worksheet><c r="A1"><v>old</v></c></worksheet>"#,
                ),
            ],
        );
        let duplicate = vec![
            ReplacementPlan {
                target: TemplateTarget::XlsxCell {
                    sheet_part: "xl/worksheets/sheet1.xml".into(),
                    cell_ref: "A1".into(),
                },
                value: Replacement::Text("a".into()),
            },
            ReplacementPlan {
                target: TemplateTarget::XlsxCell {
                    sheet_part: "xl/worksheets/sheet1.xml".into(),
                    cell_ref: "A1".into(),
                },
                value: Replacement::Text("b".into()),
            },
        ];
        assert!(replace_template(
            &xlsx,
            &directory.path().join("out.xlsx"),
            &duplicate,
            TemplateBudget::default()
        )
        .is_err());
    }
}
