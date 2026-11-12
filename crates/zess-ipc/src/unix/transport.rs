use std::{
    fs,
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    time::Duration,
};

use anyhow::{Context, Result};

use crate::SocketPath;

pub struct Stream(pub(crate) UnixStream);

impl Stream {
    pub fn connect(socket: &SocketPath) -> io::Result<Self> {
        UnixStream::connect(socket.as_path()).map(Self)
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.shutdown(how)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

pub struct Listener {
    listener: UnixListener,
    socket: SocketPath,
}

impl Listener {
    pub fn bind(socket: &SocketPath) -> Result<Self> {
        socket.remove_stale()?;
        let listener = UnixListener::bind(socket.as_path())
            .with_context(|| format!("bind {}", socket.display()))?;
        let listener = Self {
            listener,
            socket: socket.clone(),
        };
        fs::set_permissions(socket.as_path(), fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.listener.set_nonblocking(nonblocking)
    }

    pub fn accept(&self) -> io::Result<Stream> {
        self.listener.accept().map(|(stream, _)| Stream(stream))
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.socket.as_path());
    }
}
