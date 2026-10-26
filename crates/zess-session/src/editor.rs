use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
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
use zess_script::{CommandOutput, KeyResolution, ScriptRuntime};
use zess_services::{NativeServices, Observation};
use zess_text::{
    ClipboardRead, InsertHistory, Registers, TEXT_COMMANDS, TextBuffer, TextCommand, TextEffect,
    TextMode, TextPromptKind,
};
use zess_ui::{CursorShape, Display, DisplayRow, Frame, FrameRow, Span};

mod commands;
mod context;
mod diff;
mod files;
mod items;
mod mouse;
mod navigation;
mod pages;
mod preview;
mod render;
mod status;

mod ui;
use commands::{DataCommand, HelpCommand, NativeAction, PromptCommand};
use pages::{PageId, RenderTarget};
use ui::{Completion, Help};

enum Buffer {
    Text(TextBuffer),
    Data {
        kind: String,
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
        kind: String,
        path: String,
        display: Display,
        help_context: Option<ui::HelpContext>,
    },
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
    fn is_preview(&self) -> bool {
        match self {
            Self::PreviewText { .. } | Self::PreviewDiff { .. } | Self::PreviewRows { .. } => true,
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

#[derive(PartialEq)]
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
    mode: ViewMode,
    memories: HashMap<String, Value>,
    filter: zess_filter::Filter,
    options: Value,
    display: Display,
    columns: zess_ui::RowColumns,
    selection: ItemSelection,
    diff_selection: diff::Selection,
    statusline: Vec<Span>,
    scroll: usize,
    horizontal_scroll: usize,
    manual_viewport: bool,
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
            mode: ViewMode::Normal,
            memories: HashMap::new(),
            filter: zess_filter::Filter::default(),
            options: json!({}),
            display: Display::default(),
            columns: zess_ui::RowColumns::default(),
            selection: ItemSelection::default(),
            diff_selection: diff::Selection::default(),
            statusline: Vec::new(),
            scroll: 0,
            horizontal_scroll: 0,
            manual_viewport: false,
        }
    }
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
    parent: Option<ViewId>,
    return_to: Option<pages::FocusTarget>,
    layer: Option<ViewId>,
    layer_content: Option<LayerContent>,
    presentation: LayerPresentation,
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
            parent: None,
            return_to: None,
            layer: None,
            layer_content: None,
            presentation: LayerPresentation::Pane,
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

enum PromptKind {
    Command,
    Text(TextPromptKind),
    DiffSearch(diff::Search),
    Filter(QueryKind),
    MiniBuffer {
        purpose: String,
        key_scope: Option<String>,
        query_kind: Option<QueryKind>,
    },
}

struct Prompt {
    kind: PromptKind,
    text: String,
    view: ViewId,
    completion: Option<Completion>,
    cursor: usize,
    error: Option<String>,
}

impl Prompt {
    fn query(&self) -> Option<(zess_filter::QueryKind, &str)> {
        match &self.kind {
            PromptKind::Filter(kind)
            | PromptKind::MiniBuffer {
                query_kind: Some(kind),
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
    views: pages::Store,
    layout: Option<Layout>,
    focus: ViewId,
    next_id: u64,
    last_text: Option<BufferId>,
    git_patch: Option<preview::PendingPatch>,
    pending_diff_source: Option<preview::PendingDiffSource>,
    prompt: Option<Prompt>,
    help: Option<Help>,
    pending: Vec<String>,
    key_state: KeyState,
    clipboard_writes: Vec<String>,
    primary_clipboard_writes: Vec<String>,
    clipboard_read: Option<(u64, ViewId, BufferId, ClipboardRead)>,
    message: String,
    width: u16,
    height: u16,
    exiting: bool,
    mouse_regions: Vec<mouse::Region>,
    mouse_context: zess_ui::MouseContext,
    mouse_drag: Option<ViewId>,
}

impl Session {
    pub fn new(workspace: PathBuf, files: &[PathBuf], config: Option<PathBuf>) -> Result<Self> {
        let workspace = workspace.canonicalize().context("open workspace")?;
        let services = Rc::new(RefCell::new(NativeServices::new(workspace.clone())?));
        let mut session = Self {
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
            views: pages::Store::default(),
            layout: None,
            focus: 0,
            next_id: 1,
            last_text: None,
            git_patch: None,
            pending_diff_source: None,
            prompt: None,
            help: None,
            pending: Vec::new(),
            key_state: KeyState::default(),
            clipboard_writes: Vec::new(),
            primary_clipboard_writes: Vec::new(),
            clipboard_read: None,
            message: String::new(),
            width: 80,
            height: 24,
            exiting: false,
            mouse_regions: Vec::new(),
            mouse_context: zess_ui::MouseContext::default(),
            mouse_drag: None,
        };

        if session.reload() {
            session.message.clear();
        }

        let (directory, files) = match files.split_first() {
            Some((path, files)) if session.absolute(path).is_dir() => (Some(path), files),
            _ => (None, files),
        };
        if files.is_empty() {
            let buffer = session.add_buffer(Buffer::Text(TextBuffer::new("")));
            session.add_initial_view(buffer);
        } else {
            for path in files {
                let buffer = session.load_file(path)?;
                if session.layout.is_none() {
                    session.add_initial_view(buffer);
                }
            }
        }
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

    pub fn tick(&mut self) -> bool {
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
        geometry_changed
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
        let (directory, paths) = match paths.split_first() {
            Some((path, paths)) if self.absolute(Path::new(path)).is_dir() => (Some(path), paths),
            _ => (None, paths),
        };
        // Keep the tree focused after loading any accompanying files.
        for path in paths.iter().chain(directory) {
            let result = (|| -> Result<()> {
                let path = Path::new(path);
                if self.absolute(path).is_dir() {
                    self.open_directory(path)
                } else {
                    while self.views[&self.focus].parent.is_some() {
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
            && view.layer_content.is_none()
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
            && !self
                .views
                .values()
                .any(|view| view.location.buffer == id && view.layer_content.is_some())
            && !self.views.pages().any(|page| {
                page.navigation.plugin.as_ref().is_some_and(|plugin| {
                    plugin.buffers.contains(&id)
                        || (plugin.origin.buffer == id
                            && matches!(page.main.layer_content, Some(LayerContent::File)))
                })
            })
    }

    fn load_file(&mut self, path: &Path) -> Result<BufferId> {
        let path = self.absolute(path);
        let path = path.canonicalize().unwrap_or(path);
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
            .absolute(path)
            .canonicalize()
            .context("open directory")?;
        let path = path
            .strip_prefix(&self.workspace)
            .context("directory must remain within the session workspace")?;
        let argument = json!({"path":path.to_string_lossy()});
        while self.views[&self.focus].parent.is_some() {
            self.close_view(false)?;
        }
        self.open_plugin("files", argument)
    }

    fn switch_buffer(&mut self, buffer: BufferId) -> Result<()> {
        self.switch_buffer_with(buffer, Value::Null)
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
        if self.views[&source].parent.is_some() {
            bail!("close the current layer before switching buffers");
        }
        Ok(())
    }

    fn switch_buffer_with(&mut self, buffer: BufferId, argument: Value) -> Result<()> {
        self.ensure_buffer_switch_allowed(self.focus)?;
        if !self.is_switchable_buffer(buffer) {
            bail!("buffer {buffer} is not a selectable editor buffer");
        }
        self.cancel_input_context()?;
        let origin = self.focus;
        if self.buffers[&self.views[&origin].location.buffer].is_preview()
            && let Some(source) = self.preview_source(origin)
        {
            self.focus_view(source)?;
        }
        self.close_plugin_navigation(self.views.page_id(self.focus), false)?;
        self.leave_page(self.views.page_id(self.focus))?;
        *self.views.navigation_mut(self.focus) = navigation::History::default();
        let retain_text = matches!(self.buffers[&buffer], Buffer::Data { .. });
        let view = self.views.get_mut(&self.focus).context("no active view")?;
        let previous = view.location.buffer;

        if let Some(text) = self
            .buffers
            .get_mut(&view.location.buffer)
            .and_then(Buffer::text)
        {
            if retain_text {
                text.set_text_mode(self.focus, TextMode::Normal)?;
            } else {
                text.remove_view(self.focus);
            }
        }
        view.replace_location(ViewLocation::new(buffer));
        if let Buffer::Items { display, .. } = &self.buffers[&buffer] {
            view.project_items(display);
        } else if let Buffer::PreviewRows { display, .. } = &self.buffers[&buffer] {
            view.location.set_display(display.clone());
        }
        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
            text.ensure_view(self.focus);
        }
        self.remove_unused_preview_buffer(previous);
        self.remember_text();
        self.dispatch(self.focus, "init", argument)?;
        Ok(())
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
        self.cancel_text_command();
        self.views.get_mut(&self.focus).unwrap().register = RegisterInput::None;
        self.views.get_mut(&self.focus).unwrap().jump = None;
        self.views.get_mut(&self.focus).unwrap().count.clear();
        self.pending.clear();
        self.close_help_overlay();
        self.prompt_command(PromptCommand::Cancel)
    }

    fn fullscreen_layer(&self) -> Option<ViewId> {
        let mut id = match &self.help {
            Some(help) => self.views.resolve_focus(help.source)?,
            None => self.focus,
        };
        id = self.preview_source(id).unwrap_or(id);
        loop {
            let view = self.views.get(&id)?;
            if view.presentation == LayerPresentation::Fullscreen {
                return Some(id);
            }
            id = view.parent?;
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
                let Some(parent) = self.views[&target].parent else {
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
        }
        self.activate_view(view);
        Ok(())
    }

    fn activate_view(&mut self, view: ViewId) {
        self.focus = view;
        self.views.activate_preview(view);
    }

    fn invalidate_text_prompt(&mut self) {
        if let Some(prompt) = &self.prompt
            && matches!(prompt.kind, PromptKind::DiffSearch(_))
            && self.views.get(&prompt.view).is_none_or(|view| {
                !matches!(
                    self.buffers[&view.location.buffer],
                    Buffer::PreviewDiff { .. }
                )
            })
        {
            self.prompt = None;
            self.pending.clear();
            return;
        }

        let Some(Prompt {
            kind: PromptKind::Text(kind),
            view: id,
            ..
        }) = &self.prompt
        else {
            return;
        };
        let Some(view) = self.views.get(id) else {
            self.prompt = None;
            self.pending.clear();
            return;
        };
        let buffer = &self.buffers[&view.location.buffer];
        let pending = match buffer {
            Buffer::Text(text)
            | Buffer::Snapshot { text, .. }
            | Buffer::PreviewText { text, .. } => text.pending_prompt(*id),
            _ => None,
        };
        if pending != Some(*kind) {
            self.prompt = None;
            self.pending.clear();
        }
    }

    fn active_prompt(&self) -> Option<&Prompt> {
        self.prompt
            .as_ref()
            .filter(|prompt| prompt.view == self.focus)
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

    fn text_effect(&mut self, effect: TextEffect) -> Result<()> {
        if let Some(text) = effect.clipboard {
            self.clipboard_writes.push(text);
        }
        if let Some(text) = effect.primary_clipboard {
            self.primary_clipboard_writes.push(text);
        }
        if let Some(read) = effect.clipboard_read {
            let id = self.id();
            self.clipboard_read = Some((
                id,
                self.focus,
                self.views[&self.focus].location.buffer,
                read,
            ));
        }
        if let Some(message) = effect.message {
            self.message = message;
        }
        if let Some(prompt) = effect.prompt {
            self.prompt = Some(Prompt {
                kind: PromptKind::Text(prompt),
                text: String::new(),
                view: self.focus,
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

    fn diff_hunk(&mut self, forward: bool) -> Result<()> {
        self.move_diff(forward, true)
    }

    fn update_prompt(&mut self) -> Result<()> {
        if let Some(prompt) = &self.prompt
            && let PromptKind::MiniBuffer { purpose, .. } = &prompt.kind
        {
            let query = prompt.query().is_some();
            let result = self.dispatch(
                prompt.view,
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
            let buffer = self.views[&prompt.view].location.buffer;
            let effect = self
                .buffers
                .get_mut(&buffer)
                .and_then(Buffer::text)
                .context("prompt has no text buffer")?
                .preview_prompt(prompt.view, *kind, &prompt.text)?;
            return self.text_effect(effect);
        }
        if let Some(prompt) = &self.prompt
            && let PromptKind::DiffSearch(search) = &prompt.kind
        {
            let view = self.views.get_mut(&prompt.view).unwrap();
            search.restore(view);
            if !prompt.text.is_empty() {
                let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] else {
                    bail!("search requires a diff")
                };
                if let Some(row) =
                    diff.search(&prompt.text, search.selection.row(), search.forward)?
                {
                    view.location.diff_selection.set_head(Some(row));
                    view.location.manual_viewport = false;
                }
            }
            return Ok(());
        }
        if let Some(prompt) = &self.prompt
            && let PromptKind::Filter(kind) = prompt.kind
        {
            let view = prompt.view;
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
                if let PromptKind::MiniBuffer { purpose, .. } = &prompt.kind {
                    return self.dispatch(
                        prompt.view,
                        "layer_result",
                        json!({"purpose":purpose,"text":prompt.text,"cancelled":true}),
                    );
                }
                if let PromptKind::DiffSearch(search) = &prompt.kind {
                    if let Some(view) = self.views.get_mut(&prompt.view) {
                        search.restore(view);
                    }
                } else if matches!(prompt.kind, PromptKind::Filter(_)) {
                    self.set_mode(prompt.view, "normal")?;
                } else if matches!(prompt.kind, PromptKind::Text(_)) {
                    let buffer = self.views[&prompt.view].location.buffer;
                    let text = self
                        .buffers
                        .get_mut(&buffer)
                        .and_then(Buffer::text)
                        .context("prompt has no text buffer")?;
                    text.cancel_pending_command(prompt.view);
                }
                return Ok(());
            }
            PromptCommand::Submit => {
                if let PromptKind::MiniBuffer { purpose, .. } = &prompt.kind {
                    let result = self.dispatch(
                        prompt.view,
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
                    PromptKind::Command => self.command(&prompt.text)?,
                    PromptKind::Text(kind) => {
                        let buffer = self.views[&prompt.view].location.buffer;
                        let effect = self
                            .buffers
                            .get_mut(&buffer)
                            .and_then(Buffer::text)
                            .context("prompt has no text buffer")?
                            .submit_prompt(prompt.view, *kind, &prompt.text);
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
                        let view = self.views.get_mut(&prompt.view).unwrap();
                        let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer]
                        else {
                            bail!("search requires a diff")
                        };
                        if !prompt.text.is_empty() {
                            match diff.search(&prompt.text, search.selection.row(), search.forward)
                            {
                                Ok(Some(row)) => {
                                    view.location.diff_selection.set_head(Some(row));
                                    view.location.manual_viewport = false;
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
                    PromptKind::Filter(_) => self.set_mode(prompt.view, "normal")?,
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
                            query_kind: Some(kind),
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
        if self
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.view == view)
        {
            self.cancel_input_context()?;
        }
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
                view,
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
        let mut options = self
            .scripts
            .as_ref()
            .map(|runtime| runtime.view_options(kind))
            .unwrap_or_else(|| json!({}));
        if let Some(local) = self.views[&id].location.options.as_object() {
            for (name, value) in local {
                options[name] = value.clone();
            }
        }
        options
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
                .prompt
                .as_ref()
                .filter(|prompt| prompt.view == id)
                .and_then(Prompt::query)
                .map(|(kind, text)| Query {
                    kind,
                    text: text.to_owned(),
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
        let view = self.views.get_mut(&id).unwrap();
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
            self.observe_queries(id, queries)?;
        }
        for request in output.requests {
            if !self
                .views
                .get(&id)
                .is_some_and(|view| view.location.buffer == buffer)
            {
                break;
            }

            self.request(id, request)?;
        }
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
                    if view.location.buffer != buffer_id || view.parent.is_some() {
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
                let count = values.len();
                let register = register.unwrap_or(match &self.views[&view].register {
                    RegisterInput::Selected(register) => *register,
                    RegisterInput::None | RegisterInput::Selecting => '"',
                });
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
                self.text_effect(effect)?;
            }
            HostRequest::SetMode { mode } => self.set_mode(view, &mode)?,
            HostRequest::ConfigureView { options } => {
                self.scripts
                    .as_ref()
                    .context("plugin runtime unavailable")?
                    .validate_view_options_value(&options)?;
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
                    if view.parent.is_some() {
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
            HostRequest::OpenBuffer { kind, argument } => self.open_plugin(&kind, argument)?,
            HostRequest::Layer {
                purpose,
                text,
                inline,
            } => {
                if self.views[&view].layer.is_some() {
                    bail!("view already has a layer");
                }
                self.focus_view(view)?;
                let buffer = self.add_buffer(Buffer::Text(TextBuffer::new(&text)));
                let child = self.create_page(buffer);
                let layer = self.views.get_mut(&child).unwrap();
                layer.parent = Some(view);
                layer.layer_content = Some(LayerContent::Draft { purpose });
                layer.presentation = if inline {
                    LayerPresentation::Inline
                } else {
                    LayerPresentation::Pane
                };
                self.views.get_mut(&view).unwrap().layer = Some(child);
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
                let query_kind = if purpose == "find" {
                    Some(QueryKind::Plain)
                } else {
                    None
                };
                self.prompt = Some(Prompt {
                    kind: PromptKind::MiniBuffer {
                        purpose,
                        key_scope,
                        query_kind,
                    },
                    cursor: text.len(),
                    text,
                    view,
                    completion: None,
                    error: None,
                });
                self.pending.clear();
            }
            HostRequest::Preview {
                path,
                content,
                focus,
            } => self.show_preview(page, path, content, focus)?,
            HostRequest::Close => {
                self.focus_view(view)?;
                self.close_view(false)?;
            }
        }
        Ok(())
    }

    fn open_plugin(&mut self, kind: &str, argument: Value) -> Result<()> {
        if self.views.page(self.focus).navigation.root_kind.as_deref() == Some(kind)
            && (argument.is_null() || argument.as_object().is_some_and(serde_json::Map::is_empty))
        {
            return Ok(());
        }
        let text_source = self.plugin_text_source(kind, self.views[&self.focus].location.buffer)?;
        let source = self.preview_source(self.focus).unwrap_or(self.focus);
        self.focus_view(source)?;
        self.open_plugin_navigation(kind, argument, text_source)
    }

    fn split(&mut self, axis: Axis) -> Result<()> {
        if self.help.is_some() {
            return self.help_command(HelpCommand::Pin);
        }
        let source = self.focus;
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
            && let Some(parent) = self.views[&root].parent
        {
            root = parent;
        }
        let current = leaves.iter().position(|id| *id == root).unwrap_or(0);
        let mut target =
            leaves[(current as isize + delta).rem_euclid(leaves.len() as isize) as usize];
        while let Some(layer) = self.views[&target].layer {
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
            && let Some(parent) = self.views[&root].parent
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
            while let Some(layer) = self.views[&target].layer {
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
        let id = self.views.main(page);
        if self.close_plugin_navigation(page, force)? {
            return Ok(());
        }
        let return_to = self.views[&id]
            .return_to
            .and_then(|target| self.views.resolve_focus(target))
            .map(|mut target| {
                while let Some(parent) = self.views.get(&target).and_then(|view| view.parent) {
                    target = parent;
                }
                target
            });
        let view = &self.views[&id];
        if (view.parent.is_none() || matches!(view.layer_content, Some(LayerContent::File)))
            && !force
            && matches!(&self.buffers[&view.location.buffer], Buffer::Text(text) if text.modified())
        {
            bail!("unsaved buffer; :w or :q!");
        }
        if view.parent.is_none() {
            let mut leaves = Vec::new();
            self.layout.as_ref().unwrap().leaves(&mut leaves);
            if leaves == [page] {
                if !force && self.modified() {
                    bail!("unsaved buffers; :w or :q!");
                }
                self.leave_page(page)?;
                self.exiting = true;
                return Ok(());
            }
        }
        self.leave_page(page)?;
        let view = &self.views[&id];
        let buffer = view.location.buffer;
        if let Some(parent) = view.parent {
            if matches!(view.layer_content, Some(LayerContent::File)) {
                self.retire_page(page);
                self.remove_file_watch(buffer);
                self.buffers.remove(&buffer);
                self.views.get_mut(&parent).unwrap().layer = None;
                self.activate_view(parent);
                self.remember_text();
                return Ok(());
            }
            let purpose = match &view.layer_content {
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
            self.retire_page(page);
            self.remove_file_watch(buffer);
            self.buffers.remove(&buffer);
            self.views.get_mut(&parent).unwrap().layer = None;
            self.activate_view(parent);
            return Ok(());
        }
        self.retire_page(page);
        self.layout = self.layout.take().and_then(|layout| layout.remove(page));
        if self.layout.is_none() {
            let buffer = self.add_buffer(Buffer::Text(TextBuffer::new("")));
            self.add_initial_view(buffer);
        } else {
            let mut leaves = Vec::new();
            self.layout.as_ref().unwrap().leaves(&mut leaves);
            let mut focus = return_to
                .filter(|id| {
                    self.views.contains_key(id) && leaves.contains(&self.views.page_id(*id))
                })
                .unwrap_or_else(|| self.first_layout_view());
            while let Some(layer) = self.views[&focus].layer {
                focus = layer;
            }
            self.activate_view(focus);
            self.remember_text();
        }
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

    fn build_runtime(&self, with_config: bool) -> Result<ScriptRuntime> {
        let host = RuntimeHost {
            services: self.services.clone(),
            jobs: RefCell::new(HashSet::new()),
        };
        let mut runtime = ScriptRuntime::with_identities(
            move |operation, argument| host.call(&operation, argument),
            self.identities.clone(),
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
        for view in self.views.values() {
            runtime.validate_view_options_value(&view.location.options)?;
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

    fn reload(&mut self) -> bool {
        match self.build_runtime(true) {
            Ok(runtime) => {
                self.invalidate_mouse();
                self.scripts = Some(runtime);
                self.generation += 1;
                self.pending.clear();
                self.close_help_overlay();
                self.refresh_help_buffers();
                for view in self.views.values_mut() {
                    view.wake_at = None;
                    view.observations.clear();
                }
                self.message = "user plugins reloaded".into();
                true
            }
            Err(error) => {
                if self.scripts.is_none() {
                    match self.build_runtime(false) {
                        Ok(runtime) => self.scripts = Some(runtime),
                        Err(default_error) => {
                            self.message = format!(
                                "defaults failed: {default_error:#}; reload failed: {error:#}"
                            );
                            return false;
                        }
                    }
                }
                self.message = format!("reload failed: {error:#}");
                false
            }
        }
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
                    if !matches!(action, NativeAction::Text(_) | NativeAction::Prompt(_)) {
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
