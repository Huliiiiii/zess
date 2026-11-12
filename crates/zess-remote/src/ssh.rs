use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde::Serialize;
use zess_platform::{Cancellation, CancellationToken, ConsoleMode, ProcessRunner, ProcessSpec};

use crate::{Bootstrap, PROXY_PROTOCOL_VERSION, ProxyHandshake, SshConnection};

#[derive(Clone, Debug)]
pub struct SshTarget {
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<PathBuf>,
    pub config_file: Option<PathBuf>,
    pub jump_host: Option<String>,
    pub cancellation: Option<CancellationToken>,
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
        spec.console_mode = ConsoleMode::Inherit;
        spec.arg("--").arg(&self.host).arg(remote_command);
        Ok(spec)
    }

    pub(crate) fn check_cancellation(&self) -> anyhow::Result<()> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        Ok(())
    }
}

pub trait Connector {
    type Connection;
    fn connect(&mut self) -> anyhow::Result<Self::Connection>;
}

pub struct SshConnector<B, R, Q> {
    pub target: SshTarget,
    pub bootstrap: B,
    pub runner: R,
    pub request: Q,
}

impl<B: Bootstrap, R: ProcessRunner, Q: Serialize> Connector for SshConnector<B, R, Q> {
    type Connection = SshConnection;

    fn connect(&mut self) -> anyhow::Result<Self::Connection> {
        self.target.check_cancellation()?;
        let server = self.bootstrap.prepare(&self.target)?;
        self.target.check_cancellation()?;
        let spec = self.target.command(server.command())?;
        let mut connection = SshConnection::with_cancellation(
            self.runner.spawn(&spec)?,
            self.target.cancellation.clone(),
        )?;
        write_initial_request(&mut connection, &self.request, &self.target)?;
        wait_ready(&mut connection, &self.target)?;
        self.target.check_cancellation()?;
        connection.release_console()?;
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

fn wait_ready(connection: &mut SshConnection, target: &SshTarget) -> anyhow::Result<()> {
    connection.set_nonblocking(true)?;
    let deadline = Deadline::new(target);
    let mut bytes = Vec::new();
    loop {
        deadline.check()?;
        let mut byte = [0];
        match connection.read(&mut byte) {
            Ok(0) => bail!(
                "remote proxy closed before its ready handshake; upgrade the server to proxy protocol {PROXY_PROTOCOL_VERSION}"
            ),
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
    match serde_json::from_slice(&bytes).context("decode remote proxy ready handshake")? {
        ProxyHandshake::Ready { protocol_version } => anyhow::ensure!(
            protocol_version == PROXY_PROTOCOL_VERSION,
            "incompatible remote proxy handshake version"
        ),
        ProxyHandshake::Failed { message } => bail!("remote proxy: {message}"),
    }
    connection.set_nonblocking(false)?;
    Ok(())
}

struct Deadline<'a> {
    expires: Instant,
    cancellation: Option<&'a CancellationToken>,
}

impl<'a> Deadline<'a> {
    fn new(target: &'a SshTarget) -> Self {
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

pub(crate) fn execute(
    runner: &impl ProcessRunner,
    target: &SshTarget,
    command: &str,
    input: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let mut spec = target.command(command)?;
    spec.output_limit = Some(1024 * 1024);
    let output = runner
        .output(&spec, input, &Deadline::new(target))
        .with_context(|| format!("SSH to {}", target.host))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let error = anyhow::Error::new(SshCommandFailure {
            host: target.host.clone(),
            status: output.status,
            stderr,
        });
        Err(error)
    }
}

#[derive(Debug)]
pub(crate) struct SshCommandFailure {
    host: String,
    pub status: std::process::ExitStatus,
    stderr: String,
}

impl std::fmt::Display for SshCommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "SSH to {} exited with {}: {}",
            self.host, self.status, self.stderr
        )
    }
}

impl std::error::Error for SshCommandFailure {}
