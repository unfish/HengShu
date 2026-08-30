//! 三路径语义门禁共享工具：同一份源码依次在源码解释器、内存字节码 VM 和独立打包程序上执行。
#![allow(dead_code)]

use hengshu::api::{
    check_source, run_compiled, run_source, DiagnosticReport, HengshuError, RunOptions, RunResult,
    SourceSpan, DIAGNOSTIC_FILE_VARIABLE, FAILURE_EXIT_CODE,
};
use hengshu::execution::ObservationMode;
use hengshu::provenance::Notebook;
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionStatus {
    Success,
    Failure,
}

/// 一条执行路径的全部可观察结果：输出、结构化失败身份和退出码。
/// 三者都参与比较，失败用例也不例外——失败前打印了什么同样是语义的一部分。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionOutcome {
    pub status: ExecutionStatus,
    pub output: Vec<String>,
    pub error: Option<DiagnosticReport>,
    pub exit_code: i32,
}

impl ExecutionOutcome {
    fn success(output: Vec<String>) -> Self {
        Self {
            status: ExecutionStatus::Success,
            output,
            error: None,
            exit_code: 0,
        }
    }

    pub fn error_category(&self) -> Option<&str> {
        self.error
            .as_ref()
            .and_then(|report| report.category.as_deref())
    }

    pub fn span(&self) -> Option<SourceSpan> {
        self.error.as_ref().and_then(|report| report.span)
    }

    pub fn line(&self) -> Option<usize> {
        self.span().map(|span| span.start.line)
    }

    pub fn column(&self) -> Option<usize> {
        self.span().map(|span| span.start.column)
    }

    pub fn message(&self) -> &str {
        self.error
            .as_ref()
            .map(|report| report.message.as_str())
            .unwrap_or_default()
    }

    pub fn stack(&self) -> &[String] {
        self.error
            .as_ref()
            .map(|report| report.stack.as_slice())
            .unwrap_or_default()
    }

    pub fn cause_message(&self) -> Option<&str> {
        self.error
            .as_ref()
            .and_then(|report| report.cause.as_deref())
            .map(|cause| cause.message.as_str())
    }
}

fn in_process_outcome(result: Result<RunResult, HengshuError>) -> ExecutionOutcome {
    match result {
        Ok(result) => ExecutionOutcome::success(result.output),
        Err(error) => ExecutionOutcome {
            status: ExecutionStatus::Failure,
            output: error.output().to_vec(),
            error: Some(error.report()),
            exit_code: FAILURE_EXIT_CODE,
        },
    }
}

fn source_outcome(source: &str, base_dir: &Path) -> ExecutionOutcome {
    in_process_outcome(run_source(source, RunOptions::new(base_dir)))
}

fn vm_outcome(source: &str, base_dir: &Path) -> ExecutionOutcome {
    in_process_outcome(run_compiled(source, RunOptions::new(base_dir)))
}

fn packaged_outcome(source: &str, directory: &Path) -> ExecutionOutcome {
    let source_path = directory.join("语义门禁.代码");
    let executable = directory.join("语义门禁程序");
    let diagnostic_path = directory.join("语义门禁诊断.json");
    fs::write(&source_path, source).expect("写入门禁源码");
    let _ = fs::remove_file(&diagnostic_path);
    let build = Command::new(env!("CARGO_BIN_EXE_衡枢"))
        .arg("打包")
        .arg(&source_path)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("启动打包命令");
    assert!(
        build.status.success(),
        "打包失败：{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&executable)
        .current_dir(directory)
        .env(DIAGNOSTIC_FILE_VARIABLE, &diagnostic_path)
        .output()
        .expect("启动独立程序");
    let output = String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let exit_code = run
        .status
        .code()
        .unwrap_or_else(|| panic!("独立程序未正常退出：{}", run.status));
    if run.status.success() {
        return ExecutionOutcome {
            status: ExecutionStatus::Success,
            output,
            error: None,
            exit_code,
        };
    }
    let report = fs::read(&diagnostic_path).unwrap_or_else(|error| {
        panic!(
            "独立程序失败时必须写出结构化诊断：{error}\n标准错误：{}",
            String::from_utf8_lossy(&run.stderr)
        )
    });
    let error = serde_json::from_slice::<DiagnosticReport>(&report).expect("解析结构化诊断");
    ExecutionOutcome {
        status: ExecutionStatus::Failure,
        output,
        error: Some(error),
        exit_code,
    }
}

/// 在源码解释器、内存 VM 和独立包上执行同一源码并要求三者一致。
pub fn assert_three_paths(source: &str) -> ExecutionOutcome {
    assert_three_paths_with_modules(source, &[])
}

/// 与 [`assert_three_paths`] 相同，但先把附带模块写入同一目录。
pub fn assert_three_paths_with_modules(source: &str, modules: &[(&str, &str)]) -> ExecutionOutcome {
    check_source(source).expect("差分执行前必须通过解析和语义检查");
    let directory = tempfile::tempdir().expect("创建语义门禁目录");
    for (name, module_source) in modules {
        check_source(module_source).expect("附带模块必须通过解析和语义检查");
        let path = directory.path().join(format!("{name}.代码"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("创建模块目录");
        }
        fs::write(path, module_source).expect("写入附带模块");
    }
    let source_result = source_outcome(source, directory.path());
    let vm_result = vm_outcome(source, directory.path());
    let package_result = packaged_outcome(source, directory.path());
    assert_eq!(
        vm_result, source_result,
        "内存 VM 与源码解释器不一致：\n{source}"
    );
    assert_eq!(
        package_result, source_result,
        "独立包与源码解释器不一致：\n{source}"
    );
    source_result
}

/// 与 [`assert_three_paths`] 相同，但每条路径在独立目录里运行，并额外比较交付物
/// 字节和规范化观察摘要。`fixtures` 先写入每个目录，`artifacts` 是程序应写出的
/// 相对路径。
pub fn assert_three_paths_with_artifacts(
    source: &str,
    fixtures: &[(&str, &[u8])],
    artifacts: &[&str],
) -> ExecutionOutcome {
    check_source(source).expect("差分执行前必须通过解析和语义检查");
    let root = tempfile::tempdir().expect("创建语义门禁目录");
    let mut directories = Vec::new();
    for name in ["源码", "字节码", "独立包"] {
        let directory = root.path().join(name);
        fs::create_dir_all(&directory).expect("创建路径目录");
        for (fixture, bytes) in fixtures {
            let path = directory.join(fixture);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("创建夹具目录");
            }
            fs::write(path, bytes).expect("写入夹具");
        }
        directories.push(directory);
    }
    let source_result = source_outcome(source, &directories[0]);
    let vm_result = vm_outcome(source, &directories[1]);
    let package_result = packaged_outcome(source, &directories[2]);
    assert_eq!(
        vm_result, source_result,
        "内存 VM 与源码解释器不一致：\n{source}"
    );
    assert_eq!(
        package_result, source_result,
        "独立包与源码解释器不一致：\n{source}"
    );
    for artifact in artifacts {
        let expected = fs::read(directories[0].join(artifact))
            .unwrap_or_else(|error| panic!("源码路径缺少交付物 {artifact}：{error}"));
        for (label, directory) in [("内存 VM", &directories[1]), ("独立包", &directories[2])] {
            let actual = fs::read(directory.join(artifact))
                .unwrap_or_else(|error| panic!("{label}缺少交付物 {artifact}：{error}"));
            assert_eq!(
                artifact_contents(&actual),
                artifact_contents(&expected),
                "{label}的交付物 {artifact} 与源码路径不同"
            );
        }
    }
    assert_eq!(
        observation_summary(run_observed(source, &directories[1], true)),
        observation_summary(run_observed(source, &directories[0], false)),
        "内存 VM 与源码解释器的观察摘要不同：\n{source}"
    );
    source_result
}

/// ZIP 包（OOXML 交付物）在条目里写入修改时间，逐字节比较会比较时钟而不是内容，
/// 因此按条目名和内容比较。PDF 由 `printpdf` 在每次保存时写入随机 XMP instance id，
/// 字节永远不可复现，因此比较页面文本和每页绘图算子数量这类语义内容。
/// 其他交付物直接比较字节。
fn artifact_contents(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    if bytes.starts_with(b"%PDF") {
        return pdf_contents(bytes);
    }
    if !bytes.starts_with(b"PK\x03\x04") {
        return vec![(String::new(), bytes.to_vec())];
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("读取交付物压缩包");
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读取压缩包条目");
        let name = entry.name().to_owned();
        let mut content = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut content).expect("读取压缩包条目内容");
        entries.push((name, content));
    }
    entries.sort();
    entries
}

/// PDF 的语义摘要：逐页文本加算子数量，忽略随机 instance id 和文件级元数据。
fn pdf_contents(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let document = printpdf::PdfDocument::parse(
        bytes,
        &printpdf::PdfParseOptions::default(),
        &mut Vec::new(),
    )
    .expect("解析 PDF 交付物");
    let text = document.extract_text();
    document
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let page_text = text
                .get(index)
                .map(|lines| lines.join("\n"))
                .unwrap_or_default();
            (
                format!("第{}页", index + 1),
                format!("算子{}\n{page_text}", page.ops.len()).into_bytes(),
            )
        })
        .collect()
}

fn run_observed(source: &str, base_dir: &Path, compiled: bool) -> Option<Box<Notebook>> {
    let mut options = RunOptions::new(base_dir);
    options.observation = ObservationMode::Snapshots;
    let result = if compiled {
        run_compiled(source, options)
    } else {
        run_source(source, options)
    };
    result.expect("观察模式下执行必须成功").observation
}

/// 解释器在语句边界记事件，VM 在指令边界记事件，事件条数天然不同，因此比较跨路径
/// 必须一致的部分：Notebook 版本、截断标记、按序记录的交付物和警告，以及步号从 1
/// 开始连续编号。
fn observation_summary(
    notebook: Option<Box<Notebook>>,
) -> (u32, bool, Vec<String>, Vec<String>, bool) {
    let Some(notebook) = notebook else {
        return (0, false, Vec::new(), Vec::new(), true);
    };
    let mut artifacts = Vec::new();
    let mut warnings = Vec::new();
    let mut sequential = true;
    for (index, event) in notebook.events.iter().enumerate() {
        artifacts.extend(event.artifacts.iter().cloned());
        warnings.extend(event.warnings.iter().cloned());
        sequential &= event.step == index as u64 + 1;
    }
    (
        notebook.version,
        notebook.truncated,
        artifacts,
        warnings,
        sequential,
    )
}

/// 要求三条路径都成功，并返回逐行输出。
pub fn assert_three_paths_output(source: &str) -> Vec<String> {
    let outcome = assert_three_paths(source);
    assert_eq!(
        outcome.status,
        ExecutionStatus::Success,
        "预期成功但失败：{:?}\n{source}",
        outcome.error
    );
    outcome.output
}

/// 与 [`assert_three_paths_output`] 相同，但携带附加模块。
pub fn assert_three_paths_output_with_modules(
    source: &str,
    modules: &[(&str, &str)],
) -> Vec<String> {
    let outcome = assert_three_paths_with_modules(source, modules);
    assert_eq!(
        outcome.status,
        ExecutionStatus::Success,
        "预期成功但失败：{:?}\n{source}",
        outcome.error
    );
    outcome.output
}

/// 要求三条路径都以同一语言错误失败，并返回错误。
pub fn assert_three_paths_failure(source: &str) -> ExecutionOutcome {
    let outcome = assert_three_paths(source);
    assert_eq!(
        outcome.status,
        ExecutionStatus::Failure,
        "预期语言错误但成功：{:?}\n{source}",
        outcome.output
    );
    outcome
}

/// 与 [`assert_three_paths_failure`] 相同，但携带附加模块。
pub fn assert_three_paths_failure_with_modules(
    source: &str,
    modules: &[(&str, &str)],
) -> ExecutionOutcome {
    let outcome = assert_three_paths_with_modules(source, modules);
    assert_eq!(
        outcome.status,
        ExecutionStatus::Failure,
        "预期语言错误但成功：{:?}\n{source}",
        outcome.output
    );
    outcome
}
