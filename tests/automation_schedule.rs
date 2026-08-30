//! 自动化测试：夏令时、漏跑补偿、稳定文件、重复事件、重叠排队、断电恢复、通知重试
//! 和敏感值不落盘。SMTP 与 Webhook 使用本机回环夹具，不访问外部网络。

use hengshu::automation::{
    parse_calendar_expression, stable_file, EventQueue, FileEvent, MissedPolicy, RunHistory,
    RunRecord, RunStatus,
};
use hengshu::execution::{Artifact, RunOutcome, RunTermination};
use std::time::Duration;

fn utc(text: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

#[test]
fn spring_forward_and_fall_back_produce_one_instant_per_trigger() {
    let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
    let rule = parse_calendar_expression("每月29日 02:30").unwrap();
    // 2026-03-29 02:30 在柏林不存在，必须推到夏令时生效后的第一个存在时刻。
    let forward = rule
        .next_occurrence(berlin, utc("2026-03-01T00:00:00Z"))
        .unwrap();
    assert_eq!(forward, utc("2026-03-29T01:00:00Z"));

    let fall_back = parse_calendar_expression("每月25日 02:30").unwrap();
    // 2026-10-25 02:30 在柏林出现两次，取更早的一次（夏令时未结束，UTC+2）。
    let repeated = fall_back
        .next_occurrence(berlin, utc("2026-10-01T00:00:00Z"))
        .unwrap();
    assert_eq!(repeated, utc("2026-10-25T00:30:00Z"));

    // 固定偏移时区不受夏令时影响。
    let shanghai: chrono_tz::Tz = "Asia/Shanghai".parse().unwrap();
    let daily = parse_calendar_expression("每月29日 02:30").unwrap();
    assert_eq!(
        daily
            .next_occurrence(shanghai, utc("2026-03-01T00:00:00Z"))
            .unwrap(),
        utc("2026-03-28T18:30:00Z")
    );
}

#[test]
fn missed_runs_follow_the_declared_policy_and_queue_bound() {
    let shanghai: chrono_tz::Tz = "Asia/Shanghai".parse().unwrap();
    let rule = parse_calendar_expression("每周一 08:00").unwrap();
    let last_run = utc("2026-01-01T00:00:00Z");
    let now = utc("2026-02-01T00:00:00Z");

    assert!(rule
        .missed_occurrences(shanghai, last_run, now, MissedPolicy::Skip, 16)
        .is_empty());

    let once = rule.missed_occurrences(shanghai, last_run, now, MissedPolicy::RunOnce, 16);
    assert_eq!(once.len(), 1);

    let all = rule.missed_occurrences(shanghai, last_run, now, MissedPolicy::CatchUp, 16);
    assert_eq!(all.len(), 4, "{all:?}");
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(all.last(), once.last());

    let bounded = rule.missed_occurrences(shanghai, last_run, now, MissedPolicy::CatchUp, 2);
    assert_eq!(bounded.len(), 2);
}

#[test]
fn unfinished_files_are_skipped_and_repeated_events_deduplicate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("台账.csv");
    std::fs::write(&path, b"a").unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let growing = (metadata.len() + 1, metadata.modified().unwrap());
    assert!(stable_file(&path, Duration::ZERO, Some(growing))
        .unwrap()
        .is_none());

    let event = stable_file(&path, Duration::ZERO, None).unwrap().unwrap();
    let mut queue = EventQueue::new(4).unwrap();
    assert!(queue.push(event.clone()).unwrap());
    assert!(!queue.push(event.clone()).unwrap());
    let other = FileEvent {
        path: event.path.clone(),
        digest: "0".repeat(64),
        trigger_version: event.trigger_version.clone(),
    };
    assert!(queue.push(other).unwrap());
    assert_eq!(queue.pop().unwrap().digest, event.digest);
}

#[test]
fn overlapping_events_queue_up_to_the_bound_and_then_fail() {
    let mut queue = EventQueue::new(1).unwrap();
    let event = |digest: &str| FileEvent {
        path: std::path::PathBuf::from("a.csv"),
        digest: digest.into(),
        trigger_version: "1".into(),
    };
    assert!(queue.push(event("1")).unwrap());
    assert!(queue.push(event("2")).is_err());
}

#[test]
fn history_survives_restart_and_never_stores_secret_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("历史.db");
    let record = |run_id: &str| RunRecord {
        run_id: run_id.into(),
        tool: "月报".into(),
        status: RunStatus::Succeeded,
        started_ms: 1,
        finished_ms: Some(2),
        trigger_key: None,
        input_summary: "台账.csv".into(),
        output_root: "结果".into(),
        output: Vec::new(),
        diagnostics: Vec::new(),
        artifacts: Vec::new(),
        resources: hengshu::execution::ResourceSummary::default(),
        notification_status: Vec::new(),
    };
    {
        let history = RunHistory::open(&path, 16).unwrap();
        history.record(&record("r1")).unwrap();
    }
    // 断电模拟：库文件尾部损坏后仍能恢复出可用历史。
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"corrupt-tail");
    std::fs::write(&path, bytes).unwrap();
    let recovered = RunHistory::open_recover(&path, 16).unwrap();
    recovered.record(&record("r2")).unwrap();
    let listed = recovered.list(16).unwrap();
    assert!(listed.iter().any(|entry| entry.run_id == "r2"));

    let exported = recovered.export_json(16).unwrap();
    assert!(!exported.contains("hunter2"), "{exported}");
}

#[test]
fn file_trigger_reservations_survive_restarts_and_retry_only_after_failure() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("状态.db");
    let input = directory.path().join("台账.csv");
    std::fs::write(&input, b"id,amount\n1,10\n").unwrap();
    let event = stable_file(&input, Duration::ZERO, None).unwrap().unwrap();
    let now = 1_000_000;

    let history = RunHistory::open(&database, 16).unwrap();
    let reservation = history
        .reserve_file("日报", &event, now, 60_000, 1)
        .unwrap()
        .unwrap();
    drop(history);

    let restarted = RunHistory::open(&database, 16).unwrap();
    assert!(restarted
        .reserve_file("日报", &event, now + 1, 60_000, 1)
        .unwrap()
        .is_none());
    let failed = RunOutcome {
        success: false,
        termination: RunTermination::Failed,
        output: vec!["失败前输出".into()],
        ..RunOutcome::default()
    };
    restarted
        .commit_trigger(
            &reservation,
            &RunRecord::from_outcome(
                "run-1",
                "日报",
                now,
                Some(reservation.key.clone()),
                &reservation.input_summary,
                "结果",
                failed,
            ),
        )
        .unwrap();

    let retry = restarted
        .reserve_file("日报", &event, now + 2, 60_000, 1)
        .unwrap()
        .unwrap();
    let succeeded = RunOutcome {
        artifacts: vec![Artifact::from_bytes("报告.pdf", "pdf", b"pdf")],
        output: vec!["完成".into()],
        ..RunOutcome::default()
    };
    restarted
        .commit_trigger(
            &retry,
            &RunRecord::from_outcome(
                "run-2",
                "日报",
                now + 2,
                Some(retry.key.clone()),
                &retry.input_summary,
                "结果",
                succeeded,
            ),
        )
        .unwrap();
    drop(restarted);

    let completed = RunHistory::open(&database, 16).unwrap();
    assert!(completed
        .reserve_file("日报", &event, now + 120_000, 60_000, 1)
        .unwrap()
        .is_none());
    let records = completed.list(16).unwrap();
    assert_eq!(records[0].status, RunStatus::Succeeded);
    assert_eq!(records[0].output, ["完成"]);
    assert_eq!(records[0].artifacts[0].display_name, "报告.pdf");
    assert_eq!(records[1].output, ["失败前输出"]);
}

#[test]
fn concurrent_calendar_wakeups_reserve_each_occurrence_once() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("状态.db");
    RunHistory::open(&database, 16).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let now = utc("2026-01-05T00:00:00Z");
    let handles = (0..2)
        .map(|_| {
            let database = database.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let history = RunHistory::open(&database, 16).unwrap();
                let rule = parse_calendar_expression("每周一 08:00").unwrap();
                barrier.wait();
                history
                    .reserve_calendar(
                        "周报",
                        &rule,
                        "Asia/Shanghai".parse().unwrap(),
                        now,
                        MissedPolicy::Skip,
                        16,
                        60_000,
                        0,
                    )
                    .unwrap()
                    .len()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum::<usize>(),
        1
    );
}

#[test]
fn run_outcome_adapter_preserves_termination_and_structured_details() {
    for (termination, expected) in [
        (RunTermination::Cancelled, RunStatus::Cancelled),
        (RunTermination::TimedOut, RunStatus::TimedOut),
        (RunTermination::Failed, RunStatus::Failed),
    ] {
        let outcome = RunOutcome {
            success: false,
            termination,
            output: vec!["此前输出".into()],
            resources: hengshu::execution::ResourceSummary {
                elapsed_ms: 7,
                output_bytes: 12,
                ..Default::default()
            },
            ..RunOutcome::default()
        };
        let record = RunRecord::from_outcome("run", "工具", 1, None, "输入", "结果", outcome);
        assert_eq!(record.status, expected);
        assert_eq!(record.output, ["此前输出"]);
        assert_eq!(record.resources.elapsed_ms, 7);
    }
}

#[test]
fn run_outcome_adapter_scrubs_every_persisted_text_surface() {
    let secret = "history-secret-value";
    hengshu::privacy::register_secret(secret);
    let mut artifact = Artifact::from_bytes(format!("结果/{secret}.txt"), "text", b"value");
    artifact.display_name = format!("{secret}.txt");
    artifact.purpose = format!("发送 {secret}");
    let outcome = RunOutcome {
        success: false,
        termination: RunTermination::Failed,
        output: vec![format!("此前输出 {secret}")],
        diagnostics: vec![hengshu::api::DiagnosticReport {
            category: Some("运行错误".into()),
            span: None,
            message: format!("失败 {secret}"),
            stack: vec![format!("调用 {secret}")],
            definition_span: None,
            cause: Some(Box::new(hengshu::api::DiagnosticReport {
                category: Some("原因".into()),
                span: None,
                message: format!("原因 {secret}"),
                stack: Vec::new(),
                definition_span: None,
                cause: None,
            })),
        }],
        artifacts: vec![artifact],
        notification_status: vec![format!("通知 {secret}")],
        ..RunOutcome::default()
    };
    let record = RunRecord::from_outcome(
        "run",
        "工具",
        1,
        None,
        format!("输入 {secret}"),
        format!("输出/{secret}"),
        outcome,
    );
    let encoded = serde_json::to_string(&record).unwrap();
    assert!(!encoded.contains(secret), "{encoded}");
    assert!(encoded.contains("<秘密已隐藏>"), "{encoded}");
}

#[test]
fn persistent_checkpoints_apply_missed_policies_and_drain_bounded_catch_up() {
    let directory = tempfile::tempdir().unwrap();
    let history = RunHistory::open(&directory.path().join("日历.db"), 64).unwrap();
    let rule = parse_calendar_expression("每周一 08:00").unwrap();
    let timezone = "Asia/Shanghai".parse().unwrap();
    let initial = utc("2026-01-01T00:00:00Z");
    let later = utc("2026-02-01T00:00:00Z");
    for (tool, policy, expected) in [
        ("跳过", MissedPolicy::Skip, 0),
        ("一次", MissedPolicy::RunOnce, 1),
        ("全部", MissedPolicy::CatchUp, 4),
    ] {
        assert!(history
            .reserve_calendar(tool, &rule, timezone, initial, policy, 16, 60_000, 0)
            .unwrap()
            .is_empty());
        let reservations = history
            .reserve_calendar(tool, &rule, timezone, later, policy, 16, 60_000, 0)
            .unwrap();
        assert_eq!(reservations.len(), expected, "{tool}");
        if matches!(policy, MissedPolicy::RunOnce) {
            assert_eq!(
                reservations[0].scheduled_ms,
                utc("2026-01-26T00:00:00Z").timestamp_millis()
            );
        }
    }

    history
        .reserve_calendar(
            "分批",
            &rule,
            timezone,
            initial,
            MissedPolicy::CatchUp,
            2,
            60_000,
            0,
        )
        .unwrap();
    assert_eq!(
        history
            .reserve_calendar(
                "分批",
                &rule,
                timezone,
                later,
                MissedPolicy::CatchUp,
                2,
                60_000,
                0,
            )
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        history
            .reserve_calendar(
                "分批",
                &rule,
                timezone,
                later,
                MissedPolicy::CatchUp,
                2,
                60_000,
                0,
            )
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn event_storm_and_expired_leases_remain_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("风暴.csv");
    std::fs::write(&path, b"same").unwrap();
    let event = stable_file(&path, Duration::ZERO, None).unwrap().unwrap();
    let history = RunHistory::open(&directory.path().join("风暴.db"), 4).unwrap();
    assert!(history
        .reserve_file("导入", &event, 1_000, 10, 1)
        .unwrap()
        .is_some());
    for _ in 0..100 {
        assert!(history
            .reserve_file("导入", &event, 1_001, 10, 1)
            .unwrap()
            .is_none());
    }
    assert!(history
        .reserve_file("导入", &event, 1_011, 10, 1)
        .unwrap()
        .is_some());
    assert!(history
        .reserve_file("导入", &event, 1_022, 10, 1)
        .unwrap()
        .is_none());
}

#[cfg(unix)]
#[test]
fn folder_scan_rejects_symlinks_that_escape_the_declared_root() {
    use hengshu::automation::{scan_folder_events, AutomationSpec, OverlapPolicy, Trigger};
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("外部.csv");
    std::fs::write(&outside_file, b"secret").unwrap();
    symlink(&outside_file, root.path().join("链接.csv")).unwrap();
    let spec = AutomationSpec {
        timezone: "Asia/Shanghai".into(),
        trigger: Trigger::Folder {
            root: root.path().to_path_buf(),
            extension: Some("csv".into()),
        },
        concurrency: 1,
        max_queue: 4,
        timeout_ms: 1_000,
        retry_count: 0,
        missed: MissedPolicy::Skip,
        overlap: OverlapPolicy::Queue,
        notification: None,
        smtp: None,
    };
    assert!(scan_folder_events(&spec, root.path(), Duration::ZERO)
        .unwrap()
        .is_empty());
}

#[test]
fn unknown_schema_is_rejected_without_destructive_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("未来.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    let error = RunHistory::open_recover(&path, 16).err().unwrap();
    assert!(error.contains("schema 版本不支持"), "{error}");
    assert!(path.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn a_short_database_lock_is_waited_out_instead_of_losing_the_trigger() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("锁.db");
    RunHistory::open(&database, 16).unwrap();
    let (locked, receiver) = std::sync::mpsc::channel();
    let lock_path = database.clone();
    let holder = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(lock_path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        connection.execute_batch("COMMIT").unwrap();
    });
    receiver.recv().unwrap();
    let input = directory.path().join("输入.csv");
    std::fs::write(&input, b"locked").unwrap();
    let event = stable_file(&input, Duration::ZERO, None).unwrap().unwrap();
    let history = RunHistory::open(&database, 16).unwrap();
    assert!(history
        .reserve_file("导入", &event, 1, 100, 0)
        .unwrap()
        .is_some());
    holder.join().unwrap();
}
