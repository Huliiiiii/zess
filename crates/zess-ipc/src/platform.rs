use std::{
    ffi::OsString,
    fs::OpenOptions,
    io,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result};

use crate::{LocalIpc, SocketPath, Stream};

#[cfg(unix)]
#[path = "unix/daemon.rs"]
mod backend;
#[cfg(windows)]
#[path = "windows/daemon.rs"]
mod backend;

pub use backend::detach;

pub struct DaemonSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub log_path: PathBuf,
}

pub struct DaemonHandle(Child);

impl DaemonHandle {
    pub fn id(&self) -> u32 {
        self.0.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.0.try_wait()
    }

    pub fn reap(mut self) -> io::Result<()> {
        std::thread::Builder::new()
            .name("zess-daemon-reaper".into())
            .spawn(move || {
                let _ = self.0.wait();
            })
            .map(drop)
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        if self.0.try_wait()?.is_none() {
            if let Err(error) = self.0.kill()
                && self.0.try_wait()?.is_none()
            {
                return Err(error);
            }
            self.0.wait()?;
        }
        Ok(())
    }
}

pub trait ProcessWait: Send {
    fn wait(&self, timeout: Duration) -> io::Result<bool>;
}

pub trait ProcessStop: ProcessWait {
    fn is_peer(&self, stream: &Stream) -> Result<bool>;
    /// Terminates the process and waits for it to exit.
    fn terminate(&self) -> Result<()>;
}

pub trait DaemonControl: Send + Sync {
    fn connect_for_stop(
        &self,
        ipc: &dyn LocalIpc,
        endpoint: &SocketPath,
    ) -> Result<(Stream, Box<dyn ProcessStop>)>;
    fn spawn(&self, specification: &DaemonSpec) -> Result<DaemonHandle>;
    fn watch(&self, stream: &Stream) -> Result<Box<dyn ProcessWait>>;
}

#[cfg(unix)]
pub struct UnixDaemonControl;
#[cfg(windows)]
pub struct WindowsDaemonControl;

#[cfg(unix)]
pub use UnixDaemonControl as NativeDaemonControl;
#[cfg(windows)]
pub use WindowsDaemonControl as NativeDaemonControl;

impl DaemonControl for NativeDaemonControl {
    fn watch(&self, stream: &Stream) -> Result<Box<dyn ProcessWait>> {
        backend::watch(stream)
    }

    fn connect_for_stop(
        &self,
        ipc: &dyn LocalIpc,
        endpoint: &SocketPath,
    ) -> Result<(Stream, Box<dyn ProcessStop>)> {
        backend::connect_for_stop(ipc, endpoint)
    }

    fn spawn(&self, specification: &DaemonSpec) -> Result<DaemonHandle> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&specification.log_path)
            .with_context(|| format!("open {}", specification.log_path.display()))?;
        let mut command = Command::new(&specification.program);
        command
            .args(&specification.args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        if let Some(cwd) = &specification.cwd {
            command.current_dir(cwd);
        }
        detach(&mut command)?;
        Ok(DaemonHandle(
            command.spawn().context("start background server")?,
        ))
    }
}
