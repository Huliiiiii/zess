use std::io::{self, Read, Write};

use anyhow::Result;
use zess_ipc::platform::NativeDaemonControl;
use zess_ipc::{Listener, LocalIpc, NativeLocalIpc, ServerId};
use zess_platform::{NativeFileSystem, NativePlatform};

use super::*;

struct RoutedIpc {
    logical: SocketPath,
    physical: SocketPath,
}

impl LocalIpc for RoutedIpc {
    fn by_id(&self, _: ServerId) -> SocketPath {
        self.logical.clone()
    }

    fn prepare(&self, _: &SocketPath) -> Result<()> {
        unreachable!("attaching an existing session must not prepare an endpoint")
    }

    fn connect(&self, endpoint: &SocketPath) -> io::Result<Stream> {
        assert!(endpoint == &self.logical);
        NativeLocalIpc.connect(&self.physical)
    }

    fn bind(&self, _: &SocketPath) -> Result<Listener> {
        unreachable!("attaching an existing session must not bind an endpoint")
    }
}

#[test]
fn attaching_a_remote_session_uses_the_managers_ipc_route() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let physical = SocketPath::from(fixture.path().join("physical.sock"));
    let logical = SocketPath::from(fixture.path().join("logical.sock"));
    let listener = NativeLocalIpc.bind(&physical)?;
    let manager = SessionManager::new(
        Arc::new(NativeFileSystem),
        Arc::new(RoutedIpc { logical, physical }),
        Arc::new(NativeDaemonControl),
        Arc::new(NativePlatform),
    );
    let output = prepare_proxy(
        ProxyRequest::new(ProxyOperation::AttachSession {
            session_id: "0123456789abcdef".into(),
        }),
        &manager,
        &NativeFileSystem,
        &NativePlatform,
    )?;
    let ProxyOutput::Stream(mut stream) = output else {
        panic!("attach must return a protocol stream");
    };
    let mut peer = listener.accept()?;
    peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.write_all(b"remote protocol\0\xff\r\n")?;
    let mut bytes = [0; 19];
    peer.read_exact(&mut bytes)?;
    assert_eq!(&bytes, b"remote protocol\0\xff\r\n");
    Ok(())
}
