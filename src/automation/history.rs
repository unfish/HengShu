//! Versioned SQLite ownership for automation reservations, checkpoints, and run records.

use super::{CalendarRule, FileEvent, MissedPolicy};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Partial,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::TimedOut => "TimedOut",
            Self::Partial => "Partial",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "Running" => Ok(Self::Running),
            "Succeeded" => Ok(Self::Succeeded),
            "Failed" => Ok(Self::Failed),
            "Cancelled" => Ok(Self::Cancelled),
            "TimedOut" => Ok(Self::TimedOut),
            "Partial" => Ok(Self::Partial),
            _ => Err(format!("运行历史包含未知状态：{value}")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub tool: String,
    pub status: RunStatus,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub trigger_key: Option<String>,
    pub input_summary: String,
    pub output_root: String,
    pub output: Vec<String>,
    pub diagnostics: Vec<crate::api::DiagnosticReport>,
    pub artifacts: Vec<crate::execution::Artifact>,
    pub resources: crate::execution::ResourceSummary,
    pub notification_status: Vec<String>,
}

impl RunRecord {
    pub fn from_outcome(
        run_id: impl Into<String>,
        tool: impl Into<String>,
        started_ms: i64,
        trigger_key: Option<String>,
        input_summary: impl Into<String>,
        output_root: impl Into<String>,
        mut outcome: crate::execution::RunOutcome,
    ) -> Self {
        let status = match outcome.termination {
            crate::execution::RunTermination::Cancelled => RunStatus::Cancelled,
            crate::execution::RunTermination::TimedOut => RunStatus::TimedOut,
            crate::execution::RunTermination::Completed if outcome.success => RunStatus::Succeeded,
            crate::execution::RunTermination::Completed
            | crate::execution::RunTermination::Failed => RunStatus::Failed,
        };
        for artifact in &mut outcome.artifacts {
            artifact.path = PathBuf::from(crate::privacy::scrub_secrets(
                &artifact.path.to_string_lossy(),
            ));
            artifact.display_name = crate::privacy::scrub_secrets(&artifact.display_name);
            artifact.purpose = crate::privacy::scrub_secrets(&artifact.purpose);
        }
        Self {
            run_id: run_id.into(),
            tool: tool.into(),
            status,
            started_ms,
            finished_ms: Some(chrono::Utc::now().timestamp_millis()),
            trigger_key,
            input_summary: crate::privacy::scrub_secrets(&input_summary.into()),
            output_root: crate::privacy::scrub_secrets(&output_root.into()),
            output: outcome
                .output
                .into_iter()
                .map(|line| crate::privacy::scrub_secrets(&line))
                .collect(),
            diagnostics: outcome
                .diagnostics
                .into_iter()
                .map(scrub_diagnostic)
                .collect(),
            artifacts: outcome.artifacts,
            resources: outcome.resources,
            notification_status: outcome
                .notification_status
                .into_iter()
                .map(|status| crate::privacy::scrub_secrets(&status))
                .collect(),
        }
    }
}

fn scrub_diagnostic(mut diagnostic: crate::api::DiagnosticReport) -> crate::api::DiagnosticReport {
    diagnostic.message = crate::privacy::scrub_secrets(&diagnostic.message);
    diagnostic.stack = diagnostic
        .stack
        .into_iter()
        .map(|frame| crate::privacy::scrub_secrets(&frame))
        .collect();
    diagnostic.cause = diagnostic
        .cause
        .map(|cause| Box::new(scrub_diagnostic(*cause)));
    diagnostic
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerReservation {
    pub key: String,
    pub scheduled_ms: i64,
    pub input_summary: String,
}

pub struct RunHistory {
    connection: Connection,
    max_records: usize,
}

impl RunHistory {
    const SCHEMA_VERSION: u32 = 1;
    const APPLICATION_ID: u32 = 0x4845_4e47;

    pub fn open(path: &Path, max_records: usize) -> Result<Self, String> {
        if max_records == 0 {
            return Err("运行历史上限必须大于 0".into());
        }
        let connection =
            Connection::open(path).map_err(|error| format!("打开运行历史失败：{error}"))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| format!("设置运行历史锁等待失败：{error}"))?;
        let history = Self {
            connection,
            max_records,
        };
        history.initialize()?;
        Ok(history)
    }

    pub fn open_recover(path: &Path, max_records: usize) -> Result<Self, String> {
        match Self::open(path, max_records) {
            Ok(history) => Ok(history),
            Err(error)
                if !error.contains("schema 版本不支持")
                    && !error.contains("未版本化")
                    && !error.contains("应用标识") =>
            {
                quarantine_corrupt_database(path, &error)?;
                Self::open(path, max_records)
            }
            Err(error) => Err(error),
        }
    }

    fn initialize(&self) -> Result<(), String> {
        let version = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .map_err(|error| format!("读取运行历史 schema 版本失败：{error}"))?;
        if version == 0 {
            let tables = self
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                    [],
                    |row| row.get::<_, usize>(0),
                )
                .map_err(|error| format!("检查运行历史 schema 失败：{error}"))?;
            if tables != 0 {
                return Err("运行历史数据库未版本化，不支持自动迁移".into());
            }
            self.connection.execute_batch(&format!(
                "PRAGMA application_id={};
                 PRAGMA user_version={};
                 PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE runs (
                   run_id TEXT PRIMARY KEY,
                   tool TEXT NOT NULL,
                   trigger_key TEXT,
                   status TEXT NOT NULL,
                   started_ms INTEGER NOT NULL,
                   finished_ms INTEGER,
                   input_summary TEXT NOT NULL,
                   output_root TEXT NOT NULL,
                   output TEXT NOT NULL,
                   diagnostics TEXT NOT NULL,
                   artifacts TEXT NOT NULL,
                   resources TEXT NOT NULL,
                   notification_status TEXT NOT NULL
                 );
                 CREATE INDEX runs_started ON runs(started_ms DESC);
                 CREATE TABLE trigger_reservations (
                   trigger_key TEXT PRIMARY KEY,
                   tool TEXT NOT NULL,
                   trigger_kind TEXT NOT NULL,
                   scheduled_ms INTEGER NOT NULL,
                   path TEXT,
                   digest TEXT,
                   trigger_version TEXT NOT NULL,
                   status TEXT NOT NULL,
                   attempts INTEGER NOT NULL,
                   reserved_ms INTEGER NOT NULL,
                   completed_ms INTEGER
                 );
                 CREATE INDEX trigger_cleanup ON trigger_reservations(status, completed_ms, reserved_ms);
                 CREATE TABLE checkpoints (
                   tool TEXT NOT NULL,
                   trigger_kind TEXT NOT NULL,
                   checked_ms INTEGER NOT NULL,
                   PRIMARY KEY(tool, trigger_kind)
                 );",
                Self::APPLICATION_ID,
                Self::SCHEMA_VERSION
            )).map_err(|error| format!("初始化运行历史失败：{error}"))?;
            return Ok(());
        }
        if version != Self::SCHEMA_VERSION {
            return Err(format!(
                "运行历史 schema 版本不支持：{version}，当前只支持 {}",
                Self::SCHEMA_VERSION
            ));
        }
        let application_id = self
            .connection
            .query_row("PRAGMA application_id", [], |row| row.get::<_, u32>(0))
            .map_err(|error| format!("读取运行历史应用标识失败：{error}"))?;
        if application_id != Self::APPLICATION_ID {
            return Err("运行历史数据库应用标识不匹配".into());
        }
        self.connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
            )
            .map_err(|error| format!("配置运行历史失败：{error}"))
    }

    pub fn record(&self, record: &RunRecord) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("开始运行历史事务失败：{error}"))?;
        Self::write_record(&transaction, record)?;
        Self::prune_records(&transaction, self.max_records)?;
        transaction
            .commit()
            .map_err(|error| format!("提交运行历史失败：{error}"))
    }

    pub fn list(&self, limit: usize) -> Result<Vec<RunRecord>, String> {
        let mut statement = self.connection.prepare("SELECT run_id,tool,status,started_ms,finished_ms,trigger_key,input_summary,output_root,output,diagnostics,artifacts,resources,notification_status FROM runs ORDER BY started_ms DESC LIMIT ?1").map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([limit as i64], |row| {
                let status: String = row.get(2)?;
                let output: String = row.get(8)?;
                let diagnostics: String = row.get(9)?;
                let artifacts: String = row.get(10)?;
                let resources: String = row.get(11)?;
                let notifications: String = row.get(12)?;
                Ok(RunRecord {
                    run_id: row.get(0)?,
                    tool: row.get(1)?,
                    status: RunStatus::parse(&status).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            error.into(),
                        )
                    })?,
                    started_ms: row.get(3)?,
                    finished_ms: row.get(4)?,
                    trigger_key: row.get(5)?,
                    input_summary: row.get(6)?,
                    output_root: row.get(7)?,
                    output: serde_json::from_str(&output).unwrap_or_default(),
                    diagnostics: serde_json::from_str(&diagnostics).unwrap_or_default(),
                    artifacts: serde_json::from_str(&artifacts).unwrap_or_default(),
                    resources: serde_json::from_str(&resources).unwrap_or_default(),
                    notification_status: serde_json::from_str(&notifications).unwrap_or_default(),
                })
            })
            .map_err(|error| error.to_string())?;
        rows.map(|row| row.map_err(|error| error.to_string()))
            .collect()
    }

    pub fn prune(&self, keep: usize) -> Result<(), String> {
        Self::prune_records(&self.connection, keep)
    }

    pub fn export_json(&self, limit: usize) -> Result<String, String> {
        serde_json::to_string_pretty(&self.list(limit)?).map_err(|error| error.to_string())
    }

    pub fn reserve_file(
        &self,
        tool: &str,
        event: &FileEvent,
        now_ms: i64,
        lease_ms: u64,
        retry_count: usize,
    ) -> Result<Option<TriggerReservation>, String> {
        let path = event
            .path
            .canonicalize()
            .map_err(|error| format!("解析触发文件失败：{error}"))?;
        let path_text = path.to_string_lossy().to_string();
        let key = trigger_key(&[
            tool,
            "folder",
            &path_text,
            &event.digest,
            &event.trigger_version,
        ]);
        let transaction = self.begin_immediate()?;
        let reservation = Self::claim_trigger(
            &transaction,
            &key,
            tool,
            "folder",
            now_ms,
            Some(&path_text),
            Some(&event.digest),
            &event.trigger_version,
            now_ms,
            lease_ms,
            retry_count,
        )?;
        Self::prune_triggers(&transaction, self.max_records, now_ms)?;
        transaction
            .commit()
            .map_err(|error| format!("提交文件触发预约失败：{error}"))?;
        Ok(reservation.map(|mut reservation| {
            reservation.input_summary = format!("{} SHA-256 {}", path.display(), event.digest);
            reservation
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reserve_calendar(
        &self,
        tool: &str,
        rule: &CalendarRule,
        timezone: chrono_tz::Tz,
        now: chrono::DateTime<chrono::Utc>,
        policy: MissedPolicy,
        max_queue: usize,
        lease_ms: u64,
        retry_count: usize,
    ) -> Result<Vec<TriggerReservation>, String> {
        let now_ms = now.timestamp_millis();
        let transaction = self.begin_immediate()?;
        let previous_ms = transaction
            .query_row(
                "SELECT checked_ms FROM checkpoints WHERE tool=?1 AND trigger_kind='calendar'",
                [tool],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("读取自动化检查点失败：{error}"))?
            .unwrap_or_else(|| now_ms.saturating_sub(60_000));
        let previous =
            chrono::DateTime::from_timestamp_millis(previous_ms).ok_or("自动化检查点时间无效")?;
        let retryable = {
            let mut statement = transaction
                .prepare("SELECT trigger_key,scheduled_ms,trigger_version FROM trigger_reservations WHERE tool=?1 AND trigger_kind='calendar' AND status IN ('Failed','Running') AND scheduled_ms<=?2 ORDER BY scheduled_ms")
                .map_err(|error| format!("读取待重试日历触发失败：{error}"))?;
            let rows = statement
                .query_map(params![tool, now_ms], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|error| format!("读取待重试日历触发失败：{error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("读取待重试日历触发失败：{error}"))?
        };
        let mut reservations = Vec::new();
        for (key, scheduled_ms, trigger_version) in retryable {
            if reservations.len() >= max_queue {
                break;
            }
            if let Some(mut reservation) = Self::claim_trigger(
                &transaction,
                &key,
                tool,
                "calendar",
                scheduled_ms,
                None,
                None,
                &trigger_version,
                now_ms,
                lease_ms,
                retry_count,
            )? {
                reservation.input_summary = format!(
                    "日历触发 {}",
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(scheduled_ms)
                        .ok_or("日历预约时间无效")?
                );
                reservations.push(reservation);
            }
        }
        let available = max_queue.saturating_sub(reservations.len());
        let occurrences = if available == 0 {
            Vec::new()
        } else {
            match policy {
                MissedPolicy::Skip => rule
                    .missed_occurrences(timezone, previous, now, MissedPolicy::CatchUp, available)
                    .into_iter()
                    .filter(|instant| instant.timestamp() / 60 == now.timestamp() / 60)
                    .collect::<Vec<_>>(),
                MissedPolicy::RunOnce => rule.missed_occurrences(
                    timezone,
                    previous,
                    now,
                    MissedPolicy::RunOnce,
                    available,
                ),
                MissedPolicy::CatchUp => rule.missed_occurrences(
                    timezone,
                    previous,
                    now,
                    MissedPolicy::CatchUp,
                    available,
                ),
            }
        };
        let checkpoint_ms = if available == 0 {
            previous_ms
        } else if matches!(policy, MissedPolicy::CatchUp)
            && occurrences.len() == available
            && rule
                .next_occurrence(timezone, *occurrences.last().unwrap_or(&previous))
                .is_some_and(|next| next <= now)
        {
            occurrences
                .last()
                .map_or(now_ms, chrono::DateTime::timestamp_millis)
        } else {
            now_ms
        };
        transaction
            .execute(
                "INSERT INTO checkpoints(tool,trigger_kind,checked_ms) VALUES(?1,'calendar',?2) ON CONFLICT(tool,trigger_kind) DO UPDATE SET checked_ms=excluded.checked_ms",
                params![tool, checkpoint_ms],
            )
            .map_err(|error| format!("写入自动化检查点失败：{error}"))?;
        for occurrence in occurrences {
            let scheduled_ms = occurrence.timestamp_millis();
            let scheduled = scheduled_ms.to_string();
            let key = trigger_key(&[tool, "calendar", &scheduled, "1"]);
            if let Some(mut reservation) = Self::claim_trigger(
                &transaction,
                &key,
                tool,
                "calendar",
                scheduled_ms,
                None,
                None,
                "1",
                now_ms,
                lease_ms,
                retry_count,
            )? {
                reservation.input_summary = format!("日历触发 {occurrence}");
                reservations.push(reservation);
            }
        }
        Self::prune_triggers(&transaction, self.max_records, now_ms)?;
        transaction
            .commit()
            .map_err(|error| format!("提交日历触发预约失败：{error}"))?;
        Ok(reservations)
    }

    pub fn reserve_immediate(
        &self,
        tool: &str,
        now_ms: i64,
        lease_ms: u64,
    ) -> Result<TriggerReservation, String> {
        let key = trigger_key(&[tool, "manual", &now_ms.to_string()]);
        let transaction = self.begin_immediate()?;
        let reservation = Self::claim_trigger(
            &transaction,
            &key,
            tool,
            "manual",
            now_ms,
            None,
            None,
            "1",
            now_ms,
            lease_ms,
            0,
        )?
        .ok_or("立即触发预约冲突")?;
        transaction
            .commit()
            .map_err(|error| format!("提交立即触发预约失败：{error}"))?;
        Ok(TriggerReservation {
            input_summary: "手动立即触发".into(),
            ..reservation
        })
    }

    pub fn commit_trigger(
        &self,
        reservation: &TriggerReservation,
        record: &RunRecord,
    ) -> Result<(), String> {
        if record.trigger_key.as_deref() != Some(&reservation.key) {
            return Err("运行记录与触发预约不匹配".into());
        }
        let transaction = self.begin_immediate()?;
        Self::write_record(&transaction, record)?;
        let succeeded = matches!(record.status, RunStatus::Succeeded);
        let changed = transaction
            .execute(
                "UPDATE trigger_reservations SET status=?2,completed_ms=?3 WHERE trigger_key=?1 AND status='Running'",
                params![
                    reservation.key,
                    if succeeded { "Completed" } else { "Failed" },
                    if succeeded { record.finished_ms } else { None }
                ],
            )
            .map_err(|error| format!("提交触发状态失败：{error}"))?;
        if changed != 1 {
            return Err("触发预约已经被其他进程提交或不再有效".into());
        }
        Self::prune_records(&transaction, self.max_records)?;
        transaction
            .commit()
            .map_err(|error| format!("提交自动化运行失败：{error}"))
    }

    fn begin_immediate(&self) -> Result<rusqlite::Transaction<'_>, String> {
        rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)
            .map_err(|error| format!("获取自动化状态锁失败：{error}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn claim_trigger(
        connection: &Connection,
        key: &str,
        tool: &str,
        kind: &str,
        scheduled_ms: i64,
        path: Option<&str>,
        digest: Option<&str>,
        trigger_version: &str,
        now_ms: i64,
        lease_ms: u64,
        retry_count: usize,
    ) -> Result<Option<TriggerReservation>, String> {
        let current = connection
            .query_row(
                "SELECT status,attempts,reserved_ms FROM trigger_reservations WHERE trigger_key=?1",
                [key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, usize>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("读取触发预约失败：{error}"))?;
        if let Some((status, attempts, reserved_ms)) = current {
            let lease_until =
                reserved_ms.saturating_add(i64::try_from(lease_ms).unwrap_or(i64::MAX));
            if status == "Completed"
                || (status == "Running" && lease_until > now_ms)
                || attempts >= retry_count.saturating_add(1)
            {
                return Ok(None);
            }
            connection
                .execute(
                    "UPDATE trigger_reservations SET status='Running',attempts=attempts+1,reserved_ms=?2,completed_ms=NULL WHERE trigger_key=?1",
                    params![key, now_ms],
                )
                .map_err(|error| format!("重试触发预约失败：{error}"))?;
        } else {
            connection
                .execute(
                    "INSERT INTO trigger_reservations(trigger_key,tool,trigger_kind,scheduled_ms,path,digest,trigger_version,status,attempts,reserved_ms,completed_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,'Running',1,?8,NULL)",
                    params![key, tool, kind, scheduled_ms, path, digest, trigger_version, now_ms],
                )
                .map_err(|error| format!("创建触发预约失败：{error}"))?;
        }
        Ok(Some(TriggerReservation {
            key: key.into(),
            scheduled_ms,
            input_summary: String::new(),
        }))
    }

    fn write_record(connection: &Connection, record: &RunRecord) -> Result<(), String> {
        let output = serde_json::to_string(&record.output).map_err(|error| error.to_string())?;
        let diagnostics =
            serde_json::to_string(&record.diagnostics).map_err(|error| error.to_string())?;
        let artifacts =
            serde_json::to_string(&record.artifacts).map_err(|error| error.to_string())?;
        let resources =
            serde_json::to_string(&record.resources).map_err(|error| error.to_string())?;
        let notifications = serde_json::to_string(&record.notification_status)
            .map_err(|error| error.to_string())?;
        connection.execute("INSERT INTO runs(run_id,tool,trigger_key,status,started_ms,finished_ms,input_summary,output_root,output,diagnostics,artifacts,resources,notification_status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) ON CONFLICT(run_id) DO UPDATE SET tool=excluded.tool,trigger_key=excluded.trigger_key,status=excluded.status,finished_ms=excluded.finished_ms,input_summary=excluded.input_summary,output_root=excluded.output_root,output=excluded.output,diagnostics=excluded.diagnostics,artifacts=excluded.artifacts,resources=excluded.resources,notification_status=excluded.notification_status", params![record.run_id,record.tool,record.trigger_key,record.status.as_str(),record.started_ms,record.finished_ms,record.input_summary,record.output_root,output,diagnostics,artifacts,resources,notifications]).map_err(|error| format!("写入运行历史失败：{error}"))?;
        Ok(())
    }

    fn prune_records(connection: &Connection, keep: usize) -> Result<(), String> {
        connection.execute("DELETE FROM runs WHERE run_id NOT IN (SELECT run_id FROM runs ORDER BY started_ms DESC LIMIT ?1)", [keep as i64]).map_err(|error| format!("清理运行历史失败：{error}"))?;
        Ok(())
    }

    fn prune_triggers(
        connection: &Connection,
        max_records: usize,
        now_ms: i64,
    ) -> Result<(), String> {
        const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
        connection
            .execute(
                "DELETE FROM trigger_reservations WHERE status!='Running' AND COALESCE(completed_ms,reserved_ms) < ?1",
                [now_ms.saturating_sub(RETENTION_MS)],
            )
            .map_err(|error| format!("清理过期触发状态失败：{error}"))?;
        let keep = max_records.saturating_mul(4).max(64);
        connection.execute("DELETE FROM trigger_reservations WHERE status!='Running' AND trigger_key NOT IN (SELECT trigger_key FROM trigger_reservations ORDER BY COALESCE(completed_ms,reserved_ms) DESC LIMIT ?1)", [keep as i64]).map_err(|error| format!("清理触发状态容量失败：{error}"))?;
        Ok(())
    }
}

fn trigger_key(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn quarantine_corrupt_database(path: &Path, original_error: &str) -> Result<(), String> {
    let timestamp = chrono::Utc::now().timestamp_millis();
    for (source, suffix) in [
        (path.to_path_buf(), ""),
        (sqlite_sidecar(path, "-wal"), "-wal"),
        (sqlite_sidecar(path, "-shm"), "-shm"),
    ] {
        if !source.exists() {
            continue;
        }
        let backup = path.with_extension(format!("损坏-{timestamp}{suffix}"));
        std::fs::rename(&source, &backup)
            .map_err(|rename| format!("运行历史损坏且无法隔离：{original_error}; {rename}"))?;
    }
    Ok(())
}

pub(super) fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
