mod documents;
mod page_effects;

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

#[test]
fn session_status_is_available_before_the_build_handshake() {
    let server = Server::start("keep");
    let (mut stream, mut reader) = server.connect();
    framing::write(&mut stream, &ClientMessage::Status).unwrap();
    assert!(
        matches!(receive(&mut reader), ServerMessage::Status { build_id, name: Some(name), workspace, .. } if build_id == zess_build::BUILD_ID && name == "work" && workspace == server.root.to_string_lossy())
    );
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
fn a_new_terminal_recovers_the_last_confirmed_workspace_view() {
    use zess_protocol::workspace_view::{
        Axis, DocumentPanes, DocumentView, DocumentViewState, WorkspaceDraft, WorkspaceView,
    };

    let server = Server::start("stop-if-unused");
    let attach = |client: &str| {
        let (mut stream, mut reader) = server.connect();
        hello(&mut stream, &mut reader);
        framing::write(
            &mut stream,
            &ClientMessage::Attach {
                expected_session: None,
                client_id: client.to_owned(),
                width: 80,
                height: 24,
                files: vec![],
            },
        )
        .unwrap();
        let ServerMessage::Attached { workspace_view, .. } = receive(&mut reader) else {
            panic!("attachment expected")
        };
        let ServerMessage::Snapshot { state, .. } = receive(&mut reader) else {
            panic!("snapshot expected")
        };
        (stream, reader, workspace_view, state)
    };
    let (mut first, mut reader, saved, state) = attach("first");
    assert!(saved.is_none());
    let buffer_id = state.workspace.current.unwrap();
    let mut text_panes = DocumentPanes::new(state.focused_view.unwrap(), buffer_id);
    let (_, pane) = text_panes.split(Axis::Vertical);
    let view = WorkspaceView {
        text_panes: Some(text_panes),
        documents: vec![DocumentView {
            buffer_id,
            view_id: pane.view_id,
            state: DocumentViewState {
                ranges: vec![zess_protocol::TextRange { anchor: 0, head: 0 }],
                primary: 0,
                mode: "insert".into(),
                scroll: 0,
                horizontal_scroll: 0,
            },
        }],
        draft: Some(WorkspaceDraft {
            purpose: "commit-message".into(),
            text: "界".repeat(MAX_INPUT_BYTES / 3 + 1),
            inline: false,
            position: DocumentViewState {
                ranges: vec![zess_protocol::TextRange { anchor: 0, head: 0 }],
                primary: 0,
                mode: "insert".into(),
                scroll: 0,
                horizontal_scroll: 0,
            },
        }),
        ..Default::default()
    };
    let message = ClientMessage::SaveWorkspaceView { view: view.clone() };
    let json = serde_json::to_string(&message).unwrap();
    assert!(json.len() > MAX_INPUT_BYTES);
    for (part, last) in zess_protocol::chunks::json_parts(&json, MAX_INPUT_BYTES) {
        let frame = framing::encode(&ClientMessage::Chunk {
            json: part.into(),
            last,
        })
        .unwrap();
        assert!(frame.len() <= MAX_INPUT_BYTES);
        first.write_all(&frame).unwrap();
    }
    framing::write(&mut first, &ClientMessage::Detach).unwrap();
    loop {
        if matches!(receive(&mut reader), ServerMessage::WorkspaceViewSaved) {
            break;
        }
    }
    assert!(matches!(
        receive(&mut reader),
        ServerMessage::Detached {
            disposition: ClientExitDisposition::Retained
        }
    ));
    let (_, _, restored, _) = attach("second");
    assert_eq!(restored.as_ref(), Some(&view));
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

#[test]
fn file_page_updates_follow_the_subscribed_selection() {
    use zess_plugin::{HostRequest, ItemNavigation, ItemSelection, PreviewContent, PreviewPane};
    use zess_protocol::resource::{PageInput, PageSource, ResourceEvent, ResourceRequest};

    let server = Server::start("keep");
    fs::write(server.root.join("first.txt"), "first document").unwrap();
    fs::write(server.root.join("second.txt"), "second document").unwrap();
    let (mut stream, mut reader) = server.connect();
    hello(&mut stream, &mut reader);
    framing::write(
        &mut stream,
        &ClientMessage::Attach {
            expected_session: None,
            client_id: "files-client".into(),
            width: 80,
            height: 24,
            files: vec![],
        },
    )
    .unwrap();
    while !matches!(receive(&mut reader), ServerMessage::Snapshot { .. }) {}
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
    framing::write(
        &mut stream,
        &ClientMessage::Resource {
            request: ResourceRequest::Subscribe {
                subscription_id: 8,
                page_id: zess_protocol::resource::PageId {
                    context_epoch: 0,
                    sequence: 8,
                },
                clone_from: None,
                source: PageSource {
                    kind: "files".into(),
                    argument: serde_json::json!({}),
                },
                input: input.clone(),
            },
        },
    )
    .unwrap();
    loop {
        let ServerMessage::Resource { update } = receive(&mut reader) else {
            continue;
        };
        if update.subscription_id == 8
            && update.generation == 1
            && update
                .output
                .display
                .is_some_and(|display| display.rows.iter().any(|row| row.id == "second.txt"))
        {
            break;
        }
    }
    input.generation = 2;
    input.selection = ItemSelection {
        primary: Some("second.txt".into()),
        anchor: Some("second.txt".into()),
        selected: vec!["second.txt".into()],
    };
    framing::write(
        &mut stream,
        &ClientMessage::Resource {
            request: ResourceRequest::Update {
                subscription_id: 8,
                input,
                event: ResourceEvent::Select {
                    navigation: ItemNavigation::Step,
                },
            },
        },
    )
    .unwrap();
    loop {
        let ServerMessage::Resource { update } = receive(&mut reader) else {
            continue;
        };
        if update.subscription_id == 8
            && update.generation == 2
            && update.output.requests.iter().any(|request| {
                matches!(request,
                    HostRequest::PreviewGroup { panes, .. }
                        if panes.iter().any(|pane| matches!(pane, PreviewPane::Surface {
                            path, content: PreviewContent::Text { text }, ..
                        } if path == "second.txt" && text == "second document"))
                )
            })
        {
            break;
        }
    }
}
