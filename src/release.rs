use crate::pack;
use crate::security::PermissionSet;
use flate2::write::GzEncoder;
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tar::{Builder, Header};

pub fn build(
    source: &Path,
    output_dir: &Path,
    permissions: PermissionSet,
) -> Result<PathBuf, String> {
    fs::create_dir_all(output_dir).map_err(|e| format!("创建发布目录失败：{e}"))?;
    let name = source
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("衡枢程序");
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let archive = output_dir.join(format!("{name}-{platform}.tar.gz"));
    let source_key = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let source_hash = format!("{:x}", Sha256::digest(source_key.as_bytes()));
    let temporary =
        std::env::temp_dir().join(format!("hengshu-release-{}-{name}", &source_hash[..16]));
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|e| format!("清理旧发布临时文件失败：{e}"))?;
    }
    pack::build(source, &temporary, permissions)?;
    let executable =
        crate::resource::read_file_limited(&temporary, crate::resource::MAX_EXECUTABLE_BYTES)
            .map_err(|e| format!("读取发布程序失败：{e}"))?;
    fs::remove_file(&temporary).map_err(|e| format!("清理发布临时文件失败：{e}"))?;
    let checksum = format!("{:x}", Sha256::digest(&executable));
    let checksum_file = format!("{checksum}  {name}\n");

    let file = fs::File::create(&archive).map_err(|e| format!("创建发布归档失败：{e}"))?;
    let encoder: GzEncoder<_> = GzBuilder::new().mtime(0).write(file, Compression::best());
    let mut builder = Builder::new(encoder);
    append(&mut builder, name, &executable, 0o755)?;
    append(&mut builder, "SHA256SUMS", checksum_file.as_bytes(), 0o644)?;
    let encoder = builder
        .into_inner()
        .map_err(|e| format!("完成发布归档失败：{e}"))?;
    encoder
        .finish()
        .map_err(|e| format!("完成发布压缩失败：{e}"))?;
    Ok(archive)
}

fn append(
    builder: &mut Builder<GzEncoder<fs::File>>,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<(), String> {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder
        .append_data(&mut header, name, bytes)
        .map_err(|e| format!("写入发布归档失败：{e}"))
}
