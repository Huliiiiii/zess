use super::*;
use zess_protocol::ViewContent;

struct Server {
    root: std::path::PathBuf,
    socket: std::path::PathBuf,
    thread: Option<thread::JoinHandle<Result<()>>>,
}

impl Server {
    fn start() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zess-clipboard-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir(&root).unwrap();
        let file = root.join("note.txt");
        fs::write(&file, "original\n").unwrap();
        let socket = root.join("server.sock");
        let (workspace, endpoint) = (root.clone(), socket.clone());
        let thread = thread::spawn(move || {
            serve(
                &zess_ipc::NativeLocalIpc,
                &SocketPath::from(endpoint),
                Session::new(workspace, &[file], None)?,
            )
        });
        let server = Self {
            root,
            socket,
            thread: Some(thread),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while !server.socket.exists() {
            assert!(!server.thread.as_ref().unwrap().is_finished());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        server
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket) {
            let _ = framing::write(
                &mut stream,
                &ClientMessage::Hello {
                    version: zess_build::version().into(),
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

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    instance: ServerInstanceId,
    session: SessionId,
    epoch: u64,
}

impl Client {
    fn attach(
        server: &Server,
        client_id: &str,
        previous_epoch: Option<u64>,
    ) -> (Self, SessionSnapshot) {
        let mut stream = UnixStream::connect(&server.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        framing::write(
            &mut stream,
            &ClientMessage::Hello {
                version: zess_build::version().into(),
            },
        )
        .unwrap();
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(&hello).unwrap(),
            ServerMessage::Hello { .. }
        ));
        framing::write(
            &mut stream,
            &ClientMessage::Attach {
                expected_session: None,
                client_id: client_id.into(),
                width: 80,
                height: 24,
                files: Vec::new(),
                context_epoch: previous_epoch,
            },
        )
        .unwrap();
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let ServerMessage::Attached {
            server_instance,
            context_epoch,
            replay_allowed,
            ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("client did not attach")
        };
        if let Some(epoch) = previous_epoch {
            assert_eq!(replay_allowed, epoch == context_epoch);
        }
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let ServerMessage::Snapshot {
            session_id, state, ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("client did not receive a semantic snapshot")
        };
        (
            Self {
                stream,
                reader,
                instance: server_instance,
                session: session_id,
                epoch: context_epoch,
            },
            state,
        )
    }

    fn send(&mut self, message: ClientMessage) {
        framing::write(&mut self.stream, &message).unwrap();
    }

    fn receive(&mut self) -> ServerMessage {
        let mut line = String::new();
        assert!(self.reader.read_line(&mut line).unwrap() > 0);
        let message = serde_json::from_str(&line).unwrap();
        assert!(!matches!(message, ServerMessage::Rejected { .. }));
        message
    }

    fn result(&mut self, request: RequestId) -> CommandOutcome {
        loop {
            if let ServerMessage::CommandResult {
                request_id,
                result: OperationResult::Completed { outcome, .. },
                ..
            } = self.receive()
            {
                if request_id == request {
                    return outcome;
                }
            }
        }
    }

    fn snapshot_without_device_requests(&mut self) -> SessionSnapshot {
        self.send(ClientMessage::Resync {
            session_id: self.session.clone(),
            state_revision: 0,
        });
        loop {
            match self.receive() {
                ServerMessage::Snapshot { state, .. } => return state,
                ServerMessage::ClipboardRead { .. } => {
                    panic!(
                        "a different client must not receive the previous client's clipboard operation"
                    )
                }
                _ => {}
            }
        }
    }

    fn detach(mut self) {
        self.send(ClientMessage::Detach);
        let mut line = String::new();
        while self.reader.read_line(&mut line).unwrap() != 0 {
            line.clear();
        }
    }
}

fn document(state: &SessionSnapshot) -> &ViewContent {
    &state
        .views
        .iter()
        .find(|view| Some(view.view_id) == state.focused_view)
        .unwrap()
        .content
}

#[test]
fn changing_clients_cancels_a_clipboard_read_while_the_same_client_can_resume_it() {
    let server = Server::start();
    let (mut first, original) = Client::attach(&server, "A", None);
    for (sequence, key) in [(1, "\""), (2, "+")] {
        first.send(ClientMessage::Input {
            sequence,
            key: key.into(),
            context_epoch: first.epoch,
        });
    }
    first.send(ClientMessage::TrackedInput {
        request_id: RequestId(1),
        server_instance: first.instance.clone(),
        sequence: 3,
        key: "p".into(),
        context_epoch: first.epoch,
    });
    let read_id = loop {
        if let ServerMessage::ClipboardRead { id, .. } = first.receive() {
            break id;
        }
    };
    let first_epoch = first.epoch;
    first.detach();
    let (mut resumed, state) = Client::attach(&server, "A", Some(first_epoch));
    assert_eq!(resumed.epoch, first_epoch);
    assert_eq!(document(&state), document(&original));
    loop {
        if let ServerMessage::ClipboardRead { id, .. } = resumed.receive() {
            assert_eq!(id, read_id);
            break;
        }
    }
    resumed.detach();
    let (mut second, _) = Client::attach(&server, "B", None);
    assert_ne!(second.epoch, first_epoch);
    second.snapshot_without_device_requests();
    second.send(ClientMessage::ClipboardResponse {
        id: read_id,
        text: Some("B clipboard".into()),
        error: None,
    });
    second.send(ClientMessage::ResultAck {
        request_id: RequestId(1),
        server_instance: second.instance.clone(),
    });
    second.send(ClientMessage::Command {
        request_id: RequestId(1),
        server_instance: second.instance.clone(),
        action: CommandAction::Command {
            name: "text.move-left".into(),
            argument: serde_json::json!({}),
        },
    });
    assert_eq!(second.result(RequestId(1)), CommandOutcome::Applied);
    let state = second.snapshot_without_device_requests();
    assert_eq!(document(&state), document(&original));
    assert!(!state.modified);
    second.detach();
    let (mut first, _) = Client::attach(&server, "A", Some(first_epoch));
    assert!(
        matches!(first.result(RequestId(1)), CommandOutcome::Failed { message } if message.contains("another client attached"))
    );
}

#[test]
fn a_pending_clipboard_command_survives_early_acknowledgment_and_replay() {
    let server = Server::start();
    let client_id = "clipboard:resume";
    let (mut client, _) = Client::attach(&server, client_id, None);
    for (sequence, key) in [(1, "\""), (2, "+")] {
        client.send(ClientMessage::Input {
            sequence,
            key: key.into(),
            context_epoch: client.epoch,
        });
    }
    let request_id = RequestId(1);
    let instance = client.instance.clone();
    let paste = || ClientMessage::Command {
        request_id,
        server_instance: instance.clone(),
        action: CommandAction::Command {
            name: "text.paste-after".into(),
            argument: serde_json::json!({}),
        },
    };
    client.send(paste());
    let read_id = loop {
        if let ServerMessage::ClipboardRead { id, .. } = client.receive() {
            break id;
        }
    };
    client.send(ClientMessage::ResultAck {
        request_id,
        server_instance: instance.clone(),
    });
    let epoch = client.epoch;
    client.detach();

    let (mut client, _) = Client::attach(&server, client_id, Some(epoch));
    assert!(matches!(client.receive(), ServerMessage::CommandResult {
        request_id: received, result: OperationResult::Pending, ..
    } if received == request_id));
    assert!(matches!(client.receive(), ServerMessage::ClipboardRead { id, .. } if id == read_id));

    client.send(paste());
    assert!(matches!(client.receive(), ServerMessage::ClipboardRead { id, .. } if id == read_id));
    assert!(matches!(client.receive(), ServerMessage::CommandResult {
        request_id: received, result: OperationResult::Pending, ..
    } if received == request_id));
    client.send(ClientMessage::ClipboardResponse {
        id: read_id,
        text: Some("paste".into()),
        error: None,
    });
    assert_eq!(client.result(request_id), CommandOutcome::Applied);
}

#[test]
fn clipboard_effects_arrive_before_the_input_or_command_result() {
    for tracked in [false, true] {
        for read in [false, true] {
            let server = Server::start();
            let (mut client, _) = Client::attach(&server, "device-order", None);
            for (sequence, key) in [(1, "\""), (2, "+")] {
                client.send(ClientMessage::Input {
                    sequence,
                    key: key.into(),
                    context_epoch: client.epoch,
                });
            }
            loop {
                if matches!(
                    client.receive(),
                    ServerMessage::Ack {
                        acknowledged: 2,
                        ..
                    }
                ) {
                    break;
                }
            }
            let key = if read { "p" } else { "y" }.to_owned();
            client.send(if tracked {
                ClientMessage::TrackedInput {
                    request_id: RequestId(1),
                    server_instance: client.instance.clone(),
                    sequence: 3,
                    key,
                    context_epoch: client.epoch,
                }
            } else {
                ClientMessage::Input {
                    sequence: 3,
                    key,
                    context_epoch: client.epoch,
                }
            });
            let mut effect_received = false;
            loop {
                match client.receive() {
                    ServerMessage::ClipboardRead { .. } if read => effect_received = true,
                    ServerMessage::ClipboardWrite { .. } if !read => effect_received = true,
                    ServerMessage::Ack {
                        acknowledged: 3, ..
                    } => {
                        assert!(
                            effect_received,
                            "the prefix ACK must follow its clipboard effect"
                        );
                        if !tracked {
                            break;
                        }
                    }
                    ServerMessage::CommandResult {
                        request_id: RequestId(1),
                        ..
                    } => {
                        assert!(
                            effect_received,
                            "the tracked result must follow its clipboard effect"
                        );
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}
