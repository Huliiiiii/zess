use super::*;
use zess_protocol::ClientExitDisposition;

struct Server {
    root: std::path::PathBuf,
    socket: std::path::PathBuf,
    thread: Option<thread::JoinHandle<Result<()>>>,
}

impl Server {
    fn start(policy: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zess-lifecycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let config = root.join("init.w");
        fs::write(
            &config,
            format!("set-session-options! : obj 'on-client-exit '{policy}\n"),
        )
        .unwrap();
        let socket = root.join("server.sock");
        let (workspace, endpoint) = (root.clone(), socket.clone());
        let thread = thread::spawn(move || {
            let mut session = Session::new(workspace, &[], Some(config))?;
            session.set_name("work".into());
            serve(
                &zess_ipc::NativeLocalIpc,
                &SocketPath::from(endpoint),
                session,
            )
        });
        let server = Self {
            root,
            socket,
            thread: Some(thread),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server.socket.exists() {
            assert!(!server.thread.as_ref().unwrap().is_finished());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        server
    }

    fn connect(&self) -> (UnixStream, BufReader<UnixStream>) {
        let stream = UnixStream::connect(&self.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        (stream, reader)
    }

    fn status(&self) -> ServerMessage {
        let (mut stream, mut reader) = self.connect();
        hello(&mut stream, &mut reader);
        framing::write(&mut stream, &ClientMessage::Status).unwrap();
        receive(&mut reader)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket) {
            let _ = framing::write(
                &mut stream,
                &ClientMessage::Hello {
                    build_id: zess_build::BUILD_ID.into(),
                },
            );
            let _ = framing::write(&mut stream, &ClientMessage::Stop { force: true });
        }
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !thread::panicking() {
                result.unwrap().unwrap();
            }
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn receive(reader: &mut BufReader<UnixStream>) -> ServerMessage {
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0);
    serde_json::from_str(&line).unwrap()
}

fn hello(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>) {
    framing::write(
        stream,
        &ClientMessage::Hello {
            build_id: zess_build::BUILD_ID.into(),
        },
    )
    .unwrap();
    assert!(
        matches!(receive(reader), ServerMessage::Hello { build_id } if build_id == zess_build::BUILD_ID)
    );
}

#[test]
fn stopping_requires_a_matching_build_handshake() {
    let server = Server::start("keep");
    let (mut stream, mut reader) = server.connect();
    framing::write(&mut stream, &ClientMessage::Stop { force: true }).unwrap();
    assert!(
        matches!(receive(&mut reader), ServerMessage::Rejected { reason } if reason.contains("Hello"))
    );
    let (mut stream, mut reader) = server.connect();
    framing::write(
        &mut stream,
        &ClientMessage::Hello {
            build_id: "different".into(),
        },
    )
    .unwrap();
    framing::write(&mut stream, &ClientMessage::Stop { force: true }).unwrap();
    assert!(
        matches!(receive(&mut reader), ServerMessage::Hello { build_id } if build_id == zess_build::BUILD_ID)
    );
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    assert!(
        matches!(server.status(), ServerMessage::Status { name: Some(name), .. } if name == "work")
    );
}

#[test]
fn attaching_to_a_different_session_does_not_open_files() {
    let server = Server::start("keep");
    let (mut stream, mut reader) = server.connect();
    hello(&mut stream, &mut reader);
    framing::write(
        &mut stream,
        &ClientMessage::Attach {
            expected_session: Some(SessionId("gone".into())),
            client_id: "client".into(),
            width: 80,
            height: 24,
            files: vec![server.root.join("new-file").to_string_lossy().into_owned()],
            context_epoch: None,
        },
    )
    .unwrap();
    assert!(
        matches!(receive(&mut reader), ServerMessage::Rejected { reason } if reason.contains("session identity"))
    );
    let ServerMessage::Status {
        session_id,
        attached,
        ..
    } = server.status()
    else {
        panic!("status expected")
    };
    assert!(!attached);
    let (mut stream, mut reader) = server.connect();
    hello(&mut stream, &mut reader);
    framing::write(
        &mut stream,
        &ClientMessage::Attach {
            expected_session: Some(session_id),
            client_id: "client".into(),
            width: 80,
            height: 24,
            files: vec![],
            context_epoch: None,
        },
    )
    .unwrap();
    assert!(matches!(
        receive(&mut reader),
        ServerMessage::Attached { .. }
    ));
    let ServerMessage::Snapshot { state, .. } = receive(&mut reader) else {
        panic!("snapshot expected")
    };
    assert_eq!(state.buffers.len(), 1);
    assert!(matches!(
        state.buffers[0].kind,
        zess_protocol::BufferKind::Text { path: None }
    ));
}

#[test]
fn normal_quit_reports_whether_the_session_will_stop() {
    for (policy, expected) in [
        ("keep", ClientExitDisposition::Retained),
        ("stop-if-unused", ClientExitDisposition::Stopping),
    ] {
        let mut server = Server::start(policy);
        let (mut stream, mut reader) = server.connect();
        hello(&mut stream, &mut reader);
        framing::write(
            &mut stream,
            &ClientMessage::Attach {
                expected_session: None,
                client_id: "client".into(),
                width: 80,
                height: 24,
                files: vec![],
                context_epoch: None,
            },
        )
        .unwrap();
        let ServerMessage::Attached {
            server_instance, ..
        } = receive(&mut reader)
        else {
            panic!("attachment expected")
        };
        let ServerMessage::Snapshot { mut state, .. } = receive(&mut reader) else {
            panic!("snapshot expected")
        };
        framing::write(
            &mut stream,
            &ClientMessage::Command {
                request_id: RequestId(1),
                server_instance: server_instance.clone(),
                action: CommandAction::CommandLine { text: "q".into() },
            },
        )
        .unwrap();
        loop {
            match receive(&mut reader) {
                ServerMessage::Delta { delta, .. } => delta.apply(&mut state),
                ServerMessage::CommandResult { .. } => break,
                _ => {}
            }
        }
        let action = &state.client_actions[0];
        let zess_protocol::ClientActionKind::Close { buffer_id, .. } = action.kind else {
            panic!("close expected")
        };
        framing::write(
            &mut stream,
            &ClientMessage::Command {
                request_id: RequestId(2),
                server_instance,
                action: CommandAction::ClientExit {
                    action_id: action.id,
                    buffer_id,
                },
            },
        )
        .unwrap();
        loop {
            if let ServerMessage::Detached { disposition } = receive(&mut reader) {
                assert_eq!(disposition, expected);
                break;
            }
        }
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
        if expected == ClientExitDisposition::Stopping {
            server.thread.take().unwrap().join().unwrap().unwrap();
        } else {
            assert!(matches!(
                server.status(),
                ServerMessage::Status {
                    attached: false,
                    ..
                }
            ));
        }
    }
}
