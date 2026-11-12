pub mod framing;
pub mod platform;

mod path;
mod transport;

pub use path::{ServerId, SocketPath};
#[cfg(unix)]
pub use transport::UnixLocalIpc;
#[cfg(windows)]
pub use transport::WindowsLocalIpc;
pub use transport::{Listener, LocalIpc, NativeLocalIpc, Stream};

#[cfg(windows)]
mod windows;
