use std::{
    io::{self, BufReader, Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zess_ipc::{LocalIpc, Stream};
use zess_platform::{Cancellation, FileSystem, Platform, SystemProcessRunner};
use zess_remote::{
    AskpassBroker, Connector, ProgressReporter, ProxyHandshake, ProxyOperation, ProxyRequest,
    RemoteProgress, RemoteStage, SshConnection, SshConnector, SshPrompt, SshPromptHandler,
    SshPromptKind, SshTarget, StopTarget,
};
use zess_terminal::{
    AuthenticationKind, AuthenticationPrompt, ByteProgress, ConnectionContext, ConnectionFactory,
    ConnectionProgress, ConnectionStream, SessionCandidate,
};

use crate::server::{SessionInfo, SessionManager};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct RemoteOptions {
    pub target: SshTarget,
    pub bootstrap: Arc<dyn zess_remote::ServerBootstrap>,
}

pub struct SshAddress<'a> {
    pub host: &'a str,
    pub user: Option<&'a str>,
    pub port: Option<u16>,
    pub workspace: &'a str,
}

impl<'a> SshAddress<'a> {
    pub fn parse(address: &'a str) -> Result<Self> {
        let address = address
            .strip_prefix("ssh://")
            .context("invalid SSH address")?;
        let authority = address
            .split_once('/')
            .map(|(authority, _)| authority)
            .context("SSH address needs an absolute workspace path")?;
        let workspace = &address[authority.len()..];
        let (user, authority) = if let Some((user, host)) = authority.split_once('@') {
            if user.is_empty() || user.contains([':', '@']) {
                bail!("SSH address user must not contain a password");
            }
            (Some(user), host)
        } else {
            (None, authority)
        };
        let (host, port) = if let Some(host) = authority.strip_prefix('[') {
            let (host, suffix) = host.split_once(']').context("invalid SSH IPv6 address")?;
            host.parse::<std::net::Ipv6Addr>()
                .context("invalid SSH IPv6 address")?;
            let port = if suffix.is_empty() {
                None
            } else {
                Some(suffix.strip_prefix(':').context("invalid SSH address")?)
            };
            (host, port)
        } else if let Some((host, port)) = authority.split_once(':') {
            if port.contains(':') {
                bail!("IPv6 SSH addresses must be enclosed in brackets");
            }
            (host, Some(port))
        } else {
            (authority, None)
        };
        if host.is_empty() || host.contains('@') {
            bail!("SSH address needs a host");
        }
        let port = port
            .filter(|port| !port.is_empty())
            .map(|port| port.parse().context("invalid SSH port"))
            .transpose()?;
        Ok(Self {
            host,
            user,
            port,
            workspace,
        })
    }
}

type ConnectorImpl = SshConnector<SystemProcessRunner, ProxyRequest>;

pub enum RemoteSelection {
    Workspace {
        workspace: String,
        config: Option<String>,
        attach_only: bool,
    },
    Session(String),
}

pub struct RemoteConnectionFactory {
    options: RemoteOptions,
    selection: RemoteSelection,
    session_id: Option<zess_protocol::SessionId>,
    ipc: Arc<dyn LocalIpc>,
    program: PathBuf,
    display: String,
}

struct ConnectionObserver(ConnectionContext);

impl ProgressReporter for ConnectionObserver {
    fn report(&self, event: RemoteProgress) {
        let (stage, bytes) = match event {
            RemoteProgress::Stage(stage) => (stage, None),
            RemoteProgress::Transfer {
                stage,
                completed,
                total,
            } => (stage, Some(ByteProgress { completed, total })),
            RemoteProgress::Diagnostic(message) => {
                self.0.progress(ConnectionProgress {
                    message,
                    bytes: None,
                });
                return;
            }
        };
        let message = match stage {
            RemoteStage::DetectingPlatform => "Detecting remote platform…",
            RemoteStage::CheckingCache => "Checking for a matching server…",
            RemoteStage::Downloading => "Downloading matching server…",
            RemoteStage::Uploading => "Uploading server…",
            RemoteStage::Installing => "Installing server…",
            RemoteStage::Connecting => "Connecting to remote server…",
            RemoteStage::Ready => "Attaching to session…",
        };
        self.0.progress(ConnectionProgress {
            message: message.into(),
            bytes,
        });
    }
}

impl SshPromptHandler for ConnectionObserver {
    fn authenticate(
        &self,
        prompt: SshPrompt,
        cancellation: &dyn Cancellation,
    ) -> Result<Option<String>> {
        let kind = match prompt.kind {
            SshPromptKind::Secret => AuthenticationKind::Secret,
            SshPromptKind::Visible => AuthenticationKind::Visible,
            SshPromptKind::Confirmation => AuthenticationKind::Confirmation,
            SshPromptKind::Information => AuthenticationKind::Information,
        };
        self.0.authenticate_with_cancellation(
            AuthenticationPrompt {
                message: prompt.message,
                kind,
            },
            cancellation,
        )
    }
}

struct RemoteStream {
    connection: SshConnection,
    session_id: Option<zess_protocol::SessionId>,
}

impl Read for RemoteStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.connection.read(bytes)
    }
}

impl Write for RemoteStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.connection.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.connection.flush()
    }
}

impl ConnectionStream for RemoteStream {
    fn session_id(&self) -> Option<&zess_protocol::SessionId> {
        self.session_id.as_ref()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.connection.set_nonblocking(nonblocking)
    }

    fn wait_for_server_exit(&mut self, timeout: Duration) -> Result<bool> {
        Ok(self.connection.wait_for_exit(timeout)?)
    }
}

impl ConnectionFactory for RemoteConnectionFactory {
    fn display_target(&self) -> &str {
        &self.display
    }

    fn connect(&mut self, context: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
        context.cancellation().check()?;
        let observer = Arc::new(ConnectionObserver(context.clone()));
        let broker = AskpassBroker::start(
            self.ipc.clone(),
            self.program.clone(),
            context.cancellation().clone(),
            observer.clone(),
        )?;
        let mut options = self.options.clone();
        options.target.cancellation = Some(context.cancellation().clone());
        options.target.progress = Some(observer);
        options.target.askpass = Some(broker.config().clone());
        if context.session_id().is_none() && self.session_id.is_none() {
            let sessions = match &self.selection {
                RemoteSelection::Session(id) => vec![
                    options
                        .sessions()?
                        .into_iter()
                        .find(|entry| {
                            entry.id == *id
                                || entry.session_id.0 == *id
                                || entry.name.as_deref() == Some(id.as_str())
                        })
                        .with_context(|| format!("remote session {id} was not found"))?,
                ],
                RemoteSelection::Workspace { workspace, .. } => {
                    options.workspace_sessions(workspace.clone())?
                }
            };
            let selected = match sessions.as_slice() {
                [] => {
                    if matches!(
                        &self.selection,
                        RemoteSelection::Workspace {
                            attach_only: true,
                            ..
                        }
                    ) {
                        bail!("no session exists for the remote workspace");
                    }
                    None
                }
                [session] => Some(session),
                _ => {
                    let candidates = sessions
                        .iter()
                        .map(|session| SessionCandidate {
                            id: session.session_id.clone(),
                            display: session.name.clone().unwrap_or_else(|| session.id.clone()),
                            workspace: session.workspace.clone(),
                        })
                        .collect();
                    let id = context
                        .choose_session(candidates)?
                        .context("session selection cancelled")?;
                    Some(
                        sessions
                            .iter()
                            .find(|session| session.session_id == id)
                            .context("selected session is no longer available")?,
                    )
                }
            };
            if let Some(session) = selected {
                zess_protocol::check_version(zess_build::version(), &session.version)?;
                self.session_id = Some(session.session_id.clone());
            }
        }
        let pinned_session = context.session_id().or(self.session_id.as_ref());
        let operation = if let Some(id) = pinned_session {
            ProxyOperation::AttachSession {
                session_id: id.0.clone(),
            }
        } else {
            let RemoteSelection::Workspace {
                workspace, config, ..
            } = &self.selection
            else {
                unreachable!("a selected session has an identity")
            };
            ProxyOperation::Attach {
                workspace: workspace.clone(),
                files: Vec::new(),
                config: config.clone(),
            }
        };
        let connection = options.connector(operation)?.connect()?;
        let id = zess_protocol::SessionId(
            connection
                .session_id()
                .context("remote proxy did not identify the session")?
                .to_owned(),
        );
        if let Some(pinned) = pinned_session {
            anyhow::ensure!(
                id == *pinned,
                "the original zess session is no longer available"
            );
        }
        context.cancellation().check()?;
        Ok(Box::new(RemoteStream {
            connection,
            session_id: Some(id),
        }))
    }
}

impl RemoteOptions {
    fn connector(&self, operation: ProxyOperation) -> Result<ConnectorImpl> {
        Ok(SshConnector {
            target: self.target.clone(),
            bootstrap: self.bootstrap.clone(),
            runner: SystemProcessRunner,
            request: ProxyRequest::new(operation),
        })
    }

    pub fn connection_factory(
        self,
        selection: RemoteSelection,
        ipc: Arc<dyn LocalIpc>,
        program: PathBuf,
    ) -> RemoteConnectionFactory {
        let display = match &selection {
            RemoteSelection::Workspace { workspace, .. } => {
                format!("{}:{workspace}", self.target.host)
            }
            RemoteSelection::Session(id) => format!("{} (session {id})", self.target.host),
        };
        RemoteConnectionFactory {
            options: self,
            selection,
            ipc,
            program,
            display,
            session_id: None,
        }
    }

    pub fn request(&self, operation: ProxyOperation) -> Result<ProxyResponse> {
        let mut connection = self.connector(operation)?.connect()?;
        let response = zess_remote::read_request(&mut BufReader::new(&mut connection))?;
        match response {
            ProxyResponse::Failed { message } => bail!(message),
            response => Ok(response),
        }
    }

    pub fn sessions(&self) -> Result<Vec<SessionInfo>> {
        match self.request(ProxyOperation::List)? {
            ProxyResponse::Sessions { sessions } => Ok(sessions),
            _ => bail!("remote manager did not return a session list"),
        }
    }

    pub fn workspace_sessions(&self, workspace: String) -> Result<Vec<SessionInfo>> {
        match self.request(ProxyOperation::WorkspaceSessions { workspace })? {
            ProxyResponse::Sessions { sessions } => Ok(sessions),
            _ => bail!("remote manager did not return workspace sessions"),
        }
    }

    pub fn create(
        &self,
        workspace: String,
        config: Option<String>,
        files: Vec<String>,
        name: Option<String>,
    ) -> Result<SessionInfo> {
        match self.request(ProxyOperation::Start {
            workspace,
            config,
            files,
            name,
        })? {
            ProxyResponse::Session { session } => Ok(session),
            _ => bail!("remote manager did not return the created session"),
        }
    }

    pub fn stop(&self, target: StopTarget, force: bool) -> Result<()> {
        match self.request(ProxyOperation::Stop { target, force })? {
            ProxyResponse::Stopped => Ok(()),
            _ => bail!("remote manager did not confirm the stopped session"),
        }
    }

    pub fn restart(
        &self,
        session_id: Option<String>,
        workspace: Option<String>,
        config: Option<String>,
        force: bool,
    ) -> Result<SessionInfo> {
        match self.request(ProxyOperation::Restart {
            session_id,
            workspace,
            config,
            force,
        })? {
            ProxyResponse::Session { session } => Ok(session),
            _ => bail!("remote manager did not return the restarted session"),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProxyResponse {
    Sessions { sessions: Vec<SessionInfo> },
    Session { session: SessionInfo },
    Stopped,
    Failed { message: String },
}

fn workspace_path(
    workspace: &str,
    filesystem: &dyn FileSystem,
    host: &dyn Platform,
) -> Result<PathBuf> {
    let path = if workspace == "~" {
        host.user_dirs()?.home
    } else if let Some(relative) = workspace.strip_prefix("~/") {
        host.user_dirs()?.home.join(relative)
    } else {
        PathBuf::from(workspace)
    };
    let path = if path.is_absolute() {
        path
    } else {
        host.current_dir()?.join(path)
    };
    filesystem
        .canonicalize(&path)
        .with_context(|| format!("open remote workspace {}", path.display()))
}

pub fn serve_proxy(
    manager: SessionManager,
    filesystem: Arc<dyn FileSystem>,
    host: Arc<dyn Platform>,
) -> Result<()> {
    let mut input = BufReader::new(io::stdin());
    let hello: zess_protocol::ClientMessage = zess_remote::read_request(&mut input)?;
    let zess_protocol::ClientMessage::Hello { version } = hello else {
        bail!("remote proxy expected hello");
    };
    zess_remote::write_request(
        &mut io::stdout().lock(),
        &zess_protocol::ServerMessage::Hello {
            version: zess_build::version().into(),
        },
    )?;
    zess_protocol::check_version(zess_build::version(), &version)?;
    let prepared = zess_remote::read_request(&mut input)
        .and_then(|request| prepare_proxy(request, &manager, filesystem.as_ref(), host.as_ref()));
    let output = match prepared {
        Ok(output) => output,
        Err(error) => {
            return zess_remote::write_request(
                &mut io::stdout().lock(),
                &ProxyHandshake::Failed {
                    message: format!("{error:#}"),
                },
            );
        }
    };
    match output {
        ProxyOutput::Stream { stream, session_id } => {
            let peer = manager.watch(&stream)?;
            zess_remote::write_request(
                &mut io::stdout().lock(),
                &ProxyHandshake::Ready {
                    session_id: Some(session_id),
                },
            )?;
            bridge(input, stream, peer)
        }
        ProxyOutput::Response(response) => {
            zess_remote::write_request(
                &mut io::stdout().lock(),
                &ProxyHandshake::Ready { session_id: None },
            )?;
            zess_remote::write_request(&mut io::stdout().lock(), &response)
        }
    }
}

enum ProxyOutput {
    Stream { stream: Stream, session_id: String },
    Response(ProxyResponse),
}

fn prepare_proxy(
    request: ProxyRequest,
    manager: &SessionManager,
    filesystem: &dyn FileSystem,
    host: &dyn Platform,
) -> Result<ProxyOutput> {
    if let ProxyOperation::AttachSession { session_id } = &request.operation {
        let endpoint = manager.resolve(session_id)?;
        let info = manager.inspect(&endpoint)?;
        let stream = manager
            .connect(&endpoint)
            .context("connect remote session")?;
        return Ok(ProxyOutput::Stream {
            stream,
            session_id: info.session_id.0,
        });
    }
    if let ProxyOperation::Attach {
        workspace,
        files,
        config,
    } = request.operation
    {
        let workspace = workspace_path(&workspace, filesystem, host)?;
        let config = config
            .map(PathBuf::from)
            .or_else(|| crate::default_config(filesystem, host));
        let files = files.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        let endpoint = manager.endpoint_for_workspace(&workspace)?;
        let (stream, info) =
            manager.open_workspace(&workspace, &endpoint, config.as_deref(), &files)?;
        return Ok(ProxyOutput::Stream {
            stream,
            session_id: info.session_id.0,
        });
    }
    let result = match request.operation {
        ProxyOperation::List | ProxyOperation::Status => manager
            .entries()
            .map(|sessions| ProxyResponse::Sessions { sessions }),
        ProxyOperation::WorkspaceSessions { workspace } => manager
            .workspace_sessions(&workspace_path(&workspace, filesystem, host)?)
            .map(|sessions| ProxyResponse::Sessions { sessions }),
        ProxyOperation::Start {
            workspace,
            config,
            files,
            name,
        } => {
            let workspace = workspace_path(&workspace, filesystem, host)?;
            let config = config
                .map(PathBuf::from)
                .or_else(|| crate::default_config(filesystem, host));
            let files = files.into_iter().map(PathBuf::from).collect::<Vec<_>>();
            manager
                .create(&workspace, config.as_deref(), &files, name.as_deref())
                .map(|session| ProxyResponse::Session { session })
        }
        ProxyOperation::Stop { target, force } => {
            let socket = match target {
                StopTarget::Session(id) => manager.resolve(&id)?,
                StopTarget::Workspace(workspace) => manager
                    .endpoint_for_workspace(&workspace_path(&workspace, filesystem, host)?)?,
            };
            manager
                .stop(&socket, force)
                .map(|()| ProxyResponse::Stopped)
        }
        ProxyOperation::Restart {
            session_id,
            workspace,
            config,
            force,
        } => {
            let workspace = workspace
                .as_deref()
                .map(|path| workspace_path(path, filesystem, host))
                .transpose()?;
            let socket = if let Some(id) = &session_id {
                manager.resolve(id)?
            } else {
                manager.endpoint_for_workspace(
                    workspace
                        .as_deref()
                        .context("restart needs a workspace or session ID")?,
                )?
            };
            let config = config
                .map(PathBuf::from)
                .or_else(|| crate::default_config(filesystem, host));
            manager
                .restart_with_selector(
                    &socket,
                    workspace.as_deref(),
                    config.as_deref(),
                    force,
                    session_id.as_deref(),
                )
                .map(|session| ProxyResponse::Session { session })
        }
        ProxyOperation::Attach { .. } | ProxyOperation::AttachSession { .. } => unreachable!(),
    };
    result.map(ProxyOutput::Response)
}

fn bridge(
    mut input: BufReader<io::Stdin>,
    mut stream: Stream,
    peer: Box<dyn zess_ipc::platform::ProcessWait>,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    let mut outgoing = stream.try_clone()?;
    std::thread::Builder::new()
        .name("zess-proxy-input".into())
        .spawn(move || {
            let _ = io::copy(&mut input, &mut outgoing);
            let _ = outgoing.shutdown(Shutdown::Write);
        })?;
    let mut reader = BufReader::new(&mut stream);
    let mut stdout = io::stdout().lock();
    while let Some(frame) = zess_ipc::framing::read(&mut reader, zess_protocol::MAX_MESSAGE_BYTES)?
    {
        stdout.write_all(&frame)?;
        stdout.flush()?;
        let message: zess_protocol::ServerMessage = zess_ipc::framing::decode(&frame)?;
        if matches!(
            message,
            zess_protocol::ServerMessage::Detached {
                disposition: zess_protocol::ClientExitDisposition::Stopping
            } | zess_protocol::ServerMessage::Exited
        ) {
            anyhow::ensure!(
                peer.wait(Duration::from_secs(10))?,
                "remote session did not exit within 10 seconds"
            );
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

pub fn path_string(path: &Path) -> Result<String> {
    path.to_str()
        .context("remote paths must be valid UTF-8")
        .map(str::to_owned)
}
