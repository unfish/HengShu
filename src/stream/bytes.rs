//! 字节源层：把文件、HTTP body、内存缓冲等 `Read` 实现统一成分块字节流。
//!
//! 本层只负责取字节和守住缓冲上限，不解释任何格式；帧、文本行和结构化记录分别由
//! `binary.rs`、`text.rs` 和 `csv.rs` 解码。

use super::binary::ByteStream;
use super::{StreamSource, ValueStreamSource};
use crate::value::Value;
use std::io::Read;

pub(crate) trait ByteSource {
    fn read_bytes(&mut self, buffer: &mut [u8]) -> std::io::Result<usize>;
}

impl<T: Read> ByteSource for T {
    fn read_bytes(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.read(buffer)
    }
}

struct ReaderByteStream {
    reader: Box<dyn ByteSource>,
    chunk_size: usize,
    buffer_limit: usize,
}

pub(crate) fn byte_chunks(
    reader: impl Read + 'static,
    chunk_size: usize,
    buffer_limit: usize,
) -> Box<dyn ByteStream> {
    Box::new(ReaderByteStream {
        reader: Box::new(reader),
        chunk_size,
        buffer_limit,
    })
}

pub(crate) fn reader_chunks(
    reader: impl Read + 'static,
    chunk_size: usize,
    buffer_limit: usize,
) -> StreamSource {
    StreamSource::decoded(ByteValueStream {
        source: byte_chunks(reader, chunk_size, buffer_limit),
    })
}

impl ByteStream for ReaderByteStream {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.chunk_size > self.buffer_limit {
            return Err("流读取块大小超过缓冲上限".into());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(self.chunk_size)
            .map_err(|_| "流读取块大小超出可分配范围".to_string())?;
        bytes.resize(self.chunk_size, 0);
        let length = self
            .reader
            .read_bytes(&mut bytes)
            .map_err(|error| error.to_string())?;
        if length == 0 {
            Ok(None)
        } else {
            bytes.truncate(length);
            Ok(Some(bytes))
        }
    }
}

struct ByteValueStream {
    source: Box<dyn ByteStream>,
}

impl ValueStreamSource for ByteValueStream {
    fn next_value(&mut self) -> Result<Option<Value>, String> {
        self.source.next_chunk().map(|chunk| {
            chunk.map(|bytes| Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(bytes))))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn values(source: StreamSource) -> std::rc::Rc<std::cell::RefCell<dyn ValueStreamSource>> {
        let StreamSource::Decoded(source) = source else {
            panic!("预期解码流")
        };
        source
    }

    fn next_bytes(
        source: &std::rc::Rc<std::cell::RefCell<dyn ValueStreamSource>>,
    ) -> Option<Vec<u8>> {
        let value = source.borrow_mut().next_value().unwrap()?;
        match value {
            Value::Bytes(bytes) => {
                let copied = bytes.borrow().clone();
                Some(copied)
            }
            _ => panic!("预期字节值"),
        }
    }

    #[test]
    fn an_empty_reader_yields_no_chunks() {
        let source = values(reader_chunks(Cursor::new(Vec::new()), 2, 8));
        assert!(next_bytes(&source).is_none());
        assert!(next_bytes(&source).is_none());
    }

    #[test]
    fn a_reader_shorter_than_one_chunk_yields_a_single_truncated_chunk() {
        let source = values(reader_chunks(Cursor::new(b"a".to_vec()), 4, 8));
        assert_eq!(next_bytes(&source).unwrap(), b"a".to_vec());
        assert!(next_bytes(&source).is_none());
    }

    #[test]
    fn chunks_are_lazy_and_the_last_one_is_truncated() {
        let source = values(reader_chunks(Cursor::new(b"abcde".to_vec()), 2, 2));
        for expected in [b"ab".as_slice(), b"cd", b"e"] {
            assert_eq!(next_bytes(&source).unwrap(), expected.to_vec());
        }
        assert!(next_bytes(&source).is_none());
    }

    #[test]
    fn a_chunk_size_above_the_buffer_limit_is_a_resource_error() {
        let source = values(reader_chunks(Cursor::new(vec![1]), 2, 1));
        assert_eq!(
            source.borrow_mut().next_value().unwrap_err(),
            "流读取块大小超过缓冲上限"
        );
    }

    #[test]
    fn read_failures_surface_as_stream_errors() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("设备错误"))
            }
        }
        let source = values(reader_chunks(Failing, 2, 8));
        assert!(source
            .borrow_mut()
            .next_value()
            .unwrap_err()
            .contains("设备错误"));
    }

    #[test]
    fn abandoning_a_stream_early_does_not_read_the_rest() {
        let mut source = byte_chunks(Cursor::new(b"abcdef".to_vec()), 2, 8);
        assert_eq!(source.next_chunk().unwrap(), Some(b"ab".to_vec()));
        drop(source);
    }
}
