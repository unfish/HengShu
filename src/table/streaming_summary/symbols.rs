use std::collections::HashMap;
use std::rc::Rc;

use super::language_error;

pub(super) type Symbol = u32;

#[derive(Default)]
pub(super) struct StringInterner {
    ids: HashMap<Rc<str>, Symbol>,
    values: Vec<Rc<str>>,
}

impl StringInterner {
    pub(super) fn intern(&mut self, value: &str) -> Result<Symbol, String> {
        if let Some(symbol) = self.ids.get(value) {
            return Ok(*symbol);
        }
        let symbol = Symbol::try_from(self.values.len())
            .map_err(|_| language_error("唯一文本值数量超过内部索引限制"))?;
        let value: Rc<str> = Rc::from(value);
        self.ids.insert(value.clone(), symbol);
        self.values.push(value);
        Ok(symbol)
    }

    pub(super) fn resolve(&self, symbol: Symbol) -> &str {
        self.values[symbol as usize].as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_stable_and_resolves_back_to_the_original_text() {
        let mut symbols = StringInterner::default();
        let first = symbols.intern("江苏").unwrap();
        let second = symbols.intern("浙江").unwrap();
        assert_eq!(symbols.intern("江苏").unwrap(), first);
        assert_ne!(first, second);
        assert_eq!(symbols.resolve(first), "江苏");
        assert_eq!(symbols.resolve(second), "浙江");
    }
}
