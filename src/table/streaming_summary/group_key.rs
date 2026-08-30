use crate::stream::StreamRecord;
use std::cmp::Ordering;

use super::dimension::{transform_dimension, DimensionSpec};
use super::symbols::{StringInterner, Symbol};

pub(super) type GroupKey = Vec<Option<Symbol>>;

pub(super) fn dimension_key(
    record: &StreamRecord,
    dimensions: &[DimensionSpec],
    symbols: &mut StringInterner,
) -> Result<GroupKey, String> {
    let mut key = Vec::with_capacity(dimensions.len());
    for dimension in dimensions {
        let raw = record.get(dimension.index).unwrap_or("").trim();
        let value = transform_dimension(raw, dimension.transform, &dimension.column)?;
        key.push(
            value
                .as_deref()
                .map(|value| symbols.intern(value))
                .transpose()?,
        );
    }
    Ok(key)
}

pub(super) fn compare_symbol_keys(
    left: &[Option<Symbol>],
    right: &[Option<Symbol>],
    symbols: &StringInterner,
) -> Ordering {
    left.iter()
        .zip(right)
        .map(|(left, right)| match (left, right) {
            (Some(left), Some(right)) => symbols.resolve(*left).cmp(symbols.resolve(*right)),
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(symbols: &mut StringInterner, values: &[Option<&str>]) -> GroupKey {
        values
            .iter()
            .map(|value| value.map(|value| symbols.intern(value).unwrap()))
            .collect()
    }

    #[test]
    fn keys_order_by_resolved_text_and_place_missing_dimensions_first() {
        let mut symbols = StringInterner::default();
        let jiangsu = key(&mut symbols, &[Some("江苏")]);
        let zhejiang = key(&mut symbols, &[Some("浙江")]);
        let missing = key(&mut symbols, &[None]);
        assert_eq!(
            compare_symbol_keys(&jiangsu, &zhejiang, &symbols),
            "江苏".cmp("浙江")
        );
        assert_eq!(
            compare_symbol_keys(&missing, &jiangsu, &symbols),
            Ordering::Less
        );
        assert_eq!(
            compare_symbol_keys(&jiangsu, &jiangsu, &symbols),
            Ordering::Equal
        );
    }

    #[test]
    fn shorter_keys_sort_before_longer_prefixed_keys() {
        let mut symbols = StringInterner::default();
        let short = key(&mut symbols, &[Some("江苏")]);
        let long = key(&mut symbols, &[Some("江苏"), Some("南京")]);
        assert_eq!(compare_symbol_keys(&short, &long, &symbols), Ordering::Less);
    }
}
