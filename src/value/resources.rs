//! 运行时资源持有类型，按资源域分组。
//!
//! 这些类型持有进程外资源（连接、句柄、线程通道），生命周期由 `Value` 的引用计数决定。
//! 它们从 `value/mod.rs` 分出来，是为了让值枚举不再兼任所有运行时资源的注册表。

use super::PortableValue;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Read;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::Arc;

// —— HTTP ——

pub struct HttpResponseValue {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub set_cookies: Vec<String>,
    pub body: RefCell<HttpBody>,
    pub buffer_limit: usize,
}

pub enum HttpBody {
    Buffered(Vec<u8>),
    Reader(Box<dyn Read>),
}

// —— 数据库 ——

pub struct DatabaseValue {
    pub driver: String,
    pub connection: RefCell<rusqlite::Connection>,
}

// —— 网络 ——

pub enum SocketValue {
    TcpStream(TcpStream),
    TcpListener(TcpListener),
    Udp(UdpSocket),
}

// —— 并发：任务与通道 ——

#[derive(Clone, Debug)]
pub struct TaskValue {
    pub(crate) receiver:
        crossbeam_channel::Receiver<Result<PortableValue, crate::runtime::RuntimeError>>,
    pub(crate) result: RefCell<Option<Result<PortableValue, crate::runtime::RuntimeError>>>,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) status: Arc<AtomicU8>,
}

pub struct ChannelValue {
    pub sender: RefCell<Option<crossbeam_channel::Sender<PortableValue>>>,
    pub receiver: crossbeam_channel::Receiver<PortableValue>,
}
