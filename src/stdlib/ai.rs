//! OpenAI-compatible Responses adapter with deterministic offline/replay paths.
//!
//! The module deliberately returns data as ordinary `Value` records. It never
//! executes model output and never includes the API key in a result or error.

use super::codec::{from_json, to_json};
use super::error;
use crate::resource::read_string_limited;
use crate::security::PermissionSet;
use crate::value::{ensure_collection_limit, Value};
use base64::Engine;
use chrono::Utc;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use url::Url;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONTEXT_BYTES: usize = 24 * 1024;
const MAX_RETRIES: u32 = 3;
const MAX_PDF_PAGES: usize = 20;
const MAX_IMAGE_DIMENSION: u32 = 8192;
const MAX_HTML_NODES: usize = 2048;
const AI_PROMPT_VERSION: u32 = 1;
const ANALYSIS_INSTRUCTIONS: &str = "你是衡枢分析报告助手。只使用提供的确定性统计结果；不得补造字段、样本、显著性、可靠性或因果关系。明确区分数据事实、模型解释和建议行动，每个事实必须引用允许的结果路径。警告和方法前提不得省略。";

#[derive(Clone, Debug)]
struct AiConfig {
    base_url: String,
    model_name: String,
    api_key: Option<String>,
}

#[derive(Clone, Debug)]
struct AiResult {
    status: String,
    text: String,
    mode: String,
    response_id: String,
    warning: Option<String>,
    usage: Option<Value>,
    raw: Option<JsonValue>,
    endpoint: Option<String>,
    external_sent: bool,
}

impl AiResult {
    fn into_value(self) -> Value {
        let mut fields = BTreeMap::from([
            ("状态".into(), Value::Text(self.status)),
            ("文本".into(), Value::Text(self.text)),
            ("模式".into(), Value::Text(self.mode)),
            ("响应ID".into(), Value::Text(self.response_id)),
            (
                "警告".into(),
                self.warning.map(Value::Text).unwrap_or(Value::Null),
            ),
        ]);
        if let Some(usage) = self.usage {
            fields.insert("用量".into(), usage);
        }
        if let Some(raw) = self.raw {
            fields.insert("原始响应".into(), from_json(&raw));
        }
        fields.insert(
            "模型端点".into(),
            self.endpoint.map(Value::Text).unwrap_or(Value::Null),
        );
        fields.insert("已发生外发".into(), Value::Bool(self.external_sent));
        Value::record(fields)
    }
}

#[derive(Debug)]
struct AiReportDraft {
    status: String,
    html: String,
    report: Value,
    references: Value,
    citation_details: Value,
    input_summary: String,
    warning: Option<String>,
    usage: Option<Value>,
    generated_at: String,
    mode: String,
    reproducibility: String,
    endpoint: Option<String>,
    external_sent: bool,
}

impl AiReportDraft {
    fn into_value(self) -> Value {
        Value::record(BTreeMap::from([
            ("状态".into(), Value::Text(self.status)),
            ("HTML".into(), Value::Text(self.html)),
            ("报告".into(), self.report),
            ("引用字段".into(), self.references),
            ("引用".into(), self.citation_details),
            ("输入摘要".into(), Value::Text(self.input_summary)),
            (
                "警告".into(),
                self.warning.map(Value::Text).unwrap_or(Value::Null),
            ),
            ("用量".into(), self.usage.unwrap_or(Value::Null)),
            ("生成时间".into(), Value::Text(self.generated_at)),
            ("模式".into(), Value::Text(self.mode)),
            ("可复现级别".into(), Value::Text(self.reproducibility)),
            (
                "模型端点".into(),
                self.endpoint.map(Value::Text).unwrap_or(Value::Null),
            ),
            ("已发生外发".into(), Value::Bool(self.external_sent)),
        ]))
    }
}

pub(super) fn ai_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    match name {
        "配置" => {
            let config = config_from_value(args.first(), base_dir, permissions)?;
            Ok(config_value(&config))
        }
        "模拟响应" => {
            let text = args.first().map(input_text).unwrap_or_default();
            Ok(response_value("mock-response", "模拟", &text, None))
        }
        "分析结论" => analysis_conclusion(&args, &named, base_dir, permissions),
        "生成报告" => generate_report(&args, &named, base_dir, permissions),
        "提取结构化" => structured_extract(&args, &named, base_dir, permissions),
        "请求" => {
            let prompt = args.first().ok_or_else(|| error("人工智能请求缺少提示"))?;
            let config = config_from_value(args.get(1), base_dir, permissions)?;
            let mode = named
                .get("模式")
                .map(ToString::to_string)
                .unwrap_or_else(|| "在线".into());
            match mode.as_str() {
                "离线" => Ok(response_value(
                    "offline-response",
                    "离线",
                    "人工智能离线模式未调用模型",
                    None,
                )),
                "模拟" => Ok(response_value(
                    "mock-response",
                    "模拟",
                    &input_text(prompt),
                    None,
                )),
                "重放" => replay_response(&named, base_dir, permissions, &config),
                "记录" | "在线" => {
                    request_online(prompt, &config, &mode, &named, base_dir, permissions)
                }
                other => Err(error(format!(
                    "人工智能模式只支持在线、记录、重放、模拟或离线，收到“{other}”"
                ))),
            }
        }
        _ => Err(error(format!("人工智能模块不支持“{name}”"))),
    }
}

fn generate_report(
    args: &[Value],
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    let result = args
        .first()
        .ok_or_else(|| error("人工智能生成报告缺少分析结果"))?;
    if !matches!(result, Value::Record(_) | Value::Table(_)) {
        return Err(error("人工智能生成报告只接受记录或表格分析结果"));
    }
    ensure_collection_limit(result, permissions.collection_limit())
        .map_err(|_| error("人工智能分析结果超过集合资源限制"))?;
    let title = named
        .get("标题")
        .map(ToString::to_string)
        .unwrap_or_else(|| "AI 增强分析报告".into());
    let requirement = args
        .get(1)
        .map(ToString::to_string)
        .unwrap_or_else(|| "请区分数据事实、模型解释和建议行动。".into());
    let mut conclusion_named = named.clone();
    conclusion_named.insert("__任务类型".into(), Value::Text("生成报告".into()));
    let conclusion = analysis_conclusion(
        &[result.clone(), Value::Text(requirement)],
        &conclusion_named,
        base_dir,
        permissions,
    );
    let (text, references, citation_details, usage, warning, endpoint, external_sent) =
        match conclusion {
            Ok(Value::Record(conclusion_record)) => {
                let conclusion_record = conclusion_record.borrow();
                (
                    conclusion_record
                        .get("文本")
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    conclusion_record
                        .get("引用字段")
                        .cloned()
                        .unwrap_or_else(|| Value::list(Vec::new())),
                    conclusion_record
                        .get("引用")
                        .cloned()
                        .unwrap_or_else(|| Value::list(Vec::new())),
                    conclusion_record
                        .get("用量")
                        .cloned()
                        .unwrap_or(Value::Null),
                    None,
                    conclusion_record.get("模型端点").and_then(|value| {
                        (!matches!(value, Value::Null)).then(|| value.to_string())
                    }),
                    conclusion_record
                        .get("已发生外发")
                        .is_some_and(|value| value.to_string() == "真"),
                )
            }
            Ok(_) => return Err(error("人工智能结论结构无效")),
            Err(reason) => (
                "AI 未生成结论，已保留基础统计结果。".into(),
                reference_fields(result),
                reference_details(result),
                Value::Null,
                Some(reason),
                None,
                false,
            ),
        };
    let report_theme = named
        .get("主题")
        .cloned()
        .unwrap_or_else(|| Value::Text("分析报告".into()));
    let report = crate::report::call(
        "报告",
        "创建",
        vec![
            Value::Text(title.clone()),
            Value::list(vec![
                Value::record(BTreeMap::from([
                    ("标题".into(), Value::Text("AI 结论".into())),
                    ("类型".into(), Value::Text("文字".into())),
                    ("内容".into(), Value::Text(text.clone())),
                ])),
                Value::record(BTreeMap::from([
                    ("标题".into(), Value::Text("引用结果".into())),
                    ("类型".into(), Value::Text("表格".into())),
                    ("内容".into(), result.clone()),
                ])),
            ]),
        ],
        BTreeMap::from([("主题".into(), report_theme)]),
        base_dir,
        permissions,
    )?;
    let report = apply_section_order(report, named.get("章节顺序"));
    let mode = named
        .get("模式")
        .map(ToString::to_string)
        .unwrap_or_else(|| "离线".into());
    let reproducibility = if matches!(mode.as_str(), "离线" | "模拟" | "重放") {
        "完全可复现"
    } else {
        "不可复现"
    };
    let warning_html = warning
        .as_deref()
        .map(|value| {
            format!(
                "<section><h2>AI 警告</h2><p>{}</p></section>",
                escape_html(value)
            )
        })
        .unwrap_or_default();
    let default_html = format!(
        "<article><h1>{}</h1><section><h2>AI 结论</h2><p>{}</p></section><section><h2>引用字段</h2><p>{}</p></section>{}</article>",
        escape_html(&title), escape_html(&text).replace('\n', "<br>"), escape_html(&references.to_string()), warning_html
    );
    let requested_html = named
        .get("HTML模板")
        .map(ToString::to_string)
        .map(|template| {
            template
                .replace("{{标题}}", &escape_html(&title))
                .replace("{{结论}}", &escape_html(&text).replace('\n', "<br>"))
                .replace("{{引用}}", &escape_html(&citation_details.to_string()))
                .replace("{{警告}}", &warning_html)
        })
        .unwrap_or_else(|| default_html.clone());
    let (html, html_warning) =
        match validate_html_draft(&requested_html, permissions.output_limit()) {
            Ok(()) => (requested_html, None),
            Err(reason) => (default_html, Some(reason)),
        };
    let warning = match (warning, html_warning) {
        (Some(left), Some(right)) => Some(format!("{left}；{right}")),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    Ok(AiReportDraft {
        status: if warning.is_some() {
            "部分成功"
        } else {
            "成功"
        }
        .into(),
        html,
        report,
        references,
        citation_details,
        input_summary: format!("字段数={}", input_fields(result)),
        warning,
        usage: (!matches!(usage, Value::Null)).then_some(usage),
        generated_at: Utc::now().to_rfc3339(),
        mode,
        reproducibility: reproducibility.into(),
        endpoint,
        external_sent,
    }
    .into_value())
}

fn validate_html_draft(html: &str, max_bytes: usize) -> Result<(), String> {
    if html.len() > max_bytes {
        return Err("AI HTML 草稿超过输出字节预算".into());
    }
    if html.matches('<').count() > MAX_HTML_NODES {
        return Err("AI HTML 草稿超过节点预算".into());
    }
    let lower = html.to_ascii_lowercase();
    for forbidden in [
        "<script",
        "javascript:",
        "<iframe",
        "<object",
        "<embed",
        "src=\"http:",
        "src=\"https:",
        "href=\"http:",
        "href=\"https:",
    ] {
        if lower.contains(forbidden) {
            return Err(format!("AI HTML 草稿包含不支持的内容“{forbidden}”"));
        }
    }
    Ok(())
}

fn apply_section_order(report: Value, order: Option<&Value>) -> Value {
    let Some(order) = order else { return report };
    let Value::Record(record) = &report else {
        return report;
    };
    let Value::List(order) = order else {
        return report;
    };
    let names = order
        .borrow()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return report;
    }
    {
        let mut record = record.borrow_mut();
        let sections = match record.get("章节") {
            Some(Value::List(sections)) => Some(sections.borrow().clone()),
            _ => None,
        };
        if let Some(sections) = sections {
            let mut sorted = sections;
            sorted.sort_by_key(|section| {
                let title = match section {
                    Value::Record(value) => value
                        .borrow()
                        .get("标题")
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                names
                    .iter()
                    .position(|name| name == &title)
                    .unwrap_or(names.len())
            });
            record.insert("章节".into(), Value::list(sorted));
        }
    }
    report
}

fn input_fields(value: &Value) -> usize {
    match value {
        Value::Record(record) => record.borrow().len(),
        Value::Table(table) => table.borrow().rows.first().map_or(0, BTreeMap::len),
        _ => 0,
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn analysis_conclusion(
    args: &[Value],
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    let result = args
        .first()
        .ok_or_else(|| error("人工智能分析结论缺少分析结果"))?;
    if !matches!(result, Value::Record(_) | Value::Table(_)) {
        return Err(error("人工智能分析结论只接受记录或表格分析结果"));
    }
    ensure_collection_limit(result, permissions.collection_limit())
        .map_err(|_| error("人工智能分析结果超过集合资源限制"))?;
    let instruction = args
        .get(1)
        .map(ToString::to_string)
        .unwrap_or_else(|| "请区分数据事实、模型解释和建议行动，并只引用输入结果中的字段。".into());
    let audience = args.get(2).map(ToString::to_string);
    let task = named
        .get("__任务类型")
        .map(ToString::to_string)
        .unwrap_or_else(|| "分析结论".into());
    let context = prompt_context(result, &task, &instruction, audience.as_deref())?;
    let mut request_named = named.clone();
    request_named.insert(
        "__系统指令".into(),
        Value::Text(ANALYSIS_INSTRUCTIONS.into()),
    );
    request_named.insert("__响应格式".into(), from_json(&analysis_response_format()));
    request_named.insert(
        "模式".into(),
        named
            .get("模式")
            .cloned()
            .unwrap_or_else(|| Value::Text("离线".into())),
    );
    let response = ai_call(
        "请求",
        vec![Value::Text(
            serde_json::to_string(&context).map_err(|_| error("人工智能分析上下文编码失败"))?,
        )],
        request_named,
        base_dir,
        permissions,
    )?;
    let Value::Record(response_record) = response else {
        return Err(error("人工智能响应结构无效"));
    };
    let mut output = response_record.borrow().clone();
    let references = reference_fields(result);
    output.insert("引用字段".into(), references);
    output.insert("引用".into(), reference_details(result));
    output.insert(
        "事实".into(),
        Value::Text("事实必须回到引用字段核对".into()),
    );
    output.insert(
        "模型解释".into(),
        Value::Text("模型解释不等同于统计显著性".into()),
    );
    output.insert(
        "建议行动".into(),
        Value::Text("请结合业务约束复核后再执行".into()),
    );
    Ok(Value::Record(std::rc::Rc::new(std::cell::RefCell::new(
        output,
    ))))
}

fn prompt_context(
    result: &Value,
    task: &str,
    instruction: &str,
    audience: Option<&str>,
) -> Result<JsonValue, String> {
    Ok(json!({
        "提示版本": AI_PROMPT_VERSION,
        "任务类型": task,
        "用户目标": instruction,
        "受众": audience,
        "约束": {
            "统计事实来源": "仅限分析上下文",
            "禁止": ["补造字段", "补造显著性", "把模型置信度当统计置信度", "忽略警告"],
            "引用要求": "每个数据事实必须使用允许引用路径"
        },
        "分析上下文": analysis_context(result)?,
        "允许引用": reference_details_json(result),
        "输出结构": {
            "数据事实": [{"文本": "string", "引用": ["结果.字段"]}],
            "模型解释": [{"文本": "string", "引用": ["结果.字段"]}],
            "建议行动": [{"文本": "string", "依据": ["结果.字段"]}],
            "警告": ["string"]
        }
    }))
}

fn reference_details_json(value: &Value) -> JsonValue {
    match to_json(&reference_details(value)) {
        Ok(value) => value,
        Err(_) => JsonValue::Array(Vec::new()),
    }
}

fn reference_fields(value: &Value) -> Value {
    match value {
        Value::Record(record) => {
            Value::list(record.borrow().keys().cloned().map(Value::Text).collect())
        }
        Value::Table(table) => Value::list(
            table
                .borrow()
                .rows
                .first()
                .map(|row| row.keys().cloned().map(Value::Text).collect())
                .unwrap_or_default(),
        ),
        _ => Value::list(Vec::new()),
    }
}

fn reference_details(value: &Value) -> Value {
    let fields = match value {
        Value::Record(record) => record
            .borrow()
            .keys()
            .map(|field| {
                Value::record(BTreeMap::from([
                    ("路径".into(), Value::Text(format!("结果.{field}"))),
                    ("字段".into(), Value::Text(field.clone())),
                ]))
            })
            .collect(),
        Value::Table(table) => table
            .borrow()
            .rows
            .first()
            .map(|row| {
                row.keys()
                    .map(|field| {
                        Value::record(BTreeMap::from([
                            ("路径".into(), Value::Text(format!("结果[0].{field}"))),
                            ("字段".into(), Value::Text(field.clone())),
                        ]))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Value::list(fields)
}

fn analysis_context(value: &Value) -> Result<JsonValue, String> {
    let context = match value {
        Value::Record(record) => {
            let record = record.borrow();
            let fields = record
                .iter()
                .take(64)
                .map(|(name, value)| {
                    json!({"字段": bounded_text(name), "类型": value.type_name(), "示例": context_preview(value)})
                })
                .collect::<Vec<_>>();
            let metadata = context_metadata(&record);
            json!({"类型": "记录", "字段数": record.len(), "字段已截断": record.len() > fields.len(), "字段": fields, "元数据": metadata})
        }
        Value::Table(table) => {
            let table = table.borrow();
            let columns = table
                .rows
                .first()
                .map(|row| {
                    row.iter()
                        .take(64)
                        .map(|(name, value)| {
                            let unit = table
                                .column_metadata
                                .get(name)
                                .and_then(|m| m.unit.as_ref())
                                .map(|u| u.name.clone());
                            json!({"字段": bounded_text(name), "类型": value.type_name(), "单位": unit.map(|value| bounded_text(&value))})
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let sample = table
                .rows
                .iter()
                .take(5)
                .map(|row| {
                    row.iter()
                        .take(64)
                        .map(|(name, value)| (bounded_text(name), context_preview(value)))
                        .collect::<serde_json::Map<_, _>>()
                })
                .collect::<Vec<_>>();
            let metadata = json!({
                "方法": table.metadata.units.keys().map(|_| "表格元数据").next(),
                "警告": JsonValue::Null,
                "来源数": table.metadata.provenance.len()
            });
            json!({"类型": "表格", "行数": table.rows.len(), "列数": table.rows.first().map_or(0, BTreeMap::len), "列": columns, "样本": sample, "元数据": metadata})
        }
        _ => input_json(value)?,
    };
    let encoded = serde_json::to_vec(&context).map_err(|_| error("人工智能分析上下文编码失败"))?;
    if encoded.len() <= MAX_CONTEXT_BYTES {
        return Ok(context);
    }
    Ok(json!({
        "类型": value.type_name(),
        "上下文已截断": true,
        "原始字节数": encoded.len(),
        "摘要": "上下文超过限制；请缩小输入字段或先使用本地统计摘要"
    }))
}

fn context_metadata(record: &BTreeMap<String, Value>) -> JsonValue {
    let mut metadata = serde_json::Map::new();
    for key in ["方法", "警告", "字段说明", "区间方法", "模型", "样本量"] {
        if let Some(value) = record.get(key) {
            metadata.insert(key.into(), context_preview(value));
        }
    }
    JsonValue::Object(metadata)
}

fn context_preview(value: &Value) -> JsonValue {
    match value {
        Value::Text(text) => JsonValue::String(bounded_text(text)),
        Value::Table(_) | Value::Record(_) => analysis_context(value).unwrap_or(JsonValue::Null),
        Value::List(values) => {
            let values = values.borrow();
            json!({
                "项数": values.len(),
                "样本": values.iter().take(5).map(context_preview).collect::<Vec<_>>()
            })
        }
        _ => input_json(value).unwrap_or_else(|_| JsonValue::String(value.type_name().into())),
    }
}

fn bounded_text(value: &str) -> String {
    let mut chars = value.chars();
    let preview = chars.by_ref().take(256).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...[已截断]")
    } else {
        preview
    }
}

fn config_from_value(
    value: Option<&Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<AiConfig, String> {
    let mut base_url = config_value_from_dir(base_dir, "HENGSHU_AI_BASE_URL", permissions)
        .unwrap_or_else(|| DEFAULT_BASE_URL.into());
    let mut model_name = config_value_from_dir(base_dir, "HENGSHU_AI_MODEL_NAME", permissions)
        .unwrap_or_else(|| DEFAULT_MODEL.into());
    let mut api_key = config_value_from_dir(base_dir, "HENGSHU_AI_API_KEY", permissions);
    if let Some(Value::Record(record)) = value {
        let record = record.borrow();
        if let Some(value) = record.get("baseUrl").or_else(|| record.get("基础地址")) {
            base_url = value.to_string();
        }
        if let Some(value) = record.get("modelName").or_else(|| record.get("模型名")) {
            model_name = value.to_string();
        }
        if let Some(value) = record
            .get("apiKeyEnv")
            .or_else(|| record.get("密钥环境变量"))
        {
            let name = value.to_string();
            permissions.check_environment(&name)?;
            api_key = crate::dotenv::value(base_dir, &name);
        }
    } else if value.is_some() {
        return Err(error("人工智能配置必须是记录"));
    }
    let parsed = Url::parse(&base_url).map_err(|_| error("人工智能 baseUrl 不是有效 URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(error("人工智能 baseUrl 必须是 HTTP 或 HTTPS URL"));
    }
    if model_name.trim().is_empty() {
        return Err(error("人工智能 modelName 不能为空"));
    }
    Ok(AiConfig {
        base_url: base_url.trim_end_matches('/').into(),
        model_name,
        api_key,
    })
}

fn config_value_from_dir(
    base_dir: &Path,
    name: &str,
    permissions: &PermissionSet,
) -> Option<String> {
    if permissions.check_environment(name).is_err() {
        return None;
    }
    crate::dotenv::value(base_dir, name)
}

fn config_value(config: &AiConfig) -> Value {
    Value::record(BTreeMap::from([
        ("baseUrl".into(), Value::Text(config.base_url.clone())),
        ("modelName".into(), Value::Text(config.model_name.clone())),
        (
            "apiKeyConfigured".into(),
            Value::Bool(config.api_key.is_some()),
        ),
        ("apiKey".into(), Value::Text("<秘密已隐藏>".into())),
    ]))
}

fn request_online(
    prompt: &Value,
    config: &AiConfig,
    mode: &str,
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    let key = config
        .api_key
        .as_deref()
        .ok_or_else(|| error("人工智能缺少 API Key，请设置 HENGSHU_AI_API_KEY"))?;
    let endpoint = format!("{}/responses", config.base_url);
    permissions.check_network(&endpoint)?;
    let timeout = named
        .get("最长等待")
        .map(|value| crate::contracts::timeout_duration(value, "人工智能最长等待"))
        .transpose()?
        .unwrap_or(DEFAULT_TIMEOUT);
    let input = input_payload(prompt, named, base_dir, permissions)?;
    let input_hash = digest_json(&input);
    let body = response_request_body(config, input, named)?;
    let retries = named
        .get("重试次数")
        .map(Value::as_decimal)
        .transpose()?
        .map(|value| {
            let retries = value
                .to_u32()
                .ok_or_else(|| error("人工智能重试次数必须是非负整数"))?;
            if retries > MAX_RETRIES {
                return Err(error(format!("人工智能重试次数不能超过 {MAX_RETRIES}")));
            }
            Ok(retries)
        })
        .transpose()?
        .unwrap_or(0);
    let retry_delay = named
        .get("再次尝试前等待")
        .map(|value| crate::contracts::timeout_duration(value, "人工智能再次尝试前等待"))
        .transpose()?
        .unwrap_or_else(|| Duration::from_millis(100));
    let mut payload = None;
    let mut last_error = None;
    for attempt in 0..=retries {
        let result = ureq::AgentBuilder::new()
            .timeout(timeout)
            .build()
            .post(&endpoint)
            .set("Authorization", &format!("Bearer {key}"))
            .set("Content-Type", "application/json")
            .send_json(&body);
        match result {
            Ok(value) => {
                let status = value.status();
                let bytes = read_ai_response(value, permissions)?;
                match parse_response_payload(&bytes) {
                    Ok(value) => {
                        payload = Some(value);
                        break;
                    }
                    Err(parse_error) => {
                        last_error = gateway_error_message(&bytes).or(Some(parse_error));
                        if attempt < retries {
                            std::thread::sleep(retry_delay);
                            continue;
                        }
                        return Err(error(format!(
                            "人工智能请求失败：{}",
                            last_error.unwrap_or_else(|| format!("HTTP {status}"))
                        )));
                    }
                }
            }
            Err(ureq::Error::Status(status, _value))
                if retryable_status(status) && attempt < retries =>
            {
                last_error = Some(format!("HTTP {status}"));
                std::thread::sleep(retry_delay);
            }
            Err(ureq::Error::Status(status, value)) => {
                let _ = read_ai_response(value, permissions)?;
                return Err(error(format!("人工智能服务返回 HTTP {status}")));
            }
            Err(error_value) => {
                last_error = Some(error_value.to_string());
                if attempt < retries {
                    std::thread::sleep(retry_delay);
                }
            }
        }
    }
    let payload = payload.ok_or_else(|| {
        error(format!(
            "人工智能请求失败：{}",
            last_error.unwrap_or_default()
        ))
    })?;

    let text = response_text(&payload).unwrap_or_default();
    let response_id = payload
        .get("id")
        .and_then(JsonValue::as_str)
        .unwrap_or("response")
        .to_string();
    let result = response_value(&response_id, mode, &text, Some(payload.clone()));
    if let Value::Record(record) = &result {
        let mut record = record.borrow_mut();
        record.insert("模型端点".into(), Value::Text(config.base_url.clone()));
        record.insert("已发生外发".into(), Value::Bool(true));
    }
    if mode == "记录" {
        write_record(
            named,
            base_dir,
            permissions,
            &body,
            &payload,
            &input_hash,
            &response_id,
        )?;
    }
    Ok(result)
}

fn read_ai_response(
    response: ureq::Response,
    permissions: &PermissionSet,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take((permissions.buffer_limit() as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| error("人工智能响应读取失败"))?;
    if bytes.len() > permissions.buffer_limit() {
        return Err(error("人工智能响应超过缓冲资源限制"));
    }
    Ok(bytes)
}

/// Some OpenAI-compatible gateways prepend SSE diagnostics (for example an
/// `event: error` line) and then emit the final Responses JSON object. Accept
/// ordinary JSON first, then parse `data:` records and prefer a response-shaped
/// object so nested content objects cannot mask the final response.
fn parse_response_payload(bytes: &[u8]) -> Result<JsonValue, String> {
    if let Ok(payload) = serde_json::from_slice(bytes) {
        return Ok(payload);
    }
    let mut parsed = None;
    for line in String::from_utf8_lossy(bytes).lines() {
        let candidate = line.trim().strip_prefix("data:").unwrap_or(line.trim());
        if let Ok(payload) = serde_json::from_str::<JsonValue>(candidate.trim()) {
            if is_response_payload(&payload) {
                parsed = Some(payload);
            }
        }
    }
    if let Some(payload) = parsed {
        return Ok(payload);
    }
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'{' {
            continue;
        }
        let mut stream =
            serde_json::Deserializer::from_slice(&bytes[index..]).into_iter::<JsonValue>();
        if let Some(Ok(payload)) = stream.next() {
            if is_response_payload(&payload) {
                parsed = Some(payload);
            }
        }
    }
    parsed.ok_or_else(|| error("人工智能响应 JSON 无效"))
}

fn is_response_payload(payload: &JsonValue) -> bool {
    payload.get("object").and_then(JsonValue::as_str) == Some("response")
        || payload.get("output_text").is_some()
        || payload.get("output").is_some()
}

fn gateway_error_message(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes).lines().find_map(|line| {
        let candidate = line.trim().strip_prefix("data:")?.trim();
        let payload = serde_json::from_str::<JsonValue>(candidate).ok()?;
        payload
            .get("error")?
            .get("message")?
            .as_str()
            .map(str::to_string)
    })
}

fn response_request_body(
    config: &AiConfig,
    input: JsonValue,
    named: &BTreeMap<String, Value>,
) -> Result<JsonValue, String> {
    let mut body = json!({
        "model": config.model_name,
        "input": input,
        "instructions": named.get("__系统指令").map(ToString::to_string),
    });
    if let Some(format) = named.get("__响应格式") {
        body["text"] = to_json(format)?;
    }
    Ok(body)
}

#[derive(Clone, Debug)]
struct ExtractField {
    name: String,
    kind: String,
    description: Option<String>,
    nullable: bool,
}

fn structured_extract(
    args: &[Value],
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    let source = args
        .first()
        .ok_or_else(|| error("人工智能结构化提取缺少输入内容"))?;
    let specification = args
        .get(1)
        .ok_or_else(|| error("人工智能结构化提取缺少字段定义"))?;
    let fields = parse_extract_fields(specification, permissions.collection_limit())?;
    let mut request_named = named.clone();
    request_named.insert(
        "__响应格式".into(),
        from_json(&extraction_response_format(&fields)),
    );
    request_named.insert(
        "__系统指令".into(),
        Value::Text(
            "从输入中提取字段。不得推测不存在的值；允许空的字段无法确定时返回 null。每个发现的业务对象返回一行。"
                .into(),
        ),
    );
    let response = ai_call(
        "请求",
        vec![source.clone()],
        request_named,
        base_dir,
        permissions,
    )?;
    let Value::Record(record) = response else {
        return Err(error("人工智能结构化提取响应结构无效"));
    };
    let text = record
        .borrow()
        .get("文本")
        .map(ToString::to_string)
        .unwrap_or_default();
    let payload: JsonValue =
        serde_json::from_str(&text).map_err(|_| error("人工智能结构化提取结果不是有效 JSON"))?;
    let table = extraction_table(&payload, &fields, permissions.collection_limit())?;
    let row_count = match &table {
        Value::Table(table) => table.borrow().rows.len(),
        _ => 0,
    };
    let mut output = record.borrow().clone();
    output.insert("数据".into(), table);
    output.insert("行数".into(), Value::Number(Decimal::from(row_count)));
    output.insert(
        "字段".into(),
        Value::list(
            fields
                .iter()
                .map(|field| Value::Text(field.name.clone()))
                .collect(),
        ),
    );
    Ok(Value::record(output))
}

fn parse_extract_fields(
    value: &Value,
    collection_limit: usize,
) -> Result<Vec<ExtractField>, String> {
    let fields = match value {
        Value::Record(record) => record
            .borrow()
            .iter()
            .map(|(name, kind)| extract_field(name, Some(kind), None, false))
            .collect::<Result<Vec<_>, _>>()?,
        Value::List(values) => {
            let values = values.borrow();
            if values.len() > collection_limit {
                return Err(error("人工智能结构化提取字段数量超过集合资源限制"));
            }
            values
                .iter()
                .map(|value| match value {
                    Value::Text(name) => extract_field(name, None, None, false),
                    Value::Record(record) => {
                        let record = record.borrow();
                        let name = record
                            .get("名称")
                            .or_else(|| record.get("字段"))
                            .ok_or_else(|| error("结构化提取字段定义缺少“名称”"))?
                            .to_string();
                        let description = record.get("说明").map(ToString::to_string);
                        let nullable = record.get("允许空").is_some_and(Value::truth);
                        extract_field(&name, record.get("类型"), description, nullable)
                    }
                    _ => Err(error("结构化提取字段列表只接受文本或记录")),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => return Err(error("结构化提取字段定义必须是记录或列表")),
    };
    if fields.is_empty() {
        return Err(error("结构化提取至少需要一个字段"));
    }
    if fields.len() > collection_limit {
        return Err(error("人工智能结构化提取字段数量超过集合资源限制"));
    }
    let mut names = BTreeSet::new();
    for field in &fields {
        if !names.insert(field.name.clone()) {
            return Err(error(format!("结构化提取字段“{}”重复", field.name)));
        }
    }
    Ok(fields)
}

fn extract_field(
    name: &str,
    kind: Option<&Value>,
    description: Option<String>,
    nullable: bool,
) -> Result<ExtractField, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(error("结构化提取字段名称不能为空"));
    }
    let kind = kind
        .map(ToString::to_string)
        .unwrap_or_else(|| "文本".into());
    if !matches!(
        kind.as_str(),
        "文本"
            | "数字"
            | "整数"
            | "逻辑"
            | "日期"
            | "日期时间"
            | "文本列表"
            | "数字列表"
            | "整数列表"
            | "逻辑列表"
    ) {
        return Err(error(format!(
            "结构化提取字段“{name}”的类型“{kind}”不受支持"
        )));
    }
    Ok(ExtractField {
        name: name.into(),
        kind,
        description,
        nullable,
    })
}

fn extraction_response_format(fields: &[ExtractField]) -> JsonValue {
    let properties = fields
        .iter()
        .map(|field| (field.name.clone(), extraction_field_schema(field)))
        .collect::<serde_json::Map<_, _>>();
    let required = fields
        .iter()
        .map(|field| JsonValue::String(field.name.clone()))
        .collect::<Vec<_>>();
    json!({
        "format": {
            "type": "json_schema",
            "name": "hengshu_table_extraction",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "数据": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": properties,
                            "required": required,
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["数据"],
                "additionalProperties": false
            }
        }
    })
}

fn extraction_field_schema(field: &ExtractField) -> JsonValue {
    let (kind, item_kind) = match field.kind.as_str() {
        "文本" | "日期" | "日期时间" => ("string", None),
        "数字" => ("number", None),
        "整数" => ("integer", None),
        "逻辑" => ("boolean", None),
        "文本列表" => ("array", Some("string")),
        "数字列表" => ("array", Some("number")),
        "整数列表" => ("array", Some("integer")),
        "逻辑列表" => ("array", Some("boolean")),
        _ => unreachable!("字段类型已验证"),
    };
    let mut value_schema = serde_json::Map::new();
    value_schema.insert("type".into(), json!(kind));
    if let Some(item_kind) = item_kind {
        value_schema.insert("items".into(), json!({"type": item_kind}));
    }
    let date_requirement = match field.kind.as_str() {
        "日期" => Some("使用 YYYY-MM-DD 格式"),
        "日期时间" => Some("使用 RFC 3339 格式并包含时区"),
        _ => None,
    };
    let description = [field.description.as_deref(), date_requirement]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("；");
    if !description.is_empty() {
        value_schema.insert("description".into(), JsonValue::String(description));
    }
    if field.nullable {
        json!({"anyOf": [JsonValue::Object(value_schema), {"type": "null"}]})
    } else {
        JsonValue::Object(value_schema)
    }
}

fn extraction_table(
    payload: &JsonValue,
    fields: &[ExtractField],
    collection_limit: usize,
) -> Result<Value, String> {
    let rows = payload
        .get("数据")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| error("人工智能结构化提取结果缺少“数据”数组"))?;
    if rows.len() > collection_limit {
        return Err(error("人工智能结构化提取结果超过集合资源限制"));
    }
    let expected = fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut table_rows = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let object = row
            .as_object()
            .ok_or_else(|| error(format!("结构化提取结果第 {} 行不是记录", index + 1)))?;
        if object.keys().any(|name| !expected.contains(name.as_str())) {
            return Err(error(format!(
                "结构化提取结果第 {} 行包含未声明字段",
                index + 1
            )));
        }
        let mut output = BTreeMap::new();
        for field in fields {
            let value = object.get(&field.name).ok_or_else(|| {
                error(format!(
                    "结构化提取结果第 {} 行缺少字段“{}”",
                    index + 1,
                    field.name
                ))
            })?;
            validate_extracted_value(value, field, index + 1)?;
            output.insert(field.name.clone(), from_json(value));
        }
        table_rows.push(output);
    }
    Ok(Value::Table(std::rc::Rc::new(std::cell::RefCell::new(
        crate::value::Table {
            rows: table_rows,
            ..Default::default()
        },
    ))))
}

fn validate_extracted_value(
    value: &JsonValue,
    field: &ExtractField,
    row: usize,
) -> Result<(), String> {
    if value.is_null() {
        return if field.nullable {
            Ok(())
        } else {
            Err(error(format!(
                "结构化提取结果第 {row} 行字段“{}”不允许为空",
                field.name
            )))
        };
    }
    let valid = match field.kind.as_str() {
        "文本" | "日期" | "日期时间" => value.is_string(),
        "数字" => value.is_number(),
        "整数" => value.as_i64().is_some() || value.as_u64().is_some(),
        "逻辑" => value.is_boolean(),
        "文本列表" => json_array_is(value, JsonValue::is_string),
        "数字列表" => json_array_is(value, JsonValue::is_number),
        "整数列表" => json_array_is(value, |value| {
            value.as_i64().is_some() || value.as_u64().is_some()
        }),
        "逻辑列表" => json_array_is(value, JsonValue::is_boolean),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(error(format!(
            "结构化提取结果第 {row} 行字段“{}”不是{}",
            field.name, field.kind
        )))
    }
}

fn json_array_is(value: &JsonValue, predicate: impl Fn(&JsonValue) -> bool) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.iter().all(predicate))
}

fn analysis_response_format() -> JsonValue {
    json!({
        "format": {
            "type": "json_schema",
            "name": "hengshu_analysis_conclusion",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "数据事实": {"type": "array", "items": {"type": "object", "properties": {"文本": {"type": "string"}, "引用": {"type": "array", "items": {"type": "string"}}}, "required": ["文本", "引用"], "additionalProperties": false}},
                    "模型解释": {"type": "array", "items": {"type": "object", "properties": {"文本": {"type": "string"}, "引用": {"type": "array", "items": {"type": "string"}}}, "required": ["文本", "引用"], "additionalProperties": false}},
                    "建议行动": {"type": "array", "items": {"type": "object", "properties": {"文本": {"type": "string"}, "依据": {"type": "array", "items": {"type": "string"}}}, "required": ["文本", "依据"], "additionalProperties": false}},
                    "警告": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["数据事实", "模型解释", "建议行动", "警告"],
                "additionalProperties": false
            }
        }
    })
}

fn retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

fn input_payload(
    prompt: &Value,
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<JsonValue, String> {
    let mut content = vec![json!({"type": "input_text", "text": input_text(prompt)})];
    if let Some(value) = named.get("输入图片") {
        content.push(media_payload(value, "image", named, base_dir, permissions)?);
    }
    if let Some(value) = named.get("输入PDF") {
        content.push(media_payload(value, "pdf", named, base_dir, permissions)?);
    }
    if content.len() == 1 {
        input_json(prompt)
    } else {
        Ok(JsonValue::Array(content))
    }
}

fn media_payload(
    value: &Value,
    kind: &str,
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<JsonValue, String> {
    let source = value.to_string();
    if source.starts_with("data:") {
        if source.len() > permissions.buffer_limit() {
            return Err(error("人工智能媒体 data URL 超过缓冲资源限制"));
        }
        let detail = media_detail(named, kind)?;
        return Ok(if kind == "image" {
            json!({"type": "input_image", "image_url": source, "detail": detail})
        } else {
            json!({"type": "input_file", "file_url": source})
        });
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        permissions.check_network(&source)?;
        let detail = media_detail(named, kind)?;
        return Ok(if kind == "image" {
            json!({"type": "input_image", "image_url": source, "detail": detail})
        } else {
            json!({"type": "input_file", "file_url": source})
        });
    }
    let path = super::path::resolve(base_dir, value);
    permissions.check_file(&path)?;
    let metadata = fs::metadata(&path).map_err(|_| error("人工智能媒体文件读取失败"))?;
    if !metadata.is_file() {
        return Err(error("人工智能媒体输入必须是文件"));
    }
    let max = permissions.buffer_limit();
    if metadata.len() > max as u64 {
        return Err(error("人工智能媒体文件超过缓冲资源限制"));
    }
    let bytes = fs::read(&path).map_err(|_| error("人工智能媒体文件读取失败"))?;
    let detail = media_detail(named, kind)?;
    if kind == "image" {
        let dimensions = image::ImageReader::open(&path)
            .map_err(|_| error("人工智能图片解码失败"))?
            .with_guessed_format()
            .map_err(|_| error("人工智能图片格式无效"))?
            .into_dimensions()
            .map_err(|_| error("人工智能图片尺寸读取失败"))?;
        if dimensions.0 > MAX_IMAGE_DIMENSION || dimensions.1 > MAX_IMAGE_DIMENSION {
            return Ok(json!({
                "type": "input_text",
                "text": format!("图片摘要：文件={}，尺寸={}x{}，detail={}；图片超过原图发送尺寸上限。", path.display(), dimensions.0, dimensions.1, detail)
            }));
        }
    }
    if kind == "pdf" {
        let pages = lopdf::Document::load_mem(&bytes)
            .map_err(|_| error("人工智能 PDF 解析失败"))?
            .get_pages()
            .len();
        if pages > MAX_PDF_PAGES {
            return Ok(json!({
                "type": "input_text",
                "text": format!("PDF 摘要：文件={}，页数={}，detail={}；超过 {} 页，仅发送摘要。", path.display(), pages, detail, MAX_PDF_PAGES)
            }));
        }
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    if encoded.len() > permissions.buffer_limit() {
        return Err(error("人工智能媒体 Base64 编码超过缓冲资源限制"));
    }
    let mime = media_mime(&path, kind)?;
    let data = format!("data:{mime};base64,{encoded}");
    Ok(if kind == "image" {
        json!({"type": "input_image", "image_url": data, "detail": detail})
    } else {
        json!({"type": "input_file", "filename": path.file_name().and_then(|v| v.to_str()).unwrap_or("input.pdf"), "file_data": data})
    })
}

fn media_detail(named: &BTreeMap<String, Value>, kind: &str) -> Result<&'static str, String> {
    let value = named
        .get(if kind == "pdf" { "PDF细节" } else { "细节" })
        .map(ToString::to_string)
        .unwrap_or_else(|| "auto".into());
    match value.as_str() {
        "auto" | "自动" => Ok("auto"),
        "low" | "低" => Ok("low"),
        "high" | "高" => Ok("high"),
        _ => Err(error("人工智能媒体细节只支持 auto、low 或 high")),
    }
}

fn media_mime(path: &Path, kind: &str) -> Result<&'static str, String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match (kind, extension.as_str()) {
        ("image", "png") => Ok("image/png"),
        ("image", "jpg" | "jpeg") => Ok("image/jpeg"),
        ("image", "webp") => Ok("image/webp"),
        ("pdf", "pdf") => Ok("application/pdf"),
        ("image", _) => Err(error("人工智能图片只支持 PNG、JPEG 或 WebP")),
        ("pdf", _) => Err(error("人工智能 PDF 输入必须使用 .pdf 文件")),
        _ => Err(error("人工智能媒体类型无效")),
    }
}

fn digest_json(value: &JsonValue) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

fn write_record(
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
    request: &JsonValue,
    response: &JsonValue,
    input_hash: &str,
    response_id: &str,
) -> Result<(), String> {
    let path = named
        .get("记录文件")
        .ok_or_else(|| error("人工智能记录模式需要记录文件"))?;
    let path = super::path::resolve(base_dir, path);
    permissions.check_file(&path)?;
    let record = json!({
        "schema": 1,
        "request": request,
        "response": response,
        "metadata": {
            "modelName": request.get("model").and_then(JsonValue::as_str).unwrap_or(""),
            "responseId": response_id,
            "inputHash": input_hash,
            "responseHash": digest_json(response),
            "recordedAt": Utc::now().to_rfc3339(),
            "usage": response.get("usage").cloned().unwrap_or(JsonValue::Null),
        }
    });
    let bytes = serde_json::to_vec(&record).map_err(|_| error("人工智能记录编码失败"))?;
    if bytes.len() > permissions.output_limit() {
        return Err(error("人工智能记录超过输出字节预算"));
    }
    crate::security::atomic_write_file(&path, &bytes, true).map_err(error)
}

fn input_json(value: &Value) -> Result<JsonValue, String> {
    match value {
        Value::Text(text) => Ok(JsonValue::String(text.clone())),
        _ => to_json(value),
    }
}

fn input_text(value: &Value) -> String {
    input_json(value)
        .ok()
        .and_then(|json| match json {
            JsonValue::String(text) => Some(text),
            value => serde_json::to_string(&value).ok(),
        })
        .unwrap_or_else(|| value.to_string())
}

fn replay_response(
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
    _config: &AiConfig,
) -> Result<Value, String> {
    let path = named
        .get("响应文件")
        .ok_or_else(|| error("人工智能重放需要响应文件"))?;
    let path = super::path::resolve(base_dir, path);
    permissions.check_file(&path)?;
    let text = read_string_limited(&path, permissions.buffer_limit())
        .map_err(|_| error("人工智能重放响应读取失败"))?;
    let payload: JsonValue =
        serde_json::from_str(&text).map_err(|_| error("人工智能重放 JSON 无效"))?;
    let payload = payload.get("response").cloned().unwrap_or(payload);
    let response_id = payload
        .get("id")
        .and_then(JsonValue::as_str)
        .unwrap_or("replay-response")
        .to_string();
    let output = response_text(&payload).unwrap_or_default();
    Ok(response_value(&response_id, "重放", &output, Some(payload)))
}

fn response_text(payload: &JsonValue) -> Option<String> {
    if let Some(text) = payload.get("output_text").and_then(JsonValue::as_str) {
        return Some(text.into());
    }
    payload
        .get("output")
        .and_then(JsonValue::as_array)
        .and_then(|items| items.iter().find_map(|item| item.get("content")))
        .and_then(JsonValue::as_array)
        .and_then(|items| items.iter().find_map(|item| item.get("text")))
        .and_then(JsonValue::as_str)
        .map(Into::into)
}

fn response_value(response_id: &str, mode: &str, text: &str, raw: Option<JsonValue>) -> Value {
    let usage = raw
        .as_ref()
        .and_then(|value| value.get("usage"))
        .and_then(usage_value);
    AiResult {
        status: "成功".into(),
        text: text.into(),
        mode: mode.into(),
        response_id: response_id.into(),
        warning: None,
        usage,
        raw,
        endpoint: None,
        external_sent: false,
    }
    .into_value()
}

fn usage_value(usage: &JsonValue) -> Option<Value> {
    let object = usage.as_object()?;
    let mut fields = BTreeMap::new();
    for (source, target) in [
        ("input_tokens", "输入令牌"),
        ("output_tokens", "输出令牌"),
        ("total_tokens", "总令牌"),
    ] {
        if let Some(value) = object.get(source).and_then(JsonValue::as_u64) {
            fields.insert(target.into(), Value::Number(Decimal::from(value)));
        }
    }
    (!fields.is_empty()).then(|| Value::record(fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permissions() -> PermissionSet {
        let mut permissions = PermissionSet::restricted_for_source(Path::new("."));
        permissions.environment_names.clear();
        permissions
    }

    #[test]
    fn config_is_deterministic_and_never_returns_the_secret() {
        let config = AiConfig {
            base_url: "https://example.test/v1".into(),
            model_name: "demo".into(),
            api_key: Some("secret-value".into()),
        };
        let value = config_value(&config).to_string();
        assert!(value.contains("<秘密已隐藏>"));
        assert!(!value.contains("secret-value"));
    }

    #[test]
    fn response_text_supports_responses_output_text_and_nested_content() {
        assert_eq!(
            response_text(&json!({"output_text": "hello"})),
            Some("hello".into())
        );
        assert_eq!(
            response_text(&json!({"output": [{"content": [{"text": "nested"}]}]})),
            Some("nested".into())
        );
    }

    #[test]
    fn response_payload_accepts_sse_prefix_before_final_json() {
        let body = br#"event: error
data: {\"error\":{\"message\":\"temporary\"}}

{"id":"resp_final","output_text":"OK"}"#;
        let payload = parse_response_payload(body).unwrap();
        assert_eq!(payload["id"], "resp_final");
        assert_eq!(response_text(&payload).as_deref(), Some("OK"));
    }

    #[test]
    fn response_payload_does_not_select_a_nested_message_object() {
        let body = br#"{"id":"resp_1","object":"response","output":[{"id":"msg_1","type":"message","content":[{"type":"output_text","text":"OK"}]}]}
event: error
data: {"error":{"message":"late diagnostic"}}
}"#;
        let payload = parse_response_payload(body).unwrap();
        assert_eq!(payload["id"], "resp_1");
        assert_eq!(response_text(&payload).as_deref(), Some("OK"));
    }

    #[test]
    fn analysis_response_format_is_a_strict_json_schema() {
        let format = analysis_response_format();
        assert_eq!(format["format"]["type"], "json_schema");
        assert_eq!(format["format"]["strict"], true);
        assert_eq!(format["format"]["schema"]["additionalProperties"], false);
        assert_eq!(
            format["format"]["schema"]["required"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn plain_requests_keep_the_legacy_text_request_shape() {
        let config = AiConfig {
            base_url: "https://example.test/v1".into(),
            model_name: "demo".into(),
            api_key: None,
        };
        let body = response_request_body(&config, json!("hello"), &BTreeMap::new()).unwrap();
        assert_eq!(body["model"], "demo");
        assert!(body.get("text").is_none());
    }

    #[test]
    fn extraction_fields_support_names_shorthand_and_detailed_records() {
        let names = Value::list(vec![
            Value::Text("订单号".into()),
            Value::Text("客户".into()),
        ]);
        let parsed = parse_extract_fields(&names, 10).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed.iter().all(|field| field.kind == "文本"));

        let detailed = Value::list(vec![Value::record(BTreeMap::from([
            ("名称".into(), Value::Text("金额".into())),
            ("类型".into(), Value::Text("数字".into())),
            ("说明".into(), Value::Text("含税金额".into())),
            ("允许空".into(), Value::Bool(true)),
        ]))]);
        let parsed = parse_extract_fields(&detailed, 10).unwrap();
        assert_eq!(parsed[0].name, "金额");
        assert_eq!(parsed[0].kind, "数字");
        assert!(parsed[0].nullable);
    }

    #[test]
    fn extraction_schema_always_wraps_rows_and_json_becomes_a_table() {
        let fields = vec![
            ExtractField {
                name: "订单号".into(),
                kind: "文本".into(),
                description: None,
                nullable: false,
            },
            ExtractField {
                name: "金额".into(),
                kind: "数字".into(),
                description: None,
                nullable: false,
            },
        ];
        let format = extraction_response_format(&fields);
        assert_eq!(
            format["format"]["schema"]["properties"]["数据"]["type"],
            "array"
        );
        let table = extraction_table(
            &json!({"数据": [{"订单号": "A01", "金额": 12.5}, {"订单号": "A02", "金额": 8}]}),
            &fields,
            10,
        )
        .unwrap();
        let Value::Table(table) = table else {
            panic!("结构化数据应直接转换为表格");
        };
        assert_eq!(table.borrow().rows.len(), 2);
        assert_eq!(table.borrow().rows[0]["订单号"].to_string(), "A01");
        assert_eq!(table.borrow().rows[0]["金额"].to_string(), "12.5");
    }

    #[test]
    fn responses_usage_is_returned_as_structured_token_counts() {
        let value = response_value(
            "resp_1",
            "重放",
            "完成",
            Some(json!({
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 7,
                    "total_tokens": 19
                }
            })),
        );
        let Value::Record(record) = value else {
            panic!("响应应为记录");
        };
        let usage = record.borrow().get("用量").cloned().unwrap();
        assert_eq!(
            usage.to_string(),
            r#"{"总令牌": 19, "输入令牌": 12, "输出令牌": 7}"#
        );
    }

    #[test]
    fn analysis_context_describes_fields_without_unbounded_table_payload() {
        let value = Value::record(BTreeMap::from([
            ("样本数".into(), Value::Number(Decimal::from(12))),
            ("警告".into(), Value::Text("无".into())),
        ]));
        let context = analysis_context(&value).unwrap();
        assert_eq!(context["类型"], "记录");
        assert_eq!(context["字段"].as_array().unwrap().len(), 2);

        let rows = (0..12)
            .map(|index| BTreeMap::from([("销售额".into(), Value::Number(Decimal::from(index)))]))
            .collect();
        let mut table = crate::value::Table {
            rows,
            ..Default::default()
        };
        table.column_metadata.insert(
            "销售额".into(),
            crate::execution::Metadata {
                unit: Some(crate::execution::UnitIdentity {
                    name: "元".into(),
                    dimension: "货币".into(),
                    scale_numerator: 1,
                    scale_denominator: 1,
                    basis: None,
                }),
                ..Default::default()
            },
        );
        let context = analysis_context(&Value::Table(std::rc::Rc::new(std::cell::RefCell::new(
            table,
        ))))
        .unwrap();
        assert_eq!(context["行数"], 12);
        assert_eq!(context["列"][0]["单位"], "元");
        assert_eq!(context["样本"].as_array().unwrap().len(), 5);

        let long = Value::record(BTreeMap::from([(
            "备注".into(),
            Value::Text("x".repeat(400)),
        )]));
        let long_context = analysis_context(&long).unwrap();
        assert!(long_context["字段"][0]["示例"]
            .as_str()
            .unwrap()
            .ends_with("...[已截断]"));
    }

    #[test]
    fn structured_input_is_kept_as_json_instead_of_stringified() {
        let value = Value::record(BTreeMap::from([("销售额".into(), Value::Float(12.5))]));
        assert_eq!(input_json(&value).unwrap()["销售额"], json!(12.5));
        assert_eq!(input_text(&value), r#"{"销售额":12.5}"#);
    }

    #[test]
    fn offline_and_mock_modes_never_need_network_or_a_key() {
        let result = ai_call(
            "请求",
            vec![Value::Text("prompt".into())],
            BTreeMap::from([("模式".into(), Value::Text("离线".into()))]),
            Path::new("."),
            &permissions(),
        )
        .unwrap();
        assert!(result.to_string().contains("离线模式"));
    }

    #[test]
    fn default_environment_configuration_respects_the_environment_permission() {
        let permissions = PermissionSet::restricted_for_source(Path::new("."));
        let config = config_from_value(None, Path::new("."), &permissions).unwrap();
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.model_name, DEFAULT_MODEL);
        assert!(config.api_key.is_none());

        let error = config_from_value(
            Some(&Value::record(BTreeMap::from([(
                "apiKeyEnv".into(),
                Value::Text("SECRET_ENV".into()),
            )]))),
            Path::new("."),
            &permissions,
        )
        .unwrap_err();
        assert!(error.contains("环境变量"), "{error}");
    }

    #[test]
    fn configuration_reads_project_environment_file_without_process_mutation() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("环境.配置"),
            "HENGSHU_AI_BASE_URL=https://example.test/v1\nHENGSHU_AI_MODEL_NAME=本地模型\nHENGSHU_AI_API_KEY=secret-from-file\n",
        )
        .unwrap();
        let config = config_from_value(
            None,
            directory.path(),
            &PermissionSet::for_source(directory.path()),
        )
        .unwrap();
        assert_eq!(config.base_url, "https://example.test/v1");
        assert_eq!(config.model_name, "本地模型");
        assert_eq!(config.api_key.as_deref(), Some("secret-from-file"));
    }

    #[test]
    fn media_payloads_are_bounded_and_follow_file_and_network_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("图.png");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3])))
            .save(&image)
            .unwrap();
        let permissions = PermissionSet::restricted_for_source(directory.path());
        let payload = media_payload(
            &Value::Text("图.png".into()),
            "image",
            &BTreeMap::from([("细节".into(), Value::Text("high".into()))]),
            directory.path(),
            &permissions,
        )
        .unwrap();
        assert_eq!(payload["type"], "input_image");
        assert_eq!(payload["detail"], "high");
        assert!(payload["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));

        let denied = media_payload(
            &Value::Text("https://example.test/image.png".into()),
            "image",
            &BTreeMap::new(),
            directory.path(),
            &permissions,
        )
        .unwrap_err();
        assert!(denied.contains("网络主机"), "{denied}");

        let mut tiny = permissions.clone();
        tiny.max_buffer_bytes = Some(4);
        let oversized = media_payload(
            &Value::Text("图.png".into()),
            "image",
            &BTreeMap::new(),
            directory.path(),
            &tiny,
        )
        .unwrap_err();
        assert!(oversized.contains("缓冲资源限制"), "{oversized}");

        let invalid = media_payload(
            &Value::Text("图.png".into()),
            "image",
            &BTreeMap::from([("细节".into(), Value::Text("极致".into()))]),
            directory.path(),
            &permissions,
        )
        .unwrap_err();
        assert!(invalid.contains("auto、low 或 high"), "{invalid}");

        let pdf = directory.path().join("长文档.pdf");
        let mut document = printpdf::PdfDocument::new("测试");
        let pages = (0..21)
            .map(|_| printpdf::PdfPage::new(printpdf::Mm(20.0), printpdf::Mm(20.0), Vec::new()))
            .collect();
        document.with_pages(pages);
        let bytes = document.save(&printpdf::PdfSaveOptions::default(), &mut Vec::new());
        std::fs::write(&pdf, bytes).unwrap();
        let summarized = media_payload(
            &Value::Text("长文档.pdf".into()),
            "pdf",
            &BTreeMap::from([("PDF细节".into(), Value::Text("low".into()))]),
            directory.path(),
            &permissions,
        )
        .unwrap();
        assert_eq!(summarized["type"], "input_text");
        assert!(summarized["text"].as_str().unwrap().contains("仅发送摘要"));
    }

    #[test]
    fn analysis_context_includes_analysis_method_and_warning_metadata() {
        let value = Value::record(BTreeMap::from([
            ("方法".into(), Value::Text("数据概览".into())),
            ("警告".into(), Value::Text("样本较少".into())),
            ("字段说明".into(), Value::Text("销售额为含税金额".into())),
        ]));
        let context = analysis_context(&value).unwrap();
        assert_eq!(context["元数据"]["方法"], "数据概览");
        assert_eq!(context["元数据"]["警告"], "样本较少");
        assert_eq!(context["元数据"]["字段说明"], "销售额为含税金额");
    }

    #[test]
    fn prompt_context_keeps_task_contract_separate_from_user_request() {
        let result = Value::record(BTreeMap::from([
            ("方法".into(), Value::Text("数据概览".into())),
            ("警告".into(), Value::Text("样本较少".into())),
            ("销售额".into(), Value::Number(Decimal::from(120))),
        ]));
        let prompt = prompt_context(&result, "生成报告", "解释季度趋势", Some("管理层")).unwrap();
        assert_eq!(prompt["提示版本"], 1);
        assert_eq!(prompt["任务类型"], "生成报告");
        assert_eq!(prompt["用户目标"], "解释季度趋势");
        assert_eq!(prompt["受众"], "管理层");
        assert_eq!(prompt["分析上下文"]["元数据"]["方法"], "数据概览");
        assert!(prompt["输出结构"]["数据事实"].is_array());
        assert!(prompt["约束"]["禁止"].as_array().unwrap().len() >= 3);
    }

    #[test]
    fn html_draft_validation_rejects_active_external_and_oversized_markup() {
        assert!(validate_html_draft("<main><p>完成</p></main>", 1024).is_ok());
        assert!(validate_html_draft("<script>alert(1)</script>", 1024)
            .unwrap_err()
            .contains("不支持"));
        assert!(
            validate_html_draft("<img src=\"https://example.test/a.png\">", 1024)
                .unwrap_err()
                .contains("不支持")
        );
        assert!(validate_html_draft(&"x".repeat(1025), 1024)
            .unwrap_err()
            .contains("字节预算"));
        assert!(
            validate_html_draft(&"<p>".repeat(MAX_HTML_NODES + 1), usize::MAX)
                .unwrap_err()
                .contains("节点预算")
        );
    }

    #[test]
    fn recorded_response_can_be_replayed_without_network() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("响应.json");
        std::fs::write(&path, r#"{"id":"resp_1","output_text":"cached"}"#).unwrap();
        let permissions = PermissionSet::for_source(directory.path());
        let result = ai_call(
            "请求",
            vec![Value::Text("prompt".into())],
            BTreeMap::from([
                ("模式".into(), Value::Text("重放".into())),
                ("响应文件".into(), Value::Text("响应.json".into())),
            ]),
            directory.path(),
            &permissions,
        )
        .unwrap();
        assert!(result.to_string().contains("cached"));
        assert!(result.to_string().contains("resp_1"));
    }
}
