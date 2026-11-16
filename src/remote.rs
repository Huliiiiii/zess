use std::{
    io::{self, BufReader, Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zess_ipc::{SocketPath, Stream};
use zess_platform::{CancellationToken, FileSystem, Platform, SystemProcessRunner};
use zess_remote::{
    Connector, ProxyHandshake, ProxyOperation, ProxyRequest, SshConnection, SshConnector, SshTarget,
};
use zess_terminal::{ConnectionFactory, ConnectionStream};

use crate::server::{SessionInfo, SessionManager};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct RemoteOptions {
    pub target: SshTarget,
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

pub struct RemoteConnectionFactory {
    connector: ConnectorImpl,
    session_id: Option<zess_protocol::SessionId>,
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
    fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
        let connection = self.connector.connect()?;
        let session_id = Some(zess_protocol::SessionId(
            connection
                .session_id()
                .context("remote proxy did not identify the session")?
                .to_owned(),
        ));
        if let Some(pinned) = &self.session_id {
            anyhow::ensure!(
                session_id.as_ref() == Some(pinned),
                "the original zess session is no longer available"
            );
        } else if let Some(id) = &session_id {
            self.connector.request = ProxyRequest::new(ProxyOperation::AttachSession {
                session_id: id.0.clone(),
            });
            self.session_id = session_id.clone();
        }
        Ok(Box::new(RemoteStream {
            connection,
            session_id,
        }))
    }

    fn connect_with_cancellation(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn ConnectionStream>> {
        self.connector.target.cancellation = Some(cancellation.clone());
        self.connect()
    }
}

impl RemoteOptions {
    fn connector(&self, operation: ProxyOperation) -> Result<ConnectorImpl> {
        Ok(SshConnector {
            target: self.target.clone(),
            runner: SystemProcessRunner,
            request: ProxyRequest::new(operation),
        })
    }

    pub fn factory(
        &self,
        workspace: String,
        files: Vec<String>,
        config: Option<String>,
    ) -> Result<RemoteConnectionFactory> {
        Ok(RemoteConnectionFactory {
            connector: self.connector(ProxyOperation::Attach {
                workspace,
                files,
                config,
            })?,
            session_id: None,
        })
    }

    pub fn session_factory(&self, session_id: String) -> Result<RemoteConnectionFactory> {
        Ok(RemoteConnectionFactory {
            connector: self.connector(ProxyOperation::AttachSession { session_id })?,
            session_id: None,
        })
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

    pub fn stop(
        &self,
        session_id: Option<String>,
        workspace: Option<String>,
        force: bool,
    ) -> Result<()> {
        match self.request(ProxyOperation::Stop {
            session_id,
            workspace,
            force,
        })? {
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

fn endpoint(info: &SessionInfo) -> SocketPath {
    SocketPath::from(PathBuf::from(&info.endpoint))
}

pub fn serve_proxy(
    manager: SessionManager,
    filesystem: Arc<dyn FileSystem>,
    host: Arc<dyn Platform>,
) -> Result<()> {
    let mut input = BufReader::new(io::stdin());
    let hello: zess_protocol::ClientMessage = zess_remote::read_request(&mut input)?;
    let zess_protocol::ClientMessage::Hello { build_id } = hello else {
        bail!("remote proxy expected hello");
    };
    zess_remote::write_request(
        &mut io::stdout().lock(),
        &zess_protocol::ServerMessage::Hello {
            build_id: zess_build::BUILD_ID.into(),
        },
    )?;
    zess_protocol::check_build_id(&build_id)?;
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
        let info = manager.ensure_for_workspace(&workspace, config.as_deref(), &files)?;
        let stream = manager
            .connect(&endpoint(&info))
            .context("connect remote session")?;
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
        ProxyOperation::Stop {
            session_id,
            workspace,
            force,
        } => {
            let socket = if let Some(id) = session_id {
                manager.resolve(&id)?
            } else {
                manager.endpoint_for_workspace(&workspace_path(
                    workspace.as_deref().unwrap_or("."),
                    filesystem,
                    host,
                )?)?
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
