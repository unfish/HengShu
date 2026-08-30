//! S4 角色配方回归：七个示例在解释器和字节码 VM 上产生相同结果，
//! 并且原格式模板路径确实写出交付物。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::{SimpleFileOptions, ZipWriter};

fn recipe(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/角色配方")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("读取 {path:?} 失败：{error}"))
}

fn authorize_recipe(name: &str, options: &mut hengshu::api::RunOptions) {
    if name == "医生病例检验.代码" {
        options.permissions.sensitive_labels.extend([
            hengshu::execution::SensitiveLabel::Identity,
            hengshu::execution::SensitiveLabel::Medical,
        ]);
        options.permissions.endpoints.extend([
            hengshu::privacy::Endpoint::LocalFile,
            hengshu::privacy::Endpoint::Pdf,
        ]);
    }
}

fn run_both(name: &str, args: &[String]) -> Vec<String> {
    let source = recipe(name);
    let mut outputs = Vec::new();
    for compiled in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut options = hengshu::api::RunOptions::new(directory.path());
        authorize_recipe(name, &mut options);
        options.program_args = args.to_vec();
        let result = if compiled {
            hengshu::api::run_compiled(&source, options)
        } else {
            hengshu::api::run_source(&source, options)
        }
        .unwrap_or_else(|error| panic!("{name} 运行失败：{error}"));
        outputs.push(result.output);
    }
    assert_eq!(outputs[0], outputs[1], "{name} 在解释器和 VM 上结果不一致");
    outputs.pop().unwrap()
}

fn run_in(name: &str, directory: &Path, args: &[String], compiled: bool) -> Vec<String> {
    let source = recipe(name);
    let mut options = hengshu::api::RunOptions::new(directory);
    authorize_recipe(name, &mut options);
    options.program_args = args.to_vec();
    if compiled {
        hengshu::api::run_compiled(&source, options)
    } else {
        hengshu::api::run_source(&source, options)
    }
    .unwrap_or_else(|error| panic!("{name} 运行失败：{error}"))
    .output
}

fn package(path: &Path, entries: &[(&str, &[u8])]) {
    let mut archive = ZipWriter::new(File::create(path).unwrap());
    for (name, content) in entries {
        archive
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(content).unwrap();
    }
    archive.finish().unwrap();
}

#[test]
fn finance_recipe_reconciles_and_reports_on_both_paths() {
    let output = run_both("财务对账月报.代码", &[]);
    assert_eq!(
        output,
        [
            "未提供原格式模板，已跳过 XLSX 月报",
            "对账行数： 5",
            "差异行数： 3",
            "结果目录：财务结果",
        ]
    );
}

#[test]
fn finance_recipe_fills_the_original_xlsx_template() {
    let directory = tempfile::tempdir().unwrap();
    let template = directory.path().join("月报模板.xlsx");
    package(
        &template,
        &[
            ("[Content_Types].xml", b"<Types/>"),
            ("xl/workbook.xml", b"<workbook/>"),
            (
                "xl/worksheets/汇总.xml",
                br#"<worksheet><sheetData><row r="2"><c r="B2"><v>0</v></c></row><row r="3"><c r="B3"><v>0</v></c></row></sheetData></worksheet>"#,
            ),
        ],
    );
    for compiled in [false, true] {
        let output = run_in(
            "财务对账月报.代码",
            directory.path(),
            &["月报模板.xlsx".to_string()],
            compiled,
        );
        assert_eq!(output[0], "原格式月报替换数： 1");
        let filled = std::fs::read(directory.path().join("财务结果/对账月报.xlsx")).unwrap();
        assert!(filled.starts_with(b"PK"), "原格式月报必须仍是 OOXML 包");
    }
}

#[test]
fn hr_recipe_masks_before_writing_payslips() {
    let output = run_both("HR工资条批量.代码", &[]);
    assert_eq!(
        output,
        ["工资条份数： 3", "敏感列： [身份]", "结果目录：工资结果"]
    );
}

#[test]
fn sales_recipe_ranks_and_summarizes() {
    let output = run_both("销售排名排程.代码", &[]);
    assert_eq!(
        output,
        [
            "上周销售第一：赵六，销售额 205000",
            "排名行数： 4",
            "结果目录：销售结果",
        ]
    );
}

#[test]
fn boss_recipe_uses_the_selected_input_when_provided() {
    let output = run_both("老板经营报告.代码", &[]);
    assert_eq!(output[0], "未选择输入文件，使用内置样例数据");
    assert_eq!(output[1], "最新月份： 2026-06");

    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("月度.csv"),
        "月份,收入,成本\n2026-07,2400000,1500000\n",
    )
    .unwrap();
    let selected = run_in(
        "老板经营报告.代码",
        directory.path(),
        &["月度.csv".to_string()],
        false,
    );
    assert_eq!(selected[0], "已使用 GUI 选择的输入： 月度.csv");
    assert_eq!(selected[1], "最新月份： 2026-07");
}

#[test]
fn teacher_recipe_protects_small_classes() {
    let output = run_both("老师成绩单批量.代码", &[]);
    assert_eq!(
        output.iter().filter(|line| line.contains("跳过")).count(),
        2,
        "人数不足的班级不能输出个人成绩单"
    );
    assert!(output.contains(&"成绩单人数： 5".to_string()));
    assert!(output.contains(&"假名列已标记： 是".to_string()));
}

#[test]
fn civil_servant_recipe_returns_dirty_rows_and_fills_the_document_template() {
    let output = run_both("公务员汇总退回.代码", &[]);
    assert_eq!(
        output,
        [
            "未提供公文模板，已跳过原格式公文",
            "合格单位数： 2",
            "退回单位数： 2",
            "合计金额： 332500",
            "结果目录：汇总结果",
        ]
    );

    let directory = tempfile::tempdir().unwrap();
    let template = directory.path().join("公文模板.docx");
    package(
        &template,
        &[
            ("[Content_Types].xml", b"<Types/>"),
            (
                "word/document.xml",
                "<w:document><w:body><w:p><w:r><w:t>{{单位数}}/{{退回数}}/{{合计金额}}</w:t></w:r></w:p></w:body></w:document>".as_bytes(),
            ),
        ],
    );
    let filled = run_in(
        "公务员汇总退回.代码",
        directory.path(),
        &["公文模板.docx".to_string()],
        true,
    );
    assert_eq!(filled[0], "公文模板替换数： 3");
    assert!(directory.path().join("汇总结果/汇总公文.docx").exists());
}

#[test]
fn doctor_recipe_pseudonymizes_before_testing() {
    let output = run_both("医生病例检验.代码", &[]);
    assert_eq!(
        output,
        [
            "试验组人数： 4",
            "对照组人数： 4",
            "结论： 两组差异显著",
            "结果目录：病例结果",
        ]
    );

    let directory = tempfile::tempdir().unwrap();
    run_in("医生病例检验.代码", directory.path(), &[], false);
    let table = std::fs::read_to_string(directory.path().join("病例结果/脱敏病例.csv")).unwrap();
    assert!(!table.contains("张三"), "脱敏病例不能包含姓名");
    assert!(!table.contains("H0001"), "脱敏病例不能包含住院号");
}
