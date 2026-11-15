use serde::{Deserialize, Serialize};
use zess_ui::{Frame, MouseContext, MouseEvent, Span};

#[cfg(test)]
mod tests;

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct BuildMismatch {
    pub expected: &'static str,
    pub actual: String,
}

impl std::fmt::Display for BuildMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "zess build mismatch: expected {}, received {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for BuildMismatch {}

pub fn check_build_id(build_id: &str) -> Result<(), BuildMismatch> {
    if build_id == zess_build::BUILD_ID {
        Ok(())
    } else {
        Err(BuildMismatch {
            expected: zess_build::BUILD_ID,
            actual: build_id.to_owned(),
        })
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub render_plan: zess_ui::RenderPlan,
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
            ClientActionKind::Close { .. } => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceSnapshot {
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
    pub status: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewContent {
    Text { lines: Vec<VisibleTextLine> },
    Items { rows: Vec<SemanticRow> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisibleTextLine {
    pub line: usize,
    pub first_char: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRow {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub kind: String,
    pub text: String,
    pub cursor: usize,
    pub error: Option<String>,
    pub owner_view: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionDelta {
    pub render_plan: zess_ui::RenderPlan,
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
            render_plan: next.render_plan.clone(),
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
        state.render_plan = self.render_plan;
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
pub enum CommandAction {
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
    BufferClosed { buffer_id: u64, replacement: u64 },
    Saved { buffer_id: u64, version: u64 },
    Failed { message: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        build_id: String,
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
        #[serde(default)]
        context_epoch: Option<u64>,
    },
    Input {
        sequence: u64,
        key: String,
        context_epoch: u64,
    },
    TrackedInput {
        request_id: RequestId,
        server_instance: ServerInstanceId,
        sequence: u64,
        key: String,
        context_epoch: u64,
    },
    Mouse {
        sequence: u64,
        context_epoch: u64,
        mouse_context: MouseContext,
        event: MouseEvent,
    },
    Skip {
        through: u64,
        context_epoch: u64,
    },
    Resize {
        width: u16,
        height: u16,
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
    QueryOperation {
        request_id: RequestId,
        server_instance: ServerInstanceId,
    },
    ResultAck {
        request_id: RequestId,
        server_instance: ServerInstanceId,
    },
    Detach,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello {
        build_id: String,
    },
    Status {
        session_id: SessionId,
        name: Option<String>,
        server_instance: ServerInstanceId,
        state_revision: u64,
        pid: u32,
        workspace: String,
        attached: bool,
        modified: bool,
    },
    Attached {
        server_instance: ServerInstanceId,
        acknowledged: u64,
        context_epoch: u64,
        replay_allowed: bool,
        frame: Frame,
    },
    Update {
        acknowledged: u64,
        frame: Frame,
    },
    Snapshot {
        session_id: SessionId,
        server_instance: ServerInstanceId,
        state_revision: u64,
        state: SessionSnapshot,
        frame: Frame,
    },
    Delta {
        session_id: SessionId,
        server_instance: ServerInstanceId,
        base_revision: u64,
        state_revision: u64,
        delta: SessionDelta,
        frame: Option<Frame>,
    },
    Ack {
        acknowledged: u64,
        context_epoch: u64,
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

pub fn render_snapshot(state: &SessionSnapshot) -> Result<Frame, zess_ui::RenderError> {
    zess_ui::render_plan(&state.render_plan, |source| match source {
        zess_ui::RenderContent::Literal { text } => Some(text.clone()),
        zess_ui::RenderContent::Text {
            buffer_id,
            start_char,
            end_char,
        } => state
            .views
            .iter()
            .filter(|view| view.buffer_id == *buffer_id)
            .map(|view| &view.content)
            .chain(
                state
                    .surfaces
                    .iter()
                    .filter(|surface| surface.buffer_id == *buffer_id)
                    .map(|surface| &surface.content),
            )
            .find_map(|content| text_source(content, *start_char, *end_char)),
        zess_ui::RenderContent::Item {
            source,
            row_id,
            cell,
            start_char,
            end_char,
        } => {
            let content = match source {
                zess_ui::ContentSource::View { view_id } => {
                    &state
                        .views
                        .iter()
                        .find(|view| view.view_id == *view_id)?
                        .content
                }
                zess_ui::ContentSource::Surface { page_id, slot_id } => {
                    &state
                        .surfaces
                        .iter()
                        .find(|surface| surface.page_id == *page_id && surface.slot_id == *slot_id)?
                        .content
                }
            };
            let ViewContent::Items { rows } = content else {
                return None;
            };
            let row = rows.iter().find(|row| row.id == *row_id)?;
            let text = match cell {
                None => row.text.clone(),
                Some(cell) => row
                    .cells
                    .get(*cell)?
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect(),
            };
            char_slice(&text, *start_char, *end_char)
        }
    })
}

fn text_source(content: &ViewContent, start: usize, end: usize) -> Option<String> {
    let ViewContent::Text { lines } = content else {
        return None;
    };
    lines.iter().find_map(|line| {
        if start < line.first_char {
            return None;
        }
        char_slice(
            &line.text,
            start - line.first_char,
            end.checked_sub(line.first_char)?,
        )
    })
}

fn char_slice(text: &str, start: usize, end: usize) -> Option<String> {
    if end < start {
        return None;
    }
    let mut chars = text.chars();
    for _ in 0..start {
        chars.next()?;
    }
    (start..end).map(|_| chars.next()).collect()
}
