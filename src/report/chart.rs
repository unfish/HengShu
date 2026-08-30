use crate::value::Value;
use plotters::coord::types::RangedCoordf64;
use plotters::coord::Shift;
use plotters::prelude::*;
use std::collections::BTreeMap;

#[cfg_attr(test, derive(Debug))]
pub(super) struct ChartData {
    pub(super) labels: Vec<String>,
    pub(super) values: Vec<f64>,
    pub(super) series: Vec<ChartSeries>,
}

#[cfg_attr(test, derive(Debug))]
pub(super) struct ChartSeries {
    pub(super) name: String,
    pub(super) values: Vec<f64>,
}

pub(super) struct ChartOptions {
    pub(super) title: String,
    pub(super) x_axis_name: Option<String>,
    pub(super) y_axis_name: Option<String>,
    pub(super) unit: Option<String>,
    pub(super) decimal_places: usize,
    pub(super) show_values: bool,
    pub(super) reference_line: Option<f64>,
    pub(super) interval_lower: Option<Vec<f64>>,
    pub(super) interval_upper: Option<Vec<f64>>,
    pub(super) annotation: Option<String>,
}

/// 超过这个系列数的图表无法阅读，也会让 SVG 体积失控；热力图的列数同样受此约束。
const MAX_CHART_SERIES: usize = 64;

const WIDTH: u32 = 1000;
const HEIGHT: u32 = 600;
/// 横轴最多绘制的刻度标签数；超出后由 Plotters 按步长抽样，避免长标签互相压盖。
const MAX_AXIS_LABELS: usize = 20;
const GRID: RGBColor = RGBColor(0xd7, 0xdd, 0xd9);
const AXIS: RGBColor = RGBColor(0x4b, 0x57, 0x51);
const INK: RGBColor = RGBColor(0x26, 0x33, 0x2d);
const POSITIVE: RGBColor = RGBColor(0x1f, 0x6f, 0x54);
const NEGATIVE: RGBColor = RGBColor(0xb5, 0x48, 0x48);
const SERIES_COLORS: [RGBColor; 4] = [
    RGBColor(0x1f, 0x6f, 0x54),
    RGBColor(0x00, 0x7c, 0x91),
    RGBColor(0xc4, 0x7f, 0x17),
    RGBColor(0x8b, 0x4c, 0x70),
];

pub(super) fn chart_data(
    value: &Value,
    named: &BTreeMap<String, Value>,
    collection_limit: usize,
    kind: &str,
) -> Result<ChartData, String> {
    if let Value::Table(table) = value {
        let table = table.borrow();
        if table.rows.is_empty() {
            return Err(error("图表数据不能为空"));
        }
        if table.rows.len() > collection_limit {
            return Err(error("图表数据点超过集合预算"));
        }
        let x_column = named
            .get("横轴")
            .map(ToString::to_string)
            .or_else(|| {
                (kind == "相关热力图").then(|| {
                    if table.rows[0].contains_key("列") {
                        "列".into()
                    } else {
                        table.rows[0].keys().next().cloned().unwrap_or_default()
                    }
                })
            })
            .ok_or_else(|| error("表格图表需要指定“横轴”列"))?;
        let series_names = if let Some(value) = named.get("系列") {
            value
                .iter_values_limited(collection_limit)?
                .into_iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        } else if let Some(value) = named.get("纵轴") {
            match value {
                Value::List(_) => value
                    .iter_values_limited(collection_limit)?
                    .into_iter()
                    .map(|value| value.to_string())
                    .collect(),
                _ => vec![value.to_string()],
            }
        } else if kind == "相关热力图" {
            table.rows[0]
                .keys()
                .filter(|column| *column != &x_column)
                .filter(|column| {
                    table
                        .rows
                        .iter()
                        .any(|row| row.get(*column).is_some_and(|value| value.as_f64().is_ok()))
                })
                .cloned()
                .collect()
        } else {
            return Err(error("表格图表需要指定“纵轴”列"));
        };
        if series_names.is_empty() {
            return Err(error("图表至少需要一个数值系列"));
        }
        if series_names.len() > MAX_CHART_SERIES {
            return Err(error(format!(
                "图表系列数 {} 超过可读上限 {MAX_CHART_SERIES}",
                series_names.len()
            )));
        }
        // 先按行数×系列数校验总数据点，避免宽表在分配前就把内存和 SVG 撑爆。
        let points = series_names
            .len()
            .checked_mul(table.rows.len())
            .ok_or_else(|| error("图表数据点超过集合预算"))?;
        if points > collection_limit {
            return Err(error("图表数据点超过集合预算"));
        }
        let mut labels = Vec::with_capacity(table.rows.len());
        for row in &table.rows {
            let label = row
                .get(&x_column)
                .ok_or_else(|| error(format!("图表表格不存在横轴列“{x_column}”")))?;
            labels.push(label.to_string());
        }
        let mut series = Vec::with_capacity(series_names.len());
        for name in series_names {
            let values = table
                .rows
                .iter()
                .map(|row| {
                    let value = row
                        .get(&name)
                        .ok_or_else(|| error(format!("图表表格不存在系列列“{name}”")))?;
                    let number = value
                        .as_f64()
                        .map_err(|_| error(format!("图表系列列“{name}”必须是数字")))?;
                    if number.is_finite() {
                        Ok(number)
                    } else {
                        Err(error("图表数据必须是有限数字"))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            series.push(ChartSeries { name, values });
        }
        let values = series[0].values.clone();
        return Ok(ChartData {
            labels,
            values,
            series,
        });
    }

    let values = value
        .iter_values_limited(collection_limit)?
        .iter()
        .map(Value::as_f64)
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() {
        return Err(error("图表数据不能为空"));
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(error("图表数据必须是有限数字"));
    }
    Ok(ChartData {
        labels: (1..=values.len()).map(|index| index.to_string()).collect(),
        series: vec![ChartSeries {
            name: "数值".into(),
            values: values.clone(),
        }],
        values,
    })
}

pub(crate) fn chart_svg(values: &[f64], bars: bool, title: &str) -> Result<String, String> {
    render_chart_svg(
        &ChartData {
            labels: (1..=values.len()).map(|index| index.to_string()).collect(),
            values: values.to_vec(),
            series: vec![ChartSeries {
                name: "数值".into(),
                values: values.to_vec(),
            }],
        },
        bars,
        &ChartOptions {
            title: title.into(),
            x_axis_name: None,
            y_axis_name: None,
            unit: None,
            decimal_places: 2,
            show_values: false,
            reference_line: None,
            interval_lower: None,
            interval_upper: None,
            annotation: None,
        },
    )
}

/// 单位换算属于数据变换，由衡枢负责；`plotters` 只按换算后的数值绘图。
fn unit_scale(unit: Option<&str>) -> f64 {
    match unit {
        Some("万元") => 10_000.0,
        Some("亿元") => 100_000_000.0,
        _ => 1.0,
    }
}

fn scaled(values: &[f64], unit: Option<&str>) -> Vec<f64> {
    let scale = unit_scale(unit);
    values.iter().map(|value| value / scale).collect()
}

fn y_axis_title(options: &ChartOptions) -> String {
    match (&options.y_axis_name, &options.unit) {
        (Some(name), Some(unit)) => format!("{name}（{unit}）"),
        (Some(name), None) => name.clone(),
        (None, Some(unit)) => unit.clone(),
        (None, None) => String::new(),
    }
}

/// 数值域留白；常量数据也要得到一个非退化的区间，否则 Plotters 无法建坐标系。
fn domain(values: &[f64], include_zero: bool) -> (f64, f64) {
    let mut min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let mut max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || !max.is_finite() {
        min = 0.0;
        max = 1.0;
    }
    if include_zero {
        min = min.min(0.0);
        max = max.max(0.0);
    }
    if (max - min).abs() < f64::EPSILON {
        let padding = max.abs().max(1.0) * 0.1;
        min -= padding;
        max += padding;
    } else {
        let padding = (max - min) * 0.05;
        min -= padding;
        max += padding;
    }
    if include_zero {
        min = min.min(0.0);
        max = max.max(0.0);
    }
    (min, max)
}

fn format_number(value: f64, decimal_places: usize) -> String {
    let mut formatted = format!("{value:.decimal_places$}");
    if decimal_places > 0 {
        while formatted.ends_with('0') {
            formatted.pop();
        }
        if formatted.ends_with('.') {
            formatted.pop();
        }
    }
    formatted
}

fn label_style(size: u32) -> TextStyle<'static> {
    ("sans-serif", size).into_font().color(&INK)
}

/// `plotters` 不输出可访问性标注，这里统一注入 `role`/`<title>`/`<desc>`。
/// `plotters` 已经转义过它自己绘制的文本，所以只转义我们注入的这两段。
fn annotate(svg: String, title: &str, description: &str) -> Result<String, String> {
    let close = svg
        .find('>')
        .ok_or_else(|| error("图表后端没有生成有效的 SVG 根标签"))?;
    let mut output = String::with_capacity(svg.len() + title.len() + description.len() + 64);
    output.push_str(&svg[..close]);
    output.push_str(&format!(
        " role=\"img\"><title>{}</title><desc>{}</desc>",
        escape(title),
        escape(description)
    ));
    output.push_str(&svg[close + 1..]);
    Ok(output)
}

/// 所有图表共用同一块画布、白底、标题和可访问性标注，不再各自手写坐标。
fn draw<F>(options: &ChartOptions, description: &str, body: F) -> Result<String, String>
where
    F: FnOnce(&DrawingArea<SVGBackend<'_>, Shift>) -> Result<(), String>,
{
    let mut buffer = String::new();
    {
        let root = SVGBackend::with_string(&mut buffer, (WIDTH, HEIGHT)).into_drawing_area();
        root.fill(&WHITE)
            .map_err(|reason| error(format!("图表背景绘制失败：{reason}")))?;
        let area = root
            .titled(&options.title, ("sans-serif", 22).into_font().color(&INK))
            .map_err(|reason| error(format!("图表标题绘制失败：{reason}")))?;
        body(&area)?;
        area.present()
            .map_err(|reason| error(format!("图表输出失败：{reason}")))?;
    }
    annotate(buffer, &options.title, description)
}

/// 类别横轴：`plotters` 用序号建坐标系，标签由格式化器映射回业务文本。
fn category_labels(labels: &[String]) -> impl Fn(&f64) -> String + '_ {
    move |position| {
        let index = position.round();
        if (position - index).abs() > 1e-6 || index < 0.0 {
            return String::new();
        }
        labels.get(index as usize).cloned().unwrap_or_default()
    }
}

fn x_label_area(count: usize) -> u32 {
    if count > 10 {
        76
    } else {
        52
    }
}

pub(super) fn render_chart_svg(
    data: &ChartData,
    bars: bool,
    options: &ChartOptions,
) -> Result<String, String> {
    let multi = data.series.len() > 1;
    let kind = match (multi, bars) {
        (true, true) => "多系列柱状图",
        (true, false) => "多系列折线图",
        (false, true) => "柱状图",
        (false, false) => "折线图",
    };
    let description = if multi {
        format!(
            "{kind}，共 {} 个系列、{} 个分类",
            data.series.len(),
            data.labels.len()
        )
    } else {
        format!("{kind}，共 {} 个数据点", data.values.len())
    };
    let all = data
        .series
        .iter()
        .flat_map(|series| scaled(&series.values, options.unit.as_deref()))
        .collect::<Vec<_>>();
    let (min, max) = domain(&all, bars);
    let count = data.labels.len().max(1);
    let y_title = y_axis_title(options);
    let x_title = options.x_axis_name.clone().unwrap_or_default();
    draw(options, &description, |area| {
        let mut chart = ChartBuilder::on(area)
            .margin(16)
            .x_label_area_size(x_label_area(count))
            .y_label_area_size(88)
            .build_cartesian_2d(-0.6f64..count as f64 - 0.4, min..max)
            .map_err(|reason| error(format!("图表坐标系构建失败：{reason}")))?;
        chart
            .configure_mesh()
            .light_line_style(GRID)
            .bold_line_style(GRID)
            .axis_style(AXIS)
            .x_labels(count.min(MAX_AXIS_LABELS))
            .y_labels(6)
            .x_label_formatter(&category_labels(&data.labels))
            .y_label_formatter(&|value| format_number(*value, options.decimal_places))
            .x_desc(x_title.clone())
            .y_desc(y_title.clone())
            .label_style(label_style(12))
            .draw()
            .map_err(|reason| error(format!("图表坐标轴绘制失败：{reason}")))?;
        let group = data.series.len().max(1);
        for (index, series) in data.series.iter().enumerate() {
            let color = SERIES_COLORS[index % SERIES_COLORS.len()];
            let points = scaled(&series.values, options.unit.as_deref());
            if bars {
                // 分组柱宽按系列数均分；负值自然向下延伸到零线。
                let slot = 0.72 / group as f64;
                let offset = -0.36 + slot * index as f64;
                let baseline = 0.0f64.clamp(min, max);
                chart
                    .draw_series(points.iter().enumerate().map(|(position, value)| {
                        let left = position as f64 + offset;
                        Rectangle::new(
                            [(left, baseline), (left + slot * 0.9, *value)],
                            color.filled(),
                        )
                    }))
                    .map_err(|reason| error(format!("图表柱体绘制失败：{reason}")))?
                    .label(series.name.clone())
                    .legend(move |(x, y)| {
                        Rectangle::new([(x, y - 6), (x + 14, y + 6)], color.filled())
                    });
            } else {
                chart
                    .draw_series(LineSeries::new(
                        points
                            .iter()
                            .enumerate()
                            .map(|(position, value)| (position as f64, *value)),
                        color.stroke_width(3),
                    ))
                    .map_err(|reason| error(format!("图表折线绘制失败：{reason}")))?
                    .label(series.name.clone())
                    .legend(move |(x, y)| {
                        PathElement::new(vec![(x, y), (x + 18, y)], color.stroke_width(3))
                    });
                chart
                    .draw_series(
                        points
                            .iter()
                            .enumerate()
                            .map(|(position, value)| {
                                Circle::new((position as f64, *value), 4, color.filled())
                            })
                            .collect::<Vec<_>>(),
                    )
                    .map_err(|reason| error(format!("图表数据点绘制失败：{reason}")))?;
            }
            if options.show_values {
                chart
                    .draw_series(points.iter().enumerate().map(|(position, value)| {
                        Text::new(
                            format_number(*value, options.decimal_places),
                            (position as f64, *value),
                            label_style(11),
                        )
                    }))
                    .map_err(|reason| error(format!("图表数值标签绘制失败：{reason}")))?;
            }
        }
        if multi {
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(GRID)
                .label_font(label_style(12))
                .draw()
                .map_err(|reason| error(format!("图表图例绘制失败：{reason}")))?;
        }
        draw_overlays(&mut chart, options, min, max, count)?;
        Ok(())
    })
}

fn draw_overlays<DB: DrawingBackend>(
    chart: &mut ChartContext<'_, DB, Cartesian2d<RangedCoordf64, RangedCoordf64>>,
    options: &ChartOptions,
    min: f64,
    max: f64,
    count: usize,
) -> Result<(), String> {
    if let Some(line) = options.reference_line.filter(|value| value.is_finite()) {
        if (min..=max).contains(&line) {
            chart
                .draw_series(std::iter::once(PathElement::new(
                    vec![(-0.6, line), (count as f64 - 0.4, line)],
                    NEGATIVE.stroke_width(2),
                )))
                .map_err(|reason| error(format!("图表参考线绘制失败：{reason}")))?;
        }
    }
    if let (Some(lower), Some(upper)) = (&options.interval_lower, &options.interval_upper) {
        if lower.len() == count && upper.len() == count {
            let band = lower
                .iter()
                .zip(upper)
                .enumerate()
                .filter_map(|(index, (low, high))| {
                    (low.is_finite() && high.is_finite() && low <= high).then_some((
                        index as f64,
                        *low,
                        *high,
                    ))
                })
                .collect::<Vec<_>>();
            chart
                .draw_series(band.into_iter().map(|(x, low, high)| {
                    Rectangle::new(
                        [(x - 0.25, low), (x + 0.25, high)],
                        RGBColor(0x1f, 0x6f, 0x54).mix(0.15).filled(),
                    )
                }))
                .map_err(|reason| error(format!("图表区间带绘制失败：{reason}")))?;
        }
    }
    if let Some(annotation) = &options.annotation {
        chart
            .draw_series(std::iter::once(Text::new(
                annotation.clone(),
                (0.0, max),
                label_style(12),
            )))
            .map_err(|reason| error(format!("图表注释绘制失败：{reason}")))?;
    }
    Ok(())
}

pub(super) fn render_special_chart(
    data: &ChartData,
    kind: &str,
    options: &ChartOptions,
) -> Result<String, String> {
    match kind {
        "散点图" => render_scatter(data, options),
        "直方图" => render_histogram(data, options),
        "箱线图" => render_boxplot(data, options),
        "堆叠柱状图" => render_stacked(data, options),
        "瀑布图" => render_waterfall(data, options),
        "相关热力图" => render_heatmap(data, options),
        _ => render_chart_svg(data, false, options),
    }
}

fn render_scatter(data: &ChartData, options: &ChartOptions) -> Result<String, String> {
    let points = scaled(&data.values, options.unit.as_deref());
    let (min, max) = domain(&points, false);
    let count = data.labels.len().max(1);
    let y_title = y_axis_title(options);
    let x_title = options.x_axis_name.clone().unwrap_or_default();
    let description = format!("散点图，共 {} 个数据点", points.len());
    draw(options, &description, |area| {
        let mut chart = ChartBuilder::on(area)
            .margin(16)
            .x_label_area_size(x_label_area(count))
            .y_label_area_size(88)
            .build_cartesian_2d(-0.6f64..count as f64 - 0.4, min..max)
            .map_err(|reason| error(format!("图表坐标系构建失败：{reason}")))?;
        chart
            .configure_mesh()
            .light_line_style(GRID)
            .bold_line_style(GRID)
            .axis_style(AXIS)
            .x_labels(count.min(MAX_AXIS_LABELS))
            .y_labels(6)
            .x_label_formatter(&category_labels(&data.labels))
            .y_label_formatter(&|value| format_number(*value, options.decimal_places))
            .x_desc(x_title.clone())
            .y_desc(y_title.clone())
            .label_style(label_style(12))
            .draw()
            .map_err(|reason| error(format!("图表坐标轴绘制失败：{reason}")))?;
        // 散点图只画点；旧实现是先画折线再用字符串裁掉 <polyline>，那是个 hack。
        chart
            .draw_series(
                points
                    .iter()
                    .enumerate()
                    .map(|(position, value)| {
                        Circle::new((position as f64, *value), 5, POSITIVE.filled())
                    })
                    .collect::<Vec<_>>(),
            )
            .map_err(|reason| error(format!("图表散点绘制失败：{reason}")))?;
        if options.show_values {
            chart
                .draw_series(points.iter().enumerate().map(|(position, value)| {
                    Text::new(
                        format_number(*value, options.decimal_places),
                        (position as f64, *value),
                        label_style(11),
                    )
                }))
                .map_err(|reason| error(format!("图表数值标签绘制失败：{reason}")))?;
        }
        Ok(())
    })
}

fn render_histogram(data: &ChartData, options: &ChartOptions) -> Result<String, String> {
    let values = scaled(&data.values, options.unit.as_deref());
    let bins = 10usize.min(values.len().max(1));
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let width = ((max - min) / bins as f64).max(f64::EPSILON);
    let mut counts = vec![0usize; bins];
    for value in &values {
        let index = (((value - min) / width) as usize).min(bins - 1);
        counts[index] += 1;
    }
    let max_count = counts.iter().copied().max().unwrap_or(1);
    let description = format!(
        "直方图，共 {} 个数据点、{bins} 个区间；柱顶标注频数，横轴为区间、纵轴为频数",
        values.len()
    );
    let edges = (0..=bins)
        .map(|index| min + width * index as f64)
        .collect::<Vec<_>>();
    let places = options.decimal_places;
    draw(options, &description, |area| {
        let mut chart = ChartBuilder::on(area)
            .margin(16)
            .x_label_area_size(76)
            .y_label_area_size(88)
            .build_cartesian_2d(-0.6f64..bins as f64 - 0.4, 0f64..max_count as f64 * 1.15)
            .map_err(|reason| error(format!("图表坐标系构建失败：{reason}")))?;
        chart
            .configure_mesh()
            .light_line_style(GRID)
            .bold_line_style(GRID)
            .axis_style(AXIS)
            .x_labels(bins.min(MAX_AXIS_LABELS))
            .y_labels(6)
            .x_label_formatter(&|position| {
                let index = position.round();
                if (position - index).abs() > 1e-6 || index < 0.0 {
                    return String::new();
                }
                let index = index as usize;
                match (edges.get(index), edges.get(index + 1)) {
                    (Some(low), Some(high)) => format!(
                        "{}-{}",
                        format_number(*low, places),
                        format_number(*high, places)
                    ),
                    _ => String::new(),
                }
            })
            .y_label_formatter(&|value| format!("{}", value.round() as i64))
            .x_desc("区间")
            .y_desc("频数")
            .label_style(label_style(11))
            .draw()
            .map_err(|reason| error(format!("图表坐标轴绘制失败：{reason}")))?;
        chart
            .draw_series(counts.iter().enumerate().map(|(index, count)| {
                Rectangle::new(
                    [
                        (index as f64 - 0.36, 0.0),
                        (index as f64 + 0.36, *count as f64),
                    ],
                    RGBColor(0x00, 0x7c, 0x91).filled(),
                )
            }))
            .map_err(|reason| error(format!("图表柱体绘制失败：{reason}")))?;
        chart
            .draw_series(counts.iter().enumerate().map(|(index, count)| {
                Text::new(
                    count.to_string(),
                    (index as f64, *count as f64),
                    label_style(11),
                )
            }))
            .map_err(|reason| error(format!("图表频数标签绘制失败：{reason}")))?;
        Ok(())
    })
}

/// 线性插值分位数，替换旧实现里 `values[len/4]` 的粗糙取值。
fn quantile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let position = fraction * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        return sorted[lower];
    }
    let weight = position - lower as f64;
    sorted[lower] * (1.0 - weight) + sorted[upper] * weight
}

fn render_boxplot(data: &ChartData, options: &ChartOptions) -> Result<String, String> {
    let mut values = scaled(&data.values, options.unit.as_deref());
    values.sort_by(f64::total_cmp);
    let min = values[0];
    let max = values[values.len() - 1];
    let q1 = quantile(&values, 0.25);
    let median = quantile(&values, 0.5);
    let q3 = quantile(&values, 0.75);
    let (axis_min, axis_max) = domain(&values, false);
    let y_title = y_axis_title(options);
    let sample_note = format!("样本数 {}", values.len());
    let description = format!("箱线图，共 {} 个数据点；标注五数概括和样本数", values.len());
    let places = options.decimal_places;
    draw(options, &description, |area| {
        let mut chart = ChartBuilder::on(area)
            .margin(16)
            .x_label_area_size(52)
            .y_label_area_size(88)
            .build_cartesian_2d(-1.0f64..1.2f64, axis_min..axis_max)
            .map_err(|reason| error(format!("图表坐标系构建失败：{reason}")))?;
        chart
            .configure_mesh()
            .light_line_style(GRID)
            .bold_line_style(GRID)
            .axis_style(AXIS)
            .x_labels(1)
            .y_labels(6)
            .x_label_formatter(&|_| String::new())
            .y_label_formatter(&|value| format_number(*value, places))
            .x_desc(sample_note.clone())
            .y_desc(y_title.clone())
            .label_style(label_style(12))
            .draw()
            .map_err(|reason| error(format!("图表坐标轴绘制失败：{reason}")))?;
        // 须线、箱体和中位线都是 Plotters 图元组合，不再手写 SVG 坐标。
        chart
            .draw_series(std::iter::once(PathElement::new(
                vec![(0.0, min), (0.0, max)],
                RGBColor(0x00, 0x7c, 0x91).stroke_width(3),
            )))
            .map_err(|reason| error(format!("图表须线绘制失败：{reason}")))?;
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(-0.35, q1), (0.35, q3)],
                RGBColor(0x8e, 0xcb, 0xd3).filled(),
            )))
            .map_err(|reason| error(format!("图表箱体绘制失败：{reason}")))?;
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(-0.35, q1), (0.35, q3)],
                RGBColor(0x00, 0x5d, 0x6c).stroke_width(2),
            )))
            .map_err(|reason| error(format!("图表箱体边框绘制失败：{reason}")))?;
        chart
            .draw_series(std::iter::once(PathElement::new(
                vec![(-0.35, median), (0.35, median)],
                RGBColor(0x17, 0x3b, 0x43).stroke_width(4),
            )))
            .map_err(|reason| error(format!("图表中位线绘制失败：{reason}")))?;
        let annotations = [
            ("最大值：", max),
            ("P75：", q3),
            ("中位数：", median),
            ("P25：", q1),
            ("最小值：", min),
        ];
        chart
            .draw_series(annotations.into_iter().map(|(name, value)| {
                Text::new(
                    format!("{name}{}", format_number(value, places)),
                    (0.45, value),
                    label_style(12),
                )
            }))
            .map_err(|reason| error(format!("图表统计标注绘制失败：{reason}")))?;
        Ok(())
    })
}

fn render_stacked(data: &ChartData, options: &ChartOptions) -> Result<String, String> {
    let scale = unit_scale(options.unit.as_deref());
    let count = data.labels.len().max(1);
    // 堆叠只表达非负构成；负值在堆叠语义下无法叠加，按零处理。
    let totals = (0..data.labels.len())
        .map(|index| {
            data.series
                .iter()
                .map(|series| series.values[index].max(0.0) / scale)
                .sum::<f64>()
        })
        .collect::<Vec<_>>();
    let (_, max) = domain(&totals, true);
    let y_title = y_axis_title(options);
    let description = format!(
        "堆叠柱状图，共 {} 个分类、{} 个系列",
        data.labels.len(),
        data.series.len()
    );
    draw(options, &description, |area| {
        let mut chart = ChartBuilder::on(area)
            .margin(16)
            .x_label_area_size(x_label_area(count))
            .y_label_area_size(88)
            .build_cartesian_2d(-0.6f64..count as f64 - 0.4, 0f64..max)
            .map_err(|reason| error(format!("图表坐标系构建失败：{reason}")))?;
        chart
            .configure_mesh()
            .light_line_style(GRID)
            .bold_line_style(GRID)
            .axis_style(AXIS)
            .x_labels(count.min(MAX_AXIS_LABELS))
            .y_labels(6)
            .x_label_formatter(&category_labels(&data.labels))
            .y_label_formatter(&|value| format_number(*value, options.decimal_places))
            .y_desc(y_title.clone())
            .label_style(label_style(12))
            .draw()
            .map_err(|reason| error(format!("图表坐标轴绘制失败：{reason}")))?;
        let mut offsets = vec![0.0f64; data.labels.len()];
        for (index, series) in data.series.iter().enumerate() {
            let color = SERIES_COLORS[index % SERIES_COLORS.len()];
            let bars = series
                .values
                .iter()
                .enumerate()
                .map(|(position, value)| {
                    let height = (value / scale).max(0.0);
                    let bottom = offsets[position];
                    offsets[position] = bottom + height;
                    Rectangle::new(
                        [
                            (position as f64 - 0.36, bottom),
                            (position as f64 + 0.36, bottom + height),
                        ],
                        color.filled(),
                    )
                })
                .collect::<Vec<_>>();
            chart
                .draw_series(bars)
                .map_err(|reason| error(format!("图表柱体绘制失败：{reason}")))?
                .label(series.name.clone())
                .legend(move |(x, y)| {
                    Rectangle::new([(x, y - 6), (x + 14, y + 6)], color.filled())
                });
        }
        chart
            .configure_series_labels()
            .background_style(WHITE.mix(0.85))
            .border_style(GRID)
            .label_font(label_style(12))
            .draw()
            .map_err(|reason| error(format!("图表图例绘制失败：{reason}")))?;
        Ok(())
    })
}

fn render_waterfall(data: &ChartData, options: &ChartOptions) -> Result<String, String> {
    let steps = scaled(&data.values, options.unit.as_deref());
    let mut cumulative = 0.0f64;
    let mut spans = Vec::with_capacity(steps.len());
    let mut extremes = vec![0.0f64];
    for value in &steps {
        let next = cumulative + value;
        spans.push((cumulative, next));
        extremes.push(next);
        cumulative = next;
    }
    let (min, max) = domain(&extremes, true);
    let count = data.labels.len().max(1);
    let y_title = y_axis_title(options);
    let description = format!(
        "瀑布图，共 {} 个变化项；绿色为增加、红色为减少",
        steps.len()
    );
    draw(options, &description, |area| {
        let mut chart = ChartBuilder::on(area)
            .margin(16)
            .x_label_area_size(x_label_area(count))
            .y_label_area_size(88)
            .build_cartesian_2d(-0.6f64..count as f64 - 0.4, min..max)
            .map_err(|reason| error(format!("图表坐标系构建失败：{reason}")))?;
        chart
            .configure_mesh()
            .light_line_style(GRID)
            .bold_line_style(GRID)
            .axis_style(AXIS)
            .x_labels(count.min(MAX_AXIS_LABELS))
            .y_labels(6)
            .x_label_formatter(&category_labels(&data.labels))
            .y_label_formatter(&|value| format_number(*value, options.decimal_places))
            .y_desc(y_title.clone())
            .label_style(label_style(12))
            .draw()
            .map_err(|reason| error(format!("图表坐标轴绘制失败：{reason}")))?;
        chart
            .draw_series(spans.iter().enumerate().map(|(index, (from, to))| {
                let color = if to >= from { POSITIVE } else { NEGATIVE };
                Rectangle::new(
                    [(index as f64 - 0.36, *from), (index as f64 + 0.36, *to)],
                    color.filled(),
                )
            }))
            .map_err(|reason| error(format!("图表柱体绘制失败：{reason}")))?;
        if options.show_values {
            chart
                .draw_series(spans.iter().enumerate().map(|(index, (_, to))| {
                    Text::new(
                        format_number(steps[index], options.decimal_places),
                        (index as f64, *to),
                        label_style(11),
                    )
                }))
                .map_err(|reason| error(format!("图表数值标签绘制失败：{reason}")))?;
        }
        Ok(())
    })
}

fn render_heatmap(data: &ChartData, options: &ChartOptions) -> Result<String, String> {
    let rows = data.series.len().max(1);
    let columns = data.labels.len().max(1);
    let max_abs = data
        .series
        .iter()
        .flat_map(|series| series.values.iter())
        .map(|value| value.abs())
        .fold(0.0f64, f64::max)
        .max(f64::EPSILON);
    let description = format!("相关系数热力图，共 {rows} 行、{columns} 列；单元格标注数值");
    let row_names = data
        .series
        .iter()
        .map(|series| series.name.clone())
        .collect::<Vec<_>>();
    draw(options, &description, |area| {
        let mut chart = ChartBuilder::on(area)
            .margin(16)
            .x_label_area_size(76)
            .y_label_area_size(150)
            .build_cartesian_2d(-0.5f64..columns as f64 - 0.5, rows as f64 - 0.5..-0.5f64)
            .map_err(|reason| error(format!("图表坐标系构建失败：{reason}")))?;
        chart
            .configure_mesh()
            .disable_mesh()
            .axis_style(AXIS)
            .x_labels(columns.min(MAX_AXIS_LABELS))
            .y_labels(rows.min(MAX_AXIS_LABELS))
            .x_label_formatter(&category_labels(&data.labels))
            .y_label_formatter(&category_labels(&row_names))
            .label_style(label_style(11))
            .draw()
            .map_err(|reason| error(format!("图表坐标轴绘制失败：{reason}")))?;
        for (row, series) in data.series.iter().enumerate() {
            let cells = series
                .values
                .iter()
                .enumerate()
                .map(|(column, value)| {
                    let intensity = (value.abs() / max_abs).min(1.0);
                    // 正相关走绿色、负相关走红色；数值同时写在单元格里，不只靠颜色区分。
                    let color = if *value >= 0.0 {
                        RGBColor(
                            (31.0 + 180.0 * (1.0 - intensity)) as u8,
                            (111.0 + 110.0 * (1.0 - intensity)) as u8,
                            (84.0 + 140.0 * (1.0 - intensity)) as u8,
                        )
                    } else {
                        RGBColor(
                            (181.0 + 60.0 * (1.0 - intensity)) as u8,
                            (72.0 + 130.0 * (1.0 - intensity)) as u8,
                            (72.0 + 130.0 * (1.0 - intensity)) as u8,
                        )
                    };
                    Rectangle::new(
                        [
                            (column as f64 - 0.5, row as f64 - 0.5),
                            (column as f64 + 0.5, row as f64 + 0.5),
                        ],
                        color.filled(),
                    )
                })
                .collect::<Vec<_>>();
            chart
                .draw_series(cells)
                .map_err(|reason| error(format!("图表热力单元格绘制失败：{reason}")))?;
            chart
                .draw_series(series.values.iter().enumerate().map(|(column, value)| {
                    Text::new(
                        format!("{value:.2}"),
                        (column as f64, row as f64),
                        label_style(10),
                    )
                }))
                .map_err(|reason| error(format!("图表热力标注绘制失败：{reason}")))?;
        }
        Ok(())
    })
}

fn error(message: impl AsRef<str>) -> String {
    message.as_ref().to_string()
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: Vec<BTreeMap<String, Value>>) -> Value {
        Value::Table(std::rc::Rc::new(std::cell::RefCell::new(
            crate::value::Table {
                rows,
                ..Default::default()
            },
        )))
    }

    fn wide_row(series: usize) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::from([("月份".to_string(), Value::Text("2026-01".into()))]);
        for index in 0..series {
            row.insert(format!("系列{index}"), Value::Float(index as f64));
        }
        row
    }

    /// Plotters 把文本内容单独放一行（`>\n内容\n</text>`），
    /// 所以断言要比对文本节点内容，而不是拼 `>内容</text>` 子串。
    fn texts(svg: &str) -> Vec<String> {
        svg.split("<text")
            .skip(1)
            .filter_map(|chunk| {
                let start = chunk.find('>')? + 1;
                let end = chunk.find("</text>")?;
                if end < start {
                    return None;
                }
                Some(chunk[start..end].trim().to_string())
            })
            .collect()
    }

    fn has_text(svg: &str, expected: &str) -> bool {
        texts(svg).iter().any(|text| text == expected)
    }

    fn options(title: &str) -> ChartOptions {
        ChartOptions {
            title: title.into(),
            x_axis_name: None,
            y_axis_name: None,
            unit: None,
            decimal_places: 2,
            show_values: true,
            reference_line: None,
            interval_lower: None,
            interval_upper: None,
            annotation: None,
        }
    }

    #[test]
    fn line_charts_draw_reference_lines_intervals_and_annotations() {
        let data = sample(&["一月", "二月"], &[("销售额", vec![10.0, 12.0])]);
        let options = ChartOptions {
            reference_line: Some(11.0),
            interval_lower: Some(vec![9.0, 10.0]),
            interval_upper: Some(vec![11.0, 14.0]),
            annotation: Some("目标线".into()),
            ..options("带标注")
        };
        let svg = render_chart_svg(&data, false, &options).unwrap();
        assert!(svg.contains("目标线"));
        assert!(svg.matches("<rect").count() >= 1);
        assert!(svg.contains("11"));
    }

    fn sample(labels: &[&str], series: &[(&str, Vec<f64>)]) -> ChartData {
        ChartData {
            labels: labels.iter().map(ToString::to_string).collect(),
            values: series[0].1.clone(),
            series: series
                .iter()
                .map(|(name, values)| ChartSeries {
                    name: (*name).into(),
                    values: values.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn chart_data_rejects_too_many_series_before_allocating_them() {
        let source = table(vec![wide_row(MAX_CHART_SERIES + 1)]);
        let names = (0..MAX_CHART_SERIES + 1)
            .map(|index| Value::Text(format!("系列{index}")))
            .collect::<Vec<_>>();
        let message = chart_data(
            &source,
            &BTreeMap::from([
                ("横轴".to_string(), Value::Text("月份".into())),
                ("系列".to_string(), Value::list(names)),
            ]),
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
            "折线图",
        )
        .unwrap_err();
        assert!(message.contains("超过可读上限"), "{message}");

        // 恰好等于上限时必须仍然可用，边界不能收紧成 off-by-one。
        let source = table(vec![wide_row(MAX_CHART_SERIES)]);
        let names = (0..MAX_CHART_SERIES)
            .map(|index| Value::Text(format!("系列{index}")))
            .collect::<Vec<_>>();
        let data = chart_data(
            &source,
            &BTreeMap::from([
                ("横轴".to_string(), Value::Text("月份".into())),
                ("系列".to_string(), Value::list(names)),
            ]),
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
            "折线图",
        )
        .unwrap();
        assert_eq!(data.series.len(), MAX_CHART_SERIES);
    }

    #[test]
    fn chart_data_bounds_rows_and_total_points_by_the_collection_budget() {
        let rows = (0..8)
            .map(|index| {
                BTreeMap::from([
                    ("月份".to_string(), Value::Text(format!("2026-{index:02}"))),
                    ("销售额".to_string(), Value::Float(index as f64)),
                ])
            })
            .collect::<Vec<_>>();
        let source = table(rows);
        let named = BTreeMap::from([
            ("横轴".to_string(), Value::Text("月份".into())),
            ("纵轴".to_string(), Value::Text("销售额".into())),
        ]);
        let message = chart_data(&source, &named, 4, "折线图").unwrap_err();
        assert!(message.contains("超过集合预算"), "{message}");

        // 行数在预算内，但行数×系列数超预算时也必须拒绝。
        let two_series = BTreeMap::from([
            ("横轴".to_string(), Value::Text("月份".into())),
            (
                "纵轴".to_string(),
                Value::list(vec![
                    Value::Text("销售额".into()),
                    Value::Text("月份".into()),
                ]),
            ),
        ]);
        let message = chart_data(&source, &two_series, 8, "折线图").unwrap_err();
        assert!(message.contains("超过集合预算"), "{message}");

        assert_eq!(
            chart_data(&source, &named, 8, "折线图")
                .unwrap()
                .values
                .len(),
            8
        );
    }

    #[test]
    fn chart_data_rejects_non_finite_and_non_numeric_series() {
        let source = table(vec![
            BTreeMap::from([
                ("月份".to_string(), Value::Text("2026-01".into())),
                ("销售额".to_string(), Value::Float(f64::NAN)),
            ]),
            BTreeMap::from([
                ("月份".to_string(), Value::Text("2026-02".into())),
                ("销售额".to_string(), Value::Float(1.0)),
            ]),
        ]);
        let named = BTreeMap::from([
            ("横轴".to_string(), Value::Text("月份".into())),
            ("纵轴".to_string(), Value::Text("销售额".into())),
        ]);
        let message = chart_data(
            &source,
            &named,
            crate::resource::DEFAULT_MAX_COLLECTION_ITEMS,
            "折线图",
        )
        .unwrap_err();
        assert!(message.contains("有限数字"), "{message}");
    }

    /// Plotters 自己会转义它绘制的文本；我们注入的 title/desc 由 `annotate` 转义。
    /// 这两条路径都必须封住，否则恶意标题会变成 HTML 内联时的活动内容。
    #[test]
    fn every_chart_kind_escapes_hostile_titles_and_labels() {
        let data = sample(
            &["<script>alert(1)</script>", "正常"],
            &[("系列<A>", vec![1.0, 2.0]), ("系列&B", vec![-1.0, 3.0])],
        );
        let options = options("\"><script>bad</script>");
        let mut rendered = vec![
            render_chart_svg(&data, false, &options).unwrap(),
            render_chart_svg(&data, true, &options).unwrap(),
        ];
        for kind in [
            "散点图",
            "直方图",
            "箱线图",
            "堆叠柱状图",
            "瀑布图",
            "相关热力图",
        ] {
            rendered.push(render_special_chart(&data, kind, &options).unwrap());
        }
        for svg in &rendered {
            assert!(!svg.contains("<script>"), "SVG 不能透出未转义脚本");
            assert!(svg.contains("&lt;script&gt;"), "标题必须转义后出现");
            // HTML 内联和 PDF 嵌入都要求没有外部引用和主动内容。
            assert!(!svg.contains("href"), "SVG 不能引用外部资源");
            assert!(!svg.contains("<?xml"), "SVG 不能带处理指令");
            assert!(!svg.contains("<!DOCTYPE"), "SVG 不能带 DOCTYPE");
            assert!(svg.contains("role=\"img\""), "缺少可访问性 role");
            assert!(svg.contains("<title>"), "缺少 title");
            assert!(svg.contains("<desc>"), "缺少 desc");
        }
    }

    #[test]
    fn line_chart_draws_plotters_geometry_axes_and_point_markers() {
        let svg = chart_svg(&[1.0, 3.0, 2.0], false, "趋势").unwrap();
        assert!(
            svg.contains("<polyline"),
            "折线由 Plotters 的 polyline 绘制"
        );
        assert!(svg.contains("<circle"), "数据点标记");
        assert!(svg.contains("趋势"));
        assert!(svg.contains("role=\"img\""));
        assert!(has_text(&svg, "1"), "类别标签来自格式化器");
        assert!(has_text(&svg, "3"), "末尾类别标签也要绘制");
    }

    #[test]
    fn bar_chart_keeps_negative_values_and_constant_series_renderable() {
        let negative = sample(
            &["一月", "二月", "三月"],
            &[("净额", vec![-5.0, 3.0, -1.0])],
        );
        let svg = render_chart_svg(&negative, true, &options("净额")).unwrap();
        assert!(svg.contains("<rect"), "柱体由 Plotters 的 rect 绘制");
        assert!(has_text(&svg, "-5"), "负值标签应显示");

        // 常量数据在旧实现里会让值域退化；域计算必须给出非退化区间。
        let constant = sample(&["一月", "二月"], &[("持平", vec![7.0, 7.0])]);
        let svg = render_chart_svg(&constant, false, &options("持平")).unwrap();
        assert!(svg.contains("role=\"img\""));
        assert!(has_text(&svg, "7"));

        // 单点序列同样不能崩。
        let single = sample(&["唯一"], &[("值", vec![0.0])]);
        assert!(render_chart_svg(&single, true, &options("单点"))
            .unwrap()
            .contains("role=\"img\""));
    }

    #[test]
    fn multi_series_charts_draw_every_series_with_a_legend() {
        let data = sample(
            &["一月", "二月"],
            &[("销售额", vec![10.0, 12.0]), ("毛利额", vec![2.0, 3.0])],
        );
        let options = options("结构");
        let lines = render_chart_svg(&data, false, &options).unwrap();
        // 每个系列一条折线，图例再补两条短线，因此至少四条 polyline。
        assert!(
            lines.matches("<polyline").count() >= 2,
            "多系列必须逐系列绘制折线"
        );
        assert!(lines.contains("销售额"), "图例应包含系列名");
        assert!(lines.contains("毛利额"));

        let bars = render_chart_svg(&data, true, &options).unwrap();
        assert!(bars.contains("<rect"));
        assert!(bars.contains("销售额") && bars.contains("毛利额"));
    }

    #[test]
    fn long_labels_are_sampled_instead_of_overlapping() {
        let labels = (1..=40)
            .map(|index| format!("2026-{index:02}周"))
            .collect::<Vec<_>>();
        let data = ChartData {
            labels: labels.clone(),
            values: (1..=40).map(|value| value as f64).collect(),
            series: vec![ChartSeries {
                name: "周销量".into(),
                values: (1..=40).map(|value| value as f64).collect(),
            }],
        };
        let svg = render_chart_svg(&data, false, &options("周趋势")).unwrap();
        let drawn = labels
            .iter()
            .filter(|label| svg.contains(label.as_str()))
            .count();
        assert!(drawn > 0, "至少要画出一部分类别标签");
        assert!(
            drawn <= MAX_AXIS_LABELS,
            "标签数应被抽样到 {MAX_AXIS_LABELS} 以内，实际 {drawn}"
        );
    }

    #[test]
    fn axis_titles_carry_the_unit_and_values_are_scaled() {
        let data = sample(&["一月"], &[("销售额", vec![85_000_000.0])]);
        let svg = render_chart_svg(
            &data,
            false,
            &ChartOptions {
                title: "月度销售额".into(),
                x_axis_name: Some("月份".into()),
                y_axis_name: Some("销售额".into()),
                unit: Some("万元".into()),
                decimal_places: 2,
                show_values: true,
                reference_line: None,
                interval_lower: None,
                interval_upper: None,
                annotation: None,
            },
        )
        .unwrap();
        assert!(svg.contains("销售额（万元）"), "纵轴标题应带单位");
        assert!(svg.contains("月份"), "横轴标题");
        // 85,000,000 元换算成 8500 万元。
        assert!(has_text(&svg, "8500"), "单位换算后的数值标签");
    }

    #[test]
    fn distribution_charts_include_axes_units_sample_size_and_five_number_summary() {
        let data = sample(
            &["1", "2", "3", "4", "5"],
            &[("数值", vec![10.0, 20.0, 30.0, 40.0, 50.0])],
        );
        let options = ChartOptions {
            title: "销售额分布".into(),
            x_axis_name: None,
            y_axis_name: Some("销售额".into()),
            unit: Some("元".into()),
            decimal_places: 2,
            show_values: true,
            reference_line: None,
            interval_lower: None,
            interval_upper: None,
            annotation: None,
        };
        let histogram = render_special_chart(&data, "直方图", &options).unwrap();
        assert!(has_text(&histogram, "区间"));
        assert!(has_text(&histogram, "频数"));

        let boxplot = render_special_chart(&data, "箱线图", &options).unwrap();
        for label in ["最小值：", "P25：", "中位数：", "P75：", "最大值："] {
            assert!(boxplot.contains(label), "缺少统计标注 {label}");
        }
        assert!(boxplot.contains("样本数 5"), "箱线图必须标注样本数");
        assert!(boxplot.contains("销售额（元）"), "箱线图必须标注轴与单位");
        // 线性插值分位数：P25=20、中位数=30、P75=40。
        assert!(boxplot.contains("P25：20"), "{boxplot}");
        assert!(boxplot.contains("中位数：30"));
        assert!(boxplot.contains("P75：40"));
    }

    #[test]
    fn interpolated_quantiles_replace_the_old_index_division() {
        let sorted = [1.0, 2.0, 3.0, 4.0];
        // 旧实现取 values[len/4]=values[1]=2 当 P25；线性插值应给出 1.75。
        assert!((quantile(&sorted, 0.25) - 1.75).abs() < 1e-12);
        assert!((quantile(&sorted, 0.5) - 2.5).abs() < 1e-12);
        assert!((quantile(&sorted, 0.75) - 3.25).abs() < 1e-12);
        assert!((quantile(&[5.0], 0.5) - 5.0).abs() < 1e-12);
    }

    #[test]
    fn composite_charts_render_series_names_and_cell_values() {
        let data = sample(
            &["一月", "二月"],
            &[("销售额", vec![10.0, 12.0]), ("毛利额", vec![2.0, 3.0])],
        );
        let options = options("结构");
        let stacked = render_special_chart(&data, "堆叠柱状图", &options).unwrap();
        assert!(stacked.contains("销售额"), "堆叠图例");
        assert!(stacked.contains("毛利额"));
        assert!(stacked.contains("<rect"));

        let heatmap = render_special_chart(&data, "相关热力图", &options).unwrap();
        assert!(has_text(&heatmap, "10.00"), "热力图单元格标注数值");
        assert!(heatmap.contains("销售额"), "热力图行标签");

        let waterfall = render_special_chart(&data, "瀑布图", &options).unwrap();
        assert!(waterfall.contains("<rect"));

        let scatter = render_special_chart(&data, "散点图", &options).unwrap();
        assert!(scatter.contains("<circle"), "散点图必须有点");
        // 坐标轴框本身也是 polyline，所以用折线图作对照：折线图必须多出数据线。
        let lines = render_chart_svg(&data, false, &options).unwrap();
        assert!(
            lines.matches("<polyline").count() > scatter.matches("<polyline").count(),
            "散点图不应画连接线；旧实现是先画折线再用字符串裁掉 <polyline>"
        );
    }
}
