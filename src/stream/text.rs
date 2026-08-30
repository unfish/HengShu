//! 文本解码层：把字节源解释为文本行。
//!
//! `LineDecoder` 是文本行这一层的接口，与二进制帧解码（`binary.rs`）和结构化记录解码
//! （`csv.rs`）并列。JSON Lines 解码器属于同一层：它在 `LineDecoder` 之上把每行解析为
//! 结构化值，因此将来实现时放在本模块，而不是塞进字节源或表格聚合。本轮不接入语言入口。

use super::{StreamSource, ValueStreamSource};
use crate::value::Value;
use std::io::BufRead;

pub(crate) trait LineDecoder {
    fn next_line(&mut self) -> Result<Option<String>, String>;
}

struct LineStream {
    reader: Box<dyn BufRead>,
    buffer_limit: usize,
}

pub(crate) fn lines(reader: impl BufRead + 'static, buffer_limit: usize) -> StreamSource {
    StreamSource::decoded(LineStream {
        reader: Box::new(reader),
        buffer_limit,
    })
}

impl LineDecoder for LineStream {
    fn next_line(&mut self) -> Result<Option<String>, String> {
        crate::resource::read_line_limited(&mut self.reader, self.buffer_limit)
            .map_err(|error| error.to_string())
            .map(|line| line.map(|text| text.trim_end_matches(['\r', '\n']).to_string()))
    }
}

impl ValueStreamSource for LineStream {
    fn next_value(&mut self) -> Result<Option<Value>, String> {
        Ok(self.next_line()?.map(Value::Text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    fn decoder(contents: &str, buffer_limit: usize) -> LineStream {
        LineStream {
            reader: Box::new(BufReader::new(Cursor::new(contents.to_string()))),
            buffer_limit,
        }
    }

    #[test]
    fn an_empty_source_yields_no_lines() {
        let mut stream = decoder("", 16);
        assert!(stream.next_line().unwrap().is_none());
        assert!(stream.next_line().unwrap().is_none());
    }

    #[test]
    fn a_single_unterminated_line_is_still_returned() {
        let mut stream = decoder("甲", 16);
        assert_eq!(stream.next_line().unwrap().as_deref(), Some("甲"));
        assert!(stream.next_line().unwrap().is_none());
    }

    #[test]
    fn multiple_lines_drop_both_line_ending_styles_and_keep_blank_lines() {
        let mut stream = decoder("甲\r\n\n乙\n", 16);
        assert_eq!(stream.next_line().unwrap().as_deref(), Some("甲"));
        assert_eq!(stream.next_line().unwrap().as_deref(), Some(""));
        assert_eq!(stream.next_line().unwrap().as_deref(), Some("乙"));
        assert!(stream.next_line().unwrap().is_none());
    }

    #[test]
    fn an_oversized_line_stops_the_stream_at_the_buffer_limit() {
        let mut stream = decoder("甲乙丙丁戊己庚辛\n", 4);
        assert!(stream.next_line().is_err());
    }

    #[test]
    fn the_value_stream_wraps_each_line_as_text() {
        let source = lines(BufReader::new(Cursor::new("甲\n乙\n")), 16);
        let StreamSource::Decoded(source) = source else {
            panic!("预期解码流")
        };
        for expected in ["甲", "乙"] {
            let value = source.borrow_mut().next_value().unwrap().unwrap();
            assert_eq!(value.to_string(), expected);
        }
        assert!(source.borrow_mut().next_value().unwrap().is_none());
    }
}
