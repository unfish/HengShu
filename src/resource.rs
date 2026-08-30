use std::fs::File;
use std::io::{self, BufRead, Read};
use std::path::Path;

pub(crate) const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_METADATA_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_EXECUTABLE_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_BUFFER_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_COLLECTION_ITEMS: usize = 1_000_000;
pub(crate) const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_CALL_DEPTH: usize = 1024;
pub(crate) const DEFAULT_MAX_VALUE_NESTING: usize = 256;
pub(crate) const DEFAULT_MAX_MODULE_DEPTH: usize = 64;
/// 解析、语义检查和编译共用的语法嵌套深度预算，防止编译期宿主栈溢出。
/// 所有解析入口都在`EXECUTION_STACK_BYTES`大栈上运行，因此预算与调用深度同量级，
/// 既能容纳正常长度的左结合运算链，也能在超限时先报语言错误而不是撞穿宿主栈。
pub(crate) const DEFAULT_MAX_SYNTAX_DEPTH: usize = DEFAULT_MAX_CALL_DEPTH;
/// 单个通道的容量上限，防止一次创建就申请巨量驻留内存。
pub(crate) const DEFAULT_MAX_CHANNEL_CAPACITY: usize = 65_536;
/// 并行映射的工作者数量上限。
pub(crate) const DEFAULT_MAX_PARALLEL_WORKERS: usize = 64;
/// 单个金额值保留的审计记录条数上限；重复动作按次数折叠，超出上限的更早记录汇总为一条。
/// 有了上限，循环内累加金额的内存与克隆成本才能随运算次数线性增长。
pub(crate) const DEFAULT_MAX_MONEY_AUDIT_ENTRIES: usize = 64;

/// 执行线程栈大小：必须容纳`DEFAULT_MAX_CALL_DEPTH`层解释器递归的 debug 栈帧，
/// 使调用深度预算先于宿主栈耗尽触发。
pub(crate) const EXECUTION_STACK_BYTES: usize = 256 * 1024 * 1024;

pub(crate) fn execution_thread() -> std::thread::Builder {
    std::thread::Builder::new()
        .name("衡枢执行".into())
        .stack_size(EXECUTION_STACK_BYTES)
}

/// 在大栈执行线程上运行语言求值，把线程创建失败转成语言级资源错误。
pub(crate) fn on_execution_stack<T, E>(
    body: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, E>
where
    T: Send + 'static,
    E: Send + 'static + From<crate::runtime::RuntimeError>,
{
    join_on_execution_stack(body).map_err(|error| {
        E::from(
            crate::runtime::RuntimeError::new(0, format!("无法创建执行线程：{error}"))
                .with_category("资源错误"),
        )
    })?
}

/// 在同一个大栈执行线程上运行编译期工作（解析、语义检查、编译与打包），
/// 让语法嵌套深度预算先于宿主栈耗尽触发。
pub(crate) fn on_compile_stack<T>(
    body: impl FnOnce() -> Result<T, String> + Send,
) -> Result<T, String>
where
    T: Send,
{
    on_compile_stack_scoped(body).map_err(|error| format!("无法创建执行线程：{error}"))?
}

/// 借用调用方数据的编译期工作也走同一条大栈线程，无需先把源码复制成独立所有权。
pub(crate) fn on_compile_stack_scoped<T: Send>(body: impl FnOnce() -> T + Send) -> io::Result<T> {
    std::thread::scope(|scope| {
        let handle = execution_thread().spawn_scoped(scope, body)?;
        match handle.join() {
            Ok(value) => Ok(value),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

fn join_on_execution_stack<T>(body: impl FnOnce() -> T + Send + 'static) -> io::Result<T>
where
    T: Send + 'static,
{
    let handle = execution_thread().spawn(body)?;
    match handle.join() {
        Ok(value) => Ok(value),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

pub(crate) fn read_file_limited(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(limit_error(limit));
    }
    read_to_end_limited(file, limit)
}

pub(crate) fn read_string_limited(path: &Path, limit: usize) -> io::Result<String> {
    String::from_utf8(read_file_limited(path, limit)?).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("文件不是有效的 UTF-8：{error}"),
        )
    })
}

pub(crate) fn read_to_end_limited(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok(output);
        }
        let next = output
            .len()
            .checked_add(count)
            .ok_or_else(|| limit_error(limit))?;
        if next > limit {
            return Err(limit_error(limit));
        }
        output
            .try_reserve(count)
            .map_err(|_| io::Error::other("无法为输入缓冲区分配内存"))?;
        output.extend_from_slice(&chunk[..count]);
    }
}

pub(crate) fn check_file_size(path: &Path, limit: usize) -> io::Result<()> {
    if path.metadata()?.len() > limit as u64 {
        Err(limit_error(limit))
    } else {
        Ok(())
    }
}

pub(crate) fn read_line_limited(
    reader: &mut impl BufRead,
    limit: usize,
) -> io::Result<Option<String>> {
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if output.is_empty() {
                return Ok(None);
            }
            break;
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if output.len().saturating_add(count) > limit {
            return Err(limit_error(limit));
        }
        output
            .try_reserve(count)
            .map_err(|_| io::Error::other("无法为行缓冲区分配内存"))?;
        output.extend_from_slice(&available[..count]);
        let ended = available.get(count.saturating_sub(1)) == Some(&b'\n');
        reader.consume(count);
        if ended {
            break;
        }
    }
    String::from_utf8(output).map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("文本流不是有效的 UTF-8：{error}"),
        )
    })
}

fn limit_error(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("输入超过 {limit} 字节缓冲上限，请使用流式接口"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_reader_rejects_content_over_limit() {
        assert_eq!(read_to_end_limited(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(read_to_end_limited(&b"12345"[..], 4).is_err());
    }

    #[test]
    fn limited_line_reader_rejects_oversized_lines() {
        let mut valid = std::io::BufReader::new(&b"123\n"[..]);
        assert_eq!(read_line_limited(&mut valid, 4).unwrap().unwrap(), "123\n");
        let mut oversized = std::io::BufReader::new(&b"12345\n"[..]);
        assert!(read_line_limited(&mut oversized, 4).is_err());
    }
}
