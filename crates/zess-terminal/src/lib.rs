use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{self, Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    SynchronizedUpdate,
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    queue,
    style::{Attribute, Print, ResetColor, SetAttribute},
    terminal::{Clear, ClearType},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_ipc::{SocketPath, Stream, framing};
use zess_platform::{NativePlatform, Platform};
use zess_protocol::{
    ClientMessage, MAX_INPUT_BYTES, MAX_MESSAGE_BYTES, RequestId, ServerInstanceId, ServerMessage,
};
use zess_ui::{CursorShape, Frame, FrameRow, MouseEvent, MouseKind};

mod clipboard;
mod clipboard_actions;
mod clipboard_helper;
mod clipboard_write;
mod state;
mod style;
mod terminal;

#[cfg(test)]
mod tests;

use clipboard_actions::ClipboardActions;

pub use clipboard::{
    Clipboard, ClipboardJob, ClipboardReadJob, ClipboardWrite, ClipboardWriteJob, NativeClipboard,
    NoClipboardJob, Osc52Clipboard, TerminalClipboard,
};
pub use clipboard_helper::{CLIPBOARD_HELPER_ARG, run_clipboard_helper};
pub use state::RemoteStateCache;
pub use style::resolve_in as resolve_style;
pub use terminal::{CrosstermTerminal, TerminalIo};

const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_CONNECT_ATTEMPTS: u8 = 5;
const ATTACH_TIMEOUT: Duration = Duration::from_secs(3);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(16);
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(400);

/// Run the terminal client connected to a local zess server socket.
pub fn run(socket: &SocketPath) -> Result<()> {
    run_files(socket, &[])
}

/// Run the terminal client and open the requested files on the first attach.
pub fn run_files(socket: &SocketPath, files: &[PathBuf]) -> Result<()> {
    run_connection(&mut LocalConnectionFactory(socket.clone()), files)
}

/// Creates a fresh transport for the first connection and each reconnect.
pub trait ConnectionFactory {
    fn connect(&mut self) -> Result<Box<dyn ConnectionStream>>;

    fn connect_with_cancellation(
        &mut self,
        cancellation: &zess_platform::CancellationToken,
    ) -> Result<Box<dyn ConnectionStream>> {
        cancellation.check()?;
        self.connect()
    }
}

/// Nonblocking byte transport consumed by the terminal event loop.
pub trait ConnectionStream: Read + Write {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
}

impl ConnectionStream for Stream {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        Stream::set_nonblocking(self, nonblocking)
    }
}

pub struct LocalConnectionFactory(pub SocketPath);

impl ConnectionFactory for LocalConnectionFactory {
    fn connect(&mut self) -> Result<Box<dyn ConnectionStream>> {
        Stream::connect(&self.0)
            .map(|stream| Box::new(stream) as Box<dyn ConnectionStream>)
            .with_context(|| format!("connect to zess socket {}", self.0.display()))
    }
}

/// Runs a terminal client over a reconnectable local or remote transport.
pub fn run_connection(factory: &mut impl ConnectionFactory, files: &[PathBuf]) -> Result<()> {
    run_connection_with_platform(factory, files, &NativePlatform)
}

/// Runs a terminal client with host identity facts supplied by the launcher.
pub fn run_connection_with_platform(
    factory: &mut impl ConnectionFactory,
    files: &[PathBuf],
    platform: &dyn Platform,
) -> Result<()> {
    let mut terminal = CrosstermTerminal::enter()?;
    run_connection_with_devices_and_platform(
        factory,
        files,
        &mut terminal,
        &mut TerminalClipboard::default(),
        platform,
    )
}

/// Runs with caller-owned terminal and clipboard devices.
pub fn run_connection_with_devices(
    factory: &mut impl ConnectionFactory,
    files: &[PathBuf],
    terminal: &mut impl TerminalIo,
    clipboard: &mut impl Clipboard,
) -> Result<()> {
    run_connection_with_devices_and_platform(factory, files, terminal, clipboard, &NativePlatform)
}

/// Runs the terminal client with host identity facts supplied by the launcher.
pub fn run_connection_with_devices_and_platform(
    factory: &mut impl ConnectionFactory,
    files: &[PathBuf],
    terminal: &mut impl TerminalIo,
    clipboard: &mut impl Clipboard,
    platform: &dyn Platform,
) -> Result<()> {
    let files = files
        .iter()
        .map(|path| {
            path.to_str()
                .with_context(|| format!("file path is not valid UTF-8: {}", path.display()))
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>>>()?;
    run_client(factory, client_id(platform)?, files, terminal, clipboard)
}

fn client_id(platform: &dyn Platform) -> Result<String> {
    static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);
    let nonce = NEXT_CLIENT
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| anyhow::anyhow!("client namespace sequence exhausted"))?;
    let timestamp = platform
        .system_time()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(format!("{}-{timestamp}-{nonce}", platform.process_id()))
}

struct Connection {
    stream: Box<dyn ConnectionStream>,
    input: framing::Decoder,
    output: VecDeque<Vec<u8>>,
    written: usize,
}

impl Connection {
    fn connect(
        factory: &mut impl ConnectionFactory,
        client_id: &str,
        files: &[String],
        context_epoch: Option<u64>,
        width: u16,
        height: u16,
    ) -> Result<Self> {
        let stream = factory.connect()?;
        stream.set_nonblocking(true)?;
        let mut connection = Self {
            stream,
            input: framing::Decoder::default(),
            output: VecDeque::new(),
            written: 0,
        };
        connection.send(&ClientMessage::Attach {
            client_id: client_id.to_owned(),
            files: files.to_vec(),
            context_epoch,
            width,
            height,
        })?;
        Ok(connection)
    }

    fn send(&mut self, message: &ClientMessage) -> serde_json::Result<()> {
        self.output.push_back(framing::encode(message)?);
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<ServerMessage>> {
        // Limit work per UI turn even while the peer is continuously readable/writable.
        let mut budget = 64 * 1024;
        while budget > 0 {
            let Some(message) = self.output.front() else {
                break;
            };
            let end = message.len().min(self.written + budget);
            match self.stream.write(&message[self.written..end]) {
                Ok(0) => anyhow::bail!("server closed the connection"),
                Ok(count) => {
                    budget -= count;
                    self.written += count;
                    if self.written == message.len() {
                        self.output.pop_front();
                        self.written = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let mut messages = Vec::new();
        let mut bytes = [0; 8192];
        for _ in 0..8 {
            match self.stream.read(&mut bytes) {
                Ok(0) if !messages.is_empty() => break,
                Ok(0) => anyhow::bail!("server closed the connection"),
                Ok(count) => {
                    for fragment in bytes[..count].split_inclusive(|byte| *byte == b'\n') {
                        self.input.push(fragment, MAX_MESSAGE_BYTES)?;
                        if fragment.last() == Some(&b'\n') {
                            messages.push(
                                self.input
                                    .decode_next()?
                                    .expect("fragment completes a message"),
                            );
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(messages)
    }
}

enum ClientConnection {
    Disconnected {
        retry_at: Instant,
        attempts: u8,
    },
    Attaching {
        connection: Connection,
        deadline: Instant,
        attempts: u8,
    },
    Attached(Connection),
}

impl ClientConnection {
    fn active_mut(&mut self) -> Option<&mut Connection> {
        match self {
            Self::Attaching { connection, .. } | Self::Attached(connection) => Some(connection),
            Self::Disconnected { .. } => None,
        }
    }

    fn attached(&self) -> bool {
        matches!(self, Self::Attached(_))
    }

    fn disconnect(&mut self) {
        let attempts = match self {
            Self::Disconnected { attempts, .. } | Self::Attaching { attempts, .. } => *attempts,
            Self::Attached(_) => 0,
        };
        *self = Self::Disconnected {
            retry_at: Instant::now() + RECONNECT_INTERVAL,
            attempts,
        };
    }

    fn finish_attach(&mut self) {
        let old = std::mem::replace(
            self,
            Self::Disconnected {
                retry_at: Instant::now(),
                attempts: 0,
            },
        );
        *self = match old {
            Self::Attaching { connection, .. } | Self::Attached(connection) => {
                Self::Attached(connection)
            }
            disconnected => disconnected,
        };
    }
}

#[derive(Clone, Copy)]
pub enum InputRecovery {
    Ready,
    ConfirmReplay { epoch: u64 },
    AwaitSkip { epoch: u64, through: u64 },
}

enum PendingInput {
    Key(String),
    TrackedKey { key: String, request_id: RequestId },
    Mouse,
}

struct PendingOperation {
    server_instance: ServerInstanceId,
    exits_client: bool,
    action_id: Option<u64>,
    workspace_change: Option<zess_protocol::ClientActionKind>,
}

struct InputLedger {
    server_instance: Option<ServerInstanceId>,
    pending: VecDeque<(u64, PendingInput)>,
    next_sequence: u64,
    next_request_id: u64,
    acknowledged: u64,
    operations: HashMap<RequestId, PendingOperation>,
}

impl InputLedger {
    fn new() -> Self {
        Self {
            server_instance: None,
            pending: VecDeque::new(),
            next_sequence: 1,
            next_request_id: 1,
            acknowledged: 0,
            operations: HashMap::new(),
        }
    }

    fn request_id(&mut self, client_id: &str) -> Result<RequestId> {
        let sequence = self.next_request_id;
        self.next_request_id = sequence
            .checked_add(1)
            .context("client request ID sequence exhausted")?;
        Ok(RequestId(format!("{client_id}-{sequence}")))
    }

    fn attach(&mut self, server_instance: ServerInstanceId, acknowledged: u64) -> bool {
        let replaced = self
            .server_instance
            .as_ref()
            .is_some_and(|previous| previous != &server_instance);
        if replaced {
            // A new server has a new deduplication ledger. Only unresolved input
            // crosses this boundary; already acknowledged input must stay consumed.
            self.acknowledged = 0;
            self.next_sequence = 1;
            for (sequence, _) in &mut self.pending {
                *sequence = self.next_sequence;
                self.next_sequence += 1;
            }
        }
        self.server_instance = Some(server_instance);
        self.acknowledge(acknowledged);
        replaced
    }

    fn acknowledge(&mut self, acknowledged: u64) {
        self.acknowledged = self.acknowledged.max(acknowledged);
        while self
            .pending
            .front()
            .is_some_and(|(sequence, _)| *sequence <= self.acknowledged)
        {
            self.pending.pop_front();
        }
        self.next_sequence = self.next_sequence.max(self.acknowledged.saturating_add(1));
    }

    fn push(&mut self, input: PendingInput) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.pending.push_back((sequence, input));
        sequence
    }
}

enum ClipboardRead<J = ClipboardJob> {
    Idle,
    Reading {
        server_instance: ServerInstanceId,
        id: u64,
        job: J,
    },
    Ready {
        server_instance: ServerInstanceId,
        id: u64,
        response: ClientMessage,
    },
}

impl<J: ClipboardReadJob> ClipboardRead<J> {
    fn attach(&mut self, server_instance: &ServerInstanceId) {
        match self {
            Self::Reading {
                server_instance: previous,
                ..
            }
            | Self::Ready {
                server_instance: previous,
                ..
            } if previous != server_instance => *self = Self::Idle,
            _ => {}
        }
    }

    fn request(
        &mut self,
        server_instance: &ServerInstanceId,
        id: u64,
        primary: bool,
        connection: &mut Connection,
        provider: &mut impl Clipboard<ReadJob = J>,
    ) -> Result<()> {
        self.attach(server_instance);
        match self {
            Self::Reading { id: current, .. } if *current == id => return Ok(()),
            Self::Ready {
                id: current,
                response,
                ..
            } if *current == id => {
                connection.send(response)?;
                return Ok(());
            }
            _ => {}
        }
        *self = match provider.read(primary) {
            Ok(job) => Self::Reading {
                server_instance: server_instance.clone(),
                id,
                job,
            },
            Err(error) => Self::Ready {
                server_instance: server_instance.clone(),
                id,
                response: clipboard_response(id, Err(error))?,
            },
        };
        if let Self::Ready { response, .. } = self {
            connection.send(response)?;
        }
        Ok(())
    }

    fn poll(&mut self, connection: &mut ClientConnection) -> Result<()> {
        if let Self::Reading {
            server_instance,
            id,
            job,
        } = self
            && let Some(result) = job.poll()
        {
            let (server_instance, id) = (server_instance.clone(), *id);
            *self = Self::Ready {
                server_instance,
                id,
                response: clipboard_response(id, result)?,
            };
            if connection.attached()
                && let Some(active) = connection.active_mut()
                && let Self::Ready { response, .. } = self
            {
                active.send(response)?;
            }
        }
        Ok(())
    }
}

fn clipboard_response(id: u64, result: Result<String>) -> Result<ClientMessage> {
    let (text, error) = match result {
        Ok(text) => (Some(text), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let response = ClientMessage::ClipboardResponse { id, text, error };
    if framing::encode(&response)?.len() > MAX_INPUT_BYTES {
        return Ok(ClientMessage::ClipboardResponse {
            id,
            text: None,
            error: Some("clipboard contents exceed the input limit".into()),
        });
    }
    Ok(response)
}

struct QueuedCommand {
    server_instance: ServerInstanceId,
    action: zess_protocol::CommandAction,
}

struct ClientUiState {
    notice: Option<String>,
    dirty: bool,
    clicks: ClickTracker,
    commands: VecDeque<QueuedCommand>,
}

fn run_client<C: Clipboard>(
    factory: &mut impl ConnectionFactory,
    client_id: String,
    files: Vec<String>,
    terminal: &mut impl TerminalIo,
    provider: &mut C,
) -> Result<()> {
    let (mut width, mut height) = terminal.size().unwrap_or((80, 24));
    let mut connection = ClientConnection::Disconnected {
        retry_at: Instant::now(),
        attempts: 0,
    };
    let mut frame = Frame {
        width,
        height,
        ..Frame::default()
    };
    let mut ledger = InputLedger::new();
    let mut submitted_actions = HashSet::new();
    let mut client_workspace = zess_protocol::ClientWorkspace::default();
    let mut context_epoch = None;
    let mut remote_state: Option<RemoteStateCache> = None;
    let mut attach_metadata = None;
    let mut resync_deadline = None;
    let mut recovery = InputRecovery::Ready;
    let mut clipboard = ClipboardActions::new();
    let mut ui = ClientUiState {
        notice: Some("Connecting to zess server...".to_owned()),
        dirty: true,
        clicks: ClickTracker::default(),
        commands: VecDeque::new(),
    };

    loop {
        if let ClientConnection::Disconnected { retry_at, attempts } = &connection {
            if *attempts >= MAX_CONNECT_ATTEMPTS {
                anyhow::bail!(
                    "could not attach to zess after {MAX_CONNECT_ATTEMPTS} attempts: {}",
                    ui.notice.as_deref().unwrap_or("connection lost")
                );
            }
            if Instant::now() >= *retry_at {
                let attempts = attempts + 1;
                terminal.suspend()?;
                let attempt =
                    Connection::connect(factory, &client_id, &files, context_epoch, width, height);
                terminal.resume()?;
                connection = match attempt {
                    Ok(connection) => {
                        ui.notice = Some("Connected; attaching...".into());
                        ClientConnection::Attaching {
                            connection,
                            deadline: Instant::now() + ATTACH_TIMEOUT,
                            attempts,
                        }
                    }
                    Err(error) => {
                        ui.notice = Some(format!("Connection failed: {error:#}"));
                        ClientConnection::Disconnected {
                            retry_at: Instant::now() + RECONNECT_INTERVAL,
                            attempts,
                        }
                    }
                };
                ui.dirty = true;
            }
        }
        if connection.attached()
            && remote_state.is_some()
            && resync_deadline.is_none()
            && matches!(recovery, InputRecovery::Ready)
        {
            let state = &remote_state.as_ref().unwrap().state;
            for pending in &state.client_actions {
                if ledger
                    .operations
                    .values()
                    .any(|operation| operation.action_id.is_some())
                {
                    break;
                }
                if !submitted_actions.contains(&pending.id) {
                    let action = match &pending.kind {
                        zess_protocol::ClientActionKind::Close { buffer_id, force } => {
                            let Some(buffer) =
                                state.buffers.iter().find(|buffer| buffer.id == *buffer_id)
                            else {
                                continue;
                            };
                            if client_workspace.buffers == [*buffer_id]
                                && matches!(
                                    &buffer.kind,
                                    zess_protocol::BufferKind::Text { path: None }
                                )
                                && !buffer.modified
                            {
                                zess_protocol::CommandAction::ClientExit {
                                    action_id: pending.id,
                                    buffer_id: *buffer_id,
                                }
                            } else {
                                zess_protocol::CommandAction::CloseBuffer {
                                    action_id: pending.id,
                                    buffer_id: *buffer_id,
                                    replacement: client_workspace
                                        .buffers
                                        .iter()
                                        .copied()
                                        .find(|id| *id != *buffer_id),
                                    force: *force,
                                }
                            }
                        }
                        _ => zess_protocol::CommandAction::AcknowledgeClientAction {
                            action_id: pending.id,
                        },
                    };
                    submitted_actions.insert(pending.id);
                    ui.commands.push_back(QueuedCommand {
                        server_instance: ledger.server_instance.clone().unwrap(),
                        action,
                    });
                    break;
                }
            }
            while let Some(command) = ui.commands.pop_front() {
                if ledger.server_instance.as_ref() != Some(&command.server_instance) {
                    ui.notice =
                        Some("Session changed; the queued command was not submitted".into());
                    ui.dirty = true;
                    continue;
                }
                let action = command.action;
                let exits_client =
                    matches!(&action, zess_protocol::CommandAction::ClientExit { .. });
                let action_id = match &action {
                    zess_protocol::CommandAction::AcknowledgeClientAction { action_id }
                    | zess_protocol::CommandAction::CloseBuffer { action_id, .. }
                    | zess_protocol::CommandAction::ClientExit { action_id, .. } => {
                        Some(*action_id)
                    }
                    _ => None,
                };
                let workspace_change = match &action {
                    zess_protocol::CommandAction::AcknowledgeClientAction { action_id } => state
                        .client_actions
                        .iter()
                        .find(|pending| pending.id == *action_id)
                        .map(|pending| pending.kind.clone()),
                    _ => None,
                };
                let request_id = ledger.request_id(&client_id)?;
                let server_instance = ledger.server_instance.clone().unwrap();
                let message = ClientMessage::Command {
                    request_id: request_id.clone(),
                    server_instance: server_instance.clone(),
                    action,
                };
                if framing::encode(&message)?.len() > MAX_INPUT_BYTES {
                    ui.notice = Some("Command exceeds the input byte limit".into());
                    ui.dirty = true;
                    continue;
                }
                ledger.operations.insert(
                    request_id,
                    PendingOperation {
                        server_instance,
                        exits_client,
                        action_id,
                        workspace_change,
                    },
                );
                connection.active_mut().unwrap().send(&message)?;
            }
        }

        let messages = match connection.active_mut().map(Connection::poll).transpose() {
            Ok(messages) => messages.unwrap_or_default(),
            Err(error) => {
                ui.notice = Some(format!("Disconnected; reconnecting: {error}"));
                connection.disconnect();
                ui.dirty = true;
                Vec::new()
            }
        };
        for message in messages {
            let primary_write = matches!(&message, ServerMessage::ClipboardWritePrimary { .. });
            match message {
                ServerMessage::Attached {
                    server_instance,
                    acknowledged,
                    context_epoch: epoch,
                    replay_allowed,
                    frame: _,
                } => {
                    attach_metadata = Some((server_instance, acknowledged, epoch, replay_allowed));
                    remote_state = None;
                    ui.dirty = true;
                }
                ServerMessage::Snapshot {
                    session_id,
                    server_instance,
                    state_revision,
                    state,
                    frame: _,
                } => {
                    let expected_server = attach_metadata
                        .as_ref()
                        .map(|(instance, _, _, _)| instance)
                        .or(ledger.server_instance.as_ref());
                    if expected_server.is_some_and(|expected| expected != &server_instance) {
                        connection.disconnect();
                        ui.notice = Some("Server identity changed; reconnecting".into());
                        ui.dirty = true;
                        break;
                    }
                    let next_state = RemoteStateCache {
                        session_id,
                        server_instance: server_instance.clone(),
                        revision: state_revision,
                        state,
                    };
                    match zess_protocol::render_snapshot(&next_state.state) {
                        Ok(next_frame) => {
                            frame = next_frame;
                            remote_state = Some(next_state);
                        }
                        Err(error) => {
                            if let Some(active) = connection.active_mut() {
                                active.send(&ClientMessage::Resync {
                                    session_id: next_state.session_id,
                                    state_revision: remote_state
                                        .as_ref()
                                        .map_or(0, |state| state.revision),
                                })?;
                            }
                            resync_deadline = Some(Instant::now() + ATTACH_TIMEOUT);
                            ui.notice = Some(format!("Synchronizing session state: {error}"));
                            ui.dirty = true;
                            continue;
                        }
                    }
                    resync_deadline = None;
                    client_workspace = remote_state.as_ref().unwrap().state.workspace.clone();
                    let Some((attached_instance, acknowledged, epoch, replay_allowed)) =
                        attach_metadata.take()
                    else {
                        if matches!(recovery, InputRecovery::Ready) {
                            ui.notice = None;
                        }
                        ui.dirty = true;
                        continue;
                    };
                    anyhow::ensure!(
                        attached_instance == server_instance,
                        "snapshot does not match attachment server"
                    );
                    clipboard.attach(&server_instance);
                    let replaced = ledger.attach(server_instance, acknowledged);
                    if replaced {
                        submitted_actions.clear();
                    }
                    connection.finish_attach();
                    context_epoch = Some(epoch);
                    recovery = match recovery {
                        InputRecovery::AwaitSkip { through, .. } if !ledger.pending.is_empty() => {
                            InputRecovery::AwaitSkip {
                                epoch,
                                through: if replaced {
                                    ledger.pending.back().unwrap().0
                                } else {
                                    through
                                },
                            }
                        }
                        _ if ledger.pending.is_empty() => InputRecovery::Ready,
                        InputRecovery::ConfirmReplay { .. } => {
                            InputRecovery::ConfirmReplay { epoch }
                        }
                        _ if replay_allowed && !replaced => InputRecovery::Ready,
                        _ => InputRecovery::ConfirmReplay { epoch },
                    };
                    let active = connection.active_mut().unwrap();
                    ui.notice = None;
                    match recovery {
                        InputRecovery::Ready => {
                            send_pending(
                                active,
                                &ledger.pending,
                                epoch,
                                ledger.server_instance.as_ref().unwrap(),
                            )?;
                        }
                        InputRecovery::ConfirmReplay { .. } => {}
                        InputRecovery::AwaitSkip { through, .. } => {
                            active.send(&ClientMessage::Skip {
                                through,
                                context_epoch: epoch,
                            })?;
                        }
                    }
                    for (request_id, operation) in &ledger.operations {
                        active.send(&ClientMessage::QueryOperation {
                            request_id: request_id.clone(),
                            server_instance: operation.server_instance.clone(),
                        })?;
                    }
                    // Resizing while attaching must reach the final attached View.
                    if frame.width != width || frame.height != height {
                        active.send(&ClientMessage::Resize { width, height })?;
                    }
                    ui.dirty = true;
                }
                ServerMessage::Delta {
                    session_id,
                    server_instance,
                    base_revision,
                    state_revision,
                    delta,
                    frame: _,
                } => {
                    let mut next_state = remote_state.clone();
                    let rendered = if resync_deadline.is_none()
                        && next_state.as_mut().is_some_and(|state| {
                            state.apply_delta(
                                &session_id,
                                &server_instance,
                                base_revision,
                                state_revision,
                                delta,
                            )
                        }) {
                        zess_protocol::render_snapshot(&next_state.as_ref().unwrap().state).ok()
                    } else {
                        None
                    };
                    if let Some(next_frame) = rendered {
                        frame = next_frame;
                        remote_state = next_state;
                        ui.dirty = true;
                    } else if resync_deadline.is_none() {
                        if let Some(active) = connection.active_mut() {
                            active.send(&ClientMessage::Resync {
                                session_id,
                                state_revision: remote_state
                                    .as_ref()
                                    .map_or(0, |state| state.revision),
                            })?;
                        }
                        resync_deadline = Some(Instant::now() + ATTACH_TIMEOUT);
                        ui.notice = Some("Synchronizing session state...".into());
                        ui.dirty = true;
                    }
                }
                ServerMessage::Ack {
                    acknowledged,
                    context_epoch: epoch,
                } => {
                    if context_epoch == Some(epoch) {
                        ledger.acknowledge(acknowledged);
                        if matches!(recovery, InputRecovery::AwaitSkip { through, .. } if ledger.acknowledged >= through)
                            || matches!(recovery, InputRecovery::ConfirmReplay { .. } if ledger.pending.is_empty())
                        {
                            recovery = InputRecovery::Ready;
                            ui.dirty = true;
                        }
                    }
                }
                ServerMessage::CommandResult {
                    request_id,
                    server_instance,
                    result,
                } => {
                    if ledger.operations.contains_key(&request_id)
                        && ledger.server_instance.as_ref() == Some(&server_instance)
                    {
                        match result {
                            zess_protocol::OperationResult::Completed { outcome, .. } => {
                                let operation = ledger.operations.remove(&request_id).unwrap();
                                let exited = operation.exits_client
                                    && matches!(&outcome, zess_protocol::CommandOutcome::Applied);
                                match (&outcome, operation.workspace_change) {
                                    (zess_protocol::CommandOutcome::Applied, Some(change)) => {
                                        client_workspace.apply(&change)
                                    }
                                    (
                                        zess_protocol::CommandOutcome::BufferClosed {
                                            buffer_id,
                                            replacement,
                                        },
                                        _,
                                    ) => {
                                        client_workspace.buffers.retain(|id| id != buffer_id);
                                        if !client_workspace.buffers.contains(replacement) {
                                            client_workspace.buffers.push(*replacement);
                                        }
                                        client_workspace.current = Some(*replacement);
                                    }
                                    _ => {}
                                }
                                match outcome {
                                    zess_protocol::CommandOutcome::Failed { message } => {
                                        ui.notice = Some(message);
                                        ui.dirty = true;
                                    }
                                    _ => {}
                                }
                                if let Some(active) = connection.active_mut() {
                                    active.send(&ClientMessage::ResultAck {
                                        request_id,
                                        server_instance,
                                    })?;
                                }
                                if exited {
                                    return Ok(());
                                }
                            }
                            zess_protocol::OperationResult::Unknown => {
                                if let Some(operation) = ledger.operations.remove(&request_id) {
                                    if let Some(action_id) = operation.action_id {
                                        submitted_actions.remove(&action_id);
                                    }
                                }
                                ui.notice = Some("The previous operation's outcome is unknown; check the remote state before retrying".into());
                                ui.dirty = true;
                            }
                            zess_protocol::OperationResult::Pending => {}
                        }
                    }
                }
                ServerMessage::Detached => return Ok(()),
                ServerMessage::Update { .. } => {}
                ServerMessage::ClipboardWrite { text }
                | ServerMessage::ClipboardWritePrimary { text } => {
                    clipboard.copy(primary_write, text);
                }
                ServerMessage::ClipboardRead { id, primary } => {
                    if connection.attached()
                        && let Some(instance) = &ledger.server_instance
                    {
                        clipboard.read(instance.clone(), id, primary);
                    }
                }
                ServerMessage::Rejected { reason } => {
                    if ledger.pending.is_empty() {
                        anyhow::bail!("zess server rejected the client: {}", sanitize(&reason));
                    }
                    ui.notice = Some(format!(
                        "Server rejected reconnect; retaining queued input: {}",
                        sanitize(&reason)
                    ));
                    connection.disconnect();
                    ui.dirty = true;
                    break;
                }
                ServerMessage::Exited => return Ok(()),
                ServerMessage::Status { .. } => anyhow::bail!("unexpected server status response"),
            }
        }
        if let ClientConnection::Attaching { deadline, .. } = connection
            && Instant::now() >= deadline
        {
            ui.notice = Some("Timed out waiting for server attachment".into());
            connection.disconnect();
            ui.dirty = true;
        }
        if resync_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            ui.notice = Some("Timed out synchronizing session state; reconnecting".into());
            connection.disconnect();
            resync_deadline = None;
            ui.dirty = true;
        }
        if let Some(notice) = clipboard.poll(provider, terminal.writer(), &mut connection)? {
            ui.notice = Some(notice);
            ui.dirty = true;
        }

        let synchronized =
            connection.attached() && remote_state.is_some() && resync_deadline.is_none();
        let ready = synchronized && matches!(recovery, InputRecovery::Ready);
        if ui.dirty {
            terminal.set_mouse_capture(
                frame.mouse_enabled
                    && connection.attached()
                    && remote_state.is_some()
                    && resync_deadline.is_none()
                    && matches!(recovery, InputRecovery::Ready),
            )?;
            if !terminal.mouse_capture() {
                ui.clicks = ClickTracker::default();
            }
            let control_notice = match recovery {
                InputRecovery::ConfirmReplay { .. } => Some(recovery_notice()),
                InputRecovery::AwaitSkip { .. } => {
                    Some("Discarding queued input; waiting for server confirmation".into())
                }
                InputRecovery::Ready => None,
            };
            let notice = match (control_notice, ui.notice.as_deref()) {
                (Some(control), Some(message)) => Some(format!("{control} | {message}")),
                (Some(control), None) => Some(control),
                (None, _) => ui.notice.clone(),
            };
            let notice = notice.map(|notice| {
                if !connection.attached() {
                    format!("Esc/Ctrl-c exit | {notice}")
                } else if !matches!(recovery, InputRecovery::Ready) {
                    format!("Ctrl-c exit | {notice}")
                } else {
                    notice
                }
            });
            let (width, height) = terminal.size().unwrap_or((frame.width, frame.height));
            render(terminal.writer(), &frame, width, height, notice.as_deref())?;
            ui.dirty = false;
        }
        let Some(event) = terminal.poll_event(EVENT_POLL_INTERVAL)? else {
            continue;
        };
        let new_input = match &event {
            Event::Paste(_) => true,
            Event::Key(key) => key.kind != KeyEventKind::Release,
            _ => false,
        };
        if ready && new_input && ui.notice.take().is_some() {
            ui.dirty = true;
        }
        if !matches!(event, Event::Mouse(_)) {
            ui.clicks = ClickTracker::default();
        }
        match event {
            Event::Paste(text) => {
                if !ready {
                    ui.notice = Some("Session is not ready; paste was not submitted".into());
                    ui.dirty = true;
                    continue;
                }
                ui.commands.push_back(QueuedCommand {
                    server_instance: ledger.server_instance.clone().unwrap(),
                    action: zess_protocol::CommandAction::Paste { text },
                });
            }
            Event::Resize(new_width, new_height) => {
                width = new_width;
                height = new_height;
                ui.dirty = true;
                if connection.attached()
                    && let Some(active) = connection.active_mut()
                {
                    active.send(&ClientMessage::Resize { width, height })?;
                }
            }
            Event::Mouse(mouse)
                if terminal.mouse_capture() && frame.width == width && frame.height == height =>
            {
                let Some(kind) = ui
                    .clicks
                    .at(mouse.kind, mouse.column, mouse.row, Instant::now())
                else {
                    continue;
                };
                let sequence = ledger.push(PendingInput::Mouse);
                if let Some(active) = connection.active_mut() {
                    active.send(&ClientMessage::Mouse {
                        sequence,
                        context_epoch: context_epoch
                            .expect("attached clients have a context epoch"),
                        mouse_context: frame.mouse_context,
                        event: MouseEvent {
                            kind,
                            column: mouse.column,
                            row: mouse.row,
                        },
                    })?;
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let Some(active) = connection.active_mut() {
                        active.send(&ClientMessage::Detach)?;
                        let _ = active.poll();
                    }
                    return Ok(());
                }
                let cancel =
                    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                if (!connection.attached() && key.code == KeyCode::Esc)
                    || ((!connection.attached() || !matches!(recovery, InputRecovery::Ready))
                        && cancel)
                {
                    return Ok(());
                }
                if !synchronized {
                    continue;
                }
                let active = connection.active_mut().unwrap();
                match recovery {
                    InputRecovery::ConfirmReplay { epoch } => {
                        if key.code == KeyCode::Char('r')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            recovery = InputRecovery::Ready;
                            for (_, input) in &ledger.pending {
                                if let PendingInput::TrackedKey { request_id, .. } = input {
                                    ledger.operations.insert(
                                        request_id.clone(),
                                        PendingOperation {
                                            server_instance: ledger
                                                .server_instance
                                                .clone()
                                                .unwrap(),
                                            exits_client: false,
                                            action_id: None,
                                            workspace_change: None,
                                        },
                                    );
                                }
                            }
                            send_pending(
                                active,
                                &ledger.pending,
                                epoch,
                                ledger.server_instance.as_ref().unwrap(),
                            )?;
                        } else if key.code == KeyCode::Esc {
                            if let Some((through, _)) = ledger.pending.back() {
                                let through = *through;
                                recovery = InputRecovery::AwaitSkip { epoch, through };
                                active.send(&ClientMessage::Skip {
                                    through,
                                    context_epoch: epoch,
                                })?;
                            } else {
                                recovery = InputRecovery::Ready;
                            }
                        }
                        ui.dirty = true;
                        continue;
                    }
                    InputRecovery::AwaitSkip { epoch, through } => {
                        if key.code == KeyCode::Esc {
                            active.send(&ClientMessage::Skip {
                                through,
                                context_epoch: epoch,
                            })?;
                        }
                        continue;
                    }
                    InputRecovery::Ready => {}
                }
                let Some(key) = encode_key(key) else { continue };
                let epoch = context_epoch.expect("attached clients have a context epoch");
                let tracked = key == "Enter";
                if tracked {
                    let request_id = ledger.request_id(&client_id)?;
                    let server_instance = ledger.server_instance.clone().unwrap();
                    let sequence = ledger.push(PendingInput::TrackedKey {
                        key: key.clone(),
                        request_id: request_id.clone(),
                    });
                    ledger.operations.insert(
                        request_id.clone(),
                        PendingOperation {
                            server_instance: server_instance.clone(),
                            exits_client: false,
                            action_id: None,
                            workspace_change: None,
                        },
                    );
                    active.send(&ClientMessage::TrackedInput {
                        sequence,
                        key,
                        context_epoch: epoch,
                        request_id,
                        server_instance,
                    })?;
                } else {
                    let sequence = ledger.push(PendingInput::Key(key.clone()));
                    active.send(&ClientMessage::Input {
                        sequence,
                        key,
                        context_epoch: epoch,
                    })?;
                }
            }
            _ => {}
        }
    }
}

fn send_pending(
    connection: &mut Connection,
    pending: &VecDeque<(u64, PendingInput)>,
    context_epoch: u64,
    server_instance: &ServerInstanceId,
) -> Result<()> {
    for (sequence, input) in pending {
        let message = match input {
            PendingInput::Key(key) => ClientMessage::Input {
                sequence: *sequence,
                key: key.clone(),
                context_epoch,
            },
            PendingInput::TrackedKey { key, request_id } => ClientMessage::TrackedInput {
                sequence: *sequence,
                key: key.clone(),
                request_id: request_id.clone(),
                server_instance: server_instance.clone(),
                context_epoch,
            },
            PendingInput::Mouse => ClientMessage::Skip {
                through: *sequence,
                context_epoch,
            },
        };
        connection.send(&message)?;
    }
    Ok(())
}

fn recovery_notice() -> String {
    "Session changed: Ctrl-r replay queued input here; Esc discard queued input".to_owned()
}

struct Click {
    at: Instant,
    column: u16,
    row: u16,
}

#[derive(Default)]
enum ClickTracker {
    #[default]
    Idle,
    Pressed(Click),
    Released(Click),
}

impl ClickTracker {
    fn at(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        now: Instant,
    ) -> Option<MouseKind> {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Self::Released(click) = self
                    && click.column == column
                    && click.row == row
                    && now.duration_since(click.at) < DOUBLE_CLICK_INTERVAL
                {
                    *self = Self::Idle;
                    return Some(MouseKind::DoubleClick);
                }
                *self = Self::Pressed(Click {
                    at: now,
                    column,
                    row,
                });
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Self::Pressed(click) = std::mem::take(self)
                    && click.column == column
                    && click.row == row
                {
                    *self = Self::Released(click);
                }
            }
            MouseEventKind::Moved => {
                if let Self::Pressed(click) | Self::Released(click) = self
                    && (click.column != column || click.row != row)
                {
                    *self = Self::Idle;
                }
            }
            _ => *self = Self::Idle,
        }
        mouse_kind(kind)
    }
}

fn mouse_kind(kind: MouseEventKind) -> Option<MouseKind> {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => Some(MouseKind::Press),
        MouseEventKind::Drag(MouseButton::Left) => Some(MouseKind::Drag),
        MouseEventKind::Up(MouseButton::Left) => Some(MouseKind::Release),
        MouseEventKind::ScrollUp => Some(MouseKind::ScrollUp),
        MouseEventKind::ScrollDown => Some(MouseKind::ScrollDown),
        _ => None,
    }
}

fn encode_key(event: KeyEvent) -> Option<String> {
    let modifiers = event.modifiers;
    let key = match event.code {
        KeyCode::Esc => "Esc".to_owned(),
        KeyCode::Enter => "Enter".to_owned(),
        KeyCode::Backspace => "Backspace".to_owned(),
        KeyCode::Delete => "Delete".to_owned(),
        KeyCode::Tab => "Tab".to_owned(),
        KeyCode::BackTab => "S-Tab".to_owned(),
        KeyCode::Left => "Left".to_owned(),
        KeyCode::Right => "Right".to_owned(),
        KeyCode::Up => "Up".to_owned(),
        KeyCode::Down => "Down".to_owned(),
        KeyCode::Home => "Home".to_owned(),
        KeyCode::End => "End".to_owned(),
        KeyCode::PageUp => "PageUp".to_owned(),
        KeyCode::PageDown => "PageDown".to_owned(),
        KeyCode::Char(character) => {
            let character = if modifiers.contains(KeyModifiers::CONTROL) {
                character.to_ascii_lowercase()
            } else {
                character
            };
            let mut prefix = String::new();
            if modifiers.contains(KeyModifiers::CONTROL) {
                prefix.push_str("C-");
            }
            if modifiers.contains(KeyModifiers::ALT) {
                prefix.push_str("A-");
            }
            format!("{prefix}{character}")
        }
        _ => return None,
    };

    Some(key)
}

fn render(
    stdout: &mut impl Write,
    frame: &Frame,
    width: u16,
    height: u16,
    notice: Option<&str>,
) -> Result<()> {
    stdout
        .sync_update(|mut stdout| {
            let body_rows = height.saturating_sub(1) as usize;

            for row in 0..body_rows {
                queue!(
                    stdout,
                    MoveTo(0, row as u16),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                    Clear(ClearType::CurrentLine)
                )?;

                if let Some(frame_row) = frame.rows.get(row) {
                    render_row(&mut stdout, frame_row, width, &frame.styles)?;
                }
            }

            let on_prompt = frame
                .cursor
                .is_some_and(|(_, y)| y == frame.height.saturating_sub(1));
            let prompt = sanitize(&frame.prompt);
            let visible_prompt = clip_to_width(&prompt, width.saturating_sub(1) as usize).0;
            if height > 0 {
                queue!(
                    stdout,
                    MoveTo(0, height - 1),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                    Clear(ClearType::CurrentLine)
                )?;
                if on_prompt && notice.is_none() {
                    queue!(stdout, Print(&visible_prompt))?;
                } else {
                    let pending = clip_to_width(&prompt, width as usize).0;
                    let pending_width = UnicodeWidthStr::width(pending.as_str());
                    let message_width = if pending_width > 0 && notice.is_none() {
                        (width as usize).saturating_sub(pending_width + 1)
                    } else {
                        width as usize
                    };
                    queue!(
                        stdout,
                        Print(
                            clip_to_width(
                                &sanitize(notice.unwrap_or(&frame.status)),
                                message_width
                            )
                            .0
                        )
                    )?;
                    if pending_width > 0 && notice.is_none() {
                        queue!(
                            stdout,
                            MoveTo(width - pending_width as u16, height - 1),
                            Print(pending)
                        )?;
                    }
                }
            }

            if let Some((x, y)) = frame.cursor.filter(|_| notice.is_none()) {
                if width > 0 && height > 0 {
                    let x = x.min(width - 1);
                    let y = if on_prompt {
                        height - 1
                    } else {
                        y.min(height.saturating_sub(2))
                    };
                    let style = match frame.cursor_shape {
                        CursorShape::Block => SetCursorStyle::SteadyBlock,
                        CursorShape::Bar => SetCursorStyle::SteadyBar,
                        CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
                    };
                    queue!(stdout, style, MoveTo(x, y), Show)?;
                } else {
                    queue!(stdout, Hide)?;
                }
            } else {
                queue!(stdout, Hide)?;
            }

            queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
            Ok(())
        })
        .context("flush terminal frame")?
}

fn render_row(
    stdout: &mut impl Write,
    row: &FrameRow,
    width: u16,
    styles: &std::collections::HashMap<String, zess_ui::TextStyle>,
) -> Result<()> {
    let mut remaining = width as usize;

    for span in &row.spans {
        if remaining == 0 {
            break;
        }

        let resolve = |role: &str| style::resolve_in(role, styles);
        let base = span.base_style.as_deref().map(resolve).unwrap_or_default();
        style::apply(stdout, &base.patch(&resolve(&span.style)))?;
        let (text, cells, truncated) = clip_to_width(&sanitize(&span.text), remaining);
        queue!(stdout, Print(text))?;
        remaining -= cells;
        if truncated {
            break;
        }
    }

    queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
    Ok(())
}

fn clip_to_width(text: &str, maximum: usize) -> (String, usize, bool) {
    let mut output = String::new();
    let mut cells = 0;

    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        if cells + width > maximum {
            return (output, cells, true);
        }

        output.push_str(grapheme);
        cells += width;
    }

    (output, cells, false)
}

fn sanitize(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{1b}' => escaped.push_str("\\x1b"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{{{:x}}}", character as u32));
            }
            character => escaped.push(character),
        }
    }

    escaped
}
