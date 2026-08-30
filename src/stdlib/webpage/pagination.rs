use super::cdp::Cdp;
use super::chrome::{create_target, find_chrome, ChromeProcess};
use super::extract::{compile_field_rules, parse_field_rules, parse_selector, FieldRule};
use super::{error, DEFAULT_PAGE_TIMEOUT_MS};
use crate::contracts::{decimal_to_usize, timeout_duration};
use crate::security::PermissionSet;
use crate::stdlib::codec::from_json;
use crate::value::Value;
use rand::Rng;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

pub(super) struct BrowserRequest<'a> {
    pub(super) url: &'a str,
    pub(super) container_selector: &'a str,
    pub(super) fields: &'a Value,
    pub(super) next_selector: &'a str,
    pub(super) named: &'a BTreeMap<String, Value>,
    pub(super) permissions: &'a PermissionSet,
    pub(super) execution_budget: Option<Duration>,
}

pub(super) fn browser_paginate(request: BrowserRequest<'_>) -> Result<Value, String> {
    let BrowserRequest {
        url,
        container_selector,
        fields,
        next_selector,
        named,
        permissions,
        execution_budget,
    } = request;
    permissions.check_network(url)?;
    permissions.check_process()?;
    parse_selector(container_selector)?;
    parse_selector(next_selector)?;
    let rules = parse_field_rules(fields)?;
    compile_field_rules(&rules)?;

    let maximum_pages = named_usize(named, "最多采集页数", 100)?;
    if maximum_pages == 0 {
        return Err(error("最多采集页数至少为 1"));
    }
    let delay_lower = named_duration(named, "翻页后至少等待", Duration::from_secs(1))?;
    let delay_upper = named_duration(named, "翻页后最多等待", Duration::from_secs(3))?;
    if delay_lower > delay_upper {
        return Err(error("翻页后至少等待不能长于翻页后最多等待"));
    }
    let page_timeout = named_duration(
        named,
        "页面最长等待",
        Duration::from_millis(DEFAULT_PAGE_TIMEOUT_MS),
    )?;
    if page_timeout.is_zero() {
        return Err(error("页面最长等待必须大于 0"));
    }
    let ready_selector = named.get("内容加载完成标志").map(ToString::to_string);
    if let Some(selector) = &ready_selector {
        parse_selector(selector)?;
    }
    let browser_path = named
        .get("浏览器位置")
        .map(|value| PathBuf::from(value.to_string()))
        .or_else(find_chrome)
        .ok_or_else(|| error("没有找到 Chrome 或 Chromium，请用“浏览器位置”指定"))?;
    let headless = named.get("隐藏浏览器窗口").is_some_and(Value::truth);
    let deadline = execution_budget.and_then(|budget| Instant::now().checked_add(budget));

    let mut chrome = ChromeProcess::launch(&browser_path, headless, page_timeout)?;
    let target = create_target(chrome.port, "about:blank", page_timeout)?;
    let mut cdp = Cdp::connect(&target, page_timeout, permissions.clone())?;
    cdp.command(
        "Fetch.enable",
        json!({"patterns": [{"urlPattern": "*", "requestStage": "Request"}]}),
    )?;
    cdp.command("Page.enable", json!({}))?;
    cdp.command("Runtime.enable", json!({}))?;
    cdp.command("Page.navigate", json!({"url": url}))?;
    wait_for_selector(&mut cdp, container_selector, page_timeout, deadline)?;
    if let Some(selector) = &ready_selector {
        wait_for_cards_ready(
            &mut cdp,
            container_selector,
            selector,
            page_timeout,
            deadline,
        )?;
    }

    let extract_script = extraction_script(container_selector, &rules)?;
    let click_script = next_click_script(next_selector)?;
    let mut output = Vec::new();
    let mut previous_page = None;
    for page_index in 0..maximum_pages {
        check_deadline(deadline)?;
        let page = cdp.evaluate(&extract_script)?;
        let rows = page
            .as_array()
            .ok_or_else(|| error("浏览器字段提取没有返回记录列表"))?;
        if rows.is_empty() {
            if page_index == 0 {
                return Err(error("页面中没有匹配的抓取记录"));
            }
            break;
        }
        let fingerprint = serde_json::to_string(rows)
            .map_err(|value| error(format!("计算页面指纹失败：{value}")))?;
        if previous_page.as_ref() == Some(&fingerprint) {
            break;
        }
        if output.len().saturating_add(rows.len()) > permissions.collection_limit() {
            return Err(error("浏览器抓取结果超过集合项数限制"));
        }
        output.extend(rows.iter().map(from_json));
        previous_page = Some(fingerprint.clone());

        if page_index + 1 >= maximum_pages
            || !cdp.evaluate(&click_script)?.as_bool().unwrap_or(false)
        {
            break;
        }
        let delay = random_delay(delay_lower, delay_upper);
        sleep_bounded(delay, deadline)?;
        wait_for_page_change(
            &mut cdp,
            &extract_script,
            &fingerprint,
            page_timeout,
            deadline,
        )?;
        if let Some(selector) = &ready_selector {
            wait_for_cards_ready(
                &mut cdp,
                container_selector,
                selector,
                page_timeout,
                deadline,
            )?;
        }
    }
    chrome.stop();
    Ok(Value::list(output))
}

fn named_usize(
    named: &BTreeMap<String, Value>,
    name: &str,
    default: usize,
) -> Result<usize, String> {
    named
        .get(name)
        .map(Value::as_decimal)
        .transpose()?
        .map(|value| decimal_to_usize(value, name))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn named_duration(
    named: &BTreeMap<String, Value>,
    name: &str,
    default: Duration,
) -> Result<Duration, String> {
    named
        .get(name)
        .map_or(Ok(default), |value| timeout_duration(value, name))
}

pub(super) fn random_delay(lower: Duration, upper: Duration) -> Duration {
    let lower = u64::try_from(lower.as_millis()).unwrap_or(u64::MAX);
    let upper = u64::try_from(upper.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rand::thread_rng().gen_range(lower..=upper))
}

pub(super) fn extraction_script(
    container_selector: &str,
    rules: &BTreeMap<String, FieldRule>,
) -> Result<String, String> {
    let container =
        serde_json::to_string(container_selector).map_err(|value| error(value.to_string()))?;
    let rules = serde_json::to_string(rules).map_err(|value| error(value.to_string()))?;
    Ok(format!(
        r#"(() => {{
const containers = document.querySelectorAll({container});
const fields = {rules};
return Array.from(containers).map(root => {{
  const row = {{}};
  for (const [name, rule] of Object.entries(fields)) {{
    const node = rule.selector ? root.querySelector(rule.selector) : root;
    if (!node) {{ row[name] = ""; continue; }}
    if (rule.attribute !== null) row[name] = node.getAttribute(rule.attribute) || "";
    else if (rule.html) row[name] = node.innerHTML;
    else row[name] = (node.textContent || "").trim();
    if (rule.reject_private_use) {{
      const protectedText = Array.from(row[name]).some(character => {{
        const code = character.codePointAt(0);
        return (code >= 0xE000 && code <= 0xF8FF) ||
          (code >= 0xF0000 && code <= 0xFFFFD) ||
          (code >= 0x100000 && code <= 0x10FFFD);
      }});
      if (protectedText) row[name] = "";
    }}
  }}
  return row;
}});
}})()"#
    ))
}

pub(super) fn next_click_script(selector: &str) -> Result<String, String> {
    let selector = serde_json::to_string(selector).map_err(|value| error(value.to_string()))?;
    Ok(format!(
        r#"(() => {{
const holder = document.querySelector({selector});
if (!holder) return false;
const disabled = holder.matches(':disabled,[aria-disabled="true"],.ant-pagination-disabled') ||
  holder.closest('.ant-pagination-disabled,[aria-disabled="true"]');
if (disabled) return false;
const target = holder.querySelector('button,a') || holder;
if (target.disabled || target.getAttribute('aria-disabled') === 'true') return false;
target.click();
return true;
}})()"#
    ))
}

fn wait_for_selector(
    cdp: &mut Cdp,
    selector: &str,
    timeout: Duration,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let selector = serde_json::to_string(selector).map_err(|value| error(value.to_string()))?;
    let condition = format!(
        "document.readyState === 'complete' && document.querySelector({selector}) !== null"
    );
    wait_until(cdp, timeout, deadline, |cdp| {
        Ok(cdp.evaluate(&condition)?.as_bool().unwrap_or(false))
    })
    .map_err(|failure| wait_error(failure, "等待页面商品区域超时"))
}

fn wait_for_page_change(
    cdp: &mut Cdp,
    extract_script: &str,
    previous: &str,
    timeout: Duration,
    deadline: Option<Instant>,
) -> Result<(), String> {
    wait_until(cdp, timeout, deadline, |cdp| {
        let current = cdp.evaluate(extract_script)?;
        let current = serde_json::to_string(&current)
            .map_err(|value| error(format!("计算页面指纹失败：{value}")))?;
        Ok(current != previous && current != "[]")
    })
    .map_err(|failure| wait_error(failure, "点击下一页后页面内容没有在超时时间内更新"))
}

fn wait_for_cards_ready(
    cdp: &mut Cdp,
    container_selector: &str,
    ready_selector: &str,
    timeout: Duration,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let containers =
        serde_json::to_string(container_selector).map_err(|value| error(value.to_string()))?;
    let ready = serde_json::to_string(ready_selector).map_err(|value| error(value.to_string()))?;
    let condition = format!(
        "(() => {{ const rows = Array.from(document.querySelectorAll({containers})); return rows.length > 0 && rows.every(row => row.querySelector({ready}) !== null); }})()"
    );
    wait_until(cdp, timeout, deadline, |cdp| {
        Ok(cdp.evaluate(&condition)?.as_bool().unwrap_or(false))
    })
    .map_err(|failure| wait_error(failure, "等待每个网页记录的动态内容就绪超时"))
}

enum WaitFailure {
    Timeout,
    Runtime(String),
}

fn wait_error(failure: WaitFailure, timeout_message: &str) -> String {
    match failure {
        WaitFailure::Timeout => error(timeout_message),
        WaitFailure::Runtime(value) => value,
    }
}

fn wait_until(
    cdp: &mut Cdp,
    timeout: Duration,
    deadline: Option<Instant>,
    mut condition: impl FnMut(&mut Cdp) -> Result<bool, String>,
) -> Result<(), WaitFailure> {
    let local_deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(WaitFailure::Timeout)?;
    loop {
        check_deadline(deadline).map_err(WaitFailure::Runtime)?;
        if condition(cdp).map_err(WaitFailure::Runtime)? {
            return Ok(());
        }
        if Instant::now() >= local_deadline {
            return Err(WaitFailure::Timeout);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn sleep_bounded(duration: Duration, deadline: Option<Instant>) -> Result<(), String> {
    if let Some(deadline) = deadline {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| error("浏览器抓取超过程序执行时间限制"))?;
        if duration > remaining {
            return Err(error("浏览器抓取超过程序执行时间限制"));
        }
    }
    thread::sleep(duration);
    Ok(())
}

fn check_deadline(deadline: Option<Instant>) -> Result<(), String> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(error("浏览器抓取超过程序执行时间限制"))
    } else {
        Ok(())
    }
}
