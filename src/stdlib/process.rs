use super::http::min_timeout;
use super::{error, resolve};
use crate::contracts::timeout_duration;
use crate::security::PermissionSet;
use crate::value::Value;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

pub(super) fn process_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    base_dir: &Path,
    execution_budget: Option<Duration>,
    permissions: &PermissionSet,
) -> Result<Value, String> {
    if name != "执行" {
        return Err(error(format!("进程模块不支持“{name}”")));
    }
    let mut command = Command::new(args[0].to_string());
    if let Some(arguments) = args.get(1) {
        command.args(
            arguments
                .iter_values_limited(permissions.collection_limit())?
                .iter()
                .map(ToString::to_string),
        );
    }
    if let Some(directory) = named.get("工作目录") {
        command.current_dir(resolve(base_dir, directory));
    }
    if let Some(Value::Record(environment)) = named.get("环境") {
        command.envs(
            environment
                .borrow()
                .iter()
                .map(|(key, value)| (key, value.to_string())),
        );
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let timeout = named
        .get("最长等待")
        .map(|value| timeout_duration(value, "进程最长等待"))
        .transpose()?;
    let timeout = min_timeout(timeout, execution_budget);
    let (status, stdout, stderr) = execute(command, timeout, permissions.buffer_limit())?;
    Ok(Value::record(BTreeMap::from([
        (
            "退出码".into(),
            status
                .code()
                .map(|code| Value::Number(Decimal::from(code)))
                .unwrap_or(Value::Null),
        ),
        ("成功".into(), Value::Bool(status.success())),
        (
            "标准输出".into(),
            Value::Text(String::from_utf8_lossy(&stdout).into_owned()),
        ),
        (
            "标准错误".into(),
            Value::Text(String::from_utf8_lossy(&stderr).into_owned()),
        ),
    ])))
}

fn execute(
    mut command: Command,
    timeout: Option<Duration>,
    buffer_limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    let mut child = command
        .spawn()
        .map_err(|error_value| error(error_value.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("无法捕获标准输出"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| error("无法捕获标准错误"))?;
    let stdout_reader = std::thread::spawn(move || read_pipe(stdout, buffer_limit));
    let stderr_reader = std::thread::spawn(move || read_pipe(stderr, buffer_limit));
    if let Some(timeout) = timeout {
        if child
            .wait_timeout(timeout)
            .map_err(|error_value| error(error_value.to_string()))?
            .is_none()
        {
            child.kill().ok();
            child.wait().ok();
            // 不等待读取线程：继承了管道的孙进程可能仍在运行，等待它们会让超时失效。
            return Err(crate::runtime::resource_error("子进程执行超时"));
        }
    }
    let status = child
        .wait()
        .map_err(|error_value| error(error_value.to_string()))?;
    let stdout = join_reader(stdout_reader, "标准输出")?;
    let stderr = join_reader(stderr_reader, "标准错误")?;
    Ok((status, stdout, stderr))
}

fn read_pipe(pipe: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    crate::resource::read_to_end_limited(pipe, limit)
}

fn join_reader(
    reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| error(format!("{name}读取线程异常结束")))?
        .map_err(|error_value| error(error_value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    #[test]
    fn timed_process_drains_large_stdout_while_waiting() {
        let permissions = PermissionSet::for_source(Path::new("."));
        let result = process_call(
            "执行",
            vec![
                Value::Text("/bin/sh".into()),
                Value::list(vec![
                    Value::Text("-c".into()),
                    Value::Text("yes x | head -c 200000".into()),
                ]),
            ],
            BTreeMap::from([(
                "最长等待".into(),
                Value::Duration(chrono::Duration::seconds(2)),
            )]),
            Path::new("."),
            None,
            &permissions,
        )
        .unwrap();
        let Value::Record(result) = result else {
            panic!()
        };
        let Value::Text(stdout) = &result.borrow()["标准输出"] else {
            panic!()
        };
        assert_eq!(stdout.len(), 200_000);

        let mut limited = PermissionSet::for_source(Path::new("."));
        limited.max_buffer_bytes = Some(4);
        let error = process_call(
            "执行",
            vec![
                Value::Text("/bin/sh".into()),
                Value::list(vec![
                    Value::Text("-c".into()),
                    Value::Text("printf 12345".into()),
                ]),
            ],
            BTreeMap::new(),
            Path::new("."),
            None,
            &limited,
        )
        .unwrap_err();
        assert!(error.contains("缓冲上限"));
    }

    #[cfg(unix)]
    #[test]
    fn process_timeout_kills_the_child_and_reports_a_resource_error() {
        let permissions = PermissionSet::for_source(Path::new("."));
        let started = std::time::Instant::now();
        let error = process_call(
            "执行",
            vec![
                Value::Text("/bin/sh".into()),
                Value::list(vec![
                    Value::Text("-c".into()),
                    // 孙进程继承管道，等待管道关闭会让超时参数失效。
                    Value::Text("sleep 20 & wait".into()),
                ]),
            ],
            BTreeMap::from([(
                "最长等待".into(),
                Value::Duration(chrono::Duration::milliseconds(100)),
            )]),
            Path::new("."),
            None,
            &permissions,
        )
        .unwrap_err();
        assert!(started.elapsed() < StdDuration::from_secs(2), "{error}");
        assert_eq!(
            crate::runtime::RuntimeError::new(3, error).render(),
            "第 3 行发生资源错误：\n子进程执行超时"
        );
    }
}
