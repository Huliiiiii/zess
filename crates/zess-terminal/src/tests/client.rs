mod clipboard_order;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use zess_protocol::{OperationResult, SessionId, SessionSnapshot};

use super::*;

struct IdentityPlatform;

impl Platform for IdentityPlatform {
    fn host_info(&self) -> zess_platform::HostInfo {
        panic!("unexpected host information request")
    }
    fn user_dirs(&self) -> Result<zess_platform::UserDirs> {
        panic!("unexpected user directory request")
    }
    fn current_dir(&self) -> io::Result<PathBuf> {
        panic!("unexpected current directory request")
    }
    fn current_exe(&self) -> io::Result<PathBuf> {
        panic!("unexpected executable request")
    }
    fn absolute_path(&self, path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        NativePlatform.absolute_path(path)
    }
    fn process_id(&self) -> u32 {
        4242
    }
    fn system_time(&self) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + Duration::from_nanos(9000)
    }
}

struct ScriptedStream {
    phase: Rc<Cell<usize>>,
    batches: VecDeque<(usize, VecDeque<u8>)>,
    output: Rc<RefCell<Vec<u8>>>,
    close_at: Option<usize>,
    fence_probes: std::collections::HashSet<RequestId>,
    fence_reply_after: usize,
    waited: Rc<Cell<usize>>,
    exit_result: bool,
    session_id: Option<SessionId>,
}

impl ScriptedStream {
    fn new(
        phase: Rc<Cell<usize>>,
        messages: Vec<(usize, Vec<ServerMessage>)>,
        close_at: Option<usize>,
    ) -> Self {
        let batches = std::iter::once((
            0,
            vec![ServerMessage::Hello {
                build_id: zess_build::BUILD_ID.into(),
            }],
        ))
        .chain(messages)
        .map(|(phase, messages)| {
            let bytes = messages
                .into_iter()
                .flat_map(|message| framing::encode(&message).unwrap())
                .collect();
            (phase, bytes)
        })
        .collect();
        Self {
            phase,
            batches,
            output: Rc::default(),
            close_at,
            fence_probes: std::collections::HashSet::new(),
            fence_reply_after: 0,
            waited: Rc::default(),
            exit_result: true,
            session_id: None,
        }
    }
}

impl Read for ScriptedStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if let Some((phase, batch)) = self.batches.front_mut()
            && *phase <= self.phase.get()
        {
            let count = batch.len().min(bytes.len());
            for byte in &mut bytes[..count] {
                *byte = batch.pop_front().unwrap();
            }
            if batch.is_empty() {
                self.batches.pop_front();
            }
            return Ok(count);
        }
        if self.close_at.is_some_and(|phase| self.phase.get() >= phase) {
            Ok(0)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

impl Write for ScriptedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.borrow_mut().extend_from_slice(bytes);
        let probes = self
            .output
            .borrow()
            .split_inclusive(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<ClientMessage>(line).ok())
            .filter_map(|message| match message {
                ClientMessage::QueryOperation {
                    request_id,
                    server_instance,
                } if request_id.0.ends_with(".clipboard-fence") => {
                    Some((request_id, server_instance))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (request_id, server_instance) in probes {
            if self.fence_probes.insert(request_id.clone()) {
                let phase = self.phase.get().max(self.fence_reply_after);
                let index = self
                    .batches
                    .iter()
                    .position(|(at, _)| *at > phase)
                    .unwrap_or(self.batches.len());
                self.batches.insert(
                    index,
                    (
                        phase,
                        framing::encode(&ServerMessage::CommandResult {
                            request_id,
                            server_instance,
                            result: OperationResult::Unknown,
                        })
                        .unwrap()
                        .into(),
                    ),
                );
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ConnectionStream for ScriptedStream {
    fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }
    fn wait_for_server_exit(&mut self, _: Duration) -> Result<bool> {
        self.waited.set(self.waited.get() + 1);
        Ok(self.exit_result)
    }
    fn set_nonblocking(&self, _: bool) -> io::Result<()> {
        Ok(())
    }
}

struct ScriptedFactory(VecDeque<ScriptedStream>);

impl ConnectionFactory for ScriptedFactory {
    fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
        Ok(Box::new(
            self.0.pop_front().context("unexpected reconnect")?,
        ))
    }
}

struct ScriptedTerminal {
    phase: Rc<Cell<usize>>,
    events: VecDeque<Option<Event>>,
    wait_at: Option<usize>,
    output: Vec<u8>,
    mouse: bool,
}

impl TerminalIo for ScriptedTerminal {
    type Writer = Vec<u8>;
    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.output
    }
    fn size(&self) -> Result<(u16, u16)> {
        Ok((80, 24))
    }
    fn mouse_capture(&self) -> bool {
        self.mouse
    }
    fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        self.mouse = enabled;
        Ok(())
    }
    fn poll_event(&mut self, _: Duration) -> Result<Option<Event>> {
        if self.wait_at == Some(self.phase.get()) {
            std::thread::sleep(RECONNECT_INTERVAL + Duration::from_millis(20));
        }
        self.phase.set(self.phase.get() + 1);
        self.events
            .pop_front()
            .context("client did not finish within the scripted interaction")
    }
}

struct UnusedClipboard;
struct UnusedRead;
struct UnusedWrite;
impl ClipboardReadJob for UnusedRead {
    fn poll(&mut self) -> Option<Result<String>> {
        None
    }
}
impl ClipboardWriteJob for UnusedWrite {
    fn poll(&mut self) -> Option<Result<()>> {
        None
    }
}
impl Clipboard for UnusedClipboard {
    type ReadJob = UnusedRead;
    type WriteJob = UnusedWrite;
    fn read(&mut self, _: bool) -> Result<Self::ReadJob> {
        anyhow::bail!("unexpected clipboard read")
    }
    fn write(&mut self, _: bool, _: String, _: &mut impl Write) -> Result<Option<Self::WriteJob>> {
        anyhow::bail!("unexpected clipboard write")
    }
}

fn attached() -> ServerMessage {
    ServerMessage::Attached {
        server_instance: ServerInstanceId("server".into()),
        acknowledged: 0,
        context_epoch: 5,
        replay_allowed: true,
        frame: Frame::default(),
    }
}

fn snapshot(revision: u64, state: SessionSnapshot) -> ServerMessage {
    ServerMessage::Snapshot {
        session_id: SessionId("session".into()),
        server_instance: ServerInstanceId("server".into()),
        state_revision: revision,
        state,
        frame: Frame::default(),
    }
}

fn key(code: KeyCode) -> Option<Event> {
    Some(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn messages(output: &Rc<RefCell<Vec<u8>>>) -> Vec<ClientMessage> {
    output
        .borrow()
        .split_inclusive(|byte| *byte == b'\n')
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .collect()
}

#[test]
fn quitting_the_only_clean_scratch_exits_the_client() {
    let phase = Rc::new(Cell::new(0));
    let state = SessionSnapshot {
        buffers: vec![zess_protocol::BufferSummary {
            id: 7,
            kind: zess_protocol::BufferKind::Text { path: None },
            modified: false,
        }],
        workspace: zess_protocol::ClientWorkspace {
            buffers: vec![7],
            current: Some(7),
        },
        client_actions: vec![zess_protocol::ClientAction {
            id: 9,
            kind: zess_protocol::ClientActionKind::Close {
                buffer_id: 7,
                force: false,
            },
        }],
        ..Default::default()
    };
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, state)]),
            (2, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    assert!(messages(&output).iter().any(|message| matches!(
        message,
        ClientMessage::Command {
            action: zess_protocol::CommandAction::ClientExit {
                action_id: 9,
                buffer_id: 7
            },
            ..
        }
    )));
}

#[test]
fn quitting_a_file_requests_a_replacement_scratch() {
    let phase = Rc::new(Cell::new(0));
    let state = SessionSnapshot {
        buffers: vec![zess_protocol::BufferSummary {
            id: 7,
            kind: zess_protocol::BufferKind::Text {
                path: Some("note.txt".into()),
            },
            modified: false,
        }],
        workspace: zess_protocol::ClientWorkspace {
            buffers: vec![7],
            current: Some(7),
        },
        client_actions: vec![zess_protocol::ClientAction {
            id: 9,
            kind: zess_protocol::ClientActionKind::Close {
                buffer_id: 7,
                force: false,
            },
        }],
        ..Default::default()
    };
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, state)]),
            (2, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    assert!(messages(&output).iter().any(|message| matches!(
        message,
        ClientMessage::Command {
            action: zess_protocol::CommandAction::CloseBuffer {
                action_id: 9,
                buffer_id: 7,
                replacement: None,
                force: false
            },
            ..
        }
    )));
}

#[test]
fn workspace_events_change_the_client_quit_decision() {
    let phase = Rc::new(Cell::new(0));
    let state = SessionSnapshot {
        buffers: vec![
            zess_protocol::BufferSummary {
                id: 7,
                kind: zess_protocol::BufferKind::Text { path: None },
                modified: false,
            },
            zess_protocol::BufferSummary {
                id: 8,
                kind: zess_protocol::BufferKind::Text {
                    path: Some("note.txt".into()),
                },
                modified: false,
            },
        ],
        workspace: zess_protocol::ClientWorkspace {
            buffers: vec![7],
            current: Some(7),
        },
        client_actions: vec![
            zess_protocol::ClientAction {
                id: 1,
                kind: zess_protocol::ClientActionKind::Open { buffer_id: 8 },
            },
            zess_protocol::ClientAction {
                id: 2,
                kind: zess_protocol::ClientActionKind::Close {
                    buffer_id: 7,
                    force: false,
                },
            },
        ],
        ..Default::default()
    };
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, state)]),
            (
                2,
                vec![ServerMessage::CommandResult {
                    request_id: RequestId("client-1".into()),
                    server_instance: ServerInstanceId("server".into()),
                    result: OperationResult::Completed {
                        outcome: zess_protocol::CommandOutcome::Applied,
                        state_revision: 1,
                    },
                }],
            ),
            (5, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([const { None }; 5]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_client(
        &mut factory,
        "client".into(),
        Vec::new(),
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();
    assert!(messages(&output).iter().any(|message| matches!(
        message,
        ClientMessage::Command {
            action: zess_protocol::CommandAction::CloseBuffer {
                action_id: 2,
                buffer_id: 7,
                replacement: Some(8),
                ..
            },
            ..
        }
    )));
}

#[test]
fn an_unknown_workspace_action_is_submitted_again() {
    let phase = Rc::new(Cell::new(0));
    let state = SessionSnapshot {
        buffers: vec![zess_protocol::BufferSummary {
            id: 7,
            kind: zess_protocol::BufferKind::Text { path: None },
            modified: false,
        }],
        workspace: zess_protocol::ClientWorkspace {
            buffers: vec![7],
            current: Some(7),
        },
        client_actions: vec![zess_protocol::ClientAction {
            id: 9,
            kind: zess_protocol::ClientActionKind::Close {
                buffer_id: 7,
                force: false,
            },
        }],
        ..Default::default()
    };
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, state)]),
            (
                2,
                vec![ServerMessage::CommandResult {
                    request_id: RequestId("client-1".into()),
                    server_instance: ServerInstanceId("server".into()),
                    result: OperationResult::Unknown,
                }],
            ),
            (5, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([const { None }; 5]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_client(
        &mut factory,
        "client".into(),
        Vec::new(),
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();
    assert_eq!(
        messages(&output)
            .iter()
            .filter(|message| matches!(
                message,
                ClientMessage::Command {
                    action: zess_protocol::CommandAction::ClientExit { action_id: 9, .. },
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn resynchronization_restores_the_workspace_before_quitting() {
    let phase = Rc::new(Cell::new(0));
    let initial = SessionSnapshot {
        workspace: zess_protocol::ClientWorkspace {
            buffers: vec![7],
            current: Some(7),
        },
        ..Default::default()
    };
    let restored = SessionSnapshot {
        buffers: vec![zess_protocol::BufferSummary {
            id: 8,
            kind: zess_protocol::BufferKind::Text { path: None },
            modified: false,
        }],
        workspace: zess_protocol::ClientWorkspace {
            buffers: vec![8],
            current: Some(8),
        },
        client_actions: vec![zess_protocol::ClientAction {
            id: 9,
            kind: zess_protocol::ClientActionKind::Close {
                buffer_id: 8,
                force: false,
            },
        }],
        ..Default::default()
    };
    let delta = zess_protocol::SessionDelta::between(&initial, &restored);
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, initial)]),
            (
                1,
                vec![ServerMessage::Delta {
                    session_id: SessionId("session".into()),
                    server_instance: ServerInstanceId("server".into()),
                    base_revision: 3,
                    state_revision: 4,
                    delta,
                    frame: None,
                }],
            ),
            (2, vec![snapshot(4, restored)]),
            (5, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([const { None }; 5]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_client(
        &mut factory,
        "client".into(),
        Vec::new(),
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();
    assert!(messages(&output).iter().any(|message| matches!(
        message,
        ClientMessage::Command {
            action: zess_protocol::CommandAction::ClientExit {
                action_id: 9,
                buffer_id: 8
            },
            ..
        }
    )));
}

#[test]
fn input_waits_for_the_snapshot_and_pauses_until_a_revision_gap_is_resynchronized() {
    let phase = Rc::new(Cell::new(0));
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached()]),
            (1, vec![snapshot(1, SessionSnapshot::default())]),
            (
                2,
                vec![ServerMessage::Delta {
                    session_id: SessionId("session".into()),
                    server_instance: ServerInstanceId("server".into()),
                    base_revision: 3,
                    state_revision: 4,
                    delta: zess_protocol::SessionDelta::between(
                        &SessionSnapshot::default(),
                        &SessionSnapshot::default(),
                    ),
                    frame: None,
                }],
            ),
            (3, vec![snapshot(4, SessionSnapshot::default())]),
            (4, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('x')),
            key(KeyCode::Char('y')),
            key(KeyCode::Char('z')),
            key(KeyCode::Char('w')),
        ]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let messages = messages(&output);
    let keys = messages
        .iter()
        .filter_map(|message| match message {
            ClientMessage::Input { key, .. } => Some(key.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(keys, ["y", "w"]);
    assert_eq!(messages.iter().filter(|message| matches!(message, ClientMessage::Resync { session_id, state_revision: 1 } if session_id.0 == "session")).count(), 1);
}

#[test]
fn reconnect_queries_an_acknowledged_command_instead_of_executing_it_again() {
    let phase = Rc::new(Cell::new(0));
    let state = SessionSnapshot::default();
    let first = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, state)]),
            (
                1,
                vec![ServerMessage::Ack {
                    acknowledged: 1,
                    context_epoch: 5,
                }],
            ),
        ],
        Some(2),
    );
    let first_output = first.output.clone();
    // The second stream derives the request ID from the actual first request.
    let second_output = Rc::<RefCell<Vec<u8>>>::default();
    struct ReconnectFactory {
        first: Option<ScriptedStream>,
        phase: Rc<Cell<usize>>,
        first_output: Rc<RefCell<Vec<u8>>>,
        second_output: Rc<RefCell<Vec<u8>>>,
    }
    impl ConnectionFactory for ReconnectFactory {
        fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
            if let Some(first) = self.first.take() {
                return Ok(Box::new(first));
            }
            let request_id = messages(&self.first_output)
                .into_iter()
                .find_map(|message| match message {
                    ClientMessage::TrackedInput { request_id, .. } => Some(request_id),
                    _ => None,
                })
                .context("missing tracked command")?;
            let mut attached = attached();
            if let ServerMessage::Attached { acknowledged, .. } = &mut attached {
                *acknowledged = 1;
            }
            let result = ServerMessage::CommandResult {
                request_id,
                server_instance: ServerInstanceId("server".into()),
                result: OperationResult::Completed {
                    outcome: zess_protocol::CommandOutcome::Saved {
                        buffer_id: 1,
                        version: 2,
                    },
                    state_revision: 2,
                },
            };
            let mut stream = ScriptedStream::new(
                self.phase.clone(),
                vec![
                    (
                        3,
                        vec![attached, snapshot(2, SessionSnapshot::default()), result],
                    ),
                    (4, vec![ServerMessage::Exited]),
                ],
                None,
            );
            stream.output = self.second_output.clone();
            Ok(Box::new(stream))
        }
    }
    let mut factory = ReconnectFactory {
        first: Some(first),
        phase: phase.clone(),
        first_output: first_output.clone(),
        second_output: second_output.clone(),
    };
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([key(KeyCode::Enter), None, None, None]),
        wait_at: Some(2),
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices_and_platform(
        &mut factory,
        &[],
        &mut terminal,
        &mut UnusedClipboard,
        &IdentityPlatform,
    )
    .unwrap();
    let first_messages = messages(&first_output);
    let request_id = first_messages
        .iter()
        .find_map(|message| match message {
            ClientMessage::TrackedInput { request_id, .. } => Some(request_id),
            _ => None,
        })
        .unwrap();
    let client_id = first_messages
        .iter()
        .find_map(|message| match message {
            ClientMessage::Attach { client_id, .. } => Some(client_id),
            _ => None,
        })
        .unwrap();
    assert!(client_id.starts_with("4242-9000-"));
    assert_eq!(request_id.0, format!("{client_id}-1"));
    let second_messages = messages(&second_output);
    assert!(second_messages.iter().any(|message| matches!(message, ClientMessage::Attach { client_id: actual, .. } if actual == client_id)));
    assert!(second_messages.iter().any(|message| matches!(message, ClientMessage::QueryOperation { request_id: actual, .. } if actual == request_id)));
    assert!(second_messages.iter().any(|message| matches!(message, ClientMessage::ResultAck { request_id: actual, .. } if actual == request_id)));
    assert!(!second_messages.iter().any(|message| matches!(
        message,
        ClientMessage::Input { .. } | ClientMessage::TrackedInput { .. }
    )));
}

#[test]
fn the_display_resolves_text_from_the_cached_snapshot_and_delta() {
    let phase = Rc::new(Cell::new(0));
    let mut state = SessionSnapshot {
        surfaces: vec![zess_protocol::SurfaceSnapshot {
            page_id: 1,
            slot_id: "preview".into(),
            buffer_id: 7,
            header: String::new(),
            loading: false,
            content: zess_protocol::ViewContent::Text {
                lines: vec![zess_protocol::VisibleTextLine {
                    line: 0,
                    first_char: 0,
                    text: "cached-one".into(),
                }],
            },
            scroll: 0,
            horizontal_scroll: 0,
        }],
        render_plan: zess_ui::RenderPlan {
            width: 80,
            height: 24,
            rows: vec![zess_ui::RenderRow {
                spans: vec![zess_ui::RenderSpan {
                    base_style: None,
                    style: String::new(),
                    content: zess_ui::RenderContent::Text {
                        buffer_id: 7,
                        start_char: 0,
                        end_char: 10,
                    },
                }],
            }],
            ..Default::default()
        },
        ..SessionSnapshot::default()
    };
    let mut initial = snapshot(1, state.clone());
    let wrong_frame = Frame {
        width: 80,
        height: 24,
        rows: vec![FrameRow {
            spans: vec![zess_ui::Span {
                base_style: None,
                style: String::new(),
                text: "unused-wire-frame".into(),
            }],
        }],
        ..Frame::default()
    };
    if let ServerMessage::Snapshot { frame, .. } = &mut initial {
        *frame = wrong_frame.clone();
    }
    let previous = state.clone();
    if let zess_protocol::ViewContent::Text { lines } = &mut state.surfaces[0].content {
        lines[0].text = "cached-two".into();
    }
    let delta = zess_protocol::SessionDelta::between(&previous, &state);
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), initial]),
            (
                1,
                vec![ServerMessage::Delta {
                    session_id: SessionId("session".into()),
                    server_instance: ServerInstanceId("server".into()),
                    base_revision: 1,
                    state_revision: 2,
                    delta,
                    frame: Some(wrong_frame),
                }],
            ),
            (2, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let display = String::from_utf8(terminal.output).unwrap();
    assert!(display.contains("cached-one"));
    assert!(display.contains("cached-two"));
    assert!(!display.contains("unused-wire-frame"));
}

#[test]
fn pasted_text_is_submitted_as_one_paste_operation() {
    let phase = Rc::new(Cell::new(0));
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
            (1, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([Some(Event::Paste("hello\n:q".into()))]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let messages = messages(&output);
    assert_eq!(messages.iter().filter(|message| matches!(message, ClientMessage::Command { action: zess_protocol::CommandAction::Paste { text }, .. } if text == "hello\n:q")).count(), 1);
    assert!(!messages.iter().any(|message| matches!(
        message,
        ClientMessage::Input { .. } | ClientMessage::TrackedInput { .. }
    )));
}

#[test]
fn oversized_paste_is_rejected_without_losing_the_connection() {
    let phase = Rc::new(Cell::new(0));
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
            (2, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            Some(Event::Paste("\n".repeat(MAX_INPUT_BYTES / 2))),
            key(KeyCode::Char('x')),
        ]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let messages = messages(&output);
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, ClientMessage::Command { .. }))
    );
    assert!(
        messages
            .iter()
            .any(|message| matches!(message, ClientMessage::Input { key, .. } if key == "x"))
    );
    assert!(
        String::from_utf8(terminal.output)
            .unwrap()
            .contains("Command exceeds the input byte limit")
    );
}

#[test]
fn a_missing_render_source_requests_a_snapshot_before_accepting_more_input() {
    let phase = Rc::new(Cell::new(0));
    let mut invalid = SessionSnapshot::default();
    invalid.render_plan.rows = vec![zess_ui::RenderRow {
        spans: vec![zess_ui::RenderSpan {
            base_style: None,
            style: String::new(),
            content: zess_ui::RenderContent::Text {
                buffer_id: 999,
                start_char: 0,
                end_char: 1,
            },
        }],
    }];
    let delta = zess_protocol::SessionDelta::between(&SessionSnapshot::default(), &invalid);
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
            (
                1,
                vec![ServerMessage::Delta {
                    session_id: SessionId("session".into()),
                    server_instance: ServerInstanceId("server".into()),
                    base_revision: 1,
                    state_revision: 2,
                    delta,
                    frame: None,
                }],
            ),
            (2, vec![snapshot(3, SessionSnapshot::default())]),
            (3, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('a')),
            key(KeyCode::Char('b')),
            key(KeyCode::Char('c')),
        ]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let messages = messages(&output);
    let keys = messages
        .iter()
        .filter_map(|message| match message {
            ClientMessage::Input { key, .. } => Some(key.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(keys, ["a", "c"]);
    assert!(messages.iter().any(|message| matches!(
        message,
        ClientMessage::Resync {
            state_revision: 1,
            ..
        }
    )));
}

#[test]
fn an_unknown_command_result_is_not_retried_automatically() {
    let phase = Rc::new(Cell::new(0));
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(0, vec![attached(), snapshot(1, SessionSnapshot::default())])],
        Some(2),
    );
    let first_output = first.output.clone();
    let second_output = Rc::<RefCell<Vec<u8>>>::default();
    struct ReconnectFactory {
        first: Option<ScriptedStream>,
        phase: Rc<Cell<usize>>,
        first_output: Rc<RefCell<Vec<u8>>>,
        second_output: Rc<RefCell<Vec<u8>>>,
    }
    impl ConnectionFactory for ReconnectFactory {
        fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
            if let Some(first) = self.first.take() {
                return Ok(Box::new(first));
            }
            let request_id = messages(&self.first_output)
                .into_iter()
                .find_map(|message| match message {
                    ClientMessage::Command { request_id, .. } => Some(request_id),
                    _ => None,
                })
                .context("missing paste command")?;
            let unknown = ServerMessage::CommandResult {
                request_id: request_id.clone(),
                server_instance: ServerInstanceId("server".into()),
                result: OperationResult::Unknown,
            };
            let completed = ServerMessage::CommandResult {
                request_id,
                server_instance: ServerInstanceId("server".into()),
                result: OperationResult::Completed {
                    outcome: zess_protocol::CommandOutcome::Applied,
                    state_revision: 2,
                },
            };
            let mut stream = ScriptedStream::new(
                self.phase.clone(),
                vec![
                    (
                        3,
                        vec![attached(), snapshot(1, SessionSnapshot::default()), unknown],
                    ),
                    (4, vec![completed]),
                    (5, vec![ServerMessage::Exited]),
                ],
                None,
            );
            stream.output = self.second_output.clone();
            Ok(Box::new(stream))
        }
    }
    let mut factory = ReconnectFactory {
        first: Some(first),
        phase: phase.clone(),
        first_output: first_output.clone(),
        second_output: second_output.clone(),
    };
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            Some(Event::Paste("one paste".into())),
            None,
            None,
            None,
            None,
        ]),
        wait_at: Some(2),
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let first_messages = messages(&first_output);
    let request_id = first_messages
        .iter()
        .find_map(|message| match message {
            ClientMessage::Command { request_id, .. } => Some(request_id),
            _ => None,
        })
        .unwrap();
    let second_messages = messages(&second_output);
    assert!(second_messages.iter().any(|message| matches!(message, ClientMessage::QueryOperation { request_id: actual, .. } if actual == request_id)));
    assert!(
        !second_messages
            .iter()
            .any(|message| matches!(message, ClientMessage::Command { .. }))
    );
    assert!(
        String::from_utf8(terminal.output)
            .unwrap()
            .contains("outcome is unknown")
    );
}

#[test]
fn paste_received_before_synchronization_is_not_replayed_later() {
    {
        let phase = Rc::new(Cell::new(0));
        let gap = ServerMessage::Delta {
            session_id: SessionId("session".into()),
            server_instance: ServerInstanceId("server".into()),
            base_revision: 3,
            state_revision: 4,
            delta: zess_protocol::SessionDelta::between(
                &SessionSnapshot::default(),
                &SessionSnapshot::default(),
            ),
            frame: None,
        };
        let stream = ScriptedStream::new(
            phase.clone(),
            vec![
                (0, vec![attached()]),
                (1, vec![snapshot(1, SessionSnapshot::default())]),
                (2, vec![gap]),
                (3, vec![snapshot(4, SessionSnapshot::default())]),
                (4, vec![ServerMessage::Exited]),
            ],
            None,
        );
        let output = stream.output.clone();
        let mut factory = ScriptedFactory(VecDeque::from([stream]));
        let events = ["before", "ready", "gap", "restored"]
            .into_iter()
            .map(|text| Some(Event::Paste(text.into())))
            .collect();
        let mut terminal = ScriptedTerminal {
            phase,
            events,
            wait_at: None,
            output: Vec::new(),
            mouse: false,
        };
        run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard)
            .unwrap();
        let messages = messages(&output);
        let commands = messages
            .iter()
            .filter_map(|message| match message {
                ClientMessage::Command { action, .. } => Some(action),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(commands.len(), 2);
        assert!(
            matches!(commands[0], zess_protocol::CommandAction::Paste { text } if text == "ready")
        );
        assert!(
            matches!(commands[1], zess_protocol::CommandAction::Paste { text } if text == "restored")
        );
    }
}

#[test]
fn every_connection_attempt_suspends_the_terminal_and_restores_it_even_when_connecting_fails() {
    struct LifecycleTerminal {
        inner: ScriptedTerminal,
        suspended: Rc<Cell<bool>>,
        events: Rc<RefCell<Vec<&'static str>>>,
    }
    impl TerminalIo for LifecycleTerminal {
        type Writer = Vec<u8>;
        fn writer(&mut self) -> &mut Self::Writer {
            TerminalIo::writer(&mut self.inner)
        }
        fn size(&self) -> Result<(u16, u16)> {
            TerminalIo::size(&self.inner)
        }
        fn mouse_capture(&self) -> bool {
            TerminalIo::mouse_capture(&self.inner)
        }
        fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
            TerminalIo::set_mouse_capture(&mut self.inner, enabled)
        }
        fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>> {
            assert!(!self.suspended.get());
            TerminalIo::poll_event(&mut self.inner, timeout)
        }
        fn suspend(&mut self) -> Result<()> {
            assert!(!self.suspended.replace(true));
            self.events.borrow_mut().push("suspend");
            Ok(())
        }
        fn resume(&mut self) -> Result<()> {
            assert!(self.suspended.replace(false));
            self.events.borrow_mut().push("resume");
            Ok(())
        }
    }
    struct LifecycleFactory {
        calls: usize,
        suspended: Rc<Cell<bool>>,
        events: Rc<RefCell<Vec<&'static str>>>,
        phase: Rc<Cell<usize>>,
    }
    impl ConnectionFactory for LifecycleFactory {
        fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
            assert!(self.suspended.get());
            self.events.borrow_mut().push("connect");
            self.calls += 1;
            if self.calls == 1 {
                anyhow::bail!("authentication failed");
            }
            Ok(Box::new(ScriptedStream::new(
                self.phase.clone(),
                vec![(1, vec![ServerMessage::Exited])],
                None,
            )))
        }
    }
    let suspended = Rc::new(Cell::new(false));
    let events = Rc::new(RefCell::new(Vec::new()));
    let phase = Rc::new(Cell::new(0));
    let mut terminal = LifecycleTerminal {
        inner: ScriptedTerminal {
            phase: phase.clone(),
            events: VecDeque::from([None]),
            wait_at: Some(0),
            output: Vec::new(),
            mouse: false,
        },
        suspended: suspended.clone(),
        events: events.clone(),
    };
    let mut factory = LifecycleFactory {
        calls: 0,
        suspended: suspended.clone(),
        events: events.clone(),
        phase,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    assert_eq!(
        *events.borrow(),
        [
            "suspend", "connect", "resume", "suspend", "connect", "resume"
        ]
    );
    assert!(!suspended.get());
}

#[test]
fn new_input_acknowledges_an_error_so_the_command_prompt_becomes_visible() {
    for event in [key(KeyCode::Tab), Some(Event::Paste("pasted".into()))] {
        let phase = Rc::new(Cell::new(0));
        let state = SessionSnapshot {
            render_plan: zess_ui::RenderPlan {
                width: 80,
                height: 24,
                prompt: ":hel".into(),
                cursor: Some((4, 23)),
                ..Default::default()
            },
            ..Default::default()
        };
        let stream = ScriptedStream::new(
            phase.clone(),
            vec![
                (
                    0,
                    vec![
                        attached(),
                        snapshot(1, state),
                        ServerMessage::ClipboardWrite {
                            text: "copy".into(),
                        },
                    ],
                ),
                (2, vec![ServerMessage::Exited]),
            ],
            None,
        );
        let mut factory = ScriptedFactory(VecDeque::from([stream]));
        let mut frontend = ScriptedTerminal {
            phase,
            events: VecDeque::from([event, None]),
            wait_at: None,
            output: Vec::new(),
            mouse: false,
        };
        run_connection_with_devices(&mut factory, &[], &mut frontend, &mut UnusedClipboard)
            .unwrap();
        let display = String::from_utf8(frontend.output).unwrap();
        let error = display.find("unexpected clipboard write").unwrap();
        let prompt = display.find(":hel").unwrap();
        assert!(error < prompt);
        assert_eq!(display.matches("unexpected clipboard write").count(), 1);
    }
}

#[test]
fn ordinary_keys_do_not_dismiss_the_replay_confirmation_notice() {
    let phase = Rc::new(Cell::new(0));
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(0, vec![attached(), snapshot(1, SessionSnapshot::default())])],
        Some(1),
    );
    let mut reattached = attached();
    if let ServerMessage::Attached {
        replay_allowed,
        context_epoch,
        ..
    } = &mut reattached
    {
        *replay_allowed = false;
        *context_epoch = 6;
    }
    let state = SessionSnapshot {
        render_plan: zess_ui::RenderPlan {
            width: 80,
            height: 24,
            prompt: ":hel".into(),
            cursor: Some((4, 23)),
            ..Default::default()
        },
        ..Default::default()
    };
    let second = ScriptedStream::new(
        phase.clone(),
        vec![
            (2, vec![reattached, snapshot(2, state)]),
            (4, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let mut factory = ScriptedFactory(VecDeque::from([first, second]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([key(KeyCode::Char('a')), None, key(KeyCode::Tab), None]),
        wait_at: Some(1),
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let display = String::from_utf8(terminal.output).unwrap();
    assert_eq!(display.matches("Session changed: Ctrl-r replay").count(), 2);
    assert!(!display.contains(":hel"));
}

#[test]
fn frontends_and_their_operations_have_distinct_ids_when_host_identity_facts_are_unchanged() {
    let mut namespaces = Vec::new();
    let mut operation_ids = Vec::new();
    for _ in 0..2 {
        let phase = Rc::new(Cell::new(0));
        let stream = ScriptedStream::new(
            phase.clone(),
            vec![
                (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
                (3, vec![ServerMessage::Exited]),
            ],
            None,
        );
        let output = stream.output.clone();
        let mut factory = ScriptedFactory(VecDeque::from([stream]));
        let mut frontend = ScriptedTerminal {
            phase,
            events: VecDeque::from([
                key(KeyCode::Enter),
                Some(Event::Paste("command".into())),
                key(KeyCode::Enter),
            ]),
            wait_at: None,
            output: Vec::new(),
            mouse: false,
        };
        run_connection_with_devices_and_platform(
            &mut factory,
            &[],
            &mut frontend,
            &mut UnusedClipboard,
            &IdentityPlatform,
        )
        .unwrap();
        let messages = messages(&output);
        let client_id = messages
            .iter()
            .find_map(|message| match message {
                ClientMessage::Attach { client_id, .. } => Some(client_id),
                _ => None,
            })
            .unwrap();
        assert!(client_id.starts_with("4242-9000-"));
        let requests = messages
            .iter()
            .filter_map(|message| match message {
                ClientMessage::TrackedInput { request_id, .. }
                | ClientMessage::Command { request_id, .. } => Some(request_id.0.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            requests,
            [
                format!("{client_id}-1"),
                format!("{client_id}-2"),
                format!("{client_id}-3")
            ]
        );
        namespaces.push(client_id.clone());
        operation_ids.extend(requests);
    }
    assert_ne!(namespaces[0], namespaces[1]);
    assert_eq!(
        operation_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        6
    );
}

#[test]
fn reconnect_recovery_preserves_pending_command_results() {
    #[derive(Clone, Copy, Debug)]
    enum Report {
        FailedCommand,
        UnknownCommand,
    }
    struct RecoveryFrontend {
        terminal: ScriptedTerminal,
        presentations: Vec<(usize, String)>,
        output_offset: usize,
    }
    impl TerminalIo for RecoveryFrontend {
        type Writer = Vec<u8>;
        fn writer(&mut self) -> &mut Self::Writer {
            self.terminal.writer()
        }
        fn size(&self) -> Result<(u16, u16)> {
            Ok((240, 24))
        }
        fn mouse_capture(&self) -> bool {
            self.terminal.mouse_capture()
        }
        fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
            self.terminal.set_mouse_capture(enabled)
        }
        fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>> {
            let output = &self.terminal.output[self.output_offset..];
            self.presentations.push((
                self.terminal.phase.get(),
                String::from_utf8_lossy(output).into_owned(),
            ));
            self.output_offset = self.terminal.output.len();
            self.terminal.poll_event(timeout)
        }
    }
    for report in [Report::FailedCommand, Report::UnknownCommand] {
        for discard in [false, true] {
            let phase = Rc::new(Cell::new(0));
            let acknowledged = 1;
            let through = acknowledged + 1;
            let first = ScriptedStream::new(
                phase.clone(),
                vec![
                    (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
                    (
                        1,
                        vec![ServerMessage::Ack {
                            acknowledged,
                            context_epoch: 5,
                        }],
                    ),
                ],
                Some(2),
            );
            let first_output = first.output.clone();
            let second_output = Rc::<RefCell<Vec<u8>>>::default();
            struct ReconnectFactory {
                first: Option<ScriptedStream>,
                phase: Rc<Cell<usize>>,
                first_output: Rc<RefCell<Vec<u8>>>,
                second_output: Rc<RefCell<Vec<u8>>>,
                report: Report,
                acknowledged: u64,
            }
            impl ConnectionFactory for ReconnectFactory {
                fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
                    if let Some(first) = self.first.take() {
                        return Ok(Box::new(first));
                    }
                    let request_id = messages(&self.first_output)
                        .into_iter()
                        .find_map(|message| match message {
                            ClientMessage::TrackedInput { request_id, .. }
                            | ClientMessage::Command { request_id, .. } => Some(request_id),
                            _ => None,
                        })
                        .context("missing tracked command")?;
                    let mut reattached = attached();
                    if let ServerMessage::Attached {
                        acknowledged,
                        context_epoch,
                        replay_allowed,
                        ..
                    } = &mut reattached
                    {
                        *acknowledged = self.acknowledged;
                        *context_epoch = 6;
                        *replay_allowed = false;
                    }
                    let report = ServerMessage::CommandResult {
                        request_id,
                        server_instance: ServerInstanceId("server".into()),
                        result: if matches!(self.report, Report::UnknownCommand) {
                            OperationResult::Unknown
                        } else {
                            OperationResult::Completed {
                                outcome: zess_protocol::CommandOutcome::Failed {
                                    message: "command requires a git-history buffer".into(),
                                },
                                state_revision: 2,
                            }
                        },
                    };
                    let mut stream = ScriptedStream::new(
                        self.phase.clone(),
                        vec![
                            (
                                3,
                                vec![reattached, snapshot(2, SessionSnapshot::default()), report],
                            ),
                            (
                                5,
                                vec![ServerMessage::Ack {
                                    acknowledged: self.acknowledged + 1,
                                    context_epoch: 6,
                                }],
                            ),
                            (8, vec![ServerMessage::Exited]),
                        ],
                        None,
                    );
                    stream.output = self.second_output.clone();
                    Ok(Box::new(stream))
                }
            }
            let mut factory = ReconnectFactory {
                first: Some(first),
                phase: phase.clone(),
                first_output,
                second_output: second_output.clone(),
                report,
                acknowledged,
            };
            let choice = if discard {
                key(KeyCode::Esc)
            } else {
                Some(Event::Key(KeyEvent::new(
                    KeyCode::Char('r'),
                    KeyModifiers::CONTROL,
                )))
            };
            let mut frontend = RecoveryFrontend {
                terminal: ScriptedTerminal {
                    phase,
                    events: VecDeque::from([
                        key(KeyCode::Enter),
                        key(KeyCode::Char('x')),
                        None,
                        choice,
                        None,
                        key(KeyCode::Esc),
                        key(KeyCode::Char('y')),
                        None,
                    ]),
                    wait_at: Some(2),
                    output: Vec::new(),
                    mouse: false,
                },
                presentations: Vec::new(),
                output_offset: 0,
            };
            run_connection_with_devices(&mut factory, &[], &mut frontend, &mut UnusedClipboard)
                .unwrap();
            let (_, notice) = frontend
                .presentations
                .iter()
                .find(|(phase, _)| *phase == 3)
                .unwrap();
            assert!(
                notice.contains("Ctrl-r replay") && notice.contains("Esc discard"),
                "{report:?}: {notice}"
            );
            assert!(
                !notice.contains("Quit:"),
                "quit choices wait until recovery finishes"
            );
            let diagnostic = match report {
                Report::FailedCommand => Some("command requires a git-history buffer"),
                Report::UnknownCommand => Some("outcome is unknown"),
            };
            if let Some(diagnostic) = diagnostic {
                assert!(
                    notice.contains(diagnostic),
                    "the operation result remains available beside recovery controls: {notice}"
                );
                let (_, notice) = frontend
                    .presentations
                    .iter()
                    .find(|(phase, _)| *phase == if discard { 5 } else { 4 })
                    .unwrap();
                assert!(
                    notice.contains(diagnostic),
                    "the operation result remains visible after recovery: {notice}"
                );
            }

            if discard {
                let (_, notice) = frontend
                    .presentations
                    .iter()
                    .find(|(phase, _)| *phase == 4)
                    .unwrap();
                assert!(
                    notice.contains("waiting for server confirmation"),
                    "{report:?}: {notice}"
                );
                assert!(!notice.contains("Quit:"));
            }
            let messages = messages(&second_output);
            if discard {
                assert!(messages.iter().any(|message| matches!(message, ClientMessage::Skip { through: actual, context_epoch: 6 } if *actual == through)), "{report:?}");
                assert!(!messages.iter().any(
                    |message| matches!(message, ClientMessage::Input { key, .. } if key == "x")
                ));
            } else {
                assert!(messages.iter().any(|message| matches!(message, ClientMessage::Input { sequence, key, context_epoch: 6 } if *sequence == through && key == "x")), "{report:?}");
            }
            let next = through + 2;
            assert!(messages.iter().any(|message| matches!(message, ClientMessage::Input { sequence, key, context_epoch: 6 } if *sequence == next && key == "y")), "{report:?}: normal input resumes after both decisions");
        }
    }
}

#[test]
fn a_different_build_is_rejected_before_attachment() {
    let phase = Rc::new(Cell::new(0));
    let mut stream = ScriptedStream::new(phase.clone(), vec![], None);
    stream.batches[0].1 = framing::encode(&ServerMessage::Hello {
        build_id: "different-build".into(),
    })
    .unwrap()
    .into();
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::new(),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    let error = run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard)
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<zess_protocol::BuildMismatch>()
            .is_some()
    );
    let sent = messages(&output);
    assert_eq!(sent.len(), 1);
    assert!(
        matches!(&sent[0], ClientMessage::Hello { build_id } if build_id == zess_build::BUILD_ID)
    );
}

#[test]
fn attachment_uses_the_session_selected_by_the_transport() {
    let phase = Rc::new(Cell::new(0));
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
            (
                2,
                vec![ServerMessage::Detached {
                    disposition: zess_protocol::ClientExitDisposition::Retained,
                }],
            ),
        ],
        None,
    );
    stream.session_id = Some(SessionId("session".into()));
    let output = stream.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let sent = messages(&output);
    assert!(matches!(&sent[0], ClientMessage::Hello { .. }));
    assert!(
        matches!(&sent[1], ClientMessage::Attach { expected_session: Some(id), .. } if id.0 == "session")
    );
}

#[test]
fn stopping_waits_for_the_server_and_reports_a_process_that_did_not_exit() {
    let phase = Rc::new(Cell::new(0));
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
            (
                2,
                vec![ServerMessage::Detached {
                    disposition: zess_protocol::ClientExitDisposition::Stopping,
                }],
            ),
        ],
        None,
    );
    stream.exit_result = false;
    let waited = stream.waited.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    let error = run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard)
        .unwrap_err();
    assert!(error.to_string().contains("did not exit"));
    assert_eq!(waited.get(), 1);
}

#[test]
fn retaining_the_session_does_not_wait_for_its_process() {
    let phase = Rc::new(Cell::new(0));
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, SessionSnapshot::default())]),
            (
                2,
                vec![ServerMessage::Detached {
                    disposition: zess_protocol::ClientExitDisposition::Retained,
                }],
            ),
        ],
        None,
    );
    stream.exit_result = false;
    let waited = stream.waited.clone();
    let mut factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    assert_eq!(waited.get(), 0);
}

#[test]
fn reconnecting_to_a_replacement_session_does_not_replay_input() {
    let phase = Rc::new(Cell::new(0));
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(0, vec![attached(), snapshot(1, SessionSnapshot::default())])],
        Some(1),
    );
    let mut replacement = attached();
    if let ServerMessage::Attached {
        server_instance, ..
    } = &mut replacement
    {
        server_instance.0 = "replacement".into();
    }
    let second = ScriptedStream::new(phase.clone(), vec![(2, vec![replacement])], None);
    let output = second.output.clone();
    let mut factory = ScriptedFactory(VecDeque::from([first, second]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([key(KeyCode::Char('x')), None, None]),
        wait_at: Some(1),
        output: Vec::new(),
        mouse: false,
    };
    let error = run_connection_with_devices(&mut factory, &[], &mut terminal, &mut UnusedClipboard)
        .unwrap_err();
    assert!(error.to_string().contains("original zess server"));
    assert!(!messages(&output).iter().any(|message| matches!(
        message,
        ClientMessage::Input { .. }
            | ClientMessage::TrackedInput { .. }
            | ClientMessage::Command { .. }
    )));
}
