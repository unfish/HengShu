use super::error;
use crate::security::PermissionSet;
use serde_json::{json, Value as JsonValue};
use std::net::TcpStream;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

type CdpSocket = WebSocket<MaybeTlsStream<TcpStream>>;

pub(super) struct Cdp {
    socket: CdpSocket,
    next_id: u64,
    permissions: PermissionSet,
}

impl Cdp {
    pub(super) fn connect(
        url: &str,
        timeout: Duration,
        permissions: PermissionSet,
    ) -> Result<Self, String> {
        let (mut socket, _) =
            connect(url).map_err(|value| error(format!("连接浏览器调试协议失败：{value}")))?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(timeout))
                .map_err(|value| error(value.to_string()))?;
            stream
                .set_write_timeout(Some(timeout))
                .map_err(|value| error(value.to_string()))?;
        }
        Ok(Self {
            socket,
            next_id: 1,
            permissions,
        })
    }

    pub(super) fn command(&mut self, method: &str, params: JsonValue) -> Result<JsonValue, String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.socket
            .send(Message::text(
                json!({"id": id, "method": method, "params": params}).to_string(),
            ))
            .map_err(|value| error(format!("发送浏览器命令失败：{value}")))?;
        loop {
            let message = self
                .socket
                .read()
                .map_err(|value| error(format!("读取浏览器响应失败：{value}")))?;
            let text = match message {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => String::from_utf8(bytes.to_vec())
                    .map_err(|value| error(format!("浏览器响应不是 UTF-8：{value}")))?,
                Message::Ping(payload) => {
                    self.socket
                        .send(Message::Pong(payload))
                        .map_err(|value| error(value.to_string()))?;
                    continue;
                }
                Message::Close(_) => return Err(error("浏览器调试连接已关闭")),
                Message::Pong(_) | Message::Frame(_) => continue,
            };
            let value: JsonValue = serde_json::from_str(&text)
                .map_err(|value| error(format!("解析浏览器响应失败：{value}")))?;
            if value.get("method").and_then(JsonValue::as_str) == Some("Fetch.requestPaused") {
                self.handle_paused_request(&value)?;
                continue;
            }
            if value.get("id").and_then(JsonValue::as_u64) != Some(id) {
                continue;
            }
            if let Some(command_error) = value.get("error") {
                return Err(error(format!("浏览器命令“{method}”失败：{command_error}")));
            }
            return Ok(value.get("result").cloned().unwrap_or(JsonValue::Null));
        }
    }

    pub(super) fn evaluate(&mut self, expression: &str) -> Result<JsonValue, String> {
        let result = self.command(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true
            }),
        )?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(error(format!("浏览器脚本执行失败：{exception}")));
        }
        Ok(result
            .get("result")
            .and_then(|value| value.get("value"))
            .cloned()
            .unwrap_or(JsonValue::Null))
    }

    fn handle_paused_request(&mut self, event: &JsonValue) -> Result<(), String> {
        let params = event
            .get("params")
            .ok_or_else(|| error("浏览器暂停请求事件缺少参数"))?;
        let request_id = params
            .get("requestId")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| error("浏览器暂停请求事件缺少请求编号"))?;
        let url = params
            .get("request")
            .and_then(|value| value.get("url"))
            .and_then(JsonValue::as_str)
            .ok_or_else(|| error("浏览器暂停请求事件缺少网址"))?;
        let permission = authorize_browser_url(&self.permissions, url);
        let (method, params) = match permission {
            Ok(()) => ("Fetch.continueRequest", json!({"requestId": request_id})),
            Err(_) => (
                "Fetch.failRequest",
                json!({"requestId": request_id, "errorReason": "BlockedByClient"}),
            ),
        };
        self.send_without_wait(method, params)?;
        permission
    }

    fn send_without_wait(&mut self, method: &str, params: JsonValue) -> Result<(), String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.socket
            .send(Message::text(
                json!({"id": id, "method": method, "params": params}).to_string(),
            ))
            .map_err(|value| error(format!("发送浏览器请求控制命令失败：{value}")))
    }
}

pub(super) fn authorize_browser_url(
    permissions: &PermissionSet,
    address: &str,
) -> Result<(), String> {
    let scheme = url::Url::parse(address)
        .ok()
        .map(|value| value.scheme().to_string());
    match scheme.as_deref() {
        Some("http" | "https" | "ws" | "wss") => permissions.check_network(address),
        Some("about" | "blob" | "data" | "devtools") => Ok(()),
        _ => Err(error(format!("浏览器不允许加载网址“{address}”"))),
    }
}
