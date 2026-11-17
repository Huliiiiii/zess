use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde::{Serialize, de::DeserializeOwned};
use zess_platform::{Cancellation, CancellationToken, ConsoleMode, ProcessRunner, ProcessSpec};
use zess_protocol::{ClientMessage, ServerMessage, check_version};

use crate::{
    AskpassConfig, ProgressReporter, ProxyHandshake, RemoteProgress, RemoteStage, ServerBootstrap,
    SshConnection,
};

#[derive(Clone)]
pub struct SshTarget {
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<PathBuf>,
    pub config_file: Option<PathBuf>,
    pub jump_host: Option<String>,
    pub cancellation: Option<CancellationToken>,
    pub progress: Option<Arc<dyn ProgressReporter>>,
    pub askpass: Option<AskpassConfig>,
}

impl SshTarget {
    pub fn new(host: String) -> Self {
        Self {
            host,
            port: None,
            user: None,
            identity_file: None,
            config_file: None,
            jump_host: None,
            cancellation: None,
            progress: None,
            askpass: None,
        }
    }

    pub fn command(&self, remote_command: &str) -> anyhow::Result<ProcessSpec> {
        if self.host.is_empty()
            || self.host.starts_with('-')
            || self.host.chars().any(char::is_whitespace)
            || self.host.contains('\0')
        {
            bail!("SSH host must be a host name or SSH config alias");
        }
        let mut spec = ProcessSpec::new("ssh");
        spec.args(["-T", "-o", "ClearAllForwardings=yes"]);
        if let Some(port) = self.port {
            spec.args([OsString::from("-p"), port.to_string().into()]);
        }
        if let Some(user) = &self.user {
            spec.arg("-l").arg(user);
        }
        if let Some(identity) = &self.identity_file {
            spec.arg("-i").arg(identity);
        }
        if let Some(config) = &self.config_file {
            spec.arg("-F").arg(config);
        }
        if let Some(jump) = &self.jump_host {
            spec.arg("-J").arg(jump);
        }
        if let Some(askpass) = &self.askpass {
            spec.env("SSH_ASKPASS", &askpass.program)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env("ZESS_SSH_ASKPASS_ENDPOINT", askpass.endpoint.as_path());
            spec.console_mode = ConsoleMode::Hidden;
        } else {
            spec.console_mode = ConsoleMode::Inherit;
        }
        spec.arg("--").arg(&self.host).arg(remote_command);
        Ok(spec)
    }

    pub(crate) fn report(&self, event: RemoteProgress) {
        if let Some(progress) = &self.progress {
            progress.report(event);
        }
    }

    pub(crate) fn check_cancellation(&self) -> anyhow::Result<()> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        Ok(())
    }
}

impl Cancellation for SshTarget {
    fn check(&self) -> anyhow::Result<()> {
        self.check_cancellation()
    }
}

pub trait Connector {
    type Connection;
    fn connect(&mut self) -> anyhow::Result<Self::Connection>;
}

pub struct SshConnector<R, Q> {
    pub target: SshTarget,
    pub bootstrap: Arc<dyn ServerBootstrap>,
    pub runner: R,
    pub request: Q,
}

impl<R: ProcessRunner, Q: Serialize> Connector for SshConnector<R, Q> {
    type Connection = SshConnection;

    fn connect(&mut self) -> anyhow::Result<Self::Connection> {
        self.target.check_cancellation()?;
        let command = self.bootstrap.prepare(&self.target, &self.runner)?;
        self.target
            .report(RemoteProgress::Stage(RemoteStage::Connecting));
        let spec = self.target.command(&command)?;
        let mut connection = SshConnection::with_cancellation(
            self.runner.spawn(&spec)?,
            self.target.cancellation.clone(),
            self.target.progress.clone(),
        )?;
        write_initial_request(
            &mut connection,
            &ClientMessage::Hello {
                version: zess_build::version().into(),
            },
            &self.target,
        )?;
        match read_initial::<ServerMessage>(&mut connection, &self.target)? {
            ServerMessage::Hello { version } => check_version(zess_build::version(), &version)?,
            _ => bail!("remote proxy did not send hello"),
        }
        write_initial_request(&mut connection, &self.request, &self.target)?;
        connection.session_id = match read_initial(&mut connection, &self.target)? {
            ProxyHandshake::Ready { session_id } => session_id,
            ProxyHandshake::Failed { message } => bail!("remote proxy: {message}"),
        };
        connection.set_nonblocking(false)?;
        self.target.check_cancellation()?;
        connection.release_console()?;
        self.target
            .report(RemoteProgress::Stage(RemoteStage::Ready));
        Ok(connection)
    }
}

fn write_initial_request(
    connection: &mut SshConnection,
    request: &impl Serialize,
    target: &SshTarget,
) -> anyhow::Result<()> {
    let deadline = Deadline::new(target);
    let bytes = zess_ipc::framing::encode(request)?;
    anyhow::ensure!(
        bytes.len() <= 1024 * 1024,
        "remote proxy request exceeds limit"
    );
    connection.set_nonblocking(true)?;
    let mut written = 0;
    while written < bytes.len() {
        deadline.check()?;
        match connection.write(&bytes[written..]) {
            Ok(0) => bail!("remote proxy closed before receiving its initial request"),
            Ok(size) => written += size,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("send remote proxy request"),
        }
    }
    loop {
        deadline.check()?;
        match connection.flush() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("flush remote proxy request"),
        }
    }
}

fn read_initial<T: DeserializeOwned>(
    connection: &mut SshConnection,
    target: &SshTarget,
) -> anyhow::Result<T> {
    connection.set_nonblocking(true)?;
    let deadline = Deadline::new(target);
    let mut bytes = Vec::new();
    loop {
        deadline.check()?;
        let mut byte = [0];
        match connection.read(&mut byte) {
            Ok(0) => bail!("remote proxy closed during handshake"),
            Ok(_) => {
                bytes.push(byte[0]);
                anyhow::ensure!(
                    bytes.len() <= 1024 * 1024,
                    "remote proxy handshake exceeds limit"
                );
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(error).context("wait for remote proxy authentication and readiness");
            }
        }
    }
    serde_json::from_slice(&bytes).context("decode remote proxy handshake")
}

pub(crate) struct Deadline<'a> {
    expires: Instant,
    cancellation: Option<&'a CancellationToken>,
}

impl<'a> Deadline<'a> {
    pub(crate) fn new(target: &'a SshTarget) -> Self {
        Self {
            expires: Instant::now() + Duration::from_secs(120),
            cancellation: target.cancellation.as_ref(),
        }
    }
}

impl Cancellation for Deadline<'_> {
    fn check(&self) -> anyhow::Result<()> {
        if let Some(cancellation) = self.cancellation {
            cancellation.check()?;
        }
        anyhow::ensure!(Instant::now() < self.expires, "SSH connection timed out");
        Ok(())
    }
}
