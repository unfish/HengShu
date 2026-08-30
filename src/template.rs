//! OOXML template preflight and support-matrix reporting.
//!
//! Preflight is intentionally separate from replacement. It inspects the ZIP
//! package without executing Office content and rejects active/unknown parts
//! before any output file is created.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use zip::read::ZipArchive;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TemplateKind {
    Xlsx,
    Docx,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SupportStatus {
    Supported,
    Unsupported,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SupportFinding {
    pub part: String,
    pub feature: String,
    pub status: SupportStatus,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TemplatePreflight {
    pub version: u32,
    pub kind: TemplateKind,
    pub entries: usize,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub findings: Vec<SupportFinding>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TemplateBudget {
    pub max_entries: usize,
    pub max_compressed_bytes: u64,
    pub max_uncompressed_bytes: u64,
    pub max_xml_depth: usize,
}

impl Default for TemplateBudget {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_compressed_bytes: 128 * 1024 * 1024,
            max_uncompressed_bytes: 512 * 1024 * 1024,
            max_xml_depth: 256,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateError {
    NotOffice,
    Budget(String),
    InvalidPackage(String),
    Rejected(String),
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOffice => formatter.write_str("模板不是受支持的 XLSX/DOCX 文件"),
            Self::Budget(message) | Self::InvalidPackage(message) | Self::Rejected(message) => {
                formatter.write_str(message)
            }
        }
    }
}
impl std::error::Error for TemplateError {}

pub fn preflight(path: &Path, budget: TemplateBudget) -> Result<TemplatePreflight, TemplateError> {
    let kind = match path.extension().and_then(|extension| extension.to_str()) {
        Some("xlsx") => TemplateKind::Xlsx,
        Some("docx") => TemplateKind::Docx,
        _ => return Err(TemplateError::NotOffice),
    };
    let file = File::open(path)
        .map_err(|error| TemplateError::InvalidPackage(format!("读取模板失败：{error}")))?;
    inspect_archive(file, kind, budget)
}

fn inspect_archive<R: Read + Seek>(
    reader: R,
    kind: TemplateKind,
    budget: TemplateBudget,
) -> Result<TemplatePreflight, TemplateError> {
    let mut archive = ZipArchive::new(reader)
        .map_err(|error| TemplateError::InvalidPackage(format!("OOXML 压缩包无效：{error}")))?;
    crate::ooxml::validate_archive(
        &mut archive,
        crate::ooxml::OoxmlBudget {
            max_entries: budget.max_entries,
            max_entry_bytes: budget.max_uncompressed_bytes.min(64 * 1024 * 1024),
            max_total_bytes: budget.max_uncompressed_bytes,
        },
    )
    .map_err(TemplateError::Budget)?;
    let mut compressed_bytes = 0u64;
    let mut uncompressed_bytes = 0u64;
    let mut findings = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| TemplateError::InvalidPackage(error.to_string()))?;
        let name = entry.name().replace('\\', "/");
        if name.split('/').any(|part| part == "..") {
            return Err(TemplateError::Rejected(format!(
                "模板条目路径不安全：{name}"
            )));
        }
        compressed_bytes = compressed_bytes.saturating_add(entry.compressed_size());
        uncompressed_bytes = uncompressed_bytes.saturating_add(entry.size());
        if compressed_bytes > budget.max_compressed_bytes
            || uncompressed_bytes > budget.max_uncompressed_bytes
        {
            return Err(TemplateError::Budget("模板压缩包大小超过预算".into()));
        }
        let lower = name.to_ascii_lowercase();
        let feature = if lower.ends_with("vbaproject.bin") || lower.contains("/vba/") {
            Some(("宏/VBA", SupportStatus::Rejected, "不执行或保留宏内容"))
        } else if lower.contains("externallinks/")
            || lower.contains("oleobject")
            || lower.contains("embeddings/")
        {
            Some((
                "外部链接/OLE",
                SupportStatus::Rejected,
                "模板含主动或外部内容",
            ))
        } else if lower.ends_with("[content_types].xml") || lower.ends_with("_rels/.rels") {
            None
        } else if matches!(kind, TemplateKind::Xlsx)
            && (lower.starts_with("xl/worksheets/")
                || lower == "xl/styles.xml"
                || lower == "xl/workbook.xml")
        {
            Some((
                "XLSX 定点写入",
                SupportStatus::Supported,
                "可在明确目标下处理",
            ))
        } else if matches!(kind, TemplateKind::Docx)
            && (lower == "word/document.xml"
                || lower.starts_with("word/header")
                || lower.starts_with("word/footer"))
        {
            Some((
                "DOCX 定点写入",
                SupportStatus::Supported,
                "可在明确目标下处理",
            ))
        } else {
            Some((
                "未声明对象",
                SupportStatus::Unsupported,
                "不在第一版支持矩阵",
            ))
        };
        if let Some((feature, status, reason)) = feature {
            findings.push(SupportFinding {
                part: name.clone(),
                feature: feature.into(),
                status: status.clone(),
                reason: reason.into(),
            });
            if status == SupportStatus::Rejected {
                return Err(TemplateError::Rejected(format!("模板包含禁止内容：{name}")));
            }
        }
        if name.ends_with(".xml") {
            scan_xml(&mut entry, &name, budget.max_xml_depth)?;
        } else if lower.ends_with(".rels") {
            let content = read_relationships(&mut entry, &name)?;
            scan_xml(content.as_bytes(), &name, budget.max_xml_depth)?;
            if let Some(relationship) = active_relationship(&content) {
                findings.push(SupportFinding {
                    part: name.clone(),
                    feature: "外部链接/OLE".into(),
                    status: SupportStatus::Rejected,
                    reason: "关系声明指向主动或外部内容".into(),
                });
                return Err(TemplateError::Rejected(format!(
                    "模板关系包含禁止内容：{name} -> {relationship}"
                )));
            }
        }
    }
    Ok(TemplatePreflight {
        version: 1,
        kind,
        entries: archive.len(),
        compressed_bytes,
        uncompressed_bytes,
        findings,
    })
}

/// 关系部件很小但仍是不可信输入，按上限读入后再判定，避免为一个 `.rels` 分配无界内存。
fn read_relationships<R: Read>(reader: R, name: &str) -> Result<String, TemplateError> {
    const MAX_RELATIONSHIP_BYTES: u64 = 1024 * 1024;
    let mut content = String::new();
    reader
        .take(MAX_RELATIONSHIP_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|error| TemplateError::InvalidPackage(format!("读取关系部件失败：{error}")))?;
    if content.len() as u64 > MAX_RELATIONSHIP_BYTES {
        return Err(TemplateError::Budget(format!(
            "模板关系部件超过预算：{name}"
        )));
    }
    Ok(content)
}

/// 外部关系的目标不在包内，因此没有对应 zip 条目，只按条目名筛选会漏掉它们；
/// 普通超链接与外部链接共用 `TargetMode="External"`，必须按关系 Type 区分才能不误伤超链接。
fn active_relationship(content: &str) -> Option<String> {
    const ACTIVE_TYPES: [&str; 5] = [
        "/externallink",
        "/oleobject",
        "/package",
        "/subdocument",
        "/frame",
    ];
    let lower = content.to_ascii_lowercase();
    for element in lower.split("<relationship").skip(1) {
        let element = element.split('>').next().unwrap_or(element);
        let relationship_type = attribute(element, "type").unwrap_or_default();
        let active = ACTIVE_TYPES
            .iter()
            .any(|marker| relationship_type.ends_with(marker));
        let external = attribute(element, "targetmode").is_some_and(|mode| mode == "external")
            && !relationship_type.ends_with("/hyperlink");
        if active || external {
            return Some(relationship_type.to_string());
        }
    }
    None
}

fn attribute<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let start = element.find(&format!("{name}=\""))? + name.len() + 2;
    let value = &element[start..];
    value.find('"').map(|end| &value[..end])
}

/// XML 部件按块扫描而不整份载入：sharedStrings 等部件可以很大，但深度和实体声明
/// 都必须在替换前判定，否则预算只能约束字节数而约束不了解析栈。
fn scan_xml<R: Read>(mut reader: R, name: &str, max_depth: usize) -> Result<(), TemplateError> {
    const ENTITY: &[u8] = b"ENTITY";
    let mut buffer = vec![0u8; 64 * 1024];
    let mut state = XmlScan::Text;
    let mut depth = 0usize;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| TemplateError::InvalidPackage(error.to_string()))?;
        if read == 0 {
            return Ok(());
        }
        for &byte in &buffer[..read] {
            state = match state {
                XmlScan::Text => match byte {
                    b'<' => XmlScan::TagStart,
                    _ => XmlScan::Text,
                },
                XmlScan::TagStart => match byte {
                    b'/' => XmlScan::Tag {
                        closing: true,
                        previous: byte,
                    },
                    b'!' => XmlScan::Directive { matched: 0 },
                    b'?' => XmlScan::Instruction,
                    b'>' => XmlScan::Text,
                    _ => XmlScan::Tag {
                        closing: false,
                        previous: byte,
                    },
                },
                XmlScan::Directive { matched } => match byte {
                    b'[' if matched == 0 => XmlScan::Cdata { brackets: 0 },
                    b'-' if matched == 0 => XmlScan::Comment { dashes: 1 },
                    b'>' => XmlScan::Text,
                    _ if byte == ENTITY[matched] => {
                        if matched + 1 == ENTITY.len() {
                            return Err(TemplateError::Rejected(format!(
                                "模板 XML 含实体声明：{name}"
                            )));
                        }
                        XmlScan::Directive {
                            matched: matched + 1,
                        }
                    }
                    _ => XmlScan::Doctype,
                },
                XmlScan::Doctype => match byte {
                    b'>' => XmlScan::Text,
                    b'<' => XmlScan::TagStart,
                    _ => XmlScan::Doctype,
                },
                XmlScan::Instruction => match byte {
                    b'>' => XmlScan::Text,
                    _ => XmlScan::Instruction,
                },
                XmlScan::Comment { dashes } => match byte {
                    b'-' => XmlScan::Comment {
                        dashes: dashes.saturating_add(1),
                    },
                    b'>' if dashes >= 2 => XmlScan::Text,
                    _ => XmlScan::Comment { dashes: 0 },
                },
                XmlScan::Cdata { brackets } => match byte {
                    b']' => XmlScan::Cdata {
                        brackets: brackets.saturating_add(1),
                    },
                    b'>' if brackets >= 2 => XmlScan::Text,
                    _ => XmlScan::Cdata { brackets: 0 },
                },
                XmlScan::Tag { closing, previous } => match byte {
                    b'"' | b'\'' => XmlScan::Quote {
                        closing,
                        quote: byte,
                    },
                    b'>' => {
                        if closing {
                            depth = depth.saturating_sub(1);
                        } else if previous != b'/' {
                            depth += 1;
                            if depth > max_depth {
                                return Err(TemplateError::Budget(format!(
                                    "模板 XML 嵌套深度超过预算：{name}"
                                )));
                            }
                        }
                        XmlScan::Text
                    }
                    _ => XmlScan::Tag {
                        closing,
                        previous: byte,
                    },
                },
                XmlScan::Quote { closing, quote } => {
                    if byte == quote {
                        XmlScan::Tag {
                            closing,
                            previous: byte,
                        }
                    } else {
                        XmlScan::Quote { closing, quote }
                    }
                }
            };
        }
    }
}

enum XmlScan {
    Text,
    TagStart,
    Directive { matched: usize },
    Doctype,
    Instruction,
    Comment { dashes: usize },
    Cdata { brackets: usize },
    Tag { closing: bool, previous: u8 },
    Quote { closing: bool, quote: u8 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    fn package(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn preflight_reports_supported_parts_and_rejects_active_content() {
        let directory = tempdir().unwrap();
        let safe = directory.path().join("safe.xlsx");
        package(
            &safe,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                ("xl/workbook.xml", b"<workbook/>"),
                ("xl/worksheets/sheet1.xml", b"<sheet/>"),
                ("xl/styles.xml", b"<styleSheet/>"),
            ],
        );
        let report = preflight(&safe, TemplateBudget::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.status == SupportStatus::Supported));
        let unsafe_path = directory.path().join("unsafe.xlsx");
        package(&unsafe_path, &[("xl/vbaProject.bin", b"macro")]);
        assert!(matches!(
            preflight(&unsafe_path, TemplateBudget::default()),
            Err(TemplateError::Rejected(_))
        ));
    }

    #[test]
    fn preflight_rejects_camel_cased_external_link_and_ole_parts() {
        let directory = tempdir().unwrap();
        for (name, entry) in [
            ("外链.xlsx", "xl/externalLinks/externalLink1.xml"),
            ("对象.xlsx", "xl/embeddings/oleObject1.bin"),
        ] {
            let path = directory.path().join(name);
            package(
                &path,
                &[("xl/workbook.xml", b"<workbook/>"), (entry, b"payload")],
            );
            assert!(
                matches!(
                    preflight(&path, TemplateBudget::default()),
                    Err(TemplateError::Rejected(_))
                ),
                "{entry} 属于主动内容，必须拒绝"
            );
        }
    }

    #[test]
    fn preflight_rejects_external_relationships_but_keeps_ordinary_hyperlinks() {
        const BASE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let directory = tempdir().unwrap();
        for (name, relationship) in [
            (
                "外链关系.docx",
                format!(
                    "<Relationship Id=\"rId1\" Type=\"{BASE}/externalLink\" Target=\"x.xlsx\"/>"
                ),
            ),
            (
                "对象关系.docx",
                format!(
                    "<Relationship Id=\"rId2\" Type=\"{BASE}/oleObject\" Target=\"http://host/o.bin\" TargetMode=\"External\"/>"
                ),
            ),
            (
                "远程图片.docx",
                format!(
                    "<Relationship Id=\"rId3\" Type=\"{BASE}/image\" Target=\"http://host/p.png\" TargetMode=\"External\"/>"
                ),
            ),
        ] {
            let path = directory.path().join(name);
            let rels = format!("<Relationships>{relationship}</Relationships>");
            package(
                &path,
                &[
                    ("word/document.xml", b"<document/>"),
                    ("word/_rels/document.xml.rels", rels.as_bytes()),
                ],
            );
            assert!(
                matches!(
                    preflight(&path, TemplateBudget::default()),
                    Err(TemplateError::Rejected(_))
                ),
                "{name} 声明了主动或外部关系，必须拒绝"
            );
        }

        let path = directory.path().join("超链接.docx");
        let rels = format!(
            "<Relationships><Relationship Id=\"rId1\" Type=\"{BASE}/hyperlink\" Target=\"https://example.com\" TargetMode=\"External\"/></Relationships>"
        );
        package(
            &path,
            &[
                ("word/document.xml", b"<document/>"),
                ("word/_rels/document.xml.rels", rels.as_bytes()),
            ],
        );
        preflight(&path, TemplateBudget::default()).expect("普通超链接不应被拒绝");
    }

    #[test]
    fn preflight_enforces_entry_and_xml_safety_limits() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("bad.docx");
        package(
            &path,
            &[("word/document.xml", b"<!ENTITY x SYSTEM 'file:///secret'>")],
        );
        assert!(matches!(
            preflight(&path, TemplateBudget::default()),
            Err(TemplateError::Rejected(_))
        ));
        assert!(matches!(
            preflight(
                &path,
                TemplateBudget {
                    max_entries: 0,
                    ..Default::default()
                }
            ),
            Err(TemplateError::Budget(_))
        ));
    }

    #[test]
    fn preflight_enforces_xml_depth_budget_on_shared_strings() {
        let directory = tempdir().unwrap();
        let deep = directory.path().join("deep.xlsx");
        let mut shared = String::new();
        for _ in 0..300 {
            shared.push_str("<si>");
        }
        for _ in 0..300 {
            shared.push_str("</si>");
        }
        package(&deep, &[("xl/sharedStrings.xml", shared.as_bytes())]);
        assert!(matches!(
            preflight(
                &deep,
                TemplateBudget {
                    max_xml_depth: 256,
                    ..Default::default()
                }
            ),
            Err(TemplateError::Budget(_))
        ));
        assert!(preflight(
            &deep,
            TemplateBudget {
                max_xml_depth: 300,
                ..Default::default()
            }
        )
        .is_ok());
    }

    #[test]
    fn preflight_scans_entities_in_large_parts_and_ignores_inert_markup() {
        let directory = tempdir().unwrap();
        let large = directory.path().join("large.xlsx");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"<sst><!DOCTYPE sst [<!ENTITY x SYSTEM 'file:///secret'>]>");
        while bytes.len() <= 5 * 1024 * 1024 {
            bytes.extend_from_slice(b"<si><t>text</t></si>");
        }
        bytes.extend_from_slice(b"</sst>");
        package(&large, &[("xl/sharedStrings.xml", &bytes)]);
        assert!(matches!(
            preflight(&large, TemplateBudget::default()),
            Err(TemplateError::Rejected(_))
        ));
        let inert = directory.path().join("inert.xlsx");
        package(
            &inert,
            &[(
                "xl/sharedStrings.xml",
                b"<sst><!-- <si><t>ENTITY</t> --><si><![CDATA[<t><!ENTITY y 'z'>]]></si></sst>",
            )],
        );
        assert!(preflight(
            &inert,
            TemplateBudget {
                max_xml_depth: 2,
                ..Default::default()
            }
        )
        .is_ok());
    }
}
