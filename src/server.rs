use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use zess_ipc::platform::{
    DaemonControl, DaemonHandle, DaemonSpec, NativeDaemonControl, ProcessStop, ProcessWait,
};
use zess_ipc::{LocalIpc, NativeLocalIpc, ServerId, SocketPath, Stream, framing};
use zess_platform::{CancellationToken, FileSystem, NativeFileSystem, NativePlatform, Platform};
use zess_protocol::{ClientMessage, ServerInstanceId, ServerMessage, SessionId};
use zess_session::SessionLifetime;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    #[serde(rename = "build_id")]
    pub version: String,
    pub id: String,
    pub endpoint: String,
    pub session_id: SessionId,
    pub name: Option<String>,
    pub server_instance: ServerInstanceId,
    pub workspace: String,
    pub pid: u32,
    pub attached: bool,
    pub modified: bool,
    #[serde(skip)]
    pending: bool,
}

pub enum SessionTarget {
    Endpoint(SocketPath),
    Instance { endpoint: SocketPath, id: SessionId },
}

impl SessionTarget {
    fn instance(info: SessionInfo) -> Self {
        Self::Instance {
            endpoint: SocketPath::from(PathBuf::from(info.endpoint)),
            id: info.session_id,
        }
    }

    pub fn endpoint(&self) -> &SocketPath {
        match self {
            Self::Endpoint(endpoint) | Self::Instance { endpoint, .. } => endpoint,
        }
    }

    fn check_identity(&self, info: &SessionInfo) -> Result<()> {
        if let Self::Instance { id, .. } = self {
            ensure!(
                info.session_id == *id,
                "session {} is no longer available",
                id.0
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct SessionManager {
    filesystem: Arc<dyn FileSystem>,
    ipc: Arc<dyn LocalIpc>,
    daemons: Arc<dyn DaemonControl>,
    host: Arc<dyn Platform>,
    development_version: Arc<OnceLock<String>>,
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
            development_version: Arc::default(),
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
        if !matches!(message, ClientMessage::Status) {
            self.handshake(&mut stream)?;
        }
        self.request_on(&mut stream, message)
    }

    fn version(&self) -> Result<&str> {
        if let Some(version) = zess_build::RELEASE_VERSION {
            return Ok(version);
        }
        if let Some(version) = self.development_version.get() {
            return Ok(version);
        }
        let version = zess_build::development_version(&self.host.current_exe()?)?;
        Ok(self.development_version.get_or_init(|| version))
    }

    fn handshake(&self, stream: &mut Stream) -> Result<()> {
        let expected = self.version()?;
        self.request_on(
            stream,
            ClientMessage::Hello {
                version: expected.to_owned(),
            },
        )
        .and_then(|reply| match reply {
            ServerMessage::Hello { version } => {
                zess_protocol::check_version(expected, &version).map_err(Into::into)
            }
            _ => Err(anyhow::anyhow!("unexpected server handshake")),
        })
    }

    fn request_on(&self, stream: &mut Stream, message: ClientMessage) -> Result<ServerMessage> {
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
        let mut received = false;
        loop {
            stream.set_read_timeout(Some(self.request_interval(deadline)?))?;
            match stream.read(&mut buffer) {
                Ok(0) if !received => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "server did not return a management response",
                    )
                    .into());
                }
                Ok(0) => bail!("server returned an incomplete management response"),
                Ok(read) => {
                    received = true;
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
                Err(error)
                    if received
                        && matches!(
                            error.kind(),
                            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                        ) =>
                {
                    bail!("server returned an incomplete management response: {error}");
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn decode_status(endpoint: &SocketPath, status: ServerMessage) -> Result<SessionInfo> {
        match status {
            ServerMessage::Status {
                version,
                session_id,
                name,
                server_instance,
                workspace,
                pid,
                attached,
                modified,
                pending,
                ..
            } => Ok(SessionInfo {
                version,
                id: endpoint
                    .server_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| endpoint.as_path().to_string_lossy().into_owned()),
                endpoint: endpoint.as_path().to_string_lossy().into_owned(),
                session_id,
                name,
                server_instance,
                workspace,
                pid,
                attached,
                modified,
                pending,
            }),
            ServerMessage::Rejected { reason } => bail!("{reason}"),
            _ => bail!("unexpected management response from {}", endpoint.display()),
        }
    }

    fn status(&self, endpoint: &SocketPath) -> Result<SessionInfo> {
        Self::decode_status(endpoint, self.request(endpoint, ClientMessage::Status)?)
    }

    fn require_published(endpoint: &SocketPath, info: SessionInfo) -> Result<SessionInfo> {
        ensure!(
            !info.pending,
            "a session is awaiting its first attach at {}",
            endpoint.display()
        );
        Ok(info)
    }

    pub fn inspect(&self, endpoint: &SocketPath) -> Result<SessionInfo> {
        Self::require_published(endpoint, self.status(endpoint)?)
    }

    pub(crate) fn inspect_target(&self, target: &SessionTarget) -> Result<SessionInfo> {
        let info = self.inspect(target.endpoint())?;
        target.check_identity(&info)?;
        Ok(info)
    }

    pub fn entries(&self) -> Result<Vec<SessionInfo>> {
        self.check_cancellation()?;
        let mut entries = Vec::new();
        for endpoint in self.ipc.discover()? {
            self.check_cancellation()?;
            if let Some(info) = self.inspect_existing(&endpoint)?
                && !info.pending
            {
                entries.push(info);
            }
            self.check_cancellation()?;
        }
        self.check_cancellation()?;
        Ok(entries)
    }

    fn inspect_existing(&self, endpoint: &SocketPath) -> Result<Option<SessionInfo>> {
        match self.status(endpoint) {
            Ok(info) => Ok(Some(info)),
            Err(error) => {
                let kind = error.downcast_ref::<io::Error>().map(io::Error::kind);
                if matches!(
                    kind,
                    Some(io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
                ) {
                    return Ok(None);
                }
                if matches!(
                    kind,
                    Some(
                        io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionReset
                    )
                ) {
                    if !self.ipc.is_present(endpoint)? {
                        return Ok(None);
                    }
                    // A Unix socket path can remain after its listener closes.
                    #[cfg(unix)]
                    if self.ipc.connect(endpoint).is_err_and(|error| {
                        matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                        )
                    }) {
                        return Ok(None);
                    }
                }
                Err(error)
            }
        }
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

    pub fn watch(&self, stream: &Stream) -> Result<Box<dyn ProcessWait>> {
        self.daemons.watch(stream)
    }

    pub fn workspace_sessions(&self, workspace: &Path) -> Result<Vec<SessionInfo>> {
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        Ok(self
            .entries()?
            .into_iter()
            .filter(|info| Path::new(&info.workspace) == workspace)
            .collect())
    }

    pub fn resolve(&self, id: &str) -> Result<SessionTarget> {
        self.check_cancellation()?;
        let named = self.ipc.by_id(ServerId::for_name(id));
        if self.ipc.is_present(&named)? {
            return Ok(SessionTarget::Endpoint(named));
        }
        let mut first_error = None;
        for endpoint in self.ipc.discover()? {
            self.check_cancellation()?;
            let status = self.inspect_existing(&endpoint);
            self.check_cancellation()?;
            let info = match status {
                Ok(Some(info)) if !info.pending => info,
                Ok(_) => continue,
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            };
            if info.session_id.0 == id {
                return Ok(SessionTarget::instance(info));
            }
            if info.id.eq_ignore_ascii_case(id) || info.name.as_deref() == Some(id) {
                return Ok(SessionTarget::Endpoint(SocketPath::from(PathBuf::from(
                    info.endpoint,
                ))));
            }
        }
        self.check_cancellation()?;
        if let Some(error) = first_error {
            return Err(error);
        }
        if let Ok(id) = id.parse() {
            return Ok(SessionTarget::Endpoint(self.ipc.by_id(id)));
        }
        bail!("unknown session: {id}")
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

    fn launch(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        config: Option<&Path>,
        files: &[PathBuf],
        name: Option<&str>,
        lifetime: SessionLifetime,
    ) -> Result<DaemonHandle> {
        self.check_cancellation()?;
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        if self.inspect_existing(endpoint)?.is_some() {
            bail!("session already exists at {}", endpoint.display());
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
        if let Some(name) = name {
            args.extend(["--session-name".into(), name.into()]);
        }
        if matches!(lifetime, SessionLifetime::Pending) {
            args.push("--pending-attach".into());
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
        self.daemons
            .spawn(&specification)
            .context("start editor server")
    }

    fn wait_for_daemon(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        server: &mut DaemonHandle,
    ) -> Result<SessionInfo> {
        let start = Instant::now();
        loop {
            self.check_cancellation()?;
            if let Some(info) = self.inspect_existing(endpoint)? {
                ensure!(
                    info.pid == server.id(),
                    "another server started at {}",
                    endpoint.display()
                );
                self.check_workspace(&info, workspace)?;
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

    fn finish_start<T>(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        mut server: DaemonHandle,
        on_ready: impl FnOnce(SessionInfo) -> Result<T>,
    ) -> Result<T> {
        let opened = self
            .wait_for_daemon(workspace, endpoint, &mut server)
            .and_then(on_ready);
        match opened {
            Ok(session) => {
                server.reap().context("watch daemon exit")?;
                Ok(session)
            }
            Err(error) => {
                server.terminate().with_context(|| {
                    format!("opening session failed ({error:#}); terminate newly started daemon")
                })?;
                // The IPC backend checks whether another server now owns this endpoint.
                let _ = self.ipc.remove_stale(endpoint);
                Err(error)
            }
        }
    }

    fn start_named(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        config: Option<&Path>,
        files: &[PathBuf],
        name: Option<&str>,
    ) -> Result<SessionInfo> {
        let server = self.launch(
            workspace,
            endpoint,
            config,
            files,
            name,
            SessionLifetime::Retained,
        )?;
        self.finish_start(workspace, endpoint, server, Ok)
    }

    pub fn open_workspace(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        config: Option<&Path>,
        files: &[PathBuf],
    ) -> Result<(Stream, SessionInfo)> {
        self.check_cancellation()?;
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        let matches = self.workspace_sessions(&workspace)?;
        let existing = match matches.as_slice() {
            [info] => Some(info.clone()),
            [] => self.inspect_existing(endpoint)?,
            _ => bail!(
                "multiple sessions exist for {}; select one explicitly",
                workspace.display()
            ),
        };
        if let Some(info) = existing {
            self.check_workspace(&info, &workspace)?;
            zess_protocol::check_version(self.version()?, &info.version)?;
            ensure!(
                !info.pending,
                "the workspace session is awaiting its first attach"
            );
            let stream = self.connect(&SocketPath::from(PathBuf::from(&info.endpoint)))?;
            return Ok((stream, info));
        }

        let server = self.launch(
            &workspace,
            endpoint,
            config,
            files,
            None,
            SessionLifetime::Pending,
        )?;
        self.finish_start(&workspace, endpoint, server, |started| {
            ensure!(
                started.pending,
                "the new session was published before its first attach"
            );
            zess_protocol::check_version(self.version()?, &started.version)?;
            let mut stream = self.connect(endpoint)?;
            let claimed = Self::decode_status(
                endpoint,
                self.request_on(
                    &mut stream,
                    ClientMessage::ClaimPendingSession { pid: started.pid },
                )?,
            )?;
            ensure!(
                claimed.pending && claimed.server_instance == started.server_instance,
                "the provisional session changed while opening the workspace"
            );
            stream.set_read_timeout(None)?;
            stream.set_write_timeout(None)?;
            self.check_cancellation()?;
            Ok((stream, claimed))
        })
    }

    pub fn create(
        &self,
        workspace: &Path,
        config: Option<&Path>,
        files: &[PathBuf],
        name: Option<&str>,
    ) -> Result<SessionInfo> {
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        let endpoint = if let Some(name) = name {
            if name.is_empty() {
                bail!("session name cannot be empty");
            }
            self.ipc.by_id(ServerId::for_name(name))
        } else {
            if !self.workspace_sessions(&workspace)?.is_empty() {
                bail!("a session already exists for {}", workspace.display());
            }
            self.endpoint_for_workspace(&workspace)?
        };
        self.start_named(&workspace, &endpoint, config, files, name)
    }

    fn connect_with_peer(&self, endpoint: &SocketPath) -> Result<(Stream, Box<dyn ProcessStop>)> {
        self.check_cancellation()?;
        self.daemons.connect_for_stop(self.ipc.as_ref(), endpoint)
    }

    fn status_on(&self, endpoint: &SocketPath, mut stream: Stream) -> Result<SessionInfo> {
        let status = self.request_on(&mut stream, ClientMessage::Status)?;
        Self::require_published(endpoint, Self::decode_status(endpoint, status)?)
    }

    fn reconnect_peer(
        &self,
        endpoint: &SocketPath,
        process: &dyn ProcessStop,
        operation: &str,
    ) -> Result<Stream> {
        // Status closes its connection, so check the peer of the new stream.
        let stream = self.connect(endpoint)?;
        ensure!(
            process.is_peer(&stream)?,
            "session changed during {operation} at {}",
            endpoint.display()
        );
        Ok(stream)
    }

    pub(crate) fn connect_selected(
        &self,
        target: &SessionTarget,
    ) -> Result<(Stream, SessionInfo, Box<dyn ProcessStop>)> {
        let endpoint = target.endpoint();
        let (stream, process) = self.connect_with_peer(endpoint)?;
        let info = self.status_on(endpoint, stream)?;
        target.check_identity(&info)?;
        let stream = self.reconnect_peer(endpoint, process.as_ref(), "connect")?;
        Ok((stream, info, process))
    }

    fn stop_on(
        &self,
        endpoint: &SocketPath,
        mut stream: Stream,
        process: &dyn ProcessStop,
        force: bool,
    ) -> Result<()> {
        self.check_cancellation()?;
        let response = self
            .handshake(&mut stream)
            .and_then(|()| self.request_on(&mut stream, ClientMessage::Stop { force }));
        self.check_cancellation()?;
        match response {
            Ok(ServerMessage::Exited) => {}
            Ok(ServerMessage::Rejected { reason }) => bail!("{reason}"),
            Ok(_) => bail!("unexpected management response from {}", endpoint.display()),
            Err(error) if force => {
                process.terminate().with_context(|| {
                    format!("management request failed ({error:#}); stop original server process")
                })?;
                return self.check_cancellation();
            }
            Err(error) => return Err(error),
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.check_cancellation()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for server process to exit");
            }
            if process.wait(remaining.min(Duration::from_millis(50)))? {
                return Ok(());
            }
        }
    }

    pub fn stop(&self, endpoint: &SocketPath, force: bool) -> Result<()> {
        let (stream, process) = self.connect_with_peer(endpoint)?;
        self.stop_on(endpoint, stream, process.as_ref(), force)
    }

    pub fn stop_target(&self, target: &SessionTarget, force: bool) -> Result<()> {
        let SessionTarget::Instance { endpoint, .. } = target else {
            return self.stop(target.endpoint(), force);
        };
        let (stream, _, process) = self.connect_selected(target)?;
        self.stop_on(endpoint, stream, process.as_ref(), force)
    }

    pub fn restart(
        &self,
        endpoint: &SocketPath,
        workspace: Option<&Path>,
        config: Option<&Path>,
        force: bool,
    ) -> Result<SessionInfo> {
        self.restart_target(
            &SessionTarget::Endpoint(endpoint.clone()),
            workspace,
            config,
            force,
            None,
        )
    }

    pub fn restart_target(
        &self,
        target: &SessionTarget,
        workspace: Option<&Path>,
        config: Option<&Path>,
        force: bool,
        selector: Option<&str>,
    ) -> Result<SessionInfo> {
        let endpoint = target.endpoint();
        let (stream, process) = self.connect_with_peer(endpoint)?;
        let info = match self.status_on(endpoint, stream) {
            Ok(info) => {
                target.check_identity(&info)?;
                Some(info)
            }
            Err(_)
                if force && workspace.is_some() && matches!(target, SessionTarget::Endpoint(_)) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let current =
            self.filesystem
                .canonicalize(match &info {
                    Some(info) => Path::new(&info.workspace),
                    None => workspace
                        .context("workspace is required to restart an unresponsive session")?,
                })
                .context("open server workspace")?;
        if let Some(workspace) = workspace
            && self
                .filesystem
                .canonicalize(workspace)
                .context("open workspace directory")?
                != current
        {
            bail!(
                "server workspace is {}; refusing to restart it in {}",
                current.display(),
                workspace.display()
            );
        }
        let stream = self.reconnect_peer(endpoint, process.as_ref(), "restart")?;
        self.stop_on(endpoint, stream, process.as_ref(), force)?;
        let name = info
            .as_ref()
            .and_then(|info| info.name.as_deref())
            .or_else(|| {
                selector.filter(|name| {
                    endpoint
                        .server_id()
                        .is_some_and(|id| id.to_string() == ServerId::for_name(name).to_string())
                })
            });
        self.start_named(&current, endpoint, config, &[], name)
            .context("old session stopped; start replacement server")
    }
}
