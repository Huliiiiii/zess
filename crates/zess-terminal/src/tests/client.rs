mod clipboard_order;
mod command_order;
mod document_reload;
mod page_yank;
mod workspace_view;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use zess_plugin::{CommandImplementation, PluginRegistrar, Registry};
use zess_protocol::{
    BufferKind, BufferSummary, ClientWorkspace, CommandAction, CommandTarget, OperationResult,
    SessionId, SessionSnapshot, ViewArea, ViewContent, ViewSelection, ViewSnapshot,
    document::{DocumentChange, DocumentEvent, DocumentRequest, DocumentSnapshot},
};

use super::*;

#[derive(Default)]
struct Phase {
    value: AtomicUsize,
    transport_ready: AtomicBool,
}

impl Phase {
    fn load(&self, ordering: Ordering) -> usize {
        self.value.load(ordering)
    }
    fn store(&self, value: usize, ordering: Ordering) {
        self.value.store(value, ordering);
    }
}

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
    phase: Arc<Phase>,
    batches: VecDeque<(usize, VecDeque<u8>)>,
    input: framing::Decoder,
    reply: Box<dyn FnMut(ClientMessage) -> Option<ServerMessage> + Send>,
    workspace_view_reply: Box<dyn FnMut() -> Option<ServerMessage> + Send>,
    output: Arc<Mutex<Vec<u8>>>,
    close_at: Option<usize>,
    waited: Arc<Phase>,
    exit_result: bool,
    session_id: Option<SessionId>,
    cancellation: zess_platform::CancellationToken,
}

impl ScriptedStream {
    fn new(
        phase: Arc<Phase>,
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
            input: framing::Decoder::default(),
            reply: Box::new(|_| None),
            workspace_view_reply: Box::new(|| Some(ServerMessage::WorkspaceViewSaved)),
            output: Arc::default(),
            close_at,
            waited: Arc::default(),
            exit_result: true,
            session_id: None,
            cancellation: zess_platform::CancellationToken::new(),
        }
    }
}

impl Read for ScriptedStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.cancellation.check().map_err(io::Error::other)?;
        if let Some((phase, batch)) = self.batches.front_mut()
            && *phase <= self.phase.load(Ordering::SeqCst)
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
        if self
            .close_at
            .is_some_and(|phase| self.phase.load(Ordering::SeqCst) >= phase)
        {
            self.phase.transport_ready.store(false, Ordering::SeqCst);
            Ok(0)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

impl Write for ScriptedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.cancellation.check().map_err(io::Error::other)?;
        self.output.lock().unwrap().extend_from_slice(bytes);
        self.input
            .push(bytes, MAX_INPUT_BYTES)
            .map_err(io::Error::other)?;
        while let Some(message) = self.input.decode_next()? {
            let response = match message {
                ClientMessage::SaveWorkspaceView { .. } => (self.workspace_view_reply)(),
                message => (self.reply)(message),
            };
            if let Some(response) = response {
                self.batches.push_front((
                    self.phase.load(Ordering::SeqCst),
                    framing::encode(&response)?.into(),
                ));
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
        self.waited
            .store(self.waited.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
        Ok(self.exit_result)
    }
    fn set_nonblocking(&self, _: bool) -> io::Result<()> {
        self.phase.transport_ready.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct ScriptedFactory(VecDeque<ScriptedStream>);

impl ConnectionFactory for ScriptedFactory {
    fn connect(&mut self, context: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
        let mut stream = self.0.pop_front().context("unexpected reconnect")?;
        stream.cancellation = context.cancellation().clone();
        Ok(Box::new(stream))
    }
}

struct ScriptedTerminal {
    phase: Arc<Phase>,
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
    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>> {
        std::thread::sleep(timeout);
        if self.events.is_empty() && String::from_utf8_lossy(&self.output).contains("r retry") {
            return Ok(key(KeyCode::Esc));
        }
        if !self.phase.transport_ready.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if self.wait_at == Some(self.phase.load(Ordering::SeqCst)) {
            std::thread::sleep(RECONNECT_INTERVAL + Duration::from_millis(20));
        }
        self.phase
            .store(self.phase.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
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
        context_epoch: 5,
        workspace_view: None,
    }
}

fn snapshot_with_bindings(
    revision: u64,
    state: SessionSnapshot,
    bindings: zess_plugin::BindingCatalog,
) -> ServerMessage {
    ServerMessage::Snapshot {
        session_id: SessionId("session".into()),
        server_instance: ServerInstanceId("server".into()),
        state_revision: revision,
        bindings,
        state,
    }
}

fn snapshot(revision: u64, state: SessionSnapshot) -> ServerMessage {
    snapshot_with_bindings(revision, state, Default::default())
}

fn view_state(kind: BufferKind, content: ViewContent, scopes: &[&str]) -> SessionSnapshot {
    let view = ViewSnapshot {
        text_metrics: matches!(kind, BufferKind::Text { .. }).then_some(
            zess_protocol::TextMetrics {
                line_count: 1,
                tab_width: 4,
            },
        ),
        statusline: None,
        options: None,
        view_id: 3,
        buffer_id: 7,
        kind: scopes.last().unwrap().to_string(),
        key_scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        mode: "normal".into(),
        title: String::new(),
        header: String::new(),
        tabs: Vec::new(),
        loading: false,
        column_alignment: Default::default(),
        version: matches!(kind, BufferKind::Text { .. }).then_some(1),
        content,
        selection: match kind {
            BufferKind::Text { .. } => ViewSelection::Text {
                ranges: vec![zess_protocol::TextRange { anchor: 0, head: 0 }],
                primary: 0,
            },
            _ => ViewSelection::Items {
                ids: Vec::new(),
                primary: None,
            },
        },
        scroll: 0,
        horizontal_scroll: 0,
    };
    SessionSnapshot {
        views: vec![view],
        focused_view: Some(3),
        buffers: vec![BufferSummary {
            id: 7,
            kind,
            modified: false,
        }],
        layout: vec![ViewArea {
            view_id: Some(3),
            page_id: None,
            slot_id: None,
            buffer_id: 7,
            x: 0,
            y: 0,
            width: 80,
            height: 23,
        }],
        workspace: ClientWorkspace {
            buffers: vec![7],
            current: Some(7),
        },
        ..Default::default()
    }
}

fn review_snapshot(revision: u64) -> ServerMessage {
    let state = view_state(
        BufferKind::Data {
            name: "review".into(),
        },
        ViewContent::Items { rows: Vec::new() },
        &["global", "data", "review"],
    );
    let mut registry = Registry::default();
    for (key, command) in [
        ("x", "review.archive"),
        ("y", "review.accept"),
        ("z", "review.defer"),
        ("w", "review.annotate"),
        ("Enter", "review.open"),
    ] {
        registry.register_command(command, "", CommandImplementation::External(()));
        registry.binding("review", "normal", &[key], command);
    }
    registry.register_action(
        "command-prompt",
        "Enter a command",
        CommandImplementation::Native,
    );
    registry.binding("global", "normal", &[":"], "command-prompt");
    snapshot_with_bindings(revision, state, registry.binding_catalog())
}

fn invoked_commands(messages: &[ClientMessage]) -> Vec<&str> {
    messages
        .iter()
        .filter_map(|message| match message {
            ClientMessage::Command {
                action: CommandAction::Invoke { target, name, .. },
                ..
            } => {
                assert_eq!(
                    *target,
                    CommandTarget::View {
                        view_id: 3,
                        buffer_id: 7
                    }
                );
                Some(name.as_str())
            }
            _ => None,
        })
        .collect()
}

fn document_snapshot(revision: u64) -> ServerMessage {
    snapshot(
        revision,
        view_state(
            BufferKind::Text { path: None },
            ViewContent::Text { lines: Vec::new() },
            &["global", "text"],
        ),
    )
}

fn document_contents() -> ServerMessage {
    ServerMessage::Document(DocumentEvent::Snapshot(DocumentSnapshot {
        buffer_id: 7,
        version: 1,
        modified: false,
        readonly: false,
        language: None,
        path: None,
        history: Default::default(),
        text: String::new(),
    }))
}

fn key(code: KeyCode) -> Option<Event> {
    Some(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn messages(output: &Arc<Mutex<Vec<u8>>>) -> Vec<ClientMessage> {
    output
        .lock()
        .unwrap()
        .split_inclusive(|byte| *byte == b'\n')
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .collect()
}

#[test]
fn a_large_snapshot_can_finish_attaching_while_data_keeps_arriving() {
    let phase = Arc::new(Phase::default());
    let state = SessionSnapshot {
        text_options: serde_json::json!({"padding": "x".repeat(MAX_MESSAGE_BYTES)}),
        ..Default::default()
    };
    let json = serde_json::to_string(&snapshot(1, state)).unwrap();
    let mut responses = vec![attached()];
    responses.extend(
        zess_protocol::chunks::json_parts(&json, MAX_MESSAGE_BYTES).map(|(part, last)| {
            ServerMessage::Chunk {
                json: part.into(),
                last,
            }
        }),
    );
    let mut stream = ScriptedStream::new(phase.clone(), vec![(0, responses)], None);
    stream.reply = Box::new(|message| {
        matches!(message, ClientMessage::Resize { .. }).then_some(ServerMessage::Exited)
    });
    let output = stream.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: (0..300).map(|_| None).collect(),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_client(
        ScriptedFactory(VecDeque::from([stream])),
        "client".into(),
        Vec::new(),
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();
    assert!(
        messages(&output)
            .iter()
            .any(|message| matches!(message, ClientMessage::Resize { .. }))
    );
}

#[test]
fn quitting_the_only_clean_scratch_exits_the_client() {
    let phase = Arc::new(Phase::default());
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
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
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
    let phase = Arc::new(Phase::default());
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
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
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
    let phase = Arc::new(Phase::default());
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
                    request_id: RequestId(1),
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
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([const { None }; 5]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_client(
        factory,
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
    let phase = Arc::new(Phase::default());
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
                    request_id: RequestId(1),
                    server_instance: ServerInstanceId("server".into()),
                    result: OperationResult::Unknown,
                }],
            ),
            (5, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([const { None }; 5]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_client(
        factory,
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
    let phase = Arc::new(Phase::default());
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
                }],
            ),
            (2, vec![snapshot(4, restored)]),
            (5, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([const { None }; 5]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_client(
        factory,
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
fn remote_commands_wait_for_snapshot_synchronization() {
    let phase = Arc::new(Phase::default());
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached()]),
            (1, vec![review_snapshot(1)]),
            (
                3,
                vec![ServerMessage::Delta {
                    session_id: SessionId("session".into()),
                    server_instance: ServerInstanceId("server".into()),
                    base_revision: 3,
                    state_revision: 4,
                    delta: zess_protocol::SessionDelta::between(
                        &SessionSnapshot::default(),
                        &SessionSnapshot::default(),
                    ),
                }],
            ),
            (4, vec![review_snapshot(4)]),
            (6, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('x')),
            key(KeyCode::Char('y')),
            None,
            key(KeyCode::Char('z')),
            key(KeyCode::Char('w')),
            None,
        ]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let messages = messages(&output);
    assert_eq!(
        invoked_commands(&messages),
        ["review.accept", "review.annotate"]
    );
    assert_eq!(messages.iter().filter(|message| matches!(message, ClientMessage::Resync { session_id, state_revision: 1 } if session_id.0 == "session")).count(), 1);
}

#[test]
fn reconnect_queries_a_pending_command_without_executing_it_again() {
    let phase = Arc::new(Phase::default());

    let first = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), review_snapshot(1)]),
            (
                2,
                vec![ServerMessage::CommandResult {
                    request_id: RequestId(1),
                    server_instance: ServerInstanceId("server".into()),
                    result: OperationResult::Pending,
                }],
            ),
        ],
        Some(3),
    );
    let first_output = first.output.clone();
    // The second stream derives the request ID from the actual first request.
    let second_output = Arc::<Mutex<Vec<u8>>>::default();
    struct ReconnectFactory {
        first: Option<ScriptedStream>,
        phase: Arc<Phase>,
        first_output: Arc<Mutex<Vec<u8>>>,
        second_output: Arc<Mutex<Vec<u8>>>,
    }
    impl ConnectionFactory for ReconnectFactory {
        fn connect(&mut self, _: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
            if let Some(first) = self.first.take() {
                return Ok(Box::new(first));
            }
            let request_id = messages(&self.first_output)
                .into_iter()
                .find_map(|message| match message {
                    ClientMessage::Command { request_id, .. } => Some(request_id),
                    _ => None,
                })
                .context("missing tracked command")?;
            let result = ServerMessage::CommandResult {
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
                    (4, vec![attached(), review_snapshot(2), result]),
                    (5, vec![ServerMessage::Exited]),
                ],
                None,
            );
            stream.output = self.second_output.clone();
            Ok(Box::new(stream))
        }
    }
    let factory = ReconnectFactory {
        first: Some(first),
        phase: phase.clone(),
        first_output: first_output.clone(),
        second_output: second_output.clone(),
    };
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([key(KeyCode::Enter), None, None, None, None]),
        wait_at: Some(3),
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices_and_platform(
        factory,
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
            ClientMessage::Command { request_id, .. } => Some(request_id),
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
    assert_eq!(*request_id, RequestId(1));
    let second_messages = messages(&second_output);
    assert!(second_messages.iter().any(|message| matches!(message, ClientMessage::Attach { client_id: actual, .. } if actual == client_id)));
    assert!(second_messages.iter().any(|message| matches!(message, ClientMessage::QueryOperation { request_id: actual, .. } if actual == request_id)));
    assert!(second_messages.iter().any(|message| matches!(message, ClientMessage::ResultAck { request_id: actual, .. } if actual == request_id)));
    assert!(
        !second_messages
            .iter()
            .any(|message| matches!(message, ClientMessage::Command { .. }))
    );
}

#[test]
fn reconnect_acknowledges_a_resent_result_without_applying_it_again() {
    let phase = Arc::new(Phase::default());
    let result = || ServerMessage::CommandResult {
        request_id: RequestId(1),
        server_instance: ServerInstanceId("server".into()),
        result: OperationResult::Completed {
            outcome: zess_protocol::CommandOutcome::Failed {
                message: "operation failed".into(),
            },
            state_revision: 2,
        },
    };
    let first = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), review_snapshot(1)]),
            (2, vec![result()]),
        ],
        Some(3),
    );
    let first_output = first.output.clone();
    let second = ScriptedStream::new(
        phase.clone(),
        vec![
            (4, vec![attached(), review_snapshot(2), result()]),
            (5, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let second_output = second.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([key(KeyCode::Enter), None, None, None, None]),
        wait_at: Some(3),
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(
        ScriptedFactory(VecDeque::from([first, second])),
        &[],
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();

    let first_messages = messages(&first_output);
    assert!(first_messages.iter().any(|message| matches!(
        message,
        ClientMessage::Command {
            request_id: RequestId(1),
            ..
        }
    )));
    let second_messages = messages(&second_output);
    assert!(
        second_messages
            .iter()
            .any(|message| matches!(message, ClientMessage::ResultAck {
        request_id: RequestId(1), server_instance
    } if server_instance.0 == "server"))
    );
    assert_eq!(
        String::from_utf8(terminal.output)
            .unwrap()
            .matches("operation failed")
            .count(),
        1
    );
}

#[test]
fn ignored_terminal_events_do_not_discard_a_delayed_client_request() {
    let notice = "The delayed review command completed";
    for event in [
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Moved,
            column: 4,
            row: 2,
            modifiers: KeyModifiers::NONE,
        }),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
    ] {
        let phase = Arc::new(Phase::default());
        let stream = ScriptedStream::new(
            phase.clone(),
            vec![
                (0, vec![attached(), review_snapshot(1)]),
                (
                    2,
                    vec![ServerMessage::CommandResult {
                        request_id: RequestId(1),
                        server_instance: ServerInstanceId("server".into()),
                        result: OperationResult::Completed {
                            outcome: zess_protocol::CommandOutcome::ClientRequest {
                                request: zess_plugin::HostRequest::Message {
                                    text: notice.into(),
                                },
                            },
                            state_revision: 1,
                        },
                    }],
                ),
                (3, vec![ServerMessage::Exited]),
            ],
            None,
        );
        let output = stream.output.clone();
        let mut terminal = ScriptedTerminal {
            phase,
            events: VecDeque::from([key(KeyCode::Enter), Some(event.clone()), None]),
            wait_at: None,
            output: Vec::new(),
            mouse: false,
        };
        run_connection_with_devices(
            ScriptedFactory(VecDeque::from([stream])),
            &[],
            &mut terminal,
            &mut UnusedClipboard,
        )
        .unwrap();

        assert_eq!(invoked_commands(&messages(&output)), ["review.open"]);
        assert!(
            String::from_utf8(terminal.output).unwrap().contains(notice),
            "client request was discarded after {event:?}"
        );
    }
}

#[test]
fn the_display_updates_a_view_row_from_the_session_delta() {
    let phase = Arc::new(Phase::default());
    let mut state = view_state(
        BufferKind::Data {
            name: "review".into(),
        },
        ViewContent::Items {
            rows: vec![zess_protocol::SemanticRow {
                line_number: None,
                id: "entry".into(),
                text: "cached-one".into(),
                cells: Vec::new(),
                style: String::new(),
                depth: 0,
            }],
        },
        &["global", "data", "review"],
    );
    let previous = state.clone();
    let ViewContent::Items { rows } = &mut state.views[0].content else {
        unreachable!()
    };
    rows[0].text = "cached-two".into();
    let delta = zess_protocol::SessionDelta::between(&previous, &state);
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), snapshot(1, previous)]),
            (
                1,
                vec![ServerMessage::Delta {
                    session_id: SessionId("session".into()),
                    server_instance: ServerInstanceId("server".into()),
                    base_revision: 1,
                    state_revision: 2,
                    delta,
                }],
            ),
            (2, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let display = String::from_utf8(terminal.output).unwrap();
    assert!(display.contains("cached-one"));
    assert!(display.contains("cached-two"));
}

#[test]
fn pasted_text_reaches_the_document_as_one_literal_edit() {
    let phase = Arc::new(Phase::default());
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                0,
                vec![attached(), document_snapshot(1), document_contents()],
            ),
            (2, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([Some(Event::Paste("hello\n:q".into())), None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let messages = messages(&output);
    let edits: Vec<_> = messages
        .iter()
        .filter_map(|message| match message {
            ClientMessage::Document(DocumentRequest::Edit {
                buffer_id,
                base_version,
                changes,
                ..
            }) => Some((*buffer_id, *base_version, changes.as_slice())),
            _ => None,
        })
        .collect();
    let expected = [DocumentChange {
        start: 0,
        end: 0,
        insert: "hello\n:q".into(),
    }];
    assert_eq!(edits, [(7, 1, expected.as_slice())]);
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, ClientMessage::Command { .. }))
    );
}

#[test]
fn a_large_paste_is_sent_in_bounded_frames_without_losing_the_next_edit() {
    let phase = Arc::new(Phase::default());
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![attached(), document_snapshot(1), document_contents()],
        )],
        None,
    );
    let mut requests = zess_protocol::document::DocumentStream::default();
    let mut edits = 0;
    stream.reply = Box::new(move |message| {
        let ClientMessage::Document(request) = message else {
            return None;
        };
        let (buffer_id, sequence, version) = match requests.request(request).unwrap()? {
            DocumentRequest::Edit {
                buffer_id,
                sequence,
                base_version,
                ..
            } => {
                edits += 1;
                if edits == 2 {
                    return Some(ServerMessage::Exited);
                }
                (buffer_id, sequence, base_version + 1)
            }
            DocumentRequest::History {
                buffer_id,
                sequence,
                base_version,
                ..
            } => (buffer_id, sequence, base_version),
            _ => return None,
        };
        Some(ServerMessage::Document(DocumentEvent::Applied {
            buffer_id,
            sequence,
            version,
            modified: true,
        }))
    });
    let output = stream.output.clone();
    let paste = "x".repeat(MAX_INPUT_BYTES + 1);
    let mut terminal = ScriptedTerminal {
        phase,
        events: [
            Some(Event::Paste(paste.clone())),
            None,
            Some(Event::Paste("after".into())),
        ]
        .into_iter()
        .chain(std::iter::repeat_n(None, 128))
        .collect(),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(
        ScriptedFactory(VecDeque::from([stream])),
        &[],
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();
    assert!(
        output
            .lock()
            .unwrap()
            .split_inclusive(|&byte| byte == b'\n')
            .all(|frame| frame.len() <= MAX_INPUT_BYTES)
    );
    let mut stream = zess_protocol::document::DocumentStream::default();
    let mut edits = Vec::new();
    for message in messages(&output) {
        let ClientMessage::Document(request) = message else {
            continue;
        };
        if let Some(DocumentRequest::Edit {
            buffer_id: 7,
            base_version,
            changes,
            ..
        }) = stream.request(request).unwrap()
        {
            edits.push((base_version, changes));
        }
    }
    assert_eq!(edits.len(), 2);
    let [(first_version, first), (second_version, second)] = edits.as_slice() else {
        unreachable!()
    };
    assert_eq!((*first_version, *second_version), (1, 2));
    assert!(
        matches!(first.as_slice(), [change] if change.start == 0 && change.end == 0 && change.insert == paste)
    );
    assert!(matches!(second.as_slice(), [change] if change.insert == "after"));
}

#[test]
fn an_unknown_focused_view_pauses_remote_commands_until_resynchronization() {
    let phase = Arc::new(Phase::default());
    let invalid = SessionSnapshot {
        focused_view: Some(999),
        ..Default::default()
    };
    let delta = zess_protocol::SessionDelta::between(&SessionSnapshot::default(), &invalid);
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), review_snapshot(1)]),
            (
                2,
                vec![ServerMessage::Delta {
                    session_id: SessionId("session".into()),
                    server_instance: ServerInstanceId("server".into()),
                    base_revision: 1,
                    state_revision: 2,
                    delta,
                }],
            ),
            (3, vec![review_snapshot(3)]),
            (5, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('y')),
            None,
            key(KeyCode::Char('z')),
            key(KeyCode::Char('w')),
            None,
        ]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let messages = messages(&output);
    assert_eq!(
        invoked_commands(&messages),
        ["review.accept", "review.annotate"]
    );
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
    let phase = Arc::new(Phase::default());
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(0, vec![attached(), review_snapshot(1)])],
        Some(2),
    );
    let first_output = first.output.clone();
    let second_output = Arc::<Mutex<Vec<u8>>>::default();
    struct ReconnectFactory {
        first: Option<ScriptedStream>,
        phase: Arc<Phase>,
        first_output: Arc<Mutex<Vec<u8>>>,
        second_output: Arc<Mutex<Vec<u8>>>,
    }
    impl ConnectionFactory for ReconnectFactory {
        fn connect(&mut self, _: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
            if let Some(first) = self.first.take() {
                return Ok(Box::new(first));
            }
            let request_id = messages(&self.first_output)
                .into_iter()
                .find_map(|message| match message {
                    ClientMessage::Command { request_id, .. } => Some(request_id),
                    _ => None,
                })
                .context("missing review command")?;
            let unknown = ServerMessage::CommandResult {
                request_id,
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
                    (3, vec![attached(), review_snapshot(1), unknown]),
                    (4, vec![completed]),
                    (5, vec![ServerMessage::Exited]),
                ],
                None,
            );
            stream.output = self.second_output.clone();
            Ok(Box::new(stream))
        }
    }
    let factory = ReconnectFactory {
        first: Some(first),
        phase: phase.clone(),
        first_output: first_output.clone(),
        second_output: second_output.clone(),
    };
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([key(KeyCode::Enter), None, None, None, None]),
        wait_at: Some(2),
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
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
fn paste_waits_for_the_subscribed_document() {
    let phase = Arc::new(Phase::default());
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached()]),
            (1, vec![document_snapshot(1)]),
            (2, vec![document_contents()]),
            (4, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            Some(Event::Paste("before attachment".into())),
            Some(Event::Paste("before document".into())),
            Some(Event::Paste("ready".into())),
            None,
        ]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(
        ScriptedFactory(VecDeque::from([stream])),
        &[],
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();
    let requests: Vec<_> = messages(&output)
        .into_iter()
        .filter_map(|message| match message {
            ClientMessage::Document(request) => Some(request),
            _ => None,
        })
        .collect();
    assert!(matches!(requests.as_slice(), [
        DocumentRequest::Read { buffer_id: 7 },
        DocumentRequest::Edit { buffer_id: 7, base_version: 1, changes, .. },
    ] if changes == &[DocumentChange { start: 0, end: 0, insert: "ready".into() }]));
}

#[test]
fn connection_retries_keep_the_terminal_active() {
    struct LifecycleTerminal {
        inner: ScriptedTerminal,
        suspended: Arc<AtomicBool>,
        events: Arc<Mutex<Vec<&'static str>>>,
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
            assert!(!self.suspended.load(Ordering::SeqCst));
            TerminalIo::poll_event(&mut self.inner, timeout)
        }
        fn suspend(&mut self) -> Result<()> {
            assert!(!self.suspended.swap(true, Ordering::SeqCst));
            self.events.lock().unwrap().push("suspend");
            Ok(())
        }
        fn resume(&mut self) -> Result<()> {
            assert!(self.suspended.swap(false, Ordering::SeqCst));
            self.events.lock().unwrap().push("resume");
            Ok(())
        }
    }
    struct LifecycleFactory {
        calls: usize,
        suspended: Arc<AtomicBool>,
        events: Arc<Mutex<Vec<&'static str>>>,
        phase: Arc<Phase>,
    }
    impl ConnectionFactory for LifecycleFactory {
        fn connect(&mut self, _: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
            assert!(!self.suspended.load(Ordering::SeqCst));
            self.events.lock().unwrap().push("connect");
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
    let suspended = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(Vec::new()));
    let phase = Arc::new(Phase::default());
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
    let factory = LifecycleFactory {
        calls: 0,
        suspended: suspended.clone(),
        events: events.clone(),
        phase,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    assert_eq!(*events.lock().unwrap(), ["connect", "connect"]);
    assert!(!suspended.load(Ordering::SeqCst));
}

#[test]
fn typing_and_pasting_after_a_clipboard_error_update_the_local_command_prompt() {
    for (event, prompt) in [
        (key(KeyCode::Char('a')), ":a"),
        (Some(Event::Paste("pasted".into())), ":pasted"),
    ] {
        let phase = Arc::new(Phase::default());
        let stream = ScriptedStream::new(
            phase.clone(),
            vec![
                (0, vec![attached(), review_snapshot(1)]),
                (
                    1,
                    vec![ServerMessage::ClipboardWrite {
                        text: "copy".into(),
                    }],
                ),
                (3, vec![ServerMessage::Exited]),
            ],
            None,
        );
        let factory = ScriptedFactory(VecDeque::from([stream]));
        let mut frontend = ScriptedTerminal {
            phase,
            events: VecDeque::from([key(KeyCode::Char(':')), event, None]),
            wait_at: None,
            output: Vec::new(),
            mouse: false,
        };
        run_connection_with_devices(factory, &[], &mut frontend, &mut UnusedClipboard).unwrap();
        let display = String::from_utf8(frontend.output).unwrap();
        let error = display.find("unexpected clipboard write").unwrap();
        let prompt = display.find(prompt).unwrap();
        assert!(error < prompt);
        assert_eq!(display.matches("unexpected clipboard write").count(), 1);
    }
}

#[test]
fn frontends_scope_ordered_operation_ids_to_distinct_clients() {
    let mut namespaces = Vec::new();
    for _ in 0..2 {
        let phase = Arc::new(Phase::default());
        let stream = ScriptedStream::new(
            phase.clone(),
            vec![
                (0, vec![attached(), review_snapshot(1)]),
                (4, vec![ServerMessage::Exited]),
            ],
            None,
        );
        let output = stream.output.clone();
        let factory = ScriptedFactory(VecDeque::from([stream]));
        let mut frontend = ScriptedTerminal {
            phase,
            events: VecDeque::from([
                key(KeyCode::Enter),
                key(KeyCode::Char('x')),
                key(KeyCode::Enter),
                None,
            ]),
            wait_at: None,
            output: Vec::new(),
            mouse: false,
        };
        run_connection_with_devices_and_platform(
            factory,
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
                ClientMessage::Command { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(requests, [RequestId(1), RequestId(2), RequestId(3)]);
        namespaces.push(client_id.clone());
    }
    assert_ne!(namespaces[0], namespaces[1]);
}

#[test]
fn a_different_build_is_rejected_before_attachment() {
    let phase = Arc::new(Phase::default());
    let mut stream = ScriptedStream::new(phase.clone(), vec![], None);
    stream.batches[0].1 = framing::encode(&ServerMessage::Hello {
        build_id: "different-build".into(),
    })
    .unwrap()
    .into();
    let output = stream.output.clone();
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::new(),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    let error =
        run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap_err();
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
fn exiting_after_attachment_retries_are_exhausted_reports_the_connection_loss() {
    let phase = Arc::new(Phase::default());
    let streams = (1..=usize::from(MAX_CONNECT_ATTEMPTS))
        .map(|attempt| ScriptedStream::new(phase.clone(), Vec::new(), Some(attempt)))
        .collect::<VecDeque<_>>();
    let requests = streams
        .iter()
        .map(|stream| stream.output.clone())
        .collect::<Vec<_>>();
    let factory = ScriptedFactory(streams);
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from(vec![None; usize::from(MAX_CONNECT_ATTEMPTS)]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };

    let error =
        run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap_err();

    assert_eq!(error.to_string(), "server closed the connection");
    for request in requests {
        assert!(matches!(
            messages(&request).as_slice(),
            [ClientMessage::Hello { .. }, ClientMessage::Attach { .. }]
        ));
    }
}

#[test]
fn attachment_uses_the_session_selected_by_the_transport() {
    let phase = Arc::new(Phase::default());
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
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    let sent = messages(&output);
    assert!(matches!(&sent[0], ClientMessage::Hello { .. }));
    assert!(
        matches!(&sent[1], ClientMessage::Attach { expected_session: Some(id), .. } if id.0 == "session")
    );
}

#[test]
fn the_client_starts_using_a_pending_session_after_confirmation() {
    struct ObservedFactory {
        scripted: ScriptedFactory,
        attempts: Arc<Mutex<Vec<Option<SessionId>>>>,
    }

    impl ConnectionFactory for ObservedFactory {
        fn connect(&mut self, context: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
            self.attempts
                .lock()
                .unwrap()
                .push(context.session_id().cloned());
            self.scripted.connect(context)
        }
    }

    let phase = Arc::new(Phase::default());
    let abandoned = SessionId("abandoned".into());
    let abandoned_server = ServerInstanceId("abandoned-server".into());
    let mut abandoned_attach = attached();
    if let ServerMessage::Attached {
        server_instance, ..
    } = &mut abandoned_attach
    {
        *server_instance = abandoned_server.clone();
    }
    let mut abandoned_snapshot = review_snapshot(1);
    if let ServerMessage::Snapshot {
        session_id,
        server_instance,
        ..
    } = &mut abandoned_snapshot
    {
        *session_id = abandoned.clone();
        *server_instance = abandoned_server;
    }
    let mut first = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![
                ServerMessage::AttachPending,
                abandoned_attach,
                abandoned_snapshot,
            ],
        )],
        Some(1),
    );
    first.session_id = Some(abandoned.clone());
    let first_output = first.output.clone();

    let session = SessionId("session".into());
    let mut retry = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                2,
                vec![ServerMessage::AttachPending, attached(), review_snapshot(1)],
            ),
            (4, vec![ServerMessage::AttachConfirmed]),
        ],
        Some(6),
    );
    retry.session_id = Some(session.clone());
    let retry_output = retry.output.clone();

    let reconnect = ScriptedStream::new(
        phase.clone(),
        vec![
            (7, vec![attached(), review_snapshot(2)]),
            (
                8,
                vec![ServerMessage::Detached {
                    disposition: zess_protocol::ClientExitDisposition::Retained,
                }],
            ),
        ],
        None,
    );
    let attempts = Arc::default();
    let factory = ObservedFactory {
        scripted: ScriptedFactory(VecDeque::from([first, retry, reconnect])),
        attempts: Arc::clone(&attempts),
    };
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('x')),
            None,
            key(KeyCode::Char('y')),
            None,
            key(KeyCode::Char('z')),
            None,
            None,
            None,
        ]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };

    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();

    assert_eq!(
        *attempts.lock().unwrap(),
        [None, None, Some(session.clone())]
    );
    assert!(matches!(messages(&first_output).as_slice(), [
        ClientMessage::Hello { .. },
        ClientMessage::Attach { expected_session: Some(id), .. },
        ClientMessage::ConfirmAttach,
    ] if id == &abandoned));
    let retry = messages(&retry_output);
    assert!(matches!(&retry[..3], [
        ClientMessage::Hello { .. },
        ClientMessage::Attach { expected_session: Some(id), .. },
        ClientMessage::ConfirmAttach,
    ] if id == &session));
    assert_eq!(invoked_commands(&retry), ["review.defer"]);
}

#[test]
fn stopping_waits_for_the_server_and_reports_a_process_that_did_not_exit() {
    let phase = Arc::new(Phase::default());
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
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    let error =
        run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap_err();
    assert!(error.to_string().contains("did not exit"));
    assert_eq!(waited.load(Ordering::SeqCst), 1);
}

#[test]
fn retaining_the_session_does_not_wait_for_its_process() {
    let phase = Arc::new(Phase::default());
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
    let factory = ScriptedFactory(VecDeque::from([stream]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([None, None]),
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap();
    assert_eq!(waited.load(Ordering::SeqCst), 0);
}

#[test]
fn reconnecting_to_a_replacement_server_does_not_submit_or_query_the_original_command() {
    let phase = Arc::new(Phase::default());
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(0, vec![attached(), review_snapshot(1)])],
        Some(2),
    );
    let first_output = first.output.clone();
    let mut replacement = attached();
    if let ServerMessage::Attached {
        server_instance, ..
    } = &mut replacement
    {
        server_instance.0 = "replacement".into();
    }
    let second = ScriptedStream::new(phase.clone(), vec![(3, vec![replacement])], None);
    let output = second.output.clone();
    let factory = ScriptedFactory(VecDeque::from([first, second]));
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([key(KeyCode::Char('x')), None, None, None]),
        wait_at: Some(2),
        output: Vec::new(),
        mouse: false,
    };
    let error =
        run_connection_with_devices(factory, &[], &mut terminal, &mut UnusedClipboard).unwrap_err();
    assert!(
        error.to_string().contains("original zess server"),
        "{error:#}"
    );
    assert_eq!(
        invoked_commands(&messages(&first_output)),
        ["review.archive"]
    );
    assert!(!messages(&output).iter().any(|message| matches!(
        message,
        ClientMessage::Command { .. } | ClientMessage::QueryOperation { .. }
    )));
}
