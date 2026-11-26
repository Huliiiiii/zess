use std::{
    borrow::Cow,
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
use zess_ipc::platform::{DaemonControl, NativeDaemonControl, ProcessWait};
use zess_ipc::{SocketPath, Stream, framing};
use zess_platform::{NativePlatform, Platform};
use zess_plugin::HostRequest;
use zess_protocol::{
    ClientActionKind, ClientMessage, CommandAction, CommandOutcome, CommandTarget, MAX_INPUT_BYTES,
    MAX_MESSAGE_BYTES, ProtocolMismatch, RequestId, ServerInstanceId, ServerMessage, SessionId,
    VersionInfo, editor_events::EventSource,
};
use zess_ui::{CursorShape, Frame, FrameRow, MouseEvent, MouseKind, fitting_prefix};

mod clipboard;
mod clipboard_actions;
mod clipboard_helper;
mod clipboard_write;
mod connection;
mod documents;
mod editor;
mod pages;
mod presentation;
mod state;
mod statusline;
mod style;
mod terminal;
mod workbench;
mod workspace_view;

#[cfg(test)]
mod tests;

use clipboard_actions::ClipboardActions;
use connection::{ConnectionPage, ConnectionTask};

pub use connection::{
    AuthenticationKind, AuthenticationPrompt, ByteProgress, ConnectionContext, ConnectionProgress,
    SessionCandidate,
};

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
const SERVER_EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the terminal client connected to a local zess server socket.
pub fn run(socket: &SocketPath) -> Result<()> {
    run_files(socket, &[])
}

/// Run the terminal client and open the requested files on the first attach.
pub fn run_files(socket: &SocketPath, files: &[PathBuf]) -> Result<()> {
    run_connection(LocalConnectionFactory(socket.clone()), files)
}

/// Creates a fresh transport for the first connection and each reconnect.
pub trait ConnectionFactory: Send {
    fn connect(&mut self, context: &ConnectionContext) -> Result<Box<dyn ConnectionStream>>;

    fn display_target(&self) -> &str {
        "zess"
    }
}

/// Nonblocking byte transport consumed by the terminal event loop.
pub trait ConnectionStream: Read + Write + Send {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
    fn wait_for_server_exit(&mut self, timeout: Duration) -> Result<bool>;
    fn session_id(&self) -> Option<&zess_protocol::SessionId> {
        None
    }
}

pub struct LocalConnection {
    stream: Stream,
    process: Box<dyn ProcessWait>,
    session_id: Option<zess_protocol::SessionId>,
}

impl LocalConnection {
    pub fn new(stream: Stream, process: Box<dyn ProcessWait>) -> Self {
        Self {
            stream,
            process,
            session_id: None,
        }
    }

    pub fn with_session_id(mut self, session_id: zess_protocol::SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }
}

impl Read for LocalConnection {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}

impl Write for LocalConnection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl ConnectionStream for LocalConnection {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.stream.set_nonblocking(nonblocking)
    }

    fn wait_for_server_exit(&mut self, timeout: Duration) -> Result<bool> {
        self.process
            .wait(timeout)
            .context("wait for zess server to exit")
    }

    fn session_id(&self) -> Option<&zess_protocol::SessionId> {
        self.session_id.as_ref()
    }
}

pub struct LocalConnectionFactory(pub SocketPath);

impl ConnectionFactory for LocalConnectionFactory {
    fn connect(&mut self, context: &ConnectionContext) -> Result<Box<dyn ConnectionStream>> {
        context.cancellation().check()?;
        let stream = Stream::connect(&self.0)
            .with_context(|| format!("connect to zess socket {}", self.0.display()))?;
        let process = NativeDaemonControl.watch(&stream)?;
        Ok(Box::new(LocalConnection::new(stream, process)))
    }
}

/// Runs a terminal client over a reconnectable local or remote transport.
pub fn run_connection(factory: impl ConnectionFactory + 'static, files: &[PathBuf]) -> Result<()> {
    run_connection_with_platform(factory, files, &NativePlatform)
}

/// Runs a terminal client with host identity facts supplied by the launcher.
pub fn run_connection_with_platform(
    factory: impl ConnectionFactory + 'static,
    files: &[PathBuf],
    platform: &dyn Platform,
) -> Result<()> {
    let mut terminal = CrosstermTerminal::enter()?;
    let result = run_connection_with_devices_and_platform(
        factory,
        files,
        &mut terminal,
        &mut TerminalClipboard::default(),
        platform,
    );
    let restored = terminal.restore();
    result.and(restored)
}

/// Runs with caller-owned terminal and clipboard devices.
pub fn run_connection_with_devices(
    factory: impl ConnectionFactory + 'static,
    files: &[PathBuf],
    terminal: &mut impl TerminalIo,
    clipboard: &mut impl Clipboard,
) -> Result<()> {
    run_connection_with_devices_and_platform(factory, files, terminal, clipboard, &NativePlatform)
}

/// Runs the terminal client with host identity facts supplied by the launcher.
pub fn run_connection_with_devices_and_platform(
    factory: impl ConnectionFactory + 'static,
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
    expected_session: Option<SessionId>,
    input: framing::Decoder,
    message_stream: zess_protocol::chunks::MessageStream,
    output: VecDeque<Vec<u8>>,
    written: usize,
    last_read: Option<Instant>,
    pending_attach: Option<ClientMessage>,
}

impl Connection {
    fn connect(
        stream: Box<dyn ConnectionStream>,
        client_id: &str,
        files: &[String],
        expected_session: Option<&SessionId>,
        width: u16,
        height: u16,
    ) -> Result<Self> {
        let expected_session = expected_session
            .cloned()
            .or_else(|| stream.session_id().cloned());
        stream.set_nonblocking(true)?;
        let mut connection = Self {
            stream,
            expected_session: expected_session.clone(),
            input: framing::Decoder::default(),
            message_stream: Default::default(),
            output: VecDeque::new(),
            written: 0,
            last_read: None,
            pending_attach: Some(ClientMessage::Attach {
                expected_session,
                client_id: client_id.to_owned(),
                files: files.to_vec(),
                width,
                height,
            }),
        };
        connection.send(&ClientMessage::Hello {
            version: VersionInfo::new(zess_build::version()),
        })?;
        Ok(connection)
    }

    fn send(&mut self, message: &ClientMessage) -> serde_json::Result<()> {
        let bytes = framing::encode(message)?;
        if bytes.len() > MAX_INPUT_BYTES && message.can_chunk() {
            let json = std::str::from_utf8(&bytes).expect("JSON is UTF-8");
            for (part, last) in zess_protocol::chunks::json_parts(json, MAX_INPUT_BYTES) {
                self.output
                    .push_back(framing::encode(&ClientMessage::Chunk {
                        json: part.to_owned(),
                        last,
                    })?);
            }
        } else {
            self.output.push_back(bytes);
        }
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
                    self.last_read = Some(Instant::now());
                    for fragment in bytes[..count].split_inclusive(|byte| *byte == b'\n') {
                        self.input.push(fragment, MAX_MESSAGE_BYTES)?;
                        if fragment.last() == Some(&b'\n') {
                            let message = self
                                .input
                                .decode_next()?
                                .expect("fragment completes a message");
                            if self.pending_attach.is_some() {
                                match message {
                                    ServerMessage::Hello { version } => {
                                        VersionInfo::new(zess_build::version())
                                            .check_compatible(&version)?;
                                        let attach = self.pending_attach.take().unwrap();
                                        self.send(&attach)?;
                                    }
                                    ServerMessage::Rejected { reason } => {
                                        anyhow::bail!("zess handshake rejected: {reason}")
                                    }
                                    _ => {
                                        anyhow::bail!("expected zess Hello before server messages")
                                    }
                                }
                            } else if let ServerMessage::Chunk { json, last } = message {
                                if let Some(message) = self.message_stream.server(json, last)? {
                                    messages.push(message);
                                }
                            } else {
                                messages.push(message);
                            }
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

    fn wait_for_server_exit(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.stream.wait_for_server_exit(SERVER_EXIT_TIMEOUT)?,
            "zess server did not exit within {} seconds",
            SERVER_EXIT_TIMEOUT.as_secs()
        );
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum AttachKind {
    Retained,
    Pending,
}

struct AttachMetadata {
    workspace_view: Option<zess_protocol::workspace_view::WorkspaceView>,
    server_instance: ServerInstanceId,
    epoch: u64,
}

enum Attachment {
    AwaitingResponse(AttachKind),
    AwaitingSnapshot(AttachKind, AttachMetadata),
    Confirming {
        metadata: AttachMetadata,
        snapshot: Box<RemoteStateCache>,
    },
}

enum ClientConnection {
    Disconnected {
        retry_at: Instant,
        attempts: u8,
    },
    Connecting {
        task: ConnectionTask,
        attempts: u8,
    },
    Failed(anyhow::Error),
    Attaching {
        connection: Connection,
        attachment: Attachment,
        deadline: Instant,
        attempts: u8,
    },
    Attached(Connection),
}

impl ClientConnection {
    fn active_mut(&mut self) -> Option<&mut Connection> {
        match self {
            Self::Attaching { connection, .. } | Self::Attached(connection) => Some(connection),
            Self::Disconnected { .. } | Self::Connecting { .. } | Self::Failed(_) => None,
        }
    }

    fn attached(&self) -> bool {
        matches!(self, Self::Attached(_))
    }

    fn attachment_mut(&mut self) -> Option<&mut Attachment> {
        match self {
            Self::Attaching { attachment, .. } => Some(attachment),
            _ => None,
        }
    }

    fn retry_or_fail(&mut self, error: anyhow::Error) {
        let attempts = match self {
            Self::Connecting { attempts, .. } | Self::Attaching { attempts, .. } => *attempts,
            Self::Attached(_) => 0,
            Self::Disconnected { .. } | Self::Failed(_) => return,
        };
        *self = if attempts >= MAX_CONNECT_ATTEMPTS {
            Self::Failed(error)
        } else {
            Self::Disconnected {
                retry_at: Instant::now() + RECONNECT_INTERVAL,
                attempts,
            }
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

enum OperationCompletion {
    None,
    PageCommand,
    WorkspaceAction {
        id: u64,
        acknowledged_change: Option<ClientActionKind>,
    },
    PagePrompt {
        subscription_id: u64,
        purpose: String,
    },
    OpenDocument {
        view_id: u64,
    },
    DiscardScratch {
        buffer_id: u64,
    },
    Reload {
        buffer_id: u64,
    },
}

impl OperationCompletion {
    fn for_action(action: &CommandAction) -> Self {
        match action {
            CommandAction::PagePrompt {
                subscription_id,
                purpose,
                ..
            } => Self::PagePrompt {
                subscription_id: *subscription_id,
                purpose: purpose.clone(),
            },
            CommandAction::Invoke {
                target: CommandTarget::Page { .. },
                ..
            }
            | CommandAction::Request {
                target: CommandTarget::Page { .. },
                ..
            } => Self::PageCommand,
            CommandAction::DiscardScratch { buffer_id, .. } => Self::DiscardScratch {
                buffer_id: *buffer_id,
            },
            CommandAction::Request {
                target: CommandTarget::Document { buffer_id },
                request: HostRequest::Action { name, .. },
            } if name == "reload" => Self::Reload {
                buffer_id: *buffer_id,
            },
            _ => Self::None,
        }
    }

    fn not_submitted(&self, workbench: &mut workbench::Workbench, reason: &str) {
        match self {
            Self::PagePrompt {
                subscription_id,
                purpose,
            } => workbench.page_prompt_result(
                *subscription_id,
                purpose,
                &CommandOutcome::Failed {
                    message: reason.into(),
                },
            ),
            Self::DiscardScratch { buffer_id } => workbench.scratch_discard_result(*buffer_id),
            Self::Reload { buffer_id } => workbench.documents.cancel_queued_reload(*buffer_id),
            Self::None
            | Self::PageCommand
            | Self::WorkspaceAction { .. }
            | Self::OpenDocument { .. } => {}
        }
    }
}

struct PendingOperation {
    completion: OperationCompletion,
    interaction: u64,
    server_instance: ServerInstanceId,
}

struct CommandLedger {
    server_instance: Option<ServerInstanceId>,
    next_request_id: u64,
    operations: HashMap<RequestId, PendingOperation>,
    page_acks: HashSet<RequestId>,
}

impl CommandLedger {
    fn new() -> Self {
        Self {
            server_instance: None,
            next_request_id: 1,
            operations: HashMap::new(),
            page_acks: HashSet::new(),
        }
    }

    fn request_id(&mut self) -> Result<RequestId> {
        let sequence = self.next_request_id;
        self.next_request_id = sequence
            .checked_add(1)
            .context("client request ID sequence exhausted")?;
        Ok(RequestId(sequence))
    }

    fn attach(&mut self, server_instance: ServerInstanceId) -> Result<()> {
        anyhow::ensure!(
            self.server_instance
                .as_ref()
                .is_none_or(|previous| previous == &server_instance),
            "the original zess server is no longer available"
        );
        self.server_instance = Some(server_instance);
        Ok(())
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
    interaction: u64,
    server_instance: ServerInstanceId,
    action: CommandAction,
    completion: OperationCompletion,
}

enum CommandScope {
    Workspace,
    Document(u64),
    Independent,
}

impl QueuedCommand {
    fn scope(&self) -> CommandScope {
        match &self.action {
            CommandAction::ClientExit { .. }
            | CommandAction::Command { .. }
            | CommandAction::CommandLine { .. }
            | CommandAction::Paste { .. } => CommandScope::Workspace,
            CommandAction::Request {
                request: HostRequest::Action { name, .. },
                ..
            } if name == "exit" => CommandScope::Workspace,
            CommandAction::Request {
                request: HostRequest::Action { name, .. },
                ..
            } if matches!(name.as_str(), "open" | "new" | "config-reload" | "buffers") => {
                CommandScope::Independent
            }
            CommandAction::Invoke { target, .. } | CommandAction::Request { target, .. } => {
                match target {
                    CommandTarget::View { buffer_id, .. }
                    | CommandTarget::Document { buffer_id } => CommandScope::Document(*buffer_id),
                    CommandTarget::Page { .. } => CommandScope::Independent,
                }
            }
            CommandAction::Observe {
                source: EventSource::View { buffer_id, .. },
                ..
            }
            | CommandAction::CloseBuffer { buffer_id, .. }
            | CommandAction::ClosePageDocument { buffer_id, .. }
            | CommandAction::DiscardScratch { buffer_id, .. }
            | CommandAction::Save { buffer_id, .. } => CommandScope::Document(*buffer_id),
            CommandAction::Observe {
                source: EventSource::Page { .. },
                ..
            }
            | CommandAction::AcknowledgeClientAction { .. }
            | CommandAction::ApplyPagePatch { .. }
            | CommandAction::PagePrompt { .. }
            | CommandAction::OpenDocument { .. } => CommandScope::Independent,
        }
    }
}

struct ClientUiState {
    notice: Option<String>,
    dirty: bool,
    clicks: ClickTracker,
    commands: VecDeque<QueuedCommand>,
}

impl ClientUiState {
    fn ready_command(
        &mut self,
        workbench: &workbench::Workbench,
        ledger: &CommandLedger,
    ) -> Option<QueuedCommand> {
        let mut waiting_for_workspace = false;
        for (index, command) in self.commands.iter().enumerate() {
            match command.scope() {
                CommandScope::Workspace
                    if !ledger.operations.is_empty()
                        || !workbench.documents.workspace_command_ready() =>
                {
                    waiting_for_workspace = true;
                }
                CommandScope::Document(buffer)
                    if waiting_for_workspace || !workbench.documents.command_ready(buffer) => {}
                _ => return self.commands.remove(index),
            }
        }
        None
    }
}

fn sanitize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
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

    Cow::Owned(escaped)
}

fn run_client<C: Clipboard>(
    factory: impl ConnectionFactory + 'static,
    client_id: String,
    files: Vec<String>,
    terminal: &mut impl TerminalIo,
    provider: &mut C,
) -> Result<()> {
    let (mut width, mut height) = terminal.size().unwrap_or((80, 24));
    let mut page = ConnectionPage::new(factory.display_target().to_owned());
    let mut factory: Option<Box<dyn ConnectionFactory>> = Some(Box::new(factory));
    let initial = page.frame(width, height, None);
    terminal.set_mouse_capture(false)?;
    render(terminal.writer(), &initial, width, height, None)?;
    let mut connection = ClientConnection::Disconnected {
        retry_at: Instant::now(),
        attempts: 0,
    };
    let mut frame = Frame {
        width,
        height,
        ..Frame::default()
    };
    let mut ledger = CommandLedger::new();
    let mut submitted_actions = HashSet::new();
    let mut client_workspace = zess_protocol::ClientWorkspace::default();
    let mut attached_session = None;
    let mut remote_state: Option<RemoteStateCache> = None;
    let mut pages_available = false;
    let mut has_frame = false;
    let mut resync_deadline = None;
    let mut workbench = workbench::Workbench::default();
    let mut positions = workspace_view::WorkspaceViewSync::default();
    let mut interaction = 0_u64;
    let mut detaching = false;
    let mut detach_sent = false;
    let mut effects = VecDeque::<workbench::Effect>::new();
    let mut clipboard = ClipboardActions::new();
    let mut ui = ClientUiState {
        notice: Some("Connecting to zess server...".to_owned()),
        dirty: true,
        clicks: ClickTracker::default(),
        commands: VecDeque::new(),
    };

    loop {
        if let ClientConnection::Connecting { task, attempts } = &mut connection {
            task.update(&mut page);
            if let Some(finished) = task.finish() {
                let attempts = *attempts;
                let (returned_factory, result) = finished?;
                factory = Some(returned_factory);
                if page.cancelling {
                    drop(result);
                    return Ok(());
                }
                let attempt = result.and_then(|stream| {
                    Connection::connect(
                        stream,
                        &client_id,
                        &files,
                        attached_session.as_ref(),
                        width,
                        height,
                    )
                });
                match attempt {
                    Ok(active) => {
                        page.progress = ConnectionProgress {
                            message: "Connected; attaching…".into(),
                            bytes: None,
                        };
                        ui.notice = None;
                        connection = ClientConnection::Attaching {
                            connection: active,
                            attachment: Attachment::AwaitingResponse(AttachKind::Retained),
                            deadline: Instant::now() + ATTACH_TIMEOUT,
                            attempts,
                        };
                    }
                    Err(error) => {
                        ui.notice = Some(format!("Connection failed: {error:#}"));
                        connection.retry_or_fail(error);
                    }
                }
                ui.dirty = true;
            }
        }
        if let ClientConnection::Disconnected { retry_at, attempts } = &connection
            && Instant::now() >= *retry_at
        {
            resync_deadline = None;
            page.reset();
            connection = ClientConnection::Connecting {
                task: ConnectionTask::start(
                    factory
                        .take()
                        .context("connection factory is unavailable")?,
                    attached_session.clone(),
                )?,
                attempts: attempts + 1,
            };
            ui.dirty = true;
        }
        if connection.attached() && remote_state.is_some() && resync_deadline.is_none() {
            let state = &remote_state.as_ref().unwrap().state;
            for pending in &state.client_actions {
                if ledger.operations.values().any(|operation| {
                    matches!(
                        operation.completion,
                        OperationCompletion::WorkspaceAction { .. }
                    )
                }) {
                    break;
                }
                if !submitted_actions.contains(&pending.id) {
                    if let zess_protocol::ClientActionKind::OpenPage { source } = &pending.kind {
                        workbench.pages.open(&source.kind, source.argument.clone());
                        ui.dirty = true;
                    }
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
                                if workbench.documents.has_closed_edits() {
                                    continue;
                                }
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
                    let acknowledged_change =
                        matches!(action, CommandAction::AcknowledgeClientAction { .. })
                            .then(|| pending.kind.clone());
                    submitted_actions.insert(pending.id);
                    ui.commands.push_back(QueuedCommand {
                        interaction,
                        server_instance: ledger.server_instance.clone().unwrap(),
                        action,
                        completion: OperationCompletion::WorkspaceAction {
                            id: pending.id,
                            acknowledged_change,
                        },
                    });
                    break;
                }
            }
            while let Some(command) = ui.ready_command(&workbench, &ledger) {
                if ledger.server_instance.as_ref() != Some(&command.server_instance) {
                    let message = "Session changed; the queued command was not submitted";
                    command.completion.not_submitted(&mut workbench, message);
                    ui.notice = Some(message.into());
                    ui.dirty = true;
                    continue;
                }
                let request_id = ledger.request_id()?;
                let server_instance = command.server_instance;
                let message = ClientMessage::Command {
                    request_id,
                    server_instance: server_instance.clone(),
                    action: command.action,
                };
                if framing::encode(&message)?.len() > MAX_INPUT_BYTES {
                    let reason = "Command exceeds the input byte limit";
                    command.completion.not_submitted(&mut workbench, reason);
                    ui.notice = Some(reason.into());
                    ui.dirty = true;
                    continue;
                }
                if let OperationCompletion::Reload { buffer_id } = &command.completion {
                    workbench.documents.reload_submitted(*buffer_id);
                }
                ledger.operations.insert(
                    request_id,
                    PendingOperation {
                        completion: command.completion,
                        interaction: command.interaction,
                        server_instance,
                    },
                );
                connection.active_mut().unwrap().send(&message)?;
            }
        }

        let messages = match connection.active_mut().map(Connection::poll).transpose() {
            Ok(messages) => messages.unwrap_or_default(),
            Err(error) => {
                if error.downcast_ref::<ProtocolMismatch>().is_some() {
                    connection = ClientConnection::Failed(error);
                } else {
                    ui.notice = Some(format!("Disconnected; reconnecting: {error}"));
                    connection.retry_or_fail(error);
                }
                ui.dirty = true;
                Vec::new()
            }
        };
        for message in messages {
            let primary_write = matches!(&message, ServerMessage::ClipboardWritePrimary { .. });
            match message {
                ServerMessage::Hello { .. } => anyhow::bail!("unexpected duplicate zess Hello"),
                ServerMessage::AttachPending => {
                    let Some(Attachment::AwaitingResponse(kind @ AttachKind::Retained)) =
                        connection.attachment_mut()
                    else {
                        anyhow::bail!("unexpected pending zess attachment");
                    };
                    *kind = AttachKind::Pending;
                }
                ServerMessage::Attached {
                    server_instance,
                    context_epoch: epoch,
                    workspace_view,
                    ..
                } => {
                    if ledger
                        .server_instance
                        .as_ref()
                        .is_some_and(|expected| expected != &server_instance)
                    {
                        connection = ClientConnection::Failed(anyhow::anyhow!(
                            "the original zess server is no longer available"
                        ));
                        ui.dirty = true;
                        break;
                    }
                    let metadata = AttachMetadata {
                        workspace_view,
                        server_instance,
                        epoch,
                    };
                    let attachment = connection
                        .attachment_mut()
                        .context("unexpected zess attachment")?;
                    let Attachment::AwaitingResponse(kind) = *attachment else {
                        anyhow::bail!("unexpected duplicate zess attachment");
                    };
                    *attachment = Attachment::AwaitingSnapshot(kind, metadata);
                    ui.dirty = true;
                }
                message @ (ServerMessage::Snapshot { .. } | ServerMessage::AttachConfirmed) => {
                    let (next_state, metadata) = match message {
                        ServerMessage::Snapshot {
                            session_id,
                            server_instance,
                            state_revision,
                            bindings,
                            state,
                        } => {
                            let unexpected_session = attached_session
                                .as_ref()
                                .or_else(|| {
                                    connection
                                        .active_mut()
                                        .and_then(|active| active.expected_session.as_ref())
                                })
                                .is_some_and(|expected| expected != &session_id);
                            let (expected_server, pending) = match connection.attachment_mut() {
                                Some(Attachment::AwaitingSnapshot(kind, metadata)) => (
                                    Some(&metadata.server_instance),
                                    matches!(kind, AttachKind::Pending),
                                ),
                                Some(_) => {
                                    anyhow::bail!("unexpected zess snapshot during attachment")
                                }
                                None => (ledger.server_instance.as_ref(), false),
                            };
                            let error = if unexpected_session {
                                Some("the original zess session is no longer available")
                            } else if expected_server
                                .is_some_and(|expected| expected != &server_instance)
                            {
                                Some("the original zess server is no longer available")
                            } else {
                                None
                            };
                            if let Some(error) = error {
                                connection = ClientConnection::Failed(anyhow::anyhow!(error));
                                ui.dirty = true;
                                break;
                            }
                            let next_state = RemoteStateCache {
                                session_id,
                                server_instance,
                                revision: state_revision,
                                state,
                            };
                            if let Err(error) = next_state.state.validate() {
                                ui.dirty = true;
                                if pending {
                                    ui.notice = Some(format!(
                                        "Could not synchronize pending session; reconnecting: {error}"
                                    ));
                                    connection.retry_or_fail(anyhow::anyhow!(
                                        "Could not synchronize pending session: {error}"
                                    ));
                                    break;
                                }
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
                                continue;
                            }
                            workbench.statuslines.invalidate();
                            workbench.editor.set_catalog(bindings);
                            let metadata = if let Some(attachment) = connection.attachment_mut() {
                                let Attachment::AwaitingSnapshot(_, metadata) = std::mem::replace(
                                    attachment,
                                    Attachment::AwaitingResponse(AttachKind::Retained),
                                ) else {
                                    unreachable!("attachment awaits its snapshot");
                                };
                                if pending {
                                    *attachment = Attachment::Confirming {
                                        metadata,
                                        snapshot: Box::new(next_state),
                                    };
                                    connection
                                        .active_mut()
                                        .unwrap()
                                        .send(&ClientMessage::ConfirmAttach)?;
                                    continue;
                                }
                                Some(metadata)
                            } else {
                                None
                            };
                            (next_state, metadata)
                        }
                        ServerMessage::AttachConfirmed => {
                            let attachment = connection
                                .attachment_mut()
                                .context("unexpected zess attachment confirmation")?;
                            let Attachment::Confirming { metadata, snapshot } = std::mem::replace(
                                attachment,
                                Attachment::AwaitingResponse(AttachKind::Retained),
                            ) else {
                                anyhow::bail!("unexpected zess attachment confirmation");
                            };
                            (*snapshot, Some(metadata))
                        }
                        _ => unreachable!(),
                    };
                    has_frame = true;
                    let resynchronized = resync_deadline.take().is_some();
                    if resynchronized {
                        workbench.statuslines.invalidate();
                    }
                    client_workspace = next_state.state.workspace.clone();
                    let keep_local_position = remote_state.as_ref().is_some_and(|previous| {
                        !workbench.detach_synchronized()
                            || !positions.is_saved(&workbench.workspace_view(&previous.state))
                    });
                    workbench.synchronize(&next_state.state);
                    remote_state = Some(next_state);
                    let Some(AttachMetadata {
                        server_instance,
                        epoch,
                        workspace_view,
                    }) = metadata
                    else {
                        if resynchronized && let Some(active) = connection.active_mut() {
                            for (request_id, operation) in &ledger.operations {
                                if matches!(
                                    operation.completion,
                                    OperationCompletion::OpenDocument { .. }
                                        | OperationCompletion::DiscardScratch { .. }
                                        | OperationCompletion::PageCommand
                                        | OperationCompletion::PagePrompt { .. }
                                ) {
                                    active.send(&ClientMessage::QueryOperation {
                                        request_id: *request_id,
                                        server_instance: operation.server_instance.clone(),
                                    })?;
                                }
                            }
                        }
                        ui.notice = None;
                        ui.dirty = true;
                        continue;
                    };
                    let previous_epoch = workbench.pages.attach(epoch);
                    if attached_session.is_some() {
                        workbench.reconnect();
                    }
                    let state = &remote_state.as_ref().unwrap().state;
                    if (attached_session.is_none()
                        || (previous_epoch != epoch && !keep_local_position))
                        && let Some(saved) = &workspace_view
                    {
                        workbench.restore(saved, state)?;
                        if attached_session.is_none() && !files.is_empty() {
                            workbench.focus_opened(state);
                        }
                    }
                    positions.attach(workspace_view);
                    detach_sent = false;
                    attached_session = Some(remote_state.as_ref().unwrap().session_id.clone());
                    clipboard.attach(&server_instance);
                    ledger.attach(server_instance)?;
                    connection.finish_attach();
                    let active = connection.active_mut().unwrap();
                    ui.notice = None;
                    for (request_id, operation) in &ledger.operations {
                        active.send(&ClientMessage::QueryOperation {
                            request_id: *request_id,
                            server_instance: operation.server_instance.clone(),
                        })?;
                    }
                    active.send(&ClientMessage::Resize { width, height })?;
                    workbench.resize_pages(width, height);
                    ui.dirty = true;
                }
                ServerMessage::Delta {
                    session_id,
                    server_instance,
                    base_revision,
                    state_revision,
                    delta,
                } => {
                    let mut next_state = remote_state.clone();
                    let applied = resync_deadline.is_none()
                        && next_state.as_mut().is_some_and(|state| {
                            state.apply_delta(
                                &session_id,
                                &server_instance,
                                base_revision,
                                state_revision,
                                delta,
                            ) && state.state.validate().is_ok()
                        });
                    if applied {
                        workbench.synchronize(&next_state.as_ref().unwrap().state);
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
                ServerMessage::Bindings(catalog) => {
                    workbench.statuslines.invalidate();
                    workbench.editor.set_catalog(catalog);
                    ui.dirty = true;
                }
                ServerMessage::Document(event) => {
                    let state = remote_state
                        .as_ref()
                        .context("document arrived before Session snapshot")?;
                    effects.extend(workbench.apply_document(event, &state.state)?);
                    ui.dirty = true;
                }
                ServerMessage::FormattedStatusline { id, result } => {
                    match workbench.statuslines.receive(id, result) {
                        Ok(changed) => ui.dirty |= changed,
                        Err(message) => {
                            ui.notice = Some(message);
                            ui.dirty = true;
                        }
                    }
                }
                ServerMessage::Chunk { .. } => {
                    unreachable!("the connection assembles message chunks")
                }
                ServerMessage::Resource { update } => {
                    let state = remote_state
                        .as_ref()
                        .context("resource arrived before Session snapshot")?;
                    effects.extend(workbench.apply_resource(update, &state.state)?);
                    ui.dirty = true;
                }
                ServerMessage::ResourceCacheInvalidated { entries, reason } => {
                    workbench.invalidate_resource_cache(&entries, reason);
                    ui.dirty = true;
                }
                ServerMessage::WorkspaceViewSaved => positions.acknowledge()?,
                ServerMessage::CommandResult {
                    request_id,
                    server_instance,
                    result,
                } => {
                    if ledger.server_instance.as_ref() == Some(&server_instance) {
                        match result {
                            zess_protocol::OperationResult::Completed {
                                outcome:
                                    zess_protocol::CommandOutcome::DocumentOpened { .. }
                                    | zess_protocol::CommandOutcome::ScratchDiscarded { .. }
                                    | zess_protocol::CommandOutcome::PageApplied { .. },
                                ..
                            } if resync_deadline.is_some() => {}
                            zess_protocol::OperationResult::Completed { outcome, .. } => {
                                if let Some(operation) = ledger.operations.remove(&request_id) {
                                    match operation.completion {
                                        OperationCompletion::DiscardScratch { buffer_id } => {
                                            workbench.scratch_discard_result(buffer_id);
                                        }
                                        OperationCompletion::Reload { buffer_id } => {
                                            workbench.documents.reload_result(buffer_id);
                                        }
                                        OperationCompletion::PagePrompt {
                                            subscription_id,
                                            purpose,
                                        } => {
                                            workbench.page_prompt_result(
                                                subscription_id,
                                                &purpose,
                                                &outcome,
                                            );
                                            ui.dirty = true;
                                        }
                                        OperationCompletion::WorkspaceAction {
                                            acknowledged_change: Some(change),
                                            ..
                                        } if matches!(outcome, CommandOutcome::Applied) => {
                                            client_workspace.apply(&change);
                                        }
                                        OperationCompletion::OpenDocument { view_id } => {
                                            if let (
                                                CommandOutcome::DocumentOpened { buffer_id },
                                                Some(state),
                                            ) = (&outcome, &remote_state)
                                            {
                                                effects.extend(workbench.document_opened(
                                                    view_id,
                                                    *buffer_id,
                                                    &state.state,
                                                ));
                                                ui.dirty = true;
                                            }
                                        }
                                        OperationCompletion::None
                                        | OperationCompletion::PageCommand
                                        | OperationCompletion::WorkspaceAction { .. } => {}
                                    }
                                    match outcome {
                                        CommandOutcome::BufferClosed {
                                            buffer_id,
                                            replacement,
                                        } => {
                                            client_workspace.buffers.retain(|id| *id != buffer_id);
                                            if !client_workspace.buffers.contains(&replacement) {
                                                client_workspace.buffers.push(replacement);
                                            }
                                            client_workspace.current = Some(replacement);
                                        }
                                        CommandOutcome::PageDocumentClosed { buffer_id } => {
                                            workbench.page_document_closed(buffer_id);
                                            ui.dirty = true;
                                        }
                                        CommandOutcome::PageApplied {
                                            page_id,
                                            subscription_id,
                                            navigation,
                                            document,
                                        } => {
                                            if let Some(state) = &remote_state
                                                && let Some(applied) = workbench.apply_page_result(
                                                    page_id,
                                                    subscription_id,
                                                    navigation,
                                                    document,
                                                    &state.state,
                                                )?
                                            {
                                                effects.extend(applied);
                                                ledger.page_acks.insert(request_id);
                                                ui.dirty = true;
                                            }
                                        }
                                        CommandOutcome::ScratchDiscarded {
                                            buffer_id,
                                            replacement,
                                        } => {
                                            if client_workspace.current == Some(buffer_id) {
                                                client_workspace.apply(&ClientActionKind::Open {
                                                    buffer_id: replacement,
                                                });
                                                client_workspace.current = Some(replacement);
                                            }
                                            client_workspace.buffers.retain(|id| *id != buffer_id);
                                        }
                                        zess_protocol::CommandOutcome::Failed { message }
                                        | zess_protocol::CommandOutcome::Message { message } => {
                                            ui.notice = Some(message);
                                            ui.dirty = true;
                                        }
                                        zess_protocol::CommandOutcome::Yanked {
                                            values,
                                            register,
                                        } => {
                                            effects.extend(workbench.yank(register, values)?);
                                            ui.dirty = true;
                                        }
                                        zess_protocol::CommandOutcome::ClientRequest {
                                            request,
                                        } if operation.interaction == interaction => {
                                            effects.extend(workbench.apply_request(
                                                request,
                                                &remote_state.as_ref().unwrap().state,
                                            )?);
                                            ui.dirty = true;
                                        }
                                        _ => {}
                                    }
                                }
                                // A reconnect can resend a result if its earlier ACK was lost.
                                if !ledger.page_acks.contains(&request_id)
                                    && let Some(active) = connection.active_mut()
                                {
                                    active.send(&ClientMessage::ResultAck {
                                        request_id,
                                        server_instance,
                                    })?;
                                }
                            }
                            zess_protocol::OperationResult::Unknown => {
                                if let Some(operation) = ledger.operations.remove(&request_id) {
                                    let reason = "The previous operation's outcome is unknown; check the remote state before retrying";
                                    match operation.completion {
                                        OperationCompletion::DiscardScratch { buffer_id } => {
                                            workbench.scratch_discard_result(buffer_id);
                                        }
                                        OperationCompletion::Reload { buffer_id } => {
                                            workbench.documents.reload_result(buffer_id);
                                        }
                                        OperationCompletion::WorkspaceAction { id, .. } => {
                                            submitted_actions.remove(&id);
                                        }
                                        OperationCompletion::PagePrompt {
                                            subscription_id,
                                            purpose,
                                        } => {
                                            workbench.page_prompt_result(
                                                subscription_id,
                                                &purpose,
                                                &CommandOutcome::Failed {
                                                    message: reason.into(),
                                                },
                                            );
                                        }
                                        OperationCompletion::None
                                        | OperationCompletion::PageCommand
                                        | OperationCompletion::OpenDocument { .. } => {}
                                    }
                                    ui.notice = Some(reason.into());
                                    ui.dirty = true;
                                }
                            }
                            zess_protocol::OperationResult::Pending => {}
                        }
                    }
                }
                ServerMessage::Detached { disposition } => {
                    if disposition == zess_protocol::ClientExitDisposition::Stopping {
                        connection
                            .active_mut()
                            .context("server exit has no connection")?
                            .wait_for_server_exit()?;
                    }
                    return Ok(());
                }
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
                    connection = ClientConnection::Failed(anyhow::anyhow!(
                        "zess server rejected the client: {}",
                        sanitize(&reason)
                    ));
                    ui.dirty = true;
                    break;
                }
                ServerMessage::Exited => {
                    connection
                        .active_mut()
                        .context("server exit has no connection")?
                        .wait_for_server_exit()?;
                    return Ok(());
                }
                ServerMessage::Status { .. } => anyhow::bail!("unexpected server status response"),
            }
        }
        let now = Instant::now();
        let read_idle = connection.active_mut().is_some_and(|active| {
            active
                .last_read
                .is_none_or(|last_read| now >= last_read + ATTACH_TIMEOUT)
        });
        if let ClientConnection::Attaching { deadline, .. } = connection
            && now >= deadline
            && read_idle
        {
            let error = anyhow::anyhow!("Timed out waiting for server attachment");
            ui.notice = Some(error.to_string());
            connection.retry_or_fail(error);
            ui.dirty = true;
        } else if read_idle && resync_deadline.is_some_and(|deadline| now >= deadline) {
            ui.notice = Some("Timed out synchronizing session state; reconnecting".into());
            connection.retry_or_fail(anyhow::anyhow!("Timed out synchronizing session state"));
            resync_deadline = None;
            ui.dirty = true;
        }
        if let Some(notice) = clipboard.poll(provider, terminal.writer(), &mut connection)? {
            ui.notice = Some(notice);
            ui.dirty = true;
        }

        let synchronized =
            connection.attached() && remote_state.is_some() && resync_deadline.is_none();
        if pages_available != synchronized {
            workbench.set_pages_available(synchronized);
            pages_available = synchronized;
        }
        let editing = attached_session.is_some()
            && remote_state.is_some()
            && !page.awaiting_input()
            && !matches!(connection, ClientConnection::Failed(_));
        if let Some((id, result)) = clipboard.take_local_result()
            && let Some(state) = &remote_state
        {
            effects.extend(workbench.paste_clipboard(id, result, &state.state)?);
            ui.dirty = true;
        }
        while let Some(effect) = effects.pop_front() {
            match effect {
                workbench::Effect::Command(action) => {
                    let completion = OperationCompletion::for_action(&action);
                    if synchronized {
                        ui.commands.push_back(QueuedCommand {
                            interaction,
                            server_instance: ledger
                                .server_instance
                                .clone()
                                .context("command before attach")?,
                            action,
                            completion,
                        });
                    } else {
                        let reason = "Workspace unavailable; command was not submitted";
                        completion.not_submitted(&mut workbench, reason);
                        ui.notice = Some(reason.into());
                    }
                }
                workbench::Effect::OpenDocument { .. } if !synchronized => {
                    ui.notice = Some("Workspace unavailable; command was not submitted".into());
                }
                workbench::Effect::OpenDocument { view_id, path } => {
                    ui.commands.push_back(QueuedCommand {
                        interaction,
                        server_instance: ledger
                            .server_instance
                            .clone()
                            .context("command before attach")?,
                        action: CommandAction::OpenDocument { path },
                        completion: OperationCompletion::OpenDocument { view_id },
                    });
                }
                workbench::Effect::ClipboardRead { id, primary } => {
                    clipboard.read_for_editor(id, primary)
                }
                workbench::Effect::ClipboardWrite { primary, text } => {
                    clipboard.copy(primary, text)
                }
                workbench::Effect::Notice(message) => ui.notice = Some(message),
            }
            ui.dirty = true;
        }
        if synchronized {
            let active = connection.active_mut().unwrap();
            for request in workbench.documents.requests() {
                active.send(&ClientMessage::Document(request))?;
            }
            for request in workbench.statuslines.requests() {
                active.send(&request)?;
            }
            if let Some(interest) = workbench.pages.cache_interest() {
                active.send(&interest)?;
            }
            for message in workbench.pages.take_requests() {
                active.send(&message)?;
            }
            let saved = workbench.workspace_view_ready()
                && workbench.detach_synchronized()
                && positions.synchronize(
                    workbench.workspace_view(&remote_state.as_ref().unwrap().state),
                    active,
                )?;
            if saved {
                for request_id in ledger.page_acks.drain() {
                    active.send(&ClientMessage::ResultAck {
                        request_id,
                        server_instance: ledger.server_instance.clone().unwrap(),
                    })?;
                }
            }
            if detaching
                && !detach_sent
                && saved
                && workbench.detach_synchronized()
                && ledger.operations.is_empty()
                && ui.commands.is_empty()
                && clipboard.is_idle()
            {
                active.send(&ClientMessage::Detach)?;
                detach_sent = true;
            }
        }
        ui.dirty |= workbench.poll_syntax() || workbench.statuslines.loading_due(now);
        if ui.dirty || !connection.attached() {
            if editing {
                frame = workbench.render(&remote_state.as_ref().unwrap().state, width, height)?;
            }
            terminal.set_mouse_capture(frame.mouse_enabled && editing && !detaching)?;
            if !terminal.mouse_capture() {
                ui.clicks = ClickTracker::default();
            }
            let notice = ui
                .notice
                .as_ref()
                .filter(|_| !editing || detaching || !workbench.editor.external_prompt())
                .map(|message| {
                    if editing {
                        message.clone()
                    } else {
                        format!("Esc/Ctrl-c exit | {message}")
                    }
                });
            let (width, height) = terminal.size().unwrap_or((frame.width, frame.height));
            if editing {
                render(terminal.writer(), &frame, width, height, notice.as_deref())?;
            } else {
                if let ClientConnection::Failed(error) = &connection {
                    page.failed = true;
                    page.progress = ConnectionProgress {
                        message: format!("{error:#}"),
                        bytes: None,
                    };
                } else if matches!(connection, ClientConnection::Disconnected { .. })
                    && let Some(message) = &ui.notice
                {
                    page.progress = ConnectionProgress {
                        message: message.clone(),
                        bytes: None,
                    };
                }
                let panel = page.frame(width, height, has_frame.then_some(&frame));
                render(terminal.writer(), &panel, width, height, None)?;
            }
            ui.dirty = false;
        }
        let Some(event) = terminal.poll_event(EVENT_POLL_INTERVAL)? else {
            continue;
        };
        if !editing && !connection.attached() {
            let cancel = matches!(&event, Event::Key(key)
                if key.kind != KeyEventKind::Release && (key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
                    || (key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL))));
            if cancel {
                if let ClientConnection::Connecting { task, .. } = &connection {
                    page.cancel();
                    task.cancel();
                    ui.dirty = true;
                } else {
                    return if let ClientConnection::Failed(error) = connection {
                        Err(error)
                    } else {
                        Ok(())
                    };
                }
                continue;
            }
            if page.cancelling {
                continue;
            }
            if matches!(connection, ClientConnection::Failed(_))
                && matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Char('r'))
            {
                page.reset();
                connection = ClientConnection::Disconnected {
                    retry_at: Instant::now(),
                    attempts: 0,
                };
                continue;
            }
            if page.event(&event) {
                ui.dirty = true;
                continue;
            }
        }
        let new_input = match &event {
            Event::Paste(_) => true,
            Event::Key(key) => key.kind != KeyEventKind::Release,
            _ => false,
        };
        if synchronized && new_input && ui.notice.take().is_some() {
            ui.dirty = true;
        }
        if !matches!(event, Event::Mouse(_)) {
            ui.clicks = ClickTracker::default();
        }
        if let Event::Resize(new_width, new_height) = event {
            width = new_width;
            height = new_height;
            workbench.resize_pages(width, height);
            ui.dirty = true;
            if connection.attached()
                && let Some(active) = connection.active_mut()
            {
                active.send(&ClientMessage::Resize { width, height })?;
            }
            continue;
        }
        if !editing || detach_sent {
            continue;
        }
        let state = &remote_state.as_ref().unwrap().state;
        let result = match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    let notice = if let Some(notice) = workbench.documents.closed_notice() {
                        notice
                    } else {
                        detaching = true;
                        "Synchronizing before detach…".into()
                    };
                    ui.notice = Some(notice);
                    Ok(Vec::new())
                } else if detaching && key.code == KeyCode::Esc {
                    detaching = false;
                    ui.notice = None;
                    Ok(Vec::new())
                } else {
                    let Some(key) = encode_key(key) else { continue };
                    workbench.key(&key, state, width)
                }
            }
            Event::Paste(text) => workbench.paste(&text, state),
            Event::Mouse(mouse) if terminal.mouse_capture() => {
                let Some(kind) = ui
                    .clicks
                    .at(mouse.kind, mouse.column, mouse.row, Instant::now())
                else {
                    continue;
                };
                workbench.mouse(
                    &MouseEvent {
                        kind,
                        column: mouse.column,
                        row: mouse.row,
                    },
                    state,
                    width,
                    height,
                )
            }
            _ => continue,
        };
        interaction = interaction.wrapping_add(1);
        match result {
            Ok(next) => {
                for effect in next {
                    if !synchronized && let workbench::Effect::Command(action) = &effect {
                        let reason = "Workspace unavailable; command was not submitted";
                        OperationCompletion::for_action(action)
                            .not_submitted(&mut workbench, reason);
                        ui.notice = Some(reason.into());
                    } else {
                        effects.push_back(effect);
                    }
                }
            }
            Err(error) => ui.notice = Some(format!("{error:#}")),
        }
        ui.dirty = true;
    }
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

fn render_row(
    stdout: &mut impl Write,
    row: &FrameRow,
    width: u16,
    styles: &std::collections::HashMap<String, zess_ui::TextStyle>,
) -> Result<()> {
    let text = row
        .spans
        .iter()
        .map(|span| sanitize(&span.text))
        .collect::<Vec<_>>();
    let (mut bytes, _) = fitting_prefix(text.iter().map(|text| text.as_ref()), usize::from(width));

    for (span, text) in row.spans.iter().zip(text) {
        if bytes == 0 {
            break;
        }
        let visible = text.len().min(bytes);
        if visible == 0 {
            continue;
        }

        let resolve = |role: &str| style::resolve_in(role, styles);
        let base = span.base_style.as_deref().map(resolve).unwrap_or_default();
        style::apply(stdout, &base.patch(&resolve(&span.style)))?;
        queue!(stdout, Print(&text[..visible]))?;
        bytes -= visible;
    }

    queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
    Ok(())
}

fn clip_to_width(text: &str, maximum: usize) -> (&str, usize) {
    let (bytes, columns) = fitting_prefix(std::iter::once(text), maximum);
    (&text[..bytes], columns)
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
                    queue!(stdout, Print(visible_prompt))?;
                } else {
                    let (pending, pending_width) = clip_to_width(&prompt, width as usize);
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
