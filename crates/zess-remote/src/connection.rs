use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::Context;
use zess_platform::{CancellationToken, OwnedProcess};

use crate::{ProgressReporter, RemoteProgress};

pub struct SshConnection {
    process: OwnedProcess,
    pub(crate) session_id: Option<String>,
    outgoing: Option<SyncSender<Outgoing>>,
    writer: Option<JoinHandle<()>>,
    flushing: Option<Receiver<io::Result<()>>>,
    received: Option<Receiver<io::Result<Vec<u8>>>>,
    pending: VecDeque<u8>,
    reader: Option<JoinHandle<()>>,
    diagnostics: Option<JoinHandle<()>>,
    nonblocking: AtomicBool,
    eof: bool,
    cancellation: Option<CancellationToken>,
}

enum Outgoing {
    Bytes(Vec<u8>),
    Flush(mpsc::Sender<io::Result<()>>),
}

impl SshConnection {
    pub fn new(process: OwnedProcess) -> anyhow::Result<Self> {
        Self::with_cancellation(process, None, None)
    }

    pub(crate) fn with_cancellation(
        mut process: OwnedProcess,
        cancellation: Option<CancellationToken>,
        progress: Option<Arc<dyn ProgressReporter>>,
    ) -> anyhow::Result<Self> {
        let mut stdin = process.take_stdin().context("SSH stdin unavailable")?;
        let mut stdout = process.take_stdout().context("SSH stdout unavailable")?;
        let mut stderr = process.take_stderr().context("SSH stderr unavailable")?;
        let (sender, received) = mpsc::sync_channel(16);
        let (outgoing, writes) = mpsc::sync_channel(16);
        let writer = thread::Builder::new()
            .name("zess-ssh-writer".into())
            .spawn(move || {
                while let Ok(message) = writes.recv() {
                    let result = match message {
                        Outgoing::Bytes(bytes) => stdin.write_all(&bytes),
                        Outgoing::Flush(response) => {
                            let result = stdin.flush();
                            let failed = result.is_err();
                            let _ = response.send(result);
                            if failed {
                                break;
                            }
                            continue;
                        }
                    };
                    if result.is_err() {
                        break;
                    }
                }
            })?;
        let reader = match thread::Builder::new()
            .name("zess-ssh-reader".into())
            .spawn(move || {
                loop {
                    let mut bytes = vec![0; 16384];
                    let result = match stdout.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(size) => {
                            bytes.truncate(size);
                            Ok(bytes)
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => Err(error),
                    };
                    let failed = result.is_err();
                    if sender.send(result).is_err() || failed {
                        break;
                    }
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                drop(outgoing);
                let _ = process.kill();
                let _ = writer.join();
                return Err(error.into());
            }
        };
        let diagnostics = match thread::Builder::new()
            .name("zess-ssh-diagnostics".into())
            .spawn(move || {
                if let Some(progress) = progress {
                    let mut bytes = [0; 1024];
                    while let Ok(count) = stderr.read(&mut bytes) {
                        if count == 0 {
                            break;
                        }
                        progress.report(RemoteProgress::Diagnostic(
                            String::from_utf8_lossy(&bytes[..count]).into_owned(),
                        ));
                    }
                } else {
                    let _ = io::copy(&mut stderr, &mut io::stderr());
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                drop(received);
                drop(outgoing);
                let _ = process.kill();
                let _ = reader.join();
                let _ = writer.join();
                return Err(error.into());
            }
        };
        Ok(Self {
            process,
            session_id: None,
            outgoing: Some(outgoing),
            writer: Some(writer),
            flushing: None,
            received: Some(received),
            pending: VecDeque::new(),
            reader: Some(reader),
            diagnostics: Some(diagnostics),
            nonblocking: AtomicBool::new(false),
            eof: false,
            cancellation,
        })
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> io::Result<bool> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.check_cancellation()?;
            if let Some(status) = self.process.try_wait()? {
                if status.success() {
                    return Ok(true);
                }
                return Err(io::Error::other(format!(
                    "SSH process exited with {status}"
                )));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(
                Duration::from_millis(10)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        }
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn release_console(&mut self) -> io::Result<()> {
        self.process.release_console()
    }

    fn check_cancellation(&self) -> io::Result<()> {
        if let Some(cancellation) = &self.cancellation {
            cancellation
                .check()
                .map_err(|error| io::Error::new(io::ErrorKind::ConnectionAborted, error))?;
        }
        Ok(())
    }

    fn send(&self, mut message: Outgoing) -> io::Result<()> {
        let sender = self.outgoing.as_ref().expect("live connection writer");
        loop {
            self.check_cancellation()?;
            match sender.try_send(message) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(_)) if self.nonblocking.load(Ordering::Relaxed) => {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Err(TrySendError::Full(pending)) => {
                    message = pending;
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TrySendError::Disconnected(_)) => return Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
    }
}

impl Read for SshConnection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.check_cancellation()?;
        if self.pending.is_empty() && !self.eof {
            let received = self.received.as_ref().expect("live connection receiver");
            let message = if self.nonblocking.load(Ordering::Relaxed) {
                match received.try_recv() {
                    Ok(message) => Some(message),
                    Err(TryRecvError::Empty) => return Err(io::ErrorKind::WouldBlock.into()),
                    Err(TryRecvError::Disconnected) => None,
                }
            } else {
                loop {
                    self.check_cancellation()?;
                    match received.recv_timeout(Duration::from_millis(25)) {
                        Ok(message) => break Some(message),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break None,
                    }
                }
            };
            if let Some(message) = message {
                self.pending.extend(message?);
            } else {
                self.eof = true;
            }
        }
        let size = buffer.len().min(self.pending.len());
        for byte in &mut buffer[..size] {
            *byte = self.pending.pop_front().expect("pending byte");
        }
        if size == 0
            && self.eof
            && let Some(status) = self.process.try_wait()?
            && !status.success()
        {
            return Err(io::Error::other(format!(
                "SSH process exited with {status}"
            )));
        }
        Ok(size)
    }
}

impl Write for SshConnection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let size = bytes.len().min(16384);
        self.send(Outgoing::Bytes(bytes[..size].to_vec()))?;
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check_cancellation()?;
        if self.flushing.is_none() {
            let (sender, receiver) = mpsc::channel();
            self.send(Outgoing::Flush(sender))?;
            self.flushing = Some(receiver);
        }
        let receiver = self.flushing.as_ref().expect("pending flush");
        let result = if self.nonblocking.load(Ordering::Relaxed) {
            match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return Err(io::ErrorKind::WouldBlock.into()),
                Err(TryRecvError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
            }
        } else {
            loop {
                self.check_cancellation()?;
                match receiver.recv_timeout(Duration::from_millis(25)) {
                    Ok(result) => break result,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        break Err(io::ErrorKind::BrokenPipe.into());
                    }
                }
            }
        };
        self.flushing = None;
        result
    }
}

impl Drop for SshConnection {
    fn drop(&mut self) {
        self.received.take();
        self.outgoing.take();
        let _ = self.process.kill();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(diagnostics) = self.diagnostics.take() {
            let _ = diagnostics.join();
        }
    }
}
