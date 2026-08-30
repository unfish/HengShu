use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn extension_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("editors/vscode")
}

#[test]
fn vscode_manifest_registers_code_extension_and_grammar() {
    let root = extension_root();
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(root.join("package.json")).expect("读取 VS Code 清单"),
    )
    .expect("VS Code 清单必须是 JSON");
    let language = &manifest["contributes"]["languages"][0];
    assert_eq!(language["id"], "hengshu");
    assert!(language["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == ".代码"));
    assert_eq!(
        manifest["contributes"]["grammars"][0]["path"],
        "./syntaxes/hengshu.tmLanguage.json"
    );
}

#[test]
fn vscode_language_configuration_covers_chinese_comments_and_delimiters() {
    let root = extension_root();
    let config: Value = serde_json::from_str(
        &fs::read_to_string(root.join("language-configuration.json")).expect("读取语言配置"),
    )
    .expect("语言配置必须是 JSON");
    assert_eq!(config["comments"]["lineComment"], "注：");
    let brackets = config["brackets"].as_array().unwrap();
    for pair in [["(", ")"], ["[", "]"], ["{", "}"]] {
        assert!(brackets.iter().any(|v| v[0] == pair[0] && v[1] == pair[1]));
    }
    let folding = config["folding"]["markers"]["start"].as_str().unwrap();
    for keyword in ["测试", "测试准备", "每项测试前", "每项测试后", "测试收尾"]
    {
        assert!(folding.contains(keyword), "折叠规则缺少 {keyword}");
    }
}

#[test]
fn vscode_grammar_keeps_external_standard_strings_and_keywords_distinct() {
    let root = extension_root();
    let grammar: Value = serde_json::from_str(
        &fs::read_to_string(root.join("syntaxes/hengshu.tmLanguage.json")).expect("读取语法文件"),
    )
    .expect("TextMate 语法必须是 JSON");
    let text = grammar.to_string();
    for keyword in [
        "公开",
        "内部",
        "定义",
        "类型限定为",
        "定义功能",
        "否则",
        "遍历",
        "记作",
        "时重复",
        "参数化输出",
        "匹配",
        "尝试",
        "测试准备",
        "每项测试前",
        "每项测试后",
        "测试收尾",
    ] {
        assert!(text.contains(keyword), "缺少关键字 {keyword}");
    }
    for incorrect in ["并行|", "等待|", "取消|"] {
        assert!(
            !text.contains(incorrect),
            "错误控制词仍在高亮规则中：{incorrect}"
        );
    }
    assert!(text.contains("string.quoted.double.hengshu"));
    assert!(text.contains("source.hengshu"));
}

#[test]
fn vscode_extension_has_a_node_free_vsix_builder() {
    let root = extension_root();
    let script = fs::read_to_string(root.join("build-vsix.sh")).expect("读取 VSIX 脚本");
    assert!(script.contains("zip -q -r"));
    assert!(!script.contains("node "));
    assert!(!script.contains("npx "));
    assert!(root.join("vsix/[Content_Types].xml").is_file());
    assert!(root.join("vsix/extension.vsixmanifest").is_file());
}

#[test]
fn vscode_manifest_schema_covers_permissions_tools_and_automation() {
    let root = extension_root();
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(root.join("package.json")).expect("读取 VS Code 清单"),
    )
    .unwrap();
    let validations = manifest["contributes"]["jsonValidation"]
        .as_array()
        .unwrap();
    assert!(validations.iter().any(|validation| {
        validation["url"] == "./schemas/manifest.schema.json"
            && validation["fileMatch"]
                .as_array()
                .is_some_and(|matches| matches.iter().any(|value| value == "衡枢.toml"))
    }));
    let schema: Value = serde_json::from_str(
        &fs::read_to_string(root.join("schemas/manifest.schema.json"))
            .expect("读取项目清单 schema"),
    )
    .unwrap();
    for section in ["项目", "依赖", "权限", "工具", "自动化"] {
        assert!(
            schema["properties"].get(section).is_some(),
            "项目清单 schema 缺少 {section}"
        );
    }
    for field in [
        "时区",
        "触发",
        "并发",
        "队列上限",
        "超时毫秒",
        "重试次数",
        "错过策略",
        "重叠策略",
        "通知",
        "SMTP",
    ] {
        assert!(
            schema["$defs"]["automation"]["properties"]
                .get(field)
                .is_some(),
            "自动化 schema 缺少 {field}"
        );
    }
    let script = fs::read_to_string(root.join("build-vsix.sh")).unwrap();
    assert!(script.contains("manifest.schema.json"));
}
