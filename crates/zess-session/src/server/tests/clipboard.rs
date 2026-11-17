use super::*;
use zess_protocol::{CommandTarget, ViewContent};

struct Server {
    root: std::path::PathBuf,
    socket: std::path::PathBuf,
    thread: Option<thread::JoinHandle<Result<()>>>,
}

impl Server {
    fn start() -> Self {
        Self::start_with_workspace(|_| {})
    }

    fn start_with_workspace(prepare: impl FnOnce(&std::path::Path)) -> Self {
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
        prepare(&root);
        let socket = root.join("server.sock");
        let (workspace, endpoint) = (root.clone(), socket.clone());
        let thread = thread::spawn(move || {
            let mut session = Session::new(workspace, &[file], None)?;
            session.input("\"");
            session.input("+");
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

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    instance: ServerInstanceId,
    session: SessionId,
    epoch: u64,
    target: CommandTarget,
}

impl Client {
    fn attach(server: &Server, client_id: &str) -> (Self, SessionSnapshot) {
        let mut stream = UnixStream::connect(&server.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        framing::write(
            &mut stream,
            &ClientMessage::Hello {
                build_id: zess_build::BUILD_ID.into(),
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
            },
        )
        .unwrap();
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let ServerMessage::Attached {
            server_instance,
            context_epoch,
            ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("client did not attach")
        };
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let ServerMessage::Snapshot {
            session_id, state, ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("client did not receive a semantic snapshot")
        };
        let view = state
            .views
            .iter()
            .find(|view| Some(view.view_id) == state.focused_view)
            .unwrap();
        let target = CommandTarget::View {
            view_id: view.view_id,
            buffer_id: view.buffer_id,
        };
        (
            Self {
                stream,
                reader,
                instance: server_instance,
                session: session_id,
                epoch: context_epoch,
                target,
            },
            state,
        )
    }

    fn command(&self, request_id: RequestId, name: &str) -> ClientMessage {
        ClientMessage::Command {
            request_id,
            server_instance: self.instance.clone(),
            action: CommandAction::Invoke {
                target: self.target.clone(),
                name: name.into(),
                argument: serde_json::json!({}),
            },
        }
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
    let (mut first, original) = Client::attach(&server, "A");
    first.send(first.command(RequestId(1), "text.paste-after"));
    let read_id = loop {
        if let ServerMessage::ClipboardRead { id, .. } = first.receive() {
            break id;
        }
    };
    let first_epoch = first.epoch;
    first.detach();
    let (mut resumed, state) = Client::attach(&server, "A");
    assert_eq!(resumed.epoch, first_epoch);
    assert_eq!(document(&state), document(&original));
    loop {
        if let ServerMessage::ClipboardRead { id, .. } = resumed.receive() {
            assert_eq!(id, read_id);
            break;
        }
    }
    resumed.detach();
    let (mut second, _) = Client::attach(&server, "B");
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
    second.send(second.command(RequestId(1), "text.move-left"));
    assert_eq!(second.result(RequestId(1)), CommandOutcome::Applied);
    let state = second.snapshot_without_device_requests();
    assert_eq!(document(&state), document(&original));
    assert!(!state.modified);
    second.detach();
    let (mut first, _) = Client::attach(&server, "A");
    assert!(
        matches!(first.result(RequestId(1)), CommandOutcome::Failed { message } if message.contains("another client attached"))
    );
}

#[test]
fn a_pending_clipboard_command_survives_early_acknowledgment_and_replay() {
    let server = Server::start();
    let client_id = "clipboard:resume";
    let (mut client, _) = Client::attach(&server, client_id);
    let request_id = RequestId(1);
    let instance = client.instance.clone();
    client.send(client.command(request_id, "text.paste-after"));
    let read_id = loop {
        if let ServerMessage::ClipboardRead { id, .. } = client.receive() {
            break id;
        }
    };
    client.send(ClientMessage::ResultAck {
        request_id,
        server_instance: instance.clone(),
    });
    client.detach();

    let (mut client, _) = Client::attach(&server, client_id);
    assert!(matches!(client.receive(), ServerMessage::CommandResult {
        request_id: received, result: OperationResult::Pending, ..
    } if received == request_id));
    assert!(matches!(client.receive(), ServerMessage::ClipboardRead { id, .. } if id == read_id));

    client.send(client.command(request_id, "text.paste-after"));
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
fn clipboard_effects_arrive_before_the_command_result() {
    for (command, read) in [
        ("text.paste-after", true),
        ("text.yank-to-clipboard", false),
    ] {
        let server = Server::start();
        let (mut client, _) = Client::attach(&server, "device-order");
        client.send(client.command(RequestId(1), command));
        let mut effect_received = false;
        loop {
            match client.receive() {
                ServerMessage::ClipboardRead { .. } if read => effect_received = true,
                ServerMessage::ClipboardWrite { .. } if !read => effect_received = true,
                ServerMessage::CommandResult {
                    request_id: RequestId(1),
                    ..
                } => {
                    assert!(
                        effect_received,
                        "the command result must follow its clipboard effect"
                    );
                    break;
                }
                _ => {}
            }
        }
    }
}

#[test]
fn files_and_git_yanks_deliver_the_copied_values_to_the_requesting_client() {
    use zess_plugin::{HostRequest, ItemNavigation, ItemSelection};
    use zess_protocol::resource::{
        PageCommandInput, PageInput, PageSource, ResourceEvent, ResourceRequest,
    };

    let server = Server::start_with_workspace(|root| {
        for arguments in [
            &["init", "--quiet"][..],
            &["add", "note.txt"],
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        ] {
            let output = std::process::Command::new("git")
                .args(arguments)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::write(root.join("note.txt"), "working\n").unwrap();
        fs::write(root.join("other.txt"), "other\n").unwrap();
    });
    let client_id = "page-yank";
    let (mut client, _) = Client::attach(&server, client_id);
    let mut completed = Vec::new();
    for (id, kind, command, path, register) in [
        (1, "files", "files.yank-content", "note.txt", '+'),
        (2, "git", "git.yank-diff", "file:note.txt", 'a'),
    ] {
        let mut input = PageInput {
            generation: 1,
            selection: ItemSelection::default(),
            mode: "normal".into(),
            filter: None,
            register: None,
            source_buffer: None,
            width: 40,
            active: true,
            preview_width: 39,
            preview_height: 22,
        };
        client.send(ClientMessage::Resource {
            request: ResourceRequest::Subscribe {
                subscription_id: id,
                page_id: zess_protocol::resource::PageId {
                    context_epoch: 0,
                    sequence: id,
                },
                clone_from: None,
                source: PageSource {
                    kind: kind.into(),
                    argument: serde_json::json!({}),
                },
                input: input.clone(),
            },
        });
        loop {
            let ServerMessage::Resource { update } = client.receive() else {
                continue;
            };
            if update.subscription_id == id
                && update
                    .output
                    .display
                    .is_some_and(|display| display.rows.iter().any(|row| row.id == path))
            {
                break;
            }
        }
        let command_input = PageCommandInput {
            generation: input.generation,
            selection: ItemSelection {
                primary: Some(path.into()),
                anchor: Some(path.into()),
                selected: vec![path.into()],
            },
            register: Some(register),
        };
        let other = if kind == "files" {
            "other.txt"
        } else {
            "file:other.txt"
        };
        input.generation += 1;
        input.selection = ItemSelection {
            primary: Some(other.into()),
            anchor: Some(other.into()),
            selected: vec![other.into()],
        };
        input.register = Some('b');
        client.send(ClientMessage::Resource {
            request: ResourceRequest::Update {
                subscription_id: id,
                input,
                event: ResourceEvent::Select {
                    navigation: ItemNavigation::Step,
                },
            },
        });
        let request_id = RequestId(id);
        client.send(ClientMessage::Command {
            request_id,
            server_instance: client.instance.clone(),
            action: CommandAction::Invoke {
                target: CommandTarget::Page {
                    subscription_id: id,
                    input: command_input,
                },
                name: command.into(),
                argument: serde_json::json!({}),
            },
        });
        let outcome = loop {
            match client.receive() {
                ServerMessage::CommandResult {
                    request_id: received,
                    result: OperationResult::Completed { outcome, .. },
                    ..
                } if received == request_id => break outcome,
                ServerMessage::ClipboardWrite { .. }
                | ServerMessage::ClipboardWritePrimary { .. } => {
                    panic!("page yank emitted a separate clipboard write")
                }
                ServerMessage::Resource { update } => assert!(
                    !update
                        .output
                        .requests
                        .iter()
                        .any(|request| matches!(request, HostRequest::Yank { .. }))
                ),
                _ => {}
            }
        };
        let CommandOutcome::Yanked {
            values,
            register: destination,
        } = &outcome
        else {
            panic!("missing yank values in {outcome:?}");
        };
        assert_eq!(*destination, register);
        let [value] = values.as_slice() else {
            panic!("expected one copied value, got {values:?}");
        };
        if kind == "files" {
            assert_eq!(value, "working\n");
        } else {
            assert!(
                value.contains("diff --git a/note.txt b/note.txt"),
                "{value}"
            );
            assert!(value.contains("\n-original\n+working\n"), "{value}");
        }
        completed.push((request_id, outcome));
    }

    client.detach();
    let (mut client, _) = Client::attach(&server, client_id);
    for (request_id, outcome) in completed {
        assert_eq!(client.result(request_id), outcome);
    }
}
