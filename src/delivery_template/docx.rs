//! DOCX parsing, replacement, and self-contained PDF conversion.

use super::*;

pub fn replace_docx_template_to_pdf(
    input: &Path,
    output: &Path,
    plans: &[ReplacementPlan],
    template_budget: TemplateBudget,
    pdf_budget: PdfBudget,
    metadata: Metadata,
) -> Result<DocxPdfReport, String> {
    if !output
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
    {
        return Err("DOCX 转换输出必须使用 .pdf 扩展名".into());
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| format!("创建 PDF 输出目录失败：{error}"))?;
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("report");
    let temporary_docx = parent.join(format!(".{stem}.{}.衡枢转换.docx", std::process::id()));
    let temporary_pdf = parent.join(format!(".{stem}.{}.衡枢转换.pdf", std::process::id()));
    let result = (|| {
        let (replacement_preflight, replaced, replacement_diagnostics) = if plans.is_empty() {
            let preflight = preflight(input, template_budget).map_err(|error| error.to_string())?;
            if preflight.kind != TemplateKind::Docx {
                return Err("只有 DOCX 模板可以直接转换为 PDF".into());
            }
            fs::copy(input, &temporary_docx)
                .map_err(|error| format!("复制 DOCX 转换输入失败：{error}"))?;
            (preflight, 0, Vec::new())
        } else {
            let replacement = replace_template_with_metadata(
                input,
                &temporary_docx,
                plans,
                template_budget,
                metadata.clone(),
            )?;
            (
                replacement.preflight,
                replacement.replaced,
                replacement.diagnostics,
            )
        };
        if replacement_preflight.kind != TemplateKind::Docx {
            return Err("只有 DOCX 模板可以直接转换为 PDF".into());
        }
        let (document, mut diagnostics) =
            docx_to_report_document(&temporary_docx, basic_docx_pdf_font(), metadata.clone())?;
        diagnostics.extend(replacement_diagnostics);
        diagnostics.extend(
            replacement_preflight
                .findings
                .iter()
                .filter(|finding| finding.status == SupportStatus::Unsupported)
                .map(|finding| {
                    format!(
                        "DOCX 转 PDF 未转换模板对象：{}（{}）",
                        finding.part, finding.reason
                    )
                }),
        );
        let bytes = document
            .render_pdf(&pdf_budget)
            .map_err(|error| error.to_string())?;
        crate::security::atomic_write_file(output, &bytes, true)?;
        let mut artifact = Artifact::from_bytes(output, "application/pdf", &bytes);
        artifact.purpose = "Word 模板 PDF 交付".into();
        artifact.metadata = template_replacement_metadata(metadata, plans);
        Ok(DocxPdfReport {
            preflight: replacement_preflight,
            replaced,
            output: output.to_path_buf(),
            artifact,
            diagnostics,
        })
    })();
    let _ = fs::remove_file(&temporary_docx);
    let _ = fs::remove_file(&temporary_pdf);
    result
}

pub fn basic_docx_pdf_font() -> FontResource {
    FontResource {
        name: crate::delivery::DEFAULT_PDF_FONT_NAME.into(),
        bytes: crate::delivery::DEFAULT_PDF_FONT_BYTES.to_vec(),
        embedded: true,
    }
}

pub fn docx_to_report_document(
    input: &Path,
    font: FontResource,
    metadata: Metadata,
) -> Result<(ReportDocument, Vec<String>), String> {
    let file = File::open(input).map_err(|error| format!("读取 DOCX 转换输入失败：{error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("读取 DOCX 失败：{error}"))?;
    validate_template_archive(&mut archive)?;
    let document = read_zip_entry(&mut archive, "word/document.xml")?;
    let (elements, diagnostics) = docx_xml_to_elements(&document)?;
    if elements.is_empty() {
        return Err("DOCX 没有可转换的正文内容".into());
    }
    let header = read_docx_text_parts(&mut archive, "word/header")?;
    let footer = read_docx_text_parts(&mut archive, "word/footer")?;
    Ok((
        ReportDocument {
            version: Default::default(),
            page_size: crate::delivery::PageSize::A4,
            margin: 36,
            font,
            header: (!header.is_empty()).then_some(header),
            footer: (!footer.is_empty()).then_some(footer),
            elements,
            metadata,
        },
        diagnostics,
    ))
}

fn read_docx_text_parts(archive: &mut ZipArchive<File>, prefix: &str) -> Result<String, String> {
    let mut names = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .map(|entry| entry.name().to_owned())
        })
        .filter(|name| name.starts_with(prefix) && name.ends_with(".xml"))
        .collect::<Vec<_>>();
    names.sort();
    let mut lines = Vec::new();
    for name in names {
        let xml = read_zip_entry(archive, &name)?;
        let (elements, _) = docx_xml_to_elements(&xml)?;
        for element in elements {
            if let ReportElement::Paragraph { text, .. } = element {
                if !text.is_empty() {
                    lines.push(text);
                }
            }
        }
    }
    Ok(lines.join("\n"))
}

fn docx_xml_to_elements(xml: &str) -> Result<(Vec<ReportElement>, Vec<String>), String> {
    let mut reader = Reader::from_str(xml);
    let mut buffer = Vec::new();
    let mut elements = Vec::new();
    let mut diagnostics = Vec::new();
    let mut paragraph = String::new();
    let mut paragraph_depth = 0usize;
    let mut in_text = false;
    let mut table_depth = 0usize;
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;
    let mut table_rows = Vec::new();
    let mut page_break_after_paragraph = false;
    let mut unsupported = BTreeMap::<String, usize>::new();
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("解析 DOCX 转换内容失败：{error}"))?
        {
            Event::Start(start) => match start.local_name().as_ref() {
                b"p" => {
                    paragraph_depth += 1;
                    if paragraph_depth == 1 {
                        paragraph.clear();
                        page_break_after_paragraph = false;
                    }
                }
                b"t" => in_text = true,
                b"tbl" => {
                    table_depth += 1;
                    if table_depth == 1 {
                        table_rows.clear();
                    }
                }
                b"tr" if table_depth == 1 => row.clear(),
                b"tc" if table_depth == 1 => {
                    cell.clear();
                    in_cell = true;
                }
                b"br" => {
                    if start.attributes().flatten().any(|attribute| {
                        attribute.key.local_name().as_ref() == b"type"
                            && attribute.value.as_ref() == b"page"
                    }) {
                        page_break_after_paragraph = true;
                    } else {
                        paragraph.push('\n');
                    }
                }
                b"tab" => paragraph.push('\t'),
                b"drawing" | b"pict" | b"object" | b"altChunk" | b"txbxContent" => {
                    *unsupported
                        .entry(String::from_utf8_lossy(start.local_name().as_ref()).into_owned())
                        .or_default() += 1;
                }
                _ => {}
            },
            Event::Empty(start) => match start.local_name().as_ref() {
                b"br" => {
                    if start.attributes().flatten().any(|attribute| {
                        attribute.key.local_name().as_ref() == b"type"
                            && attribute.value.as_ref() == b"page"
                    }) {
                        page_break_after_paragraph = true;
                    } else {
                        paragraph.push('\n');
                    }
                }
                b"tab" => paragraph.push('\t'),
                b"drawing" | b"pict" | b"object" | b"altChunk" | b"txbxContent" => {
                    *unsupported
                        .entry(String::from_utf8_lossy(start.local_name().as_ref()).into_owned())
                        .or_default() += 1;
                }
                _ => {}
            },
            Event::Text(text) if in_text => {
                let decoded = text
                    .decode()
                    .map_err(|error| format!("解码 DOCX 文本失败：{error}"))?;
                let unescaped = quick_xml::escape::unescape(&decoded)
                    .map_err(|error| format!("解析 DOCX 文本实体失败：{error}"))?;
                paragraph.push_str(&unescaped);
            }
            Event::End(end) => match end.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => {
                    paragraph_depth = paragraph_depth.saturating_sub(1);
                    if paragraph_depth == 0 {
                        let text = paragraph.trim_end().to_string();
                        if in_cell {
                            if !cell.is_empty() && !text.is_empty() {
                                cell.push('\n');
                            }
                            cell.push_str(&text);
                        } else if !text.is_empty() {
                            elements.push(ReportElement::Paragraph {
                                text,
                                font_size: 10,
                            });
                        }
                        if page_break_after_paragraph && !in_cell {
                            elements.push(ReportElement::PageBreak);
                        }
                    }
                }
                b"tc" if table_depth == 1 => {
                    row.push(cell.clone());
                    in_cell = false;
                }
                b"tr" if table_depth == 1 => {
                    if !row.is_empty() {
                        table_rows.push(row.clone());
                    }
                }
                b"tbl" => {
                    if table_depth == 1 && !table_rows.is_empty() {
                        let width = table_rows.iter().map(Vec::len).max().unwrap_or(0);
                        if width > 0 {
                            for row in &mut table_rows {
                                row.resize(width, String::new());
                            }
                            let headers = table_rows.remove(0);
                            elements.push(ReportElement::Table {
                                headers,
                                rows: table_rows.clone(),
                            });
                        }
                    }
                    table_depth = table_depth.saturating_sub(1);
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    diagnostics.extend(
        unsupported
            .into_iter()
            .map(|(kind, count)| format!("DOCX 转 PDF 忽略不支持的对象：{kind}（{count} 个）")),
    );
    Ok((elements, diagnostics))
}

pub(super) fn replace_docx(
    input: &Path,
    output: &Path,
    plans: &[ReplacementPlan],
    preflight_report: TemplatePreflight,
    metadata: Metadata,
) -> Result<ReplacementReport, String> {
    let mut replacements = BTreeMap::new();
    let mut controls = BTreeMap::new();
    for plan in plans {
        let placeholder = match &plan.target {
            TemplateTarget::DocxPlaceholder { placeholder } => Some(placeholder),
            TemplateTarget::DocxContentControl { tag } => {
                if tag.is_empty() {
                    return Err("DOCX 内容控件标签不能为空".into());
                }
                plan.value.validate()?;
                if controls.insert(tag.clone(), plan.value.as_text()).is_some() {
                    return Err(format!("DOCX 内容控件重复：{tag}"));
                }
                None
            }
            _ => return Err("DOCX 模板计划包含非 DOCX 目标".into()),
        };
        let Some(placeholder) = placeholder else {
            continue;
        };
        if placeholder.is_empty() {
            return Err("DOCX 占位符不能为空".into());
        }
        plan.value.validate()?;
        replacements.insert(placeholder.clone(), plan.value.as_text());
    }
    let mut replaced = 0;
    let mut diagnostics = Vec::new();
    rewrite_package(input, output, |name, bytes| {
        if name != "word/document.xml"
            && !name.starts_with("word/header")
            && !name.starts_with("word/footer")
        {
            return Ok(bytes.to_vec());
        }
        let source_text =
            String::from_utf8(bytes.to_vec()).map_err(|_| "DOCX XML 不是 UTF-8".to_string())?;
        for placeholder in replacements.keys() {
            if source_text
                .matches(&format!("{{{{{placeholder}}}}}"))
                .count()
                > 1
            {
                diagnostics.push(format!("DOCX 占位符重复：{placeholder}"));
            }
        }
        let (text, count) = replace_docx_xml(bytes, &replacements)?;
        let (text, control_count) = replace_docx_controls(&text, &controls)?;
        replaced += count + control_count;
        Ok(text)
    })?;
    if replaced == 0 {
        return Err(format!(
            "DOCX 未找到任何声明的占位符：{}",
            replacements.keys().cloned().collect::<Vec<_>>().join(",")
        ));
    }
    let artifact = Artifact::observe_file(
        output,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    .map_err(|error| error.to_string())
    .map(|mut artifact| {
        artifact.metadata = template_replacement_metadata(metadata.clone(), plans);
        artifact.purpose = "模板文件交付".into();
        artifact
    })?;
    Ok(ReplacementReport {
        preflight: preflight_report,
        replaced,
        output: output.to_path_buf(),
        artifact,
        diagnostics,
    })
}

fn replace_docx_controls(
    bytes: &[u8],
    controls: &BTreeMap<String, String>,
) -> Result<(Vec<u8>, usize), String> {
    if controls.is_empty() {
        return Ok((bytes.to_vec(), 0));
    }
    let mut source =
        String::from_utf8(bytes.to_vec()).map_err(|_| "DOCX XML 不是 UTF-8".to_string())?;
    let mut replaced = 0;
    for (tag, value) in controls {
        let marker = format!("w:val=\"{tag}\"");
        let Some(tag_pos) = source.find(&marker) else {
            continue;
        };
        let Some(content_start) = source[tag_pos..]
            .find("<w:sdtContent")
            .map(|offset| tag_pos + offset)
        else {
            continue;
        };
        let Some(open_end) = source[content_start..]
            .find('>')
            .map(|offset| content_start + offset + 1)
        else {
            continue;
        };
        let Some(content_end) = source[open_end..]
            .find("</w:sdtContent>")
            .map(|offset| open_end + offset)
        else {
            continue;
        };
        let region = &source[open_end..content_end];
        let Some(text_start) = region.find("<w:t") else {
            continue;
        };
        let text_open_start = open_end + text_start;
        let Some(text_open_end) = source[text_open_start..]
            .find('>')
            .map(|offset| text_open_start + offset + 1)
        else {
            continue;
        };
        let Some(text_end) = source[text_open_end..content_end]
            .find("</w:t>")
            .map(|offset| text_open_end + offset)
        else {
            continue;
        };
        source.replace_range(text_open_end..text_end, &xml_text(value));
        replaced += 1;
    }
    Ok((source.into_bytes(), replaced))
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Clone, Debug)]
struct DocxTextNode {
    open: String,
    text: String,
    close: String,
}

fn replace_docx_xml(
    bytes: &[u8],
    replacements: &BTreeMap<String, String>,
) -> Result<(Vec<u8>, usize), String> {
    let source =
        String::from_utf8(bytes.to_vec()).map_err(|_| "DOCX XML 不是 UTF-8".to_string())?;
    let mut literals = Vec::new();
    let mut nodes = Vec::new();
    let mut cursor = 0usize;
    let mut node_start = source.find("<w:t");
    while let Some(start) = node_start {
        let open_end = source[start..]
            .find('>')
            .map(|offset| start + offset + 1)
            .ok_or_else(|| "DOCX 文本节点不完整".to_string())?;
        let close_start = source[open_end..]
            .find("</w:t>")
            .map(|offset| open_end + offset)
            .ok_or_else(|| "DOCX 文本节点不完整".to_string())?;
        literals.push(source[cursor..start].to_string());
        nodes.push(DocxTextNode {
            open: source[start..open_end].to_string(),
            text: source[open_end..close_start].to_string(),
            close: "</w:t>".into(),
        });
        cursor = close_start + "</w:t>".len();
        node_start = source[cursor..].find("<w:t").map(|offset| cursor + offset);
    }
    literals.push(source[cursor..].to_string());
    if nodes.is_empty() {
        return Ok((bytes.to_vec(), 0));
    }
    let mut count = 0;
    for (placeholder, value) in replacements {
        let marker = format!("{{{{{placeholder}}}}}");
        loop {
            let joined = nodes
                .iter()
                .map(|node| node.text.as_str())
                .collect::<String>();
            let Some(start) = joined.find(&marker) else {
                break;
            };
            let end = start + marker.len();
            let mut offset = 0usize;
            let mut first = true;
            for node in &mut nodes {
                let node_start = offset;
                let node_end = offset + node.text.len();
                if node_end > start && node_start < end {
                    let local_start = start.saturating_sub(node_start).min(node.text.len());
                    let local_end = end.saturating_sub(node_start).min(node.text.len());
                    if first {
                        node.text = format!(
                            "{}{}{}",
                            &node.text[..local_start],
                            xml_text(value),
                            &node.text[local_end..]
                        );
                        first = false;
                    } else {
                        node.text.replace_range(local_start..local_end, "");
                    }
                }
                offset = node_end;
            }
            count += 1;
        }
    }
    let mut output = String::new();
    for (index, node) in nodes.iter().enumerate() {
        output.push_str(&literals[index]);
        output.push_str(&node.open);
        output.push_str(&node.text);
        output.push_str(&node.close);
    }
    output.push_str(literals.last().unwrap_or(&String::new()));
    Ok((output.into_bytes(), count))
}
