use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use smallvec::{SmallVec, smallvec};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use zess_filter::{Query, QueryKind};
use zess_plugin::{
    BufferId, CommandCount, FileSnapshot, HostRequest, IdentityRegistry, ItemNavigation,
    ItemSelection, KeyState, Navigation, PluginContext, PluginInput, PreviewArea as Rect,
    PreviewContent, PreviewLayout, PreviewPane, QueryWatch, SelectionChange, SnapshotId,
    SnapshotPresentation, TextContext, ViewId,
};
use zess_plugins::builtin::BuiltinPlugins;
use zess_script::{
    CommandOutput, KeyResolution, NativeSteelCompiler, ScriptCompiler, ScriptRuntime,
};
use zess_services::{NativeServices, Observation};
use zess_text::{
    ClipboardRead, HelixSyntaxProvider, InsertHistory, Registers, SyntaxProvider, TEXT_COMMANDS,
    TextBuffer, TextCommand, TextEffect, TextMode, TextPromptKind,
};
use zess_ui::{CursorShape, Display, DisplayRow, Frame, FrameRow, ScrollMode, Span};

mod commands;
mod context;
mod diff;
mod files;
mod items;
mod mouse;
mod navigation;
mod pages;
mod presentation;
mod preview;
mod render;
mod snapshot;
mod status;
#[cfg(test)]
mod tests;
mod ui;
use commands::{DataCommand, HelpCommand, NativeAction, PromptCommand};
use pages::{PageId, RenderTarget};
use ui::{Completion, Help};

enum Buffer {
    Text(TextBuffer),
    Data {
        kind: String,
        argument: Value,
        modified: bool,
        text_source: Option<BufferId>,
    },
    Snapshot {
        id: SnapshotId,
        text: TextBuffer,
    },
    PreviewText {
        path: String,
        snapshot: Option<SnapshotId>,
        text: TextBuffer,
    },
    PreviewDiff {
        path: String,
        diff: diff::Diff,
    },
    PreviewRows {
        kind: PreviewRowKind,
        path: String,
        display: Display,
    },
    Items {
        role: ItemsRole,
        kind: String,
        path: String,
        display: Display,
        help_context: Option<ui::HelpContext>,
    },
}

#[derive(PartialEq)]
enum ItemsRole {
    Standalone,
    Preview,
}

#[derive(PartialEq)]
enum PreviewRowKind {
    Content,
    Directory,
    Unsupported,
}

impl PreviewRowKind {
    fn scope(&self) -> &str {
        match self {
            Self::Directory => "directory",
            Self::Content | Self::Unsupported => "preview",
        }
    }
}

impl Buffer {
    fn plugin_kind(&self) -> Option<&str> {
        match self {
            Self::Data { kind, .. } => Some(kind),
            _ => None,
        }
    }

    fn is_preview(&self) -> bool {
        match self {
            Self::PreviewText { .. }
            | Self::PreviewDiff { .. }
            | Self::PreviewRows { .. }
            | Self::Items {
                role: ItemsRole::Preview,
                ..
            } => true,
            _ => false,
        }
    }

    fn text(&mut self) -> Option<&mut TextBuffer> {
        match self {
            Self::Text(text) | Self::Snapshot { text, .. } | Self::PreviewText { text, .. } => {
                Some(text)
            }
            Self::Data { .. }
            | Self::Items { .. }
            | Self::PreviewRows { .. }
            | Self::PreviewDiff { .. } => None,
        }
    }
}

enum LayerContent {
    Draft { purpose: String },
    File,
}

struct LayerAttachment {
    parent: ViewId,
    content: LayerContent,
    presentation: LayerPresentation,
}

#[derive(Clone, Copy, PartialEq)]
enum LayerPresentation {
    Inline,
    Pane,
    Fullscreen,
}

#[derive(Clone, PartialEq)]
enum ViewMode {
    Normal,
    Select,
    Filter,
    Custom(String),
}

impl ViewMode {
    fn from_name(name: &str) -> Self {
        match name {
            "normal" => Self::Normal,
            "select" => Self::Select,
            "filter" => Self::Filter,
            _ => Self::Custom(name.into()),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Normal => "normal",
            Self::Select => "select",
            Self::Filter => "filter",
            Self::Custom(name) => name,
        }
    }
}

#[derive(Clone)]
struct ViewLocation {
    buffer: BufferId,
    config_generation: Option<u64>,
    mode: ViewMode,
    memories: HashMap<String, Value>,
    filter: zess_filter::Filter,
    options: Value,
    display: Display,
    columns: zess_ui::RowColumns,
    selection: ItemSelection,
    diff_selection: diff::Selection,
    diff_presentation: diff::Presentation,
    statusline: Vec<Span>,
    scroll: usize,
    horizontal_scroll: usize,
    scroll_mode: ScrollMode,
}

impl ViewLocation {
    fn set_display(&mut self, display: Display) {
        let unchanged = display.rows_revision.is_some()
            && display.rows_revision == self.display.rows_revision
            && display.column_alignment == self.display.column_alignment;
        if !unchanged {
            self.columns = zess_ui::RowColumns::measure(&display.rows, display.column_alignment);
        }
        self.display = display;
    }

    fn new(buffer: BufferId) -> Self {
        Self {
            buffer,
            config_generation: None,
            mode: ViewMode::Normal,
            memories: HashMap::new(),
            filter: zess_filter::Filter::default(),
            options: json!({}),
            display: Display::default(),
            columns: zess_ui::RowColumns::default(),
            selection: ItemSelection::default(),
            diff_selection: diff::Selection::default(),
            diff_presentation: diff::Presentation::Raw,
            statusline: Vec::new(),
            scroll: 0,
            horizontal_scroll: 0,
            scroll_mode: ScrollMode::FollowSelection,
        }
    }
}

#[derive(Default)]
enum KeySequence {
    #[default]
    Idle,
    Prefix {
        keys: Vec<String>,
        matching: KeyState,
    },
}

impl KeySequence {
    fn keys(&self) -> &[String] {
        match self {
            Self::Idle => &[],
            Self::Prefix { keys, .. } => keys,
        }
    }

    fn clear(&mut self) {
        *self = Self::Idle;
    }

    fn synchronize(&mut self, runtime: &ScriptRuntime, scopes: &[&str], mode: &str) {
        if let Self::Prefix { matching, .. } = self
            && runtime.with_keymap(|keymap| keymap.synchronize(matching, scopes, mode))
        {
            self.clear();
        }
    }

    fn advance(
        &mut self,
        runtime: &ScriptRuntime,
        scopes: &[&str],
        mode: &str,
        key: &str,
    ) -> (KeyResolution, Self) {
        let previous = std::mem::take(self);
        let mut matching = match &previous {
            Self::Idle => KeyState::default(),
            Self::Prefix { matching, .. } => matching.clone(),
        };
        let context = matching.clone();
        let resolution = runtime.advance_key(&mut matching, scopes, mode, key);
        let previous = if matching.shares_context(&context) {
            previous
        } else {
            Self::Idle
        };
        if matches!(resolution, KeyResolution::Prefix) {
            let mut keys = previous.keys().to_vec();
            keys.push(key.into());
            *self = Self::Prefix { keys, matching };
        }
        (resolution, previous)
    }
}

struct ClipboardReadRequest {
    id: u64,
    view: ViewId,
    buffer: BufferId,
    operation: ClipboardRead,
    operation_id: Option<String>,
}

enum RegisterInput {
    None,
    Selecting,
    Selected(char),
}

struct View {
    id: ViewId,
    location: ViewLocation,
    viewport_width: usize,
    viewport_height: usize,
    count: CommandCount,
    register: RegisterInput,
    jump: Option<items::ItemJump>,
    return_to: Option<pages::FocusTarget>,
    wake_at: Option<Instant>,
    preview_area: Option<Rect>,
    observations: Vec<(QueryWatch, Observation)>,
}

impl View {
    fn new(id: ViewId, buffer: BufferId) -> Self {
        Self {
            id,
            location: ViewLocation::new(buffer),
            viewport_width: 0,
            viewport_height: 0,
            count: CommandCount::default(),
            register: RegisterInput::None,
            jump: None,
            return_to: None,
            wake_at: None,
            preview_area: None,
            observations: Vec::new(),
        }
    }

    fn replace_location(&mut self, next: ViewLocation) -> ViewLocation {
        self.wake_at = None;
        self.observations.clear();
        self.count.clear();
        self.register = RegisterInput::None;
        self.jump = None;
        std::mem::replace(&mut self.location, next)
    }
}

enum ViewDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Horizontal,
    Vertical,
}

enum Layout {
    Leaf(PageId),
    Split {
        axis: Axis,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    fn leaves(&self, leaves: &mut Vec<PageId>) {
        match self {
            Self::Leaf(id) => leaves.push(*id),
            Self::Split { first, second, .. } => {
                first.leaves(leaves);
                second.leaves(leaves);
            }
        }
    }

    fn split(&mut self, target: PageId, new: PageId, axis: Axis) -> bool {
        match self {
            Self::Leaf(id) if *id == target => {
                *self = Self::Split {
                    axis,
                    first: Box::new(Self::Leaf(target)),
                    second: Box::new(Self::Leaf(new)),
                };
                true
            }
            Self::Split { first, second, .. } => {
                first.split(target, new, axis) || second.split(target, new, axis)
            }
            _ => false,
        }
    }

    fn remove(self, target: PageId) -> Option<Self> {
        match self {
            Self::Leaf(id) => (id != target).then_some(Self::Leaf(id)),
            Self::Split {
                axis,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(other), None) | (None, Some(other)) => Some(other),
                _ => None,
            },
        }
    }

    fn children_on_axis<'a>(&'a self, axis: Axis, out: &mut Vec<&'a Layout>) {
        match self {
            Self::Split {
                axis: child_axis,
                first,
                second,
            } if *child_axis == axis => {
                first.children_on_axis(axis, out);
                second.children_on_axis(axis, out);
            }
            _ => out.push(self),
        }
    }

    fn page_rectangles(&self, rect: Rect, out: &mut Vec<(PageId, Rect)>) {
        match self {
            Self::Leaf(id) => out.push((*id, rect)),
            Self::Split { axis, .. } => {
                let mut children = Vec::new();
                self.children_on_axis(*axis, &mut children);
                let count = children.len();
                let gap = usize::from(*axis == Axis::Vertical);
                let extent = match axis {
                    Axis::Vertical => rect.width,
                    Axis::Horizontal => rect.height,
                };
                let mut available = extent.saturating_sub(gap * (count - 1));
                let mut offset = 0;
                for (index, child) in children.into_iter().enumerate() {
                    let size = available / (count - index);
                    let area = match axis {
                        Axis::Vertical => Rect {
                            x: rect.x + offset.min(extent),
                            width: size,
                            ..rect
                        },
                        Axis::Horizontal => Rect {
                            y: rect.y + offset,
                            height: size,
                            ..rect
                        },
                    };
                    child.page_rectangles(area, out);
                    available -= size;
                    offset += size + gap;
                }
            }
        }
    }
}

enum MiniBufferInput {
    Text,
    Query(QueryKind),
}

enum PromptKind {
    Command,
    Text(TextPromptKind),
    DiffSearch(diff::Search),
    Filter(QueryKind),
    MiniBuffer {
        purpose: String,
        key_scope: Option<String>,
        input: MiniBufferInput,
    },
}

struct PromptTarget {
    view: ViewId,
    buffer: BufferId,
}

struct Prompt {
    kind: PromptKind,
    text: String,
    target: PromptTarget,
    completion: Option<Completion>,
    cursor: usize,
    error: Option<String>,
}

impl Prompt {
    fn query(&self) -> Option<(zess_filter::QueryKind, &str)> {
        match &self.kind {
            PromptKind::Filter(kind)
            | PromptKind::MiniBuffer {
                input: MiniBufferInput::Query(kind),
                ..
            } => Some((*kind, &self.text)),
            _ => None,
        }
    }
}

struct RuntimeHost {
    services: Rc<RefCell<NativeServices>>,
    jobs: RefCell<HashSet<u64>>,
}

impl RuntimeHost {
    fn call(&self, operation: &str, argument: Value) -> Value {
        let handle = argument.get("handle").and_then(Value::as_u64);
        let result = self.services.borrow_mut().call(operation, argument);
        match operation {
            "job.start" => {
                if let Some(handle) = result["handle"].as_u64() {
                    self.jobs.borrow_mut().insert(handle);
                }
            }
            "job.poll" if result["status"] != "pending" => {
                if let Some(handle) = handle {
                    self.jobs.borrow_mut().remove(&handle);
                }
            }
            "job.cancel" => {
                if let Some(handle) = handle {
                    self.jobs.borrow_mut().remove(&handle);
                }
            }
            _ => {}
        }
        result
    }
}

impl Drop for RuntimeHost {
    fn drop(&mut self) {
        for handle in self.jobs.get_mut().drain() {
            self.services
                .borrow_mut()
                .call("job.cancel", json!({"handle":handle}));
        }
    }
}

/// A session belongs to the server, independently of its terminal connection.
pub struct Session {
    syntax_provider: Arc<dyn SyntaxProvider>,
    script_compiler: Arc<dyn ScriptCompiler>,
    file_watches: BTreeMap<BufferId, files::FileWatch>,
    workspace: PathBuf,
    config: Option<PathBuf>,
    services: Rc<RefCell<NativeServices>>,
    fs_revision: u64,
    registers: Registers,
    insert_history: InsertHistory,
    scripts: Option<ScriptRuntime>,
    identities: IdentityRegistry,
    builtins: BuiltinPlugins,
    generation: u64,
    buffers: BTreeMap<BufferId, Buffer>,
    client_workspace: zess_protocol::ClientWorkspace,
    client_actions: Vec<zess_protocol::ClientAction>,
    views: pages::Store,
    layout: Option<Layout>,
    focus: ViewId,
    next_id: u64,
    last_text: Option<BufferId>,
    git_patch: Option<preview::PendingPatch>,
    pending_diff_source: Option<preview::PendingDiffSource>,
    prompt: Option<Prompt>,
    help: Option<Help>,
    key_sequence: KeySequence,
    clipboard_writes: Vec<String>,
    primary_clipboard_writes: Vec<String>,
    clipboard_read: Option<ClipboardReadRequest>,
    message: String,
    width: u16,
    height: u16,
    exiting: bool,
    current_operation: Option<String>,
    pending_operations: HashSet<String>,
    operation_outcome: Option<zess_protocol::CommandOutcome>,
    operation_completions: Vec<(String, zess_protocol::CommandOutcome)>,
    mouse_regions: Vec<mouse::Region>,
    presentation_regions: Vec<mouse::Region>,
    command_mouse: Option<ui::CommandGrid>,
    mouse_context: zess_ui::MouseContext,
    mouse_drag: Option<ViewId>,
}

impl Session {
    pub fn new(workspace: PathBuf, files: &[PathBuf], config: Option<PathBuf>) -> Result<Self> {
        let services = NativeServices::new(workspace.clone())?;
        Self::with_services(workspace, files, config, services)
    }

    pub fn with_services(
        workspace: PathBuf,
        files: &[PathBuf],
        config: Option<PathBuf>,
        services: NativeServices,
    ) -> Result<Self> {
        let syntax_provider = Arc::new(HelixSyntaxProvider::with_platform(
            Arc::new(zess_platform::NativePlatform),
            services.file_system(),
        ));
        Self::with_capabilities(
            workspace,
            files,
            config,
            services,
            syntax_provider,
            Arc::new(NativeSteelCompiler),
        )
    }

    pub fn with_capabilities(
        workspace: PathBuf,
        files: &[PathBuf],
        config: Option<PathBuf>,
        services: NativeServices,
        syntax_provider: Arc<dyn SyntaxProvider>,
        script_compiler: Arc<dyn ScriptCompiler>,
    ) -> Result<Self> {
        let filesystem = services.file_system();
        let workspace = filesystem
            .canonicalize(&workspace)
            .context("open workspace")?;
        let services = Rc::new(RefCell::new(services));
        let mut session = Self {
            syntax_provider,
            script_compiler,
            file_watches: BTreeMap::new(),
            workspace,
            config,
            builtins: BuiltinPlugins::new(services.clone()),
            services,
            fs_revision: 0,
            registers: Registers::default(),
            insert_history: InsertHistory::default(),
            scripts: None,
            identities: IdentityRegistry::default(),
            generation: 0,
            buffers: BTreeMap::new(),
            client_workspace: zess_protocol::ClientWorkspace::default(),
            client_actions: Vec::new(),
            views: pages::Store::default(),
            layout: None,
            focus: 0,
            next_id: 1,
            last_text: None,
            git_patch: None,
            pending_diff_source: None,
            prompt: None,
            help: None,
            key_sequence: KeySequence::default(),
            clipboard_writes: Vec::new(),
            primary_clipboard_writes: Vec::new(),
            clipboard_read: None,
            message: String::new(),
            width: 80,
            height: 24,
            exiting: false,
            current_operation: None,
            pending_operations: HashSet::new(),
            operation_outcome: None,
            operation_completions: Vec::new(),
            mouse_regions: Vec::new(),
            presentation_regions: Vec::new(),
            command_mouse: None,
            mouse_context: zess_ui::MouseContext::default(),
            mouse_drag: None,
        };

        if let Err(error) = session.reload() {
            session.message = match session.build_runtime(false) {
                Ok(runtime) => {
                    session.scripts = Some(runtime);
                    format!("{error:#}")
                }
                Err(default_error) => format!("defaults failed: {default_error:#}; {error:#}"),
            };
        }

        let (directory, files) = match files.split_first() {
            Some((path, files))
                if filesystem
                    .metadata(&session.absolute(path))
                    .is_ok_and(|entry| entry.is_dir()) =>
            {
                (Some(path), files)
            }
            _ => (None, files),
        };
        if files.is_empty() {
            let buffer = session.add_buffer(Buffer::Text(TextBuffer::with_capabilities(
                "",
                filesystem,
                session.syntax_provider.clone(),
            )));
            session.add_initial_view(buffer);
        } else {
            for path in files {
                let buffer = session.load_file(path)?;
                if session.layout.is_none() {
                    session.add_initial_view(buffer);
                }
            }
        }
        session.client_workspace.buffers = session
            .buffers
            .iter()
            .filter_map(|(&id, buffer)| matches!(buffer, Buffer::Text(_)).then_some(id))
            .collect();
        session.client_workspace.current = Some(session.views[&session.focus].location.buffer);

        if let Some(path) = directory {
            session.open_directory(path)?;
        }

        Ok(session)
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.invalidate_mouse();
        self.width = width.clamp(10, 1000);
        self.height = height.clamp(3, 500);

        if let Some(view) = self.views.get_mut(&self.focus) {
            view.jump = None;
        }
    }

    pub fn exiting(&self) -> bool {
        self.exiting
    }

    fn request_stop(&mut self, force: bool) -> Result<()> {
        anyhow::ensure!(
            force || !self.modified(),
            "unsaved buffers; save them first or stop with force"
        );
        self.exiting = true;
        Ok(())
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn modified(&self) -> bool {
        self.buffers.values().any(|buffer| match buffer {
            Buffer::Text(text) => text.modified(),
            Buffer::Data { modified, .. } => *modified,
            Buffer::Snapshot { .. }
            | Buffer::PreviewText { .. }
            | Buffer::PreviewDiff { .. }
            | Buffer::PreviewRows { .. }
            | Buffer::Items { .. } => false,
        })
    }

    pub fn on_client_exit(&self) -> zess_plugin::OnClientExit {
        self.scripts
            .as_ref()
            .map(|scripts| scripts.on_client_exit())
            .unwrap_or_default()
    }

    pub fn tick(&mut self) -> bool {
        let outputs = self
            .scripts
            .as_mut()
            .map(|scripts| scripts.poll_operations())
            .unwrap_or_default();
        let operations_changed = !outputs.is_empty();
        for output in outputs {
            let mut completions = output.completed_operations;
            for request in output.requests {
                let result = match request {
                    HostRequest::Message { text } => {
                        self.message = text;
                        Ok(())
                    }
                    HostRequest::Yank { values, register } => {
                        self.yank(values, register.unwrap_or('"'))
                    }
                    _ => Err(anyhow::anyhow!("background operation requires a view")),
                };
                if let Err(error) = result {
                    for completion in &mut completions {
                        completion.result = zess_plugin::OperationResult::Failed {
                            message: format!("{error:#}"),
                        };
                    }
                    self.message = format!("operation: {error:#}");
                    break;
                }
            }
            self.complete_operations(completions);
        }
        let geometry_changed = self.update_plugin_geometry();
        let patch_changed = self.poll_git_patch();
        let source_changed = self.poll_diff_source();
        let mut syntax_changed = false;
        for buffer in self.buffers.values_mut() {
            if let Buffer::PreviewText { text, .. } = buffer {
                syntax_changed |= text.poll_syntax();
            }
        }
        let watch_error = match self.services.borrow_mut().poll_changes() {
            Ok(()) => false,
            Err(error) => {
                self.message = format!("workspace watcher: {error:#}");
                true
            }
        };
        let file_changed = self.poll_file_changes();
        if file_changed && let Err(error) = self.refresh_context_plugins() {
            self.message = format!("context plugin: {error:#}");
        }
        let revision = self.services.borrow().fs_revision();
        let changed = revision != self.fs_revision;
        self.fs_revision = revision;
        let mut notifications = Vec::new();
        for (id, view) in self.views.iter_mut() {
            let Buffer::Data { kind, .. } = &self.buffers[&view.location.buffer] else {
                continue;
            };
            let mut queries = Vec::new();
            for (query, observation) in &mut view.observations {
                if observation.changed() {
                    queries.push(query.clone());
                }
            }
            let mut refresh_all = false;
            if changed && let Some(runtime) = &self.scripts {
                let fields = runtime.context_fields(kind, "fs_changed");
                refresh_all = fields.iter().any(|field| field == "fs.revision");
            }
            if refresh_all || !queries.is_empty() {
                notifications.push((id, refresh_all, queries));
            }
        }
        let notified = !notifications.is_empty();
        for (id, refresh_all, queries) in notifications {
            let argument = if refresh_all {
                Value::Null
            } else {
                json!({"queries":queries})
            };
            match self.dispatch(id, "fs_changed", argument) {
                Ok(()) => {
                    if let Some(view) = self.views.get_mut(&id) {
                        for (query, observation) in &mut view.observations {
                            if queries.contains(query) {
                                observation.acknowledge();
                            }
                        }
                    }
                }
                Err(error) => {
                    self.message = format!("filesystem refresh: {error:#}");
                }
            }
        }
        let now = Instant::now();
        let due = self
            .views
            .iter()
            .filter_map(|(&id, view)| match view.wake_at {
                Some(when) if when <= now => Some(id),
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in &due {
            self.views.get_mut(id).unwrap().wake_at = None;
            if let Err(error) = self.dispatch(*id, "tick", Value::Null) {
                self.message = format!("plugin task: {error:#}");
            }
        }
        operations_changed
            || geometry_changed
            || patch_changed
            || source_changed
            || syntax_changed
            || watch_error
            || file_changed
            || changed
            || notified
            || !due.is_empty()
    }

    pub fn open_paths(&mut self, paths: &[String]) {
        let filesystem = self.services.borrow().file_system();
        let (directory, paths) = match paths.split_first() {
            Some((path, paths))
                if filesystem
                    .metadata(&self.absolute(Path::new(path)))
                    .is_ok_and(|entry| entry.is_dir()) =>
            {
                (Some(path), paths)
            }
            _ => (None, paths),
        };
        // Keep the tree focused after loading any accompanying files.
        for path in paths.iter().chain(directory) {
            let result = (|| -> Result<()> {
                let path = Path::new(path);
                if filesystem
                    .metadata(&self.absolute(path))
                    .is_ok_and(|entry| entry.is_dir())
                {
                    self.open_directory(path)
                } else {
                    while self.views.parent(self.focus).is_some() {
                        self.close_view(false)?;
                    }
                    let buffer = self.load_file(path)?;
                    self.switch_buffer(buffer)
                }
            })();
            if let Err(error) = result {
                self.message = format!("error: {error:#}");
                break;
            }
        }
    }

    fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn add_buffer(&mut self, mut buffer: Buffer) -> BufferId {
        if let Some(text) = buffer.text() {
            text.set_registers(self.registers.clone());
            text.set_insert_history(self.insert_history.clone());
        }
        let id = self.id();
        self.buffers.insert(id, buffer);
        id
    }

    fn add_initial_view(&mut self, buffer: BufferId) {
        self.focus = self.create_page(buffer);
        self.layout = Some(Layout::Leaf(self.views.page_id(self.focus)));
        self.remember_text();
    }

    fn remember_text(&mut self) {
        let previous = self.last_text;
        if let Some(view) = self.views.get(&self.focus)
            && self.views.layer_content(view.id).is_none()
            && matches!(
                self.buffers.get(&view.location.buffer),
                Some(Buffer::Text(_))
            )
        {
            self.last_text = Some(view.location.buffer);
        }
        if previous != self.last_text
            && let Err(error) = self.refresh_context_plugins()
        {
            self.message = format!("context plugin: {error:#}");
        }
    }

    fn absolute(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.workspace.join(path)
        }
    }

    fn is_switchable_buffer(&self, id: BufferId) -> bool {
        self.buffers
            .get(&id)
            .is_some_and(|buffer| !buffer.is_preview())
            && !self.views.values().any(|view| {
                view.location.buffer == id && self.views.layer_content(view.id).is_some()
            })
            && !self.views.pages().any(|page| {
                page.navigation.plugin.as_ref().is_some_and(|plugin| {
                    plugin.buffers.contains(&id)
                        || (plugin.origin.buffer == id
                            && matches!(
                                self.views.layer_content(page.main.id),
                                Some(LayerContent::File)
                            ))
                })
            })
    }

    fn load_file(&mut self, path: &Path) -> Result<BufferId> {
        let path = self.absolute(path);
        let path = self
            .services
            .borrow()
            .file_system()
            .canonicalize(&path)
            .unwrap_or(path);
        for (id, buffer) in &self.buffers {
            if let Buffer::Text(text) = buffer
                && self.is_switchable_buffer(*id)
                && text.path() == Some(path.as_path())
            {
                return Ok(*id);
            }
        }
        self.open_file_buffer(&path)
    }

    fn open_directory(&mut self, path: &Path) -> Result<()> {
        let path = self
            .services
            .borrow()
            .file_system()
            .canonicalize(&self.absolute(path))
            .context("open directory")?;
        let path = path
            .strip_prefix(&self.workspace)
            .context("directory must remain within the session workspace")?;
        let argument = json!({"path":path.to_string_lossy()});
        while self.views.parent(self.focus).is_some() {
            self.close_view(false)?;
        }
        self.open_plugin(self.focus, "files", argument)
    }

    fn switch_buffer(&mut self, buffer: BufferId) -> Result<()> {
        self.switch_buffer_with(self.focus, buffer, Value::Null)
    }

    fn ensure_buffer_switch_allowed(&self, view: ViewId) -> Result<()> {
        self.ensure_focus_allowed(view)?;
        let source = match &self.help {
            Some(help) if help.view == view => {
                self.views.resolve_focus(help.source).unwrap_or(view)
            }
            _ => view,
        };
        let source = self.preview_source(source).unwrap_or(source);
        if self.views.parent(source).is_some() {
            bail!("close the current layer before switching buffers");
        }
        Ok(())
    }

    fn switch_buffer_with(
        &mut self,
        origin: ViewId,
        buffer: BufferId,
        argument: Value,
    ) -> Result<()> {
        self.ensure_buffer_switch_allowed(origin)?;
        if !self.is_switchable_buffer(buffer) {
            bail!("buffer {buffer} is not a selectable editor buffer");
        }
        let target = self.preview_source(origin).unwrap_or(origin);
        if origin == self.focus && target != origin {
            let source = target;
            self.focus_view(source)?;
        }
        self.close_plugin_navigation(self.views.page_id(target), false)?;
        self.leave_page(self.views.page_id(target))?;
        *self.views.navigation_mut(target) = navigation::History::default();
        let retain_text = matches!(self.buffers[&buffer], Buffer::Data { .. });
        let view = self.views.get_mut(&target).context("no active view")?;
        let previous = view.location.buffer;

        if let Some(text) = self
            .buffers
            .get_mut(&view.location.buffer)
            .and_then(Buffer::text)
        {
            if retain_text {
                text.set_text_mode(target, TextMode::Normal)?;
            } else {
                text.remove_view(target);
            }
        }
        view.replace_location(ViewLocation::new(buffer));
        if let Buffer::Items { display, .. } = &self.buffers[&buffer] {
            view.project_items(display);
        } else if let Buffer::PreviewRows { display, .. } = &self.buffers[&buffer] {
            view.location.set_display(display.clone());
        }
        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
            text.ensure_view(target);
        }
        self.remove_unused_preview_buffer(previous);
        if matches!(
            self.buffers.get(&buffer),
            Some(Buffer::Text(_) | Buffer::Data { .. })
        ) {
            let workspace = self.projected_client_workspace();
            if !workspace.buffers.contains(&buffer) {
                let id = self.id();
                self.client_actions.push(zess_protocol::ClientAction {
                    id,
                    kind: zess_protocol::ClientActionKind::Open { buffer_id: buffer },
                });
            }
            if target == self.focus && workspace.current != Some(buffer) {
                let id = self.id();
                self.client_actions.push(zess_protocol::ClientAction {
                    id,
                    kind: zess_protocol::ClientActionKind::Activate { buffer_id: buffer },
                });
            }
            if previous != buffer
                && matches!(self.buffers.get(&previous), Some(Buffer::Text(text)) if text.path().is_none() && !text.modified())
                && !self.views.references_buffer(previous)
            {
                self.buffers.remove(&previous);
                let id = self.id();
                self.client_actions.push(zess_protocol::ClientAction {
                    id,
                    kind: zess_protocol::ClientActionKind::Forget {
                        buffer_id: previous,
                    },
                });
            }
        }
        self.remember_text();
        self.dispatch(target, "init", argument)?;
        Ok(())
    }

    pub fn input(&mut self, key: &str) {
        let _ = self.input_result(key);
    }

    fn input_result(&mut self, key: &str) -> Result<()> {
        self.mouse_drag = None;
        self.message.clear();
        if let Err(error) = self.invalidate_prompt() {
            self.message = format!("prompt: {error:#}");
            return Err(error);
        }
        self.cancel_clipboard_read("clipboard read cancelled by later input");
        let before = self.selection_snapshot();
        let result = self.handle_input(key);
        if let Err(error) = &result {
            if self.active_prompt().is_none() {
                self.cancel_text_command();
            }
            self.message = format!("error: {error:#}");
        }
        if !self.exiting() {
            let result = (|| -> Result<()> {
                self.emit_event(
                    "input",
                    json!({
                        "key": key,
                        "view_id": self.focus,
                        "buffer_id": self.views[&self.focus].location.buffer,
                    }),
                )?;
                let after = self.selection_snapshot();
                if before != after {
                    self.emit_event("selection-changed", after)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.message = format!("event: {error:#}");
                return Err(error);
            }
        }
        result
    }

    fn selection_snapshot(&mut self) -> Value {
        let view = &self.views[&self.focus];
        if let Some(text) = self
            .buffers
            .get_mut(&view.location.buffer)
            .and_then(Buffer::text)
        {
            json!({"view_id":self.focus,"buffer_id":view.location.buffer,"cursors":text.selections(self.focus)})
        } else if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] {
            let range = view
                .location
                .diff_selection
                .rows(diff.index(view.location.diff_presentation));
            json!({"view_id":self.focus,"buffer_id":view.location.buffer,"diff_selection":{"row": view.location.diff_selection.row(),"start_row":range.as_ref().map(|range| range.start()),"end_row":range.as_ref().map(|range| range.end())}})
        } else {
            json!({"view_id":self.focus,"buffer_id":view.location.buffer,"selection":view.location.selection})
        }
    }

    fn emit_event(&mut self, name: &str, argument: Value) -> Result<()> {
        if let Some(runtime) = &mut self.scripts
            && runtime.has_event(name)
        {
            let result = runtime.event(name, &argument)?;
            if let Ok(request) = serde_json::from_value::<HostRequest>(result) {
                self.request(self.focus, request)?;
            }
        }
        Ok(())
    }

    fn cancel_text_command(&mut self) {
        let buffer = self.views[&self.focus].location.buffer;
        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
            text.cancel_pending_command(self.focus);
        }
    }

    fn cancel_input_context(&mut self) -> Result<()> {
        self.cancel_suspended_prompt()?;
        self.cancel_clipboard_read("clipboard input context changed");
        self.prompt_command(PromptCommand::Cancel)?;
        self.cancel_text_command();
        self.views.get_mut(&self.focus).unwrap().register = RegisterInput::None;
        self.views.get_mut(&self.focus).unwrap().jump = None;
        self.views.get_mut(&self.focus).unwrap().count.clear();
        self.key_sequence.clear();
        self.close_help_overlay()?;
        self.cancel_prompt()
    }

    fn fullscreen_layer(&self) -> Option<ViewId> {
        let mut id = match &self.help {
            Some(help) => self.views.resolve_focus(help.source)?,
            None => self.focus,
        };
        id = self.preview_source(id).unwrap_or(id);
        loop {
            let view = self.views.get(&id)?;
            if self.views.presentation(view.id) == LayerPresentation::Fullscreen {
                return Some(id);
            }
            id = self.views.parent(view.id)?;
        }
    }

    fn visible_pages(&self) -> Vec<(PageId, Rect)> {
        let area = Rect {
            x: 0,
            y: 0,
            width: self.width as usize,
            height: self.height.saturating_sub(1) as usize,
        };
        let mut rectangles = Vec::new();
        if let Some(layer) = self.fullscreen_layer() {
            rectangles.push((self.views.page_id(layer), area));
        } else if let Some(layout) = &self.layout {
            layout.page_rectangles(area, &mut rectangles);
        }
        rectangles
    }

    fn preview_area(&self, view: ViewId) -> Option<Rect> {
        let page = self.views.page_id(view);
        self.visible_pages()
            .into_iter()
            .find(|(id, _)| *id == page)
            .map(|(_, area)| area.split_preview().1)
    }

    fn visible_rectangles(&self) -> Vec<(RenderTarget, Rect)> {
        let mut rectangles = Vec::new();
        for (page, area) in self.visible_pages() {
            self.views.rectangles(page, area, &mut rectangles);
        }
        rectangles
    }

    fn update_plugin_geometry(&mut self) -> bool {
        let mut changed = Vec::new();
        for (&id, view) in self.views.iter() {
            if !matches!(self.buffers[&view.location.buffer], Buffer::Data { .. }) {
                continue;
            }
            if self.preview_area(id) != view.preview_area {
                changed.push(id);
            }
        }
        for id in &changed {
            if let Err(error) = self.dispatch(*id, "render", Value::Null) {
                self.message = format!("plugin geometry: {error:#}");
            }
        }
        !changed.is_empty()
    }

    fn ensure_focus_allowed(&self, view: ViewId) -> Result<()> {
        if let Some(layer) = self.fullscreen_layer() {
            let mut target = self.preview_source(view).unwrap_or(view);
            loop {
                if target == layer {
                    break;
                }
                let Some(parent) = self.views.parent(target) else {
                    bail!("close the fullscreen layer before switching views");
                };
                target = parent;
            }
        }
        Ok(())
    }

    fn focus_view(&mut self, view: ViewId) -> Result<()> {
        self.ensure_focus_allowed(view)?;
        if self.focus != view {
            self.cancel_input_context()?;
            self.blur_diff(self.focus);
        }
        anyhow::ensure!(self.views.contains_key(&view), "view no longer exists");
        self.activate_view(view);
        Ok(())
    }

    fn activate_view(&mut self, view: ViewId) {
        if self.focus != view {
            self.key_sequence.clear();
        }
        self.focus = view;
        let buffer = self.views[&view].location.buffer;
        let workspace = self.projected_client_workspace();
        if workspace.buffers.contains(&buffer) && workspace.current != Some(buffer) {
            let id = self.id();
            self.client_actions.push(zess_protocol::ClientAction {
                id,
                kind: zess_protocol::ClientActionKind::Activate { buffer_id: buffer },
            });
        }
        self.focus_diff(view);
        self.views.activate_preview(view);
    }

    fn prompts(&self) -> impl Iterator<Item = &Prompt> {
        self.prompt
            .iter()
            .chain(self.help.iter().flat_map(|help| &help.suspended))
    }

    fn prompt_target_is_valid(&self, prompt: &Prompt) -> bool {
        if !self.prompt_target_exists(prompt) {
            return false;
        }
        match &prompt.kind {
            PromptKind::Text(kind) => match self.buffers.get(&prompt.target.buffer) {
                Some(
                    Buffer::Text(text)
                    | Buffer::Snapshot { text, .. }
                    | Buffer::PreviewText { text, .. },
                ) => text.pending_prompt(prompt.target.view) == Some(*kind),
                _ => false,
            },
            PromptKind::DiffSearch(_) => matches!(
                self.buffers.get(&prompt.target.buffer),
                Some(Buffer::PreviewDiff { .. })
            ),
            PromptKind::Command | PromptKind::Filter(_) | PromptKind::MiniBuffer { .. } => true,
        }
    }

    fn invalidate_prompt(&mut self) -> Result<()> {
        if self.prompt.as_ref().is_some_and(|prompt| {
            self.focus != prompt.target.view || !self.prompt_target_is_valid(prompt)
        }) {
            self.cancel_prompt()?;
        }
        if self
            .help
            .as_ref()
            .and_then(|help| help.suspended.as_ref())
            .is_some_and(|prompt| !self.prompt_target_is_valid(prompt))
        {
            self.cancel_suspended_prompt()?;
        }
        Ok(())
    }

    fn cancel_prompt(&mut self) -> Result<()> {
        let Some(mut prompt) = self.prompt.take() else {
            return Ok(());
        };
        let result = self.cancel_prompt_effects(&prompt);
        if let Err(error) = &result
            && self.prompt_target_exists(&prompt)
        {
            prompt.error = Some(format!("{error:#}"));
            self.prompt = Some(prompt);
        }
        result
    }

    fn prompt_target_exists(&self, prompt: &Prompt) -> bool {
        self.views
            .get(&prompt.target.view)
            .is_some_and(|view| view.location.buffer == prompt.target.buffer)
    }

    fn cancel_prompt_effects(&mut self, prompt: &Prompt) -> Result<()> {
        self.key_sequence.clear();
        self.restore_prompt(prompt);
        if !self.prompt_target_exists(prompt) {
            return Ok(());
        }
        match &prompt.kind {
            PromptKind::Filter(_) => {
                let previous = self.views[&prompt.target.view].location.mode.clone();
                let result = self.set_mode(prompt.target.view, "normal");
                if result.is_err()
                    && let Some(view) = self.views.get_mut(&prompt.target.view)
                    && view.location.buffer == prompt.target.buffer
                {
                    view.location.mode = previous;
                }
                result
            }
            PromptKind::MiniBuffer { purpose, .. } => self.dispatch(
                prompt.target.view,
                "layer_result",
                json!({"purpose":purpose,"text":prompt.text,"cancelled":true}),
            ),
            PromptKind::Command | PromptKind::Text(_) | PromptKind::DiffSearch(_) => Ok(()),
        }
    }

    fn cancel_suspended_prompt(&mut self) -> Result<()> {
        let Some(mut prompt) = self.help.as_mut().and_then(|help| help.suspended.take()) else {
            return Ok(());
        };
        let result = self.cancel_prompt_effects(&prompt);
        if let Err(error) = &result
            && self.prompt_target_exists(&prompt)
        {
            prompt.error = Some(format!("{error:#}"));
            if let Some(help) = &mut self.help {
                help.suspended = Some(prompt);
            } else {
                self.prompt = Some(prompt);
            }
        }
        result
    }

    fn cancel_prompt_for_view(&mut self, view: ViewId) -> Result<()> {
        if self
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.target.view == view)
        {
            self.cancel_prompt()?;
        }
        if self
            .help
            .as_ref()
            .and_then(|help| help.suspended.as_ref())
            .is_some_and(|prompt| prompt.target.view == view)
        {
            self.cancel_suspended_prompt()?;
        }
        Ok(())
    }

    fn restore_prompt(&mut self, prompt: &Prompt) {
        match &prompt.kind {
            PromptKind::Text(kind) => {
                if let Some(text) = self
                    .buffers
                    .get_mut(&prompt.target.buffer)
                    .and_then(Buffer::text)
                    && text.pending_prompt(prompt.target.view) == Some(*kind)
                {
                    text.cancel_pending_command(prompt.target.view);
                }
            }
            PromptKind::DiffSearch(search) => {
                if let Some(view) = self.views.get_mut(&prompt.target.view)
                    && view.location.buffer == prompt.target.buffer
                {
                    search.restore(view);
                }
            }
            PromptKind::Command | PromptKind::Filter(_) | PromptKind::MiniBuffer { .. } => {}
        }
    }

    fn active_prompt(&self) -> Option<&Prompt> {
        self.prompt
            .as_ref()
            .filter(|prompt| prompt.target.view == self.focus)
    }

    fn key_context(&self) -> (SmallVec<[&str; 4]>, &str) {
        if let Some(prompt) = self.active_prompt() {
            return if let PromptKind::MiniBuffer { key_scope, .. } = &prompt.kind {
                let mut scopes = smallvec!["global", "minibuffer"];
                if let Some(scope) = key_scope {
                    scopes.push(scope.as_str());
                }
                (scopes, "prompt")
            } else {
                (smallvec!["global"], "prompt")
            };
        }
        let view = &self.views[&self.focus];
        let (mut scopes, mode) = match &self.buffers[&view.location.buffer] {
            Buffer::Text(text) | Buffer::Snapshot { text, .. } => {
                (smallvec!["global", "text"], text.view(self.focus).mode())
            }
            Buffer::PreviewText { text, .. } => (
                smallvec!["global", "text", "preview"],
                text.view(self.focus).mode(),
            ),
            Buffer::PreviewDiff { .. } => (smallvec!["global", "preview", "diff"], "normal"),
            Buffer::PreviewRows { .. } => {
                (smallvec!["global", "preview", "preview-rows"], "normal")
            }
            Buffer::Items { kind, .. } => (
                smallvec!["global", "data", kind.as_str()],
                view.location.mode.as_str(),
            ),
            Buffer::Data { kind, .. } => (
                smallvec!["global", "data", kind.as_str()],
                view.location.mode.as_str(),
            ),
        };
        if let Some(scope) = view
            .location
            .options
            .get("key-scope")
            .and_then(Value::as_str)
        {
            scopes.push(scope);
        }
        (scopes, mode)
    }

    fn command_count(&self) -> CommandCount {
        let view = &self.views[&self.focus];
        match &self.buffers[&view.location.buffer] {
            Buffer::Text(text)
            | Buffer::Snapshot { text, .. }
            | Buffer::PreviewText { text, .. } => text.view(self.focus).command_count(),
            _ => view.count,
        }
    }

    fn synchronize_key_sequence(&mut self) {
        if self.key_sequence.keys().is_empty() {
            return;
        }
        let mut sequence = std::mem::take(&mut self.key_sequence);
        {
            let (scopes, mode) = self.key_context();
            if let Some(runtime) = &self.scripts {
                sequence.synchronize(runtime, &scopes, mode);
            } else {
                sequence.clear();
            }
        }
        self.key_sequence = sequence;
    }

    fn handle_input(&mut self, key: &str) -> Result<()> {
        if matches!(self.views[&self.focus].register, RegisterInput::Selecting) {
            let key = if matches!(key, "space" | "Space") {
                " "
            } else {
                key
            };
            let mut characters = key.chars();
            let view = self.views.get_mut(&self.focus).unwrap();
            view.register = match (characters.next(), characters.next()) {
                (Some(character), None) => RegisterInput::Selected(character),
                _ => {
                    view.count.clear();
                    RegisterInput::None
                }
            };
            return Ok(());
        }
        if self.views[&self.focus].jump.is_some() {
            return self.item_jump_input(key);
        }
        if self.help.is_none() && self.prompt.is_none() {
            let buffer = self.views[&self.focus].location.buffer;
            if self
                .buffers
                .get_mut(&buffer)
                .and_then(Buffer::text)
                .is_some_and(|text| text.has_pending_input(self.focus))
            {
                return self.text_input(key);
            }
        }
        let digit = key
            .as_bytes()
            .first()
            .copied()
            .filter(|_| key.len() == 1)
            .filter(u8::is_ascii_digit)
            .map(|digit| digit - b'0');
        let match_key = if key == " " { "space" } else { key };
        let mut sequence = std::mem::take(&mut self.key_sequence);
        let (allows_count, resolution, previous_state) = {
            let (scopes, mode) = self.key_context();
            let (resolution, previous) = sequence.advance(
                self.scripts
                    .as_ref()
                    .context("plugin runtime unavailable")?,
                &scopes,
                mode,
                match_key,
            );
            (matches!(mode, "normal" | "select"), resolution, previous)
        };
        self.key_sequence = sequence;
        if self.help.is_none() && self.active_prompt().is_none() && allows_count {
            if let Some(digit) = digit {
                let continuation = !self.command_count().is_empty();
                let starts_count = digit != 0 && matches!(&resolution, KeyResolution::Unbound);
                if continuation || starts_count {
                    self.key_sequence = previous_state;
                    let buffer = self.views[&self.focus].location.buffer;
                    if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
                        text.push_command_count(self.focus, digit);
                    } else {
                        self.views.get_mut(&self.focus).unwrap().count.push(digit);
                    }
                    return Ok(());
                }
            }
        }
        match resolution {
            KeyResolution::Command(command) => {
                self.key_sequence.clear();
                self.execute_command(&command, json!({}))
            }
            KeyResolution::Prefix => Ok(()),
            KeyResolution::Unbound => {
                let was_pending = !previous_state.keys().is_empty();
                self.views.get_mut(&self.focus).unwrap().register = RegisterInput::None;
                self.views.get_mut(&self.focus).unwrap().count.clear();
                self.key_sequence.clear();
                if was_pending {
                    self.cancel_text_command();
                    return Ok(());
                }
                if self.active_prompt().is_some() {
                    return self.prompt_input(key);
                }
                self.cancel_text_command();
                let buffer = self.views[&self.focus].location.buffer;
                if self
                    .buffers
                    .get_mut(&buffer)
                    .and_then(Buffer::text)
                    .is_some_and(|text| text.view(self.focus).text_mode() == TextMode::Insert)
                {
                    self.text_input(key)?;
                }
                Ok(())
            }
        }
    }

    fn text_input(&mut self, key: &str) -> Result<()> {
        let buffer = self.views[&self.focus].location.buffer;
        let effect = self
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .context("command requires a text buffer")?
            .input_text(self.focus, key)?;
        self.text_effect(effect)
    }

    fn text_effect(&mut self, effect: TextEffect) -> Result<()> {
        self.text_effect_for(self.focus, effect)
    }

    fn text_effect_for(&mut self, origin: ViewId, effect: TextEffect) -> Result<()> {
        if let Some(text) = effect.clipboard {
            self.clipboard_writes.push(text);
        }
        if let Some(text) = effect.primary_clipboard {
            self.primary_clipboard_writes.push(text);
        }
        if let Some(read) = effect.clipboard_read {
            self.cancel_clipboard_read("clipboard read superseded");
            let id = self.id();
            let operation_id = self.current_operation.clone();
            if let Some(operation_id) = &operation_id {
                self.pending_operations.insert(operation_id.clone());
            }
            self.clipboard_read = Some(ClipboardReadRequest {
                id,
                view: origin,
                buffer: self.views[&origin].location.buffer,
                operation: read,
                operation_id,
            });
        }
        if let Some(message) = effect.message {
            self.message = message;
        }
        if let Some(prompt) = effect.prompt {
            self.prompt = Some(Prompt {
                kind: PromptKind::Text(prompt),
                text: String::new(),
                target: PromptTarget {
                    view: origin,
                    buffer: self.views[&origin].location.buffer,
                },
                completion: None,
                error: None,
                cursor: 0,
            });
        }
        if effect.changed {
            self.refresh_context_plugins()?;
        }
        Ok(())
    }

    fn yank(&mut self, values: Vec<String>, register: char) -> Result<()> {
        let count = values.len();
        let clipboard = self.registers.write(register, values)?;
        let mut effect = TextEffect {
            message: Some(format!("yanked {count} selection(s)")),
            ..TextEffect::default()
        };
        if register == '*' {
            effect.primary_clipboard = clipboard;
        } else {
            effect.clipboard = clipboard;
        }
        self.text_effect(effect)
    }

    pub(crate) fn flush_clipboard_writes(
        &mut self,
        mut write: impl FnMut(String) -> Result<()>,
    ) -> bool {
        let writes = std::mem::take(&mut self.clipboard_writes);
        let requested = !writes.is_empty();
        for text in writes {
            if let Err(error) = write(text) {
                self.message = format!("clipboard: {error:#}");
            }
        }
        requested
    }

    pub(crate) fn flush_primary_clipboard_writes(
        &mut self,
        mut write: impl FnMut(String) -> Result<()>,
    ) -> bool {
        let writes = std::mem::take(&mut self.primary_clipboard_writes);
        let requested = !writes.is_empty();
        for text in writes {
            if let Err(error) = write(text) {
                self.message = format!("clipboard: {error:#}");
            }
        }
        requested
    }

    pub(crate) fn clipboard_request(&self) -> Option<(u64, bool)> {
        self.clipboard_read
            .as_ref()
            .map(|request| (request.id, request.operation.register == '*'))
    }

    pub(crate) fn complete_clipboard_read(
        &mut self,
        id: u64,
        text: Option<String>,
        error: Option<String>,
    ) {
        let Some(request) = self.clipboard_read.take() else {
            return;
        };
        if request.id != id {
            self.clipboard_read = Some(request);
            return;
        }
        let ClipboardReadRequest {
            view,
            buffer,
            operation: read,
            operation_id,
            ..
        } = request;
        let result = (|| -> Result<()> {
            if let Some(error) = error {
                bail!(error);
            }
            let text = text.context("no data returned")?;
            anyhow::ensure!(
                self.focus == view
                    && self
                        .views
                        .get(&view)
                        .is_some_and(|current| current.location.buffer == buffer),
                "clipboard input context changed"
            );
            let before = self.selection_snapshot();
            let effect = self
                .buffers
                .get_mut(&buffer)
                .and_then(Buffer::text)
                .context("clipboard buffer no longer exists")?
                .complete_clipboard_read(view, read, text)?;
            if let Err(error) = self.text_effect(effect) {
                self.message = format!("clipboard applied; refresh: {error:#}");
            }
            let after = self.selection_snapshot();
            if before != after
                && let Err(error) = self.emit_event("selection-changed", after)
            {
                self.message = format!("event: {error:#}");
            }
            Ok(())
        })();
        if let Err(error) = &result {
            self.message = format!("clipboard: {error:#}");
        }
        if let Some(operation_id) = operation_id {
            let outcome = match result {
                Ok(()) => zess_protocol::CommandOutcome::Applied,
                Err(error) => zess_protocol::CommandOutcome::Failed {
                    message: format!("{error:#}"),
                },
            };
            self.finish_operation(operation_id, outcome);
        }
    }

    fn cancel_clipboard_read(&mut self, reason: &str) {
        if let Some(request) = self.clipboard_read.take()
            && let Some(operation_id) = request.operation_id
        {
            self.finish_operation(
                operation_id,
                zess_protocol::CommandOutcome::Failed {
                    message: reason.into(),
                },
            );
        }
    }

    fn prompt_input(&mut self, key: &str) -> Result<()> {
        let prompt = self.prompt.as_mut().unwrap();
        prompt.completion = None;
        let value = if key == "space" { " " } else { key };
        if value.chars().count() == 1 && !value.contains(['\n', '\r']) {
            prompt.error = None;
            prompt.text.insert_str(prompt.cursor, value);
            prompt.cursor += value.len();
        }
        self.update_prompt()
    }

    fn update_prompt(&mut self) -> Result<()> {
        if let Some(prompt) = &self.prompt
            && let PromptKind::MiniBuffer { purpose, .. } = &prompt.kind
        {
            let query = prompt.query().is_some();
            let result = self.dispatch(
                prompt.target.view,
                "minibuffer_changed",
                json!({"purpose":purpose,"text":prompt.text}),
            );
            if query {
                self.prompt.as_mut().unwrap().error =
                    result.err().map(|error| format!("{error:#}"));
                return Ok(());
            }
            return result;
        }
        if let Some(prompt) = &self.prompt
            && let PromptKind::Text(kind) = &prompt.kind
        {
            let buffer = prompt.target.buffer;
            let effect = self
                .buffers
                .get_mut(&buffer)
                .and_then(Buffer::text)
                .context("prompt has no text buffer")?
                .preview_prompt(prompt.target.view, *kind, &prompt.text)?;
            return self.text_effect(effect);
        }
        if let Some(prompt) = &self.prompt
            && let PromptKind::DiffSearch(search) = &prompt.kind
        {
            let view = self.views.get_mut(&prompt.target.view).unwrap();
            search.restore(view);
            if !prompt.text.is_empty() {
                let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] else {
                    bail!("search requires a diff")
                };
                if let Some(row) = diff.search(
                    view.location.diff_presentation,
                    &prompt.text,
                    search.selection.row(),
                    search.forward,
                )? {
                    view.location.diff_selection.set_head(Some(row));
                    view.location.scroll_mode = ScrollMode::FollowSelection;
                }
            }
            return Ok(());
        }
        if let Some(prompt) = &self.prompt
            && let PromptKind::Filter(kind) = prompt.kind
        {
            let view = prompt.target.view;
            let query = Query {
                kind,
                text: prompt.text.clone(),
            };
            if prompt.error.is_some() || self.views[&view].location.filter.query() == &query {
                return Ok(());
            }
            let buffer = self.views[&view].location.buffer;
            let result = if let Buffer::Items { display, .. } = &self.buffers[&buffer] {
                match zess_filter::Filter::compile(&query, zess_filter::Case::Insensitive) {
                    Ok(filter) => {
                        let target = self.views.get_mut(&view).unwrap();
                        target.location.filter = filter;
                        target.location.selection = ItemSelection::default();
                        target.location.scroll = 0;
                        target.project_items(display);
                        Ok(())
                    }
                    Err(error) => Err(error.into()),
                }
            } else {
                self.dispatch(view, "filter_changed", Value::Null)
            };
            match result {
                Ok(()) => {
                    if !matches!(self.buffers[&buffer], Buffer::Items { .. }) {
                        self.views.get_mut(&view).unwrap().location.filter =
                            zess_filter::Filter::compile(&query, zess_filter::Case::Sensitive)?;
                    }
                    self.prompt.as_mut().unwrap().error = None;
                }
                Err(error) => {
                    self.prompt.as_mut().unwrap().error = Some(format!("{error:#}"));
                }
            }
        }
        Ok(())
    }

    fn prompt_command(&mut self, command: PromptCommand) -> Result<()> {
        if matches!(command, PromptCommand::Cancel) {
            return self.cancel_prompt();
        }
        let Some(mut prompt) = self.prompt.take() else {
            return Ok(());
        };
        let command = if prompt.query().is_some() {
            match command {
                PromptCommand::CompleteLeft => PromptCommand::MoveLeft,
                PromptCommand::CompleteRight => PromptCommand::MoveRight,
                command => command,
            }
        } else {
            command
        };
        match command {
            PromptCommand::Cancel => {
                unreachable!("cancellation is handled before taking the prompt")
            }
            PromptCommand::Submit => {
                if let PromptKind::MiniBuffer { purpose, .. } = &prompt.kind {
                    let result = self.dispatch(
                        prompt.target.view,
                        "layer_result",
                        json!({"purpose":purpose,"text":prompt.text,"cancelled":false}),
                    );
                    if let Err(error) = &result {
                        prompt.error = Some(format!("{error:#}"));
                        self.prompt = Some(prompt);
                    }
                    return result;
                }
                match &prompt.kind {
                    PromptKind::Command => {
                        let command = if prompt.completion.is_none()
                            && !prompt.text.is_empty()
                            && !prompt.text.contains(char::is_whitespace)
                        {
                            self.command_candidates(&prompt.text)
                                .into_iter()
                                .next()
                                .map_or_else(|| prompt.text.clone(), |candidate| candidate.name)
                        } else {
                            prompt.text.clone()
                        };
                        self.command(&command)?;
                    }
                    PromptKind::Text(kind) => {
                        let buffer = prompt.target.buffer;
                        let effect = self
                            .buffers
                            .get_mut(&buffer)
                            .and_then(Buffer::text)
                            .context("prompt has no text buffer")?
                            .submit_prompt(prompt.target.view, *kind, &prompt.text);
                        let effect = match effect {
                            Ok(effect) => effect,
                            Err(error) => {
                                prompt.error = Some(format!("{error:#}"));
                                self.prompt = Some(prompt);
                                return Err(error);
                            }
                        };
                        self.text_effect(effect)?;
                    }
                    PromptKind::DiffSearch(search) => {
                        let view = self.views.get_mut(&prompt.target.view).unwrap();
                        let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer]
                        else {
                            bail!("search requires a diff")
                        };
                        if !prompt.text.is_empty() {
                            match diff.search(
                                view.location.diff_presentation,
                                &prompt.text,
                                search.selection.row(),
                                search.forward,
                            ) {
                                Ok(Some(row)) => {
                                    view.location.diff_selection.set_head(Some(row));
                                    view.location.scroll_mode = ScrollMode::FollowSelection;
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    prompt.error = Some(format!("{error:#}"));
                                    self.prompt = Some(prompt);
                                    return Err(error);
                                }
                            }
                        }
                        self.registers.set_search_pattern(prompt.text.clone());
                    }
                    PromptKind::Filter(_) => self.set_mode(prompt.target.view, "normal")?,
                    PromptKind::MiniBuffer { .. } => unreachable!(),
                }
                return Ok(());
            }
            PromptCommand::CompleteNext
            | PromptCommand::CompletePrevious
            | PromptCommand::CompleteLeft
            | PromptCommand::CompleteRight
            | PromptCommand::CompleteUp
            | PromptCommand::CompleteDown
                if matches!(prompt.kind, PromptKind::Command) =>
            {
                let columns = self.command_grid_columns(&prompt);
                let (backwards, steps) = match command {
                    PromptCommand::CompletePrevious | PromptCommand::CompleteLeft => (true, 1),
                    PromptCommand::CompleteUp => (true, columns),
                    PromptCommand::CompleteDown => (false, columns),
                    _ => (false, 1),
                };
                self.complete_command(&mut prompt, backwards, steps);
                prompt.cursor = prompt.text.len();
            }
            PromptCommand::DeleteBackward => {
                prompt.error = None;
                if prompt.cursor > 0 {
                    let start = prompt.text[..prompt.cursor]
                        .char_indices()
                        .last()
                        .map_or(0, |(index, _)| index);
                    prompt.text.drain(start..prompt.cursor);
                    prompt.cursor = start;
                }
                prompt.completion = None;
            }
            PromptCommand::Clear => {
                prompt.error = None;
                prompt.text.clear();
                prompt.cursor = 0;
                prompt.completion = None;
            }
            PromptCommand::MoveLeft => {
                prompt.cursor = prompt.text[..prompt.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(index, _)| index)
            }
            PromptCommand::MoveRight => {
                prompt.cursor += prompt.text[prompt.cursor..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8)
            }
            PromptCommand::MoveHome => prompt.cursor = 0,
            PromptCommand::MoveEnd => prompt.cursor = prompt.text.len(),
            PromptCommand::DeleteForward => {
                prompt.error = None;
                prompt.completion = None;
                let end = prompt.cursor
                    + prompt.text[prompt.cursor..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8);
                prompt.text.drain(prompt.cursor..end);
            }
            PromptCommand::ToggleGlob | PromptCommand::ToggleRegex => {
                if let Some((kind, _)) = prompt.query() {
                    let next = match command {
                        PromptCommand::ToggleGlob => zess_filter::QueryKind::Glob,
                        _ => zess_filter::QueryKind::Regex,
                    };
                    let next = if kind == next {
                        zess_filter::QueryKind::Plain
                    } else {
                        next
                    };
                    match &mut prompt.kind {
                        PromptKind::Filter(kind)
                        | PromptKind::MiniBuffer {
                            input: MiniBufferInput::Query(kind),
                            ..
                        } => *kind = next,
                        _ => unreachable!(),
                    }
                    prompt.error = None;
                    prompt.completion = None;
                }
            }
            _ => {}
        }
        self.prompt = Some(prompt);
        self.update_prompt()
    }

    fn set_mode(&mut self, view: ViewId, mode: &str) -> Result<()> {
        self.key_sequence.clear();
        self.cancel_prompt_for_view(view)?;
        let buffer = self.views[&view].location.buffer;
        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
            text.set_mode(view, mode)?;
        }
        let target = self.views.get_mut(&view).unwrap();
        target.jump = None;
        target.count.clear();
        target.register = RegisterInput::None;
        let was_filter = target.location.mode == ViewMode::Filter;
        target.location.mode = ViewMode::from_name(mode);
        target
            .location
            .memories
            .entry(mode.into())
            .or_insert_with(|| json!({}));
        if target.location.mode == ViewMode::Filter {
            let query = target.location.filter.query();
            let text = query.text.clone();
            self.prompt = Some(Prompt {
                kind: PromptKind::Filter(query.kind),
                cursor: text.len(),
                text,
                target: PromptTarget { view, buffer },
                completion: None,
                error: None,
            });
        }
        if was_filter || mode == "filter" {
            self.dispatch(view, "mode_changed", Value::Null)?;
        }
        Ok(())
    }

    fn effective_view_options(&self, id: ViewId, kind: &str) -> Value {
        let overrides = &self.views[&id].location.options;
        self.scripts.as_ref().map_or_else(
            || Value::Object(overrides.as_object().cloned().unwrap_or_default()),
            |runtime| runtime.view_options_with_overrides(kind, overrides.as_object()),
        )
    }

    fn dispatch(&mut self, id: ViewId, event: &str, argument: Value) -> Result<()> {
        self.dispatch_inner(id, event, argument, true)
    }

    fn observe_queries(&mut self, id: ViewId, queries: Vec<QueryWatch>) -> Result<()> {
        let mut desired = Vec::new();
        let mut added = Vec::new();
        for query in queries {
            if desired.contains(&query) {
                continue;
            }
            if !self.views[&id]
                .observations
                .iter()
                .any(|(existing, _)| existing == &query)
            {
                let observation = self
                    .services
                    .borrow_mut()
                    .observe(&query.operation, &query.argument)?;
                added.push((query.clone(), observation));
            }
            desired.push(query);
        }
        let mut old = std::mem::take(&mut self.views.get_mut(&id).unwrap().observations);
        let mut observations = Vec::new();
        for query in desired {
            let observation =
                if let Some(index) = old.iter().position(|(existing, _)| existing == &query) {
                    old.remove(index)
                } else {
                    let index = added
                        .iter()
                        .position(|(existing, _)| existing == &query)
                        .unwrap();
                    added.remove(index)
                };
            observations.push(observation);
        }
        self.views.get_mut(&id).unwrap().observations = observations;
        Ok(())
    }

    fn dispatch_inner(
        &mut self,
        id: ViewId,
        event: &str,
        argument: Value,
        propagate: bool,
    ) -> Result<()> {
        let Some(view) = self.views.get(&id) else {
            return Ok(());
        };
        let Buffer::Data { kind, .. } = &self.buffers[&view.location.buffer] else {
            return Ok(());
        };
        let kind = kind.clone();
        let buffer = view.location.buffer;
        let preview_area = self.preview_area(id);
        let mut input = PluginInput {
            preview_area,
            buffer_id: buffer,
            view_id: id,
            generation: self.generation,
            preview_debounce: self
                .scripts
                .as_ref()
                .context("plugin runtime unavailable; fix configuration and :config-reload")?
                .editor_options()
                .preview_debounce,
            event: event.into(),
            argument,
            query: self
                .prompts()
                .find(|prompt| prompt.target.view == id && prompt.target.buffer == buffer)
                .and_then(Prompt::query)
                .map(|(kind, text)| Query {
                    kind,
                    text: text.to_owned(),
                })
                .or_else(|| {
                    (event == "filter_changed").then(|| view.location.filter.query().clone())
                }),
            options: self.effective_view_options(id, &kind),
            mode: view.location.mode.as_str().into(),
            selection: view.location.selection.clone(),
            register: match &view.register {
                RegisterInput::Selected(register) => Some(*register),
                RegisterInput::None | RegisterInput::Selecting => None,
            },
            context: PluginContext::default(),
        };
        let fields = self
            .scripts
            .as_ref()
            .map(|runtime| runtime.context_fields(&kind, event))
            .unwrap_or_default();
        input.context = self.context(id, &fields);
        let output = self
            .scripts
            .as_mut()
            .context("plugin runtime unavailable; fix configuration and :config-reload")?
            .dispatch(&kind, &input)?;
        self.pending_operations.extend(output.pending_operations);
        let mut completions = output.completed_operations;
        let view = self.views.get_mut(&id).unwrap();
        if matches!(event, "init" | "view_entered") {
            view.location.config_generation = Some(self.generation);
        }
        let changed = output.changed;
        view.preview_area = preview_area;
        if let Buffer::Data { modified, .. } = self.buffers.get_mut(&buffer).unwrap() {
            if let Some(value) = output.modified {
                *modified = value;
            }
        }
        if let Some(display) = output.display {
            if view.location.display.header != display.header
                || !view
                    .location
                    .display
                    .rows
                    .iter()
                    .map(|row| &row.id)
                    .eq(display.rows.iter().map(|row| &row.id))
            {
                view.jump = None;
            }
            view.location.set_display(display);
        }
        if let Some(selection) = output.selection {
            view.location.selection = selection;
        }
        view.reconcile_selection();
        if let Some(queries) = output.watches {
            if let Err(error) = self.observe_queries(id, queries) {
                for completion in &mut completions {
                    completion.result = zess_plugin::OperationResult::Failed {
                        message: format!("{error:#}"),
                    };
                }
                self.complete_operations(completions);
                return Err(error);
            }
        }
        for request in output.requests {
            if !self
                .views
                .get(&id)
                .is_some_and(|view| view.location.buffer == buffer)
            {
                break;
            }

            if let Err(error) = self.request(id, request) {
                for completion in &mut completions {
                    completion.result = zess_plugin::OperationResult::Failed {
                        message: format!("{error:#}"),
                    };
                }
                self.complete_operations(completions);
                return Err(error);
            }
        }
        self.complete_operations(completions);
        if changed && propagate {
            let mut siblings = Vec::new();
            for (&other, view) in self.views.iter() {
                if other != id && view.location.buffer == buffer {
                    siblings.push(other);
                }
            }
            for sibling in siblings {
                self.dispatch_inner(sibling, "render", Value::Null, false)?;
            }
        }
        Ok(())
    }

    fn reinitialize_plugin_view(&mut self, id: ViewId) -> Result<()> {
        let Some(view) = self.views.get_mut(&id) else {
            return Ok(());
        };
        let buffer = view.location.buffer;
        let Buffer::Data { argument, .. } = &self.buffers[&buffer] else {
            return Ok(());
        };
        let argument = argument.clone();
        let selection = view.location.selection.clone();
        let filtered = !view.location.filter.is_empty();
        if self.views.child(id).is_none() {
            self.views.get_mut(&id).unwrap().location.memories.clear();
        }
        self.views.get_mut(&id).unwrap().location.mode = ViewMode::Normal;
        self.dispatch(id, "init", argument)?;
        if filtered
            && self
                .views
                .get(&id)
                .is_some_and(|view| view.location.buffer == buffer)
        {
            self.dispatch(id, "filter_changed", Value::Null)?;
        }
        if let Some(view) = self.views.get_mut(&id)
            && view.location.buffer == buffer
            && selection != view.location.selection
            && view
                .location
                .display
                .rows
                .iter()
                .any(|row| Some(&row.id) == selection.primary.as_ref())
        {
            view.location.selection = selection;
            view.reconcile_selection();
            self.dispatch(
                id,
                "select",
                json!(SelectionChange {
                    navigation: Some(ItemNavigation::Jump)
                }),
            )?;
        }
        Ok(())
    }

    fn enter_plugin_view(&mut self, id: ViewId, argument: Value) -> Result<()> {
        let location = &self.views[&id].location;
        if location
            .config_generation
            .is_some_and(|generation| generation != self.generation)
            && let Some(kind) = self.buffers[&location.buffer].plugin_kind()
            && self
                .scripts
                .as_ref()
                .is_some_and(|runtime| !runtime.is_native_buffer(kind))
        {
            return self.reinitialize_plugin_view(id);
        }
        self.dispatch(id, "view_entered", argument)
    }

    fn request(&mut self, view: ViewId, request: HostRequest) -> Result<()> {
        let page = self.views.page_id(view);
        match request {
            HostRequest::Navigate { navigation } => self.navigate(view, navigation)?,
            HostRequest::OpenSnapshot {
                snapshot,
                presentation,
            } => self.open_snapshot(view, snapshot, presentation)?,
            HostRequest::PreviewGroup {
                panes,
                layout,
                focus,
            } => self.show_preview_group(page, panes, layout, focus)?,
            HostRequest::PreviewPending => {
                let page = self.views.page_id(view);
                if let Some(previews) = self.views.previews_mut(page) {
                    previews.publication = pages::PreviewPublication::Pending;
                }
            }
            HostRequest::ConfigurePreview { header, key_scope } => {
                if let Some(previews) = self.views.previews_mut(page) {
                    for slot in &mut previews.members {
                        match &mut slot.content {
                            pages::SlotContent::Surface(surface) => surface.header = header.clone(),
                            pages::SlotContent::View { view, .. } => {
                                view.location.display.header = header.clone();
                                view.location.options["key-scope"] = json!(key_scope);
                            }
                        }
                    }
                }
            }
            HostRequest::Action { name, argument } => {
                let action = NativeAction::resolve(&name)?;
                self.native_command(view, action, argument)?;
            }
            HostRequest::BufferEvent {
                kind,
                event,
                argument,
            } => {
                let mut target = view;
                if self.buffers[&self.views[&view].location.buffer].is_preview() {
                    if let Some(source) = self.preview_source(view) {
                        target = source;
                    }
                }
                if !matches!(&self.buffers[&self.views[&target].location.buffer], Buffer::Data {kind:current,..} if *current==kind)
                {
                    bail!("command requires a {kind} buffer");
                }
                self.dispatch(target, &event, argument)?;
            }
            HostRequest::Schedule { after_ms } => {
                self.views.get_mut(&view).unwrap().wake_at =
                    Some(Instant::now() + Duration::from_millis(after_ms.clamp(10, 60_000)))
            }
            HostRequest::FocusText { buffer_id, line } => {
                if !matches!(
                    self.buffers.get(&buffer_id),
                    Some(Buffer::Text(_) | Buffer::Snapshot { .. })
                ) {
                    bail!("source text buffer no longer exists");
                }
                let existing = self.views.iter().find_map(|(&id, view)| {
                    if view.location.buffer != buffer_id || self.views.parent(view.id).is_some() {
                        return None;
                    }
                    Some(id)
                });
                if let Some(existing) = existing {
                    self.focus_view(existing)?;
                } else {
                    self.focus_view(view)?;
                    self.switch_buffer(buffer_id)?;
                }
                self.buffers
                    .get_mut(&buffer_id)
                    .and_then(Buffer::text)
                    .unwrap()
                    .goto_line(self.focus, line);
                self.remember_text();
            }
            HostRequest::Message { text } => self.message = text,
            HostRequest::Yank { values, register } => {
                let register = register.unwrap_or(match &self.views[&view].register {
                    RegisterInput::Selected(register) => *register,
                    RegisterInput::None | RegisterInput::Selecting => '"',
                });
                self.yank(values, register)?;
            }
            HostRequest::SetMode { mode } => self.set_mode(view, &mode)?,
            HostRequest::ConfigureView { options } => {
                let kind = self.buffers[&self.views[&view].location.buffer].plugin_kind();
                self.scripts
                    .as_ref()
                    .context("plugin runtime unavailable")?
                    .validate_view_options_value(kind, &options)?;
                let Value::Object(options) = options else {
                    unreachable!("view options were validated as an object");
                };
                self.views
                    .get_mut(&view)
                    .unwrap()
                    .location
                    .options
                    .as_object_mut()
                    .context("view options must be an object")?
                    .extend(options);
            }
            HostRequest::OpenFile { path, line } => {
                let path = self.plugin_file_path(&path)?;
                let existing = self.views.iter().find_map(|(&id, view)| {
                    if self.views.parent(view.id).is_some() {
                        return None;
                    }
                    match &self.buffers[&view.location.buffer] {
                        Buffer::Text(text) if text.path() == Some(path.as_path()) => Some(id),
                        _ => None,
                    }
                });
                let buffer = if let Some(existing) = existing {
                    self.focus_view(existing)?;
                    self.views[&existing].location.buffer
                } else {
                    self.ensure_buffer_switch_allowed(view)?;
                    let buffer = self.load_file(&path)?;
                    self.focus_view(view)?;
                    self.switch_buffer(buffer)?;
                    buffer
                };
                if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
                    text.goto_line(self.focus, line);
                }
                self.remember_text();
            }
            HostRequest::OpenWorktreeSource {
                path,
                line,
                presentation,
            } => self.open_worktree_source(view, path, line, presentation)?,
            HostRequest::FileLayer { path, line } => self.open_file_layer(view, &path, line)?,
            HostRequest::OpenBuffer { kind, argument } => {
                self.open_plugin(view, &kind, argument)?
            }
            HostRequest::Layer {
                purpose,
                text,
                inline,
            } => {
                if self.views.child(view).is_some() {
                    bail!("view already has a layer");
                }
                self.focus_view(view)?;
                self.cancel_input_context()?;
                let view = self.focus;
                anyhow::ensure!(self.views.child(view).is_none(), "view already has a layer");
                let text = TextBuffer::with_capabilities(
                    &text,
                    self.services.borrow().file_system(),
                    self.syntax_provider.clone(),
                );
                let buffer = self.add_buffer(Buffer::Text(text));
                let child = self.create_page(buffer);
                self.views.attach_layer(
                    child,
                    LayerAttachment {
                        parent: view,
                        content: LayerContent::Draft { purpose },
                        presentation: if inline {
                            LayerPresentation::Inline
                        } else {
                            LayerPresentation::Pane
                        },
                    },
                );
                self.focus_view(child)?;
                self.buffers
                    .get_mut(&buffer)
                    .and_then(Buffer::text)
                    .unwrap()
                    .execute_command(child, "text.insert")?;
            }
            HostRequest::MiniBuffer {
                purpose,
                text,
                key_scope,
            } => {
                anyhow::ensure!(
                    view == self.focus,
                    "minibuffer input requires the focused view"
                );
                let input = if purpose == "find" {
                    MiniBufferInput::Query(QueryKind::Plain)
                } else {
                    MiniBufferInput::Text
                };
                self.prompt = Some(Prompt {
                    kind: PromptKind::MiniBuffer {
                        purpose,
                        key_scope,
                        input,
                    },
                    cursor: text.len(),
                    text,
                    target: PromptTarget {
                        view,
                        buffer: self.views[&view].location.buffer,
                    },
                    completion: None,
                    error: None,
                });
                self.key_sequence.clear();
            }
            HostRequest::Preview {
                path,
                content,
                focus,
            } => self.show_preview(page, path, content, focus)?,
            HostRequest::Close => {
                self.close_view_from(view, false)?;
            }
        }
        Ok(())
    }

    fn open_plugin(&mut self, origin: ViewId, kind: &str, argument: Value) -> Result<()> {
        if self.views.page(origin).navigation.root_kind.as_deref() == Some(kind)
            && (argument.is_null() || argument.as_object().is_some_and(serde_json::Map::is_empty))
        {
            return Ok(());
        }
        let text_source = self.plugin_text_source(kind, self.views[&origin].location.buffer)?;
        let source = self.preview_source(origin).unwrap_or(origin);
        if origin == self.focus && source != origin {
            self.focus_view(source)?;
        }
        self.open_plugin_navigation(source, kind, argument, text_source)
    }

    fn split(&mut self, origin: ViewId, axis: Axis) -> Result<()> {
        if self.help.as_ref().is_some_and(|help| help.view == origin) {
            return self.help_command(HelpCommand::Pin);
        }
        self.ensure_buffer_switch_allowed(origin)?;
        let source = origin;
        self.ensure_buffer_switch_allowed(source)?;
        let location = self.views[&source].location.clone();
        let buffer = location.buffer;
        let target = self.create_page(buffer);
        if self.views.main(self.views.page_id(source)) == source {
            self.views.navigation_mut(target).root_kind =
                self.views.page(source).navigation.root_kind.clone();
        }
        let view = self.views.get_mut(&target).unwrap();
        view.location = location;
        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
            text.clone_view(source, target);
        }
        self.layout.as_mut().unwrap().split(
            self.views.page_id(source),
            self.views.page_id(target),
            axis,
        );
        self.focus_view(target)?;
        self.dispatch(target, "view_cloned", json!({"source_view":source}))?;
        Ok(())
    }

    fn focus_next(&mut self, delta: isize) -> Result<()> {
        let leaves = self
            .visible_rectangles()
            .into_iter()
            .filter_map(|(target, _)| match target {
                RenderTarget::View(id) => Some(id),
                RenderTarget::Surface { .. } => None,
            })
            .collect::<Vec<_>>();
        let mut root = self.focus;
        while !leaves.contains(&root)
            && let Some(parent) = self.views.parent(root)
        {
            root = parent;
        }
        let current = leaves.iter().position(|id| *id == root).unwrap_or(0);
        let mut target =
            leaves[(current as isize + delta).rem_euclid(leaves.len() as isize) as usize];
        while let Some(layer) = self.views.child(target) {
            target = layer;
        }
        self.focus_view(target)?;
        self.remember_text();
        Ok(())
    }

    fn focus_direction(&mut self, direction: ViewDirection) -> Result<()> {
        let rectangles = self
            .visible_rectangles()
            .into_iter()
            .filter_map(|(target, rect)| match target {
                RenderTarget::View(id) => Some((id, rect)),
                RenderTarget::Surface { .. } => None,
            })
            .collect::<Vec<_>>();
        let mut root = self.focus;
        while !rectangles.iter().any(|(id, _)| *id == root)
            && let Some(parent) = self.views.parent(root)
        {
            root = parent;
        }
        let Some((_, current)) = rectangles.iter().find(|(id, _)| *id == root) else {
            return Ok(());
        };
        let horizontal = matches!(direction, ViewDirection::Left | ViewDirection::Right);
        let target = rectangles
            .iter()
            .filter(|(id, rect)| {
                *id != root
                    && match direction {
                        ViewDirection::Left => rect.x + rect.width <= current.x,
                        ViewDirection::Right => rect.x >= current.x + current.width,
                        ViewDirection::Up => rect.y + rect.height <= current.y,
                        ViewDirection::Down => rect.y >= current.y + current.height,
                    }
            })
            .min_by_key(|(_, rect)| {
                let gap = match direction {
                    ViewDirection::Left => current.x - rect.x - rect.width,
                    ViewDirection::Right => rect.x - current.x - current.width,
                    ViewDirection::Up => current.y - rect.y - rect.height,
                    ViewDirection::Down => rect.y - current.y - current.height,
                };
                if horizontal {
                    (
                        gap,
                        (rect.y * 2 + rect.height).abs_diff(current.y * 2 + current.height),
                    )
                } else {
                    (
                        gap,
                        (rect.x * 2 + rect.width).abs_diff(current.x * 2 + current.width),
                    )
                }
            })
            .map(|(id, _)| *id);
        if let Some(mut target) = target {
            while let Some(layer) = self.views.child(target) {
                target = layer;
            }
            self.focus_view(target)?;
            self.remember_text();
        }
        Ok(())
    }

    fn close_view(&mut self, force: bool) -> Result<()> {
        if self.help.is_some() {
            return self.help_command(HelpCommand::Close);
        }
        let page = self.views.page_id(self.focus);
        if let Some(mut child) = self.views.layer_in_page(page) {
            while let Some(next) = self.views.child(child) {
                child = next;
            }
            self.focus_view(child)?;
            return self.close_view(force);
        }
        let id = self.views.main(page);
        if self.close_plugin_navigation(page, force)? {
            return Ok(());
        }
        let buffer = self.views[&id].location.buffer;
        if self.views.parent(id).is_none()
            && self.projected_client_workspace().buffers.contains(&buffer)
        {
            let action_id = self.id();
            self.client_actions.push(zess_protocol::ClientAction {
                id: action_id,
                kind: zess_protocol::ClientActionKind::Close {
                    buffer_id: buffer,
                    force,
                },
            });
            return Ok(());
        }
        let return_to = self.views[&id]
            .return_to
            .and_then(|target| self.views.resolve_focus(target))
            .map(|mut target| {
                while let Some(parent) = self.views.parent(target) {
                    target = parent;
                }
                target
            });
        let view = &self.views[&id];
        if (self.views.parent(view.id).is_none()
            || matches!(self.views.layer_content(view.id), Some(LayerContent::File)))
            && !force
            && matches!(&self.buffers[&view.location.buffer], Buffer::Text(text) if text.modified())
        {
            bail!("unsaved buffer; :w or :q!");
        }
        self.leave_page(page)?;
        let view = &self.views[&id];
        let buffer = view.location.buffer;
        if let Some(parent) = self.views.parent(view.id) {
            if matches!(self.views.layer_content(view.id), Some(LayerContent::File)) {
                self.views.detach_layer(id);
                self.retire_page(page)?;
                self.remove_file_watch(buffer);
                self.buffers.remove(&buffer);
                self.activate_view(parent);
                self.remember_text();
                return Ok(());
            }
            let purpose = match &self.views.layer_content(view.id) {
                Some(LayerContent::Draft { purpose }) => purpose.clone(),
                _ => unreachable!("a draft layer must have a purpose"),
            };
            let text = self
                .buffers
                .get_mut(&buffer)
                .and_then(Buffer::text)
                .unwrap()
                .text();
            self.views
                .get_mut(&parent)
                .unwrap()
                .location
                .memories
                .insert(format!("draft:{purpose}"), json!({"text":text}));
            if self.scripts.is_some() {
                self.dispatch(
                    parent,
                    "layer_result",
                    json!({"purpose":purpose,"text":text,"cancelled":force}),
                )?;
            } else if !force {
                bail!(
                    "plugin runtime unavailable; draft retained in the layer until :config-reload succeeds"
                );
            }
            self.views.detach_layer(id);
            self.retire_page(page)?;
            self.remove_file_watch(buffer);
            self.buffers.remove(&buffer);
            self.activate_view(parent);
            return Ok(());
        }
        self.retire_page(page)?;
        self.layout = self.layout.take().and_then(|layout| layout.remove(page));
        if self.layout.is_none() {
            let buffer = self
                .projected_client_workspace()
                .current
                .context("no workspace buffer to display")?;
            self.add_initial_view(buffer);
        } else {
            let mut leaves = Vec::new();
            self.layout.as_ref().unwrap().leaves(&mut leaves);
            let mut focus = return_to
                .filter(|id| {
                    self.views.contains_key(id) && leaves.contains(&self.views.page_id(*id))
                })
                .unwrap_or_else(|| self.first_layout_view());
            while let Some(layer) = self.views.child(focus) {
                focus = layer;
            }
            self.activate_view(focus);
            self.remember_text();
        }
        Ok(())
    }

    fn close_view_from(&mut self, origin: ViewId, force: bool) -> Result<()> {
        if origin == self.focus {
            return self.close_view(force);
        }
        let page = self.views.page_id(origin);
        if self.close_plugin_navigation(page, force)? {
            return Ok(());
        }
        let buffer = self.views[&origin].location.buffer;
        anyhow::ensure!(
            self.projected_client_workspace().buffers.contains(&buffer),
            "view is not a workspace buffer"
        );
        let id = self.id();
        self.client_actions.push(zess_protocol::ClientAction {
            id,
            kind: zess_protocol::ClientActionKind::Close {
                buffer_id: buffer,
                force,
            },
        });
        Ok(())
    }

    fn close_client_buffer(
        &mut self,
        action_id: u64,
        buffer: BufferId,
        replacement: Option<BufferId>,
        force: bool,
    ) -> Result<BufferId> {
        let index = self
            .client_actions
            .iter()
            .position(|action| {
                action.id == action_id
                    && action.kind
                        == (zess_protocol::ClientActionKind::Close {
                            buffer_id: buffer,
                            force,
                        })
            })
            .context("close request is no longer pending")?;
        self.client_actions.remove(index);
        anyhow::ensure!(
            self.client_workspace.buffers.contains(&buffer),
            "buffer is no longer in the workspace"
        );
        if !force
            && match self.buffers.get(&buffer) {
                Some(Buffer::Text(text)) => text.modified(),
                Some(Buffer::Data { modified, .. }) => *modified,
                _ => false,
            }
        {
            bail!("unsaved buffer; :w or :q!");
        }
        let replacement = match replacement {
            Some(id) => {
                anyhow::ensure!(
                    id != buffer && self.client_workspace.buffers.contains(&id),
                    "replacement buffer is not in the workspace"
                );
                id
            }
            None => {
                anyhow::ensure!(
                    self.client_workspace.buffers.len() == 1,
                    "replacement buffer is required"
                );
                let text = TextBuffer::with_capabilities(
                    "",
                    self.services.borrow().file_system(),
                    self.syntax_provider.clone(),
                );
                let id = self.add_buffer(Buffer::Text(text));
                self.client_workspace.buffers.push(id);
                id
            }
        };
        let views: Vec<_> = self
            .views
            .values()
            .filter(|view| {
                view.location.buffer == buffer
                    && self.views.parent(view.id).is_none()
                    && self.views.main(self.views.page_id(view.id)) == view.id
            })
            .map(|view| view.id)
            .collect();
        for view in views {
            self.focus_view(view)?;
            self.switch_buffer(replacement)?;
        }
        let pages: Vec<_> = self.views.pages().map(|page| page.main.id).collect();
        for page in pages {
            self.views
                .navigation_mut(page)
                .replace_buffer(buffer, replacement);
        }
        let text_replacement =
            matches!(self.buffers.get(&replacement), Some(Buffer::Text(_))).then_some(replacement);
        for value in self.buffers.values_mut() {
            if let Buffer::Data { text_source, .. } = value
                && *text_source == Some(buffer)
            {
                *text_source = text_replacement;
            }
        }
        self.client_workspace.buffers.retain(|id| *id != buffer);
        self.client_workspace.current = Some(replacement);
        self.client_actions.retain(|action| !matches!(action.kind, zess_protocol::ClientActionKind::Close { buffer_id, .. } if buffer_id == buffer));
        if self.last_text == Some(buffer) {
            self.last_text = text_replacement;
        }
        self.remove_file_watch(buffer);
        self.buffers.remove(&buffer);
        Ok(replacement)
    }

    fn projected_client_workspace(&self) -> zess_protocol::ClientWorkspace {
        let mut workspace = self.client_workspace.clone();
        for action in &self.client_actions {
            workspace.apply(&action.kind);
        }
        workspace
    }

    fn client_exit(&mut self, action_id: u64, buffer: BufferId) -> Result<()> {
        let index = self.client_actions.iter().position(|action| action.id == action_id && matches!(action.kind, zess_protocol::ClientActionKind::Close { buffer_id, .. } if buffer_id == buffer)).context("quit request is no longer pending")?;
        self.client_actions.remove(index);
        self.client_actions.retain(|action| !matches!(action.kind, zess_protocol::ClientActionKind::Close { buffer_id, .. } if buffer_id == buffer));
        Ok(())
    }

    fn refresh_context_plugins(&mut self) -> Result<()> {
        let Some(scripts) = &self.scripts else {
            return Ok(());
        };
        let mut ids = Vec::new();
        for (&id, view) in self.views.iter() {
            let Buffer::Data { kind, .. } = &self.buffers[&view.location.buffer] else {
                continue;
            };
            let fields = scripts.context_fields(kind, "refresh");
            if fields.iter().any(|field| field.starts_with("text.")) {
                ids.push(id);
            }
        }
        for id in ids {
            self.dispatch(id, "refresh", Value::Null)?;
        }
        Ok(())
    }

    fn plugin_buffers(&self) -> impl Iterator<Item = (&str, bool)> {
        self.buffers.values().filter_map(|buffer| match buffer {
            Buffer::Data { kind, modified, .. } => Some((kind.as_str(), *modified)),
            _ => None,
        })
    }

    fn build_runtime(&self, with_config: bool) -> Result<ScriptRuntime> {
        let host = RuntimeHost {
            services: self.services.clone(),
            jobs: RefCell::new(HashSet::new()),
        };
        let mut runtime = ScriptRuntime::with_capabilities(
            move |operation, argument| host.call(&operation, argument),
            self.identities.clone(),
            self.services.borrow().file_system(),
            self.script_compiler.clone(),
        )?;
        for (id, description) in commands::command_infos() {
            runtime.register_native_action(id, description);
        }
        for (id, description) in commands::command_line_infos() {
            runtime.register_native_command(id, description);
        }
        for (alias, name) in commands::ALIASES {
            runtime.register_command_alias(alias, name);
        }
        self.builtins.register(&mut runtime);
        runtime.load_builtin_plugins()?;
        if with_config && let Some(config) = &self.config {
            if let Some(directory) = config.parent() {
                runtime.preload_modules(directory)?;
            }
            runtime.load(config)?;
        }
        runtime.validate_bindings()?;
        runtime.validate_view_options()?;
        runtime.validate_statusline_layouts()?;
        for (kind, modified) in self.plugin_buffers() {
            if !runtime.has_buffer_kind(kind) {
                bail!(
                    "configuration removes the {kind} buffer plugin; close its pages before reloading"
                );
            }
            if let Some(current) = &self.scripts
                && (current.is_native_buffer(kind) || runtime.is_native_buffer(kind))
                && !current.shares_native_buffer(&runtime, kind)
            {
                if modified {
                    bail!(
                        "save or undo pending {kind} buffer changes, then close its pages before switching plugins"
                    );
                }
                bail!(
                    "configuration changes the {kind} buffer plugin; close its pages before reloading"
                );
            }
        }
        let locations = self.views.values().map(|view| &view.location).chain(
            self.views
                .pages()
                .flat_map(|page| page.navigation.locations()),
        );
        for location in locations {
            let kind = self.buffers[&location.buffer].plugin_kind();
            if let Some(kind) = kind {
                let overrides = location
                    .options
                    .as_object()
                    .context("view options must be an object")?;
                let options = runtime.view_options_with_overrides(kind, Some(overrides));
                runtime.validate_view_options_value(Some(kind), &options)?;
            } else {
                runtime.validate_view_options_value(None, &location.options)?;
            }
        }
        let mut modes = vec![
            "normal".to_owned(),
            "select".into(),
            "insert".into(),
            "prompt".into(),
            "help".into(),
        ];
        modes.extend(
            runtime
                .modes()
                .into_iter()
                .map(|mode| self.identities.mode_name(mode.name).to_string()),
        );
        for mode in &modes {
            runtime.validate_bindings_for(&["global", "text"], mode)?;
            runtime.validate_bindings_for(&["global", "text", "preview"], mode)?;
            runtime.validate_bindings_for(&["global", "preview", "diff"], mode)?;
            runtime.validate_bindings_for(&["global", "preview", "preview-rows"], mode)?;
            runtime.validate_bindings_for(&["global", "data", "help"], mode)?;
            runtime.validate_bindings_for(&["global", "minibuffer"], mode)?;
            for kind in runtime.buffer_kinds() {
                runtime.validate_bindings_for(&["global", "data", &kind], mode)?;
            }
        }
        Ok(runtime)
    }

    fn reload(&mut self) -> Result<()> {
        let runtime = self.build_runtime(true).context("reload failed")?;
        self.close_help_overlay().context("reload failed")?;
        self.invalidate_mouse();
        self.scripts = Some(runtime);
        self.generation += 1;
        self.key_sequence.clear();
        self.refresh_help_buffers();
        for view in self.views.values_mut() {
            view.wake_at = None;
            view.observations.clear();
        }
        Ok(())
    }

    pub fn command(&mut self, command: &str) -> Result<()> {
        self.message.clear();
        let (name, argument) = command
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((command.trim(), ""));
        if name.is_empty() {
            return Ok(());
        }
        let result = self
            .scripts
            .as_ref()
            .context("plugin runtime unavailable")?
            .resolve_command_line(name)
            .and_then(|name| self.execute_command(&name, json!({"argument":argument.trim()})));
        if result.is_err() && self.active_prompt().is_none() {
            self.cancel_text_command();
        }
        result
    }

    fn execute_command(&mut self, name: &str, argument: Value) -> Result<()> {
        let owner = self.focus;
        self.views.get_mut(&self.focus).unwrap().jump = None;
        let result = (|| -> Result<bool> {
            let result = self
                .scripts
                .as_mut()
                .context("plugin runtime unavailable")?
                .command(name, &argument)?;
            Ok(match result {
                CommandOutput::Request(HostRequest::Action { name, argument }) => {
                    let action = NativeAction::resolve(&name)?;
                    if !matches!(
                        action,
                        NativeAction::Text(_)
                            | NativeAction::Prompt(_)
                            | NativeAction::Editor(commands::EditorCommand::Help)
                    ) {
                        self.cancel_text_command();
                    }
                    self.native_command(self.focus, action, argument)?;
                    matches!(
                        action,
                        NativeAction::Count(_) | NativeAction::Data(DataCommand::SelectRegister)
                    )
                }
                CommandOutput::Request(request) => {
                    self.cancel_text_command();
                    self.request(self.focus, request)?;
                    false
                }
                CommandOutput::Value(value) => {
                    self.cancel_text_command();
                    if !value.is_null() {
                        self.message = value.to_string();
                    }
                    false
                }
            })
        })();
        if !matches!(result, Ok(true))
            && let Some(view) = self.views.get_mut(&owner)
        {
            view.count.clear();
            view.register = RegisterInput::None;
        }
        result.map(|_| ())
    }

    pub fn frame(&mut self) -> Frame {
        self.presentation_regions.clear();
        if let Err(error) = self.invalidate_prompt() {
            self.message = format!("prompt: {error:#}");
        }
        self.synchronize_key_sequence();
        let previous_regions = std::mem::take(&mut self.mouse_regions);
        let previous_command_mouse = self.command_mouse.take();
        let width = usize::from(self.width);
        let height = usize::from(self.height.saturating_sub(1));
        let rectangles = self.visible_rectangles();
        let mut rows = (0..height).map(|_| FrameRow::default()).collect::<Vec<_>>();
        let mut cursor = None;
        for (target, rect) in rectangles {
            let (pane, position) = match target {
                RenderTarget::View(id) => {
                    self.render_view_at(id, rect.width, rect.height, (rect.x, rect.y))
                }
                RenderTarget::Surface { page, slot } => {
                    (self.render_surface(page, &slot, rect), None)
                }
            };
            for (offset, row) in pane.into_iter().enumerate().take(rect.height) {
                let output = &mut rows[rect.y + offset].spans;
                let used = output
                    .iter()
                    .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                    .sum::<usize>();
                if used < rect.x {
                    output.push(Span {
                        base_style: None,
                        text: format!("{}│", " ".repeat(rect.x - used - 1)),
                        style: "window".into(),
                    });
                }
                output.extend(clip_spans(row.spans, rect.width));
            }
            if let Some((x, y)) = position {
                cursor = Some(((rect.x + x as usize) as u16, (rect.y + y as usize) as u16));
            }
        }
        let input = self.active_prompt().map(Prompt::render_input);
        let mut prompt = input
            .as_ref()
            .map(|(text, _)| text.clone())
            .unwrap_or_else(|| self.pending_input());
        if let Some((_, mut position)) = input {
            while position >= width && !prompt.is_empty() {
                let ch = prompt.chars().next().unwrap();
                prompt.drain(..ch.len_utf8());
                position = position.saturating_sub(ch.width().unwrap_or(0));
            }
            cursor = Some((position as u16, self.height - 1));
        }
        if self.help.is_some() {
            self.mouse_regions.clear();
        }
        self.record_command_mouse();
        self.render_help(&mut rows, width);
        if self
            .active_prompt()
            .is_some_and(|prompt| !matches!(prompt.kind, PromptKind::Command))
        {
            self.presentation_regions = std::mem::take(&mut self.mouse_regions);
        }
        if self.mouse_regions.len() != previous_regions.len()
            || self
                .mouse_regions
                .iter()
                .zip(&previous_regions)
                .any(|(current, previous)| {
                    (&current.target, current.buffer, current.pane, current.body)
                        != (
                            &previous.target,
                            previous.buffer,
                            previous.pane,
                            previous.body,
                        )
                })
        {
            self.mouse_context.layout_revision = self.mouse_context.layout_revision.wrapping_add(1);
        }
        if self.mouse_regions != previous_regions || self.command_mouse != previous_command_mouse {
            self.mouse_context.content_revision =
                self.mouse_context.content_revision.wrapping_add(1);
        }
        if let Some(error) = self
            .active_prompt()
            .and_then(|prompt| prompt.error.as_ref())
            && let Some(row) = rows.last_mut()
        {
            let text = if UnicodeWidthStr::width(error.as_str()) > width {
                let mut used = 1;
                let tail = error
                    .chars()
                    .rev()
                    .take_while(|ch| {
                        used += ch.width().unwrap_or(0);
                        used <= width
                    })
                    .collect::<String>();
                format!("…{}", tail.chars().rev().collect::<String>())
            } else {
                error.clone()
            };
            row.spans = vec![Span {
                base_style: None,
                text: fit(&text, width),
                style: "error".into(),
            }];
        }
        if (self.help.is_some() && self.active_prompt().is_none())
            || !self.key_sequence.keys().is_empty()
        {
            cursor = None;
        }
        let mut styles = self.services.borrow().git_styles();
        for buffer in self.buffers.values() {
            if let Buffer::PreviewDiff { diff, .. } = buffer
                && let Some(rendered) = &diff.rendered
            {
                styles.extend(rendered.styles.clone());
            }
        }
        Frame {
            mouse_context: self.mouse_context,
            mouse_enabled: self.mouse_enabled(),
            width: self.width,
            height: self.height,
            rows,
            styles,
            cursor,
            cursor_shape: if self.active_prompt().is_some() {
                CursorShape::Bar
            } else {
                let view = &self.views[&self.focus];
                let mode = match &self.buffers[&view.location.buffer] {
                    Buffer::Text(text)
                    | Buffer::Snapshot { text, .. }
                    | Buffer::PreviewText { text, .. } => text.view(self.focus).mode(),
                    Buffer::Data { .. }
                    | Buffer::Items { .. }
                    | Buffer::PreviewRows { .. }
                    | Buffer::PreviewDiff { .. } => view.location.mode.as_str(),
                };
                match mode {
                    "insert" => CursorShape::Bar,
                    "select" => CursorShape::Underline,
                    _ => CursorShape::Block,
                }
            },
            status: self.message.clone(),
            prompt,
        }
    }
}

fn fit(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        if grapheme.chars().any(char::is_control) {
            continue;
        }
        let size = UnicodeWidthStr::width(grapheme);
        if used + size > width {
            break;
        }
        output.push_str(grapheme);
        used += size;
    }
    output.push_str(&" ".repeat(width - used));
    output
}

fn clip_spans(spans: Vec<Span>, width: usize) -> Vec<Span> {
    let mut left = width;
    let mut output = Vec::new();
    for span in spans {
        if left == 0 {
            break;
        }
        let length = UnicodeWidthStr::width(span.text.as_str());
        if length <= left {
            left -= length;
            output.push(span);
        } else {
            output.push(Span {
                base_style: span.base_style,
                text: fit(&span.text, left),
                style: span.style,
            });
            left = 0;
        }
    }
    if left > 0 {
        output.push(Span {
            base_style: None,
            text: " ".repeat(left),
            style: String::new(),
        });
    }
    output
}
