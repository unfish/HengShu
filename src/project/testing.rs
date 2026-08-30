use super::MANIFEST;
use crate::runtime;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Copy, Default)]
pub struct TestOptions<'a> {
    pub filter: Option<&'a str>,
    pub list_only: bool,
    pub fail_fast: bool,
    pub allow_empty: bool,
}

#[derive(Debug)]
pub struct TestSummary {
    pub selected: usize,
    pub listed: Vec<(PathBuf, String)>,
    pub passed: Vec<(PathBuf, String)>,
    pub failed: Vec<(PathBuf, String, String)>,
}

#[cfg(test)]
pub(super) fn test(
    input: &Path,
    filter: Option<&str>,
) -> Result<(usize, Vec<(String, String)>), String> {
    let report = test_with_options(
        input,
        TestOptions {
            filter,
            allow_empty: true,
            ..TestOptions::default()
        },
    )?;
    Ok((
        report.passed.len(),
        report
            .failed
            .into_iter()
            .map(|(_, name, error)| (name, error))
            .collect(),
    ))
}

pub fn test_with_options(input: &Path, options: TestOptions<'_>) -> Result<TestSummary, String> {
    let files = discover_test_files(input)?;
    let display_root = if input.is_file() {
        input.parent().unwrap_or(input)
    } else {
        input
    };
    let mut summary = TestSummary {
        selected: 0,
        listed: Vec::new(),
        passed: Vec::new(),
        failed: Vec::new(),
    };
    for file in files {
        let source = crate::resource::read_string_limited(&file, crate::resource::MAX_SOURCE_BYTES)
            .map_err(|error| format!("读取测试文件“{}”失败：{error}", file.display()))?;
        let relative = file
            .strip_prefix(display_root)
            .unwrap_or(&file)
            .to_path_buf();
        let names = crate::resource::on_compile_stack(|| runtime::list_tests(&source))
            .map_err(|error| format!("测试文件“{}”检查失败：\n{error}", file.display()))?
            .into_iter()
            .filter(|name| options.filter.is_none_or(|filter| name.contains(filter)))
            .collect::<Vec<_>>();
        summary.selected += names.len();
        if options.list_only {
            summary
                .listed
                .extend(names.into_iter().map(|name| (relative.clone(), name)));
            continue;
        }
        if names.is_empty() {
            continue;
        }
        let report = runtime::execute_tests_on_execution_stack(
            source.clone(),
            file.parent().unwrap_or(input).to_path_buf(),
            options.filter.map(str::to_string),
            options.fail_fast,
        )
        .map_err(|error| format!("测试文件“{}”执行失败：\n{}", file.display(), error.render()))?;
        summary.passed.extend(
            report
                .passed
                .into_iter()
                .map(|name| (relative.clone(), name)),
        );
        let file_failed = !report.failed.is_empty();
        summary.failed.extend(
            report
                .failed
                .into_iter()
                .map(|(name, error)| (relative.clone(), name, error)),
        );
        if options.fail_fast && file_failed {
            break;
        }
    }
    if summary.selected == 0 && !options.allow_empty {
        let detail = options
            .filter
            .map(|filter| format!("，筛选条件为“{filter}”"))
            .unwrap_or_default();
        return Err(format!("没有发现可执行的测试{detail}"));
    }
    Ok(summary)
}

fn discover_test_files(input: &Path) -> Result<Vec<PathBuf>, String> {
    if !input.exists() {
        return Err(format!("测试路径不存在：{}", input.display()));
    }
    if input.is_file() {
        if input
            .extension()
            .is_none_or(|extension| extension != "代码")
        {
            return Err("单个测试文件必须使用 .代码 后缀".into());
        }
        return Ok(vec![input.to_path_buf()]);
    }
    let root = if input.join(MANIFEST).is_file() {
        input.join("tests")
    } else {
        input.to_path_buf()
    };
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut files = WalkDir::new(&root)
        .into_iter()
        .filter_entry(|entry| {
            !entry.file_type().is_dir()
                || !matches!(
                    entry.file_name().to_string_lossy().as_ref(),
                    "target" | ".git" | ".衡枢缓存"
                )
        })
        .filter_map(|entry| match entry {
            Ok(entry)
                if entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "代码") =>
            {
                Some(Ok(entry.path().to_path_buf()))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error.to_string())),
        })
        .collect::<Result<Vec<_>, String>>()?;
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::create;
    use std::fs;
    #[test]
    fn test_filter_counts_only_matching_passes_and_failures() {
        let directory = tempfile::tempdir().unwrap();
        create("筛选项目", directory.path()).unwrap();
        fs::write(
            directory.path().join("tests/筛选.代码"),
            "测试 \"通过甲\"：\n    断言(是)\n结束\n\n测试 \"通过乙\"：\n    断言(是)\n结束\n\n测试 \"失败甲\"：\n    断言(否)\n结束\n",
        )
        .unwrap();
        let (passed, failed) = test(directory.path(), Some("甲")).unwrap();
        assert_eq!(passed, 1);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "失败甲");
    }

    #[test]
    fn test_discovery_accepts_single_file_plain_directory_and_project_root() {
        let directory = tempfile::tempdir().unwrap();
        let plain = directory.path().join("普通目录");
        fs::create_dir_all(plain.join("子目录")).unwrap();
        let first = plain.join("甲.代码");
        fs::write(&first, "测试 \"甲\"：\n断言(是)\n结束\n").unwrap();
        fs::write(
            plain.join("子目录/乙.代码"),
            "测试 \"乙\"：\n断言(是)\n结束\n",
        )
        .unwrap();

        let single = test_with_options(&first, TestOptions::default()).unwrap();
        assert_eq!(single.passed.len(), 1);
        let recursive = test_with_options(
            &plain,
            TestOptions {
                list_only: true,
                ..TestOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            recursive
                .listed
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["乙", "甲"]
        );

        let project = directory.path().join("项目");
        create("项目", &project).unwrap();
        let report = test_with_options(&project, TestOptions::default()).unwrap();
        assert_eq!(report.passed.len(), 1);
        assert_eq!(report.passed[0].1, "基础运算");
    }

    #[test]
    fn test_discovery_rejects_empty_missing_and_non_code_inputs() {
        let directory = tempfile::tempdir().unwrap();
        assert!(test_with_options(directory.path(), TestOptions::default())
            .unwrap_err()
            .contains("没有发现"));
        assert_eq!(
            test_with_options(
                directory.path(),
                TestOptions {
                    allow_empty: true,
                    ..TestOptions::default()
                }
            )
            .unwrap()
            .selected,
            0
        );
        assert!(
            test_with_options(&directory.path().join("不存在"), TestOptions::default())
                .unwrap_err()
                .contains("路径不存在")
        );
        let text = directory.path().join("测试.txt");
        fs::write(&text, "").unwrap();
        assert!(test_with_options(&text, TestOptions::default())
            .unwrap_err()
            .contains(".代码"));
    }

    #[test]
    fn test_discovery_reports_syntax_depth_instead_of_overflowing() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("深度.代码");
        let nested = format!(
            "测试 \"深度\"：\n断言({}是{})\n结束\n",
            "(".repeat(1100),
            ")".repeat(1100)
        );
        fs::write(&file, nested).unwrap();

        let error = test_with_options(
            &file,
            TestOptions {
                list_only: true,
                ..TestOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("语法嵌套深度超过"), "{error}");
    }

    #[test]
    fn fail_fast_stops_after_first_failed_file() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("01失败.代码"),
            "测试 \"首项失败\"：\n断言(否)\n结束\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("02通过.代码"),
            "测试 \"后续通过\"：\n断言(是)\n结束\n",
        )
        .unwrap();
        let report = test_with_options(
            directory.path(),
            TestOptions {
                fail_fast: true,
                ..TestOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(report.passed.is_empty());
    }
}
