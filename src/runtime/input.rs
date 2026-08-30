use crossbeam_channel::{bounded, Receiver, Sender};
use std::io;
use std::sync::OnceLock;
use std::time::Duration;

type InputResult = Result<String, String>;
type InputRequest = Sender<InputResult>;

static STDIN_REQUESTS: OnceLock<Result<Sender<InputRequest>, String>> = OnceLock::new();

fn requests() -> Result<&'static Sender<InputRequest>, String> {
    STDIN_REQUESTS
        .get_or_init(|| {
            let (sender, receiver) = crossbeam_channel::unbounded::<InputRequest>();
            std::thread::Builder::new()
                .name("hengshu-stdin".into())
                .spawn(move || input_worker(receiver))
                .map_err(|error| format!("创建标准输入工作线程失败：{error}"))?;
            Ok(sender)
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn input_worker(receiver: Receiver<InputRequest>) {
    for response in receiver {
        let mut input = String::new();
        let result = io::stdin()
            .read_line(&mut input)
            .map(|_| input)
            .map_err(|error| error.to_string());
        let _ = response.send(result);
    }
}

pub(super) enum ReadLine {
    Line(String),
    TimedOut,
}

pub(super) fn read_line(timeout: Option<Duration>) -> Result<ReadLine, String> {
    let (response, result) = bounded(1);
    requests()?
        .send(response)
        .map_err(|_| "标准输入工作线程不可用".to_string())?;
    match timeout {
        Some(timeout) => match result.recv_timeout(timeout) {
            Ok(line) => line.map(ReadLine::Line),
            Err(_) => Ok(ReadLine::TimedOut),
        },
        None => result
            .recv()
            .map_err(|_| "标准输入工作线程不可用".to_string())?
            .map(ReadLine::Line),
    }
}

#[cfg(test)]
pub(super) fn uses_single_worker() -> bool {
    match (requests(), requests()) {
        (Ok(left), Ok(right)) => Sender::same_channel(left, right),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn timed_input_reuses_one_process_worker() {
        assert!(super::uses_single_worker());
    }
}
