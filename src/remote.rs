use std::{
    io::{self, BufReader, Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zess_ipc::{SocketPath, Stream};
use zess_platform::{CancellationToken, FileSystem, Platform, SystemProcessRunner};
use zess_remote::{
    ArtifactCatalog, Connector, ProxyHandshake, ProxyOperation, ProxyRequest, SshBootstrap,
    SshConnection, SshConnector, SshTarget,
};
use zess_terminal::{ConnectionFactory, ConnectionStream};

use crate::server::{SessionInfo, SessionManager};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct RemoteOptions {
    pub target: SshTarget,
    pub server_binary: Option<PathBuf>,
    pub server_artifacts: Option<PathBuf>,
    pub file_system: Arc<dyn FileSystem>,
    pub platform: Arc<dyn Platform>,
}

type ConnectorImpl =
    SshConnector<SshBootstrap<SystemProcessRunner>, SystemProcessRunner, ProxyRequest>;

pub struct RemoteConnectionFactory(ConnectorImpl);

struct RemoteStream(SshConnection);

impl Read for RemoteStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}

impl Write for RemoteStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl ConnectionStream for RemoteStream {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }
}

impl ConnectionFactory for RemoteConnectionFactory {
    fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
        Ok(Box::new(RemoteStream(self.0.connect()?)))
    }

    fn connect_with_cancellation(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn ConnectionStream>> {
        self.0.target.cancellation = Some(cancellation.clone());
        self.connect()
    }
}

impl RemoteOptions {
    fn connector(&self, operation: ProxyOperation) -> Result<ConnectorImpl> {
        let file_system = self.file_system.clone();
        let artifacts = if let Some(path) = &self.server_artifacts {
            ArtifactCatalog::read_with_file_system(path, file_system.as_ref())?
        } else {
            ArtifactCatalog::default()
        };
        Ok(SshConnector {
            target: self.target.clone(),
            bootstrap: SshBootstrap {
                runner: SystemProcessRunner,
                version: env!("CARGO_PKG_VERSION").to_owned(),
                artifacts,
                server_binary: self.server_binary.clone(),
                file_system,
                platform: self.platform.clone(),
            },
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
        Ok(RemoteConnectionFactory(self.connector(
            ProxyOperation::Attach {
                workspace,
                files,
                config,
            },
        )?))
    }

    pub fn session_factory(&self, session_id: String) -> Result<RemoteConnectionFactory> {
        Ok(RemoteConnectionFactory(
            self.connector(ProxyOperation::AttachSession { session_id })?,
        ))
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

    pub fn create(
        &self,
        workspace: String,
        config: Option<String>,
        files: Vec<String>,
    ) -> Result<SessionInfo> {
        match self.request(ProxyOperation::Start {
            workspace,
            config,
            files,
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
    zess_remote::write_request(&mut io::stdout().lock(), &ProxyHandshake::ready())?;
    match output {
        ProxyOutput::Stream(stream) => bridge(input, stream),
        ProxyOutput::Response(response) => {
            zess_remote::write_request(&mut io::stdout().lock(), &response)
        }
    }
}

enum ProxyOutput {
    Stream(Stream),
    Response(ProxyResponse),
}

fn prepare_proxy(
    request: ProxyRequest,
    manager: &SessionManager,
    filesystem: &dyn FileSystem,
    host: &dyn Platform,
) -> Result<ProxyOutput> {
    request.validate()?;
    if let ProxyOperation::AttachSession { session_id } = &request.operation {
        let endpoint = manager.resolve(session_id)?;
        let stream = manager
            .connect(&endpoint)
            .context("connect remote session")?;
        return Ok(ProxyOutput::Stream(stream));
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
        return Ok(ProxyOutput::Stream(stream));
    }
    let result = match request.operation {
        ProxyOperation::List | ProxyOperation::Status => manager
            .entries()
            .map(|sessions| ProxyResponse::Sessions { sessions }),
        ProxyOperation::Start {
            workspace,
            config,
            files,
        } => {
            let workspace = workspace_path(&workspace, filesystem, host)?;
            let config = config
                .map(PathBuf::from)
                .or_else(|| crate::default_config(filesystem, host));
            let files = files.into_iter().map(PathBuf::from).collect::<Vec<_>>();
            manager
                .ensure_for_workspace(&workspace, config.as_deref(), &files)
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
            let socket = if let Some(id) = session_id {
                manager.resolve(&id)?
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
                .restart(&socket, workspace.as_deref(), config.as_deref(), force)
                .map(|session| ProxyResponse::Session { session })
        }
        ProxyOperation::Attach { .. } | ProxyOperation::AttachSession { .. } => unreachable!(),
    };
    result.map(ProxyOutput::Response)
}

fn bridge(mut input: BufReader<io::Stdin>, mut stream: Stream) -> Result<()> {
    stream.set_nonblocking(false)?;
    let mut outgoing = stream.try_clone()?;
    std::thread::Builder::new()
        .name("zess-proxy-input".into())
        .spawn(move || {
            let _ = io::copy(&mut input, &mut outgoing);
            let _ = outgoing.shutdown(Shutdown::Write);
        })?;
    let result = io::copy(&mut stream, &mut io::stdout().lock());
    let _ = stream.shutdown(Shutdown::Both);
    result.context("forward remote session output")?;
    Ok(())
}

pub fn path_string(path: &Path) -> Result<String> {
    path.to_str()
        .context("remote paths must be valid UTF-8")
        .map(str::to_owned)
}
