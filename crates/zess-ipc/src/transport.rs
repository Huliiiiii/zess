use std::{
    io,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{ServerId, SocketPath};

#[cfg(unix)]
#[path = "unix/transport.rs"]
mod backend;
#[cfg(windows)]
#[path = "windows/transport.rs"]
mod backend;

pub use backend::{Listener, Stream};

pub trait LocalIpc: Send + Sync {
    fn discover(&self) -> Result<Vec<SocketPath>> {
        SocketPath::discover()
    }

    fn by_id(&self, id: ServerId) -> SocketPath {
        SocketPath::by_id(id)
    }

    fn for_workspace(&self, workspace: &Path) -> Result<SocketPath> {
        SocketPath::for_workspace(workspace)
    }

    fn is_present(&self, endpoint: &SocketPath) -> io::Result<bool> {
        endpoint.is_present()
    }

    fn log_path(&self, endpoint: &SocketPath) -> PathBuf {
        endpoint.log_path()
    }

    fn remove_stale(&self, endpoint: &SocketPath) -> Result<()> {
        endpoint.remove_stale()
    }

    fn prepare(&self, endpoint: &SocketPath) -> Result<()>;
    fn connect(&self, endpoint: &SocketPath) -> io::Result<Stream>;
    fn bind(&self, endpoint: &SocketPath) -> Result<Listener>;
}

#[cfg(unix)]
pub struct UnixLocalIpc;
#[cfg(windows)]
pub struct WindowsLocalIpc;

#[cfg(unix)]
pub use UnixLocalIpc as NativeLocalIpc;
#[cfg(windows)]
pub use WindowsLocalIpc as NativeLocalIpc;

impl LocalIpc for NativeLocalIpc {
    fn prepare(&self, endpoint: &SocketPath) -> Result<()> {
        endpoint.prepare_parent()
    }

    fn connect(&self, endpoint: &SocketPath) -> io::Result<Stream> {
        Stream::connect(endpoint)
    }

    fn bind(&self, endpoint: &SocketPath) -> Result<Listener> {
        Listener::bind(endpoint)
    }
}
