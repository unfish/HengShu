//! Deterministic SMTP message construction, transport policy validation and a
//! bounded STARTTLS/SMTPS client with per-recipient isolation.

use crate::execution::Sensitivity;
use crate::privacy::{Endpoint, EndpointPolicy};
use crate::security::PermissionSet;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TlsMode {
    StartTls,
    Smtps,
    /// 仅本机回环夹具可用的明文通道。
    PlaintextLoopback,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub from: String,
    pub timeout_ms: u64,
    pub max_attachment_bytes: usize,
}

fn loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

impl SmtpConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.host.is_empty() || self.from.is_empty() {
            return Err("SMTP 主机和发件人不能为空".into());
        }
        if self.port == 0 || self.timeout_ms == 0 {
            return Err("SMTP 端口和超时必须大于 0".into());
        }
        if !self.from.contains('@') {
            return Err("SMTP 发件人地址无效".into());
        }
        if matches!(self.tls, TlsMode::PlaintextLoopback) && !loopback_host(&self.host) {
            return Err("SMTP 明文通道只允许本机回环夹具".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SmtpAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SmtpMessage {
    pub to: Vec<String>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub attachments: Vec<SmtpAttachment>,
    pub message_id: String,
}

pub fn stable_message_id(run_id: &str, recipient: &str, host: &str) -> String {
    let digest = Sha256::digest(format!("{run_id}\0{recipient}\0{host}").as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("<{}@{}>", &hex[..32], host)
}

pub fn build_message(
    config: &SmtpConfig,
    run_id: &str,
    recipient: &str,
    subject: &str,
    text: &str,
    html: Option<&str>,
    attachments: Vec<SmtpAttachment>,
) -> Result<SmtpMessage, String> {
    config.validate()?;
    if !recipient.contains('@') {
        return Err("SMTP 收件人地址无效".into());
    }
    let total: usize = attachments
        .iter()
        .map(|attachment| attachment.bytes.len())
        .sum();
    if total > config.max_attachment_bytes {
        return Err("SMTP 附件超过预算".into());
    }
    if attachments.iter().any(|attachment| {
        attachment.filename.is_empty()
            || attachment.filename.contains("..")
            || attachment.filename.contains('/')
            || attachment.filename.contains('\\')
    }) {
        return Err("SMTP 附件文件名不安全".into());
    }
    Ok(SmtpMessage {
        to: vec![recipient.into()],
        subject: subject.into(),
        text: text.into(),
        html: html.map(str::to_owned),
        attachments,
        message_id: stable_message_id(run_id, recipient, &config.host),
    })
}

pub fn render_mime(config: &SmtpConfig, message: &SmtpMessage) -> Result<String, String> {
    config.validate()?;
    if message.to.len() != 1 {
        return Err("SMTP 单封消息必须隔离收件人".into());
    }
    String::from_utf8(lettre_message(config, message)?.formatted())
        .map_err(|_| "SMTP MIME 不是 UTF-8".into())
}

fn lettre_message(config: &SmtpConfig, message: &SmtpMessage) -> Result<lettre::Message, String> {
    use lettre::message::{
        header::ContentType, Attachment, Mailbox, Message, MultiPart, SinglePart,
    };

    let from = config
        .from
        .parse::<Mailbox>()
        .map_err(|_| "SMTP 发件人地址无效")?;
    let to = message
        .to
        .first()
        .ok_or("SMTP 缺少收件人")?
        .parse::<Mailbox>()
        .map_err(|_| "SMTP 收件人地址无效")?;
    let builder = Message::builder()
        .from(from)
        .to(to)
        .subject(&message.subject)
        .message_id(Some(message.message_id.clone()));
    if message.html.is_none() && message.attachments.is_empty() {
        return builder
            .header(ContentType::TEXT_PLAIN)
            .body(message.text.clone())
            .map_err(|error| format!("SMTP 消息构造失败：{error}"));
    }
    let boundary_seed = crate::execution::hex_digest(message.message_id.as_bytes());
    let alternative = MultiPart::alternative()
        .boundary(format!("hengshu-alt-{}", &boundary_seed[..16]))
        .singlepart(SinglePart::plain(message.text.clone()));
    let alternative = if let Some(html) = &message.html {
        alternative.singlepart(SinglePart::html(html.clone()))
    } else {
        alternative
    };
    let mut multipart = if message.attachments.is_empty() {
        alternative
    } else {
        MultiPart::mixed()
            .boundary(format!("hengshu-mixed-{}", &boundary_seed[16..32]))
            .multipart(alternative)
    };
    for attachment in &message.attachments {
        let content_type =
            ContentType::parse(&attachment.content_type).map_err(|_| "SMTP 附件 MIME 类型无效")?;
        multipart = multipart.singlepart(
            Attachment::new(attachment.filename.clone())
                .body(attachment.bytes.clone(), content_type),
        );
    }
    builder
        .multipart(multipart)
        .map_err(|error| format!("SMTP 消息构造失败：{error}"))
}

/// 秘密只在会话内使用，不进入错误、日志或历史。
#[derive(Clone)]
pub struct SmtpCredentials {
    pub username: String,
    pub secret: String,
}

impl std::fmt::Debug for SmtpCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmtpCredentials")
            .field("username", &self.username)
            .field("secret", &"<已隐藏>")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SmtpDelivery {
    pub recipient: String,
    pub message_id: String,
    pub attempts: usize,
}

#[derive(Clone, Debug)]
struct SmtpFailure {
    message: String,
    retryable: bool,
}

impl SmtpFailure {
    fn temporary(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

struct Session {
    reader: BufReader<TcpStream>,
}

impl Session {
    fn connect(config: &SmtpConfig) -> Result<Self, SmtpFailure> {
        let timeout = Duration::from_millis(config.timeout_ms);
        let address = format!("{}:{}", config.host.trim_matches(['[', ']']), config.port);
        let target = std::net::ToSocketAddrs::to_socket_addrs(&address)
            .map_err(|error| SmtpFailure::temporary(format!("SMTP 主机解析失败：{error}")))?
            .next()
            .ok_or_else(|| SmtpFailure::permanent("SMTP 主机没有可用地址"))?;
        let tcp = TcpStream::connect_timeout(&target, timeout)
            .map_err(|error| SmtpFailure::temporary(format!("SMTP 连接失败：{error}")))?;
        tcp.set_read_timeout(Some(timeout))
            .and_then(|_| tcp.set_write_timeout(Some(timeout)))
            .map_err(|error| SmtpFailure::temporary(format!("SMTP 超时设置失败：{error}")))?;
        let mut session = Self {
            reader: BufReader::new(tcp),
        };
        session.expect(&[220])?;
        Ok(session)
    }

    fn read_reply(&mut self) -> Result<(u16, String), SmtpFailure> {
        let mut text = String::new();
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|error| SmtpFailure::temporary(format!("SMTP 读取应答失败：{error}")))?;
            if read == 0 {
                return Err(SmtpFailure::temporary("SMTP 连接被对方关闭"));
            }
            if line.len() < 3 {
                return Err(SmtpFailure::permanent("SMTP 应答格式无效"));
            }
            let code = line[..3]
                .parse::<u16>()
                .map_err(|_| SmtpFailure::permanent("SMTP 应答状态码无效"))?;
            text.push_str(line.trim_end());
            if line.as_bytes().get(3) == Some(&b'-') {
                text.push('\n');
                continue;
            }
            return Ok((code, text));
        }
    }

    fn expect(&mut self, accepted: &[u16]) -> Result<String, SmtpFailure> {
        let (code, text) = self.read_reply()?;
        if accepted.contains(&code) {
            return Ok(text);
        }
        let message = format!("SMTP 服务器返回 {code}");
        if (400..500).contains(&code) {
            Err(SmtpFailure::temporary(message))
        } else {
            Err(SmtpFailure::permanent(message))
        }
    }

    /// `secret` 为真时不把命令内容写进错误，避免凭据泄漏。
    fn command(
        &mut self,
        line: &str,
        accepted: &[u16],
        secret: bool,
    ) -> Result<String, SmtpFailure> {
        let stream = self.reader.get_mut();
        stream
            .write_all(format!("{line}\r\n").as_bytes())
            .and_then(|_| stream.flush())
            .map_err(|error| {
                if secret {
                    SmtpFailure::temporary("SMTP 发送认证命令失败".to_string())
                } else {
                    SmtpFailure::temporary(format!("SMTP 发送命令失败：{error}"))
                }
            })?;
        self.expect(accepted)
    }

    fn data(&mut self, body: &str) -> Result<(), SmtpFailure> {
        self.command("DATA", &[354], false)?;
        let mut payload = String::with_capacity(body.len() + 8);
        for line in body.split("\r\n") {
            if line.starts_with('.') {
                payload.push('.');
            }
            payload.push_str(line);
            payload.push_str("\r\n");
        }
        payload.push_str(".\r\n");
        let stream = self.reader.get_mut();
        stream
            .write_all(payload.as_bytes())
            .and_then(|_| stream.flush())
            .map_err(|error| SmtpFailure::temporary(format!("SMTP 写入正文失败：{error}")))?;
        self.expect(&[250])?;
        Ok(())
    }
}

fn deliver(
    config: &SmtpConfig,
    message: &SmtpMessage,
    credentials: Option<&SmtpCredentials>,
    body: &str,
) -> Result<(), SmtpFailure> {
    if !matches!(config.tls, TlsMode::PlaintextLoopback) {
        return deliver_with_lettre(config, message, credentials);
    }
    let mut session = Session::connect(config)?;
    session.command("EHLO hengshu", &[250], false)?;
    if let Some(credentials) = credentials {
        let engine = base64::engine::general_purpose::STANDARD;
        session.command("AUTH LOGIN", &[334], false)?;
        session.command(&engine.encode(&credentials.username), &[334], true)?;
        session.command(&engine.encode(&credentials.secret), &[235], true)?;
    }
    session.command(&format!("MAIL FROM:<{}>", config.from), &[250], false)?;
    session.command(&format!("RCPT TO:<{}>", message.to[0]), &[250, 251], false)?;
    session.data(body)?;
    session.command("QUIT", &[221], false).ok();
    Ok(())
}

fn deliver_with_lettre(
    config: &SmtpConfig,
    message: &SmtpMessage,
    credentials: Option<&SmtpCredentials>,
) -> Result<(), SmtpFailure> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{SmtpTransport, Transport};
    let body = lettre_message(config, message).map_err(SmtpFailure::permanent)?;
    let timeout = Duration::from_millis(config.timeout_ms);
    let mut transport = match config.tls {
        TlsMode::StartTls => SmtpTransport::starttls_relay(&config.host),
        TlsMode::Smtps => SmtpTransport::relay(&config.host),
        TlsMode::PlaintextLoopback => unreachable!(),
    }
    .map_err(|error| SmtpFailure::permanent(format!("SMTP 传输配置失败：{error}")))?
    .port(config.port)
    .timeout(Some(timeout));
    if let Some(credentials) = credentials {
        transport = transport.credentials(Credentials::new(
            credentials.username.clone(),
            credentials.secret.clone(),
        ));
    }
    transport.build().send(&body).map(|_| ()).map_err(|error| {
        let message = if error.is_transient() || error.is_timeout() {
            format!("SMTP 临时失败：{error}")
        } else {
            format!("SMTP 永久失败：{error}")
        };
        if error.is_transient() || error.is_timeout() || error.is_transport_shutdown() {
            SmtpFailure::temporary(message)
        } else {
            SmtpFailure::permanent(message)
        }
    })
}

/// 发送单封隔离消息；只对临时失败重试，永久失败立即返回。
pub fn send_message(
    config: &SmtpConfig,
    message: &SmtpMessage,
    credentials: Option<&SmtpCredentials>,
    permissions: &PermissionSet,
    policy: &EndpointPolicy,
    sensitivity: &Sensitivity,
    retries: usize,
) -> Result<SmtpDelivery, String> {
    config.validate()?;
    if message.to.len() != 1 {
        return Err("SMTP 单封消息必须隔离收件人".into());
    }
    permissions.check_network(&config.host)?;
    policy.allow(Endpoint::SmtpBody, sensitivity)?;
    if !message.attachments.is_empty() {
        policy.allow(Endpoint::SmtpAttachment, sensitivity)?;
    }
    let body = render_mime(config, message)?;
    let mut failure = None;
    for attempt in 0..=retries {
        match deliver(config, message, credentials, &body) {
            Ok(()) => {
                return Ok(SmtpDelivery {
                    recipient: message.to[0].clone(),
                    message_id: message.message_id.clone(),
                    attempts: attempt + 1,
                })
            }
            Err(error) if error.retryable && attempt < retries => failure = Some(error),
            Err(error) => return Err(error.message),
        }
    }
    Err(failure
        .map(|failure| failure.message)
        .unwrap_or_else(|| "SMTP 发送失败".into()))
}

/// 逐收件人独立建立会话，单个失败不影响其他收件人，结果按输入顺序稳定返回。
pub fn send_batch(
    config: &SmtpConfig,
    messages: &[SmtpMessage],
    credentials: Option<&SmtpCredentials>,
    permissions: &PermissionSet,
    policy: &EndpointPolicy,
    sensitivity: &Sensitivity,
    retries: usize,
) -> Vec<Result<SmtpDelivery, String>> {
    messages
        .iter()
        .map(|message| {
            send_message(
                config,
                message,
                credentials,
                permissions,
                policy,
                sensitivity,
                retries,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> SmtpConfig {
        SmtpConfig {
            host: "mail.example.com".into(),
            port: 587,
            tls: TlsMode::StartTls,
            from: "sender@example.com".into(),
            timeout_ms: 5000,
            max_attachment_bytes: 100,
        }
    }
    #[test]
    fn smtp_message_has_stable_id_and_isolated_recipient() {
        let message = build_message(
            &config(),
            "run-1",
            "a@example.com",
            "结果",
            "正文",
            Some("<p>正文</p>"),
            vec![SmtpAttachment {
                filename: "report.pdf".into(),
                content_type: "application/pdf".into(),
                bytes: vec![1, 2],
            }],
        )
        .unwrap();
        let mime = render_mime(&config(), &message).unwrap();
        assert!(mime.contains("Message-ID:"));
        assert!(mime.contains("report.pdf"));
        assert!(mime.contains("hengshu-mixed-"));
        assert_eq!(
            message.message_id,
            stable_message_id("run-1", "a@example.com", "mail.example.com")
        );
    }
    #[test]
    fn smtp_rejects_attachment_budget_and_unsafe_filename() {
        assert!(build_message(
            &config(),
            "r",
            "a@example.com",
            "s",
            "t",
            None,
            vec![SmtpAttachment {
                filename: "../secret".into(),
                content_type: "text/plain".into(),
                bytes: vec![1]
            }]
        )
        .is_err());
        assert!(build_message(
            &config(),
            "r",
            "a@example.com",
            "s",
            "t",
            None,
            vec![SmtpAttachment {
                filename: "x".into(),
                content_type: "text/plain".into(),
                bytes: vec![0; 101]
            }]
        )
        .is_err());
    }

    /// 本机回环 SMTP 夹具：前 `temporary_failures` 个连接返回 421，其余走完整会话。
    fn loopback_smtp(
        temporary_failures: usize,
        permanent: bool,
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut connection = 0_usize;
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut transcript = String::new();
                stream.write_all(b"220 fixture\r\n").unwrap();
                if connection < temporary_failures {
                    connection += 1;
                    let mut line = String::new();
                    reader.read_line(&mut line).ok();
                    stream.write_all("421 稍后再试\r\n".as_bytes()).ok();
                    sender.send(format!("失败连接\n{line}")).ok();
                    continue;
                }
                connection += 1;
                let mut challenges = 0_usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    transcript.push_str(&line);
                    let upper = line.trim_end().to_ascii_uppercase();
                    let reply: &str = if upper.starts_with("EHLO") {
                        "250-fixture\r\n250 OK\r\n"
                    } else if upper == "AUTH LOGIN" {
                        "334 VXNlcm5hbWU6\r\n"
                    } else if upper.starts_with("MAIL FROM") {
                        if permanent {
                            "550 拒绝发件人\r\n"
                        } else {
                            "250 OK\r\n"
                        }
                    } else if upper.starts_with("RCPT TO") {
                        "250 OK\r\n"
                    } else if upper == "DATA" {
                        stream.write_all("354 继续\r\n".as_bytes()).unwrap();
                        loop {
                            let mut body = String::new();
                            if reader.read_line(&mut body).unwrap_or(0) == 0 || body == ".\r\n" {
                                break;
                            }
                            transcript.push_str(&body);
                        }
                        "250 已入队\r\n"
                    } else if upper == "QUIT" {
                        stream.write_all("221 再见\r\n".as_bytes()).ok();
                        break;
                    } else if challenges < 2 {
                        challenges += 1;
                        if challenges == 1 {
                            "334 UGFzc3dvcmQ6\r\n"
                        } else {
                            "235 认证通过\r\n"
                        }
                    } else {
                        "500 未知命令\r\n"
                    };
                    stream.write_all(reply.as_bytes()).unwrap();
                    stream.flush().unwrap();
                }
                sender.send(transcript).ok();
            }
        });
        (port, receiver)
    }

    fn loopback_config(port: u16) -> SmtpConfig {
        SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            tls: TlsMode::PlaintextLoopback,
            from: "sender@example.com".into(),
            timeout_ms: 5000,
            max_attachment_bytes: 1024,
        }
    }

    fn loopback_permissions() -> PermissionSet {
        let mut permissions = PermissionSet::for_source(std::path::Path::new("."));
        permissions.network_hosts = vec!["127.0.0.1".into()];
        permissions
    }

    fn smtp_policy() -> EndpointPolicy {
        EndpointPolicy {
            allowed: std::collections::BTreeSet::from([
                Endpoint::SmtpBody,
                Endpoint::SmtpAttachment,
            ]),
            sensitive_allowed: std::collections::BTreeSet::new(),
        }
    }

    #[test]
    fn smtp_delivers_over_loopback_with_auth_and_recipient_isolation() {
        let (port, transcripts) = loopback_smtp(0, false);
        let config = loopback_config(port);
        let messages = ["a@example.com", "b@example.com"]
            .into_iter()
            .map(|recipient| {
                build_message(
                    &config,
                    "run-1",
                    recipient,
                    "结果",
                    "正文\r\n.开头行",
                    None,
                    vec![SmtpAttachment {
                        filename: "report.pdf".into(),
                        content_type: "application/pdf".into(),
                        bytes: vec![1, 2, 3],
                    }],
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let credentials = SmtpCredentials {
            username: "sender".into(),
            secret: "秘密口令".into(),
        };
        let results = send_batch(
            &config,
            &messages,
            Some(&credentials),
            &loopback_permissions(),
            &smtp_policy(),
            &Sensitivity::default(),
            0,
        );
        assert_eq!(results.len(), 2);
        for (index, result) in results.iter().enumerate() {
            let delivery = result.as_ref().unwrap();
            assert_eq!(delivery.recipient, messages[index].to[0]);
            assert_eq!(delivery.attempts, 1);
        }
        for message in &messages {
            let transcript = transcripts
                .recv_timeout(Duration::from_secs(5))
                .expect("夹具应记录会话");
            assert!(transcript.contains("AUTH LOGIN"), "{transcript}");
            assert!(!transcript.contains("秘密口令"), "{transcript}");
            assert_eq!(transcript.matches("RCPT TO:<").count(), 1, "{transcript}");
            assert!(transcript.contains(&message.message_id), "{transcript}");
            assert!(transcript.contains("report.pdf"), "{transcript}");
            assert!(
                transcript.contains("Content-Transfer-Encoding: base64"),
                "{transcript}"
            );
        }
    }

    #[test]
    fn smtp_retries_temporary_failures_but_not_permanent_ones() {
        let (port, _transcripts) = loopback_smtp(1, false);
        let config = loopback_config(port);
        let message =
            build_message(&config, "run-2", "a@example.com", "s", "正文", None, vec![]).unwrap();
        let delivery = send_message(
            &config,
            &message,
            None,
            &loopback_permissions(),
            &smtp_policy(),
            &Sensitivity::default(),
            1,
        )
        .unwrap();
        assert_eq!(delivery.attempts, 2);

        let (port, _transcripts) = loopback_smtp(0, true);
        let permanent = loopback_config(port);
        let error = send_message(
            &permanent,
            &message,
            None,
            &loopback_permissions(),
            &smtp_policy(),
            &Sensitivity::default(),
            3,
        )
        .unwrap_err();
        assert!(error.contains("550"), "{error}");
    }

    #[test]
    fn smtp_requires_endpoint_authority_and_refuses_remote_plaintext() {
        let (port, _transcripts) = loopback_smtp(0, false);
        let config = loopback_config(port);
        let message = build_message(
            &config,
            "run-3",
            "a@example.com",
            "s",
            "正文",
            None,
            vec![SmtpAttachment {
                filename: "report.pdf".into(),
                content_type: "application/pdf".into(),
                bytes: vec![1],
            }],
        )
        .unwrap();
        let body_only = EndpointPolicy {
            allowed: std::collections::BTreeSet::from([Endpoint::SmtpBody]),
            sensitive_allowed: std::collections::BTreeSet::new(),
        };
        assert!(send_message(
            &config,
            &message,
            None,
            &loopback_permissions(),
            &body_only,
            &Sensitivity::default(),
            0
        )
        .is_err());
        assert!(send_message(
            &config,
            &message,
            None,
            &PermissionSet::restricted_for_source(std::path::Path::new(".")),
            &smtp_policy(),
            &Sensitivity::default(),
            0
        )
        .is_err());
        let mut remote = loopback_config(port);
        remote.host = "mail.example.com".into();
        assert!(remote.validate().is_err());
    }
}
