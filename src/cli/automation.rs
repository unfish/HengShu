//! Scheduler, packaged-trigger, and notification command adapters.

use super::*;

pub(super) fn schedule_plan(
    tool_name: &str,
    program: Option<PathBuf>,
    poll_seconds: u64,
) -> Result<(PathBuf, crate::automation::scheduler::InstallPlan), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let root = project::find_root(&current).ok_or("找不到衡枢.toml")?;
    let manifest = project::read_manifest(&root)?;
    let spec = manifest
        .automations
        .get(tool_name)
        .cloned()
        .ok_or_else(|| format!("工具“{tool_name}”没有自动化声明"))?;
    spec.validate()?;
    let program = program
        .unwrap_or_else(|| root.join(pack::default_output(&root.join(&manifest.project.entry))));
    if !program.exists() {
        return Err(format!("排程需要已打包的 CLI 产物：{}", program.display()));
    }
    // OS task identifiers are deliberately ASCII-only; keep the user-facing
    // Chinese tool name in the command argument and derive a stable identifier
    // for platform task names.
    let identifier = format!(
        "tool-{}",
        &crate::execution::hex_digest(tool_name.as_bytes())[..16]
    );
    let task = crate::automation::scheduler::task_from_spec(
        &identifier,
        &program,
        vec!["--衡枢自动化触发".into(), "--工具".into(), tool_name.into()],
        &root,
        spec,
        poll_seconds,
    )?;
    let platform = crate::automation::scheduler::SchedulerPlatform::current()?;
    Ok((
        root,
        crate::automation::scheduler::install_plan(platform, &task)?,
    ))
}

fn history_database(root: &Path, args: &[String]) -> PathBuf {
    option(args, &["--数据库", "--database"])
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".衡枢运行历史.db"))
}

/// 排程与文件监视使用的无界面单次运行入口：不创建窗口，把结果写入运行历史。
pub(super) fn schedule_trigger(tool_name: &str, args: &[String]) -> Result<(), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let root = project::find_root(&current).ok_or("找不到衡枢.toml")?;
    let manifest = project::read_manifest(&root)?;
    let tool = manifest
        .tools
        .get(tool_name)
        .ok_or_else(|| format!("没有工具声明“{tool_name}”"))?;
    let entry = root.join(&tool.entry);
    let spec = manifest
        .automations
        .get(tool_name)
        .cloned()
        .ok_or_else(|| format!("工具“{tool_name}”没有自动化声明"))?;
    let history_path = history_database(&root, args);
    let notification_permissions = permissions(&entry, &[])?;
    host_permissions(&entry, &notification_permissions).check_file(&history_path)?;
    let history = crate::automation::RunHistory::open_recover(&history_path, 10_000)?;
    let mut command_args = vec!["--工具".to_string(), tool_name.to_string()];
    let mut index = 0;
    while index < args.len() {
        if matches!(
            args[index].as_str(),
            "--参数" | "--parameter" | "--参数文件" | "--parameter-file"
        ) {
            if let Some(value) = args.get(index + 1) {
                command_args.push(args[index].clone());
                command_args.push(value.clone());
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    command_args.extend(["--最长执行时间".into(), format!("{}毫秒", spec.timeout_ms)]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut invocations = Vec::new();
    if matches!(&spec.trigger, crate::automation::Trigger::Folder { .. }) {
        for event in crate::automation::scan_folder_events(&spec, &root, std::time::Duration::ZERO)?
        {
            let Some(reservation) = history.reserve_file(
                tool_name,
                &event,
                now_ms,
                spec.timeout_ms,
                spec.retry_count,
            )?
            else {
                continue;
            };
            let file = event.path.clone();
            let relative = file.strip_prefix(&root).unwrap_or(&file).to_path_buf();
            let mut current = command_args.clone();
            if let Some((name, _)) = tool.parameters.iter().find(|(_, parameter)| {
                matches!(
                    parameter.type_name.trim(),
                    "路径" | "文件" | "path" | "file"
                )
            }) {
                current.extend(["--参数".into(), format!("{name}={}", relative.display())]);
            } else {
                current.extend(["--".into(), event.path.display().to_string()]);
            }
            invocations.push((reservation, current));
        }
    } else {
        invocations.push((
            history.reserve_immediate(tool_name, now_ms, spec.timeout_ms)?,
            command_args,
        ));
    }
    match spec.overlap {
        crate::automation::OverlapPolicy::Queue => {}
        crate::automation::OverlapPolicy::Skip => invocations.truncate(spec.concurrency),
        crate::automation::OverlapPolicy::Replace => {
            if let Some(latest) = invocations.pop() {
                invocations.clear();
                invocations.push(latest);
            }
        }
    }
    if invocations.is_empty() {
        return Ok(());
    }
    let output_root = root.join(tool.output_root(crate::execution::OutputKind::Cli));
    let mut first_failure = None;
    for group in invocations.chunks(spec.concurrency) {
        let mut outcomes = std::thread::scope(|scope| {
            group
                .iter()
                .map(|(reservation, current)| {
                    let reservation = reservation.clone();
                    let entry = &entry;
                    scope.spawn(move || (reservation, scheduled_source_outcome(entry, current)))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        (
                            crate::automation::TriggerReservation {
                                key: String::new(),
                                scheduled_ms: now_ms,
                                input_summary: "自动化工作线程".into(),
                            },
                            automation_failure_outcome("自动化工作线程异常终止"),
                        )
                    })
                })
                .collect::<Vec<_>>()
        });
        for (index, (reservation, mut outcome)) in outcomes.drain(..).enumerate() {
            if reservation.key.is_empty() {
                return Err("自动化工作线程异常终止".into());
            }
            let started = chrono::Utc::now().timestamp_millis();
            let run_id = format!(
                "{}-{}-{}-{index}",
                tool_name,
                started,
                &reservation.key[..12]
            );
            let status_text = if outcome.success {
                "自动化运行成功"
            } else {
                "自动化运行失败"
            };
            outcome
                .notification_status
                .extend(embedded_notification_status(
                    &spec,
                    tool_name,
                    &run_id,
                    status_text,
                    &root,
                    &notification_permissions,
                ));
            if !outcome.success && first_failure.is_none() {
                first_failure = outcome
                    .diagnostics
                    .first()
                    .map(|diagnostic| crate::privacy::scrub_secrets(&diagnostic.message));
            }
            let record = crate::automation::RunRecord::from_outcome(
                run_id,
                tool_name,
                started,
                Some(reservation.key.clone()),
                &reservation.input_summary,
                output_root.display().to_string(),
                outcome,
            );
            history.commit_trigger(&reservation, &record)?;
        }
    }
    first_failure.map_or(Ok(()), Err)
}

fn automation_failure_outcome(message: impl Into<String>) -> crate::execution::RunOutcome {
    crate::execution::RunOutcome {
        success: false,
        termination: crate::execution::RunTermination::Failed,
        diagnostics: vec![crate::api::DiagnosticReport {
            category: Some("自动化错误".into()),
            span: None,
            message: crate::privacy::scrub_secrets(&message.into()),
            stack: Vec::new(),
            definition_span: None,
            cause: None,
        }],
        ..crate::execution::RunOutcome::default()
    }
}

fn scheduled_source_outcome(path: &Path, command_args: &[String]) -> crate::execution::RunOutcome {
    let prepared = (|| {
        let parameters = tool_parameters(path, command_args)?;
        let root = project::find_root(path).ok_or("自动化源码不在项目中")?;
        let manifest = project::read_manifest(&root)?;
        let tool_name = option(command_args, &["--工具", "--tool"]).ok_or("缺少自动化工具名")?;
        let tool = manifest
            .tools
            .get(&tool_name)
            .ok_or_else(|| format!("没有工具声明“{tool_name}”"))?;
        let output_root = root.join(tool.output_root(crate::execution::OutputKind::Cli));
        fs::create_dir_all(&output_root).map_err(|error| format!("创建输出目录失败：{error}"))?;
        let mut program_args = command_args
            .iter()
            .position(|argument| argument == "--")
            .map(|index| rest(command_args, index + 1).to_vec())
            .unwrap_or_default();
        program_args.extend(tool.encode_program_args(&parameters)?);
        let text = source(path)?;
        let mut options = crate::api::RunOptions::new(path.parent().unwrap_or(Path::new(".")));
        options.print_output = true;
        options.program_args = program_args;
        options.permissions = permissions(path, command_args)?;
        Ok::<_, String>((text, options))
    })();
    match prepared {
        Ok((text, options)) => crate::api::run_source_outcome(&text, options),
        Err(error) => automation_failure_outcome(error),
    }
}

pub(super) fn embedded_automation_trigger(
    embedded: crate::pack::EmbeddedProgram,
    args: &[String],
    base: &Path,
) -> Result<(), String> {
    let spec = embedded
        .descriptor
        .automation
        .clone()
        .ok_or("独立包没有固化自动化声明")?;
    let tool = embedded
        .descriptor
        .tool
        .clone()
        .ok_or("自动化独立包缺少工具 schema")?;
    let forced = args.iter().any(|argument| argument == "--衡枢自动化立即");
    let mut permissions = embedded.permissions.clone();
    permissions.anchor_file_roots(base);
    permissions.execution_timeout_ms = Some(
        permissions
            .execution_timeout_ms
            .map_or(spec.timeout_ms, |current| current.min(spec.timeout_ms)),
    );
    let history_path = base.join(".衡枢运行历史.db");
    let mut host = permissions.clone();
    if !host.file_roots.contains(&base.to_path_buf()) {
        host.file_roots.push(base.to_path_buf());
    }
    host.check_file(&history_path)?;
    let history = crate::automation::RunHistory::open_recover(&history_path, 10_000)?;
    let now = chrono::Utc::now();
    let filtered = args
        .iter()
        .filter(|argument| !matches!(argument.as_str(), "--衡枢自动化触发" | "--衡枢自动化立即"))
        .cloned()
        .collect::<Vec<_>>();
    let mut invocations = Vec::new();
    if matches!(&spec.trigger, crate::automation::Trigger::Folder { .. }) {
        for event in crate::automation::scan_folder_events(&spec, base, std::time::Duration::ZERO)?
        {
            let Some(reservation) = history.reserve_file(
                &tool.name,
                &event,
                now.timestamp_millis(),
                spec.timeout_ms,
                spec.retry_count,
            )?
            else {
                continue;
            };
            let mut current = filtered.clone();
            if let Some((name, _)) = tool.parameters.iter().find(|(_, parameter)| {
                matches!(
                    parameter.type_name.trim(),
                    "路径" | "文件" | "path" | "file"
                )
            }) {
                let relative = event.path.strip_prefix(base).unwrap_or(&event.path);
                current.extend(["--参数".into(), format!("{name}={}", relative.display())]);
            } else {
                current.extend(["--".into(), event.path.display().to_string()]);
            }
            invocations.push((reservation, current));
        }
    } else {
        let reservations = if forced {
            vec![history.reserve_immediate(&tool.name, now.timestamp_millis(), spec.timeout_ms)?]
        } else if let crate::automation::Trigger::Calendar { expression } = &spec.trigger {
            let rule = crate::automation::parse_calendar_expression(expression)?;
            let timezone = spec
                .timezone
                .parse::<chrono_tz::Tz>()
                .map_err(|_| format!("IANA 时区无效：{}", spec.timezone))?;
            history.reserve_calendar(
                &tool.name,
                &rule,
                timezone,
                now,
                spec.missed,
                spec.max_queue,
                spec.timeout_ms,
                spec.retry_count,
            )?
        } else {
            Vec::new()
        };
        invocations.extend(
            reservations
                .into_iter()
                .map(|reservation| (reservation, filtered.clone())),
        );
    }
    if matches!(spec.overlap, crate::automation::OverlapPolicy::Skip) {
        invocations.truncate(spec.concurrency);
    } else if matches!(spec.overlap, crate::automation::OverlapPolicy::Replace) {
        if let Some(latest) = invocations.pop() {
            invocations.clear();
            invocations.push(latest);
        }
    }
    if invocations.is_empty() {
        return Ok(());
    }
    let mut first_error = None;
    let mut first_failure = None;
    let output_root = base.join(tool.output_root(crate::execution::OutputKind::Cli));
    for group in invocations.chunks(spec.concurrency) {
        let results = std::thread::scope(|scope| {
            group
                .iter()
                .map(|(reservation, invocation)| {
                    let reservation = reservation.clone();
                    let program_args = embedded_tool_arguments(invocation, Some(&tool));
                    let bytecode = &embedded.bytecode;
                    let modules = embedded.modules.clone();
                    let permissions = permissions.clone();
                    scope.spawn(move || {
                        let (outcome, error) = match program_args {
                            Ok(program_args) => crate::api::run_embedded_outcome(
                                bytecode,
                                modules,
                                base,
                                true,
                                program_args,
                                permissions,
                                ObservationMode::Off,
                                ObservationBudget::default(),
                            ),
                            Err(error) => (automation_failure_outcome(error), None),
                        };
                        (reservation, outcome, error)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "自动化工作线程异常终止".to_string())
                })
                .collect::<Vec<_>>()
        });
        for result in results {
            let (reservation, mut outcome, error) = result?;
            let started = chrono::Utc::now().timestamp_millis();
            let run_id = format!("{}-{}-{}", tool.name, started, &reservation.key[..12]);
            let status_text = if outcome.success {
                "自动化运行成功"
            } else {
                "自动化运行失败"
            };
            outcome
                .notification_status
                .extend(embedded_notification_status(
                    &spec,
                    &tool.name,
                    &run_id,
                    status_text,
                    base,
                    &permissions,
                ));
            if !outcome.success && first_failure.is_none() {
                first_failure = outcome
                    .diagnostics
                    .first()
                    .map(|diagnostic| diagnostic.message.clone());
            }
            let record = crate::automation::RunRecord::from_outcome(
                run_id,
                &tool.name,
                started,
                Some(reservation.key.clone()),
                &reservation.input_summary,
                output_root.display().to_string(),
                outcome,
            );
            history.commit_trigger(&reservation, &record)?;
            if first_error.is_none() {
                first_error = error;
            }
        }
    }
    if let Some(error) = first_error {
        Err(render_and_report(&error, Some(&host)))
    } else {
        first_failure.map_or(Ok(()), Err)
    }
}

fn embedded_notification_status(
    spec: &crate::automation::AutomationSpec,
    tool_name: &str,
    run_id: &str,
    status_text: &str,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Vec<String> {
    let policy = permissions.endpoint_policy();
    let sensitivity = crate::execution::Sensitivity::default();
    let mut status = Vec::new();
    if let Some(notification) = &spec.notification {
        let secret = notification.secret_environment.as_deref().and_then(|name| {
            permissions.check_environment(name).ok()?;
            crate::dotenv::value(base_dir, name)
        });
        let message = crate::automation::WebhookMessage {
            title: tool_name.into(),
            text: status_text.into(),
            link: None,
        };
        match crate::automation::send_webhook(
            notification.kind,
            &notification.address,
            &message,
            secret.as_deref(),
            permissions,
            &policy,
            &sensitivity,
        ) {
            Ok(()) => status.push("Webhook:成功".into()),
            Err(error) => status.push(crate::privacy::scrub_secrets(&format!(
                "Webhook:失败：{error}"
            ))),
        }
    }
    if let Some(notification) = &spec.smtp {
        let username = notification
            .username_environment
            .as_deref()
            .and_then(|name| {
                permissions.check_environment(name).ok()?;
                crate::dotenv::value(base_dir, name)
            });
        let secret = permissions
            .check_environment(&notification.secret_environment)
            .and_then(|_| {
                crate::dotenv::value(base_dir, &notification.secret_environment)
                    .ok_or_else(|| "SMTP 密钥环境变量不存在".to_string())
            });
        match secret {
            Err(error) => status.push(format!("SMTP:失败：{error}")),
            Ok(secret) => {
                crate::privacy::register_secret(&secret);
                let config = crate::smtp::SmtpConfig {
                    host: notification.host.clone(),
                    port: notification.port,
                    tls: notification.tls,
                    from: notification.from.clone(),
                    timeout_ms: 30_000,
                    max_attachment_bytes: permissions.buffer_limit(),
                };
                let credentials =
                    username.map(|username| crate::smtp::SmtpCredentials { username, secret });
                for recipient in &notification.recipients {
                    let result = crate::smtp::build_message(
                        &config,
                        run_id,
                        recipient,
                        tool_name,
                        status_text,
                        None,
                        Vec::new(),
                    )
                    .and_then(|message| {
                        crate::smtp::send_message(
                            &config,
                            &message,
                            credentials.as_ref(),
                            permissions,
                            &policy,
                            &sensitivity,
                            spec.retry_count,
                        )
                    });
                    match result {
                        Ok(delivery) => {
                            status.push(format!("SMTP:{recipient}:成功:{}次", delivery.attempts))
                        }
                        Err(error) => status.push(crate::privacy::scrub_secrets(&format!(
                            "SMTP:{recipient}:失败：{error}"
                        ))),
                    }
                }
            }
        }
    }
    status
}
