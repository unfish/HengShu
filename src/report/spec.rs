use crate::value::Value;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const REPORT_SPEC_VERSION: u32 = 1;

/// 未声明列规范时优先排在前面的键，保持既有报告的列顺序。
const PREFERRED_LEADING_COLUMNS: [&str; 4] = ["列", "周期", "分类", "原分类"];

/// 自动格式保留的最大小数位；显式格式由 `FormatSpec::decimal_places` 决定。
const AUTOMATIC_DECIMAL_PLACES: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum FormatKind {
    Automatic,
    Number,
    Money,
    Percentage,
    DateTime,
    Duration,
    Text,
}

impl FormatKind {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "自动" => Self::Automatic,
            "数字" => Self::Number,
            "金额" => Self::Money,
            "百分比" => Self::Percentage,
            "日期时间" => Self::DateTime,
            "持续时间" => Self::Duration,
            "文本" => Self::Text,
            other => return Err(format!("报告列格式不支持“{other}”")),
        })
    }

    fn numeric(self) -> bool {
        matches!(
            self,
            Self::Automatic | Self::Number | Self::Money | Self::Percentage | Self::Duration
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum NegativeStyle {
    Minus,
    Parentheses,
}

impl NegativeStyle {
    fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "负号" => Self::Minus,
            "括号" => Self::Parentheses,
            other => return Err(format!("报告负数样式不支持“{other}”")),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum Alignment {
    Left,
    Center,
    Right,
}

impl Alignment {
    fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "左" | "左对齐" => Self::Left,
            "中" | "居中" => Self::Center,
            "右" | "右对齐" => Self::Right,
            other => return Err(format!("报告列对齐不支持“{other}”")),
        })
    }

    pub(crate) fn css_class(self) -> &'static str {
        match self {
            Self::Left => "cell-left",
            Self::Center => "cell-center",
            Self::Right => "cell-right",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum TotalKind {
    Sum,
    Average,
    Count,
    Max,
    Min,
}

impl TotalKind {
    fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "求和" | "合计" => Self::Sum,
            "平均" => Self::Average,
            "计数" => Self::Count,
            "最大" => Self::Max,
            "最小" => Self::Min,
            other => return Err(format!("报告列汇总不支持“{other}”")),
        })
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Sum => "合计",
            Self::Average => "平均",
            Self::Count => "计数",
            Self::Max => "最大",
            Self::Min => "最小",
        }
    }

    fn apply(self, samples: &[f64]) -> Option<f64> {
        if self == Self::Count {
            return Some(samples.len() as f64);
        }
        if samples.is_empty() {
            return None;
        }
        Some(match self {
            Self::Sum => samples.iter().sum(),
            Self::Average => samples.iter().sum::<f64>() / samples.len() as f64,
            Self::Max => samples.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            Self::Min => samples.iter().copied().fold(f64::INFINITY, f64::min),
            Self::Count => unreachable!("计数已提前返回"),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct FormatSpec {
    pub kind: FormatKind,
    pub unit: Option<String>,
    pub decimal_places: Option<u8>,
    pub grouping: bool,
    pub negative: NegativeStyle,
    pub null: String,
}

impl Default for FormatSpec {
    fn default() -> Self {
        Self {
            kind: FormatKind::Automatic,
            unit: None,
            decimal_places: None,
            grouping: true,
            negative: NegativeStyle::Minus,
            null: "—".into(),
        }
    }
}

impl FormatSpec {
    fn from_record(record: &BTreeMap<String, Value>) -> Result<Self, String> {
        let mut format = Self::default();
        if let Some(kind) = record.get("类型") {
            format.kind = FormatKind::parse(&kind.to_string())?;
        }
        if let Some(unit) = record.get("单位").filter(present) {
            let unit = unit.to_string();
            if !unit.trim().is_empty() {
                format.unit = Some(unit);
            }
        }
        if let Some(places) = record.get("小数位").filter(present) {
            let places = crate::contracts::decimal_to_usize(places.as_decimal()?, "报告列小数位")?;
            if places > 8 {
                return Err("报告列小数位不能超过 8".into());
            }
            format.decimal_places = Some(places as u8);
        }
        if let Some(grouping) = record.get("千分位").filter(present) {
            format.grouping = grouping.truth();
        }
        if let Some(negative) = record.get("负数").filter(present) {
            format.negative = NegativeStyle::parse(&negative.to_string())?;
        }
        if let Some(null) = record.get("空值").filter(present) {
            format.null = null.to_string();
        }
        Ok(format)
    }

    fn places(&self) -> u32 {
        self.decimal_places
            .map(u32::from)
            .unwrap_or(AUTOMATIC_DECIMAL_PLACES)
    }

    /// 显式声明的小数位保留尾随零；自动格式去掉尾随零。
    fn trims_trailing_zeros(&self) -> bool {
        self.decimal_places.is_none()
    }

    fn suffix(&self) -> &'static str {
        if self.kind == FormatKind::Percentage {
            "%"
        } else {
            ""
        }
    }

    pub(crate) fn alignment(&self) -> Alignment {
        if self.kind.numeric() {
            Alignment::Right
        } else {
            Alignment::Left
        }
    }

    /// HTML/PDF/CSV 展示列共用的唯一文本渲染路径。
    pub(crate) fn render(&self, value: &Value) -> String {
        match value {
            Value::Null => self.null.clone(),
            Value::Bool(flag) => if *flag { "是" } else { "否" }.into(),
            Value::Money(money) => format!(
                "{} {}",
                money.currency,
                self.render_decimal(money.amount, Some(2))
            ),
            Value::Number(number) => {
                format!("{}{}", self.render_decimal(*number, None), self.suffix())
            }
            Value::Float(number) if number.is_finite() => {
                format!("{}{}", self.render_float(*number), self.suffix())
            }
            Value::NumericArray(array) if array.data.len() == 1 => {
                format!("{}{}", self.render_float(array.data[0]), self.suffix())
            }
            Value::List(values) if values.borrow().len() == 1 => {
                self.render(&values.borrow()[0].clone())
            }
            other => other.to_string(),
        }
    }

    fn render_float(&self, value: f64) -> String {
        match Decimal::from_f64_retain(value) {
            Some(decimal) => self.render_decimal(decimal, None),
            None => value.to_string(),
        }
    }

    fn render_decimal(&self, value: Decimal, places_override: Option<u32>) -> String {
        let places = places_override.unwrap_or_else(|| self.places());
        let rounded = value.round_dp(places);
        let mut text = format!("{rounded:.places$}", places = places as usize);
        if places > 0 && places_override.is_none() && self.trims_trailing_zeros() {
            text = text.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text.as_str()), |rest| (true, rest));
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let integer = if self.grouping {
            group_thousands(integer)
        } else {
            integer.to_string()
        };
        let magnitude = if fraction.is_empty() {
            integer
        } else {
            format!("{integer}.{fraction}")
        };
        if magnitude.chars().all(|c| c == '0' || c == '.' || c == ',') {
            return magnitude;
        }
        match (negative, self.negative) {
            (false, _) => magnitude,
            (true, NegativeStyle::Minus) => format!("-{magnitude}"),
            (true, NegativeStyle::Parentheses) => format!("({magnitude})"),
        }
    }

    /// XLSX 原生数字格式，让单元格保持数值而不是展示文本。
    pub(crate) fn xlsx_number_format(&self) -> Option<String> {
        if !self.kind.numeric() {
            return None;
        }
        let places = self.decimal_places.map(u32::from).unwrap_or(2) as usize;
        let integer = if self.grouping { "#,##0" } else { "0" };
        let mut pattern = if places == 0 {
            integer.to_string()
        } else {
            format!("{integer}.{}", "0".repeat(places))
        };
        if self.kind == FormatKind::Percentage {
            pattern.push_str("\"%\"");
        }
        if self.negative == NegativeStyle::Parentheses {
            pattern = format!("{pattern};({pattern})");
        }
        Some(pattern)
    }
}

fn present(value: &&Value) -> bool {
    !matches!(value, Value::Null)
}

fn group_thousands(integer: &str) -> String {
    let grouped = integer
        .chars()
        .rev()
        .enumerate()
        .flat_map(|(index, character)| {
            if index > 0 && index % 3 == 0 {
                vec![',', character]
            } else {
                vec![character]
            }
        })
        .collect::<String>();
    grouped.chars().rev().collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ColumnSpec {
    pub name: String,
    pub label: Option<String>,
    pub format: FormatSpec,
    pub width: Option<u16>,
    pub align: Option<Alignment>,
    pub total: Option<TotalKind>,
    pub description: Option<String>,
}

impl ColumnSpec {
    pub(crate) fn automatic(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            label: None,
            format: FormatSpec::default(),
            width: None,
            align: None,
            total: None,
            description: None,
        }
    }

    fn from_record(record: &BTreeMap<String, Value>) -> Result<Self, String> {
        let name = record
            .get("名称")
            .filter(present)
            .ok_or_else(|| "报告列规范缺少“名称”".to_string())?
            .to_string();
        let format = match record.get("格式") {
            Some(Value::Record(format)) => FormatSpec::from_record(&format.borrow())?,
            Some(value) if present(&value) => FormatSpec {
                kind: FormatKind::parse(&value.to_string())?,
                ..FormatSpec::default()
            },
            _ => FormatSpec::default(),
        };
        let width = record
            .get("宽度")
            .filter(present)
            .map(|value| {
                let width = crate::contracts::decimal_to_usize(value.as_decimal()?, "报告列宽度")?;
                u16::try_from(width.clamp(1, 400)).map_err(|_| "报告列宽度超出范围".to_string())
            })
            .transpose()?;
        Ok(Self {
            name,
            label: record.get("标题").filter(present).map(ToString::to_string),
            format,
            width,
            align: record
                .get("对齐")
                .filter(present)
                .map(|value| Alignment::parse(&value.to_string()))
                .transpose()?,
            total: record
                .get("汇总")
                .filter(|value| present(value) && value.to_string() != "无")
                .map(|value| TotalKind::parse(&value.to_string()))
                .transpose()?,
            description: record.get("说明").filter(present).map(ToString::to_string),
        })
    }

    /// 列标题带上单位，避免每个单元格重复单位文本。
    pub(crate) fn header(&self) -> String {
        let base = self.label.clone().unwrap_or_else(|| self.name.clone());
        match &self.format.unit {
            Some(unit) if !base.contains(unit.as_str()) => format!("{base}（{unit}）"),
            _ => base,
        }
    }

    pub(crate) fn alignment(&self) -> Alignment {
        self.align.unwrap_or_else(|| self.format.alignment())
    }

    pub(crate) fn render(&self, value: &Value) -> String {
        self.format.render(value)
    }

    pub(crate) fn sample(value: &Value) -> Option<f64> {
        match value {
            Value::Number(number) => number.to_f64(),
            Value::Money(money) => money.amount.to_f64(),
            Value::Float(number) if number.is_finite() => Some(*number),
            Value::NumericArray(array) if array.data.len() == 1 => Some(array.data[0]),
            _ => None,
        }
    }

    /// 汇总的原始数值，供 XLSX 写入原生数字；`None` 表示该列不汇总或没有可汇总样本。
    pub(crate) fn total_value(&self, rows: &[BTreeMap<String, Value>]) -> Option<f64> {
        let total = self.total?;
        let samples = rows
            .iter()
            .filter_map(|row| row.get(&self.name).and_then(Self::sample))
            .collect::<Vec<_>>();
        total.apply(&samples).filter(|value| value.is_finite())
    }

    /// 汇总值以列自身格式渲染，供 HTML/PDF 展示。
    pub(crate) fn total_text(&self, rows: &[BTreeMap<String, Value>]) -> Option<String> {
        let value = self.total_value(rows)?;
        Some(if self.total == Some(TotalKind::Count) {
            FormatSpec {
                kind: FormatKind::Number,
                decimal_places: Some(0),
                ..FormatSpec::default()
            }
            .render(&Value::Float(value))
        } else {
            self.format.render(&Value::Float(value))
        })
    }
}

/// 一个报告章节解析后的列与说明信息，四种导出器共用。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TableSpec {
    declared: Vec<ColumnSpec>,
    pub caption: Option<String>,
}

impl TableSpec {
    pub(crate) fn from_section(
        section: &BTreeMap<String, Value>,
        limit: usize,
    ) -> Result<Self, String> {
        let declared = match section.get("列规范").filter(present) {
            Some(columns) => columns
                .iter_values_limited(limit)
                .map_err(|_| "报告列规范必须是列表".to_string())?
                .iter()
                .map(|column| match column {
                    Value::Record(record) => ColumnSpec::from_record(&record.borrow()),
                    _ => Err("报告列规范的每一项必须是记录".into()),
                })
                .collect::<Result<Vec<_>, String>>()?,
            None => Vec::new(),
        };
        let mut seen = BTreeSet::new();
        for column in &declared {
            if !seen.insert(column.name.clone()) {
                return Err(format!("报告列规范重复声明“{}”", column.name));
            }
        }
        Ok(Self {
            declared,
            caption: section.get("说明").filter(present).map(ToString::to_string),
        })
    }

    pub(crate) fn has_declared_columns(&self) -> bool {
        !self.declared.is_empty()
    }

    /// 列顺序由列规范决定；未声明的键按既有启发式顺序追加，保证数据不丢失。
    pub(crate) fn resolve(&self, rows: &[BTreeMap<String, Value>]) -> Vec<ColumnSpec> {
        let mut discovered = rows
            .iter()
            .flat_map(|row| row.keys().cloned())
            .collect::<BTreeSet<_>>();
        let mut columns = Vec::with_capacity(discovered.len());
        for column in &self.declared {
            discovered.remove(&column.name);
            columns.push(column.clone());
        }
        if self.declared.is_empty() {
            for preferred in PREFERRED_LEADING_COLUMNS {
                if discovered.remove(preferred) {
                    columns.push(ColumnSpec::automatic(preferred));
                }
            }
        }
        columns.extend(discovered.into_iter().map(ColumnSpec::automatic));
        columns
    }

    pub(crate) fn column_for(&self, name: &str) -> ColumnSpec {
        self.declared
            .iter()
            .find(|column| column.name == name)
            .cloned()
            .unwrap_or_else(|| ColumnSpec::automatic(name))
    }

    pub(crate) fn descriptions(&self) -> Vec<(String, String)> {
        self.declared
            .iter()
            .filter_map(|column| {
                column
                    .description
                    .clone()
                    .map(|description| (column.header(), description))
            })
            .collect()
    }

    /// 汇总行标签取第一个声明汇总的列；`None` 表示本章节不输出汇总行。
    pub(crate) fn total_label(&self) -> Option<&'static str> {
        self.declared
            .iter()
            .find_map(|column| column.total)
            .map(TotalKind::label)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportTheme {
    Executive,
    Analysis,
    Audit,
}

impl ReportTheme {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "经营简报" => Ok(Self::Executive),
            "分析报告" => Ok(Self::Analysis),
            "审计底稿" => Ok(Self::Audit),
            _ => Err(format!(
                "报告主题只支持经营简报、分析报告或审计底稿，收到“{value}”"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Executive => "经营简报",
            Self::Analysis => "分析报告",
            Self::Audit => "审计底稿",
        }
    }
}

pub fn normalize_report(
    title: String,
    sections: Value,
    subtitle: Option<String>,
    theme: &str,
) -> Result<Value, String> {
    let theme = ReportTheme::parse(theme)?;
    if title.trim().is_empty() {
        return Err("报告标题不能为空".into());
    }
    validate_sections(&sections)?;
    Ok(Value::record(BTreeMap::from([
        (
            "版本".into(),
            Value::Number(rust_decimal::Decimal::from(REPORT_SPEC_VERSION)),
        ),
        ("标题".into(), Value::Text(title)),
        (
            "副标题".into(),
            subtitle.map(Value::Text).unwrap_or(Value::Null),
        ),
        ("主题".into(), Value::Text(theme.name().into())),
        ("章节".into(), sections),
        ("元数据".into(), Value::record(BTreeMap::new())),
    ])))
}

fn validate_sections(sections: &Value) -> Result<(), String> {
    let limit = crate::resource::DEFAULT_MAX_COLLECTION_ITEMS;
    let values = sections
        .iter_values_limited(limit)
        .map_err(|_| "报告章节必须是列表".to_string())?;
    for (index, section) in values.iter().enumerate() {
        let Value::Record(section) = section else {
            return Err(format!("报告第{}个章节必须是记录", index + 1));
        };
        let section = section.borrow();
        let kind = section
            .get("类型")
            .map(ToString::to_string)
            .unwrap_or_else(|| "文字".into());
        if !matches!(
            kind.as_str(),
            "指标" | "表格" | "图表" | "文字" | "提示" | "分页" | "附录" | "列表"
        ) {
            return Err(format!("报告章节类型不支持“{kind}”"));
        }
        if kind != "分页" && !section.contains_key("内容") {
            return Err(format!("报告第{}个章节缺少“内容”", index + 1));
        }
        TableSpec::from_section(&section, limit)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(entries: [(&str, Value); 1]) -> Value {
        Value::record(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    #[test]
    fn report_spec_has_explicit_version_theme_and_metadata() {
        let report =
            normalize_report("经营报告".into(), Value::list(Vec::new()), None, "经营简报").unwrap();
        let Value::Record(report) = report else {
            panic!("expected report")
        };
        let report = report.borrow();
        assert_eq!(report["版本"].to_string(), "1");
        assert_eq!(report["主题"].to_string(), "经营简报");
        assert!(matches!(report["元数据"], Value::Record(_)));
    }

    #[test]
    fn report_spec_rejects_unknown_section_and_column_format() {
        let sections = Value::list(vec![Value::record(BTreeMap::from([
            ("类型".into(), Value::Text("未知".into())),
            ("内容".into(), Value::Null),
        ]))]);
        assert!(normalize_report("报告".into(), sections, None, "分析报告")
            .unwrap_err()
            .contains("章节类型不支持"));
        let sections = Value::list(vec![Value::record(BTreeMap::from([
            ("类型".into(), Value::Text("表格".into())),
            ("内容".into(), Value::Null),
            (
                "列规范".into(),
                Value::list(vec![Value::record(BTreeMap::from([
                    ("名称".into(), Value::Text("金额".into())),
                    (
                        "格式".into(),
                        record([("类型", Value::Text("货币".into()))]),
                    ),
                ]))]),
            ),
        ]))]);
        assert!(normalize_report("报告".into(), sections, None, "分析报告")
            .unwrap_err()
            .contains("报告列格式不支持"));
    }

    #[test]
    fn format_spec_drives_places_grouping_negatives_and_null() {
        let money = FormatSpec {
            kind: FormatKind::Money,
            unit: Some("元".into()),
            decimal_places: Some(2),
            grouping: true,
            negative: NegativeStyle::Parentheses,
            null: "无".into(),
        };
        assert_eq!(
            money.render(&Value::Number(Decimal::new(123_456_789, 3))),
            "123,456.79"
        );
        assert_eq!(
            money.render(&Value::Number(Decimal::new(-45_600, 2))),
            "(456.00)"
        );
        assert_eq!(money.render(&Value::Null), "无");
        assert_eq!(
            money.xlsx_number_format().as_deref(),
            Some("#,##0.00;(#,##0.00)")
        );

        let percent = FormatSpec {
            kind: FormatKind::Percentage,
            decimal_places: Some(1),
            ..FormatSpec::default()
        };
        assert_eq!(percent.render(&Value::Float(12.345)), "12.3%");
        assert_eq!(
            percent.xlsx_number_format().as_deref(),
            Some("#,##0.0\"%\"")
        );

        let plain = FormatSpec {
            kind: FormatKind::Number,
            grouping: false,
            decimal_places: Some(0),
            ..FormatSpec::default()
        };
        assert_eq!(plain.render(&Value::Float(12345.6)), "12346");
        assert_eq!(plain.xlsx_number_format().as_deref(), Some("0"));

        let text = FormatSpec {
            kind: FormatKind::Text,
            ..FormatSpec::default()
        };
        assert_eq!(text.xlsx_number_format(), None);
        assert_eq!(text.alignment(), Alignment::Left);
    }

    #[test]
    fn automatic_format_keeps_natural_precision_without_column_name_guessing() {
        let automatic = FormatSpec::default();
        assert_eq!(
            automatic.render(&Value::Number(Decimal::new(12_345_678, 2))),
            "123,456.78"
        );
        assert_eq!(
            automatic.render(&Value::Number(Decimal::from(9_000))),
            "9,000"
        );
        assert_eq!(automatic.render(&Value::Bool(true)), "是");
        assert_eq!(automatic.render(&Value::Null), "—");
    }

    #[test]
    fn column_spec_drives_header_order_alignment_and_totals() {
        let section = BTreeMap::from([
            ("类型".to_string(), Value::Text("表格".into())),
            ("说明".to_string(), Value::Text("按月汇总".into())),
            (
                "列规范".to_string(),
                Value::list(vec![
                    Value::record(BTreeMap::from([
                        ("名称".into(), Value::Text("销售额".into())),
                        ("标题".into(), Value::Text("销售金额".into())),
                        (
                            "格式".into(),
                            Value::record(BTreeMap::from([
                                ("类型".into(), Value::Text("金额".into())),
                                ("单位".into(), Value::Text("元".into())),
                                ("小数位".into(), Value::Number(Decimal::from(2))),
                            ])),
                        ),
                        ("汇总".into(), Value::Text("求和".into())),
                        ("说明".into(), Value::Text("含税销售金额".into())),
                        ("宽度".into(), Value::Number(Decimal::from(18))),
                    ])),
                    Value::record(BTreeMap::from([
                        ("名称".into(), Value::Text("月份".into())),
                        ("格式".into(), Value::Text("文本".into())),
                        ("对齐".into(), Value::Text("居中".into())),
                    ])),
                ]),
            ),
        ]);
        let spec = TableSpec::from_section(&section, 1_000).unwrap();
        let rows = vec![
            BTreeMap::from([
                ("月份".to_string(), Value::Text("2026-01".into())),
                ("销售额".to_string(), Value::Number(Decimal::from(1_000))),
                ("备注".to_string(), Value::Text("正常".into())),
            ]),
            BTreeMap::from([
                ("月份".to_string(), Value::Text("2026-02".into())),
                ("销售额".to_string(), Value::Number(Decimal::from(2_500))),
            ]),
        ];
        let columns = spec.resolve(&rows);
        assert_eq!(
            columns.iter().map(ColumnSpec::header).collect::<Vec<_>>(),
            vec!["销售金额（元）", "月份", "备注"]
        );
        assert_eq!(columns[0].alignment(), Alignment::Right);
        assert_eq!(columns[1].alignment(), Alignment::Center);
        assert_eq!(columns[0].width, Some(18));
        assert_eq!(columns[0].total_text(&rows).as_deref(), Some("3,500.00"));
        assert_eq!(columns[1].total_text(&rows), None);
        assert_eq!(
            spec.descriptions(),
            vec![("销售金额（元）".to_string(), "含税销售金额".to_string())]
        );
        assert_eq!(spec.caption.as_deref(), Some("按月汇总"));
    }

    #[test]
    fn table_spec_rejects_duplicate_and_out_of_range_columns() {
        let duplicate = BTreeMap::from([(
            "列规范".to_string(),
            Value::list(vec![
                Value::record(BTreeMap::from([(
                    "名称".into(),
                    Value::Text("金额".into()),
                )])),
                Value::record(BTreeMap::from([(
                    "名称".into(),
                    Value::Text("金额".into()),
                )])),
            ]),
        )]);
        assert!(TableSpec::from_section(&duplicate, 1_000)
            .unwrap_err()
            .contains("重复声明"));

        let wide = BTreeMap::from([(
            "列规范".to_string(),
            Value::list(vec![Value::record(BTreeMap::from([
                ("名称".into(), Value::Text("金额".into())),
                (
                    "格式".into(),
                    Value::record(BTreeMap::from([(
                        "小数位".into(),
                        Value::Number(Decimal::from(12)),
                    )])),
                ),
            ]))]),
        )]);
        assert!(TableSpec::from_section(&wide, 1_000)
            .unwrap_err()
            .contains("小数位"));
    }
}
