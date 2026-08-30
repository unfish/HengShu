use chrono::{Datelike, Duration, NaiveDate};
use std::borrow::Cow;

#[derive(Clone)]
pub(super) struct DimensionSpec {
    pub(super) column: String,
    pub(super) output: String,
    pub(super) index: usize,
    pub(super) transform: DimensionTransform,
}

#[derive(Clone, Copy)]
pub(super) enum DimensionTransform {
    Raw,
    Date,
    Week,
    Month,
    Quarter,
    Year,
}

pub(super) fn transform_dimension<'a>(
    value: &'a str,
    transform: DimensionTransform,
    column: &str,
) -> Result<Option<Cow<'a, str>>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    let prefix = |length: usize, label: &str| {
        value
            .get(..length)
            .map(Cow::Borrowed)
            .ok_or_else(|| format!("列“{column}”的值“{value}”不是有效{label}"))
    };
    Ok(Some(match transform {
        DimensionTransform::Raw => Cow::Borrowed(value),
        DimensionTransform::Date => prefix(10, "日期")?,
        DimensionTransform::Week => {
            let date = NaiveDate::parse_from_str(prefix(10, "日期")?.as_ref(), "%Y-%m-%d")
                .map_err(|_| format!("列“{column}”的值“{value}”不是有效日期"))?;
            let monday = date
                .checked_sub_signed(Duration::days(date.weekday().num_days_from_monday().into()))
                .ok_or_else(|| format!("列“{column}”的值“{value}”超出日期支持范围"))?;
            Cow::Owned(monday.format("%Y-%m-%d").to_string())
        }
        DimensionTransform::Month => prefix(7, "月份")?,
        DimensionTransform::Year => prefix(4, "年份")?,
        DimensionTransform::Quarter => {
            let year = prefix(4, "年份")?;
            let month = value
                .get(5..7)
                .and_then(|month| month.parse::<u8>().ok())
                .filter(|month| (1..=12).contains(month))
                .ok_or_else(|| format!("列“{column}”的值“{value}”不是有效月份"))?;
            Cow::Owned(format!("{year}-Q{}", (month - 1) / 3 + 1))
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_truncate_or_normalize_calendar_values() {
        let cases = [
            (DimensionTransform::Raw, "2026-01-02 08:00:00"),
            (DimensionTransform::Date, "2026-01-02"),
            (DimensionTransform::Week, "2025-12-29"),
            (DimensionTransform::Month, "2026-01"),
            (DimensionTransform::Quarter, "2026-Q1"),
            (DimensionTransform::Year, "2026"),
        ];
        for (transform, expected) in cases {
            let actual = transform_dimension("2026-01-02 08:00:00", transform, "日期")
                .unwrap()
                .unwrap();
            assert_eq!(actual.as_ref(), expected);
        }
    }

    #[test]
    fn empty_values_become_null_dimensions_and_short_values_are_rejected() {
        assert!(transform_dimension("", DimensionTransform::Date, "日期")
            .unwrap()
            .is_none());
        assert!(
            transform_dimension("2026", DimensionTransform::Date, "日期")
                .unwrap_err()
                .contains("不是有效日期")
        );
        assert!(
            transform_dimension("2026-13-01", DimensionTransform::Quarter, "日期")
                .unwrap_err()
                .contains("不是有效月份")
        );
    }
}
