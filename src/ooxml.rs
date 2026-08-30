use std::io::{Read, Seek};
use zip::ZipArchive;

#[derive(Clone, Copy, Debug)]
pub(crate) struct OoxmlBudget {
    pub max_entries: usize,
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
}

pub(crate) fn validate_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    budget: OoxmlBudget,
) -> Result<(), String> {
    if archive.len() > budget.max_entries {
        return Err("OOXML 压缩包条目数超过预算".into());
    }
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().replace('\\', "/");
        if name.split('/').any(|part| part == "..") {
            return Err(format!("OOXML 条目路径不安全：{name}"));
        }
        if entry.size() > budget.max_entry_bytes {
            return Err(format!("OOXML 部件超过字节预算：{name}"));
        }
        total = total.saturating_add(entry.size());
        if total > budget.max_total_bytes {
            return Err("OOXML 累计展开字节超过预算".into());
        }
    }
    Ok(())
}

pub(crate) fn read_bytes_limited(
    reader: &mut impl Read,
    name: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取 OOXML 部件失败：{name}：{error}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("OOXML 部件超过字节预算：{name}"));
    }
    Ok(bytes)
}

pub(crate) fn read_string_limited(
    reader: &mut impl Read,
    name: &str,
    max_bytes: u64,
) -> Result<String, String> {
    String::from_utf8(read_bytes_limited(reader, name, max_bytes)?)
        .map_err(|_| format!("OOXML XML 部件不是 UTF-8：{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn archive_and_entry_budgets_reject_expansion_and_oversized_reads() {
        let mut package = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut package);
            writer
                .start_file(
                    "xl/worksheets/sheet1.xml",
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"<worksheet/>").unwrap();
            writer.finish().unwrap();
        }
        package.set_position(0);
        let mut archive = ZipArchive::new(package).unwrap();
        assert!(validate_archive(
            &mut archive,
            OoxmlBudget {
                max_entries: 1,
                max_entry_bytes: 64,
                max_total_bytes: 64,
            }
        )
        .is_ok());
        assert!(validate_archive(
            &mut archive,
            OoxmlBudget {
                max_entries: 1,
                max_entry_bytes: 4,
                max_total_bytes: 64,
            }
        )
        .is_err());
        let mut bytes = Cursor::new(vec![0u8; 5]);
        assert!(read_bytes_limited(&mut bytes, "part", 4).is_err());
    }
}
