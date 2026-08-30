//! 当前用户级 OS 任务适配：Windows 任务计划、macOS launchd、Linux systemd user timer。
//!
//! 安装描述是纯数据：文件内容与要执行的命令分开生成，便于在没有对应平台的机器上测试。
//! 实际写入和执行需要子进程权限，并且只允许写入当前用户的任务目录。

use super::{AutomationSpec, MissedPolicy, OverlapPolicy, Trigger};
use crate::security::PermissionSet;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SchedulerPlatform {
    Windows,
    MacOs,
    Linux,
}

impl SchedulerPlatform {
    pub fn current() -> Result<Self, String> {
        match std::env::consts::OS {
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            other => Err(format!("当前平台不支持排程安装：{other}")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTask {
    /// 任务标识只允许字母、数字、点、下划线和短横线，避免命令与文件名注入。
    pub identifier: String,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_dir: PathBuf,
    pub spec: AutomationSpec,
    /// 文件夹监视在三平台都退化为有界轮询，秒数由此决定。
    pub poll_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub platform: SchedulerPlatform,
    pub files: Vec<(PathBuf, String)>,
    pub install: Vec<Vec<String>>,
    pub query: Vec<String>,
    pub uninstall: Vec<Vec<String>>,
    pub remove_files: Vec<PathBuf>,
}

fn valid_identifier(identifier: &str) -> Result<(), String> {
    if identifier.is_empty() || identifier.len() > 64 {
        return Err("排程任务标识长度必须在 1 到 64 之间".into());
    }
    if !identifier
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
    {
        return Err("排程任务标识只允许字母、数字、点、下划线和短横线".into());
    }
    Ok(())
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "找不到当前用户主目录".to_string())
}

impl ScheduledTask {
    pub fn validate(&self) -> Result<(), String> {
        valid_identifier(&self.identifier)?;
        self.spec.validate()?;
        if self.program.as_os_str().is_empty() {
            return Err("排程任务缺少 CLI 程序路径".into());
        }
        if self.poll_seconds == 0 || self.poll_seconds > 86_400 {
            return Err("排程轮询间隔必须在 1 秒到 1 天之间".into());
        }
        if self
            .arguments
            .iter()
            .any(|argument| argument.chars().any(char::is_control))
        {
            return Err("排程任务参数不允许包含控制字符".into());
        }
        Ok(())
    }

    fn command_line(&self) -> String {
        std::iter::once(self.program.display().to_string())
            .chain(self.arguments.iter().cloned())
            .map(|part| systemd_quote(&part))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn windows_quote(argument: &str) -> String {
    if !argument.is_empty()
        && !argument
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return argument.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
            backslashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            backslashes = 0;
            quoted.push(character);
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

fn systemd_quote(argument: &str) -> String {
    let escaped = argument.replace('%', "%%");
    if !escaped.is_empty()
        && escaped.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '/' | '.' | '_' | '-' | '=' | ':')
        })
    {
        escaped
    } else {
        format!("\"{}\"", escaped.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn windows_plan(task: &ScheduledTask) -> Result<InstallPlan, String> {
    let name = format!("衡枢\\{}", task.identifier);
    let trigger = match &task.spec.trigger {
        Trigger::Calendar { .. } => "<TimeTrigger><StartBoundary>2026-01-01T00:00:00</StartBoundary><Enabled>true</Enabled><Repetition><Interval>PT60S</Interval></Repetition></TimeTrigger>".into(),
        Trigger::Folder { .. } => format!(
            "<TimeTrigger><StartBoundary>2026-01-01T00:00:00</StartBoundary><Enabled>true</Enabled><Repetition><Interval>PT{}S</Interval></Repetition></TimeTrigger>",
            task.poll_seconds
        ),
    };
    let multiple = match task.spec.overlap {
        OverlapPolicy::Queue => "Queue",
        OverlapPolicy::Skip => "IgnoreNew",
        OverlapPolicy::Replace => "StopExisting",
    };
    let missed = matches!(task.spec.missed, MissedPolicy::Skip);
    let arguments = task
        .arguments
        .iter()
        .map(|argument| escape_xml(&windows_quote(argument)))
        .collect::<Vec<_>>()
        .join(" ");
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><RegistrationInfo><Description>衡枢自动化 {identifier}</Description></RegistrationInfo><Triggers>{trigger}</Triggers><Settings><MultipleInstancesPolicy>{multiple}</MultipleInstancesPolicy><StartWhenAvailable>{available}</StartWhenAvailable><ExecutionTimeLimit>PT{timeout}S</ExecutionTimeLimit><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries></Settings><Actions><Exec><Command>{program}</Command><Arguments>{arguments}</Arguments><WorkingDirectory>{working}</WorkingDirectory></Exec></Actions></Task>\n",
        identifier = escape_xml(&task.identifier),
        available = if missed { "false" } else { "true" },
        timeout = task.spec.timeout_ms.div_ceil(1000),
        program = escape_xml(&task.program.display().to_string()),
        working = escape_xml(&task.working_dir.display().to_string()),
    );
    let file = std::env::temp_dir().join(format!("衡枢排程-{}.xml", task.identifier));
    Ok(InstallPlan {
        platform: SchedulerPlatform::Windows,
        files: vec![(file.clone(), xml)],
        install: vec![vec![
            "schtasks.exe".into(),
            "/Create".into(),
            "/TN".into(),
            name.clone(),
            "/XML".into(),
            file.display().to_string(),
            "/F".into(),
        ]],
        query: vec![
            "schtasks.exe".into(),
            "/Query".into(),
            "/TN".into(),
            name.clone(),
            "/V".into(),
        ],
        uninstall: vec![vec![
            "schtasks.exe".into(),
            "/Delete".into(),
            "/TN".into(),
            name,
            "/F".into(),
        ]],
        remove_files: vec![file],
    })
}

fn macos_plan(task: &ScheduledTask) -> Result<InstallPlan, String> {
    let label = format!("com.hengshu.{}", task.identifier);
    let path = home()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"));
    let schedule = match &task.spec.trigger {
        Trigger::Calendar { .. } => {
            "<key>StartInterval</key><integer>60</integer>".into()
        }
        Trigger::Folder { root, .. } => format!(
            "<key>WatchPaths</key><array><string>{}</string></array><key>StartInterval</key><integer>{}</integer>",
            escape_xml(&root.display().to_string()),
            task.poll_seconds
        ),
    };
    let arguments = std::iter::once(task.program.display().to_string())
        .chain(task.arguments.iter().cloned())
        .map(|argument| format!("<string>{}</string>", escape_xml(&argument)))
        .collect::<String>();
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array>{arguments}</array><key>WorkingDirectory</key><string>{working}</string>{schedule}<key>RunAtLoad</key><false/><key>ExitTimeOut</key><integer>{timeout}</integer><key>ProcessType</key><string>Background</string></dict></plist>\n",
        working = escape_xml(&task.working_dir.display().to_string()),
        timeout = task.spec.timeout_ms.div_ceil(1000),
    );
    let domain = format!("gui/{}", user_id()?);
    Ok(InstallPlan {
        platform: SchedulerPlatform::MacOs,
        files: vec![(path.clone(), plist)],
        install: vec![vec![
            "launchctl".into(),
            "bootstrap".into(),
            domain.clone(),
            path.display().to_string(),
        ]],
        query: vec![
            "launchctl".into(),
            "print".into(),
            format!("{domain}/{label}"),
        ],
        uninstall: vec![vec![
            "launchctl".into(),
            "bootout".into(),
            format!("{domain}/{label}"),
        ]],
        remove_files: vec![path],
    })
}

/// launchd 域需要当前用户 UID；从主目录属主读取，避免引入额外依赖或提升权限。
fn user_id() -> Result<u32, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::metadata(home()?)
            .map_err(|error| format!("读取当前用户主目录失败：{error}"))?
            .uid())
    }
    #[cfg(not(unix))]
    {
        Ok(0)
    }
}

fn linux_plan(task: &ScheduledTask) -> Result<InstallPlan, String> {
    let unit = format!("衡枢-{}", task.identifier);
    let directory = home()?.join(".config").join("systemd").join("user");
    let service_path = directory.join(format!("{unit}.service"));
    let timer_path = directory.join(format!("{unit}.timer"));
    let service = format!(
        "[Unit]\nDescription=衡枢自动化 {identifier}\n\n[Service]\nType=oneshot\nWorkingDirectory={working}\nExecStart={command}\nTimeoutStartSec={timeout}\n",
        identifier = task.identifier,
        working = systemd_quote(&task.working_dir.display().to_string()),
        command = task.command_line(),
        timeout = task.spec.timeout_ms.div_ceil(1000),
    );
    let schedule = match &task.spec.trigger {
        Trigger::Calendar { .. } => "OnBootSec=60\nOnUnitActiveSec=60\nPersistent=false\n".into(),
        Trigger::Folder { .. } => format!(
            "OnBootSec={seconds}\nOnUnitActiveSec={seconds}\nPersistent=false\n",
            seconds = task.poll_seconds
        ),
    };
    let timer = format!(
        "[Unit]\nDescription=衡枢自动化定时器 {identifier}\n\n[Timer]\n{schedule}Unit={unit}.service\nAccuracySec=1s\n\n[Install]\nWantedBy=timers.target\n",
        identifier = task.identifier,
    );
    Ok(InstallPlan {
        platform: SchedulerPlatform::Linux,
        files: vec![(service_path.clone(), service), (timer_path.clone(), timer)],
        install: vec![
            vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
            vec![
                "systemctl".into(),
                "--user".into(),
                "enable".into(),
                "--now".into(),
                format!("{unit}.timer"),
            ],
        ],
        query: vec![
            "systemctl".into(),
            "--user".into(),
            "status".into(),
            format!("{unit}.timer"),
        ],
        uninstall: vec![vec![
            "systemctl".into(),
            "--user".into(),
            "disable".into(),
            "--now".into(),
            format!("{unit}.timer"),
        ]],
        remove_files: vec![service_path, timer_path],
    })
}

pub fn install_plan(
    platform: SchedulerPlatform,
    task: &ScheduledTask,
) -> Result<InstallPlan, String> {
    task.validate()?;
    match platform {
        SchedulerPlatform::Windows => windows_plan(task),
        SchedulerPlatform::MacOs => macos_plan(task),
        SchedulerPlatform::Linux => linux_plan(task),
    }
}

fn run_command(command: &[String]) -> Result<String, String> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| "排程命令为空".to_string())?;
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("执行排程命令失败：{program} {error}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(text)
    } else {
        Err(format!("排程命令失败：{program} {text}"))
    }
}

fn check_paths(plan: &InstallPlan, permissions: &PermissionSet) -> Result<(), String> {
    permissions.check_process()?;
    for path in plan
        .files
        .iter()
        .map(|(path, _)| path)
        .chain(plan.remove_files.iter())
    {
        permissions.check_file(path)?;
    }
    Ok(())
}

pub fn install(plan: &InstallPlan, permissions: &PermissionSet) -> Result<String, String> {
    check_paths(plan, permissions)?;
    let mut committed = Vec::<(PathBuf, Option<Vec<u8>>)>::new();
    for (path, contents) in &plan.files {
        let original = if path.exists() {
            let metadata = path
                .metadata()
                .map_err(|error| format!("读取排程描述失败：{error}"))?;
            if metadata.len() > 1024 * 1024 {
                rollback_descriptions(&committed);
                return Err("已有排程描述超过恢复字节预算".into());
            }
            Some(std::fs::read(path).map_err(|error| format!("读取排程描述失败：{error}"))?)
        } else {
            None
        };
        if let Err(error) = crate::security::atomic_write_file(path, contents.as_bytes(), true) {
            rollback_descriptions(&committed);
            return Err(format!("写出排程描述失败：{error}"));
        }
        committed.push((path.clone(), original));
    }
    let mut log = String::new();
    for command in &plan.install {
        match run_command(command) {
            Ok(output) => log.push_str(&output),
            Err(error) => {
                for uninstall in plan.uninstall.iter().rev() {
                    let _ = run_command(uninstall);
                }
                rollback_descriptions(&committed);
                return Err(error);
            }
        }
    }
    Ok(log)
}

fn rollback_descriptions(files: &[(PathBuf, Option<Vec<u8>>)]) {
    for (path, original) in files.iter().rev() {
        if let Some(bytes) = original {
            let _ = crate::security::atomic_write_file(path, bytes, true);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn query(plan: &InstallPlan, permissions: &PermissionSet) -> Result<String, String> {
    permissions.check_process()?;
    run_command(&plan.query)
}

pub fn uninstall(plan: &InstallPlan, permissions: &PermissionSet) -> Result<String, String> {
    check_paths(plan, permissions)?;
    let mut log = String::new();
    for command in &plan.uninstall {
        log.push_str(&run_command(command).unwrap_or_else(|error| format!("{error}\n")));
    }
    for path in &plan.remove_files {
        if path.exists() {
            std::fs::remove_file(path).map_err(|error| format!("删除排程描述失败：{error}"))?;
        }
    }
    Ok(log)
}

/// 排程安装默认使用工程目录作为工作目录，可执行文件必须存在。
pub fn task_from_spec(
    identifier: &str,
    program: &Path,
    arguments: Vec<String>,
    working_dir: &Path,
    spec: AutomationSpec,
    poll_seconds: u64,
) -> Result<ScheduledTask, String> {
    let task = ScheduledTask {
        identifier: identifier.into(),
        program: program.to_path_buf(),
        arguments,
        working_dir: working_dir.to_path_buf(),
        spec,
        poll_seconds,
    };
    task.validate()?;
    Ok(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::parse_calendar_expression;
    use std::collections::BTreeSet;

    fn spec(trigger: Trigger) -> AutomationSpec {
        AutomationSpec {
            timezone: "Asia/Shanghai".into(),
            trigger,
            concurrency: 1,
            max_queue: 4,
            timeout_ms: 60_000,
            retry_count: 1,
            missed: MissedPolicy::RunOnce,
            overlap: OverlapPolicy::Skip,
            notification: None,
            smtp: None,
        }
    }

    fn task(trigger: Trigger) -> ScheduledTask {
        ScheduledTask {
            identifier: "月报".into(),
            program: PathBuf::from("/opt/月报"),
            arguments: vec!["--参数".into(), "月份=2026-08".into()],
            working_dir: PathBuf::from("/opt"),
            spec: spec(trigger),
            poll_seconds: 60,
        }
    }

    #[test]
    fn identifier_and_arguments_are_validated() {
        let mut invalid = task(Trigger::Calendar {
            expression: "每周一 08:00".into(),
        });
        invalid.identifier = "月报".into();
        assert!(invalid.validate().is_err(), "非 ASCII 标识必须被拒绝");
        invalid.identifier = "monthly-report".into();
        invalid.validate().unwrap();
        invalid.arguments = vec!["坏\n参数".into()];
        assert!(invalid.validate().is_err());
        invalid.arguments = Vec::new();
        invalid.poll_seconds = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn all_three_platforms_produce_user_level_calendar_plans() {
        let mut calendar = task(Trigger::Calendar {
            expression: "每周三 08:30".into(),
        });
        calendar.identifier = "monthly-report".into();
        let windows = install_plan(SchedulerPlatform::Windows, &calendar).unwrap();
        assert!(windows.files[0].1.contains("<Interval>PT60S</Interval>"));
        assert_eq!(windows.install[0][0], "schtasks.exe");
        assert!(windows.uninstall[0].contains(&"/Delete".to_string()));

        let macos = install_plan(SchedulerPlatform::MacOs, &calendar).unwrap();
        assert!(macos.files[0]
            .1
            .contains("<key>StartInterval</key><integer>60</integer>"));
        assert!(macos.files[0]
            .0
            .to_string_lossy()
            .contains("Library/LaunchAgents"));
        assert_eq!(macos.install[0][1], "bootstrap");

        let linux = install_plan(SchedulerPlatform::Linux, &calendar).unwrap();
        assert!(linux.files[1].1.contains("OnUnitActiveSec=60"));
        assert!(linux.files[1].1.contains("Persistent=false"));
        assert!(linux.files[0]
            .1
            .contains("ExecStart=/opt/月报 --参数 月份=2026-08"));
        assert!(linux.uninstall[0].contains(&"--user".to_string()));
    }

    #[test]
    fn folder_triggers_fall_back_to_bounded_polling_on_every_platform() {
        let mut folder = task(Trigger::Folder {
            root: PathBuf::from("/待处理"),
            extension: Some("xlsx".into()),
        });
        folder.identifier = "watch-inbox".into();
        folder.poll_seconds = 30;
        let windows = install_plan(SchedulerPlatform::Windows, &folder).unwrap();
        assert!(windows.files[0].1.contains("<Interval>PT30S</Interval>"));
        let macos = install_plan(SchedulerPlatform::MacOs, &folder).unwrap();
        assert!(macos.files[0].1.contains("<key>WatchPaths</key>"));
        assert!(macos.files[0]
            .1
            .contains("<key>StartInterval</key><integer>30</integer>"));
        let linux = install_plan(SchedulerPlatform::Linux, &folder).unwrap();
        assert!(linux.files[1].1.contains("OnUnitActiveSec=30"));
    }

    #[test]
    fn monthly_rules_and_overlap_policies_reach_the_platform_descriptions() {
        let mut monthly = task(Trigger::Calendar {
            expression: "每月5日 23:59".into(),
        });
        monthly.identifier = "monthly-5".into();
        monthly.spec.overlap = OverlapPolicy::Replace;
        monthly.spec.missed = MissedPolicy::Skip;
        let windows = install_plan(SchedulerPlatform::Windows, &monthly).unwrap();
        assert!(windows.files[0].1.contains("<Interval>PT60S</Interval>"));
        assert!(windows.files[0]
            .1
            .contains("<MultipleInstancesPolicy>StopExisting</MultipleInstancesPolicy>"));
        assert!(windows.files[0]
            .1
            .contains("<StartWhenAvailable>false</StartWhenAvailable>"));
        let macos = install_plan(SchedulerPlatform::MacOs, &monthly).unwrap();
        assert!(macos.files[0]
            .1
            .contains("<key>StartInterval</key><integer>60</integer>"));
        let linux = install_plan(SchedulerPlatform::Linux, &monthly).unwrap();
        assert!(linux.files[1].1.contains("OnUnitActiveSec=60"));
        assert!(linux.files[1].1.contains("Persistent=false"));
    }

    #[test]
    fn descriptions_escape_xml_and_installation_needs_process_permission() {
        let mut injected = task(Trigger::Calendar {
            expression: "每周一 08:00".into(),
        });
        injected.identifier = "safe-name".into();
        injected.arguments = vec!["--参数".into(), "备注=<a href=\"x\">&amp;</a>".into()];
        let windows = install_plan(SchedulerPlatform::Windows, &injected).unwrap();
        assert!(!windows.files[0].1.contains("<a href="));
        assert!(windows.files[0]
            .1
            .contains("&lt;a href=\\&quot;x\\&quot;&gt;"));
        let macos = install_plan(SchedulerPlatform::MacOs, &injected).unwrap();
        assert!(!macos.files[0].1.contains("<a href="));

        let directory = tempfile::tempdir().unwrap();
        let mut plan = install_plan(SchedulerPlatform::Linux, &injected).unwrap();
        plan.files = vec![(directory.path().join("unit.timer"), "[Timer]\n".into())];
        plan.remove_files = vec![directory.path().join("unit.timer")];
        plan.install = Vec::new();
        let denied = PermissionSet::restricted_for_source(Path::new("."));
        assert!(install(&plan, &denied).is_err());
        let mut allowed = denied.clone();
        allowed.allow_process = true;
        allowed.file_roots = vec![std::fs::canonicalize(directory.path()).unwrap()];
        install(&plan, &allowed).unwrap();
        assert!(directory.path().join("unit.timer").exists());
        plan.uninstall = Vec::new();
        uninstall(&plan, &allowed).unwrap();
        assert!(!directory.path().join("unit.timer").exists());
    }

    #[test]
    fn platform_arguments_preserve_spaces_quotes_backslashes_and_specifiers() {
        let mut scheduled = task(Trigger::Calendar {
            expression: "每周一 08:00".into(),
        });
        scheduled.identifier = "quoted-arguments".into();
        scheduled.program = PathBuf::from("/opt/衡枢 tools/main");
        scheduled.working_dir = PathBuf::from("/data/month %i");
        scheduled.arguments = vec![
            "--参数".into(),
            "备注=含 空格与\"引号\"".into(),
            "路径=C:\\reports\\".into(),
            "模板=%i".into(),
        ];

        let windows = install_plan(SchedulerPlatform::Windows, &scheduled).unwrap();
        let windows_xml = &windows.files[0].1;
        assert!(windows_xml.contains("&quot;备注=含 空格与\\&quot;引号\\&quot;&quot;"));
        assert!(windows_xml.contains("路径=C:\\reports\\"));

        let macos = install_plan(SchedulerPlatform::MacOs, &scheduled).unwrap();
        assert!(macos.files[0]
            .1
            .contains("<string>备注=含 空格与&quot;引号&quot;</string>"));

        let linux = install_plan(SchedulerPlatform::Linux, &scheduled).unwrap();
        let service = &linux.files[0].1;
        assert!(service.contains("WorkingDirectory=\"/data/month %%i\""));
        assert!(service.contains("\"备注=含 空格与\\\"引号\\\"\""));
        assert!(service.contains("模板=%%i"));
    }

    #[test]
    fn current_platform_is_resolved_or_reported() {
        let platform = SchedulerPlatform::current();
        if cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )) {
            assert!(platform.is_ok());
        } else {
            assert!(platform.is_err());
        }
        let rule = parse_calendar_expression("每周日 06:00").unwrap();
        assert_eq!(rule.weekdays, BTreeSet::from([7]));
    }
}
