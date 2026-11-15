use std::{
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use zess_ipc::platform::{
    DaemonControl, DaemonHandle, DaemonSpec, NativeDaemonControl, ProcessStop, ProcessWait,
};
use zess_ipc::{Listener, LocalIpc, NativeLocalIpc, ServerId, SocketPath, Stream};
use zess_platform::{
    CancellationToken, HostInfo, NativeFileSystem, NativePlatform, Platform, UserDirs,
};

#[path = "../src/server.rs"]
pub mod server;

struct ScopedIpc {
    endpoint: SocketPath,
    stale: SocketPath,
    log: PathBuf,
}

impl LocalIpc for ScopedIpc {
    fn prepare(&self, endpoint: &SocketPath) -> Result<()> {
        NativeLocalIpc.prepare(endpoint)
    }

    fn connect(&self, endpoint: &SocketPath) -> io::Result<Stream> {
        NativeLocalIpc.connect(endpoint)
    }

    fn bind(&self, endpoint: &SocketPath) -> Result<Listener> {
        NativeLocalIpc.bind(endpoint)
    }

    fn discover(&self) -> Result<Vec<SocketPath>> {
        Ok(vec![self.endpoint.clone(), self.stale.clone()])
    }

    fn by_id(&self, id: ServerId) -> SocketPath {
        SocketPath::from(self.endpoint.as_path().with_file_name(format!("{id}.sock")))
    }

    fn for_workspace(&self, _: &Path) -> Result<SocketPath> {
        Ok(self.endpoint.clone())
    }

    fn log_path(&self, _: &SocketPath) -> PathBuf {
        self.log.clone()
    }
}

struct TestHost(PathBuf);

impl Platform for TestHost {
    fn host_info(&self) -> HostInfo {
        NativePlatform.host_info()
    }

    fn user_dirs(&self) -> Result<UserDirs> {
        NativePlatform.user_dirs()
    }

    fn current_dir(&self) -> io::Result<PathBuf> {
        Ok(self.0.clone())
    }

    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(PathBuf::from(env!("CARGO_BIN_EXE_zess")))
    }

    fn absolute_path(&self, path: &Path) -> io::Result<PathBuf> {
        NativePlatform.absolute_path(path)
    }

    fn process_id(&self) -> u32 {
        NativePlatform.process_id()
    }

    fn system_time(&self) -> std::time::SystemTime {
        NativePlatform.system_time()
    }
}

struct TestDaemons {
    log: PathBuf,
    spawns: AtomicUsize,
    stops: Arc<AtomicUsize>,
    reject_watch: AtomicBool,
}

impl TestDaemons {
    fn check_watch(&self) -> Result<()> {
        if self.reject_watch.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "process watch unavailable",
            )
            .into());
        }
        Ok(())
    }
}

struct TestStopPeer {
    target: Box<dyn ProcessStop>,
    stops: Arc<AtomicUsize>,
}

impl ProcessWait for TestStopPeer {
    fn wait(&self, timeout: Duration) -> io::Result<bool> {
        self.target.wait(timeout)
    }
}

impl ProcessStop for TestStopPeer {
    fn is_peer(&self, stream: &Stream) -> Result<bool> {
        self.target.is_peer(stream)
    }

    fn terminate(&self) -> Result<()> {
        self.stops.fetch_add(1, Ordering::Relaxed);
        self.target.terminate()
    }
}

impl DaemonControl for TestDaemons {
    fn watch(&self, stream: &Stream) -> Result<Box<dyn ProcessWait>> {
        self.check_watch()?;
        NativeDaemonControl.watch(stream)
    }

    fn connect_for_stop(
        &self,
        ipc: &dyn LocalIpc,
        endpoint: &SocketPath,
    ) -> Result<(Stream, Box<dyn ProcessStop>)> {
        self.check_watch()?;
        let (stream, target) = NativeDaemonControl.connect_for_stop(ipc, endpoint)?;
        Ok((
            stream,
            Box::new(TestStopPeer {
                target,
                stops: self.stops.clone(),
            }),
        ))
    }

    fn spawn(&self, specification: &DaemonSpec) -> Result<DaemonHandle> {
        self.spawns.fetch_add(1, Ordering::Relaxed);
        NativeDaemonControl.spawn(specification)
    }
}

struct Server {
    root: tempfile::TempDir,
    manager: server::SessionManager,
    daemons: Arc<TestDaemons>,
    endpoint: SocketPath,
    workspace: PathBuf,
    config: PathBuf,
}

impl Server {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let config = root.path().join("init.w");
        std::fs::write(&config, "").unwrap();
        let endpoint = SocketPath::from(root.path().join("0123456789abcdef.sock"));
        let daemons = Arc::new(TestDaemons {
            log: root.path().join("server.log"),
            spawns: AtomicUsize::new(0),
            stops: Arc::default(),
            reject_watch: AtomicBool::new(false),
        });
        let ipc = Arc::new(ScopedIpc {
            endpoint: endpoint.clone(),
            stale: SocketPath::from(root.path().join("unreachable.sock")),
            log: daemons.log.clone(),
        });
        let manager = server::SessionManager::new(
            Arc::new(NativeFileSystem),
            ipc.clone(),
            daemons.clone(),
            Arc::new(TestHost(root.path().to_owned())),
        );
        Self {
            root,
            manager,
            daemons,
            endpoint,
            workspace,
            config,
        }
    }

    fn start(&self) -> server::SessionInfo {
        self.manager
            .start(&self.workspace, &self.endpoint, Some(&self.config), &[])
            .unwrap_or_else(|error| {
                panic!(
                    "{error:#}\n{}",
                    std::fs::read_to_string(&self.daemons.log).unwrap_or_default()
                )
            })
    }
}

#[test]
fn workspace_session_creation_uses_the_configured_endpoint_and_log_path() {
    let server = Server::new();
    let created = server
        .manager
        .ensure_for_workspace(&server.workspace.join("."), Some(&server.config), &[])
        .unwrap();
    assert_eq!(Path::new(&created.endpoint), server.endpoint.as_path());
    assert_eq!(Path::new(&created.workspace), server.workspace);
    assert!(server.daemons.log.is_file());
}

#[test]
fn restarting_does_not_spawn_a_replacement_when_process_cannot_be_watched() {
    let server = Server::new();
    server.start();
    server.daemons.reject_watch.store(true, Ordering::Relaxed);
    let error = server
        .manager
        .restart(&server.endpoint, None, Some(&server.config), false)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 1);
    server.daemons.reject_watch.store(false, Ordering::Relaxed);
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.manager.inspect(&self.endpoint).is_ok() {
            let _ = self.manager.stop(&self.endpoint, true);
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.endpoint.is_present().unwrap_or(false) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn opening_a_workspace_reuses_its_existing_session() {
    let server = Server::new();
    let started = server.start();
    assert!(!started.session_id.0.is_empty());
    assert!(!started.server_instance.0.is_empty());
    let reused = server
        .manager
        .ensure(
            &server.workspace,
            &SocketPath::from(server.root.path().join("another.sock")),
            Some(&server.config),
            &[],
        )
        .unwrap();
    assert_eq!(reused.session_id, started.session_id);
    assert_eq!(reused.pid, started.pid);
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 1);
    let entries = server.manager.entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, started.session_id);
}

#[test]
fn restarting_replaces_the_daemon_and_preserves_the_workspace_endpoint() {
    let server = Server::new();
    let before = server.start();
    let after = server
        .manager
        .restart(&server.endpoint, None, Some(&server.config), false)
        .unwrap();
    assert_ne!(after.pid, before.pid);
    assert_ne!(after.server_instance, before.server_instance);
    assert_eq!(after.workspace, before.workspace);
    assert_eq!(after.endpoint, before.endpoint);
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 2);
}

#[test]
fn a_malformed_status_from_another_server_does_not_block_a_session_id() {
    use zess_ipc::framing;
    use zess_protocol::{ClientMessage, ServerMessage};

    let server = Server::new();
    let started = server.start();
    let broken = SocketPath::from(server.root.path().join("broken.sock"));
    let listener = NativeLocalIpc.bind(&broken).unwrap();
    listener.set_nonblocking(true).unwrap();
    let manager = server::SessionManager::new(
        Arc::new(NativeFileSystem),
        Arc::new(ScopedIpc {
            endpoint: broken,
            stale: server.endpoint.clone(),
            log: server.daemons.log.clone(),
        }),
        server.daemons.clone(),
        Arc::new(TestHost(server.root.path().to_owned())),
    );

    let done = AtomicBool::new(false);
    let (listed, unknown, selected) = thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    result => result.unwrap(),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let frame = framing::read(&mut reader, 1024 * 1024).unwrap().unwrap();
                assert!(matches!(
                    framing::decode::<ClientMessage>(&frame).unwrap(),
                    ClientMessage::Hello { .. }
                ));
                framing::write(
                    &mut stream,
                    &ServerMessage::Hello {
                        build_id: zess_build::BUILD_ID.to_owned(),
                    },
                )
                .unwrap();
                let frame = framing::read(&mut reader, 1024 * 1024).unwrap().unwrap();
                assert!(matches!(
                    framing::decode::<ClientMessage>(&frame).unwrap(),
                    ClientMessage::Status
                ));
                stream.write_all(b"not json\n").unwrap();
            }
        });
        let listed = manager.entries();
        let unknown = manager.resolve("missing-session");
        let selected = manager.resolve(&started.session_id.0);
        done.store(true, Ordering::Release);
        (listed, unknown, selected)
    });

    let list_error = listed.unwrap_err();
    assert_eq!(list_error.to_string(), "decode server management response");
    assert_eq!(unknown.err().unwrap().to_string(), list_error.to_string());
    let (_stream, selected, _) = manager.connect_selected(&selected.unwrap()).unwrap();
    assert_eq!(selected.session_id, started.session_id);
}

#[test]
fn operations_by_session_id_reject_a_replacement_at_the_same_endpoint() {
    let server = Server::new();
    let original = server.start();
    let target = server.manager.resolve(&original.session_id.0).unwrap();

    let replacement = server
        .manager
        .restart(&server.endpoint, None, Some(&server.config), false)
        .unwrap();
    assert_ne!(replacement.session_id, original.session_id);

    let unavailable = format!("session {} is no longer available", original.session_id.0);
    let error = server.manager.inspect_target(&target).unwrap_err();
    assert_eq!(error.to_string(), unavailable);
    let error = server.manager.connect_selected(&target).err().unwrap();
    assert_eq!(error.to_string(), unavailable);
    let error = server.manager.stop_target(&target, true).unwrap_err();
    assert_eq!(error.to_string(), unavailable);
    let error = server
        .manager
        .restart_target(
            &target,
            Some(&server.workspace),
            Some(&server.config),
            true,
            Some(&original.session_id.0),
        )
        .unwrap_err();
    assert_eq!(error.to_string(), unavailable);

    let current = server.manager.inspect(&server.endpoint).unwrap();
    assert_eq!(current.session_id, replacement.session_id);
}

#[test]
fn cancelling_session_creation_before_it_starts_does_not_spawn_a_daemon() {
    let server = Server::new();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let manager = server.manager.clone().with_cancellation(cancellation);
    let error = manager
        .ensure(
            &server.workspace,
            &server.endpoint,
            Some(&server.config),
            &[],
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "operation was cancelled");
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 0);
}

#[test]
fn a_build_mismatch_does_not_start_another_session() {
    let server = Server::new();
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let manager = server.manager.clone();
    let workspace = server.workspace.clone();
    let endpoint = server.endpoint.clone();
    let worker = thread::spawn(move || manager.ensure(&workspace, &endpoint, None, &[]));
    let mut stream = listener.accept().unwrap();
    zess_ipc::framing::read(
        &mut BufReader::new(stream.try_clone().unwrap()),
        1024 * 1024,
    )
    .unwrap()
    .unwrap();
    stream
        .write_all(
            &zess_ipc::framing::encode(&zess_protocol::ServerMessage::Hello {
                build_id: "different-build".into(),
            })
            .unwrap(),
        )
        .unwrap();
    let error = worker.join().unwrap().unwrap_err();
    assert!(
        error
            .downcast_ref::<zess_protocol::BuildMismatch>()
            .is_some()
    );
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 0);
}

#[test]
fn cancelling_a_force_stop_wait_does_not_signal_the_daemon() {
    let server = Server::new();
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let cancellation = CancellationToken::new();
    let manager = server
        .manager
        .clone()
        .with_cancellation(cancellation.clone());
    let endpoint = server.endpoint.clone();
    let (sender, result) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || sender.send(manager.stop(&endpoint, true)).unwrap());
    let stream = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let request = zess_ipc::framing::read(&mut reader, 1024 * 1024)
        .unwrap()
        .unwrap();
    assert!(matches!(
        zess_ipc::framing::decode::<zess_protocol::ClientMessage>(&request).unwrap(),
        zess_protocol::ClientMessage::Hello { .. }
    ));
    cancellation.cancel();
    let error = result
        .recv_timeout(Duration::from_secs(1))
        .expect("management response wait did not observe cancellation")
        .unwrap_err();
    assert_eq!(error.to_string(), "operation was cancelled");
    assert_eq!(server.daemons.stops.load(Ordering::Relaxed), 0);
    worker.join().unwrap();
}

#[test]
fn management_responses_can_arrive_across_poll_intervals() {
    let server = Server::new();
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let manager = server.manager.clone();
    let endpoint = server.endpoint.clone();
    let worker = thread::spawn(move || manager.inspect(&endpoint));
    let mut stream = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    zess_ipc::framing::read(
        &mut BufReader::new(stream.try_clone().unwrap()),
        1024 * 1024,
    )
    .unwrap()
    .unwrap();
    stream
        .write_all(
            &zess_ipc::framing::encode(&zess_protocol::ServerMessage::Hello {
                build_id: zess_build::BUILD_ID.to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
    zess_ipc::framing::read(
        &mut BufReader::new(stream.try_clone().unwrap()),
        1024 * 1024,
    )
    .unwrap()
    .unwrap();
    let response = zess_ipc::framing::encode(&zess_protocol::ServerMessage::Status {
        session_id: zess_protocol::SessionId("fragmented-session".into()),
        name: None,
        server_instance: zess_protocol::ServerInstanceId("fragmented-instance".into()),
        state_revision: 5,
        pid: 42,
        workspace: server.workspace.to_string_lossy().into_owned(),
        attached: false,
        modified: false,
    })
    .unwrap();
    let middle = response.len() / 2;
    stream.write_all(&response[..middle]).unwrap();
    thread::sleep(Duration::from_millis(120));
    stream.write_all(&response[middle..]).unwrap();
    let info = worker.join().unwrap().unwrap();
    assert_eq!(info.session_id.0, "fragmented-session");
    assert_eq!(info.pid, 42);
    assert_eq!(info.workspace, server.workspace.to_string_lossy());
}
