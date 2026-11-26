use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zess_ui::Span;

pub mod bindings;
pub mod chunks;
pub mod document;
pub mod editor_events;
pub mod workspace_view;

pub mod resource;

#[cfg(test)]
mod tests;

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const PROTOCOL_VERSION: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Zero denotes a peer with no advertised protocol version.
    #[serde(default)]
    pub protocol_version: u32,
    pub build_id: String,
}

#[derive(Debug)]
pub struct ProtocolMismatch {
    pub expected: VersionInfo,
    pub actual: VersionInfo,
}

impl std::fmt::Display for ProtocolMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "zess protocol mismatch: expected protocol {} (build {}), received protocol {} (build {})",
            self.expected.protocol_version,
            self.expected.build_id,
            self.actual.protocol_version,
            self.actual.build_id,
        )?;
        if self.actual.protocol_version == 0 {
            formatter.write_str(" (peer did not advertise a protocol version)")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProtocolMismatch {}

impl VersionInfo {
    pub fn new(build_id: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            build_id: build_id.into(),
        }
    }

    pub fn check_compatible(&self, actual: &Self) -> Result<(), ProtocolMismatch> {
        if self.protocol_version != 0 && self.protocol_version == actual.protocol_version {
            Ok(())
        } else {
            Err(ProtocolMismatch {
                expected: self.clone(),
                actual: actual.clone(),
            })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientExitDisposition {
    Retained,
    Stopping,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerInstanceId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// Identifies an operation within a client. First submissions use increasing IDs
/// starting at 1, and retries reuse the original ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub workspace_root: String,
    pub editor_hooks: editor_events::EditorHooks,
    pub editor_options: zess_plugin::EditorOptions,
    pub text_options: serde_json::Value,
    pub styles: HashMap<String, zess_ui::TextStyle>,
    pub views: Vec<ViewSnapshot>,
    pub surfaces: Vec<SurfaceSnapshot>,
    pub layout: Vec<ViewArea>,
    pub focused_view: Option<u64>,
    pub prompt: Option<PromptSnapshot>,
    pub status: String,
    pub modified: bool,
    pub buffers: Vec<BufferSummary>,
    pub workspace: ClientWorkspace,
    pub client_actions: Vec<ClientAction>,
}

impl SessionSnapshot {
    /// Checks references and text positions before the client adopts the snapshot.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .focused_view
            .is_some_and(|id| !self.views.iter().any(|view| view.view_id == id))
        {
            return Err("focused view is missing");
        }
        for view in &self.views {
            if let ViewSelection::Text { ranges, primary } = &view.selection
                && *primary >= ranges.len()
            {
                return Err("text view has no primary selection");
            }
        }

        if let Some(prompt) = &self.prompt {
            if !self.views.iter().any(|view| {
                view.view_id == prompt.owner_view && view.buffer_id == prompt.owner_buffer
            }) {
                return Err("prompt owner is missing");
            }
            if !prompt.text.is_char_boundary(prompt.cursor) {
                return Err("prompt cursor is outside its input");
            }
        }
        for (content, metrics) in self
            .views
            .iter()
            .map(|view| (&view.content, &view.text_metrics))
            .chain(
                self.surfaces
                    .iter()
                    .map(|surface| (&surface.content, &surface.text_metrics)),
            )
        {
            if let ViewContent::Text { lines } = content {
                let metrics = metrics
                    .as_ref()
                    .ok_or("text viewport has no document metrics")?;
                if metrics.tab_width == 0 || metrics.line_count == 0 {
                    return Err("text viewport has invalid document metrics");
                }
                for line in lines {
                    if line.line >= metrics.line_count
                        || line
                            .first_char
                            .checked_add(line.text.chars().count())
                            .is_none()
                    {
                        return Err("text viewport contains an invalid source range");
                    }
                }
            }
        }

        for area in &self.layout {
            let exists = match (area.view_id, area.page_id, &area.slot_id) {
                (Some(id), None, None) => self
                    .views
                    .iter()
                    .any(|view| view.view_id == id && view.buffer_id == area.buffer_id),
                (None, Some(page), Some(slot)) => self.surfaces.iter().any(|surface| {
                    surface.page_id == page
                        && surface.slot_id == *slot
                        && surface.buffer_id == area.buffer_id
                }),
                _ => false,
            };
            if !exists {
                return Err("layout refers to a missing view or surface");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BufferSummary {
    pub id: u64,
    pub kind: BufferKind,
    pub modified: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BufferKind {
    Text { path: Option<String> },
    Data { name: String },
    Snapshot,
    Preview,
    Items { name: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientWorkspace {
    pub buffers: Vec<u64>,
    pub current: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientAction {
    pub id: u64,
    pub kind: ClientActionKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientActionKind {
    OpenPage { source: resource::PageSource },
    Open { buffer_id: u64 },
    Activate { buffer_id: u64 },
    Forget { buffer_id: u64 },
    Close { buffer_id: u64, force: bool },
}

impl ClientWorkspace {
    pub fn apply(&mut self, action: &ClientActionKind) {
        match *action {
            ClientActionKind::Open { buffer_id } => {
                if !self.buffers.contains(&buffer_id) {
                    self.buffers.push(buffer_id);
                }
            }
            ClientActionKind::Activate { buffer_id } => self.current = Some(buffer_id),
            ClientActionKind::Forget { buffer_id } => {
                self.buffers.retain(|id| *id != buffer_id);
                if self.current == Some(buffer_id) {
                    self.current = None;
                }
            }
            ClientActionKind::OpenPage { .. } | ClientActionKind::Close { .. } => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceSnapshot {
    pub text_metrics: Option<TextMetrics>,
    pub column_alignment: zess_ui::ColumnAlignment,
    pub soft_wrap: bool,
    pub page_id: u64,
    pub slot_id: String,
    pub buffer_id: u64,
    pub header: String,
    pub loading: bool,
    pub content: ViewContent,
    pub scroll: usize,
    pub horizontal_scroll: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewSnapshot {
    pub text_metrics: Option<TextMetrics>,
    pub statusline: Option<zess_plugin::StatusLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
    pub key_scopes: Vec<String>,
    pub view_id: u64,
    pub buffer_id: u64,
    pub kind: String,
    pub title: String,
    pub header: String,
    pub tabs: Vec<zess_ui::DisplayTab>,
    pub loading: bool,
    pub column_alignment: zess_ui::ColumnAlignment,
    pub version: Option<u64>,
    pub mode: String,
    pub content: ViewContent,
    pub selection: ViewSelection,
    pub scroll: usize,
    pub horizontal_scroll: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewContent {
    Text { lines: Vec<VisibleTextLine> },
    Items { rows: Vec<SemanticRow> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextMetrics {
    pub line_count: usize,
    pub tab_width: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceHighlight {
    pub start: usize,
    pub end: usize,
    pub style: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisibleTextLine {
    pub line: usize,
    pub first_char: usize,
    pub first_column: usize,
    pub end_of_line: bool,
    pub highlights: Vec<SourceHighlight>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRow {
    pub line_number: Option<usize>,
    pub id: String,
    pub text: String,
    pub cells: Vec<Vec<Span>>,
    pub style: String,
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewSelection {
    Text {
        ranges: Vec<TextRange>,
        primary: usize,
    },
    Items {
        ids: Vec<String>,
        primary: Option<String>,
    },
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextRange {
    pub anchor: usize,
    pub head: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewArea {
    pub view_id: Option<u64>,
    pub page_id: Option<u64>,
    pub slot_id: Option<String>,
    pub buffer_id: u64,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptSnapshot {
    pub label: String,
    pub owner_buffer: u64,
    pub kind: String,
    pub text: String,
    pub cursor: usize,
    pub error: Option<String>,
    pub owner_view: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionDelta {
    pub editor_hooks: editor_events::EditorHooks,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_options: Option<zess_plugin::EditorOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_options: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub styles: Option<HashMap<String, zess_ui::TextStyle>>,
    pub changed_views: Vec<ViewSnapshot>,
    pub surfaces: Vec<SurfaceSnapshot>,
    pub removed_views: Vec<u64>,
    pub layout: Vec<ViewArea>,
    pub focused_view: Option<u64>,
    pub prompt: Option<PromptSnapshot>,
    pub status: String,
    pub modified: bool,
    pub buffers: Vec<BufferSummary>,
    pub workspace: ClientWorkspace,
    pub client_actions: Vec<ClientAction>,
}

impl SessionDelta {
    pub fn between(previous: &SessionSnapshot, next: &SessionSnapshot) -> Self {
        Self {
            editor_hooks: next.editor_hooks,
            editor_options: (previous.editor_options != next.editor_options)
                .then_some(next.editor_options),
            text_options: (previous.text_options != next.text_options)
                .then(|| next.text_options.clone()),
            styles: (previous.styles != next.styles).then(|| next.styles.clone()),
            changed_views: next
                .views
                .iter()
                .filter(|view| {
                    previous
                        .views
                        .iter()
                        .find(|old| old.view_id == view.view_id)
                        != Some(*view)
                })
                .cloned()
                .collect(),
            removed_views: previous
                .views
                .iter()
                .filter(|view| !next.views.iter().any(|new| new.view_id == view.view_id))
                .map(|view| view.view_id)
                .collect(),
            surfaces: next.surfaces.clone(),
            layout: next.layout.clone(),
            focused_view: next.focused_view,
            prompt: next.prompt.clone(),
            status: next.status.clone(),
            modified: next.modified,
            buffers: next.buffers.clone(),
            workspace: next.workspace.clone(),
            client_actions: next.client_actions.clone(),
        }
    }

    pub fn apply(self, state: &mut SessionSnapshot) {
        state.editor_hooks = self.editor_hooks;
        if let Some(options) = self.editor_options {
            state.editor_options = options;
        }
        if let Some(options) = self.text_options {
            state.text_options = options;
        }
        if let Some(styles) = self.styles {
            state.styles = styles;
        }
        state
            .views
            .retain(|view| !self.removed_views.contains(&view.view_id));
        for view in self.changed_views {
            if let Some(previous) = state
                .views
                .iter_mut()
                .find(|old| old.view_id == view.view_id)
            {
                *previous = view;
            } else {
                state.views.push(view);
            }
        }
        state.views.sort_by_key(|view| view.view_id);
        state.surfaces = self.surfaces;
        state.layout = self.layout;
        state.focused_view = self.focused_view;
        state.prompt = self.prompt;
        state.status = self.status;
        state.modified = self.modified;
        state.buffers = self.buffers;
        state.workspace = self.workspace;
        state.client_actions = self.client_actions;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandTarget {
    View {
        view_id: u64,
        buffer_id: u64,
    },
    Page {
        subscription_id: u64,
        input: resource::PageCommandInput,
    },
    Document {
        buffer_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandAction {
    Observe {
        source: editor_events::EventSource,
        event: editor_events::EditorEvent,
    },
    AcknowledgeClientAction {
        action_id: u64,
    },
    CloseBuffer {
        action_id: u64,
        buffer_id: u64,
        replacement: Option<u64>,
        force: bool,
    },
    ClientExit {
        action_id: u64,
        buffer_id: u64,
    },
    Paste {
        text: String,
    },
    CommandLine {
        text: String,
    },
    Command {
        name: String,
        argument: serde_json::Value,
    },
    ClosePageDocument {
        buffer_id: u64,
        force: bool,
    },
    ApplyPagePatch {
        subscription_id: u64,
        request: resource::PagePatch,
    },
    PagePrompt {
        subscription_id: u64,
        input: resource::PageCommandInput,
        purpose: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirmation: Option<String>,
        text: String,
        cancelled: bool,
    },
    Invoke {
        target: CommandTarget,
        name: String,
        argument: serde_json::Value,
    },
    Request {
        target: CommandTarget,
        request: zess_plugin::HostRequest,
    },
    OpenDocument {
        path: Option<String>,
    },
    DiscardScratch {
        buffer_id: u64,
        replacement: u64,
    },
    Save {
        buffer_id: u64,
        version: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OperationResult {
    Pending,
    Completed {
        outcome: CommandOutcome,
        state_revision: u64,
    },
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandOutcome {
    Applied,
    ClientRequest {
        request: zess_plugin::HostRequest,
    },
    Yanked {
        values: Vec<String>,
        register: char,
    },
    Message {
        message: String,
    },
    BufferClosed {
        buffer_id: u64,
        replacement: u64,
    },
    PageDocumentClosed {
        buffer_id: u64,
    },
    PageApplied {
        page_id: resource::PageId,
        subscription_id: u64,
        navigation: Vec<zess_plugin::Navigation>,
        document: Option<resource::PageDocument>,
    },
    DocumentOpened {
        buffer_id: u64,
    },
    ScratchDiscarded {
        buffer_id: u64,
        replacement: u64,
    },
    Saved {
        buffer_id: u64,
        version: u64,
    },
    Failed {
        message: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Document(document::DocumentRequest),
    ClaimPendingSession {
        pid: u32,
    },
    ConfirmAttach,
    Hello {
        #[serde(flatten)]
        version: VersionInfo,
    },
    Status,
    Stop {
        force: bool,
    },
    Attach {
        expected_session: Option<SessionId>,
        client_id: String,
        width: u16,
        height: u16,
        #[serde(default)]
        files: Vec<String>,
    },
    Resize {
        width: u16,
        height: u16,
    },
    SaveWorkspaceView {
        view: workspace_view::WorkspaceView,
    },
    ClipboardResponse {
        id: u64,
        text: Option<String>,
        error: Option<String>,
    },
    Resync {
        session_id: SessionId,
        state_revision: u64,
    },
    Command {
        request_id: RequestId,
        server_instance: ServerInstanceId,
        action: CommandAction,
    },
    Chunk {
        json: String,
        last: bool,
    },
    FormatStatusline {
        id: u64,
        name: String,
        context: zess_plugin::StatusContext,
    },
    Resource {
        request: resource::ResourceRequest,
    },
    QueryOperation {
        request_id: RequestId,
        server_instance: ServerInstanceId,
    },
    /// Releases a completed outcome. Further queries for that request return `Unknown`.
    ResultAck {
        request_id: RequestId,
        server_instance: ServerInstanceId,
    },
    Detach,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Bindings(bindings::BindingCatalog),
    Document(document::DocumentEvent),
    AttachPending,
    AttachConfirmed,
    Hello {
        #[serde(flatten)]
        version: VersionInfo,
    },
    Status {
        #[serde(flatten)]
        version: VersionInfo,
        session_id: SessionId,
        name: Option<String>,
        server_instance: ServerInstanceId,
        state_revision: u64,
        pid: u32,
        workspace: String,
        attached: bool,
        modified: bool,
        #[serde(default)]
        pending: bool,
    },
    Attached {
        server_instance: ServerInstanceId,
        context_epoch: u64,
        workspace_view: Option<workspace_view::WorkspaceView>,
    },
    WorkspaceViewSaved,
    Snapshot {
        session_id: SessionId,
        server_instance: ServerInstanceId,
        state_revision: u64,
        state: SessionSnapshot,
        bindings: bindings::BindingCatalog,
    },
    Delta {
        session_id: SessionId,
        server_instance: ServerInstanceId,
        base_revision: u64,
        state_revision: u64,
        delta: SessionDelta,
    },
    Chunk {
        json: String,
        last: bool,
    },
    FormattedStatusline {
        id: u64,
        result: Result<zess_plugin::StatusLine, String>,
    },
    Resource {
        update: resource::ResourceUpdate,
    },
    ResourceCacheInvalidated {
        entries: Vec<resource::PageCacheEntry>,
        reason: resource::ResourceCacheInvalidation,
    },
    CommandResult {
        request_id: RequestId,
        server_instance: ServerInstanceId,
        result: OperationResult,
    },
    Detached {
        disposition: ClientExitDisposition,
    },
    ClipboardWrite {
        text: String,
    },
    ClipboardWritePrimary {
        text: String,
    },
    ClipboardRead {
        id: u64,
        primary: bool,
    },
    Rejected {
        reason: String,
    },
    Exited,
}
