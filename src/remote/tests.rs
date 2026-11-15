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
    fn discover(&self) -> Result<Vec<SocketPath>> {
        Ok(Vec::new())
    }

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
    let worker = std::thread::spawn(move || -> Result<()> {
        let mut peer = listener.accept()?;
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        let mut input = BufReader::new(peer.try_clone()?);
        let _: zess_protocol::ClientMessage = zess_remote::read_request(&mut input)?;
        zess_remote::write_request(
            &mut peer,
            &zess_protocol::ServerMessage::Hello {
                build_id: zess_build::BUILD_ID.into(),
            },
        )?;
        let _: zess_protocol::ClientMessage = zess_remote::read_request(&mut input)?;
        zess_remote::write_request(
            &mut peer,
            &zess_protocol::ServerMessage::Status {
                session_id: zess_protocol::SessionId("actual-session".into()),
                name: None,
                server_instance: zess_protocol::ServerInstanceId("instance".into()),
                state_revision: 0,
                pid: 1,
                workspace: "/".into(),
                attached: false,
                modified: false,
            },
        )?;
        let mut peer = listener.accept()?;
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        let mut bytes = [0; 19];
        peer.read_exact(&mut bytes)?;
        assert_eq!(&bytes, b"remote protocol\0\xff\r\n");
        Ok(())
    });
    let output = prepare_proxy(
        ProxyRequest::new(ProxyOperation::AttachSession {
            session_id: "0123456789abcdef".into(),
        }),
        &manager,
        &NativeFileSystem,
        &NativePlatform,
    )?;
    let ProxyOutput::Stream {
        mut stream,
        session_id,
        ..
    } = output
    else {
        panic!("attach must return a protocol stream");
    };
    assert_eq!(session_id, "actual-session");
    stream.write_all(b"remote protocol\0\xff\r\n")?;
    worker.join().unwrap()?;
    Ok(())
}
