use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet},
    ops::Range,
    rc::Rc,
};

use serde_json::{Value, json};
use zess_plugin::{
    HostRequest, ItemNavigation, ItemSelection, Navigation, PageNavigationSource, StatusConfig,
};
use zess_protocol::{
    ClientMessage, CommandAction, CommandTarget,
    resource::{
        FilterKind, PageCacheEntry, PageCacheKey, PageCommandInput, PageDocument, PageFilter,
        PageId, PageInput, PageSource, PageSplit, ResourceCacheInvalidation, ResourceEvent,
        ResourceRequest, ResourceUpdate, ResourceUpdateKind,
    },
    workspace_view::pages::{PagePosition, PageTabPosition, PageWorkspace},
};
use zess_ui::{Display, Frame, MouseEvent, MouseKind, RowColumns};

use crate::statusline::Statuslines;

mod cache;
mod files;
mod presentation;
mod render_cache;
#[cfg(test)]
mod tests;
use files::{DirectoryCache, FileRows, FileRowsUpdate};
pub(crate) use presentation::item_rows;
use presentation::{PreviewSource, Previews};
use render_cache::{RenderCache, RowsProjection};

struct PageIdentity {
    view_id: u64,
    buffer_id: u64,
}

struct PendingSourceNavigation {
    generation: u64,
    fullscreen: bool,
}

struct PageSubscription {
    id: u64,
    // An active page gets a new transport ID after reconnect.
    first_id: u64,
    patch_revision: Option<u64>,
    source_navigation: Option<PendingSourceNavigation>,
}

impl PageSubscription {
    fn cancel_fullscreen_navigation(&mut self) {
        if self
            .source_navigation
            .as_ref()
            .is_some_and(|pending| pending.fullscreen)
        {
            self.source_navigation = None;
        }
    }
}

enum PreviewFocus {
    Automatic,
    Local(u64),
    Requested(u64),
}

struct Page {
    position: PagePosition,
    display: Display,
    file_rows: FileRows,
    columns: RowColumns,
    item_count: usize,
    identity: Option<PageIdentity>,
    options: Value,
    previews: Previews,
    visible_preview: Option<PageCacheEntry>,
    visible_rows: Option<PageCacheEntry>,
    subscription: Option<PageSubscription>,
    generation: u64,
    preview_focus: PreviewFocus,
    document_scoped: bool,
    modified: bool,
    jump: Option<zess_ui::ItemJump>,
    choosing_register: bool,
    register: Option<char>,
    dragging: bool,
}

impl Page {
    fn new(page_id: PageId, source: PageSource, source_buffer: Option<u64>) -> Self {
        let display = Display {
            title: match source.kind.as_str() {
                "files" => "Files".into(),
                "git" | "git-history" | "git-commit-files" => "Git".into(),
                other => other.to_owned(),
            },
            ..Default::default()
        };
        Self {
            position: PagePosition {
                page_id,
                clone_from: None,
                source,
                source_buffer,
                selection: ItemSelection::default(),
                scroll: 0,
                horizontal_scroll: 0,
                mode: "normal".into(),
                filter: None,
                preview: None,
            },
            display,
            file_rows: FileRows::default(),
            columns: RowColumns::default(),
            item_count: 0,
            identity: None,
            options: json!({}),
            previews: Previews::default(),
            visible_preview: None,
            visible_rows: None,
            subscription: None,
            generation: 0,
            preview_focus: PreviewFocus::Automatic,
            document_scoped: true,
            modified: false,
            jump: None,
            register: None,
            dragging: false,
            choosing_register: false,
        }
    }

    fn subscription_id(&self) -> Option<u64> {
        self.subscription
            .as_ref()
            .map(|subscription| subscription.id)
    }

    fn command_input(&self, register: Option<char>) -> PageCommandInput {
        PageCommandInput {
            generation: self.generation,
            selection: self.position.selection.clone(),
            register,
        }
    }

    fn command_target(&self, register: Option<char>) -> Option<CommandTarget> {
        Some(CommandTarget::Page {
            subscription_id: self.subscription_id()?,
            input: self.command_input(register),
        })
    }

    fn preview_pending(&mut self) {
        if let Some(subscription) = &mut self.subscription {
            subscription.patch_revision = None;
        }
        self.previews.pending();
    }

    fn preview_key(&self, width: u16, height: u16) -> Option<PageCacheKey> {
        Some(PageCacheKey::Preview {
            primary: self.position.selection.primary.clone()?,
            width: usize::from(width) / 2,
            height: usize::from(height.saturating_sub(1)),
        })
    }

    fn has_selected_preview(&self, width: u16, height: u16) -> bool {
        self.visible_preview
            .as_ref()
            .is_some_and(|entry| self.preview_key(width, height).as_ref() == Some(&entry.key))
    }

    fn forget_cached_projections(&mut self, cache: &mut RenderCache) {
        cache.forget_page(self.position.page_id);
        self.visible_preview = None;
        self.visible_rows = None;
    }

    fn invalidate_visible_projections(
        &mut self,
        entries: &[PageCacheEntry],
        reason: ResourceCacheInvalidation,
    ) {
        if self
            .visible_preview
            .as_ref()
            .is_some_and(|entry| entries.contains(entry))
        {
            self.visible_preview = None;
            if reason == ResourceCacheInvalidation::Changed {
                self.preview_pending();
            }
        }
        if self
            .visible_rows
            .as_ref()
            .is_some_and(|entry| entries.contains(entry))
        {
            self.visible_rows = None;
        }
    }

    fn can_focus_preview(&self, generation: u64) -> bool {
        !matches!(self.preview_focus, PreviewFocus::Local(current) if current == generation)
    }

    fn input(&self, width: u16, height: u16, focused: bool) -> PageInput {
        PageInput {
            generation: self.generation,
            selection: self.verified_file_selection().clone(),
            register: self.register,
            mode: self.position.mode.clone(),
            filter: self.position.filter.clone(),
            source_buffer: self.position.source_buffer,
            width: if self.previews.snapshot().is_some() {
                usize::from(width.saturating_sub(1)) / 2
            } else {
                usize::from(width)
            },
            active: focused && self.previews.scope().is_none(),
            preview_width: usize::from(width) / 2,
            preview_height: usize::from(height.saturating_sub(1)),
        }
    }

    fn item_rows(&self, height: u16) -> Range<usize> {
        let status = self.options.get("statusline").and_then(Value::as_str) != Some("");
        let end = usize::from(height.saturating_sub(1)).saturating_sub(usize::from(status));
        let header = end > 0 && (!self.display.tabs.is_empty() || !self.display.header.is_empty());
        usize::from(header)..end
    }

    fn choose(&mut self, row: usize, extend: bool, height: usize) -> bool {
        let Some(target) = self.display.rows.get(row).filter(|row| !row.id.is_empty()) else {
            return false;
        };
        let selection = &mut self.position.selection;
        if extend || self.position.mode == "select" {
            let anchor = self
                .display
                .rows
                .iter()
                .position(|row| Some(&row.id) == selection.anchor.as_ref())
                .unwrap_or(row);
            selection.selected = self.display.rows[anchor.min(row)..=anchor.max(row)]
                .iter()
                .filter(|row| !row.id.is_empty())
                .map(|row| row.id.clone())
                .collect();
        } else {
            selection.selected = vec![target.id.clone()];
            selection.anchor = Some(target.id.clone());
        }
        selection.primary = Some(target.id.clone());
        self.position.scroll = zess_ui::scroll_with_margin(self.position.scroll, row, height, 5);
        true
    }

    fn update(
        &mut self,
        event: ResourceEvent,
        width: u16,
        height: u16,
        focused: bool,
    ) -> Option<ClientMessage> {
        if let ResourceEvent::Select { navigation } = &event {
            self.preview_pending();
            if self.defer_file_selection(*navigation) {
                if let Some(subscription) = &mut self.subscription {
                    subscription.source_navigation = None;
                }
                return None;
            }
        }
        let subscription = self.subscription.as_mut()?;
        let subscription_id = subscription.id;
        self.generation += 1;
        if matches!(event, ResourceEvent::Resize)
            && let PreviewFocus::Local(generation) | PreviewFocus::Requested(generation) =
                &mut self.preview_focus
            && *generation + 1 == self.generation
        {
            *generation = self.generation;
        }
        match event {
            ResourceEvent::OpenSource { fullscreen, .. }
            | ResourceEvent::CommitSource { fullscreen, .. } => {
                subscription.source_navigation = Some(PendingSourceNavigation {
                    generation: self.generation,
                    fullscreen,
                });
            }
            _ if event.supersedes_source_navigation() => subscription.source_navigation = None,
            _ => {}
        }
        Some(ClientMessage::Resource {
            request: ResourceRequest::Update {
                subscription_id,
                input: self.input(width, height, focused),
                event,
            },
        })
    }

    fn measure_display(&mut self) {
        self.columns = RowColumns::measure(&self.display.rows, self.display.column_alignment);
        self.item_count = self
            .display
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .filter(|id| !id.is_empty())
            .collect::<HashSet<_>>()
            .len();
    }

    fn apply_projection(&mut self, update: &mut ResourceUpdate, height: u16, rows_unchanged: bool) {
        self.document_scoped = update.document_scoped;
        let output = &mut update.output;
        if let Some(metadata) = update.metadata.take() {
            self.identity = Some(PageIdentity {
                view_id: metadata.view_id,
                buffer_id: metadata.buffer_id,
            });
            self.options = metadata.options;
        }
        if let Some(modified) = output.modified {
            self.modified = modified;
        }
        let waiting_for_rows = !rows_unchanged
            && output
                .display
                .as_ref()
                .is_some_and(|display| display.loading && display.rows.is_empty());
        if waiting_for_rows && !self.display.rows.is_empty() {
            let display = output.display.as_mut().unwrap();
            self.display.loading = display.loading;
            self.display.status = std::mem::take(&mut display.status);
        } else {
            if let Some(mut display) = output.display.take() {
                if self.jump.is_some()
                    && (display.header != self.display.header
                        || !rows_unchanged
                            && !display.rows.iter().map(|row| &row.id).eq(self
                                .display
                                .rows
                                .iter()
                                .map(|row| &row.id)))
                {
                    self.jump = None;
                }
                let measure =
                    !rows_unchanged || display.column_alignment != self.display.column_alignment;
                if rows_unchanged {
                    display.rows = std::mem::take(&mut self.display.rows);
                }
                self.display = display;
                if measure {
                    self.measure_display();
                }
            }
            if let Some(selection) = output.selection.take()
                && (!waiting_for_rows || self.position.selection.primary.is_none())
            {
                let moved = selection.primary != self.position.selection.primary;
                self.position.selection = selection;
                if moved {
                    self.subscription.as_mut().unwrap().source_navigation = None;
                    if matches!(self.preview_focus, PreviewFocus::Requested(generation) if generation == update.generation)
                    {
                        self.preview_focus = PreviewFocus::Local(update.generation);
                    }
                }
                if moved
                    && let Some(row) = self
                        .display
                        .rows
                        .iter()
                        .position(|r| Some(&r.id) == self.position.selection.primary.as_ref())
                {
                    self.position.scroll = zess_ui::scroll_with_margin(
                        self.position.scroll,
                        row,
                        self.item_rows(height).len(),
                        5,
                    );
                }
            }
        }
    }

    fn snapshot(&self) -> PagePosition {
        let mut position = self.position.clone();
        position.preview = self.previews.snapshot().or(position.preview);
        position
    }

    fn restore(position: PagePosition, workspace: &str) -> Self {
        let mut page = Self::new(
            position.page_id,
            position.source.clone(),
            position.source_buffer,
        );
        page.position = position;
        page.previews.set_root(workspace);
        page
    }
}

#[derive(Default)]
struct PageTransport {
    context_epoch: Cell<u64>,
    page_watermarks: RefCell<BTreeMap<u64, u64>>,
    subscription: Cell<u64>,
    requests: RefCell<Vec<ClientMessage>>,
    file_cache: RefCell<DirectoryCache>,
    render_cache: RefCell<RenderCache>,
    announced_cache: RefCell<Option<HashSet<PageCacheEntry>>>,
}

impl PageTransport {
    fn allocate_page(&self) -> PageId {
        let context_epoch = self.context_epoch.get();
        let mut watermarks = self.page_watermarks.borrow_mut();
        let sequence = watermarks.entry(context_epoch).or_default();
        *sequence += 1;
        PageId {
            context_epoch,
            sequence: *sequence,
        }
    }
}

#[derive(Default)]
pub(crate) struct PagePublication {
    pub(crate) requests: Vec<HostRequest>,
    pub(crate) document: Option<PageDocument>,
}

pub(crate) enum PageMouseHit {
    Handled,
    Command(String),
}

fn allocate_id(counter: &Cell<u64>) -> u64 {
    let next = counter.get() + 1;
    counter.set(next);
    next
}

#[derive(Default)]
pub(crate) struct Pages {
    stack: Vec<Page>,
    suspended: bool,
    cached: Vec<Page>,
    inactive_tabs: BTreeMap<(Option<PageId>, String, String), Vec<Page>>,
    transport: Rc<PageTransport>,
    effects: Vec<HostRequest>,
    actions: Vec<CommandAction>,
    last_search: String,
    width: u16,
    height: u16,
    inactive: bool,
    source_buffer: Option<u64>,
    workspace: String,
}

impl Pages {
    pub(crate) fn sibling(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            width: self.width,
            height: self.height,
            source_buffer: self.source_buffer,
            workspace: self.workspace.clone(),
            ..Self::default()
        }
    }

    pub(crate) fn attach(&self, context_epoch: u64) -> u64 {
        self.transport.context_epoch.replace(context_epoch)
    }

    pub(crate) fn page_watermarks(&self) -> Vec<PageId> {
        self.transport
            .page_watermarks
            .borrow()
            .iter()
            .map(|(&context_epoch, &sequence)| PageId {
                context_epoch,
                sequence,
            })
            .collect()
    }

    pub(crate) fn restore_page_watermarks(&self, saved: &[PageId]) {
        *self.transport.page_watermarks.borrow_mut() = saved
            .iter()
            .map(|id| (id.context_epoch, id.sequence))
            .collect();
    }

    pub(crate) fn set_source_buffer(&mut self, id: Option<u64>) {
        self.source_buffer = id;
    }

    pub(crate) fn set_workspace(&mut self, workspace: String) {
        if self.workspace == workspace {
            return;
        }
        self.workspace = workspace;
        self.clear_cache();
        for page in self
            .stack
            .iter_mut()
            .chain(&mut self.cached)
            .chain(self.inactive_tabs.values_mut().flatten())
        {
            page.visible_preview = None;
            page.visible_rows = None;
            page.previews.set_root(&self.workspace);
        }
    }

    pub(crate) fn event_slot(&self) -> Option<&str> {
        self.stack.last()?.previews.focused_slot()
    }

    pub(crate) fn diff_search_active(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|page| page.previews.diff_search_active())
    }

    pub(crate) fn event_selection(&self) -> Option<zess_protocol::editor_events::EditorSelection> {
        let page = self.stack.last()?;
        Some(page.previews.event_selection().unwrap_or_else(|| {
            zess_protocol::editor_events::EditorSelection::Items {
                selection: page.verified_file_selection().clone(),
            }
        }))
    }

    pub(crate) fn subscription(&self) -> Option<u64> {
        self.stack.last()?.subscription_id()
    }

    pub(crate) fn command_target(&self, register: Option<char>) -> Option<CommandTarget> {
        self.stack.last()?.command_target(register)
    }

    pub(crate) fn verifying_file_rows(&self) -> bool {
        self.stack.last().is_some_and(Page::verifying_file_rows)
    }

    pub(crate) fn prepare_files_command(
        &mut self,
        target: &mut CommandTarget,
        kind: &str,
        event: &str,
    ) {
        if kind != "files" {
            return;
        }
        let CommandTarget::Page {
            subscription_id,
            input,
        } = target
        else {
            return;
        };
        let Some(page) = self.stack.last_mut().filter(|page| {
            page.subscription_id() == Some(*subscription_id) && page.previews.scope().is_none()
        }) else {
            return;
        };
        if page.navigate_file_rows(event, &mut self.transport.file_cache.borrow_mut()) {
            page.generation += 1;
            input.generation = page.generation;
        }
    }

    fn file_cache_entries(&self) -> Vec<PageCacheEntry> {
        self.transport.file_cache.borrow().entries()
    }

    fn invalidate_file_cache(
        &mut self,
        entries: &[PageCacheEntry],
        reason: ResourceCacheInvalidation,
    ) {
        self.transport.file_cache.borrow_mut().invalidate(entries);
        if matches!(reason, ResourceCacheInvalidation::Changed)
            && let Some(page) = self.stack.last_mut()
        {
            page.file_cache_invalidated(entries);
        }
    }

    fn clear_file_cache(&self) {
        self.transport.file_cache.borrow_mut().clear();
    }

    pub(crate) fn set_files_available(&mut self, available: bool) {
        if available {
            if self.verifying_file_rows() {
                self.update(ResourceEvent::Resize);
            }
        } else {
            self.clear_file_cache();
            if let Some(page) = self.stack.last_mut() {
                page.revalidate_file_rows();
            }
        }
    }

    pub(crate) fn clear_cache(&self) {
        self.clear_file_cache();
        self.transport.render_cache.borrow_mut().clear();
        self.transport.announced_cache.borrow_mut().take();
    }

    pub(crate) fn invalidate_cache(
        &mut self,
        entries: &[PageCacheEntry],
        reason: ResourceCacheInvalidation,
    ) {
        self.invalidate_file_cache(entries, reason);
        self.transport.render_cache.borrow_mut().invalidate(entries);
        for page in self
            .stack
            .iter_mut()
            .chain(&mut self.cached)
            .chain(self.inactive_tabs.values_mut().flatten())
        {
            page.invalidate_visible_projections(entries, reason);
        }
    }

    pub(crate) fn cache_interest(&self) -> Option<ClientMessage> {
        let entries = self
            .file_cache_entries()
            .into_iter()
            .chain(self.transport.render_cache.borrow().entries().cloned())
            .collect::<HashSet<_>>();
        let mut announced = self.transport.announced_cache.borrow_mut();
        if announced.as_ref() == Some(&entries) {
            return None;
        }
        let request = ResourceRequest::CacheInterest {
            entries: entries.iter().cloned().collect(),
        };
        *announced = Some(entries);
        Some(ClientMessage::Resource { request })
    }

    pub(crate) fn take_register(&mut self) -> Option<char> {
        let page = self.stack.last_mut()?;
        page.subscription_id()?;
        page.register.take()
    }

    pub(crate) fn scope(&self) -> Option<&str> {
        let page = self
            .stack
            .last()
            .filter(|page| page.subscription.is_some())?;
        Some(page.previews.scope().unwrap_or(&page.position.source.kind))
    }

    pub(crate) fn kind(&self) -> Option<&str> {
        let page = self
            .stack
            .last()
            .filter(|page| page.subscription.is_some())?;
        Some(page.previews.kind().unwrap_or(&page.position.source.kind))
    }

    pub(crate) fn mode(&self) -> Option<&str> {
        self.stack
            .last()
            .filter(|page| page.subscription.is_some())
            .map(|page| page.previews.mode().unwrap_or(page.position.mode.as_str()))
    }

    pub(crate) fn allow_preview_focus(&mut self, kind: &str, event: &str) {
        if kind == "files"
            && matches!(event, "find" | "toggle_ignored")
            && let Some(page) = self
                .stack
                .last_mut()
                .filter(|page| page.position.source.kind == kind)
        {
            page.forget_cached_projections(&mut self.transport.render_cache.borrow_mut());
        }
        if let Some(page) = self.stack.last_mut()
            && page.position.source.kind == kind
            && (event == "preview"
                || (event == "open" && matches!(kind, "git" | "git-commit-files")))
        {
            // A preview reply for this generation refers to the current selection.
            page.preview_focus = PreviewFocus::Requested(page.generation);
        }
    }

    pub(crate) fn focus_changed(&mut self) {
        if let Some(page) = self.stack.last_mut() {
            page.preview_focus = PreviewFocus::Local(page.generation);
            page.position.preview = None;
            if let Some(subscription) = &mut page.subscription {
                subscription.cancel_fullscreen_navigation();
            }
        }
    }

    fn subscribe(&mut self) {
        if self.suspended {
            return;
        }
        let Some(page) = self.stack.last_mut() else {
            return;
        };
        page.forget_cached_projections(&mut self.transport.render_cache.borrow_mut());
        let subscription_id = allocate_id(&self.transport.subscription);
        let first_id = page
            .subscription
            .as_ref()
            .map_or(subscription_id, |previous| previous.first_id);
        page.subscription = Some(PageSubscription {
            id: subscription_id,
            first_id,
            patch_revision: None,
            source_navigation: None,
        });
        page.generation = 0;
        page.preview_focus = PreviewFocus::Automatic;
        page.identity = None;
        page.revalidate_file_rows();
        page.preview_pending();
        self.transport
            .requests
            .borrow_mut()
            .push(ClientMessage::Resource {
                request: ResourceRequest::Subscribe {
                    subscription_id,
                    page_id: page.position.page_id,
                    clone_from: page.position.clone_from,
                    source: page.position.source.clone(),
                    input: page.input(self.width, self.height, !self.inactive),
                },
            });
    }

    pub(crate) fn split(&self) -> anyhow::Result<Self> {
        let source = self.stack.last().expect("split requires a visible page");
        let mut pages = self.sibling();
        pages.last_search.clone_from(&self.last_search);
        let mut position = source.position.clone();
        let page_id = self.transport.allocate_page();
        position.page_id = page_id;
        position.clone_from = Some(PageSplit {
            source: source.position.page_id,
            model: source
                .position
                .clone_from
                .map_or(source.position.page_id, |origin| origin.model),
        });
        pages.stack.push(Page {
            position,
            display: source.display.clone(),
            columns: source.columns.clone(),
            item_count: source.item_count,
            options: source.options.clone(),
            previews: source.previews.split()?,
            document_scoped: source.document_scoped,
            modified: source.modified,
            ..Page::new(
                page_id,
                source.position.source.clone(),
                source.position.source_buffer,
            )
        });
        pages.subscribe();
        Ok(pages)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub(crate) fn active_page_id(&self) -> Option<PageId> {
        self.stack.last().map(|page| page.position.page_id)
    }

    pub(crate) fn command_origin(&self) -> Option<(PageId, u64)> {
        let page = self.stack.last()?;
        Some((page.position.page_id, page.subscription.as_ref()?.first_id))
    }

    pub(crate) fn command_result_subscription(&self, page_id: PageId, origin: u64) -> Option<u64> {
        let page = self.stack.last()?;
        let subscription = page.subscription.as_ref()?;
        (page.position.page_id == page_id
            && (subscription.first_id..=subscription.id).contains(&origin))
        .then_some(subscription.id)
    }

    pub(crate) fn reconnect(&mut self) {
        self.subscribe();
    }

    fn release(&mut self, discard: bool) {
        if let Some(page) = self.stack.last_mut() {
            page.forget_cached_projections(&mut self.transport.render_cache.borrow_mut());
            let subscription = page.subscription.take();
            let request = if discard {
                self.transport
                    .file_cache
                    .borrow_mut()
                    .forget_page(page.position.page_id);
                ResourceRequest::Discard {
                    page_id: page.position.page_id,
                }
            } else if let Some(subscription) = subscription {
                ResourceRequest::Release {
                    subscription_id: subscription.id,
                }
            } else {
                return;
            };
            self.transport
                .requests
                .borrow_mut()
                .push(ClientMessage::Resource { request });
        }
    }

    pub(crate) fn discard(self) {
        let mut cache = self.transport.file_cache.borrow_mut();
        let requests = self
            .stack
            .iter()
            .chain(&self.cached)
            .chain(self.inactive_tabs.values().flatten())
            .map(|page| {
                let page_id = page.position.page_id;
                cache.forget_page(page_id);
                ClientMessage::Resource {
                    request: ResourceRequest::Discard { page_id },
                }
            });
        self.transport.requests.borrow_mut().extend(requests);
    }

    pub(crate) fn suspend(&mut self) {
        self.release(false);
        self.suspended = true;
    }

    pub(crate) fn resume(&mut self) {
        if self.suspended {
            self.suspended = false;
            self.subscribe();
        }
    }

    pub(crate) fn return_to_document(&mut self) {
        self.release(false);
        self.cached.append(&mut self.stack);
        self.suspended = false;
    }

    pub(crate) fn open(&mut self, kind: &str, argument: Value) {
        self.resume();
        let source = PageSource {
            kind: kind.into(),
            argument,
        };
        if self
            .stack
            .last()
            .is_some_and(|page| page.position.source == source)
        {
            return;
        }
        self.release(false);
        let source_buffer = self
            .stack
            .last()
            .map_or(self.source_buffer, |page| page.position.source_buffer);
        let parent = self.active_page_id();
        let tab = self.inactive_tabs.iter().find_map(|(address, stack)| {
            let page = stack.first().unwrap();
            (address.0 == parent
                && page.position.source == source
                && (!page.document_scoped || page.position.source_buffer == source_buffer))
                .then(|| address.clone())
        });
        if let Some(tab) = tab {
            self.stack.extend(self.inactive_tabs.remove(&tab).unwrap());
            self.subscribe();
            return;
        }
        let mut page = if let Some(index) = self.cached.iter().position(|page| {
            page.position.source == source
                && (!page.document_scoped || page.position.source_buffer == source_buffer)
        }) {
            self.cached.remove(index)
        } else {
            Page::new(self.transport.allocate_page(), source, source_buffer)
        };
        page.position.source_buffer = source_buffer;
        page.previews.set_root(&self.workspace);
        self.stack.push(page);
        self.subscribe();
    }

    fn discard_inactive_tab(&mut self, stack: Vec<Page>) {
        for page in stack {
            let page_id = page.position.page_id;
            self.discard_child_tabs(page_id);
            self.transport.file_cache.borrow_mut().forget_page(page_id);
            self.transport
                .requests
                .borrow_mut()
                .push(ClientMessage::Resource {
                    request: ResourceRequest::Discard { page_id },
                });
        }
    }

    fn discard_child_tabs(&mut self, parent: PageId) {
        let tabs: Vec<_> = self
            .inactive_tabs
            .keys()
            .filter(|(owner, _, _)| *owner == Some(parent))
            .cloned()
            .collect();
        for tab in tabs {
            let stack = self.inactive_tabs.remove(&tab).unwrap();
            self.discard_inactive_tab(stack);
        }
    }

    pub(crate) fn close(&mut self, force: bool) -> bool {
        let Some(page) = self.stack.last() else {
            return false;
        };
        if page.modified && !force {
            self.effects.push(HostRequest::Message {
                text: "Buffer has unsaved changes".into(),
            });
            return true;
        }
        self.release(force);
        let page = self.stack.pop().unwrap();
        if force {
            self.discard_child_tabs(page.position.page_id);
        } else {
            self.cached.push(page);
        }
        self.subscribe();
        true
    }

    fn switch_page(
        &mut self,
        group: &str,
        current: &str,
        target: &str,
        kind: &str,
        argument: &Value,
        source: Option<&PageNavigationSource>,
    ) {
        let Some(source) = source else {
            if !self
                .stack
                .last()
                .is_some_and(|page| page.position.source.kind == kind)
            {
                self.release(false);
                self.cached.extend(self.stack.pop());
                self.open(kind, argument.clone());
            }
            return;
        };
        if !self
            .stack
            .last()
            .is_some_and(|page| page.position.source.kind == source.kind)
        {
            self.effects.push(HostRequest::Message {
                text: format!("command requires a {} buffer", source.kind),
            });
            return;
        }
        if current == target {
            return;
        }
        let root = self
            .stack
            .iter()
            .rposition(|page| page.position.source.kind == source.tab_kind)
            .unwrap_or(self.stack.len() - 1);
        let parent = root
            .checked_sub(1)
            .map(|index| self.stack[index].position.page_id);

        self.release(false);
        let outgoing = self.stack.split_off(root);
        if let Some(replaced) = self
            .inactive_tabs
            .insert((parent, group.into(), current.into()), outgoing)
        {
            self.discard_inactive_tab(replaced);
        }
        if let Some(incoming) = self
            .inactive_tabs
            .remove(&(parent, group.into(), target.into()))
        {
            self.stack.extend(incoming);
            self.subscribe();
        } else {
            self.open(kind, argument.clone());
        }
    }

    fn update(&mut self, event: ResourceEvent) {
        if matches!(event, ResourceEvent::Refresh)
            && let Some(page) = self.stack.last_mut()
            && page.position.source.kind == "files"
        {
            self.transport.file_cache.borrow_mut().clear();
            page.revalidate_file_rows();
        }
        let Some(page) = self.stack.last_mut() else {
            return;
        };
        if page.subscription.is_some() && matches!(event, ResourceEvent::Select { .. }) {
            self.transport
                .render_cache
                .borrow_mut()
                .show_previous_preview(page, self.width, self.height);
        } else if matches!(
            event,
            ResourceEvent::FilterChanged
                | ResourceEvent::MiniBufferChanged { .. }
                | ResourceEvent::Refresh
        ) {
            page.forget_cached_projections(&mut self.transport.render_cache.borrow_mut());
        }
        if let Some(request) = page.update(event, self.width, self.height, !self.inactive) {
            self.transport.requests.borrow_mut().push(request);
        }
    }

    pub(crate) fn resize(&mut self, width: u16, height: u16) {
        if (self.width, self.height) != (width, height) {
            self.width = width;
            self.height = height;
            if let Some(page) = self.stack.last_mut() {
                page.forget_cached_projections(&mut self.transport.render_cache.borrow_mut());
            }
            self.update(ResourceEvent::Resize);
        }
    }

    pub(crate) fn set_focused(&mut self, focused: bool) {
        if self.inactive == focused {
            self.inactive = !focused;
            if !focused && let Some(page) = self.stack.last_mut() {
                page.preview_focus = PreviewFocus::Local(page.generation);
                if let Some(subscription) = &mut page.subscription {
                    subscription.cancel_fullscreen_navigation();
                }
            }
            self.update(ResourceEvent::Resize);
        }
    }

    pub(crate) fn preview_text(&mut self) -> Option<(&mut zess_text::TextBuffer, u64)> {
        self.stack
            .last_mut()
            .filter(|page| page.subscription.is_some())?
            .previews
            .text_view()
    }

    pub(crate) fn text_command(
        &mut self,
        name: &str,
        count: usize,
    ) -> Option<anyhow::Result<zess_text::TextEffect>> {
        self.stack
            .last_mut()
            .filter(|page| page.subscription.is_some())?
            .previews
            .text_command(name, count)
    }

    pub(crate) fn text_input(
        &mut self,
        key: &str,
    ) -> Option<anyhow::Result<zess_text::TextEffect>> {
        self.stack
            .last_mut()
            .filter(|page| page.subscription.is_some())?
            .previews
            .text_input(key)
    }

    pub(crate) fn paste(&mut self, text: &str) -> Option<anyhow::Result<zess_text::TextEffect>> {
        self.stack
            .last_mut()
            .filter(|page| page.subscription.is_some())?
            .previews
            .paste(text)
    }

    pub(crate) fn poll_syntax(&mut self) -> bool {
        self.stack
            .last_mut()
            .filter(|page| page.subscription.is_some())
            .is_some_and(|page| page.previews.poll_syntax())
    }

    pub(crate) fn current_filter(&self) -> Option<&PageFilter> {
        self.stack.last()?.position.filter.as_ref()
    }

    pub(crate) fn set_mode(&mut self, mode: &str) {
        if let Some(page) = self.stack.last_mut()
            && page.position.mode != mode
        {
            page.position.mode = mode.into();
            self.update(ResourceEvent::ModeChanged);
        }
    }

    pub(crate) fn filter(&mut self, filter: Option<PageFilter>) {
        if let Some(page) = self.stack.last_mut() {
            page.position.filter = filter;
            self.update(ResourceEvent::FilterChanged);
        }
    }

    pub(crate) fn prompt_changed(&mut self, purpose: &str, text: String, query_kind: FilterKind) {
        if let Some(page) = self.stack.last_mut() {
            if purpose == "diff-search" {
                if let Err(error) = page.previews.search(&text) {
                    self.effects.push(HostRequest::Message {
                        text: error.to_string(),
                    });
                }
                return;
            }
            self.update(ResourceEvent::MiniBufferChanged {
                purpose: purpose.into(),
                text,
                query_kind,
            });
        }
    }

    pub(crate) fn submit_input(
        &mut self,
        purpose: &str,
        confirmation: Option<String>,
        text: &str,
        cancelled: bool,
    ) -> Option<CommandAction> {
        if purpose == "diff-search" {
            let page = self.stack.last_mut()?;
            if cancelled {
                page.previews.cancel_search();
            } else {
                page.previews.accept_search();
                self.last_search = text.into();
            }
            return None;
        }
        let page = self.stack.last()?;
        Some(CommandAction::PagePrompt {
            subscription_id: page.subscription_id()?,
            input: page.command_input(page.register),
            purpose: purpose.into(),
            confirmation,
            text: text.into(),
            cancelled,
        })
    }

    pub(crate) fn feed_key(&mut self, key: &str) -> bool {
        let Some(page) = self
            .stack
            .last_mut()
            .filter(|page| page.subscription.is_some())
        else {
            return false;
        };
        if page.choosing_register {
            page.choosing_register = false;
            let mut chars = key.chars();
            page.register = chars.next().filter(|_| chars.next().is_none());
            self.update(ResourceEvent::Register);
            return true;
        }
        let Some(jump) = page.jump.take() else {
            return false;
        };
        match jump.input(key) {
            zess_ui::JumpInput::Waiting(jump) => page.jump = Some(jump),
            zess_ui::JumpInput::Cancelled => {}
            zess_ui::JumpInput::Selected(id) => {
                if let Some(row) = page.display.rows.iter().position(|r| r.id == id) {
                    page.choose(row, false, page.item_rows(self.height).len());
                    self.update(ResourceEvent::Select {
                        navigation: ItemNavigation::Jump,
                    });
                }
            }
        }
        true
    }

    pub(crate) fn command(&mut self, name: &str, count: usize) -> bool {
        let count = count.max(1);
        if matches!(name, "git" | "files") {
            self.open(name, json!({}));
            return true;
        }
        let Some(page) = self
            .stack
            .last_mut()
            .filter(|page| page.subscription.is_some())
        else {
            return false;
        };
        let height = page.item_rows(self.height).len();
        let focused_slot = page.previews.focused_slot().map(str::to_owned);
        if page.previews.command(name, count) {
            if page.previews.focused_slot() != focused_slot.as_deref() {
                self.focus_changed();
            }
            return true;
        }
        match name {
            "diff.apply-selection" => {
                let subscription = page.subscription.as_ref().unwrap();
                match subscription
                    .patch_revision
                    .ok_or_else(|| {
                        anyhow::anyhow!("wait for the current Git preview to finish loading")
                    })
                    .and_then(|revision| page.previews.selected_patch(revision))
                {
                    Ok(request) => self.actions.push(CommandAction::ApplyPagePatch {
                        subscription_id: subscription.id,
                        request,
                    }),
                    Err(error) => self.effects.push(HostRequest::Message {
                        text: error.to_string(),
                    }),
                }
                return true;
            }
            "diff.search-forward" | "diff.search-backward" => {
                if page.previews.begin_search(name == "diff.search-forward") {
                    self.effects.push(HostRequest::MiniBuffer {
                        purpose: "diff-search".into(),
                        text: String::new(),
                        key_scope: None,
                        confirmation: None,
                    });
                }
                return true;
            }
            "diff.search-next" | "diff.search-previous" => {
                if let Err(error) = page
                    .previews
                    .repeat_search(&self.last_search, name == "diff.search-next")
                {
                    self.effects.push(HostRequest::Message {
                        text: error.to_string(),
                    });
                }
                return true;
            }
            "diff.edit-source" | "preview.open-source" => {
                if page.verifying_file_rows() {
                    self.effects.push(HostRequest::Message {
                        text: "wait for Files to finish loading".into(),
                    });
                    return true;
                }
                let fullscreen = name == "diff.edit-source";
                let event = match page.previews.source() {
                    Ok(PreviewSource::Commit { commit, path, line }) => {
                        ResourceEvent::CommitSource {
                            commit,
                            path,
                            line,
                            fullscreen,
                        }
                    }
                    Ok(PreviewSource::Worktree { path, line }) => ResourceEvent::OpenSource {
                        path,
                        line,
                        fullscreen,
                    },
                    Ok(PreviewSource::Directory { path }) => {
                        self.open("files", json!({"path": path}));
                        return true;
                    }
                    Err(error) => {
                        self.effects.push(HostRequest::Message {
                            text: error.to_string(),
                        });
                        return true;
                    }
                };
                self.update(event);
                return true;
            }
            "data.select-register" => {
                page.choosing_register = true;
                return true;
            }
            "data.goto-word" => {
                page.jump = zess_ui::ItemJump::new(
                    page.display
                        .rows
                        .iter()
                        .skip(page.position.scroll)
                        .take(height)
                        .map(|r| r.id.as_str()),
                );
                return true;
            }
            "q" | "quit" => return self.close(false),
            "q!" | "quit!" => return self.close(true),
            "git-commit-files.back" => return self.close(false),
            "data.escape" | "preview.return" => {
                if page.previews.leave() {
                    self.focus_changed();
                    return true;
                }
                if name == "preview.return" {
                    return true;
                }
                if page.position.mode == "normal" && page.position.selection.selected.len() <= 1 {
                    return self.close(false);
                }
                page.position.mode = "normal".into();
                let primary = page.position.selection.primary.clone();
                page.position.selection = ItemSelection {
                    selected: primary.iter().cloned().collect(),
                    anchor: primary.clone(),
                    primary,
                };
                self.update(ResourceEvent::ModeChanged);
                return true;
            }
            "data.select-mode" => {
                page.position.mode = if page.position.mode == "select" {
                    "normal"
                } else {
                    "select"
                }
                .into();
                page.position
                    .selection
                    .anchor
                    .clone_from(&page.position.selection.primary);
                self.update(ResourceEvent::ModeChanged);
                return true;
            }
            "data.keep-primary" => {
                page.position.selection.selected =
                    page.position.selection.primary.iter().cloned().collect();
                page.position
                    .selection
                    .anchor
                    .clone_from(&page.position.selection.primary);
                page.position.mode = "normal".into();
                self.update(ResourceEvent::ModeChanged);
                return true;
            }
            "data.select-all" => {
                page.position.selection.selected = page
                    .display
                    .rows
                    .iter()
                    .filter(|row| !row.id.is_empty())
                    .map(|row| row.id.clone())
                    .collect();
                self.update(ResourceEvent::Select {
                    navigation: ItemNavigation::Jump,
                });
                return true;
            }
            "data.refresh" => {
                self.update(ResourceEvent::Refresh);
                return true;
            }
            _ => {}
        }
        let rows = &page.display.rows;
        let primary = page.position.selection.primary.as_ref();
        let current = rows
            .iter()
            .position(|row| Some(&row.id) == primary)
            .unwrap_or(0);
        let step = |down: bool, count: usize| {
            if down {
                rows.iter()
                    .enumerate()
                    .skip(current + 1)
                    .filter(|(_, row)| !row.id.is_empty())
                    .take(count)
                    .last()
                    .map(|(row, _)| row)
            } else {
                rows.iter()
                    .enumerate()
                    .take(current)
                    .rev()
                    .filter(|(_, row)| !row.id.is_empty())
                    .take(count)
                    .last()
                    .map(|(row, _)| row)
            }
            .unwrap_or(current)
        };
        let mut page_scroll = None;
        let target = match name {
            "data.move-down" | "data.extend-lines" => step(true, count),
            "data.move-up" => step(false, count),
            "data.goto-first" | "data.goto-last" if count > 1 => rows
                .iter()
                .enumerate()
                .filter(|(_, row)| !row.id.is_empty())
                .take(count)
                .last()
                .map_or(0, |(row, _)| row),
            "data.goto-first" => rows.iter().position(|row| !row.id.is_empty()).unwrap_or(0),
            "data.goto-last" => rows.iter().rposition(|row| !row.id.is_empty()).unwrap_or(0),
            "data.page-half-down" | "data.page-half-up" => {
                let down = name == "data.page-half-down";
                let direction = if down {
                    zess_ui::ScrollDirection::Down
                } else {
                    zess_ui::ScrollDirection::Up
                };
                let scroll = zess_ui::HalfPageScroll::new(height, count, direction);
                let target = step(down, scroll.distance());
                page_scroll = Some(scroll.viewport_start(page.position.scroll, target, 5));
                target
            }
            "data.copy-selection-down" => {
                let target = step(true, 1);
                if let Some(row) = rows.get(target) {
                    let selection = &mut page.position.selection;
                    if !selection.selected.contains(&row.id) {
                        selection.selected.push(row.id.clone());
                    }
                    selection.primary = Some(row.id.clone());
                    selection.anchor = Some(row.id.clone());
                    page.position.scroll =
                        zess_ui::scroll_with_margin(page.position.scroll, target, height, 5);
                    self.update(ResourceEvent::Select {
                        navigation: ItemNavigation::Step,
                    });
                }
                return true;
            }
            _ => return false,
        };
        if page.choose(target, name == "data.extend-lines", height) {
            if let Some(scroll) = page_scroll {
                page.position.scroll = scroll;
            }
            let navigation = if count == 1
                && matches!(
                    name,
                    "data.move-up" | "data.move-down" | "data.extend-lines"
                ) {
                ItemNavigation::Step
            } else {
                ItemNavigation::Jump
            };
            self.update(ResourceEvent::Select { navigation });
        }
        true
    }

    pub(crate) fn request(&mut self, request: &HostRequest, count: usize) -> bool {
        match request {
            HostRequest::OpenBuffer { kind, argument } => self.open(kind, argument.clone()),
            HostRequest::Navigate {
                navigation: Navigation::Back,
            }
            | HostRequest::Close => return self.close(false),
            HostRequest::Navigate {
                navigation: Navigation::Push { kind, argument },
            } => self.open(kind, argument.clone()),
            HostRequest::Navigate {
                navigation:
                    Navigation::Page {
                        group,
                        current,
                        page,
                        kind,
                        argument,
                        source,
                    },
            } => self.switch_page(group, current, page, kind, argument, source.as_ref()),
            HostRequest::Action { name, .. } => return self.command(name, count),
            HostRequest::BufferEvent { kind, event, .. } if self.scope() == Some(kind.as_str()) => {
                return self.command(&format!("{kind}.{event}"), count);
            }
            _ => return false,
        }
        true
    }

    fn discard_document(&mut self, document: PageDocument) {
        if document.fullscreen {
            self.actions.push(CommandAction::ClosePageDocument {
                buffer_id: document.buffer_id,
                force: false,
            });
        }
    }

    pub(crate) fn apply(&mut self, mut update: ResourceUpdate) -> PagePublication {
        let is_source_navigation = matches!(update.kind, ResourceUpdateKind::SourceNavigation);
        let Some(page) = self.stack.last_mut().filter(|page| {
            let Some(subscription) = &page.subscription else {
                return false;
            };
            if subscription.id != update.subscription_id {
                return false;
            }
            if is_source_navigation {
                subscription
                    .source_navigation
                    .as_ref()
                    .is_some_and(|pending| pending.generation == update.generation)
            } else {
                update.generation <= page.generation
            }
        }) else {
            if let Some(document) = update.document {
                self.discard_document(document);
            }
            return PagePublication::default();
        };
        let subscription = page.subscription.as_mut().unwrap();
        if is_source_navigation {
            subscription.source_navigation = None;
            return PagePublication {
                requests: update.output.requests,
                document: update.document,
            };
        }
        let current_projection = page.generation == update.generation;
        let show_preview = current_projection && !page.has_deferred_file_selection();
        let mut retry_rows = false;
        let mut deferred_selection = None;
        if current_projection {
            let mut rows_unchanged = false;
            let mut cache = self.transport.render_cache.borrow_mut();
            if update
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.options != page.options)
            {
                page.forget_cached_projections(&mut cache);
            }
            if page.position.source.kind == "files"
                && update.metadata.as_ref().is_some_and(|metadata| {
                    metadata.options["git-status"] != page.options["git-status"]
                })
            {
                self.transport
                    .file_cache
                    .borrow_mut()
                    .forget_page(page.position.page_id);
                if update.output.display.is_none() {
                    page.revalidate_file_rows();
                    retry_rows = true;
                }
            }
            if let Some(display) = &mut update.output.display {
                if let Some(rows) = update.cache.rows.take() {
                    match cache.fill_rows(page, display, rows) {
                        RowsProjection::Received => {}
                        RowsProjection::Unchanged => rows_unchanged = true,
                        RowsProjection::Missing => {
                            display.loading = true;
                            retry_rows = true;
                        }
                    }
                } else if !display.loading || !display.rows.is_empty() {
                    page.visible_rows = None;
                }
            }
            match page.receive_file_rows(&mut update, &mut self.transport.file_cache.borrow_mut()) {
                FileRowsUpdate::None => {}
                FileRowsUpdate::Missing => retry_rows = true,
                FileRowsUpdate::Selection(navigation) => deferred_selection = Some(navigation),
            }
            page.apply_projection(&mut update, self.height, rows_unchanged);
        }
        if !show_preview {
            // Preserve one-time effects even when the projection refers to an older selection.
            update.output.requests.retain(|request| {
                !matches!(
                    request,
                    HostRequest::PreviewGroup { .. }
                        | HostRequest::Preview { .. }
                        | HostRequest::PreviewPending
                        | HostRequest::ConfigurePreview { .. }
                )
            });
        }
        let output = update.output;
        let last_preview = output.requests.iter().rposition(|request| {
            matches!(
                request,
                HostRequest::PreviewGroup { .. }
                    | HostRequest::Preview { .. }
                    | HostRequest::PreviewPending
            )
        });
        let mut preview = show_preview.then(|| update.cache.preview.take()).flatten();
        let preview_index = preview
            .as_ref()
            .map(|preview| preview.request_index.min(output.requests.len()));
        let mut retry_preview = false;
        let mut requests = Vec::new();
        let effects = output.requests.into_iter().map(Some).chain([None]);
        for (index, request) in effects.enumerate() {
            // A navigation effect can retire the target before the rest of its output is drained.
            let Some(page) = self
                .stack
                .last_mut()
                .filter(|page| page.subscription_id() == Some(update.subscription_id))
            else {
                break;
            };
            if preview_index == Some(index) {
                let confirmed = self.transport.render_cache.borrow_mut().confirm_preview(
                    page,
                    preview.take().unwrap(),
                    self.width,
                    self.height,
                    update.generation,
                );
                if confirmed && last_preview.is_none_or(|last| last < index) {
                    page.subscription.as_mut().unwrap().patch_revision = update.patch_revision;
                } else if !confirmed {
                    page.preview_pending();
                    retry_preview = true;
                }
            }
            let Some(request) = request else {
                break;
            };
            match request {
                HostRequest::PreviewGroup {
                    panes,
                    layout,
                    focus,
                } => {
                    let focus = focus && page.can_focus_preview(update.generation);
                    if panes.iter().any(|pane| pane.loading())
                        && page.has_selected_preview(self.width, self.height)
                    {
                        if focus {
                            page.previews.focus();
                        }
                        page.preview_pending();
                        continue;
                    }
                    let last =
                        last_preview == Some(index) && preview_index.is_none_or(|at| at <= index);
                    page.subscription.as_mut().unwrap().patch_revision =
                        last.then_some(update.patch_revision).flatten();
                    page.visible_preview = None;
                    page.previews.replace(panes, layout, focus);
                }
                HostRequest::Preview {
                    path,
                    content,
                    focus,
                } => {
                    page.subscription.as_mut().unwrap().patch_revision = None;
                    let focus = focus && page.can_focus_preview(update.generation);
                    page.visible_preview = None;
                    page.previews.single(path, content, focus);
                }
                HostRequest::PreviewPending => page.preview_pending(),
                HostRequest::ConfigurePreview { header, key_scope } => {
                    page.previews.configure(header, key_scope)
                }
                HostRequest::SetMode { mode } => page.position.mode = mode,
                HostRequest::Schedule { .. } => {}
                request => {
                    if !self.request(&request, 1) {
                        requests.push(request);
                    }
                }
            }
        }
        if current_projection
            && let Some(page) = self
                .stack
                .last_mut()
                .filter(|page| page.subscription_id() == Some(update.subscription_id))
            && let Some(preview) = &page.position.preview
            && page.previews.restore(preview)
        {
            page.position.preview = None;
        }
        if self.subscription() == Some(update.subscription_id) {
            if let Some(navigation) = deferred_selection {
                self.update(ResourceEvent::Select { navigation });
            } else if retry_preview {
                self.update(ResourceEvent::Refresh);
            } else if retry_rows {
                self.update(ResourceEvent::Resize);
            }
        }
        let mut document = update.document;
        if self.subscription() != Some(update.subscription_id)
            && let Some(discarded) = document.take()
        {
            self.discard_document(discarded);
        }
        PagePublication { requests, document }
    }

    pub(crate) fn mouse(
        &mut self,
        event: &MouseEvent,
        scroll_lines: usize,
    ) -> Option<PageMouseHit> {
        if event.kind != MouseKind::Release
            && (event.column >= self.width || event.row >= self.height.saturating_sub(1))
        {
            return None;
        }
        let page = self
            .stack
            .last_mut()
            .filter(|page| page.subscription.is_some())?;
        let item_rows = page.item_rows(self.height);
        let height = item_rows.len();
        let register_cleared = if matches!(event.kind, MouseKind::Press | MouseKind::DoubleClick) {
            page.jump = None;
            page.choosing_register = false;
            page.register.take().is_some()
        } else {
            false
        };
        let previous_focus = page.previews.focused_slot().map(str::to_owned);
        if page.previews.mouse(event, scroll_lines) {
            if event.kind != MouseKind::Drag {
                page.dragging = false;
            }
            let focus_changed = page.previews.focused_slot() != previous_focus.as_deref();
            if register_cleared {
                self.update(ResourceEvent::Register);
            }
            if focus_changed {
                self.focus_changed();
            }
            return Some(PageMouseHit::Handled);
        }
        let mut hit = PageMouseHit::Handled;
        match event.kind {
            MouseKind::ScrollDown | MouseKind::ScrollUp => {
                page.dragging = false;
                let lines = scroll_lines.min(isize::MAX as usize) as isize;
                let delta = if event.kind == MouseKind::ScrollUp {
                    -lines
                } else {
                    lines
                };
                page.position.scroll = page.position.scroll.saturating_add_signed(delta).min(
                    zess_ui::scroll_limit_with_margin(page.display.rows.len(), height, 5),
                );
            }
            MouseKind::Press | MouseKind::DoubleClick | MouseKind::Drag => {
                let extend = event.kind == MouseKind::Drag;
                if extend && !page.dragging {
                    return Some(hit);
                }
                page.previews.leave();
                let row = usize::from(event.row);
                if item_rows.contains(&row)
                    && page.choose(page.position.scroll + row - item_rows.start, extend, height)
                {
                    page.dragging = true;
                    page.jump = None;
                    if matches!(event.kind, MouseKind::DoubleClick)
                        && let Some(name) = page
                            .options
                            .get("double-click-command")
                            .and_then(Value::as_str)
                    {
                        page.dragging = false;
                        hit = PageMouseHit::Command(name.into());
                    }
                    self.update(ResourceEvent::Select {
                        navigation: ItemNavigation::Jump,
                    });
                } else if register_cleared {
                    self.update(ResourceEvent::Register);
                }
                if previous_focus.is_some() {
                    self.focus_changed();
                }
            }
            MouseKind::Release => page.dragging = false,
        }
        Some(hit)
    }

    pub(crate) fn render(
        &mut self,
        frame: &mut Frame,
        config: Option<&StatusConfig>,
        statuslines: &mut Statuslines,
        focused: bool,
    ) -> bool {
        let Some(page) = self.stack.last_mut() else {
            return false;
        };
        let Some(subscription) = page.subscription_id() else {
            return false;
        };
        presentation::render(frame, page, config, statuslines, subscription, focused);
        if focused {
            if let Some(jump) = &page.jump {
                frame.prompt = jump.prompt();
            } else if page.choosing_register {
                frame.prompt = "Select register".into();
            }
        }
        true
    }

    pub(crate) fn snapshot(&self) -> PageWorkspace {
        PageWorkspace {
            stack: self.stack.iter().map(Page::snapshot).collect(),
            cached: self.cached.iter().map(Page::snapshot).collect(),
            inactive_tabs: self
                .inactive_tabs
                .iter()
                .map(|((parent, group, tab), pages)| PageTabPosition {
                    parent: *parent,
                    group: group.clone(),
                    tab: tab.clone(),
                    stack: pages.iter().map(Page::snapshot).collect(),
                })
                .collect(),
            suspended: self.suspended,
        }
    }

    pub(crate) fn restore(
        &mut self,
        workspace: PageWorkspace,
        command_origin: Option<(PageId, u64)>,
    ) {
        self.release(false);
        self.suspended = workspace.suspended;
        {
            let mut watermarks = self.transport.page_watermarks.borrow_mut();
            for page in workspace.positions() {
                let PageId {
                    context_epoch,
                    sequence,
                } = page.page_id;
                let last = watermarks.entry(context_epoch).or_default();
                *last = (*last).max(sequence);
            }
        }
        self.stack = workspace
            .stack
            .into_iter()
            .map(|position| Page::restore(position, &self.workspace))
            .collect();
        self.cached = workspace
            .cached
            .into_iter()
            .map(|position| Page::restore(position, &self.workspace))
            .collect();
        self.inactive_tabs = workspace
            .inactive_tabs
            .into_iter()
            .filter(|tab| !tab.stack.is_empty())
            .map(|tab| {
                (
                    (tab.parent, tab.group, tab.tab),
                    tab.stack
                        .into_iter()
                        .map(|position| Page::restore(position, &self.workspace))
                        .collect(),
                )
            })
            .collect();
        self.subscribe();
        if let Some((page_id, first_id)) = command_origin
            && let Some(page) = self
                .stack
                .last_mut()
                .filter(|page| page.position.page_id == page_id)
            && let Some(subscription) = &mut page.subscription
        {
            subscription.first_id = first_id;
        }
    }

    pub(crate) fn take_requests(&self) -> Vec<ClientMessage> {
        std::mem::take(&mut *self.transport.requests.borrow_mut())
    }

    pub(crate) fn take_actions(&mut self) -> Vec<CommandAction> {
        std::mem::take(&mut self.actions)
    }

    pub(crate) fn take_effects(&mut self) -> Vec<HostRequest> {
        std::mem::take(&mut self.effects)
    }
}
