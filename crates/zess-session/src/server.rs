use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::{ErrorKind, Read, Write},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use zess_ipc::{LocalIpc, SocketPath, Stream, framing};
use zess_platform::{NativePlatform, Platform};
use zess_protocol::{
    ClientMessage, CommandAction, CommandOutcome, MAX_INPUT_BYTES, MAX_MESSAGE_BYTES,
    OperationResult, RequestId, ServerInstanceId, ServerMessage, SessionDelta, SessionId,
    SessionSnapshot,
};

use crate::Session;
use zess_ui::Frame;

#[cfg(all(test, unix))]
mod tests;

struct PendingMessage {
    bytes: Vec<u8>,
    update: bool,
}

enum ConnectionRole {
    AwaitHello {
        accepted: Instant,
    },
    Unidentified {
        accepted: Instant,
    },
    Attached {
        client: String,
    },
    ConfirmingAttach {
        client: String,
    },
    Closing {
        client: Option<String>,
        deadline: Instant,
    },
}

impl ConnectionRole {
    fn client(&self) -> Option<&String> {
        match self {
            Self::Attached { client } | Self::ConfirmingAttach { client } => Some(client),
            Self::Closing { client, .. } => client.as_ref(),
            Self::Unidentified { .. } | Self::AwaitHello { .. } => None,
        }
    }
}

enum ServerState {
    Running,
    Draining { deadline: Instant },
    Stopped,
}

impl ServerState {
    fn drain(&mut self, connections: &mut Vec<Connection>) {
        let Self::Draining { deadline } = self else {
            return;
        };
        connections
            .retain_mut(|connection| connection.flush().is_ok() && !connection.output.is_empty());
        if connections.is_empty() || Instant::now() >= *deadline {
            connections.clear();
            *self = Self::Stopped;
        }
    }
}

const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const FIRST_ATTACH_TIMEOUT: Duration = Duration::from_secs(15);

struct Connection {
    stream: Stream,
    input: framing::Decoder,
    output: VecDeque<PendingMessage>,
    written: usize,
    role: ConnectionRole,
    claimed_pending_session: bool,
    eof: bool,
    exit_disposition: Option<zess_protocol::ClientExitDisposition>,
}

impl Connection {
    fn new(stream: Stream) -> Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            input: framing::Decoder::default(),
            output: VecDeque::new(),
            written: 0,
            role: ConnectionRole::AwaitHello {
                accepted: Instant::now(),
            },
            claimed_pending_session: false,
            eof: false,
            exit_disposition: None,
        })
    }

    fn disconnect(self, session: &mut Session, active: &mut Option<String>) {
        if self.role.client().is_some() {
            session.invalidate_mouse();
            *active = None;
        }
    }

    fn close_after_flush(&mut self) {
        self.role = ConnectionRole::Closing {
            client: self.role.client().cloned(),
            deadline: Instant::now() + DRAIN_TIMEOUT,
        };
    }

    fn send(&mut self, message: &ServerMessage) -> Result<()> {
        let bytes = framing::encode(message)?;
        if matches!(
            message,
            ServerMessage::ClipboardWrite { .. } | ServerMessage::ClipboardWritePrimary { .. }
        ) && bytes.len() > MAX_MESSAGE_BYTES
        {
            bail!(
                "selection exceeds the clipboard message limit ({} bytes)",
                MAX_MESSAGE_BYTES
            );
        }
        let update = matches!(message, ServerMessage::Update { .. });
        // Preserve the first message in case it has been partially written, and
        // coalesce only adjacent frames so one-time effects keep their order.
        if update && self.output.len() > 1 && self.output.back().unwrap().update {
            self.output.pop_back();
        }
        self.output.push_back(PendingMessage { bytes, update });
        Ok(())
    }

    fn send_clipboard_effects(&mut self, session: &mut Session) -> Result<()> {
        session.flush_clipboard_writes(|text| self.send(&ServerMessage::ClipboardWrite { text }));
        session.flush_primary_clipboard_writes(|text| {
            self.send(&ServerMessage::ClipboardWritePrimary { text })
        });
        if let Some((id, primary)) = session.clipboard_request() {
            self.send(&ServerMessage::ClipboardRead { id, primary })?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        while let Some(message) = self.output.front() {
            match self.stream.write(&message.bytes[self.written..]) {
                Ok(0) => bail!("client closed"),
                Ok(count) => {
                    self.written += count;
                    if self.written == message.bytes.len() {
                        self.output.pop_front();
                        self.written = 0;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Vec<ClientMessage>> {
        let mut messages = Vec::new();
        let mut bytes = [0; 8192];
        loop {
            match self.stream.read(&mut bytes) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(count) => {
                    for fragment in bytes[..count].split_inclusive(|byte| *byte == b'\n') {
                        self.input.push(fragment, MAX_INPUT_BYTES)?;
                        if fragment.last() == Some(&b'\n') {
                            messages.push(
                                self.input
                                    .decode_next()?
                                    .expect("fragment completes a message"),
                            );
                        }
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(messages)
    }
}

struct PublishedState {
    session_id: SessionId,
    server_instance: ServerInstanceId,
    revision: u64,
    semantic: SessionSnapshot,
    frame: Frame,
}

impl PublishedState {
    fn new(session: &mut Session, server_instance: ServerInstanceId) -> Self {
        let frame = session.frame();
        let semantic = session.snapshot_with_frame(&frame);
        Self {
            session_id: SessionId(format!("session-{}", server_instance.0)),
            server_instance,
            revision: 1,
            semantic,
            frame,
        }
    }

    fn snapshot(&self) -> ServerMessage {
        ServerMessage::Snapshot {
            session_id: self.session_id.clone(),
            server_instance: self.server_instance.clone(),
            state_revision: self.revision,
            state: self.semantic.clone(),
            frame: self.frame.clone(),
        }
    }

    fn refresh(&mut self, session: &mut Session) -> Option<ServerMessage> {
        let frame = session.frame();
        let semantic = session.snapshot_with_frame(&frame);
        if self.semantic == semantic && self.frame == frame {
            return None;
        }
        let base_revision = self.revision;
        self.revision += 1;
        let delta = SessionDelta::between(&self.semantic, &semantic);
        let frame_delta = (self.frame != frame).then(|| frame.clone());
        self.semantic = semantic;
        self.frame = frame;
        Some(ServerMessage::Delta {
            session_id: self.session_id.clone(),
            server_instance: self.server_instance.clone(),
            base_revision,
            state_revision: self.revision,
            delta,
            frame: frame_delta,
        })
    }

    fn publish(&mut self, session: &mut Session, connection: &mut Connection) -> Result<()> {
        if let Some(message) = self.refresh(session) {
            connection.send(&message)?;
        }
        Ok(())
    }
}

#[derive(PartialEq)]
enum OperationAction {
    Command(CommandAction),
    Input {
        sequence: u64,
        key: String,
        context_epoch: u64,
    },
}

struct Operation {
    action: OperationAction,
    completed: Option<OperationCompletion>,
}

impl Operation {
    fn result(&self) -> OperationResult {
        match &self.completed {
            Some(completion) => OperationResult::Completed {
                outcome: completion.outcome.clone(),
                state_revision: completion.state_revision,
            },
            None => OperationResult::Pending,
        }
    }
}

struct OperationCompletion {
    outcome: CommandOutcome,
    state_revision: u64,
}

#[derive(Default)]
struct ClientOperations {
    last_started: u64,
    unacknowledged: BTreeMap<RequestId, Operation>,
}

#[derive(Default)]
struct OperationTable {
    clients: HashMap<String, ClientOperations>,
}

impl OperationTable {
    fn execute(
        &mut self,
        client: &str,
        request_id: RequestId,
        action: OperationAction,
        session: &mut Session,
        published: &mut PublishedState,
    ) -> (OperationResult, Option<ServerMessage>) {
        let operations = self.clients.entry(client.to_owned()).or_default();
        if let Some(operation) = operations.unacknowledged.get(&request_id) {
            return (
                if operation.action == action {
                    operation.result()
                } else {
                    OperationResult::Completed {
                        outcome: CommandOutcome::Failed {
                            message: "request ID already belongs to another command".into(),
                        },
                        state_revision: published.revision,
                    }
                },
                None,
            );
        }
        // Acknowledged operations can be dropped because new requests arrive in
        // order. Gaps are allowed when the client discards an unsent request.
        if request_id.0 <= operations.last_started {
            return (OperationResult::Unknown, None);
        }
        operations.last_started = request_id.0;
        let token = format!("{client}:{}", request_id.0);
        let outcome = match &action {
            OperationAction::Command(action) => session.begin_operation(&token, action),
            OperationAction::Input { key, .. } => session.tracked_input(&token, key),
        };
        let delta = published.refresh(session);
        let operation = Operation {
            action,
            completed: outcome.map(|outcome| OperationCompletion {
                outcome,
                state_revision: published.revision,
            }),
        };
        let result = operation.result();
        operations.unacknowledged.insert(request_id, operation);
        (result, delta)
    }

    fn complete<'token>(
        &mut self,
        token: &'token str,
        outcome: CommandOutcome,
        revision: u64,
    ) -> Option<(&'token str, RequestId, OperationResult)> {
        let (client, request_id) = token.rsplit_once(':')?;
        let request_id = RequestId(request_id.parse().ok()?);
        let operation = self
            .clients
            .get_mut(client)?
            .unacknowledged
            .get_mut(&request_id)?;
        if operation.completed.is_some() {
            return None;
        }
        operation.completed = Some(OperationCompletion {
            outcome,
            state_revision: revision,
        });
        Some((client, request_id, operation.result()))
    }

    fn result(&self, client: &str, request_id: RequestId) -> OperationResult {
        self.clients
            .get(client)
            .and_then(|operations| operations.unacknowledged.get(&request_id))
            .map(Operation::result)
            .unwrap_or(OperationResult::Unknown)
    }

    fn acknowledge(&mut self, client: &str, request_id: RequestId) {
        if let Some(operations) = self.clients.get_mut(client)
            && operations
                .unacknowledged
                .get(&request_id)
                .is_some_and(|operation| operation.completed.is_some())
        {
            operations.unacknowledged.remove(&request_id);
        }
    }

    fn resend(
        &self,
        client: &str,
        instance: &ServerInstanceId,
        connection: &mut Connection,
    ) -> Result<()> {
        if let Some(operations) = self.clients.get(client) {
            for (&request_id, operation) in &operations.unacknowledged {
                connection.send(&ServerMessage::CommandResult {
                    request_id,
                    server_instance: instance.clone(),
                    result: operation.result(),
                })?;
            }
        }
        Ok(())
    }
}

pub enum SessionLifetime {
    Retained,
    Pending,
}

struct PendingSession {
    deadline: Instant,
    claimed: bool,
}

/// Serve a single active terminal while retaining all session state on detach.
pub fn serve(ipc: &dyn LocalIpc, socket: &SocketPath, session: Session) -> Result<()> {
    serve_with_platform(
        ipc,
        socket,
        session,
        &NativePlatform,
        SessionLifetime::Retained,
    )
}

/// Serves with caller-supplied IPC, host identity facts, and initial session lifetime.
pub fn serve_with_platform(
    ipc: &dyn LocalIpc,
    socket: &SocketPath,
    mut session: Session,
    platform: &dyn Platform,
    lifetime: SessionLifetime,
) -> Result<()> {
    let listener = ipc.bind(socket)?;
    listener.set_nonblocking(true)?;
    let mut pending = match lifetime {
        SessionLifetime::Retained => None,
        SessionLifetime::Pending => Some(PendingSession {
            deadline: Instant::now() + FIRST_ATTACH_TIMEOUT,
            claimed: false,
        }),
    };
    let mut connections = Vec::<Connection>::new();
    let mut active = None::<String>;
    let mut acknowledged = HashMap::<String, u64>::new();
    let mut previous_client = None::<String>;
    let mut state = ServerState::Running;
    let process_id = platform.process_id();
    let server_instance = ServerInstanceId(format!(
        "{}-{}",
        process_id,
        platform
            .system_time()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut published = PublishedState::new(&mut session, server_instance.clone());
    let mut operations = OperationTable::default();
    let mut context_epoch = platform
        .system_time()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos() as u64;

    loop {
        if matches!(state, ServerState::Draining { .. }) {
            state.drain(&mut connections);
            thread::sleep(Duration::from_millis(8));
            continue;
        }
        if matches!(state, ServerState::Stopped) {
            break;
        }
        if pending
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.deadline)
        {
            break;
        }
        loop {
            match listener.accept() {
                Ok(stream) => connections.push(Connection::new(stream)?),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let mut index = 0;
        while index < connections.len() {
            let connection = &mut connections[index];
            let messages = if matches!(
                connection.role,
                ConnectionRole::Closing { .. } | ConnectionRole::ConfirmingAttach { .. }
            ) {
                Ok(Vec::new())
            } else {
                connection.receive()
            };
            let mut disconnected = messages.is_err();
            if let Ok(messages) = messages {
                for message in messages {
                    if matches!(connection.role, ConnectionRole::AwaitHello { .. })
                        && !matches!(
                            message,
                            ClientMessage::Hello { .. }
                                | ClientMessage::Status
                                | ClientMessage::ClaimPendingSession { .. }
                        )
                    {
                        connection.send(&ServerMessage::Rejected {
                            reason:
                                "Hello with a matching build is required before session requests"
                                    .into(),
                        })?;
                        connection.close_after_flush();
                        break;
                    }
                    if pending.is_some() {
                        let allowed = match &message {
                            ClientMessage::Hello { .. }
                            | ClientMessage::Status
                            | ClientMessage::ClaimPendingSession { .. }
                            | ClientMessage::Detach => true,
                            ClientMessage::Attach { .. }
                            | ClientMessage::Resync { .. }
                            | ClientMessage::ConfirmAttach => connection.claimed_pending_session,
                            _ => false,
                        };
                        if !allowed {
                            connection.send(&ServerMessage::Rejected {
                                reason: "session is awaiting its first terminal attachment".into(),
                            })?;
                            connection.close_after_flush();
                            break;
                        }
                    }
                    match message {
                        ClientMessage::Hello { build_id }
                            if matches!(connection.role, ConnectionRole::AwaitHello { .. }) =>
                        {
                            connection.send(&ServerMessage::Hello {
                                build_id: zess_build::BUILD_ID.to_owned(),
                            })?;
                            if zess_protocol::check_build_id(&build_id).is_err() {
                                connection.close_after_flush();
                                break;
                            }
                            connection.role = ConnectionRole::Unidentified {
                                accepted: Instant::now(),
                            };
                        }
                        message @ (ClientMessage::Status
                        | ClientMessage::ClaimPendingSession { .. })
                            if matches!(
                                connection.role,
                                ConnectionRole::Unidentified { .. }
                                    | ConnectionRole::AwaitHello { .. }
                            ) =>
                        {
                            let claim = if let ClientMessage::ClaimPendingSession { pid } = message
                            {
                                if let Some(pending) = pending.as_mut()
                                    && !pending.claimed
                                    && pid == process_id
                                    && matches!(connection.role, ConnectionRole::AwaitHello { .. })
                                {
                                    pending.claimed = true;
                                    connection.claimed_pending_session = true;
                                    true
                                } else {
                                    connection.send(&ServerMessage::Rejected {
                                        reason: "session is unavailable for first attachment"
                                            .into(),
                                    })?;
                                    connection.close_after_flush();
                                    break;
                                }
                            } else {
                                false
                            };
                            connection.send(&ServerMessage::Status {
                                build_id: zess_build::BUILD_ID.to_owned(),
                                session_id: published.session_id.clone(),
                                name: session.name().map(str::to_owned),
                                server_instance: server_instance.clone(),
                                state_revision: published.revision,
                                pid: process_id,
                                workspace: session.workspace().to_string_lossy().into_owned(),
                                attached: active.is_some(),
                                modified: session.modified(),
                                pending: pending.is_some(),
                            })?;
                            if !claim {
                                connection.close_after_flush();
                                break;
                            }
                        }
                        ClientMessage::Stop { force }
                            if matches!(connection.role, ConnectionRole::Unidentified { .. }) =>
                        {
                            if !force && session.modified() {
                                connection.send(&ServerMessage::Rejected {
                                    reason: "unsaved buffers; save them first or use --force to discard them".into(),
                                })?;
                                connection.close_after_flush();
                            } else {
                                state = ServerState::Draining {
                                    deadline: Instant::now() + DRAIN_TIMEOUT,
                                };
                            }
                            break;
                        }
                        ClientMessage::Attach {
                            expected_session,
                            client_id,
                            width,
                            height,
                            files,
                            context_epoch: previous_epoch,
                        } if matches!(connection.role, ConnectionRole::Unidentified { .. }) => {
                            if expected_session
                                .as_ref()
                                .is_some_and(|expected| expected != &published.session_id)
                            {
                                connection.send(&ServerMessage::Rejected { reason: "session identity changed; the original session is no longer available".into() })?;
                                connection.close_after_flush();
                            } else if active.is_some() {
                                connection.send(&ServerMessage::Rejected {
                                    reason:
                                        "another terminal is attached; detach it with Ctrl-z first"
                                            .into(),
                                })?;
                                connection.close_after_flush();
                            } else if client_id.is_empty() || client_id.len() > 128 {
                                disconnected = true;
                            } else {
                                if previous_client
                                    .as_ref()
                                    .is_some_and(|previous| previous != &client_id)
                                {
                                    session.cancel_clipboard_operations(
                                        "clipboard operation cancelled: another client attached",
                                    );
                                    published.refresh(&mut session);
                                    for (token, outcome) in session.take_operation_completions() {
                                        operations.complete(&token, outcome, published.revision);
                                    }
                                    context_epoch += 1;
                                }
                                previous_client = Some(client_id.clone());
                                active = Some(client_id.clone());
                                connection.role = ConnectionRole::Attached {
                                    client: client_id.clone(),
                                };
                                session.resize(width, height);
                                if !acknowledged.contains_key(&client_id) {
                                    session.open_paths(&files);
                                }
                                if pending.is_some() {
                                    connection.send(&ServerMessage::AttachPending)?;
                                }
                                let client_id_for_resend = client_id.clone();
                                connection.send(&ServerMessage::Attached {
                                    server_instance: server_instance.clone(),
                                    acknowledged: *acknowledged.entry(client_id).or_default(),
                                    context_epoch,
                                    replay_allowed: previous_epoch
                                        .is_none_or(|epoch| epoch == context_epoch),
                                    frame: session.frame(),
                                })?;
                                published.refresh(&mut session);
                                connection.send(&published.snapshot())?;
                                operations.resend(
                                    &client_id_for_resend,
                                    &server_instance,
                                    connection,
                                )?;
                                if pending.is_none()
                                    && let Some((id, primary)) = session.clipboard_request()
                                {
                                    connection
                                        .send(&ServerMessage::ClipboardRead { id, primary })?;
                                }
                            }
                        }
                        ClientMessage::ConfirmAttach
                            if connection.claimed_pending_session
                                && matches!(connection.role, ConnectionRole::Attached { .. })
                                && !connection.eof
                                && pending
                                    .as_ref()
                                    .is_some_and(|pending| Instant::now() < pending.deadline) =>
                        {
                            connection.send(&ServerMessage::AttachConfirmed)?;
                            connection.role = ConnectionRole::ConfirmingAttach {
                                client: connection.role.client().unwrap().clone(),
                            };
                            break;
                        }
                        ClientMessage::Input {
                            sequence,
                            key,
                            context_epoch: input_epoch,
                        } if matches!(connection.role, ConnectionRole::Attached { .. }) => {
                            if input_epoch != context_epoch {
                                connection.send(&ServerMessage::Rejected {
                                    reason: "input context changed; reconnect before replaying queued input".into(),
                                })?;
                                connection.close_after_flush();
                                break;
                            }
                            let client = connection.role.client().unwrap();
                            let watermark = acknowledged.get_mut(client).unwrap();
                            if sequence == *watermark + 1 {
                                session.input(&key);
                                *watermark = sequence;
                            } else if sequence > *watermark + 1 {
                                connection.send(&ServerMessage::Rejected {
                                    reason: "input sequence gap; reconnect to retry unacknowledged input".into(),
                                })?;
                                connection.close_after_flush();
                                break;
                            }
                            connection.send_clipboard_effects(&mut session)?;
                            connection.send(&ServerMessage::Ack {
                                acknowledged: *watermark,
                                context_epoch,
                            })?;
                            published.publish(&mut session, connection)?;
                        }
                        ClientMessage::TrackedInput {
                            request_id,
                            server_instance: expected,
                            sequence,
                            key,
                            context_epoch: input_epoch,
                        } if matches!(connection.role, ConnectionRole::Attached { .. }) => {
                            if expected != server_instance {
                                connection.send(&ServerMessage::CommandResult {
                                    request_id,
                                    server_instance: server_instance.clone(),
                                    result: OperationResult::Unknown,
                                })?;
                                continue;
                            }
                            if input_epoch != context_epoch {
                                connection.send(&ServerMessage::Rejected { reason: "input context changed; reconnect before replaying queued input".into() })?;
                                connection.close_after_flush();
                                break;
                            }
                            let client = connection.role.client().unwrap().clone();
                            let watermark = acknowledged.get_mut(&client).unwrap();
                            let result = if sequence == *watermark + 1 {
                                let (result, delta) = operations.execute(
                                    &client,
                                    request_id,
                                    OperationAction::Input {
                                        sequence,
                                        key,
                                        context_epoch: input_epoch,
                                    },
                                    &mut session,
                                    &mut published,
                                );
                                *watermark = sequence;
                                if let Some(delta) = delta {
                                    connection.send(&delta)?;
                                }
                                result
                            } else if sequence <= *watermark {
                                operations.result(&client, request_id)
                            } else {
                                connection.send(&ServerMessage::Rejected { reason: "input sequence gap; reconnect to retry unacknowledged input".into() })?;
                                connection.close_after_flush();
                                break;
                            };
                            connection.send_clipboard_effects(&mut session)?;
                            published.publish(&mut session, connection)?;
                            connection.send(&ServerMessage::Ack {
                                acknowledged: *watermark,
                                context_epoch,
                            })?;
                            connection.send(&ServerMessage::CommandResult {
                                request_id,
                                server_instance: server_instance.clone(),
                                result,
                            })?;
                        }
                        ClientMessage::Mouse {
                            sequence,
                            context_epoch: input_epoch,
                            mouse_context,
                            event,
                        } if matches!(connection.role, ConnectionRole::Attached { .. }) => {
                            if input_epoch != context_epoch {
                                connection.send(&ServerMessage::Rejected { reason: "input context changed; reconnect before sending mouse input".into() })?;
                                connection.close_after_flush();
                                break;
                            }
                            let watermark = acknowledged
                                .get_mut(connection.role.client().unwrap())
                                .unwrap();
                            if sequence == *watermark + 1 {
                                session.mouse(mouse_context, event);
                                *watermark = sequence;
                            } else if sequence > *watermark + 1 {
                                connection.send(&ServerMessage::Rejected { reason: "input sequence gap; reconnect to retry unacknowledged input".into() })?;
                                connection.close_after_flush();
                                break;
                            }
                            connection.send_clipboard_effects(&mut session)?;
                            connection.send(&ServerMessage::Ack {
                                acknowledged: *watermark,
                                context_epoch,
                            })?;
                            published.publish(&mut session, connection)?;
                        }
                        ClientMessage::Skip {
                            through,
                            context_epoch: input_epoch,
                        } if matches!(connection.role, ConnectionRole::Attached { .. })
                            && input_epoch == context_epoch =>
                        {
                            let watermark = acknowledged
                                .get_mut(connection.role.client().unwrap())
                                .unwrap();
                            *watermark = (*watermark).max(through);
                            connection.send(&ServerMessage::Ack {
                                acknowledged: *watermark,
                                context_epoch,
                            })?;
                            published.publish(&mut session, connection)?;
                        }
                        ClientMessage::Resize { width, height }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            session.resize(width, height);
                            let watermark = acknowledged[connection.role.client().unwrap()];
                            connection.send(&ServerMessage::Ack {
                                acknowledged: watermark,
                                context_epoch,
                            })?;
                            published.publish(&mut session, connection)?;
                        }
                        ClientMessage::ClipboardResponse { id, text, error }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            session.complete_clipboard_read(id, text, error);
                            let watermark = acknowledged[connection.role.client().unwrap()];
                            connection.send_clipboard_effects(&mut session)?;
                            connection.send(&ServerMessage::Ack {
                                acknowledged: watermark,
                                context_epoch,
                            })?;
                            published.publish(&mut session, connection)?;
                        }
                        ClientMessage::Resync { session_id, .. }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            if session_id != published.session_id {
                                connection.send(&ServerMessage::Rejected {
                                    reason: "session identity changed".into(),
                                })?;
                                if pending.is_some() {
                                    connection.close_after_flush();
                                    break;
                                }
                            } else {
                                published.refresh(&mut session);
                                connection.send(&published.snapshot())?;
                            }
                        }
                        ClientMessage::Command {
                            request_id,
                            server_instance: expected,
                            action,
                        } if matches!(connection.role, ConnectionRole::Attached { .. }) => {
                            let client_exit =
                                matches!(&action, zess_protocol::CommandAction::ClientExit { .. });
                            let result = if expected != server_instance {
                                OperationResult::Unknown
                            } else {
                                let client = connection.role.client().unwrap().clone();
                                let (result, delta) = operations.execute(
                                    &client,
                                    request_id,
                                    OperationAction::Command(action),
                                    &mut session,
                                    &mut published,
                                );
                                if let Some(delta) = delta {
                                    connection.send(&delta)?;
                                }
                                result
                            };
                            connection.send_clipboard_effects(&mut session)?;
                            published.publish(&mut session, connection)?;
                            let exited = client_exit
                                && matches!(
                                    &result,
                                    OperationResult::Completed {
                                        outcome: zess_protocol::CommandOutcome::Applied,
                                        ..
                                    }
                                );
                            connection.send(&ServerMessage::CommandResult {
                                request_id,
                                server_instance: server_instance.clone(),
                                result,
                            })?;
                            if exited {
                                let disposition = if active.as_ref() == connection.role.client()
                                    && !session.modified()
                                    && session.on_client_exit()
                                        == zess_plugin::OnClientExit::StopIfUnused
                                {
                                    zess_protocol::ClientExitDisposition::Stopping
                                } else {
                                    zess_protocol::ClientExitDisposition::Retained
                                };
                                connection.send(&ServerMessage::Detached { disposition })?;
                                connection.close_after_flush();
                                connection.exit_disposition = Some(disposition);
                            }
                        }
                        ClientMessage::QueryOperation {
                            request_id,
                            server_instance: expected,
                        } if matches!(connection.role, ConnectionRole::Attached { .. }) => {
                            let result = if expected == server_instance {
                                operations.result(connection.role.client().unwrap(), request_id)
                            } else {
                                OperationResult::Unknown
                            };
                            connection.send(&ServerMessage::CommandResult {
                                request_id,
                                server_instance: server_instance.clone(),
                                result,
                            })?;
                        }
                        ClientMessage::ResultAck {
                            request_id,
                            server_instance: expected,
                        } if matches!(connection.role, ConnectionRole::Attached { .. }) => {
                            if expected == server_instance {
                                operations
                                    .acknowledge(connection.role.client().unwrap(), request_id);
                            }
                        }
                        ClientMessage::Detach => {
                            disconnected = true;
                            break;
                        }
                        _ => {
                            disconnected = true;
                            break;
                        }
                    }
                    if pending.is_none()
                        && matches!(connection.role, ConnectionRole::Attached { .. })
                    {
                        let copied = session.flush_clipboard_writes(|text| {
                            connection.send(&ServerMessage::ClipboardWrite { text })
                        });
                        let primary_copied = session.flush_primary_clipboard_writes(|text| {
                            connection.send(&ServerMessage::ClipboardWritePrimary { text })
                        });
                        if copied || primary_copied {
                            published.publish(&mut session, connection)?;
                        }
                    }
                    if session.exiting() || matches!(state, ServerState::Draining { .. }) {
                        break;
                    }
                }
            }
            let flush_failed = connection.flush().is_err();
            if flush_failed {
                disconnected = true;
            }
            if connection.eof {
                disconnected = true;
            }
            if !disconnected
                && connection.output.is_empty()
                && let ConnectionRole::ConfirmingAttach { client } = &connection.role
            {
                pending = None;
                connection.claimed_pending_session = false;
                connection.role = ConnectionRole::Attached {
                    client: client.clone(),
                };
                if let Some((id, primary)) = session.clipboard_request() {
                    connection.send(&ServerMessage::ClipboardRead { id, primary })?;
                }
            }
            if matches!(connection.role, ConnectionRole::Closing { .. })
                && connection.output.is_empty()
            {
                disconnected = true;
            }
            match connection.role {
                ConnectionRole::Unidentified { accepted }
                | ConnectionRole::AwaitHello { accepted }
                    if accepted.elapsed() > Duration::from_secs(5) =>
                {
                    disconnected = true
                }
                ConnectionRole::Closing { deadline, .. } if Instant::now() >= deadline => {
                    disconnected = true
                }
                _ => {}
            }
            if disconnected {
                if pending.is_some() && connection.claimed_pending_session {
                    return Ok(());
                }
                let stopping = connection.exit_disposition
                    == Some(zess_protocol::ClientExitDisposition::Stopping);
                connections
                    .remove(index)
                    .disconnect(&mut session, &mut active);
                if stopping {
                    state = ServerState::Draining {
                        deadline: Instant::now() + DRAIN_TIMEOUT,
                    };
                }
            } else {
                index += 1;
            }
            if matches!(state, ServerState::Draining { .. }) {
                break;
            }
        }
        if session.exiting() || matches!(state, ServerState::Draining { .. }) {
            for connection in &mut connections {
                if matches!(
                    connection.role,
                    ConnectionRole::Attached { .. } | ConnectionRole::Unidentified { .. }
                ) {
                    let _ = connection.send(&ServerMessage::Exited);
                }
                connection.close_after_flush();
            }
            state = ServerState::Draining {
                deadline: Instant::now() + DRAIN_TIMEOUT,
            };
            continue;
        }
        if pending.is_some() {
            thread::sleep(Duration::from_millis(8));
            continue;
        }
        let changed = session.tick();
        let completed = session.take_operation_completions();
        let has_attached = connections
            .iter()
            .any(|connection| matches!(connection.role, ConnectionRole::Attached { .. }));
        let copied = has_attached
            && session.flush_clipboard_writes(|text| {
                if let Some(connection) = connections
                    .iter_mut()
                    .find(|connection| matches!(connection.role, ConnectionRole::Attached { .. }))
                {
                    connection.send(&ServerMessage::ClipboardWrite { text })?;
                }
                Ok(())
            });
        let primary_copied = has_attached
            && session.flush_primary_clipboard_writes(|text| {
                if let Some(connection) = connections
                    .iter_mut()
                    .find(|connection| matches!(connection.role, ConnectionRole::Attached { .. }))
                {
                    connection.send(&ServerMessage::ClipboardWritePrimary { text })?;
                }
                Ok(())
            });
        if changed || copied || primary_copied || !completed.is_empty() {
            let delta = published.refresh(&mut session);
            let mut index = 0;
            while index < connections.len() {
                let connection = &mut connections[index];
                if let ConnectionRole::Attached { client } = &connection.role {
                    let watermark = acknowledged[client];
                    connection.send(&ServerMessage::Ack {
                        acknowledged: watermark,
                        context_epoch,
                    })?;
                    if let Some(delta) = &delta {
                        connection.send(delta)?;
                    }
                }
                if connection.flush().is_err() {
                    connections
                        .remove(index)
                        .disconnect(&mut session, &mut active);
                } else {
                    index += 1;
                }
            }
        }
        for (token, outcome) in completed {
            if let Some((client, request_id, result)) =
                operations.complete(&token, outcome, published.revision)
            {
                if let Some(connection) = connections.iter_mut().find(|connection| {
                    connection
                        .role
                        .client()
                        .is_some_and(|owner| owner == client)
                }) {
                    connection.send(&ServerMessage::CommandResult {
                        request_id,
                        server_instance: server_instance.clone(),
                        result,
                    })?;
                }
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}
