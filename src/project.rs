//! 项目服务：脚手架、依赖管理、测试、格式化与文档。

use std::fs;
use std::path::{Path, PathBuf};

mod dependencies;
mod docs;
mod format;
mod testing;

pub use dependencies::{
    add_dependency, audit, cache_dependency, read_manifest, remove_dependency, resolve_module,
    update_lock, Manifest, PermissionManifest, ProjectInfo, MANIFEST,
};
pub use docs::{benchmark, document};
pub use format::format;
pub use testing::{test_with_options, TestOptions};

pub fn create(name: &str, directory: &Path) -> Result<(), String> {
    let source_dir = directory.join("src");
    fs::create_dir_all(&source_dir).map_err(|e| e.to_string())?;
    let manifest = Manifest {
        project: ProjectInfo {
            name: name.into(),
            version: "0.1.0".into(),
            entry: "src/主程序.代码".into(),
            license: "MIT".into(),
        },
        dependencies: std::collections::BTreeMap::new(),
        permissions: PermissionManifest::default(),
        tools: std::collections::BTreeMap::new(),
        automations: std::collections::BTreeMap::new(),
    };
    dependencies::write_manifest(directory, &manifest)?;
    fs::write(
        source_dir.join("主程序.代码"),
        format!("输出（\"{name} 已准备就绪\"）\n"),
    )
    .map_err(|e| e.to_string())?;
    fs::create_dir_all(directory.join("tests")).map_err(|e| e.to_string())?;
    fs::write(
        directory.join("tests/基础测试.代码"),
        "测试 \"基础运算\"：\n    断言相等（1 + 1，2）\n结束\n",
    )
    .map_err(|e| e.to_string())?;
    update_lock(directory)?;
    Ok(())
}

pub fn project_entry(path: &Path) -> Result<PathBuf, String> {
    let root = find_root(path).ok_or_else(|| "找不到衡枢.toml".to_string())?;
    let manifest = read_manifest(&root)?;
    Ok(root.join(manifest.project.entry))
}

pub fn find_root(path: &Path) -> Option<PathBuf> {
    let mut current = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    loop {
        if current.join(MANIFEST).exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_root_is_the_nearest_ancestor_holding_a_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("项目");
        create("项目", &root).unwrap();
        let nested = root.join("src/深/更深");
        fs::create_dir_all(&nested).unwrap();
        let root = root.canonicalize().unwrap();
        assert_eq!(find_root(&nested).unwrap().canonicalize().unwrap(), root);
        assert_eq!(
            find_root(&root.join("src/主程序.代码"))
                .unwrap()
                .canonicalize()
                .unwrap(),
            root
        );
        assert_eq!(
            project_entry(&nested).unwrap().canonicalize().unwrap(),
            root.join("src/主程序.代码").canonicalize().unwrap()
        );
    }

    #[test]
    fn a_directory_without_a_manifest_has_no_project_root() {
        let directory = tempfile::tempdir().unwrap();
        assert!(find_root(directory.path()).is_none());
        assert!(project_entry(directory.path())
            .unwrap_err()
            .contains("找不到衡枢.toml"));
    }
}
