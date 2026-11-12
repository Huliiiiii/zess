use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zess_ipc::platform::{DaemonControl, DaemonSpec, NativeDaemonControl};
use zess_ipc::{LocalIpc, NativeLocalIpc, SocketPath, Stream, framing};
use zess_platform::{CancellationToken, FileSystem, NativeFileSystem, NativePlatform, Platform};
use zess_protocol::{ClientMessage, ServerInstanceId, ServerMessage, SessionId};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub endpoint: String,
    pub session_id: SessionId,
    pub server_instance: ServerInstanceId,
    pub workspace: String,
    pub pid: u32,
    pub attached: bool,
    pub modified: bool,
}

#[derive(Clone)]
pub struct SessionManager {
    filesystem: Arc<dyn FileSystem>,
    ipc: Arc<dyn LocalIpc>,
    daemons: Arc<dyn DaemonControl>,
    host: Arc<dyn Platform>,
    cancellation: Option<CancellationToken>,
}

impl SessionManager {
    pub fn native() -> Self {
        Self::new(
            Arc::new(NativeFileSystem),
            Arc::new(NativeLocalIpc),
            Arc::new(NativeDaemonControl),
            Arc::new(NativePlatform),
        )
    }

    pub fn new(
        filesystem: Arc<dyn FileSystem>,
        ipc: Arc<dyn LocalIpc>,
        daemons: Arc<dyn DaemonControl>,
        host: Arc<dyn Platform>,
    ) -> Self {
        Self {
            filesystem,
            ipc,
            daemons,
            host,
            cancellation: None,
        }
    }

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    fn check_cancellation(&self) -> Result<()> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        Ok(())
    }

    fn request_interval(&self, deadline: Instant) -> Result<Duration> {
        self.check_cancellation()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "server management request timed out",
            )
            .into());
        }
        Ok(remaining.min(Duration::from_millis(50)))
    }

    fn request(&self, endpoint: &SocketPath, message: ClientMessage) -> Result<ServerMessage> {
        self.check_cancellation()?;
        let mut stream = self
            .ipc
            .connect(endpoint)
            .with_context(|| format!("connect to server at {}", endpoint.display()))?;
        self.check_cancellation()?;
        let message = framing::encode(&message)?;
        let mut pending = message.as_slice();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !pending.is_empty() {
            stream.set_write_timeout(Some(self.request_interval(deadline)?))?;
            match stream.write(pending) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "server management connection stopped accepting input",
                    )
                    .into());
                }
                Ok(written) => pending = &pending[written..],
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut response = framing::Decoder::default();
        let mut buffer = [0; 4096];
        loop {
            stream.set_read_timeout(Some(self.request_interval(deadline)?))?;
            match stream.read(&mut buffer) {
                Ok(0) => bail!(
                    "server at {} did not return a management response; close it with :exit, or use server stop with --force to discard the session",
                    endpoint.display()
                ),
                Ok(read) => {
                    response.push(&buffer[..read], 1024 * 1024)?;
                    if let Some(response) = response
                        .decode_next()
                        .context("decode server management response")?
                    {
                        self.check_cancellation()?;
                        return Ok(response);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn inspect(&self, endpoint: &SocketPath) -> Result<SessionInfo> {
        match self.request(endpoint, ClientMessage::Status)? {
            ServerMessage::Status {
                session_id,
                server_instance,
                workspace,
                pid,
                attached,
                modified,
                ..
            } => Ok(SessionInfo {
                id: endpoint
                    .server_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| endpoint.as_path().to_string_lossy().into_owned()),
                endpoint: endpoint.as_path().to_string_lossy().into_owned(),
                session_id,
                server_instance,
                workspace,
                pid,
                attached,
                modified,
            }),
            ServerMessage::Rejected { reason } => bail!("{reason}"),
            _ => bail!("unexpected management response from {}", endpoint.display()),
        }
    }

    pub fn entries(&self) -> Result<Vec<SessionInfo>> {
        self.check_cancellation()?;
        let mut entries = Vec::new();
        for endpoint in self.ipc.discover()? {
            self.check_cancellation()?;
            if let Ok(info) = self.inspect(&endpoint) {
                entries.push(info);
            }
            self.check_cancellation()?;
        }
        self.check_cancellation()?;
        Ok(entries)
    }

    pub fn endpoint_for_workspace(&self, workspace: &Path) -> Result<SocketPath> {
        self.check_cancellation()?;
        self.ipc.for_workspace(workspace)
    }

    pub fn connect(&self, endpoint: &SocketPath) -> Result<Stream> {
        self.check_cancellation()?;
        let stream = self.ipc.connect(endpoint).context("connect session")?;
        self.check_cancellation()?;
        Ok(stream)
    }

    pub fn ensure_for_workspace(
        &self,
        workspace: &Path,
        config: Option<&Path>,
        files: &[PathBuf],
    ) -> Result<SessionInfo> {
        self.check_cancellation()?;
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        let endpoint = self.endpoint_for_workspace(&workspace)?;
        self.ensure(&workspace, &endpoint, config, files)
    }

    pub fn resolve(&self, id: &str) -> Result<SocketPath> {
        self.check_cancellation()?;
        if let Ok(id) = id.parse() {
            return Ok(self.ipc.by_id(id));
        }
        for endpoint in self.ipc.discover()? {
            self.check_cancellation()?;
            if let Ok(info) = self.inspect(&endpoint)
                && (info.session_id.0 == id || info.id == id)
            {
                return Ok(endpoint);
            }
            self.check_cancellation()?;
        }
        self.check_cancellation()?;
        bail!("unknown session: {id}")
    }

    pub fn ensure(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        config: Option<&Path>,
        files: &[PathBuf],
    ) -> Result<SessionInfo> {
        self.check_cancellation()?;
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        if let Ok(info) = self.inspect(endpoint) {
            self.check_workspace(&info, &workspace)?;
            return Ok(info);
        }
        for info in self.entries()? {
            if Path::new(&info.workspace) == workspace {
                return Ok(info);
            }
        }
        self.start(&workspace, endpoint, config, files)
    }

    pub fn start(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        config: Option<&Path>,
        files: &[PathBuf],
    ) -> Result<SessionInfo> {
        self.check_cancellation()?;
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        if let Ok(info) = self.inspect(endpoint) {
            self.check_workspace(&info, &workspace)?;
            return Ok(info);
        }
        self.check_cancellation()?;
        self.ipc.prepare(endpoint)?;
        let mut args = vec![
            "server".into(),
            "--workspace".into(),
            workspace.as_os_str().to_owned(),
            "--socket".into(),
            endpoint.as_path().as_os_str().to_owned(),
        ];
        if let Some(config) = config {
            args.extend(["--config".into(), config.as_os_str().to_owned()]);
        }
        args.push("--".into());
        args.extend(files.iter().map(|path| path.as_os_str().to_owned()));
        let specification = DaemonSpec {
            program: self.host.current_exe()?,
            args,
            cwd: Some(self.host.current_dir()?),
            log_path: self.ipc.log_path(endpoint),
        };
        self.check_cancellation()?;
        let mut server = self
            .daemons
            .spawn(&specification)
            .context("start editor server")?;
        let start = Instant::now();
        loop {
            self.check_cancellation()?;
            if let Ok(info) = self.inspect(endpoint) {
                self.check_workspace(&info, &workspace)?;
                return Ok(info);
            }
            self.check_cancellation()?;
            if let Some(status) = server.try_wait()? {
                bail!(
                    "server exited ({status}); see {}",
                    self.ipc.log_path(endpoint).display()
                );
            }
            if start.elapsed() > Duration::from_secs(20) {
                bail!(
                    "server startup timed out; see {}",
                    self.ipc.log_path(endpoint).display()
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn check_workspace(&self, info: &SessionInfo, workspace: &Path) -> Result<()> {
        self.check_cancellation()?;
        if Path::new(&info.workspace) != workspace {
            bail!(
                "server workspace is {}; refusing to use it in {}",
                info.workspace,
                workspace.display()
            );
        }
        Ok(())
    }

    pub fn stop(&self, endpoint: &SocketPath, force: bool) -> Result<()> {
        let response = self.request(endpoint, ClientMessage::Stop { force });
        self.check_cancellation()?;
        match response {
            Ok(ServerMessage::Exited) => Ok(()),
            Ok(ServerMessage::Rejected { reason }) => bail!("{reason}"),
            Ok(_) => bail!("unexpected management response from {}", endpoint.display()),
            Err(error) if force => self.daemons.stop(endpoint).with_context(|| {
                format!("management request failed ({error:#}); stop socket peer")
            }),
            Err(error) => Err(error),
        }
    }

    fn restart_workspace(
        &self,
        endpoint: &SocketPath,
        workspace: Option<&Path>,
        force: bool,
    ) -> Result<PathBuf> {
        let response = self.inspect(endpoint);
        self.check_cancellation()?;
        match response {
            Ok(info) => {
                let current = self.filesystem.canonicalize(Path::new(&info.workspace)).context("open server workspace")?;
                if let Some(workspace) = workspace
                    && self.filesystem.canonicalize(workspace).context("open workspace directory")? != current
                {
                    bail!("server workspace is {}; refusing to restart it in {}", current.display(), workspace.display());
                }
                Ok(current)
            }
            Err(_) if force => self.filesystem.canonicalize(workspace.context("cannot determine the older server's workspace; supply --workspace DIR with --force")?).context("open server workspace"),
            Err(error) => Err(error).context("cannot inspect server; older servers require --force to discard their session"),
        }
    }

    fn wait_stopped(&self, endpoint: &SocketPath) -> Result<()> {
        self.check_cancellation()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while self
            .ipc
            .is_present(endpoint)
            .context("wait for stopped server socket removal")?
        {
            self.check_cancellation()?;
            if Instant::now() >= deadline {
                bail!(
                    "stopped server did not remove {}; replacement was not started",
                    endpoint.display()
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.check_cancellation()?;
        Ok(())
    }

    pub fn restart(
        &self,
        endpoint: &SocketPath,
        workspace: Option<&Path>,
        config: Option<&Path>,
        force: bool,
    ) -> Result<SessionInfo> {
        self.check_cancellation()?;
        let workspace = self.restart_workspace(endpoint, workspace, force)?;
        self.stop(endpoint, force)?;
        self.wait_stopped(endpoint)?;
        self.start(&workspace, endpoint, config, &[])
            .context("old session stopped; start replacement server")
    }
}

pub fn socket_by_id(id: &str) -> Result<SocketPath> {
    SessionManager::native().resolve(id)
}

pub fn list(socket: Option<&SocketPath>) -> Result<()> {
    let manager = SessionManager::native();
    list_with_manager(&manager, socket)
}

pub(crate) fn list_with_manager(
    manager: &SessionManager,
    socket: Option<&SocketPath>,
) -> Result<()> {
    let explicit = socket.is_some();
    let sockets = if let Some(socket) = socket {
        vec![socket.clone()]
    } else {
        manager.ipc.discover()?
    };
    if sockets.is_empty() {
        println!("No servers found.");
        return Ok(());
    }
    println!("ID\tPID\tCLIENT\tMODIFIED\tWORKSPACE\tSOCKET");
    for socket in sockets {
        let id = if explicit {
            "-".into()
        } else {
            socket
                .server_id()
                .map(|id| id.to_string())
                .unwrap_or_else(|| socket.as_path().to_string_lossy().into_owned())
        };
        match manager.request(&socket, ClientMessage::Status) {
            Ok(ServerMessage::Status {
                pid,
                workspace,
                attached,
                modified,
                ..
            }) => println!(
                "{id}\t{pid}\t{}\t{}\t{workspace}\t{}",
                if attached { "attached" } else { "detached" },
                if modified { "yes" } else { "no" },
                socket.display()
            ),
            Ok(ServerMessage::Rejected { reason }) => bail!("{reason}"),
            Ok(_) => bail!("unexpected management response from {}", socket.display()),
            Err(error) if explicit => return Err(error),
            Err(error) => {
                println!("{id}\t-\tunavailable\t-\t-\t{}", socket.display());
                eprintln!("{}: {error:#}", socket.display());
            }
        }
    }
    Ok(())
}

pub fn stop(socket: &SocketPath, force: bool) -> Result<()> {
    SessionManager::native().stop(socket, force)?;
    println!("Stopped server at {}", socket.display());
    Ok(())
}

pub fn start(
    workspace: &Path,
    socket: &SocketPath,
    config: Option<&Path>,
    files: &[PathBuf],
) -> Result<()> {
    SessionManager::native().start(workspace, socket, config, files)?;
    Ok(())
}

pub fn restart(
    socket: &SocketPath,
    workspace: Option<&Path>,
    config: Option<&Path>,
    force: bool,
) -> Result<()> {
    let manager = SessionManager::native();
    restart_with_manager(&manager, socket, workspace, config, force)
}

pub(crate) fn restart_with_manager(
    manager: &SessionManager,
    socket: &SocketPath,
    workspace: Option<&Path>,
    config: Option<&Path>,
    force: bool,
) -> Result<()> {
    let workspace = manager.restart_workspace(socket, workspace, force)?;
    eprintln!(
        "Restart creates a fresh session; open buffers, layout and undo history are reset{}.",
        if force {
            "; unsaved changes will be discarded"
        } else {
            ""
        }
    );
    manager.stop(socket, force)?;
    println!("Stopped server at {}", socket.display());
    manager.wait_stopped(socket)?;
    manager
        .start(&workspace, socket, config, &[])
        .context("old session stopped; start replacement server")?;
    println!(
        "Restarted server at {} using {}",
        socket.display(),
        manager.host.current_exe()?.display()
    );
    Ok(())
}
