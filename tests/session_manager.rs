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
use zess_ipc::{Listener, LocalIpc, NativeLocalIpc, ServerId, SocketPath, Stream, framing};
use zess_platform::{
    CancellationToken, HostInfo, NativeFileSystem, NativePlatform, Platform, UserDirs,
};
use zess_protocol::{ClientMessage, ServerMessage};

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
    cancel_after_spawn: std::sync::Mutex<Option<(CancellationToken, std::sync::mpsc::Sender<u32>)>>,
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
        let daemon = NativeDaemonControl.spawn(specification)?;
        if let Some((token, pid)) = self.cancel_after_spawn.lock().unwrap().take() {
            token.cancel();
            pid.send(daemon.id()).unwrap();
        }
        Ok(daemon)
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
            cancel_after_spawn: Default::default(),
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
            .create(&self.workspace, Some(&self.config), &[], None)
            .unwrap_or_else(|error| {
                panic!(
                    "{error:#}\n{}",
                    std::fs::read_to_string(&self.daemons.log).unwrap_or_default()
                )
            })
    }
}

fn wait_for_endpoint_removal(endpoint: &SocketPath) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while endpoint.is_present().unwrap() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

fn receive_server_message(reader: &mut BufReader<Stream>) -> ServerMessage {
    let frame = framing::read(reader, 1024 * 1024).unwrap().unwrap();
    framing::decode(&frame).unwrap()
}

fn attach_to_pending_session(mut stream: Stream, info: &server::SessionInfo) -> BufReader<Stream> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    framing::write(
        &mut stream,
        &ClientMessage::Hello {
            build_id: info.build_id.clone(),
        },
    )
    .unwrap();

    let mut reader = BufReader::new(stream);
    assert!(matches!(
        receive_server_message(&mut reader),
        ServerMessage::Hello { build_id } if build_id == info.build_id
    ));
    framing::write(
        reader.get_mut(),
        &ClientMessage::Attach {
            expected_session: Some(info.session_id.clone()),
            client_id: "test-terminal".into(),
            width: 80,
            height: 24,
            files: Vec::new(),
        },
    )
    .unwrap();
    assert!(matches!(
        receive_server_message(&mut reader),
        ServerMessage::AttachPending
    ));
    assert!(matches!(
        receive_server_message(&mut reader),
        ServerMessage::Attached { server_instance, .. } if server_instance == info.server_instance
    ));
    assert!(matches!(
        receive_server_message(&mut reader),
        ServerMessage::Snapshot { session_id, .. } if session_id == info.session_id
    ));
    reader
}

#[test]
fn workspace_session_creation_uses_the_configured_endpoint_and_log_path() {
    let server = Server::new();
    let created = server
        .manager
        .create(&server.workspace.join("."), Some(&server.config), &[], None)
        .unwrap();
    assert_eq!(Path::new(&created.endpoint), server.endpoint.as_path());
    assert_eq!(Path::new(&created.workspace), server.workspace);
    assert!(server.daemons.log.is_file());
}

#[test]
fn retrying_after_an_interrupted_first_attach_opens_a_new_session() {
    let server = Server::new();
    let (stream, interrupted) = server
        .manager
        .open_workspace(
            &server.workspace,
            &server.endpoint,
            Some(&server.config),
            &[],
        )
        .unwrap();
    #[cfg(any(target_os = "linux", windows))]
    let process = server.manager.watch(&stream).unwrap();
    let reader = attach_to_pending_session(stream, &interrupted);
    assert!(server.manager.entries().unwrap().is_empty());

    drop(reader);
    #[cfg(any(target_os = "linux", windows))]
    assert!(
        process.wait(Duration::from_secs(3)).unwrap(),
        "the interrupted session's daemon did not exit"
    );
    assert!(
        wait_for_endpoint_removal(&server.endpoint),
        "the interrupted session's endpoint remained present"
    );

    let (stream, retried) = server
        .manager
        .open_workspace(
            &server.workspace,
            &server.endpoint,
            Some(&server.config),
            &[],
        )
        .unwrap();
    assert_ne!(retried.session_id, interrupted.session_id);
    let mut reader = attach_to_pending_session(stream, &retried);
    framing::write(reader.get_mut(), &ClientMessage::ConfirmAttach).unwrap();
    assert!(matches!(
        receive_server_message(&mut reader),
        ServerMessage::AttachConfirmed
    ));
    drop(reader);

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let entries = server.manager.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, retried.session_id);
        if !entries[0].attached {
            break;
        }
        assert!(Instant::now() < deadline, "the terminal stayed attached");
        thread::sleep(Duration::from_millis(10));
    }
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
    let (_stream, reused) = server
        .manager
        .open_workspace(
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
        .open_workspace(
            &server.workspace,
            &server.endpoint,
            Some(&server.config),
            &[],
        )
        .err()
        .expect("a cancelled workspace open succeeded");
    assert_eq!(error.to_string(), "operation was cancelled");
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn cancelling_named_session_startup_reaps_the_spawned_daemon() {
    let server = Server::new();
    let cancellation = CancellationToken::new();
    let (send_pid, pid) = std::sync::mpsc::channel();
    *server.daemons.cancel_after_spawn.lock().unwrap() = Some((cancellation.clone(), send_pid));
    let manager = server.manager.clone().with_cancellation(cancellation);
    let error = manager
        .create(&server.workspace, Some(&server.config), &[], Some("review"))
        .unwrap_err();
    assert_eq!(error.to_string(), "operation was cancelled");
    assert!(!PathBuf::from(format!("/proc/{}", pid.try_recv().unwrap())).exists());
    let endpoint = server
        .endpoint
        .as_path()
        .with_file_name(format!("{}.sock", ServerId::for_name("review")));
    assert!(!SocketPath::from(endpoint).is_present().unwrap());
}

#[test]
fn a_build_mismatch_does_not_start_another_session() {
    let server = Server::new();
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let manager = server.manager.clone();
    let workspace = server.workspace.clone();
    let endpoint = server.endpoint.clone();
    let worker = thread::spawn(move || manager.open_workspace(&workspace, &endpoint, None, &[]));
    let mut stream = listener.accept().unwrap();
    zess_ipc::framing::read(
        &mut BufReader::new(stream.try_clone().unwrap()),
        1024 * 1024,
    )
    .unwrap()
    .unwrap();
    stream
        .write_all(
            &zess_ipc::framing::encode(&zess_protocol::ServerMessage::Status {
                build_id: "different-build".into(),
                session_id: zess_protocol::SessionId("old".into()),
                name: None,
                server_instance: zess_protocol::ServerInstanceId("old-instance".into()),
                state_revision: 0,
                pid: 123,
                workspace: server.workspace.to_string_lossy().into_owned(),
                attached: false,
                modified: true,
                pending: false,
            })
            .unwrap(),
        )
        .unwrap();
    let error = worker
        .join()
        .unwrap()
        .err()
        .expect("a mismatched session was opened");
    assert!(
        error
            .downcast_ref::<zess_protocol::BuildMismatch>()
            .is_some()
    );
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 0);
}

#[test]
fn a_session_with_a_different_build_does_not_block_another_workspace() {
    let server = Server::new();
    let other = server.root.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let new_endpoint = SocketPath::from(server.root.path().join("new.sock"));
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let old_workspace = server.workspace.clone();
    let fixture = thread::spawn(move || {
        let mut stream = listener.accept().unwrap();
        let request = zess_ipc::framing::read(
            &mut BufReader::new(stream.try_clone().unwrap()),
            1024 * 1024,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            zess_ipc::framing::decode::<zess_protocol::ClientMessage>(&request).unwrap(),
            zess_protocol::ClientMessage::Status
        ));
        stream
            .write_all(
                &zess_ipc::framing::encode(&zess_protocol::ServerMessage::Status {
                    build_id: "different-build".into(),
                    session_id: zess_protocol::SessionId("old".into()),
                    name: None,
                    server_instance: zess_protocol::ServerInstanceId("old-instance".into()),
                    state_revision: 0,
                    pid: 123,
                    workspace: old_workspace.to_string_lossy().into_owned(),
                    attached: false,
                    modified: true,
                    pending: false,
                })
                .unwrap(),
            )
            .unwrap();
    });
    let (stream, info) = server
        .manager
        .open_workspace(&other, &new_endpoint, Some(&server.config), &[])
        .unwrap();
    fixture.join().unwrap();
    assert_eq!(Path::new(&info.workspace), other.canonicalize().unwrap());
    assert_eq!(info.build_id, zess_build::BUILD_ID);
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 1);
    drop(stream);
    assert!(wait_for_endpoint_removal(&new_endpoint));
}

#[cfg(unix)]
#[test]
fn workspace_lookup_resolves_symbolic_links() {
    let server = Server::new();
    let started = server.start();
    let link = server.root.path().join("alias");
    std::os::unix::fs::symlink(&server.workspace, &link).unwrap();
    let sessions = server.manager.workspace_sessions(&link).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, started.session_id);
}

#[test]
fn multiple_sessions_remain_visible_when_one_has_a_different_build() {
    let server = Server::new();
    let endpoints = [
        server.endpoint.clone(),
        SocketPath::from(server.root.path().join("unreachable.sock")),
    ];
    let mut fixtures = Vec::new();
    for (index, endpoint) in endpoints.iter().enumerate() {
        let listener = NativeLocalIpc.bind(endpoint).unwrap();
        let workspace = server.workspace.clone();
        fixtures.push(thread::spawn(move || {
            for _ in 0..2 {
                let mut stream = listener.accept().unwrap();
                zess_ipc::framing::read(
                    &mut BufReader::new(stream.try_clone().unwrap()),
                    1024 * 1024,
                )
                .unwrap()
                .unwrap();
                stream
                    .write_all(
                        &zess_ipc::framing::encode(&zess_protocol::ServerMessage::Status {
                            build_id: if index == 0 {
                                "different-build"
                            } else {
                                zess_build::BUILD_ID
                            }
                            .into(),
                            session_id: zess_protocol::SessionId(format!("session-{index}")),
                            name: Some(format!("session-{index}")),
                            server_instance: zess_protocol::ServerInstanceId(format!(
                                "instance-{index}"
                            )),
                            state_revision: 0,
                            pid: 123,
                            workspace: workspace.to_string_lossy().into_owned(),
                            attached: false,
                            modified: false,
                            pending: false,
                        })
                        .unwrap(),
                    )
                    .unwrap();
            }
        }));
    }
    let sessions = server
        .manager
        .workspace_sessions(&server.workspace)
        .unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .iter()
            .any(|session| session.build_id == "different-build")
    );
    let error = server
        .manager
        .open_workspace(&server.workspace, &server.endpoint, None, &[])
        .err()
        .expect("opening a workspace with multiple sessions succeeded");
    assert!(error.to_string().contains("multiple sessions"));
    assert_eq!(server.daemons.spawns.load(Ordering::Relaxed), 0);
    for fixture in fixtures {
        fixture.join().unwrap();
    }
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
    let response = zess_ipc::framing::encode(&zess_protocol::ServerMessage::Status {
        build_id: zess_build::BUILD_ID.to_owned(),
        session_id: zess_protocol::SessionId("fragmented-session".into()),
        name: None,
        server_instance: zess_protocol::ServerInstanceId("fragmented-instance".into()),
        state_revision: 5,
        pid: 42,
        workspace: server.workspace.to_string_lossy().into_owned(),
        attached: false,
        modified: false,
        pending: false,
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

#[test]
fn discovery_skips_a_server_that_closes_before_its_status_response() {
    let server = Server::new();
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let manager = server.manager.clone();
    let worker = thread::spawn(move || manager.entries());
    let stream = listener.accept().unwrap();
    zess_ipc::framing::read(
        &mut BufReader::new(stream.try_clone().unwrap()),
        1024 * 1024,
    )
    .unwrap()
    .unwrap();
    drop(listener);
    drop(stream);
    assert!(worker.join().unwrap().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn discovery_skips_a_socket_left_by_a_dead_server() {
    use std::io::Read;

    let server = Server::new();
    let listener = std::os::unix::net::UnixListener::bind(server.endpoint.as_path()).unwrap();
    let manager = server.manager.clone();
    let worker = thread::spawn(move || manager.entries());
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // Leave part of the request unread so closing the connection can reset it.
    stream.read_exact(&mut [0]).unwrap();
    drop(listener);

    // A concurrently spawned daemon can briefly inherit the listener before exec.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match NativeLocalIpc.connect(&server.endpoint) {
            Ok(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => break,
            _ => panic!("listener did not stop accepting connections"),
        }
    }
    drop(stream);

    assert!(worker.join().unwrap().unwrap().is_empty());
    let (stream, session) = server
        .manager
        .open_workspace(
            &server.workspace,
            &server.endpoint,
            Some(&server.config),
            &[],
        )
        .unwrap();
    assert_eq!(Path::new(&session.endpoint), server.endpoint.as_path());
    drop(stream);
    assert!(wait_for_endpoint_removal(&server.endpoint));
}

#[test]
fn discovery_reports_a_live_server_that_closes_without_a_status_response() {
    let server = Server::new();
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let manager = server.manager.clone();
    let worker = thread::spawn(move || manager.entries());
    let stream = listener.accept().unwrap();
    zess_ipc::framing::read(
        &mut BufReader::new(stream.try_clone().unwrap()),
        1024 * 1024,
    )
    .unwrap()
    .unwrap();
    drop(stream);
    let error = worker.join().unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("did not return a management response")
    );
}

#[test]
fn discovery_reports_an_incomplete_status_response() {
    let server = Server::new();
    let listener = NativeLocalIpc.bind(&server.endpoint).unwrap();
    let manager = server.manager.clone();
    let worker = thread::spawn(move || manager.entries());
    let mut stream = listener.accept().unwrap();
    zess_ipc::framing::read(
        &mut BufReader::new(stream.try_clone().unwrap()),
        1024 * 1024,
    )
    .unwrap()
    .unwrap();
    stream.write_all(b"{\"type\":").unwrap();
    drop(listener);
    drop(stream);
    let error = worker.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("incomplete management response"));
}
