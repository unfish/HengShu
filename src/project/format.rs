use crate::formatter;
use std::path::Path;
use walkdir::WalkDir;

pub fn format(root: &Path, check: bool) -> Result<usize, String> {
    let mut changed = 0;
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some("target" | ".git")))
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "代码")
            && formatter::format_file(entry.path(), check)?
        {
            changed += 1;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn formatter_skips_build_and_repository_metadata() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("target")).unwrap();
        fs::create_dir_all(directory.path().join(".git")).unwrap();
        fs::write(directory.path().join("主程序.代码"), "定义 数值 为 1\n").unwrap();
        fs::write(
            directory.path().join("target/忽略.代码"),
            "定义 数值 为  1\n",
        )
        .unwrap();
        fs::write(directory.path().join(".git/忽略.代码"), "定义 数值 为  1\n").unwrap();
        format(directory.path(), false).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("target/忽略.代码")).unwrap(),
            "定义 数值 为  1\n"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join(".git/忽略.代码")).unwrap(),
            "定义 数值 为  1\n"
        );
    }
}
