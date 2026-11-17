use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
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
    SessionSnapshot, resource::ResourceCacheInvalidation,
};

use crate::Session;

mod documents;
#[cfg(all(test, unix))]
mod tests;

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
    message_stream: zess_protocol::chunks::MessageStream,
    output: VecDeque<Vec<u8>>,
    written: usize,
    role: ConnectionRole,
    claimed_pending_session: bool,
    subscribed_documents: HashMap<u64, documents::DocumentState>,
    document_stream: zess_protocol::document::DocumentStream,
    eof: bool,
    exit_disposition: Option<zess_protocol::ClientExitDisposition>,
}

impl Connection {
    fn new(stream: Stream) -> Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            input: framing::Decoder::default(),
            message_stream: Default::default(),
            output: VecDeque::new(),
            written: 0,
            role: ConnectionRole::AwaitHello {
                accepted: Instant::now(),
            },
            claimed_pending_session: false,
            subscribed_documents: HashMap::new(),
            document_stream: Default::default(),
            eof: false,
            exit_disposition: None,
        })
    }

    fn disconnect(
        self,
        session: &mut Session,
        active: &mut Option<String>,
        workspace_view: Option<&zess_protocol::workspace_view::WorkspaceView>,
        awaiting_delivery: impl Fn(u64) -> bool,
    ) {
        if self.role.client().is_some() {
            session.detach_resources(workspace_view, |id| {
                self.owns_page_document(id, workspace_view) || awaiting_delivery(id)
            });
            *active = None;
        }
    }

    fn owns_page_document(
        &self,
        id: u64,
        workspace_view: Option<&zess_protocol::workspace_view::WorkspaceView>,
    ) -> bool {
        self.subscribed_documents.contains_key(&id)
            || workspace_view
                .and_then(|view| view.page_document)
                .is_some_and(|layer| layer.buffer_id == id)
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
        if bytes.len() > MAX_MESSAGE_BYTES && message.can_chunk() {
            let json = std::str::from_utf8(&bytes)?;
            for (part, last) in zess_protocol::chunks::json_parts(json, MAX_MESSAGE_BYTES) {
                self.output
                    .push_back(framing::encode(&ServerMessage::Chunk {
                        json: part.to_owned(),
                        last,
                    })?);
            }
        } else {
            self.output.push_back(bytes);
        }
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

    fn flush(&mut self) -> Result<bool> {
        let mut advanced = false;
        while let Some(bytes) = self.output.front() {
            match self.stream.write(&bytes[self.written..]) {
                Ok(0) => bail!("client closed"),
                Ok(count) => {
                    advanced = true;
                    self.written += count;
                    if self.written == bytes.len() {
                        self.output.pop_front();
                        self.written = 0;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(advanced)
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
                            let message = self
                                .input
                                .decode_next()?
                                .expect("fragment completes a message");
                            if let ClientMessage::Document(request) = message {
                                if let Some(request) = self.document_stream.request(request)? {
                                    anyhow::ensure!(
                                        !matches!(
                                            request,
                                            zess_protocol::document::DocumentRequest::Chunk { .. }
                                        ),
                                        "nested document message"
                                    );
                                    messages.push(ClientMessage::Document(request));
                                }
                            } else {
                                messages.push(message);
                            }
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

fn publish_resources(session: &mut Session, connection: Option<&mut Connection>) -> Result<()> {
    let updates = session.take_resource_updates();
    let invalidated = session.take_resource_invalidations();
    if let Some(connection) = connection {
        for update in updates {
            connection.send(&ServerMessage::Resource { update })?;
        }
        let mut changed = Vec::new();
        let mut evicted = Vec::new();
        for (reason, entry) in invalidated {
            match reason {
                ResourceCacheInvalidation::Changed => changed.push(entry),
                ResourceCacheInvalidation::Evicted => evicted.push(entry),
            }
        }
        for (reason, entries) in [
            (ResourceCacheInvalidation::Changed, changed),
            (ResourceCacheInvalidation::Evicted, evicted),
        ] {
            if !entries.is_empty() {
                connection.send(&ServerMessage::ResourceCacheInvalidated { entries, reason })?;
            }
        }
    }
    Ok(())
}

struct PublishedState {
    session_id: SessionId,
    server_instance: ServerInstanceId,
    revision: u64,
    semantic: SessionSnapshot,
}

impl PublishedState {
    fn new(session: &mut Session, server_instance: ServerInstanceId) -> Self {
        Self {
            session_id: SessionId(format!("session-{}", server_instance.0)),
            server_instance,
            revision: 1,
            semantic: session.snapshot(),
        }
    }

    fn snapshot(&self, session: &Session) -> ServerMessage {
        ServerMessage::Snapshot {
            session_id: self.session_id.clone(),
            server_instance: self.server_instance.clone(),
            state_revision: self.revision,
            state: self.semantic.clone(),
            bindings: session.binding_catalog(),
        }
    }

    fn refresh(&mut self, session: &mut Session) -> Option<ServerMessage> {
        let semantic = session.snapshot();
        if self.semantic == semantic {
            return None;
        }
        let base_revision = self.revision;
        self.revision += 1;
        let delta = SessionDelta::between(&self.semantic, &semantic);
        self.semantic = semantic;
        Some(ServerMessage::Delta {
            session_id: self.session_id.clone(),
            server_instance: self.server_instance.clone(),
            base_revision,
            state_revision: self.revision,
            delta,
        })
    }

    fn publish(&mut self, session: &mut Session, connection: &mut Connection) -> Result<()> {
        if let Some(message) = self.refresh(session) {
            connection.send(&message)?;
        }
        Ok(())
    }
}

struct Operation {
    action: CommandAction,
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

    fn fullscreen_document(&self) -> Option<u64> {
        let completion = self.completed.as_ref()?;
        match &completion.outcome {
            CommandOutcome::PageApplied {
                document: Some(document),
                ..
            } if document.fullscreen => Some(document.buffer_id),
            _ => None,
        }
    }
}

struct OperationCompletion {
    outcome: CommandOutcome,
    state_revision: u64,
    sequence: u64,
}

#[derive(Default)]
struct ClientOperations {
    last_started: u64,
    last_completed: u64,
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
        action: CommandAction,
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
        let outcome = session.begin_operation(&token, &action);
        let delta = published.refresh(session);
        let operation = Operation {
            action,
            completed: outcome.map(|outcome| {
                operations.last_completed += 1;
                OperationCompletion {
                    outcome,
                    state_revision: published.revision,
                    sequence: operations.last_completed,
                }
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
        let operations = self.clients.get_mut(client)?;
        let operation = operations.unacknowledged.get_mut(&request_id)?;
        if operation.completed.is_some() {
            return None;
        }
        operations.last_completed += 1;
        operation.completed = Some(OperationCompletion {
            outcome,
            state_revision: revision,
            sequence: operations.last_completed,
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

    fn acknowledge(&mut self, client: &str, request_id: RequestId) -> Option<u64> {
        let operations = self.clients.get_mut(client)?;
        operations
            .unacknowledged
            .get(&request_id)?
            .completed
            .as_ref()?;
        operations
            .unacknowledged
            .remove(&request_id)?
            .fullscreen_document()
    }

    fn retains_page_document(&self, buffer_id: u64) -> bool {
        self.clients.values().any(|operations| {
            operations
                .unacknowledged
                .values()
                .any(|operation| operation.fullscreen_document() == Some(buffer_id))
        })
    }

    fn resend(
        &self,
        client: &str,
        instance: &ServerInstanceId,
        connection: &mut Connection,
    ) -> Result<()> {
        if let Some(operations) = self.clients.get(client) {
            let mut replay = operations.unacknowledged.iter().collect::<Vec<_>>();
            replay.sort_by_key(|(request_id, operation)| {
                (
                    operation
                        .completed
                        .as_ref()
                        .map_or(u64::MAX, |completion| completion.sequence),
                    **request_id,
                )
            });
            for (&request_id, operation) in replay {
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
    let mut clients_with_open_paths = HashSet::new();
    let mut previous_client = None::<String>;
    let mut workspace_view = None;
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
    let mut binding_revision = session.binding_revision();
    let mut operations = OperationTable::default();
    let mut document_operations = documents::DocumentOperations::default();
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
                    let message = match message {
                        ClientMessage::Chunk { json, last }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            match connection.message_stream.client(json, last) {
                                Ok(Some(message)) => message,
                                Ok(None) => continue,
                                Err(_) => {
                                    disconnected = true;
                                    break;
                                }
                            }
                        }
                        message => message,
                    };
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
                                if !files.is_empty()
                                    && clients_with_open_paths.insert(client_id.clone())
                                {
                                    session.open_paths(&files);
                                }
                                if pending.is_some() {
                                    connection.send(&ServerMessage::AttachPending)?;
                                }
                                connection.send(&ServerMessage::Attached {
                                    server_instance: server_instance.clone(),
                                    context_epoch,
                                    workspace_view: workspace_view.clone(),
                                })?;
                                published.refresh(&mut session);
                                connection.send(&published.snapshot(&session))?;
                                operations.resend(&client_id, &server_instance, connection)?;
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
                        ClientMessage::Document(request)
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            document_operations.request(&mut session, connection, request)?;
                            published.publish(&mut session, connection)?;
                        }
                        ClientMessage::FormatStatusline { id, name, context }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            let result = session
                                .format_client_statusline(&name, &context)
                                .map_err(|error| format!("{error:#}"));
                            connection.send(&ServerMessage::FormattedStatusline { id, result })?;
                        }
                        ClientMessage::SaveWorkspaceView { view }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            workspace_view = Some(view);
                            connection.send(&ServerMessage::WorkspaceViewSaved)?;
                        }
                        ClientMessage::Resize { width, height }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            session.resize(width, height);
                            published.publish(&mut session, connection)?;
                        }
                        ClientMessage::ClipboardResponse { id, text, error }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            session.complete_clipboard_read(id, text, error);
                            connection.send_clipboard_effects(&mut session)?;
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
                                connection.send(&published.snapshot(&session))?;
                                let client = connection.role.client().unwrap().clone();
                                operations.resend(&client, &server_instance, connection)?;
                            }
                        }
                        ClientMessage::Resource { request }
                            if matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            session.handle_resource(request);
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
                                    action,
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
                            if matches!(
                                &result,
                                OperationResult::Completed {
                                    outcome: CommandOutcome::PageApplied { .. },
                                    ..
                                }
                            ) {
                                publish_resources(&mut session, Some(connection))?;
                            }
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
                            if expected == server_instance
                                && let Some(id) = operations
                                    .acknowledge(connection.role.client().unwrap(), request_id)
                                && !connection.owns_page_document(id, workspace_view.as_ref())
                                && !operations.retains_page_document(id)
                            {
                                let _ = session.close_page_document(id, false);
                                published.publish(&mut session, connection)?;
                            }
                        }
                        ClientMessage::Detach
                            if pending.is_none()
                                && matches!(connection.role, ConnectionRole::Attached { .. }) =>
                        {
                            connection.send(&ServerMessage::Detached {
                                disposition: zess_protocol::ClientExitDisposition::Retained,
                            })?;
                            connection.close_after_flush();
                            break;
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
                        connection.publish_documents(&mut session)?;
                    }
                    if session.exiting() || matches!(state, ServerState::Draining { .. }) {
                        break;
                    }
                }
            }
            match connection.flush() {
                Ok(advanced) => {
                    if advanced
                        && connection.claimed_pending_session
                        && matches!(connection.role, ConnectionRole::Attached { .. })
                        && let Some(pending) = &mut pending
                    {
                        pending.deadline = Instant::now() + FIRST_ATTACH_TIMEOUT;
                    }
                }
                Err(_) => disconnected = true,
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
                connections.remove(index).disconnect(
                    &mut session,
                    &mut active,
                    workspace_view.as_ref(),
                    |id| operations.retains_page_document(id),
                );
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
        let revision = session.binding_revision();
        if revision != binding_revision {
            binding_revision = revision;
            if let Some(connection) = connections
                .iter_mut()
                .find(|connection| matches!(connection.role, ConnectionRole::Attached { .. }))
            {
                connection.send(&ServerMessage::Bindings(session.binding_catalog()))?;
            }
        }
        let attached = connections
            .iter_mut()
            .find(|connection| matches!(connection.role, ConnectionRole::Attached { .. }));
        publish_resources(&mut session, attached)?;
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
            for connection in &mut connections {
                if matches!(connection.role, ConnectionRole::Attached { .. }) {
                    if let Some(delta) = &delta {
                        connection.send(delta)?;
                    }
                    connection.publish_documents(&mut session)?;
                }
            }
            for (token, outcome) in completed {
                if let Some((client, request_id, result)) =
                    operations.complete(&token, outcome, published.revision)
                    && let Some(connection) = connections.iter_mut().find(|connection| {
                        connection
                            .role
                            .client()
                            .is_some_and(|owner| owner == client)
                    })
                {
                    connection.send(&ServerMessage::CommandResult {
                        request_id,
                        server_instance: server_instance.clone(),
                        result,
                    })?;
                }
            }
            // A failed flush can detach the terminal and reclaim fullscreen documents.
            let mut index = 0;
            while index < connections.len() {
                let connection = &mut connections[index];
                if connection.flush().is_err() {
                    connections.remove(index).disconnect(
                        &mut session,
                        &mut active,
                        workspace_view.as_ref(),
                        |id| operations.retains_page_document(id),
                    );
                } else {
                    index += 1;
                }
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}
