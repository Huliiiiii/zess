use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use zess_ipc::platform::{
    DaemonControl, DaemonSpec, NativeDaemonControl, ProcessStop, ProcessWait,
};
use zess_ipc::{LocalIpc, NativeLocalIpc, ServerId, SocketPath, Stream, framing};
use zess_platform::{CancellationToken, FileSystem, NativeFileSystem, NativePlatform, Platform};
use zess_protocol::{ClientMessage, ServerInstanceId, ServerMessage, SessionId};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub endpoint: String,
    pub session_id: SessionId,
    pub name: Option<String>,
    pub server_instance: ServerInstanceId,
    pub workspace: String,
    pub pid: u32,
    pub attached: bool,
    pub modified: bool,
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
        self.handshake(&mut stream)?;
        self.request_on(&mut stream, message)
    }

    fn handshake(&self, stream: &mut Stream) -> Result<()> {
        self.request_on(
            stream,
            ClientMessage::Hello {
                build_id: zess_build::BUILD_ID.to_owned(),
            },
        )
        .and_then(|reply| match reply {
            ServerMessage::Hello { build_id } => {
                zess_protocol::check_build_id(&build_id).map_err(Into::into)
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
        loop {
            stream.set_read_timeout(Some(self.request_interval(deadline)?))?;
            match stream.read(&mut buffer) {
                Ok(0) => bail!("server did not return a management response"),
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

    fn decode_status(endpoint: &SocketPath, status: ServerMessage) -> Result<SessionInfo> {
        match status {
            ServerMessage::Status {
                session_id,
                name,
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
                name,
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

    pub fn inspect(&self, endpoint: &SocketPath) -> Result<SessionInfo> {
        Self::decode_status(endpoint, self.request(endpoint, ClientMessage::Status)?)
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
            match self.inspect(&endpoint) {
                Ok(info) => entries.push(info),
                Err(error)
                    if error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) => {}
                Err(error) => return Err(error),
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

    pub fn watch(&self, stream: &Stream) -> Result<Box<dyn ProcessWait>> {
        self.daemons.watch(stream)
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
            let status = self.inspect(&endpoint);
            self.check_cancellation()?;
            let info = match status {
                Ok(info) => info,
                Err(error)
                    if error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
                {
                    continue;
                }
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
        let matches: Vec<_> = self
            .entries()?
            .into_iter()
            .filter(|info| Path::new(&info.workspace) == workspace)
            .collect();
        match matches.as_slice() {
            [info] => return Ok(info.clone()),
            [] => {}
            _ => bail!(
                "multiple sessions exist for {}; select one explicitly",
                workspace.display()
            ),
        }
        if self.ipc.is_present(endpoint)? {
            let info = self.inspect(endpoint)?;
            self.check_workspace(&info, &workspace)?;
            return Ok(info);
        }
        self.start(&workspace, endpoint, config, files)
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
        if self.ipc.is_present(&endpoint)? {
            bail!("session already exists at {}", endpoint.display());
        }
        self.start_named(&workspace, &endpoint, config, files, name)
    }

    pub fn start(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        config: Option<&Path>,
        files: &[PathBuf],
    ) -> Result<SessionInfo> {
        self.start_named(workspace, endpoint, config, files, None)
    }

    fn start_named(
        &self,
        workspace: &Path,
        endpoint: &SocketPath,
        config: Option<&Path>,
        files: &[PathBuf],
        name: Option<&str>,
    ) -> Result<SessionInfo> {
        self.check_cancellation()?;
        let workspace = self
            .filesystem
            .canonicalize(workspace)
            .context("open workspace directory")?;
        if self.ipc.is_present(endpoint)? {
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
            if self.ipc.is_present(endpoint)? {
                let info = self.inspect(endpoint)?;
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

    fn connect_with_peer(&self, endpoint: &SocketPath) -> Result<(Stream, Box<dyn ProcessStop>)> {
        self.check_cancellation()?;
        self.daemons.connect_for_stop(self.ipc.as_ref(), endpoint)
    }

    fn status_on(&self, endpoint: &SocketPath, mut stream: Stream) -> Result<SessionInfo> {
        self.handshake(&mut stream)?;
        let status = self.request_on(&mut stream, ClientMessage::Status)?;
        Self::decode_status(endpoint, status)
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
