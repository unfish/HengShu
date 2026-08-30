//! 二进制解码层：把字节源解释为帧。
//!
//! 二进制读取按格式分四类，本模块只覆盖前三类的接口，第四类交给专用模块：
//! * 原始 chunk：不解释边界，直接透出字节源的分块，见 `bytes::reader_chunks`。
//! * 固定长度帧：每帧字节数由调用方给定，见 `FixedFrameDecoder`。
//! * 长度前缀帧：帧头声明本帧长度，见 `LengthPrefixedFrameDecoder`。
//! * 带 schema 的格式：帧布局由文件自身的元数据决定，无法在本层通用解码，
//!   因此 Parquet 这类格式各自成模块（`parquet.rs`）。
//!
//! 两类预算在本层生效：`FrameBudget::frame_size` 限制单帧字节数，`FrameBudget::total_bytes`
//! 限制整条流累计消费的字节数。越界一律报 malformed frame 类错误，不向上层泄漏 IO 细节。
//! 本轮不接入语言入口。

use std::mem::size_of;

pub(crate) trait ByteStream {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String>;
}

#[allow(dead_code)]
pub(crate) trait FrameDecoder {
    fn next_frame(&mut self, source: &mut dyn ByteStream) -> Result<Option<Vec<u8>>, String>;
}

/// 帧解码的三重预算：单帧字节数、重组缓冲字节数、整条流累计字节数。
///
/// 单帧与缓冲必须分开：一个字节 chunk 可以跨帧边界，缓冲因此会短暂超过一帧的大小。
#[allow(dead_code)]
pub(crate) struct FrameBudget {
    frame_size: usize,
    buffer_limit: usize,
    total_bytes: Option<usize>,
    consumed: usize,
}

#[allow(dead_code)]
impl FrameBudget {
    pub(crate) fn new(
        frame_size: usize,
        buffer_limit: usize,
        total_bytes: Option<usize>,
    ) -> Result<Self, String> {
        if frame_size == 0 {
            return Err("二进制帧大小必须大于零".into());
        }
        if frame_size > buffer_limit {
            return Err("二进制帧大小超过缓冲上限".into());
        }
        if total_bytes.is_some_and(|total| frame_size > total) {
            return Err("二进制帧大小超过字节预算".into());
        }
        Ok(Self {
            frame_size,
            buffer_limit,
            total_bytes,
            consumed: 0,
        })
    }

    fn admit_frame(&mut self, length: usize, framing_bytes: usize) -> Result<(), String> {
        if length > self.frame_size {
            return Err("二进制帧长度超过帧大小预算".into());
        }
        let consumed = self
            .consumed
            .saturating_add(framing_bytes)
            .saturating_add(length);
        if self.total_bytes.is_some_and(|total| consumed > total) {
            return Err("二进制流累计字节超过资源限制".into());
        }
        self.consumed = consumed;
        Ok(())
    }

    fn admit_buffer(&self, pending: usize, incoming: usize) -> Result<(), String> {
        if pending.saturating_add(incoming) > self.buffer_limit {
            return Err("二进制帧缓冲超过资源限制".into());
        }
        Ok(())
    }
}

/// 把缓冲填到至少 `wanted` 字节；返回 false 表示流已结束且缓冲不足。
fn fill(
    pending: &mut Vec<u8>,
    wanted: usize,
    source: &mut dyn ByteStream,
    budget: &FrameBudget,
) -> Result<bool, String> {
    while pending.len() < wanted {
        let Some(chunk) = source.next_chunk()? else {
            return Ok(false);
        };
        budget.admit_buffer(pending.len(), chunk.len())?;
        pending.extend(chunk);
    }
    Ok(true)
}

#[allow(dead_code)]
pub(crate) struct FixedFrameDecoder {
    budget: FrameBudget,
    pending: Vec<u8>,
}

#[allow(dead_code)]
impl FixedFrameDecoder {
    pub(crate) fn new(frame_size: usize, buffer_limit: usize) -> Result<Self, String> {
        Ok(Self::with_budget(FrameBudget::new(
            frame_size,
            buffer_limit,
            None,
        )?))
    }

    pub(crate) fn with_budget(budget: FrameBudget) -> Self {
        Self {
            budget,
            pending: Vec::new(),
        }
    }
}

impl FrameDecoder for FixedFrameDecoder {
    fn next_frame(&mut self, source: &mut dyn ByteStream) -> Result<Option<Vec<u8>>, String> {
        let frame_size = self.budget.frame_size;
        if !fill(&mut self.pending, frame_size, source, &self.budget)? {
            if self.pending.is_empty() {
                return Ok(None);
            }
            return Err("二进制流在完整帧结束前提前结束".into());
        }
        self.budget.admit_frame(frame_size, 0)?;
        Ok(Some(self.pending.drain(..frame_size).collect()))
    }
}

/// 长度前缀帧：帧头是大端 `u32`，声明随后的负载字节数。
#[allow(dead_code)]
pub(crate) struct LengthPrefixedFrameDecoder {
    budget: FrameBudget,
    pending: Vec<u8>,
}

const PREFIX_BYTES: usize = size_of::<u32>();

#[allow(dead_code)]
impl LengthPrefixedFrameDecoder {
    pub(crate) fn new(budget: FrameBudget) -> Self {
        Self {
            budget,
            pending: Vec::new(),
        }
    }
}

impl FrameDecoder for LengthPrefixedFrameDecoder {
    fn next_frame(&mut self, source: &mut dyn ByteStream) -> Result<Option<Vec<u8>>, String> {
        if !fill(&mut self.pending, PREFIX_BYTES, source, &self.budget)? {
            if self.pending.is_empty() {
                return Ok(None);
            }
            return Err("二进制流在帧长度前缀结束前提前结束".into());
        }
        let mut prefix = [0u8; PREFIX_BYTES];
        prefix.copy_from_slice(&self.pending[..PREFIX_BYTES]);
        let length = usize::try_from(u32::from_be_bytes(prefix))
            .map_err(|_| "二进制帧长度超过内部索引限制".to_string())?;
        if length == 0 {
            return Err("二进制帧长度必须大于零".into());
        }
        // 长度前缀已经从源读取，也属于整条流的累计消耗。
        self.budget.admit_frame(length, PREFIX_BYTES)?;
        let wanted = PREFIX_BYTES.saturating_add(length);
        if !fill(&mut self.pending, wanted, source, &self.budget)? {
            return Err("二进制流在完整帧结束前提前结束".into());
        }
        self.pending.drain(..PREFIX_BYTES);
        Ok(Some(self.pending.drain(..length).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::bytes::byte_chunks;
    use super::*;
    use std::io::Cursor;

    fn chunks(bytes: Vec<u8>, chunk_size: usize) -> Box<dyn ByteStream> {
        let limit = bytes.len().max(chunk_size);
        byte_chunks(Cursor::new(bytes), chunk_size, limit)
    }

    fn budget(frame_size: usize) -> FrameBudget {
        FrameBudget::new(frame_size, 32, None).unwrap()
    }

    #[test]
    fn an_empty_source_yields_no_frames_for_either_decoder() {
        let mut source = chunks(Vec::new(), 2);
        assert!(FixedFrameDecoder::new(2, 8)
            .unwrap()
            .next_frame(&mut *source)
            .unwrap()
            .is_none());
        let mut source = chunks(Vec::new(), 4);
        assert!(LengthPrefixedFrameDecoder::new(budget(8))
            .next_frame(&mut *source)
            .unwrap()
            .is_none());
    }

    #[test]
    fn fixed_frames_preserve_boundaries_across_chunk_splits() {
        let mut source = chunks(vec![1, 2, 3, 4], 3);
        let mut decoder = FixedFrameDecoder::new(2, 8).unwrap();
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![1, 2]));
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![3, 4]));
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), None);
    }

    #[test]
    fn a_trailing_partial_fixed_frame_is_malformed() {
        let mut source = chunks(vec![1, 2, 3, 4, 5], 2);
        let mut decoder = FixedFrameDecoder::new(2, 8).unwrap();
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![1, 2]));
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![3, 4]));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制流在完整帧结束前提前结束"
        );
    }

    #[test]
    fn length_prefixed_frames_decode_payloads_of_differing_sizes() {
        let bytes = vec![0, 0, 0, 1, 9, 0, 0, 0, 3, 7, 7, 7];
        let mut source = chunks(bytes, 5);
        let mut decoder = LengthPrefixedFrameDecoder::new(budget(8));
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![9]));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap(),
            Some(vec![7, 7, 7])
        );
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), None);
    }

    #[test]
    fn truncated_prefixes_and_payloads_are_distinguishable_errors() {
        let mut source = chunks(vec![0, 0], 2);
        let mut decoder = LengthPrefixedFrameDecoder::new(budget(8));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制流在帧长度前缀结束前提前结束"
        );

        let mut source = chunks(vec![0, 0, 0, 4, 1, 2], 3);
        let mut decoder = LengthPrefixedFrameDecoder::new(budget(8));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制流在完整帧结束前提前结束"
        );
    }

    #[test]
    fn frames_longer_than_the_frame_budget_are_rejected() {
        let mut source = chunks(vec![0, 0, 0, 9, 1, 2, 3], 8);
        let mut decoder = LengthPrefixedFrameDecoder::new(budget(4));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制帧长度超过帧大小预算"
        );
    }

    #[test]
    fn a_chunk_larger_than_the_reassembly_buffer_is_rejected() {
        let mut source = chunks(vec![1, 2, 3, 4, 5, 6, 7, 8], 8);
        let mut decoder = FixedFrameDecoder::with_budget(FrameBudget::new(2, 4, None).unwrap());
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制帧缓冲超过资源限制"
        );
    }

    #[test]
    fn a_zero_length_frame_is_malformed() {
        let mut source = chunks(vec![0, 0, 0, 0], 4);
        let mut decoder = LengthPrefixedFrameDecoder::new(budget(4));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制帧长度必须大于零"
        );
    }

    #[test]
    fn the_cumulative_byte_budget_stops_a_long_stream() {
        let mut source = chunks(vec![1, 2, 3, 4, 5, 6], 2);
        let mut decoder = FixedFrameDecoder::with_budget(FrameBudget::new(2, 8, Some(4)).unwrap());
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![1, 2]));
        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![3, 4]));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制流累计字节超过资源限制"
        );
    }

    #[test]
    fn length_prefixes_count_toward_the_cumulative_byte_budget() {
        let bytes = vec![0, 0, 0, 1, 9, 0, 0, 0, 1, 8];
        let mut source = chunks(bytes, 2);
        let budget = FrameBudget::new(1, 16, Some(8)).unwrap();
        let mut decoder = LengthPrefixedFrameDecoder::new(budget);

        assert_eq!(decoder.next_frame(&mut *source).unwrap(), Some(vec![9]));
        assert_eq!(
            decoder.next_frame(&mut *source).unwrap_err(),
            "二进制流累计字节超过资源限制"
        );
    }

    #[test]
    fn budgets_reject_degenerate_configurations() {
        assert_eq!(
            FrameBudget::new(0, 8, None).err().unwrap(),
            "二进制帧大小必须大于零"
        );
        assert_eq!(
            FrameBudget::new(9, 8, None).err().unwrap(),
            "二进制帧大小超过缓冲上限"
        );
        assert_eq!(
            FrameBudget::new(8, 32, Some(4)).err().unwrap(),
            "二进制帧大小超过字节预算"
        );
        assert_eq!(
            FixedFrameDecoder::new(9, 8).err().unwrap(),
            "二进制帧大小超过缓冲上限"
        );
    }
}
