use super::error;
use crate::security::PermissionSet;
use crate::value::Value;
use std::collections::BTreeMap;
#[cfg(test)]
use std::path::Path;
use std::time::Duration;

const DEFAULT_PAGE_TIMEOUT_MS: u64 = 30_000;

mod cdp;
mod chrome;
mod extract;
mod pagination;

#[cfg(test)]
use cdp::authorize_browser_url;
#[cfg(test)]
use chrome::find_chrome;
#[cfg(test)]
use extract::contains_private_use;
#[cfg(test)]
use extract::parse_field_rules;
use extract::{extract_records, select_values};
use pagination::{browser_paginate, BrowserRequest};
#[cfg(test)]
use pagination::{extraction_script, next_click_script, random_delay};

pub(super) fn webpage_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    permissions: &PermissionSet,
    execution_budget: Option<Duration>,
) -> Result<Value, String> {
    match name {
        "取出文本" => select_values(&args[0], &args[1], None, false, permissions),
        "取出HTML" => select_values(&args[0], &args[1], None, true, permissions),
        "取出属性" => select_values(
            &args[0],
            &args[1],
            Some(args[2].to_string()),
            false,
            permissions,
        ),
        "按字段采集" => extract_records(
            &args[0].to_string(),
            &args[1].to_string(),
            &args[2],
            permissions.collection_limit(),
        ),
        "逐页采集" => {
            let url = args[0].to_string();
            let container_selector = args[1].to_string();
            let next_selector = args[3].to_string();
            browser_paginate(BrowserRequest {
                url: &url,
                container_selector: &container_selector,
                fields: &args[2],
                next_selector: &next_selector,
                named: &named,
                permissions,
                execution_budget,
            })
        }
        _ => Err(error(format!("网页模块不支持“{name}”"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    fn fields() -> Value {
        Value::record(BTreeMap::from([
            ("名称".into(), Value::Text(".name".into())),
            (
                "编码".into(),
                Value::record(BTreeMap::from([
                    ("选择器".into(), Value::Text("a".into())),
                    ("属性".into(), Value::Text("data-code".into())),
                ])),
            ),
        ]))
    }

    #[test]
    fn css_selectors_extract_chinese_text_attributes_and_html() {
        let permissions = PermissionSet::for_source(Path::new("."));
        let html = Value::Text(
            "<main><p class='item' data-code='甲&amp;1'>中文 &amp; 数据<b>!</b></p></main>".into(),
        );
        let text = select_values(
            &html,
            &Value::Text(".item".into()),
            None,
            false,
            &permissions,
        )
        .unwrap();
        assert_eq!(text.iter_values().unwrap()[0].to_string(), "中文 & 数据!");
        let attribute = select_values(
            &html,
            &Value::Text(".item".into()),
            Some("data-code".into()),
            false,
            &permissions,
        )
        .unwrap();
        assert_eq!(attribute.iter_values().unwrap()[0].to_string(), "甲&1");
        let html = select_values(
            &html,
            &Value::Text(".item".into()),
            None,
            true,
            &permissions,
        )
        .unwrap();
        assert_eq!(
            html.iter_values().unwrap()[0].to_string(),
            "中文 &amp; 数据<b>!</b>"
        );
    }

    #[test]
    fn record_extraction_handles_missing_fields_and_invalid_configuration() {
        let html = "<article><a data-code='A1'></a><span class='name'>商品一</span></article><article><span class='name'>商品二</span></article>";
        let records = extract_records(html, "article", &fields(), 10).unwrap();
        let records = records.iter_values().unwrap();
        let Value::Record(first) = &records[0] else {
            panic!("需要记录")
        };
        assert_eq!(first.borrow()["名称"].to_string(), "商品一");
        assert_eq!(first.borrow()["编码"].to_string(), "A1");
        let Value::Record(second) = &records[1] else {
            panic!("需要记录")
        };
        assert_eq!(second.borrow()["编码"].to_string(), "");
        assert!(extract_records(html, "[", &fields(), 10)
            .unwrap_err()
            .contains("CSS 选择器"));
        assert!(extract_records(html, "article", &Value::list(vec![]), 10)
            .unwrap_err()
            .contains("字段配置必须是记录"));
    }

    #[test]
    fn record_extraction_obeys_collection_limit() {
        let error = extract_records(
            "<article></article><article></article>",
            "article",
            &fields(),
            1,
        )
        .unwrap_err();
        assert!(error.contains("集合项数限制"));
    }

    #[test]
    fn protected_private_use_text_can_be_rejected_explicitly() {
        let fields = Value::record(BTreeMap::from([(
            "价格".into(),
            Value::record(BTreeMap::from([
                ("选择器".into(), Value::Text(".price".into())),
                ("拒绝私用字符".into(), Value::Bool(true)),
            ])),
        )]));
        let records = extract_records(
            "<article><span class='price'>￥\u{e232}12</span></article>",
            "article",
            &fields,
            10,
        )
        .unwrap()
        .iter_values()
        .unwrap();
        let Value::Record(record) = &records[0] else {
            panic!("需要记录")
        };
        assert_eq!(record.borrow()["价格"].to_string(), "");
        assert!(contains_private_use("\u{f0001}"));
        assert!(!contains_private_use("￥123.45"));
    }

    #[test]
    fn generated_browser_scripts_are_data_driven_and_check_disabled_next_page() {
        let rules = parse_field_rules(&fields()).unwrap();
        let extraction = extraction_script("article", &rules).unwrap();
        assert!(extraction.contains("querySelectorAll"));
        assert!(extraction.contains("data-code"));
        let click = next_click_script(".next").unwrap();
        assert!(click.contains("ant-pagination-disabled"));
        assert!(click.contains("target.click()"));
    }

    #[test]
    #[ignore = "需要本机安装 Chrome 或 Chromium"]
    fn chrome_paginates_a_local_dynamic_fixture() {
        if find_chrome().is_none() {
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stopped = stopped.clone();
        let server = thread::spawn(move || {
            let html = r#"<!doctype html><meta charset="utf-8">
<div id="items"><article><span class="name">商品一</span><a data-code="A1"></a></article></div>
<button id="next" onclick="document.getElementById('items').innerHTML='<article><span class=&quot;name&quot;>商品二</span><a data-code=&quot;A2&quot;></a></article>';this.disabled=true">下一页</button>"#;
            while !server_stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 2048];
                        let _ = stream.read(&mut request);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            html.len(),
                            html
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(value) if value.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        let mut permissions = PermissionSet::restricted_for_source(Path::new("."));
        permissions.unrestricted = true;
        let url = format!("http://{address}/products");
        let fields = fields();
        let named = BTreeMap::from([
            ("最多采集页数".into(), Value::Number(3.into())),
            (
                "翻页后至少等待".into(),
                Value::Duration(chrono::Duration::zero()),
            ),
            (
                "翻页后最多等待".into(),
                Value::Duration(chrono::Duration::zero()),
            ),
            (
                "页面最长等待".into(),
                Value::Duration(chrono::Duration::seconds(10)),
            ),
            ("内容加载完成标志".into(), Value::Text(".name".into())),
            ("隐藏浏览器窗口".into(), Value::Bool(true)),
        ]);
        let result = browser_paginate(BrowserRequest {
            url: &url,
            container_selector: "article",
            fields: &fields,
            next_selector: "#next",
            named: &named,
            permissions: &permissions,
            execution_budget: Some(Duration::from_secs(30)),
        })
        .unwrap();
        stopped.store(true, Ordering::Relaxed);
        server.join().unwrap();
        let records = result.iter_values().unwrap();
        assert_eq!(records.len(), 2);
        let Value::Record(second) = &records[1] else {
            panic!("需要记录")
        };
        assert_eq!(second.borrow()["名称"].to_string(), "商品二");
        assert_eq!(second.borrow()["编码"].to_string(), "A2");
    }

    #[test]
    fn browser_subresources_follow_network_permissions() {
        let mut permissions = PermissionSet::restricted_for_source(Path::new("."));
        permissions.network_hosts = vec!["example.com".into()];
        assert!(authorize_browser_url(&permissions, "https://example.com/app.js").is_ok());
        assert!(authorize_browser_url(&permissions, "data:text/plain,ok").is_ok());
        let denied =
            authorize_browser_url(&permissions, "https://tracker.invalid/pixel").unwrap_err();
        assert!(denied.contains("tracker.invalid"));
    }

    #[test]
    fn random_page_delays_stay_inside_the_configured_bounds() {
        for _ in 0..256 {
            assert!(
                (Duration::from_millis(1_200)..=Duration::from_millis(3_200)).contains(
                    &random_delay(Duration::from_millis(1_200), Duration::from_millis(3_200))
                )
            );
        }
        assert_eq!(
            random_delay(Duration::from_millis(500), Duration::from_millis(500)),
            Duration::from_millis(500)
        );
    }
}
