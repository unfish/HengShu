//! Bounded automation declarations, stable-file deduplication and POST-only
//! webhook adapters. OS scheduler installation is intentionally a later shell.

pub mod scheduler;

use crate::execution::Sensitivity;
use crate::privacy::{Endpoint, EndpointPolicy};
use crate::security::PermissionSet;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationSpec {
    #[serde(alias = "时区")]
    pub timezone: String,
    #[serde(alias = "触发")]
    pub trigger: Trigger,
    #[serde(alias = "并发", default = "one")]
    pub concurrency: usize,
    #[serde(alias = "队列上限", default = "default_queue")]
    pub max_queue: usize,
    #[serde(alias = "超时毫秒", default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(alias = "重试次数", default)]
    pub retry_count: usize,
    #[serde(alias = "错过策略", default)]
    pub missed: MissedPolicy,
    #[serde(alias = "重叠策略", default)]
    pub overlap: OverlapPolicy,
    #[serde(alias = "通知", default)]
    pub notification: Option<WebhookNotification>,
    #[serde(alias = "SMTP", alias = "smtp", default)]
    pub smtp: Option<SmtpNotification>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebhookNotification {
    #[serde(alias = "地址", alias = "url")]
    pub address: String,
    #[serde(default, alias = "平台")]
    pub kind: WebhookKind,
    #[serde(default, alias = "密钥环境变量")]
    pub secret_environment: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SmtpNotification {
    pub host: String,
    pub port: u16,
    pub tls: crate::smtp::TlsMode,
    pub from: String,
    pub recipients: Vec<String>,
    #[serde(default)]
    pub username_environment: Option<String>,
    pub secret_environment: String,
}

fn one() -> usize {
    1
}

fn default_queue() -> usize {
    16
}

fn default_timeout_ms() -> u64 {
    600_000
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Trigger {
    #[serde(alias = "日历")]
    Calendar {
        #[serde(alias = "表达式")]
        expression: String,
    },
    #[serde(alias = "文件夹")]
    Folder {
        #[serde(alias = "根目录")]
        root: PathBuf,
        #[serde(alias = "扩展名", default)]
        extension: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum MissedPolicy {
    #[default]
    #[serde(alias = "跳过")]
    Skip,
    #[serde(alias = "补跑一次")]
    RunOnce,
    #[serde(alias = "全部补跑")]
    CatchUp,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum OverlapPolicy {
    #[default]
    #[serde(alias = "排队")]
    Queue,
    #[serde(alias = "跳过")]
    Skip,
    #[serde(alias = "替换")]
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CalendarRule {
    pub weekdays: BTreeSet<u32>,
    pub day_of_month: Option<u32>,
    pub hour: u32,
    pub minute: u32,
}

pub fn parse_calendar_expression(expression: &str) -> Result<CalendarRule, String> {
    let parts = expression.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 2 {
        return Err("日历表达式必须写成“每周一 08:00”或“每月5日 08:00”".into());
    }
    let time = parts[1].split_once(':').ok_or("日历时间必须是 HH:MM")?;
    let hour = time.0.parse::<u32>().map_err(|_| "日历小时无效")?;
    let minute = time.1.parse::<u32>().map_err(|_| "日历分钟无效")?;
    if hour > 23 || minute > 59 {
        return Err("日历时间超出范围".into());
    }
    let mut weekdays = BTreeSet::new();
    let mut day_of_month = None;
    if let Some(value) = parts[0].strip_prefix("每周") {
        let weekday = match value {
            "一" => 1,
            "二" => 2,
            "三" => 3,
            "四" => 4,
            "五" => 5,
            "六" => 6,
            "日" | "天" => 7,
            _ => return Err("星期必须是周一到周日".into()),
        };
        weekdays.insert(weekday);
    } else if let Some(value) = parts[0]
        .strip_prefix("每月")
        .and_then(|value| value.strip_suffix('日'))
    {
        let day = value.parse::<u32>().map_err(|_| "月份日期无效")?;
        if !(1..=31).contains(&day) {
            return Err("月份日期必须在 1 到 31 之间".into());
        }
        day_of_month = Some(day);
    } else {
        return Err("日历周期必须是每周或每月".into());
    }
    Ok(CalendarRule {
        weekdays,
        day_of_month,
        hour,
        minute,
    })
}

impl CalendarRule {
    fn matches_date(&self, date: chrono::NaiveDate) -> bool {
        use chrono::Datelike;
        match self.day_of_month {
            Some(day) => date.day() == day,
            None => self.weekdays.contains(&date.weekday().number_from_monday()),
        }
    }

    /// 把规则的本地时间落到时区上的真实时刻：夏令时前跳导致的不存在时间推到下一个存在的
    /// 时刻，回拨导致的重复时间取更早的那个，保证一个触发点只对应一个瞬时。
    fn resolve(
        &self,
        timezone: chrono_tz::Tz,
        date: chrono::NaiveDate,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        use chrono::TimeZone;
        let mut minutes = 0i64;
        while minutes <= 180 {
            let naive =
                date.and_hms_opt(self.hour, self.minute, 0)? + chrono::Duration::minutes(minutes);
            match timezone.from_local_datetime(&naive) {
                chrono::LocalResult::Single(local) => {
                    return Some(local.with_timezone(&chrono::Utc))
                }
                chrono::LocalResult::Ambiguous(earlier, _) => {
                    return Some(earlier.with_timezone(&chrono::Utc))
                }
                chrono::LocalResult::None => minutes += 15,
            }
        }
        None
    }

    pub fn next_occurrence(
        &self,
        timezone: chrono_tz::Tz,
        after: chrono::DateTime<chrono::Utc>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let mut date = after.with_timezone(&timezone).date_naive();
        for _ in 0..400 {
            if self.matches_date(date) {
                if let Some(instant) = self.resolve(timezone, date) {
                    if instant > after {
                        return Some(instant);
                    }
                }
            }
            date = date.succ_opt()?;
        }
        None
    }

    /// 列出 `(last_run, now]` 内应触发但没有运行的时刻，并按错过策略取舍。
    pub fn missed_occurrences(
        &self,
        timezone: chrono_tz::Tz,
        last_run: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        policy: MissedPolicy,
        max_queue: usize,
    ) -> Vec<chrono::DateTime<chrono::Utc>> {
        if matches!(policy, MissedPolicy::RunOnce) {
            let mut date = now.with_timezone(&timezone).date_naive();
            for _ in 0..400 {
                if self.matches_date(date) {
                    if let Some(instant) = self.resolve(timezone, date) {
                        if instant <= now && instant > last_run {
                            return vec![instant];
                        }
                    }
                }
                let Some(previous) = date.pred_opt() else {
                    break;
                };
                date = previous;
            }
            return Vec::new();
        }
        let mut missed = Vec::new();
        let mut cursor = last_run;
        while let Some(instant) = self.next_occurrence(timezone, cursor) {
            if instant > now || missed.len() >= max_queue {
                break;
            }
            missed.push(instant);
            cursor = instant;
        }
        match policy {
            MissedPolicy::Skip => Vec::new(),
            MissedPolicy::RunOnce => unreachable!("补跑一次已在有界反向搜索中处理"),
            MissedPolicy::CatchUp => missed,
        }
    }

    pub fn is_due(&self, timezone: chrono_tz::Tz, now: chrono::DateTime<chrono::Utc>) -> bool {
        let local_date = now.with_timezone(&timezone).date_naive();
        [local_date.pred_opt(), Some(local_date)]
            .into_iter()
            .flatten()
            .filter(|date| self.matches_date(*date))
            .filter_map(|date| self.resolve(timezone, date))
            .any(|instant| instant.timestamp() / 60 == now.timestamp() / 60)
    }
}

impl AutomationSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.timezone.parse::<chrono_tz::Tz>().is_err() {
            return Err(format!("IANA 时区无效：{}", self.timezone));
        }
        if self.concurrency == 0 || self.max_queue == 0 {
            return Err("自动化并发和队列上限必须大于 0".into());
        }
        if self.timeout_ms == 0 {
            return Err("自动化超时必须大于 0".into());
        }
        if matches!(self.missed, MissedPolicy::CatchUp) && self.max_queue < 2 {
            return Err("补跑策略要求队列至少容纳 2 项".into());
        }
        Ok(())
    }

    pub fn accepts_path(&self, path: &Path) -> bool {
        match &self.trigger {
            Trigger::Calendar { .. } => false,
            Trigger::Folder { root, extension } => {
                path.starts_with(root)
                    && extension.as_ref().is_none_or(|extension| {
                        path.extension()
                            .and_then(|value| value.to_str())
                            .is_some_and(|value| {
                                value.eq_ignore_ascii_case(extension.trim_start_matches('.'))
                            })
                    })
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileEvent {
    pub path: PathBuf,
    pub digest: String,
    pub trigger_version: String,
}

pub fn stable_file(
    path: &Path,
    wait: Duration,
    previous: Option<(u64, SystemTime)>,
) -> Result<Option<FileEvent>, String> {
    const MAX_MONITORED_FILE_BYTES: u64 = 64 * 1024 * 1024;
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("解析监视文件失败：{error}"))?;
    let mut file =
        std::fs::File::open(&canonical).map_err(|error| format!("打开监视文件失败：{error}"))?;
    let first = file
        .metadata()
        .map_err(|error| format!("读取监视文件失败：{error}"))?;
    let current = (
        first.len(),
        first.modified().map_err(|error| error.to_string())?,
    );
    if current.0 > MAX_MONITORED_FILE_BYTES {
        return Err("监视文件超过字节预算".into());
    }
    if previous.is_some_and(|previous| previous != current) {
        return Ok(None);
    }
    if wait > Duration::ZERO {
        std::thread::sleep(wait.min(Duration::from_secs(2)));
    }
    let second = file
        .metadata()
        .map_err(|error| format!("读取监视文件失败：{error}"))?;
    if second.len() != current.0
        || second.modified().map_err(|error| error.to_string())? != current.1
    {
        return Ok(None);
    }
    let current_path = canonical
        .canonicalize()
        .map_err(|error| format!("重新解析监视文件失败：{error}"))?;
    if current_path != canonical
        || !same_file_identity(
            &second,
            &std::fs::metadata(&canonical).map_err(|error| error.to_string())?,
        )
    {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(second.len()).unwrap_or(0));
    use std::io::Read;
    file.by_ref()
        .take(MAX_MONITORED_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取监视文件失败：{error}"))?;
    if bytes.len() as u64 > MAX_MONITORED_FILE_BYTES {
        return Err("监视文件超过字节预算".into());
    }
    let final_metadata = file
        .metadata()
        .map_err(|error| format!("读取监视文件失败：{error}"))?;
    if final_metadata.len() != second.len()
        || final_metadata
            .modified()
            .map_err(|error| error.to_string())?
            != second.modified().map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Some(FileEvent {
        path: canonical,
        digest,
        trigger_version: "1".into(),
    }))
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

#[derive(Clone, Debug)]
pub struct EventQueue {
    capacity: usize,
    seen: BTreeSet<String>,
    queue: VecDeque<(String, FileEvent)>,
}

impl EventQueue {
    pub fn new(capacity: usize) -> Result<Self, String> {
        if capacity == 0 {
            Err("监视队列上限必须大于 0".into())
        } else {
            Ok(Self {
                capacity,
                seen: BTreeSet::new(),
                queue: VecDeque::new(),
            })
        }
    }
    pub fn push(&mut self, event: FileEvent) -> Result<bool, String> {
        let key = format!(
            "{}:{}:{}",
            event.path.display(),
            event.digest,
            event.trigger_version
        );
        if self.seen.contains(&key) {
            return Ok(false);
        }
        if self.queue.len() >= self.capacity {
            return Err("监视队列超过预算".into());
        }
        self.seen.insert(key.clone());
        self.queue.push_back((key, event));
        Ok(true)
    }
    pub fn pop(&mut self) -> Option<FileEvent> {
        let (key, event) = self.queue.pop_front()?;
        self.seen.remove(&key);
        Some(event)
    }
}

#[derive(Clone, Debug)]
pub struct FolderWatchOptions {
    pub channel_capacity: usize,
    pub stable_wait: Duration,
    /// `Some` enables periodic reconciliation for network filesystems or lost OS events.
    pub poll_fallback: Option<Duration>,
}

impl Default for FolderWatchOptions {
    fn default() -> Self {
        Self {
            channel_capacity: 64,
            stable_wait: Duration::from_millis(250),
            poll_fallback: None,
        }
    }
}

enum FolderWatchMessage {
    Event(Event),
    Error(String),
}

/// Long-running folder event adapter. Platform events only wake the adapter;
/// path acceptance, stability and content deduplication happen before return.
pub struct FolderWatcher {
    spec: AutomationSpec,
    root: PathBuf,
    receiver: crossbeam_channel::Receiver<FolderWatchMessage>,
    _watcher: RecommendedWatcher,
    options: FolderWatchOptions,
    next_poll: Option<std::time::Instant>,
    pending: EventQueue,
    processed: BTreeMap<PathBuf, String>,
}

impl FolderWatcher {
    pub fn new(
        spec: &AutomationSpec,
        project_root: &Path,
        options: FolderWatchOptions,
    ) -> Result<Self, String> {
        spec.validate()?;
        if options.channel_capacity == 0 {
            return Err("监视事件通道上限必须大于 0".into());
        }
        if options.poll_fallback.is_some_and(|value| value.is_zero()) {
            return Err("监视轮询回退间隔必须大于 0".into());
        }
        let (declared_root, extension) = match &spec.trigger {
            Trigger::Folder { root, extension } => (root, extension),
            Trigger::Calendar { .. } => return Err("文件监视器需要文件夹触发器".into()),
        };
        let root = if declared_root.is_absolute() {
            declared_root.clone()
        } else {
            project_root.join(declared_root)
        };
        let root = root
            .canonicalize()
            .map_err(|error| format!("读取监视目录失败：{error}"))?;
        if !root.is_dir() {
            return Err("监视根路径不是目录".into());
        }
        let mut resolved_spec = spec.clone();
        resolved_spec.trigger = Trigger::Folder {
            root: root.clone(),
            extension: extension.clone(),
        };
        let (sender, receiver) = crossbeam_channel::bounded(options.channel_capacity);
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let message = match result {
                Ok(event) => FolderWatchMessage::Event(event),
                Err(error) => FolderWatchMessage::Error(error.to_string()),
            };
            let _ = sender.try_send(message);
        })
        .map_err(|error| format!("启动文件监视失败：{error}"))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| format!("监视目录失败：{error}"))?;
        let next_poll = options
            .poll_fallback
            .map(|interval| std::time::Instant::now() + interval);
        Ok(Self {
            spec: resolved_spec,
            root,
            receiver,
            _watcher: watcher,
            options,
            next_poll,
            pending: EventQueue::new(spec.max_queue)?,
            processed: BTreeMap::new(),
        })
    }

    pub fn next(
        &mut self,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Option<FileEvent>, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if let Some(event) = self.pending.pop() {
                if self.processed.get(&event.path) == Some(&event.digest) {
                    continue;
                }
                self.processed
                    .insert(event.path.clone(), event.digest.clone());
                if self.processed.len() > self.options.channel_capacity {
                    if let Some(oldest) = self.processed.keys().next().cloned() {
                        self.processed.remove(&oldest);
                    }
                }
                return Ok(Some(event));
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            if self.next_poll.is_some_and(|next| now >= next) {
                self.enqueue_scan()?;
                self.next_poll = self.options.poll_fallback.map(|interval| now + interval);
                continue;
            }
            let until_deadline = deadline.saturating_duration_since(now);
            let until_poll = self
                .next_poll
                .map(|next| next.saturating_duration_since(now))
                .unwrap_or(until_deadline);
            let wait = until_deadline
                .min(until_poll)
                .min(Duration::from_millis(100));
            match self.receiver.recv_timeout(wait) {
                Ok(FolderWatchMessage::Event(event)) => self.enqueue_event(&event)?,
                Ok(FolderWatchMessage::Error(error)) => {
                    if self.options.poll_fallback.is_none() {
                        return Err(format!("文件监视事件失败：{error}"));
                    }
                    self.next_poll = Some(std::time::Instant::now());
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    return Err("文件监视事件通道已关闭".into())
                }
            }
        }
    }

    fn enqueue_event(&mut self, event: &Event) -> Result<(), String> {
        for path in candidate_paths(event) {
            self.enqueue_path(&path)?;
        }
        Ok(())
    }

    fn enqueue_scan(&mut self) -> Result<(), String> {
        let mut directories = vec![self.root.clone()];
        while let Some(directory) = directories.pop() {
            let entries = std::fs::read_dir(&directory)
                .map_err(|error| format!("读取监视目录失败：{error}"))?;
            for entry in entries {
                let entry = entry.map_err(|error| format!("读取监视目录项失败：{error}"))?;
                let file_type = entry
                    .file_type()
                    .map_err(|error| format!("读取监视目录项类型失败：{error}"))?;
                let path = entry.path();
                if file_type.is_dir() && !file_type.is_symlink() {
                    directories.push(path);
                } else if file_type.is_file() {
                    self.enqueue_path(&path)?;
                }
            }
        }
        Ok(())
    }

    fn enqueue_path(&mut self, path: &Path) -> Result<(), String> {
        if !path.is_file() {
            return Ok(());
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("解析监视文件失败：{error}"))?;
        if !canonical.starts_with(&self.root) || !self.spec.accepts_path(&canonical) {
            return Ok(());
        }
        if let Some(event) = stable_file(&canonical, self.options.stable_wait, None)? {
            if event.path.starts_with(&self.root) && self.spec.accepts_path(&event.path) {
                let _ = self.pending.push(event)?;
            }
        }
        Ok(())
    }
}

fn candidate_paths(event: &Event) -> Vec<PathBuf> {
    if matches!(event.kind, EventKind::Access(_) | EventKind::Remove(_)) {
        Vec::new()
    } else {
        event.paths.clone()
    }
}

pub fn cancellation_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// One-shot reconciliation used by scheduled invocations before a long-lived
/// watcher is available. It uses the same canonical path and queue checks.
pub fn scan_folder_events(
    spec: &AutomationSpec,
    project_root: &Path,
    stable_wait: Duration,
) -> Result<Vec<FileEvent>, String> {
    let mut watcher = FolderWatcher::new(
        spec,
        project_root,
        FolderWatchOptions {
            stable_wait,
            poll_fallback: None,
            ..FolderWatchOptions::default()
        },
    )?;
    watcher.enqueue_scan()?;
    let mut events = Vec::new();
    while let Some(event) = watcher.pending.pop() {
        events.push(event);
    }
    events.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(events)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum WebhookKind {
    WeCom,
    DingTalk,
    #[default]
    Feishu,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebhookMessage {
    pub title: String,
    pub text: String,
    pub link: Option<String>,
}

pub fn webhook_payload(kind: WebhookKind, message: &WebhookMessage) -> serde_json::Value {
    let link = message
        .link
        .as_ref()
        .map(|link| format!("\n{link}"))
        .unwrap_or_default();
    match kind {
        WebhookKind::WeCom => {
            serde_json::json!({ "msgtype": "markdown", "markdown": { "content": format!("{}\n{}{link}", message.title, message.text) } })
        }
        WebhookKind::DingTalk => {
            serde_json::json!({ "msgtype": "markdown", "markdown": { "title": message.title, "text": format!("{}{link}", message.text) } })
        }
        WebhookKind::Feishu => {
            serde_json::json!({ "msg_type": "text", "content": { "text": format!("{}\n{}{link}", message.title, message.text) } })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebhookRequest {
    pub method: String,
    pub url: String,
    pub body: serde_json::Value,
}

fn webhook_signature(secret: &str, timestamp_ms: i64) -> String {
    use base64::Engine;
    use hmac::Mac;
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 接受任意长度密钥");
    mac.update(format!("{timestamp_ms}\n{secret}").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// 秘密只用于签名；返回值中的 URL 不得直接写入日志或诊断。
pub fn webhook_request(
    kind: WebhookKind,
    url: &str,
    message: &WebhookMessage,
    secret: Option<&str>,
    timestamp_ms: i64,
) -> Result<WebhookRequest, String> {
    let mut parsed = url::Url::parse(url).map_err(|error| format!("Webhook URL 无效：{error}"))?;
    let loopback = matches!(
        parsed.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1")
    );
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err("Webhook 默认只允许 HTTPS，仅本机回环夹具可用 HTTP".into());
    }
    let mut body = webhook_payload(kind, message);
    if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
        let signature = webhook_signature(secret, timestamp_ms);
        match kind {
            WebhookKind::WeCom => return Err("企业微信群机器人不支持签名 secret".into()),
            WebhookKind::DingTalk => {
                parsed
                    .query_pairs_mut()
                    .append_pair("timestamp", &timestamp_ms.to_string())
                    .append_pair("sign", &signature);
            }
            WebhookKind::Feishu => {
                body["timestamp"] = serde_json::json!(timestamp_ms.to_string());
                body["sign"] = serde_json::json!(signature);
            }
        }
    }
    Ok(WebhookRequest {
        method: "POST".into(),
        url: parsed.to_string(),
        body,
    })
}

fn redacted_endpoint(url: &str) -> String {
    url::Url::parse(url)
        .map(|mut parsed| {
            parsed.set_query(None);
            parsed.to_string()
        })
        .unwrap_or_else(|_| "Webhook 端点".into())
}

pub fn send_webhook(
    kind: WebhookKind,
    url: &str,
    message: &WebhookMessage,
    secret: Option<&str>,
    permissions: &PermissionSet,
    policy: &EndpointPolicy,
    sensitivity: &Sensitivity,
) -> Result<(), String> {
    let request = webhook_request(
        kind,
        url,
        message,
        secret,
        chrono::Utc::now().timestamp_millis(),
    )?;
    let parsed = url::Url::parse(&request.url).map_err(|error| error.to_string())?;
    permissions.check_network(parsed.host_str().unwrap_or_default())?;
    policy.allow(Endpoint::Webhook, sensitivity)?;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .redirects(0)
        .build();
    let endpoint = redacted_endpoint(&request.url);
    match agent.post(&request.url).send_json(request.body) {
        Ok(response) if (300..400).contains(&response.status()) => Err(format!(
            "Webhook 默认禁止重定向：{endpoint} 返回 {}",
            response.status()
        )),
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, _)) if (300..400).contains(&code) => {
            Err(format!("Webhook 默认禁止重定向：{endpoint} 返回 {code}"))
        }
        Err(ureq::Error::Status(code, _)) => {
            Err(format!("Webhook POST 失败：{endpoint} 返回 {code}"))
        }
        Err(ureq::Error::Transport(transport)) => Err(format!(
            "Webhook POST 传输失败：{endpoint} {}",
            transport.kind()
        )),
    }
}

mod history;
#[cfg(test)]
use history::sqlite_sidecar;
pub use history::{RunHistory, RunRecord, RunStatus, TriggerReservation};
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn automation_validation_and_queue_dedup_are_bounded() {
        let spec = AutomationSpec {
            timezone: "Asia/Shanghai".into(),
            trigger: Trigger::Calendar {
                expression: "每周一 08:00".into(),
            },
            concurrency: 1,
            max_queue: 2,
            timeout_ms: 1000,
            retry_count: 1,
            missed: MissedPolicy::CatchUp,
            overlap: OverlapPolicy::Queue,
            notification: None,
            smtp: None,
        };
        spec.validate().unwrap();
        assert_eq!(parse_calendar_expression("每周一 08:00").unwrap().hour, 8);
        assert!(parse_calendar_expression("每月32日 08:00").is_err());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.csv");
        fs::write(&path, b"a").unwrap();
        let event = stable_file(&path, Duration::ZERO, None).unwrap().unwrap();
        let mut queue = EventQueue::new(1).unwrap();
        assert!(queue.push(event.clone()).unwrap());
        assert!(!queue.push(event).unwrap());
        let folder = AutomationSpec {
            timezone: "Asia/Shanghai".into(),
            trigger: Trigger::Folder {
                root: directory.path().into(),
                extension: Some("csv".into()),
            },
            concurrency: 1,
            max_queue: 1,
            timeout_ms: 1000,
            retry_count: 0,
            missed: MissedPolicy::Skip,
            overlap: OverlapPolicy::Skip,
            notification: None,
            smtp: None,
        };
        folder.validate().unwrap();
        assert!(folder.accepts_path(&path));
    }

    #[test]
    fn notify_events_are_only_candidates_until_the_file_is_accepted() {
        use notify::event::{AccessKind, CreateKind, RemoveKind};

        let path = PathBuf::from("inbox/report.csv");
        let created = Event::new(EventKind::Create(CreateKind::File)).add_path(path.clone());
        assert_eq!(candidate_paths(&created), vec![path.clone()]);
        let accessed = Event::new(EventKind::Access(AccessKind::Any)).add_path(path.clone());
        assert!(candidate_paths(&accessed).is_empty());
        let removed = Event::new(EventKind::Remove(RemoveKind::File)).add_path(path);
        assert!(candidate_paths(&removed).is_empty());
    }

    #[test]
    fn folder_watcher_receives_native_events_and_filters_extensions() {
        let directory = tempfile::tempdir().unwrap();
        let spec = AutomationSpec {
            timezone: "Asia/Shanghai".into(),
            trigger: Trigger::Folder {
                root: directory.path().into(),
                extension: Some("csv".into()),
            },
            concurrency: 1,
            max_queue: 4,
            timeout_ms: 1000,
            retry_count: 0,
            missed: MissedPolicy::Skip,
            overlap: OverlapPolicy::Queue,
            notification: None,
            smtp: None,
        };
        let mut watcher = FolderWatcher::new(
            &spec,
            directory.path(),
            FolderWatchOptions {
                stable_wait: Duration::ZERO,
                poll_fallback: Some(Duration::from_millis(20)),
                ..FolderWatchOptions::default()
            },
        )
        .unwrap();
        std::fs::write(directory.path().join("ignored.txt"), b"ignored").unwrap();
        let expected = directory.path().join("accepted.csv");
        std::fs::write(&expected, b"accepted").unwrap();
        let cancelled = AtomicBool::new(false);
        let event = watcher
            .next(Duration::from_secs(2), &cancelled)
            .unwrap()
            .expect("native event or explicit poll fallback should find CSV");
        assert_eq!(event.path, expected.canonicalize().unwrap());
        assert!(watcher
            .next(Duration::from_millis(100), &cancelled)
            .unwrap()
            .is_none());
        cancelled.store(true, Ordering::Relaxed);
        assert!(watcher
            .next(Duration::from_secs(1), &cancelled)
            .unwrap()
            .is_none());
    }

    fn loopback_fixture(response: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                let mut length = 0_usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    let end = line.trim().is_empty();
                    request.push_str(&line);
                    if end {
                        break;
                    }
                }
                let mut body = vec![0_u8; length];
                reader.read_exact(&mut body).unwrap();
                request.push_str(&String::from_utf8_lossy(&body));
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().ok();
                sender.send(request).unwrap();
            }
        });
        (format!("http://127.0.0.1:{port}/robot"), receiver)
    }

    fn loopback_permissions() -> PermissionSet {
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.network_hosts = vec!["127.0.0.1".into()];
        permissions
    }

    fn webhook_policy() -> EndpointPolicy {
        EndpointPolicy {
            allowed: BTreeSet::from([Endpoint::Webhook]),
            sensitive_allowed: BTreeSet::new(),
        }
    }

    #[test]
    fn webhook_adapters_only_send_post_to_local_fixture() {
        let message = WebhookMessage {
            title: "失败".into(),
            text: "摘要".into(),
            link: Some("https://example.com/报告".into()),
        };
        for (kind, marker) in [
            (WebhookKind::WeCom, "\"msgtype\":\"markdown\""),
            (WebhookKind::DingTalk, "\"title\":\"失败\""),
            (WebhookKind::Feishu, "\"msg_type\":\"text\""),
        ] {
            let (url, requests) =
                loopback_fixture("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            send_webhook(
                kind,
                &url,
                &message,
                None,
                &loopback_permissions(),
                &webhook_policy(),
                &Sensitivity::default(),
            )
            .unwrap();
            let request = requests
                .recv_timeout(Duration::from_secs(5))
                .expect("本地夹具应收到请求");
            assert!(request.starts_with("POST /robot HTTP/1.1"), "{request}");
            assert_eq!(request.matches(" HTTP/1.1\r\n").count(), 1, "{request}");
            assert!(request.contains(marker), "{request}");
            assert!(request.contains("example.com/报告"), "{request}");
        }
    }

    #[test]
    fn webhook_signature_only_applies_where_the_platform_supports_it() {
        let message = WebhookMessage {
            title: "失败".into(),
            text: "摘要".into(),
            link: None,
        };
        let dingtalk = webhook_request(
            WebhookKind::DingTalk,
            "https://oapi.dingtalk.com/robot/send?access_token=令牌",
            &message,
            Some("秘密"),
            1_700_000_000_000,
        )
        .unwrap();
        assert_eq!(dingtalk.method, "POST");
        assert!(dingtalk.url.contains("timestamp=1700000000000"));
        assert!(dingtalk.url.contains("sign="));
        assert!(dingtalk.body.get("timestamp").is_none());

        let feishu = webhook_request(
            WebhookKind::Feishu,
            "https://open.feishu.cn/open-apis/bot/v2/hook/令牌",
            &message,
            Some("秘密"),
            1_700_000_000_000,
        )
        .unwrap();
        assert_eq!(feishu.body["timestamp"], "1700000000000");
        assert_eq!(
            feishu.body["sign"],
            webhook_signature("秘密", 1_700_000_000_000)
        );
        assert!(!feishu.url.contains("sign="));

        assert!(webhook_request(
            WebhookKind::WeCom,
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=令牌",
            &message,
            Some("秘密"),
            1_700_000_000_000,
        )
        .is_err());
        assert!(webhook_request(
            WebhookKind::Feishu,
            "http://example.com/hook",
            &message,
            None,
            0
        )
        .is_err());
    }

    #[test]
    fn webhook_refuses_redirects_without_leaking_the_signature() {
        let message = WebhookMessage {
            title: "失败".into(),
            text: "摘要".into(),
            link: None,
        };
        let (url, _requests) = loopback_fixture(
            "HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\nContent-Length: 0\r\n\r\n",
        );
        let error = send_webhook(
            WebhookKind::DingTalk,
            &url,
            &message,
            Some("秘密"),
            &loopback_permissions(),
            &webhook_policy(),
            &Sensitivity::default(),
        )
        .unwrap_err();
        assert!(error.contains("禁止重定向"), "{error}");
        assert!(!error.contains("sign="), "{error}");
        assert!(!error.contains("秘密"), "{error}");
    }

    #[test]
    fn webhook_requires_network_permission_for_the_target_host() {
        let message = WebhookMessage {
            title: "失败".into(),
            text: "摘要".into(),
            link: None,
        };
        assert!(send_webhook(
            WebhookKind::Feishu,
            "https://open.feishu.cn/hook",
            &message,
            None,
            &PermissionSet::for_source(Path::new(".")),
            &webhook_policy(),
            &Sensitivity::default()
        )
        .is_err());
        let (url, _requests) = loopback_fixture("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let sensitive = Sensitivity {
            labels: BTreeSet::from([crate::execution::SensitiveLabel::Financial]),
            ..Default::default()
        };
        assert!(send_webhook(
            WebhookKind::Feishu,
            &url,
            &message,
            None,
            &loopback_permissions(),
            &webhook_policy(),
            &sensitive
        )
        .is_err());
        assert!(send_webhook(
            WebhookKind::Feishu,
            &url,
            &message,
            None,
            &loopback_permissions(),
            &EndpointPolicy::default(),
            &Sensitivity::default()
        )
        .is_err());
    }

    #[test]
    fn run_history_records_prunes_queries_and_exports() {
        let directory = tempfile::tempdir().unwrap();
        let history = RunHistory::open(&directory.path().join("runs.db"), 2).unwrap();
        for index in 0..3 {
            history
                .record(&RunRecord {
                    run_id: index.to_string(),
                    tool: "工具".into(),
                    status: RunStatus::Succeeded,
                    started_ms: index,
                    finished_ms: Some(index + 1),
                    trigger_key: None,
                    input_summary: "in".into(),
                    output_root: "out".into(),
                    output: Vec::new(),
                    diagnostics: Vec::new(),
                    artifacts: Vec::new(),
                    resources: crate::execution::ResourceSummary::default(),
                    notification_status: Vec::new(),
                })
                .unwrap();
        }
        assert_eq!(history.list(10).unwrap().len(), 2);
        assert!(history.export_json(10).unwrap().contains("工具"));
    }

    #[test]
    fn run_history_recovery_is_recoverable_for_corrupt_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runs.db");
        std::fs::write(&path, b"not sqlite").unwrap();
        let wal = sqlite_sidecar(&path, "-wal");
        let shm = sqlite_sidecar(&path, "-shm");
        std::fs::write(&wal, b"stale wal").unwrap();
        std::fs::write(&shm, b"stale shm").unwrap();
        let history = RunHistory::open_recover(&path, 2).unwrap();
        assert!(history.list(2).unwrap().is_empty());
        let backups = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("损坏-"))
            .collect::<Vec<_>>();
        assert!(!backups.is_empty(), "{backups:?}");
        assert_ne!(std::fs::read(&wal).unwrap_or_default(), b"stale wal");
        assert_ne!(std::fs::read(&shm).unwrap_or_default(), b"stale shm");
    }
}
