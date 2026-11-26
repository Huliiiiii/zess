mod artifact;
mod askpass;
mod bootstrap;
mod connection;
mod progress;
mod ssh;

#[cfg(test)]
mod tests;

pub use artifact::{GithubReleaseArtifacts, ServerArtifact, ServerArtifacts, ServerTarget};
pub use askpass::{
    AskpassBroker, AskpassConfig, SshPrompt, SshPromptHandler, SshPromptKind, run_askpass,
};
pub use bootstrap::{InstalledSshBootstrap, ServerBootstrap, SshBootstrap};
pub use connection::SshConnection;
pub use progress::{ProgressReporter, RemoteProgress, RemoteStage};
pub use ssh::{Connector, SshConnector, SshTarget};

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProxyHandshake {
    Ready { session_id: Option<String> },
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyRequest {
    pub operation: ProxyOperation,
}

impl ProxyRequest {
    pub fn new(operation: ProxyOperation) -> Self {
        Self { operation }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum ProxyOperation {
    AttachSession {
        session_id: String,
    },
    Attach {
        workspace: String,
        files: Vec<String>,
        config: Option<String>,
    },
    List,
    WorkspaceSessions {
        workspace: String,
    },
    Start {
        name: Option<String>,
        workspace: String,
        config: Option<String>,
        files: Vec<String>,
    },
    Restart {
        session_id: Option<String>,
        workspace: Option<String>,
        config: Option<String>,
        force: bool,
    },
    Stop {
        target: StopTarget,
        force: bool,
    },
    Status,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopTarget {
    Session(String),
    Workspace(String),
}

/// Sends the initial workspace/management request before the protocol stream.
pub fn write_request(writer: &mut impl Write, request: &impl Serialize) -> anyhow::Result<()> {
    zess_ipc::framing::write(writer, request)?;
    writer.flush()?;
    Ok(())
}

/// Reads exactly the initial request, leaving the protocol bytes in the reader.
pub fn read_request<T: DeserializeOwned>(reader: &mut impl BufRead) -> anyhow::Result<T> {
    let bytes = zess_ipc::framing::read(reader, 1024 * 1024)?
        .ok_or_else(|| anyhow::anyhow!("remote proxy request missing"))?;
    Ok(zess_ipc::framing::decode(&bytes)?)
}
