//! Bounded report layout IR backed by a mature PDF encoder.
//!
//! This is deliberately a report renderer, not an Office conversion layer.
//! XLSX/DOCX template handling remains a separate capability.

use crate::execution::{Artifact, ContractVersion, Metadata};
use serde::{Deserialize, Serialize};

pub const REPORT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PageSize {
    A4,
    Letter,
    Custom { width: u32, height: u32 },
}

impl PageSize {
    fn points(self) -> (u32, u32) {
        match self {
            Self::A4 => (595, 842),
            Self::Letter => (612, 792),
            Self::Custom { width, height } => (width, height),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FontResource {
    pub name: String,
    #[serde(skip)]
    pub bytes: Vec<u8>,
    pub embedded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReportElement {
    Paragraph {
        text: String,
        font_size: u32,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Image {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        mime: String,
    },
    PageBreak,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportDocument {
    pub version: ContractVersion,
    pub page_size: PageSize,
    pub margin: u32,
    pub font: FontResource,
    pub header: Option<String>,
    pub footer: Option<String>,
    pub elements: Vec<ReportElement>,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MetricCard {
    pub label: String,
    pub value: String,
    pub unit: Option<String>,
}

pub const DEFAULT_PDF_FONT_NAME: &str = "Noto Sans SC";
pub const DEFAULT_PDF_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/NotoSansSC-Regular.otf");
const PDF_MIN_COLUMN_WIDTH: u32 = 72;

/// 来源图的可读 PDF 报告：按节点类型稳定排序，超出行数上限时显式标注截断。
/// 敏感节点应先经过 `ProvenanceGraph::view` 过滤，本函数不做授权判断。
pub fn provenance_report(
    graph: &crate::provenance::ProvenanceGraph,
    font: FontResource,
    max_rows: usize,
) -> ReportDocument {
    let mut rows = graph
        .nodes
        .values()
        .map(|node| {
            vec![
                node.kind.clone(),
                node.label.clone(),
                node.parents.len().to_string(),
                node.id.chars().take(12).collect(),
            ]
        })
        .collect::<Vec<_>>();
    rows.sort();
    let total = rows.len();
    let truncated = total > max_rows || graph.truncated;
    rows.truncate(max_rows);
    let mut paragraphs = vec![format!("节点总数：{total}")];
    if truncated {
        paragraphs.push(format!("已截断：仅显示前 {} 个节点", rows.len()));
    }
    document_from_data(
        "衡枢来源图报告",
        paragraphs,
        [(
            vec![
                "类型".to_string(),
                "说明".to_string(),
                "父节点数".to_string(),
                "节点".to_string(),
            ],
            rows,
        )],
        Vec::new(),
        font,
    )
}

pub fn document_from_data(
    title: impl Into<String>,
    paragraphs: impl IntoIterator<Item = String>,
    tables: impl IntoIterator<Item = (Vec<String>, Vec<Vec<String>>)>,
    metrics: impl IntoIterator<Item = MetricCard>,
    font: FontResource,
) -> ReportDocument {
    let mut elements = Vec::new();
    elements.push(ReportElement::Paragraph {
        text: title.into(),
        font_size: 18,
    });
    for metric in metrics {
        elements.push(ReportElement::Paragraph {
            text: format!(
                "{}: {}{}",
                metric.label,
                metric.value,
                metric.unit.map_or(String::new(), |unit| format!(" {unit}"))
            ),
            font_size: 12,
        });
    }
    for paragraph in paragraphs {
        elements.push(ReportElement::Paragraph {
            text: paragraph,
            font_size: 10,
        });
    }
    for (headers, rows) in tables {
        elements.push(ReportElement::Table { headers, rows });
    }
    ReportDocument {
        version: Default::default(),
        page_size: PageSize::A4,
        margin: 36,
        font,
        header: None,
        footer: None,
        elements,
        metadata: Metadata::default(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PdfBudget {
    pub max_pages: usize,
    pub max_elements: usize,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
    pub max_output_bytes: usize,
}

impl Default for PdfBudget {
    fn default() -> Self {
        Self {
            max_pages: 100,
            max_elements: 10_000,
            max_text_bytes: 4 * 1024 * 1024,
            max_image_bytes: 32 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryError {
    EmptyFont,
    UnsupportedFont(String),
    ElementLimit,
    PageLimit,
    TextLimit,
    ImageLimit,
    OutputLimit,
    InvalidImage(String),
    InvalidLayout(String),
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFont => formatter.write_str("PDF 字体资源不能为空"),
            Self::UnsupportedFont(name) => write!(formatter, "PDF 不支持字体资源“{name}”"),
            Self::ElementLimit => formatter.write_str("PDF 元素超过预算"),
            Self::PageLimit => formatter.write_str("PDF 页数超过预算"),
            Self::TextLimit => formatter.write_str("PDF 文本超过预算"),
            Self::ImageLimit => formatter.write_str("PDF 图片超过预算"),
            Self::OutputLimit => formatter.write_str("PDF 输出字节超过预算"),
            Self::InvalidImage(mime) => write!(formatter, "PDF 不支持图片类型“{mime}”"),
            Self::InvalidLayout(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DeliveryError {}

impl ReportDocument {
    pub fn preflight(&self, budget: &PdfBudget) -> Result<usize, DeliveryError> {
        let standard_font = self.font.name == "Helvetica";
        if self.font.embedded && self.font.bytes.is_empty() {
            return Err(DeliveryError::EmptyFont);
        }
        if !self.font.embedded && !standard_font {
            return Err(DeliveryError::UnsupportedFont(self.font.name.clone()));
        }
        if !self.font.embedded && self.contains_non_ascii_text() {
            return Err(DeliveryError::UnsupportedFont(self.font.name.clone()));
        }
        if self.margin.saturating_mul(2) >= self.page_size.points().0.min(self.page_size.points().1)
        {
            return Err(DeliveryError::InvalidLayout("PDF 页边距过大".into()));
        }
        if self.elements.len() > budget.max_elements {
            return Err(DeliveryError::ElementLimit);
        }
        let mut pages = 1usize;
        let mut text_bytes = self.header.as_ref().map_or(0, |text| text.len())
            + self.footer.as_ref().map_or(0, |text| text.len());
        let mut image_bytes = 0usize;
        for element in &self.elements {
            match element {
                ReportElement::Paragraph { text, font_size } => {
                    if *font_size == 0 || *font_size > 256 {
                        return Err(DeliveryError::InvalidLayout("PDF 字号超出范围".into()));
                    }
                    text_bytes = text_bytes.saturating_add(text.len());
                }
                ReportElement::Table { headers, rows } => {
                    if headers.is_empty() || rows.iter().any(|row| row.len() != headers.len()) {
                        return Err(DeliveryError::InvalidLayout("PDF 表格列数不一致".into()));
                    }
                    text_bytes =
                        text_bytes.saturating_add(headers.iter().map(String::len).sum::<usize>());
                    text_bytes = text_bytes
                        .saturating_add(rows.iter().flatten().map(String::len).sum::<usize>());
                    let available_width = self
                        .page_size
                        .points()
                        .0
                        .saturating_sub(self.margin.saturating_mul(2));
                    let horizontal_groups =
                        table_group_count(headers.len(), max_table_columns(available_width));
                    pages = pages
                        .saturating_add(
                            rows.len()
                                .saturating_add(34)
                                .checked_div(35)
                                .unwrap_or(0)
                                .saturating_mul(horizontal_groups),
                        )
                        .saturating_sub(1);
                }
                ReportElement::Image {
                    bytes,
                    mime,
                    width,
                    height,
                } => {
                    if *width == 0 || *height == 0 {
                        return Err(DeliveryError::InvalidLayout("PDF 图片尺寸不能为零".into()));
                    }
                    if !matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/svg+xml") {
                        return Err(DeliveryError::InvalidImage(mime.clone()));
                    }
                    image_bytes = image_bytes.saturating_add(bytes.len());
                }
                ReportElement::PageBreak => pages = pages.saturating_add(1),
            }
        }
        if pages > budget.max_pages {
            return Err(DeliveryError::PageLimit);
        }
        if text_bytes > budget.max_text_bytes {
            return Err(DeliveryError::TextLimit);
        }
        if image_bytes > budget.max_image_bytes {
            return Err(DeliveryError::ImageLimit);
        }
        Ok(pages)
    }

    pub fn render_pdf(&self, budget: &PdfBudget) -> Result<Vec<u8>, DeliveryError> {
        self.preflight(budget)?;
        let (width, height) = self.page_size.points();
        let mut pdf = printpdf::PdfDocument::new("衡枢报告");
        let font = if self.font.embedded {
            let mut warnings = Vec::new();
            match printpdf::ParsedFont::from_bytes(&self.font.bytes, 0, &mut warnings) {
                Some(parsed) => RenderFont::External(pdf.add_font(&parsed)),
                None if matches!(self.font.name.as_str(), "Helvetica" | "STSong-Light") => {
                    RenderFont::Helvetica
                }
                None => return Err(DeliveryError::UnsupportedFont(self.font.name.clone())),
            }
        } else {
            RenderFont::Helvetica
        };

        let mut image_ids = Vec::new();
        let mut decoded_image_bytes = 0usize;
        for element in &self.elements {
            if let ReportElement::Image {
                bytes,
                mime,
                width,
                height,
            } = element
            {
                if mime == "image/svg+xml" {
                    let svg = std::str::from_utf8(bytes)
                        .map_err(|_| DeliveryError::InvalidImage(mime.clone()))?;
                    let mut warnings = Vec::new();
                    let parsed = printpdf::Svg::parse(svg, &mut warnings)
                        .map_err(|_| DeliveryError::InvalidImage(mime.clone()))?;
                    let source_width = parsed.width.map_or(1, |value| value.0);
                    let source_height = parsed.height.map_or(1, |value| value.0);
                    image_ids.push(RenderImage::Svg {
                        id: pdf.add_xobject(&parsed),
                        source_width,
                        source_height,
                        width: *width,
                        height: *height,
                    });
                } else {
                    let (image, decoded_bytes) =
                        decode_report_image(bytes, mime, budget.max_image_bytes)?;
                    decoded_image_bytes = decoded_image_bytes.saturating_add(decoded_bytes);
                    if decoded_image_bytes > budget.max_image_bytes {
                        return Err(DeliveryError::ImageLimit);
                    }
                    image_ids.push(RenderImage::Raster {
                        id: pdf.add_image(&image),
                        source_width: image.width,
                        source_height: image.height,
                        width: *width,
                        height: *height,
                    });
                }
            }
        }

        let header_line_count = self
            .header
            .as_deref()
            .map_or(0, |header| header.lines().count());
        let page_top = height
            .saturating_sub(self.margin)
            .saturating_sub((header_line_count as u32).saturating_mul(14));
        let mut layout = PdfLayout::new(
            width,
            height,
            self.margin,
            page_top,
            self.header.as_deref(),
            self.footer.as_deref(),
        );
        let mut image_index = 0usize;
        for (element_index, element) in self.elements.iter().enumerate() {
            match element {
                ReportElement::Paragraph { text, font_size } => {
                    let line_width = ((width.saturating_sub(self.margin.saturating_mul(2)))
                        / (*font_size).max(1)) as usize;
                    for line in text.lines().flat_map(|line| wrap_text(line, line_width)) {
                        layout.add_text(line, *font_size);
                    }
                }
                ReportElement::Table { headers, rows } => {
                    for (group_index, (group_headers, group_rows)) in table_groups(
                        headers,
                        rows,
                        max_table_columns(width.saturating_sub(self.margin.saturating_mul(2))),
                    )
                    .into_iter()
                    .enumerate()
                    {
                        if group_index > 0 && !layout.is_at_page_top() {
                            layout.new_page();
                        }
                        let mut rows_on_page = 0usize;
                        for (row_index, row) in group_rows.iter().enumerate() {
                            let header_height = layout.table_row_height(&group_headers, 9);
                            let row_height = layout.table_row_height(row, 9);
                            let needs_new_page = rows_on_page >= 35
                                || !layout.fits_height(if row_index == 0 {
                                    header_height.saturating_add(row_height)
                                } else {
                                    row_height
                                });
                            if row_index == 0 || needs_new_page {
                                if needs_new_page && !layout.is_at_page_top() {
                                    layout.new_page();
                                }
                                layout.add_table_row(&group_headers, 9, true);
                                rows_on_page = 0;
                            }
                            layout.add_table_row(row, 9, false);
                            rows_on_page += 1;
                        }
                        layout.add_spacing(14);
                    }
                }
                ReportElement::Image {
                    width: _image_width,
                    height: _image_height,
                    ..
                } => {
                    let image = image_ids
                        .get(image_index)
                        .cloned()
                        .ok_or_else(|| DeliveryError::InvalidLayout("PDF 图片资源丢失".into()))?;
                    layout.add_image(image)?;
                    image_index += 1;
                }
                ReportElement::PageBreak => {
                    if element_index + 1 < self.elements.len() {
                        layout.new_page();
                    }
                }
            }
        }
        if layout.pages.len() > budget.max_pages {
            return Err(DeliveryError::PageLimit);
        }
        let page_width = points_to_mm(width);
        let page_height = points_to_mm(height);
        let pages = layout
            .pages
            .into_iter()
            .map(|commands| {
                printpdf::PdfPage::new(page_width, page_height, commands_to_ops(commands, &font))
            })
            .collect();
        pdf.with_pages(pages);
        let mut warnings = Vec::new();
        let bytes = pdf.save(
            &printpdf::PdfSaveOptions {
                image_optimization: None,
                ..Default::default()
            },
            &mut warnings,
        );
        if bytes.len() > budget.max_output_bytes {
            return Err(DeliveryError::OutputLimit);
        }
        Ok(bytes)
    }

    fn contains_non_ascii_text(&self) -> bool {
        self.header
            .iter()
            .chain(self.footer.iter())
            .any(|text| !text.is_ascii())
            || self.elements.iter().any(|element| match element {
                ReportElement::Paragraph { text, .. } => !text.is_ascii(),
                ReportElement::Table { headers, rows } => {
                    headers.iter().any(|text| !text.is_ascii())
                        || rows.iter().flatten().any(|text| !text.is_ascii())
                }
                _ => false,
            })
    }

    /// 报告 PDF 的专用来源节点：父节点是报告数据携带的来源引用。
    pub fn record_provenance(
        &self,
        graph: &mut crate::provenance::ProvenanceGraph,
        output: &str,
        budget: &PdfBudget,
        graph_budget: crate::provenance::GraphBudget,
    ) -> Result<String, DeliveryError> {
        let pages = self.preflight(budget)?;
        graph
            .add_report_pdf(
                output,
                pages,
                self.metadata
                    .provenance
                    .iter()
                    .map(|reference| reference.source_id.clone()),
                self.metadata.clone(),
                graph_budget,
            )
            .map_err(|_| DeliveryError::ElementLimit)
    }

    pub fn render_artifact(
        &self,
        path: impl Into<std::path::PathBuf>,
        budget: &PdfBudget,
    ) -> Result<Artifact, DeliveryError> {
        crate::privacy::guard_endpoint(crate::privacy::Endpoint::Pdf, &self.metadata.sensitivity)
            .map_err(DeliveryError::InvalidLayout)?;
        let bytes = self.render_pdf(budget)?;
        let mut artifact = Artifact::from_bytes(path, "application/pdf", &bytes);
        artifact.purpose = "报告交付".into();
        artifact.metadata = self.metadata.clone();
        Ok(artifact)
    }

    /// Render and atomically commit a PDF artifact after endpoint and file
    /// policy checks. All report-producing entry points use this path.
    pub fn write_artifact(
        &self,
        path: impl Into<std::path::PathBuf>,
        budget: &PdfBudget,
    ) -> Result<Artifact, DeliveryError> {
        let path = path.into();
        crate::privacy::guard_endpoint(crate::privacy::Endpoint::Pdf, &self.metadata.sensitivity)
            .map_err(DeliveryError::InvalidLayout)?;
        let bytes = self.render_pdf(budget)?;
        crate::security::atomic_write_file(&path, &bytes, true)
            .map_err(|error| DeliveryError::InvalidLayout(format!("PDF 输出提交失败：{error}")))?;
        let mut artifact = Artifact::from_bytes(path, "application/pdf", &bytes);
        artifact.purpose = "报告交付".into();
        artifact.metadata = self.metadata.clone();
        Ok(artifact)
    }
}

mod layout;
#[cfg(test)]
use layout::LayoutCommand;
use layout::{
    commands_to_ops, decode_report_image, max_table_columns, points_to_mm, table_group_count,
    table_groups, wrap_text, PdfLayout, RenderFont, RenderImage,
};
#[cfg(test)]
mod tests {
    use super::*;

    fn parse_pdf(bytes: &[u8]) -> printpdf::PdfDocument {
        printpdf::PdfDocument::parse(
            bytes,
            &printpdf::PdfParseOptions::default(),
            &mut Vec::new(),
        )
        .unwrap()
    }

    fn pdf_text(document: &printpdf::PdfDocument) -> String {
        document
            .extract_text()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn encoded_image(format: image::ImageFormat) -> Vec<u8> {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            2,
            image::Rgb([255, 0, 0]),
        ));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    fn document() -> ReportDocument {
        ReportDocument {
            version: Default::default(),
            page_size: PageSize::A4,
            margin: 36,
            font: FontResource {
                name: DEFAULT_PDF_FONT_NAME.into(),
                bytes: DEFAULT_PDF_FONT_BYTES.to_vec(),
                embedded: true,
            },
            header: Some("Report".into()),
            footer: Some("End".into()),
            elements: vec![
                ReportElement::Paragraph {
                    text: "Hello (PDF)".into(),
                    font_size: 12,
                },
                ReportElement::Table {
                    headers: vec!["A".into(), "B".into()],
                    rows: vec![vec!["1".into(), "2".into()]],
                },
            ],
            metadata: Metadata::default(),
        }
    }

    #[test]
    fn report_preflight_and_pdf_output_are_bounded_and_valid() {
        let document = document();
        assert_eq!(document.preflight(&PdfBudget::default()).unwrap(), 1);
        let pdf = document.render_pdf(&PdfBudget::default()).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(pdf.windows(5).any(|window| window == b"%%EOF"));
        let parsed = parse_pdf(&pdf);
        assert_eq!(parsed.pages.len(), 1);
        assert!(pdf_text(&parsed).contains("Hello (PDF)"));
    }

    #[test]
    fn preflight_rejects_every_budget_and_layout_violation() {
        let budget = |pages: usize, elements: usize, text: usize, images: usize| PdfBudget {
            max_pages: pages,
            max_elements: elements,
            max_text_bytes: text,
            max_image_bytes: images,
            ..Default::default()
        };
        let mut paged = document();
        paged.elements.push(ReportElement::PageBreak);
        assert_eq!(
            paged.preflight(&budget(1, 10, 1024, 1024)),
            Err(DeliveryError::PageLimit)
        );
        assert_eq!(
            document().preflight(&budget(10, 1, 1024, 1024)),
            Err(DeliveryError::ElementLimit)
        );
        assert_eq!(
            document().preflight(&budget(10, 10, 4, 1024)),
            Err(DeliveryError::TextLimit)
        );

        let mut imaged = document();
        imaged.elements.push(ReportElement::Image {
            bytes: vec![0; 64],
            width: 2,
            height: 2,
            mime: "image/jpeg".into(),
        });
        assert_eq!(
            imaged.preflight(&budget(10, 10, 1024, 8)),
            Err(DeliveryError::ImageLimit)
        );

        let mut wide_margin = document();
        wide_margin.margin = 500;
        assert!(matches!(
            wide_margin.preflight(&PdfBudget::default()),
            Err(DeliveryError::InvalidLayout(_))
        ));

        let mut big_font = document();
        big_font.elements = vec![ReportElement::Paragraph {
            text: "x".into(),
            font_size: 999,
        }];
        assert!(matches!(
            big_font.preflight(&PdfBudget::default()),
            Err(DeliveryError::InvalidLayout(_))
        ));

        let mut ragged = document();
        ragged.elements = vec![ReportElement::Table {
            headers: vec!["A".into(), "B".into()],
            rows: vec![vec!["1".into()]],
        }];
        assert!(matches!(
            ragged.preflight(&PdfBudget::default()),
            Err(DeliveryError::InvalidLayout(_))
        ));

        let mut zero_image = document();
        zero_image.elements = vec![ReportElement::Image {
            bytes: vec![1],
            width: 0,
            height: 1,
            mime: "image/png".into(),
        }];
        assert!(matches!(
            zero_image.preflight(&PdfBudget::default()),
            Err(DeliveryError::InvalidLayout(_))
        ));

        let mut external_font = document();
        external_font.font.name = "ExternalFont".into();
        external_font.font.embedded = false;
        assert_eq!(
            external_font.preflight(&PdfBudget::default()),
            Err(DeliveryError::UnsupportedFont("ExternalFont".into()))
        );

        assert_eq!(
            document().render_pdf(&PdfBudget {
                max_output_bytes: 16,
                ..Default::default()
            }),
            Err(DeliveryError::OutputLimit)
        );
    }

    #[test]
    fn unsupported_objects_fail_before_writing() {
        let mut empty_font_document = document();
        empty_font_document.font.bytes.clear();
        assert_eq!(
            empty_font_document.render_pdf(&PdfBudget::default()),
            Err(DeliveryError::EmptyFont)
        );
        let mut invalid_document = self::document();
        invalid_document.elements.push(ReportElement::Image {
            bytes: vec![1],
            width: 1,
            height: 1,
            mime: "image/gif".into(),
        });
        assert_eq!(
            invalid_document.render_pdf(&PdfBudget::default()),
            Err(DeliveryError::InvalidImage("image/gif".into()))
        );
    }

    #[test]
    fn jpeg_images_are_embedded_and_positioned_in_pdf() {
        let mut document = document();
        document.elements.push(ReportElement::Image {
            bytes: encoded_image(image::ImageFormat::Jpeg),
            width: 120,
            height: 80,
            mime: "image/jpeg".into(),
        });
        let pdf = document.render_pdf(&PdfBudget::default()).unwrap();
        let parsed = parse_pdf(&pdf);
        assert_eq!(parsed.pages.len(), 1);
        assert!(parsed
            .pages
            .iter()
            .flat_map(|page| &page.ops)
            .any(|op| { matches!(op, printpdf::Op::UseXobject { .. }) }));
        assert!(pdf.len() > 1_000);
    }

    #[test]
    fn png_images_are_decoded_to_rgb_and_cjk_text_uses_cid_font() {
        let mut document = document();
        document.elements = vec![
            ReportElement::Paragraph {
                text: "中文报告".into(),
                font_size: 12,
            },
            ReportElement::Image {
                bytes: encoded_image(image::ImageFormat::Png),
                width: 1,
                height: 1,
                mime: "image/png".into(),
            },
        ];
        let pdf = document.render_pdf(&PdfBudget::default()).unwrap();
        let parsed = parse_pdf(&pdf);
        assert!(pdf_text(&parsed).contains("中文报告"));
        assert!(!parsed.resources.fonts.map.is_empty());
        assert!(parsed
            .pages
            .iter()
            .flat_map(|page| &page.ops)
            .any(|op| { matches!(op, printpdf::Op::UseXobject { .. }) }));
    }

    #[test]
    fn long_text_wraps_and_table_headers_repeat_after_page_breaks() {
        let mut document = document();
        document.elements = vec![
            ReportElement::Paragraph {
                text: "a".repeat(2_000),
                font_size: 12,
            },
            ReportElement::Table {
                headers: vec!["ColumnA".into(), "ColumnB".into()],
                rows: (0..100)
                    .map(|index| vec![index.to_string(), "Value".into()])
                    .collect(),
            },
        ];
        let pdf = document
            .render_pdf(&PdfBudget {
                max_pages: 100,
                ..Default::default()
            })
            .unwrap();
        let parsed = parse_pdf(&pdf);
        assert!(parsed.pages.len() > 1);
        assert!(pdf_text(&parsed).matches("ColumnA").count() > 1);
    }

    #[test]
    fn report_data_conversion_has_finite_metric_table_and_text_elements() {
        let document = document_from_data(
            "经营报告",
            ["说明".into()],
            [(vec!["A".into()], vec![vec!["1".into()]])],
            [MetricCard {
                label: "收入".into(),
                value: "10".into(),
                unit: Some("元".into()),
            }],
            FontResource {
                name: DEFAULT_PDF_FONT_NAME.into(),
                bytes: DEFAULT_PDF_FONT_BYTES.to_vec(),
                embedded: true,
            },
        );
        assert_eq!(document.elements.len(), 4);
        assert!(matches!(
            document.elements[1],
            ReportElement::Paragraph { .. }
        ));
        assert!(matches!(document.elements[3], ReportElement::Table { .. }));
    }

    #[test]
    fn heading_after_table_starts_below_the_last_row() {
        let mut layout = PdfLayout::new(595, 842, 36, 792, None, None);
        layout.add_table_row(&["表头".into()], 9, true);
        layout.add_table_row(&["最后一行".into()], 9, false);
        let table_bottom = layout.y;
        layout.add_spacing(14);
        layout.add_text("下一段标题".into(), 14);
        let heading_y = layout
            .pages
            .last()
            .unwrap()
            .iter()
            .rev()
            .find_map(|command| match command {
                LayoutCommand::Text { text, y, .. } if text == "下一段标题" => Some(*y),
                _ => None,
            })
            .unwrap();
        assert_eq!(heading_y, table_bottom - 14);
    }

    #[test]
    fn wide_tables_only_split_when_needed_and_repeat_the_key_column() {
        let narrow_headers = (0..7).map(|index| format!("列{index}")).collect::<Vec<_>>();
        let narrow_rows = vec![(0..7).map(|index| index.to_string()).collect::<Vec<_>>()];
        let narrow = table_groups(&narrow_headers, &narrow_rows, 7);
        assert_eq!(narrow.len(), 1);

        let wide_headers = (0..9).map(|index| format!("列{index}")).collect::<Vec<_>>();
        let wide_rows = vec![(0..9).map(|index| format!("值{index}")).collect::<Vec<_>>()];
        let groups = table_groups(&wide_headers, &wide_rows, 7);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0[0], "列0");
        assert_eq!(groups[1].0[0], "列0");
        assert_eq!(groups[0].1[0][0], "值0");
        assert_eq!(groups[1].1[0][0], "值0");
        assert_eq!(groups[1].0, vec!["列0", "列7", "列8"]);
        assert_eq!(groups[1].1[0], vec!["值0", "值7", "值8"]);
    }

    #[test]
    fn explicit_page_break_creates_multiple_pdf_page_objects() {
        let mut document = document();
        document.elements = vec![
            ReportElement::Paragraph {
                text: "第一页".into(),
                font_size: 10,
            },
            ReportElement::PageBreak,
            ReportElement::Paragraph {
                text: "第二页".into(),
                font_size: 10,
            },
        ];
        let pdf = document.render_pdf(&PdfBudget::default()).unwrap();
        let parsed = parse_pdf(&pdf);
        assert_eq!(parsed.pages.len(), 2);
        let text = pdf_text(&parsed);
        assert!(text.contains("第一页"));
        assert!(text.contains("第二页"));
    }
}
