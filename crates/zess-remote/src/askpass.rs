use std::{
    io::{self, BufReader, Read},
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zess_ipc::{LocalIpc, SocketPath, Stream, framing};
use zess_platform::{Cancellation, CancellationToken};

#[cfg(test)]
mod tests;

const MESSAGE_LIMIT: usize = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone)]
pub struct AskpassConfig {
    pub program: PathBuf,
    pub endpoint: SocketPath,
}

#[derive(Serialize, Deserialize)]
pub struct SshPrompt {
    pub message: String,
    pub kind: SshPromptKind,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum SshPromptKind {
    Secret,
    Visible,
    Confirmation,
    Information,
}

pub trait SshPromptHandler: Send + Sync {
    fn authenticate(
        &self,
        prompt: SshPrompt,
        cancellation: &dyn Cancellation,
    ) -> Result<Option<String>>;
}

struct PromptCancellation {
    task: CancellationToken,
    broker: CancellationToken,
    request: CancellationToken,
}

impl Cancellation for PromptCancellation {
    fn check(&self) -> Result<()> {
        self.task.check()?;
        self.broker.check()?;
        self.request.check()
    }
}

pub struct AskpassBroker {
    config: AskpassConfig,
    shutdown: CancellationToken,
    worker: Option<thread::JoinHandle<()>>,
}

impl AskpassBroker {
    pub fn start(
        ipc: Arc<dyn LocalIpc>,
        program: PathBuf,
        cancellation: CancellationToken,
        handler: Arc<dyn SshPromptHandler>,
    ) -> Result<Self> {
        let directory = tempfile::Builder::new().prefix("zess-askpass-").tempdir()?;
        let endpoint = SocketPath::private(directory.path().join("askpass.sock"));
        ipc.prepare(&endpoint)?;
        let listener = ipc.bind(&endpoint)?;
        listener.set_nonblocking(true)?;
        let config = AskpassConfig { program, endpoint };
        let shutdown = CancellationToken::new();
        let broker_shutdown = shutdown.clone();
        let worker = thread::Builder::new()
            .name("zess-askpass-broker".into())
            .spawn(move || {
                let _directory = directory;
                let mut requests: Vec<thread::JoinHandle<()>> = Vec::new();
                while broker_shutdown.check().is_ok() && cancellation.check().is_ok() {
                    let mut index = 0;
                    while index < requests.len() {
                        if requests[index].is_finished() {
                            let _ = requests.swap_remove(index).join();
                        } else {
                            index += 1;
                        }
                    }
                    match listener.accept() {
                        Ok(stream) => {
                            let handler = Arc::clone(&handler);
                            let cancellation = PromptCancellation {
                                task: cancellation.clone(),
                                broker: broker_shutdown.clone(),
                                request: CancellationToken::new(),
                            };
                            if let Ok(request) = thread::Builder::new()
                                .name("zess-askpass-request".into())
                                .spawn(move || serve_prompt(stream, handler, cancellation))
                            {
                                requests.push(request);
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) =>
                        {
                            thread::sleep(POLL_INTERVAL);
                        }
                        Err(_) => break,
                    }
                }
                broker_shutdown.cancel();
                for request in requests {
                    let _ = request.join();
                }
            })?;
        Ok(Self {
            config,
            shutdown,
            worker: Some(worker),
        })
    }

    pub fn config(&self) -> &AskpassConfig {
        &self.config
    }
}

impl Drop for AskpassBroker {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_prompt(
    mut stream: Stream,
    handler: Arc<dyn SshPromptHandler>,
    cancellation: PromptCancellation,
) {
    let result = (|| -> Result<()> {
        stream.set_nonblocking(true)?;
        let mut decoder = framing::Decoder::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let prompt = loop {
            cancellation.check()?;
            anyhow::ensure!(Instant::now() < deadline, "askpass prompt timed out");
            let mut bytes = [0; 4096];
            match stream.read(&mut bytes) {
                Ok(0) => anyhow::bail!("missing askpass prompt"),
                Ok(count) => {
                    decoder.push(&bytes[..count], MESSAGE_LIMIT)?;
                    if let Some(prompt) = decoder.decode_next()? {
                        break prompt;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    thread::sleep(POLL_INTERVAL)
                }
                Err(error) => return Err(error.into()),
            }
        };
        let request = cancellation.request.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let handler_cancellation = PromptCancellation {
            task: cancellation.task.clone(),
            broker: cancellation.broker.clone(),
            request: request.clone(),
        };
        let ui = thread::Builder::new()
            .name("zess-askpass-ui".into())
            .spawn(move || {
                let _ = sender.send(handler.authenticate(prompt, &handler_cancellation));
            })?;
        let result = (|| -> Result<()> {
            loop {
                if cancellation.check().is_err() {
                    break;
                }
                match receiver.try_recv() {
                    Ok(Ok(response)) => {
                        stream.set_nonblocking(false)?;
                        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                        framing::write(&mut stream, &response)?;
                        break;
                    }
                    Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match stream.read(&mut [0; 1]) {
                    Ok(_) => break,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
                thread::sleep(POLL_INTERVAL);
            }
            Ok(())
        })();
        request.cancel();
        let _ = ui.join();
        result
    })();
    let _ = result;
}

pub fn run_askpass(
    ipc: &dyn LocalIpc,
    endpoint: &SocketPath,
    prompt: String,
    hint: Option<&str>,
) -> Result<Option<String>> {
    let kind = match hint {
        Some("confirm") => SshPromptKind::Confirmation,
        Some("none") => SshPromptKind::Information,
        _ => SshPromptKind::Secret,
    };
    let endpoint = SocketPath::private(endpoint.as_path().to_path_buf());
    let mut stream = ipc
        .connect(&endpoint)
        .context("connect to askpass broker")?;
    framing::write(
        &mut stream,
        &SshPrompt {
            message: prompt,
            kind,
        },
    )?;
    let mut reader = BufReader::new(stream);
    let bytes = framing::read(&mut reader, MESSAGE_LIMIT)?.context("askpass broker closed")?;
    let response: Option<String> = framing::decode(&bytes)?;
    Ok(if matches!(kind, SshPromptKind::Confirmation) {
        response
            .filter(|answer| answer.eq_ignore_ascii_case("yes"))
            .map(|_| String::new())
    } else {
        response
    })
}
