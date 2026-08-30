//! PDF layout engine and printpdf operation adapter.

use super::*;

#[derive(Clone)]
pub(super) enum RenderFont {
    External(printpdf::FontId),
    Helvetica,
}

#[derive(Clone)]
pub(super) enum RenderImage {
    Raster {
        id: printpdf::XObjectId,
        source_width: usize,
        source_height: usize,
        width: u32,
        height: u32,
    },
    Svg {
        id: printpdf::XObjectId,
        source_width: usize,
        source_height: usize,
        width: u32,
        height: u32,
    },
}

pub(super) enum LayoutCommand {
    Text {
        text: String,
        x: u32,
        y: i32,
        size: u32,
    },
    Image {
        id: printpdf::XObjectId,
        x: u32,
        y: i32,
        scale_x: f32,
        scale_y: f32,
    },
    TableCell {
        lines: Vec<String>,
        x: u32,
        y: i32,
        width: u32,
        height: u32,
        header: bool,
    },
}

pub(super) struct PdfLayout<'a> {
    pub(super) pages: Vec<Vec<LayoutCommand>>,
    width: u32,
    height: u32,
    margin: u32,
    page_top: u32,
    pub(super) y: i32,
    header: Option<&'a str>,
    footer: Option<&'a str>,
}

impl<'a> PdfLayout<'a> {
    pub(super) fn new(
        width: u32,
        height: u32,
        margin: u32,
        page_top: u32,
        header: Option<&'a str>,
        footer: Option<&'a str>,
    ) -> Self {
        let mut layout = Self {
            pages: Vec::new(),
            width,
            height,
            margin,
            page_top,
            y: page_top as i32,
            header,
            footer,
        };
        layout.new_page();
        layout
    }

    pub(super) fn new_page(&mut self) {
        let mut commands = Vec::new();
        let mut header_y = self.height.saturating_sub(self.margin) as i32;
        for line in self.header.into_iter().flat_map(str::lines) {
            commands.push(LayoutCommand::Text {
                text: line.to_string(),
                x: self.margin,
                y: header_y,
                size: 10,
            });
            header_y -= 14;
        }
        for (index, line) in self.footer.into_iter().flat_map(str::lines).enumerate() {
            commands.push(LayoutCommand::Text {
                text: line.to_string(),
                x: self.margin,
                y: self.margin as i32 + (index as i32 * 12),
                size: 10,
            });
        }
        self.pages.push(commands);
        self.y = self.page_top as i32;
    }

    pub(super) fn is_at_page_top(&self) -> bool {
        self.y == self.page_top as i32
    }

    fn fits(&self, line_count: usize, font_size: u32) -> bool {
        let height = line_count.saturating_mul(font_size.saturating_add(4) as usize);
        self.y.saturating_sub(height as i32) > self.margin as i32
    }

    pub(super) fn fits_height(&self, height: u32) -> bool {
        self.y.saturating_sub(height as i32) > self.margin as i32
    }

    pub(super) fn add_text(&mut self, text: String, font_size: u32) {
        if !self.fits(1, font_size) {
            self.new_page();
        }
        self.pages
            .last_mut()
            .expect("PDF layout always has one page")
            .push(LayoutCommand::Text {
                text,
                x: self.margin,
                y: self.y,
                size: font_size,
            });
        self.y -= font_size.saturating_add(4) as i32;
    }

    pub(super) fn add_spacing(&mut self, points: u32) {
        if !self.fits_height(points) {
            self.new_page();
        } else {
            self.y -= points as i32;
        }
    }

    pub(super) fn table_row_height(&self, cells: &[String], font_size: u32) -> u32 {
        let column_width = self
            .width
            .saturating_sub(self.margin.saturating_mul(2))
            .checked_div(cells.len().max(1) as u32)
            .unwrap_or(1);
        let character_width = ((column_width.saturating_sub(6)) / font_size.max(1)) as usize;
        let wrapped = cells
            .iter()
            .map(|cell| wrap_text(cell, character_width))
            .collect::<Vec<_>>();
        let line_count = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        (line_count as u32)
            .saturating_mul(font_size.saturating_add(2))
            .saturating_add(6)
    }

    pub(super) fn add_table_row(&mut self, cells: &[String], font_size: u32, header: bool) {
        let column_width = self
            .width
            .saturating_sub(self.margin.saturating_mul(2))
            .checked_div(cells.len().max(1) as u32)
            .unwrap_or(1);
        let character_width = ((column_width.saturating_sub(6)) / font_size.max(1)) as usize;
        let wrapped = cells
            .iter()
            .map(|cell| wrap_text(cell, character_width))
            .collect::<Vec<_>>();
        let row_height = self.table_row_height(cells, font_size);
        let y = self.y - row_height as i32;
        let page = self
            .pages
            .last_mut()
            .expect("PDF layout always has one page");
        for (index, lines) in wrapped.into_iter().enumerate() {
            page.push(LayoutCommand::TableCell {
                lines,
                x: self.margin + column_width.saturating_mul(index as u32),
                y,
                width: column_width,
                height: row_height,
                header,
            });
        }
        self.y = y;
    }

    pub(super) fn add_image(&mut self, image: RenderImage) -> Result<(), DeliveryError> {
        let (width, height) = match &image {
            RenderImage::Raster { width, height, .. } | RenderImage::Svg { width, height, .. } => {
                (*width, *height)
            }
        };
        let available_width = self.width.saturating_sub(self.margin.saturating_mul(2));
        let available_height = self.page_top.saturating_sub(self.margin);
        if width > available_width || height > available_height {
            return Err(DeliveryError::InvalidLayout(
                "PDF 图片尺寸超过页面可用区域".into(),
            ));
        }
        if self.y.saturating_sub(height as i32) <= self.margin as i32 {
            self.new_page();
        }
        self.y -= height as i32;
        let command = match image {
            RenderImage::Raster {
                id,
                source_width,
                source_height,
                ..
            } => LayoutCommand::Image {
                id,
                x: self.margin,
                y: self.y,
                scale_x: width as f32 / source_width.max(1) as f32,
                scale_y: height as f32 / source_height.max(1) as f32,
            },
            RenderImage::Svg {
                id,
                source_width,
                source_height,
                ..
            } => LayoutCommand::Image {
                id,
                x: self.margin,
                y: self.y,
                scale_x: width as f32 / source_width.max(1) as f32,
                scale_y: height as f32 / source_height.max(1) as f32,
            },
        };
        self.pages
            .last_mut()
            .expect("PDF layout always has one page")
            .push(command);
        self.y -= 8;
        Ok(())
    }
}

pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let characters = text.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return vec![String::new()];
    }
    characters
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

pub(super) fn max_table_columns(available_width: u32) -> usize {
    (available_width / PDF_MIN_COLUMN_WIDTH).max(2) as usize
}

pub(super) fn table_group_count(column_count: usize, columns_per_page: usize) -> usize {
    if column_count == 0 || column_count <= columns_per_page {
        1
    } else {
        column_count
            .saturating_sub(1)
            .saturating_add(columns_per_page.saturating_sub(2))
            / columns_per_page.saturating_sub(1).max(1)
    }
}

pub(super) fn table_groups(
    headers: &[String],
    rows: &[Vec<String>],
    columns_per_page: usize,
) -> Vec<(Vec<String>, Vec<Vec<String>>)> {
    if headers.len() <= columns_per_page {
        return vec![(headers.to_vec(), rows.to_vec())];
    }
    let chunk_size = columns_per_page.saturating_sub(1).max(1);
    (1..headers.len())
        .step_by(chunk_size)
        .map(|start| {
            let end = start.saturating_add(chunk_size).min(headers.len());
            let indexes = std::iter::once(0usize)
                .chain(start..end)
                .collect::<Vec<_>>();
            let group_headers = indexes
                .iter()
                .map(|index| headers[*index].clone())
                .collect::<Vec<_>>();
            let group_rows = rows
                .iter()
                .map(|row| {
                    indexes
                        .iter()
                        .map(|index| row.get(*index).cloned().unwrap_or_default())
                        .collect()
                })
                .collect();
            (group_headers, group_rows)
        })
        .collect()
}

pub(super) fn points_to_mm(points: u32) -> printpdf::Mm {
    printpdf::Mm(points as f32 * 25.4 / 72.0)
}

pub(super) fn commands_to_ops(
    commands: Vec<LayoutCommand>,
    font: &RenderFont,
) -> Vec<printpdf::Op> {
    let mut ops = Vec::new();
    for command in commands {
        match command {
            LayoutCommand::Text { text, x, y, size } => {
                ops.push(black_fill());
                ops.push(printpdf::Op::StartTextSection);
                ops.push(printpdf::Op::SetTextCursor {
                    pos: printpdf::Point {
                        x: printpdf::Pt(x as f32),
                        y: printpdf::Pt(y as f32),
                    },
                });
                match font {
                    RenderFont::External(id) => {
                        ops.push(printpdf::Op::SetFont {
                            size: printpdf::Pt(size as f32),
                            font: printpdf::PdfFontHandle::External(id.clone()),
                        });
                        ops.push(printpdf::Op::ShowText {
                            items: vec![printpdf::TextItem::Text(text)],
                        });
                    }
                    RenderFont::Helvetica => {
                        ops.push(printpdf::Op::SetFont {
                            size: printpdf::Pt(size as f32),
                            font: printpdf::PdfFontHandle::Builtin(
                                printpdf::BuiltinFont::Helvetica,
                            ),
                        });
                        ops.push(printpdf::Op::ShowText {
                            items: vec![printpdf::TextItem::Text(text)],
                        });
                    }
                }
                ops.push(printpdf::Op::EndTextSection);
            }
            LayoutCommand::Image {
                id,
                x,
                y,
                scale_x,
                scale_y,
            } => ops.push(printpdf::Op::UseXobject {
                id,
                transform: printpdf::XObjectTransform {
                    translate_x: Some(printpdf::Pt(x as f32)),
                    translate_y: Some(printpdf::Pt(y as f32)),
                    scale_x: Some(scale_x),
                    scale_y: Some(scale_y),
                    dpi: Some(72.0),
                    ..Default::default()
                },
            }),
            LayoutCommand::TableCell {
                lines,
                x,
                y,
                width,
                height,
                header,
            } => {
                if header {
                    ops.push(printpdf::Op::SetFillColor {
                        col: printpdf::Color::Rgb(printpdf::Rgb {
                            r: 0.88,
                            g: 0.93,
                            b: 0.90,
                            icc_profile: None,
                        }),
                    });
                    ops.push(printpdf::Op::DrawPolygon {
                        polygon: printpdf::Polygon {
                            rings: vec![printpdf::PolygonRing {
                                points: vec![
                                    printpdf::LinePoint {
                                        p: printpdf::Point {
                                            x: printpdf::Pt(x as f32),
                                            y: printpdf::Pt(y as f32),
                                        },
                                        bezier: false,
                                    },
                                    printpdf::LinePoint {
                                        p: printpdf::Point {
                                            x: printpdf::Pt((x + width) as f32),
                                            y: printpdf::Pt(y as f32),
                                        },
                                        bezier: false,
                                    },
                                    printpdf::LinePoint {
                                        p: printpdf::Point {
                                            x: printpdf::Pt((x + width) as f32),
                                            y: printpdf::Pt((y + height as i32) as f32),
                                        },
                                        bezier: false,
                                    },
                                    printpdf::LinePoint {
                                        p: printpdf::Point {
                                            x: printpdf::Pt(x as f32),
                                            y: printpdf::Pt((y + height as i32) as f32),
                                        },
                                        bezier: false,
                                    },
                                ],
                            }],
                            mode: printpdf::PaintMode::Fill,
                            winding_order: printpdf::WindingOrder::NonZero,
                        },
                    });
                }
                ops.push(printpdf::Op::SetOutlineThickness {
                    pt: printpdf::Pt(0.4),
                });
                ops.push(printpdf::Op::DrawLine {
                    line: printpdf::Line {
                        points: vec![
                            printpdf::LinePoint {
                                p: printpdf::Point {
                                    x: printpdf::Pt(x as f32),
                                    y: printpdf::Pt(y as f32),
                                },
                                bezier: false,
                            },
                            printpdf::LinePoint {
                                p: printpdf::Point {
                                    x: printpdf::Pt((x + width) as f32),
                                    y: printpdf::Pt(y as f32),
                                },
                                bezier: false,
                            },
                            printpdf::LinePoint {
                                p: printpdf::Point {
                                    x: printpdf::Pt((x + width) as f32),
                                    y: printpdf::Pt((y + height as i32) as f32),
                                },
                                bezier: false,
                            },
                            printpdf::LinePoint {
                                p: printpdf::Point {
                                    x: printpdf::Pt(x as f32),
                                    y: printpdf::Pt((y + height as i32) as f32),
                                },
                                bezier: false,
                            },
                        ],
                        is_closed: true,
                    },
                });
                for (line_index, text) in lines.into_iter().enumerate() {
                    ops.push(black_fill());
                    ops.push(printpdf::Op::StartTextSection);
                    ops.push(printpdf::Op::SetTextCursor {
                        pos: printpdf::Point {
                            x: printpdf::Pt((x + 3) as f32),
                            y: printpdf::Pt(
                                (y + height as i32 - 11 - line_index as i32 * 11) as f32,
                            ),
                        },
                    });
                    match font {
                        RenderFont::External(id) => {
                            ops.push(printpdf::Op::SetFont {
                                size: printpdf::Pt(7.5),
                                font: printpdf::PdfFontHandle::External(id.clone()),
                            });
                            ops.push(printpdf::Op::ShowText {
                                items: vec![printpdf::TextItem::Text(text)],
                            });
                        }
                        RenderFont::Helvetica => {
                            ops.push(printpdf::Op::SetFont {
                                size: printpdf::Pt(7.5),
                                font: printpdf::PdfFontHandle::Builtin(
                                    printpdf::BuiltinFont::Helvetica,
                                ),
                            });
                            ops.push(printpdf::Op::ShowText {
                                items: vec![printpdf::TextItem::Text(text)],
                            });
                        }
                    }
                    ops.push(printpdf::Op::EndTextSection);
                }
            }
        }
    }
    ops
}

pub(super) fn black_fill() -> printpdf::Op {
    printpdf::Op::SetFillColor {
        col: printpdf::Color::Rgb(printpdf::Rgb {
            r: 0.08,
            g: 0.10,
            b: 0.09,
            icc_profile: None,
        }),
    }
}

pub(super) fn decode_report_image(
    bytes: &[u8],
    mime: &str,
    max_decoded_bytes: usize,
) -> Result<(printpdf::RawImage, usize), DeliveryError> {
    let format = match mime {
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/png" => image::ImageFormat::Png,
        _ => return Err(DeliveryError::InvalidImage(mime.into())),
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(max_decoded_bytes as u64);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| DeliveryError::InvalidImage(mime.into()))?;
    let rgba = decoded.into_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;
    let required = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(DeliveryError::ImageLimit)?;
    if required > max_decoded_bytes {
        return Err(DeliveryError::ImageLimit);
    }
    Ok((
        printpdf::RawImage {
            pixels: printpdf::RawImageData::U8(rgba.into_raw()),
            width,
            height,
            data_format: printpdf::RawImageFormat::RGBA8,
            tag: Vec::new(),
        },
        required,
    ))
}
