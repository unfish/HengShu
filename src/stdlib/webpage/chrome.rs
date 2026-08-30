use super::error;
use rand::Rng;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(super) fn find_chrome() -> Option<PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

pub(super) struct ChromeProcess {
    child: Option<Child>,
    profile: PathBuf,
    pub(super) port: u16,
}

impl ChromeProcess {
    pub(super) fn launch(
        executable: &Path,
        headless: bool,
        timeout: Duration,
    ) -> Result<Self, String> {
        if !executable.is_file() {
            return Err(error(format!("浏览器程序不存在：{}", executable.display())));
        }
        let profile = std::env::temp_dir().join(format!(
            "hengshu-chrome-{}-{:016x}",
            std::process::id(),
            rand::thread_rng().gen::<u64>()
        ));
        fs::create_dir(&profile).map_err(|value| error(value.to_string()))?;
        let mut command = Command::new(executable);
        command
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-networking")
            .arg("--window-size=1440,1000")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if headless {
            command.arg("--headless=new");
        }
        let child = command
            .spawn()
            .map_err(|value| error(format!("启动浏览器失败：{value}")))?;
        let mut process = Self {
            child: Some(child),
            profile,
            port: 0,
        };
        process.port = process.wait_for_port(timeout)?;
        Ok(process)
    }

    fn wait_for_port(&mut self, timeout: Duration) -> Result<u16, String> {
        let path = self.profile.join("DevToolsActivePort");
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| error("浏览器启动超时设置过大"))?;
        loop {
            if let Ok(text) = fs::read_to_string(&path) {
                if let Some(port) = text.lines().next().and_then(|value| value.parse().ok()) {
                    return Ok(port);
                }
            }
            if self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .is_some()
            {
                return Err(error("浏览器在调试端口就绪前退出"));
            }
            if Instant::now() >= deadline {
                return Err(error("等待浏览器调试端口超时"));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(super) fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChromeProcess {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.profile);
    }
}

pub(super) fn create_target(port: u16, url: &str, timeout: Duration) -> Result<String, String> {
    let encoded: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
    let endpoint = format!("http://127.0.0.1:{port}/json/new?{encoded}");
    let response = ureq::put(&endpoint)
        .timeout(timeout)
        .call()
        .map_err(|value| error(format!("创建浏览器页面失败：{value}")))?;
    let value: JsonValue = response
        .into_json()
        .map_err(|value| error(format!("解析浏览器页面信息失败：{value}")))?;
    value
        .get("webSocketDebuggerUrl")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| error("浏览器没有返回页面调试地址"))
}
