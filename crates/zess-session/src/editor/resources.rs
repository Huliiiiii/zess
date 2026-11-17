use super::*;
use zess_plugin::PluginOutput;
use zess_protocol::resource::{
    FilterKind, PageCacheEntry, PageCommandInput, PageDocument, PageId, PageInput, PageMetadata,
    PageSource, PageSplit, ResourceCacheInvalidation, ResourceEvent, ResourceRequest,
    ResourceUpdate, ResourceUpdateKind,
};

mod cache;
mod documents;
mod patches;

fn page_query(kind: FilterKind, text: String) -> Query {
    Query {
        kind: match kind {
            FilterKind::Plain => QueryKind::Plain,
            FilterKind::Glob => QueryKind::Glob,
            FilterKind::Regex => QueryKind::Regex,
        },
        text,
    }
}

struct PendingSource {
    handle: u64,
    generation: u64,
    line: usize,
    fullscreen: bool,
}

struct Subscription {
    page_id: PageId,
    model_id: PageId,
    source: PageSource,
    buffer: BufferId,
    view: ViewId,
    text_source: Option<BufferId>,
    text_version: Option<u64>,
    config_generation: u64,
    input: PageInput,
    options: serde_json::Map<String, Value>,
    watches: Vec<(QueryWatch, Observation)>,
    wake_at: Option<Instant>,
    commit_source: Option<PendingSource>,
    patches: patches::PublishedPatches,
}

impl Subscription {
    fn effective_options(&self, runtime: &ScriptRuntime) -> Value {
        runtime.view_options_with_overrides(&self.source.kind, Some(&self.options))
    }

    fn metadata(&self, options: Value) -> PageMetadata {
        PageMetadata {
            view_id: self.view,
            buffer_id: self.buffer,
            options,
        }
    }
}

#[derive(Default)]
pub(super) struct Subscriptions {
    active: BTreeMap<u64, Subscription>,
    inactive: BTreeMap<PageId, Subscription>,
    modified_buffers: HashSet<BufferId>,
    pub(super) fullscreen_documents: HashSet<BufferId>,
    updates: Vec<ResourceUpdate>,
    cache: cache::CachePublications,
    pub(super) workspace_changed: bool,
}

impl Subscriptions {
    pub(super) fn plugin_buffers(&self) -> impl Iterator<Item = (&str, bool)> {
        self.active
            .values()
            .chain(self.inactive.values())
            .map(|page| {
                (
                    page.source.kind.as_str(),
                    self.modified_buffers.contains(&page.buffer),
                )
            })
    }

    pub(super) fn validate_view_options(&self, runtime: &ScriptRuntime) -> Result<()> {
        for page in self.active.values().chain(self.inactive.values()) {
            runtime.validate_view_options_value(
                Some(&page.source.kind),
                &page.effective_options(runtime),
            )?;
        }
        Ok(())
    }

    pub(super) fn modified(&self) -> bool {
        !self.modified_buffers.is_empty()
    }

    fn publish_error(&mut self, subscription_id: u64, generation: u64, error: anyhow::Error) {
        self.updates.push(ResourceUpdate {
            kind: ResourceUpdateKind::Page,
            subscription_id,
            generation,
            document_scoped: self
                .active
                .get(&subscription_id)
                .is_none_or(|page| page.text_source.is_some()),
            patch_revision: None,
            metadata: None,
            document: None,
            cache: Default::default(),
            output: PluginOutput {
                requests: vec![HostRequest::Message {
                    text: format!("{error:#}"),
                }],
                ..PluginOutput::default()
            },
        });
    }

    fn page_outcome(
        &self,
        subscription_id: u64,
        navigation: Vec<zess_plugin::Navigation>,
        document: Option<PageDocument>,
    ) -> zess_protocol::CommandOutcome {
        zess_protocol::CommandOutcome::PageApplied {
            page_id: self.active[&subscription_id].page_id,
            subscription_id,
            navigation,
            document,
        }
    }

    fn take_page_effects(
        &self,
        update: &mut ResourceUpdate,
    ) -> Option<zess_protocol::CommandOutcome> {
        use zess_plugin::Navigation;

        let navigation: Vec<_> = update
            .output
            .requests
            .extract_if(.., |request| {
                matches!(
                    request,
                    HostRequest::Navigate { .. }
                        | HostRequest::OpenBuffer { .. }
                        | HostRequest::Close
                )
            })
            .map(|request| match request {
                HostRequest::Navigate { navigation } => navigation,
                HostRequest::OpenBuffer { kind, argument } => Navigation::Push { kind, argument },
                HostRequest::Close => Navigation::Back,
                _ => unreachable!(),
            })
            .collect();
        let document = update.document.take();
        if navigation.is_empty() && document.is_none() {
            return None;
        }
        Some(self.page_outcome(update.subscription_id, navigation, document))
    }
}

impl Session {
    pub(super) fn page_event_source(&self, id: u64) -> Option<(ViewId, BufferId)> {
        let page = self.resources.active.get(&id)?;
        Some((page.view, page.buffer))
    }

    pub(crate) fn has_page_subscription(&self, id: u64) -> bool {
        self.resources.active.contains_key(&id)
    }

    fn release_page(&mut self, id: u64) {
        if !self.has_page_subscription(id) {
            return;
        }
        let leaving = self.dispatch_page(id, "view_leaving", Value::Null);
        let mut page = self.resources.active.remove(&id).unwrap();
        let mut unpublished = Vec::new();
        self.resources.updates.retain(|update| {
            if update.subscription_id != id {
                return true;
            }
            if let Some(document) = &update.document
                && document.fullscreen
            {
                unpublished.push(document.buffer_id);
            }
            false
        });
        for buffer in unpublished {
            let _ = self.close_page_document(buffer, false);
        }
        if let Some(pending) = page.commit_source.take() {
            self.services.borrow_mut().cancel_query(pending.handle);
        }
        page.wake_at = None;
        page.watches.clear();
        self.resources.cache.release_page(page.page_id);
        page.patches = Default::default();
        if let Err(error) = leaving {
            self.message = format!("leaving {}: {error:#}", page.source.kind);
        }
        self.resources.inactive.insert(page.page_id, page);
    }

    fn discard_page(&mut self, page_id: PageId) {
        self.resources.cache.invalidate_page(page_id);
        if let Some((&id, _)) = self
            .resources
            .active
            .iter()
            .find(|(_, page)| page.page_id == page_id)
        {
            self.release_page(id);
        }
        let Some(page) = self.resources.inactive.remove(&page_id) else {
            return;
        };
        let shared = self
            .resources
            .active
            .values()
            .chain(self.resources.inactive.values())
            .any(|other| other.buffer == page.buffer);
        if !shared {
            self.resources.workspace_changed |=
                self.resources.modified_buffers.remove(&page.buffer);
        }
        if let Some(runtime) = &mut self.scripts {
            if shared {
                runtime.remove_buffer_view(page.buffer, page.view);
            } else {
                runtime.remove_buffer(page.buffer);
            }
        }
    }

    pub(crate) fn detach_resources(
        &mut self,
        workspace: Option<&zess_protocol::workspace_view::WorkspaceView>,
        retained_document: impl Fn(BufferId) -> bool,
    ) {
        let ids = self.resources.active.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.release_page(id);
        }
        self.resources.cache = Default::default();
        if let Some(workspace) = workspace {
            let retained: HashSet<_> = workspace
                .pages
                .iter()
                .flat_map(|pane| pane.pages.positions())
                .map(|page| page.page_id)
                .collect();
            let watermarks: BTreeMap<_, _> = workspace
                .page_watermarks
                .iter()
                .map(|id| (id.context_epoch, id.sequence))
                .collect();
            let retired = self
                .resources
                .inactive
                .keys()
                .copied()
                .filter(|id| {
                    watermarks
                        .get(&id.context_epoch)
                        .is_some_and(|&last| id.sequence <= last)
                        && !retained.contains(id)
                })
                .collect::<Vec<_>>();
            for page_id in retired {
                self.discard_page(page_id);
            }
        }
        let unclaimed = self
            .resources
            .fullscreen_documents
            .iter()
            .copied()
            .filter(|id| !retained_document(*id))
            .filter(|id| {
                !self.operation_completions.iter().any(|(_, outcome)| {
                    matches!(outcome, zess_protocol::CommandOutcome::PageApplied {
                    document: Some(document), ..
                } if document.fullscreen && document.buffer_id == *id)
                })
            })
            .collect::<Vec<_>>();
        for id in unclaimed {
            let _ = self.close_page_document(id, false);
        }
    }

    fn initialize_resource_page(
        &mut self,
        id: u64,
        event: &str,
        argument: Value,
        restore_filter: bool,
        selection: Option<ItemSelection>,
    ) -> Result<()> {
        let start = self.resources.updates.len();
        self.dispatch_page(id, event, argument)?;
        if restore_filter {
            self.dispatch_page(id, "filter_changed", Value::Null)?;
        }
        let page = self.resources.active.get_mut(&id).unwrap();
        if let Some(mut selection) = selection
            && let Some(primary) = &selection.primary
            && page.input.selection != selection
        {
            let display = self.resources.updates[start..]
                .iter()
                .rev()
                .filter(|update| update.subscription_id == id)
                .find_map(|update| update.output.display.as_ref());
            if display.is_none_or(|display| {
                (display.loading && display.rows.is_empty())
                    || display.rows.iter().any(|row| &row.id == primary)
            }) {
                if let Some(display) = display.filter(|display| !display.rows.is_empty()) {
                    selection.reconcile(&display.rows);
                }
                page.input.selection = selection;
                let start = self.resources.updates.len();
                self.dispatch_page(
                    id,
                    "select",
                    json!(SelectionChange {
                        navigation: Some(zess_plugin::ItemNavigation::Jump)
                    }),
                )?;
                if let Some(update) = self.resources.updates[start..]
                    .iter_mut()
                    .find(|update| update.subscription_id == id)
                {
                    update
                        .output
                        .selection
                        .get_or_insert_with(|| self.resources.active[&id].input.selection.clone());
                }
            }
        }
        Ok(())
    }

    fn open_resource(
        &mut self,
        id: u64,
        page_id: PageId,
        clone_from: Option<PageSplit>,
        source: PageSource,
        input: PageInput,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.has_page_subscription(id)
                && !self
                    .resources
                    .active
                    .values()
                    .any(|page| page.page_id == page_id),
            "page subscription already exists"
        );
        let model_id = clone_from.map_or(page_id, |origin| origin.model);
        let text_source = self.plugin_text_source(&source.kind, input.source_buffer)?;
        if let Some(page) = self.resources.inactive.get(&page_id) {
            anyhow::ensure!(
                page.model_id == model_id
                    && page.source == source
                    && page.text_source == text_source,
                "retained page belongs to a different source"
            );
        }
        let mut argument = source.argument.clone();
        let retained = self.resources.inactive.contains_key(&page_id);
        let native = self
            .scripts
            .as_ref()
            .is_some_and(|runtime| runtime.is_native_buffer(&source.kind));
        let (page, event, restore_filter) =
            if let Some(mut page) = self.resources.inactive.remove(&page_id) {
                let reinitialize = !native && page.config_generation != self.generation;
                let restore_filter =
                    page.input.filter != input.filter || (reinitialize && input.filter.is_some());
                page.input = input;
                let event = if reinitialize { "init" } else { "view_entered" };
                page.config_generation = self.generation;
                (page, event, restore_filter)
            } else {
                let mut existing = self
                    .resources
                    .active
                    .values()
                    .chain(self.resources.inactive.values());
                let parent = clone_from
                    .and_then(|origin| existing.clone().find(|page| page.page_id == origin.source));
                let model = parent.or_else(|| existing.find(|page| page.model_id == model_id));
                let (buffer, event, options, restore_filter) = if let Some(model) = model {
                    anyhow::ensure!(
                        model.model_id == model_id
                            && model.source == source
                            && model.text_source == text_source,
                        "split source does not match the new page"
                    );
                    if !native && model.config_generation != self.generation {
                        (
                            model.buffer,
                            "init",
                            parent.map_or_else(Default::default, |parent| parent.options.clone()),
                            input.filter.is_some(),
                        )
                    } else if parent.is_some() {
                        argument = json!({"source_view": model.view});
                        (
                            model.buffer,
                            "view_cloned",
                            model.options.clone(),
                            model.input.filter != input.filter,
                        )
                    } else {
                        (
                            model.buffer,
                            "view_entered",
                            Default::default(),
                            input.filter.is_some(),
                        )
                    }
                } else {
                    (
                        self.id(),
                        "init",
                        Default::default(),
                        input.filter.is_some(),
                    )
                };
                (
                    Subscription {
                        page_id,
                        model_id,
                        source,
                        buffer,
                        view: self.id(),
                        text_source,
                        text_version: None,
                        config_generation: self.generation,
                        input,
                        options,
                        watches: Vec::new(),
                        wake_at: None,
                        commit_source: None,
                        patches: Default::default(),
                    },
                    event,
                    restore_filter,
                )
            };
        let selection =
            (restore_filter || (retained && event == "init")).then(|| page.input.selection.clone());
        self.resources.active.insert(id, page);
        self.initialize_resource_page(id, event, argument, restore_filter, selection)
    }

    fn page_output(
        &mut self,
        id: u64,
        mut input: PluginInput,
        mut output: PluginOutput,
    ) -> Result<ResourceUpdate> {
        let page = self.resources.active.get_mut(&id).unwrap();
        let generation = page.input.generation;
        let document_scoped = page.text_source.is_some();
        if let Some(modified) = output.modified {
            self.resources.workspace_changed |= if modified {
                self.resources.modified_buffers.insert(page.buffer)
            } else {
                self.resources.modified_buffers.remove(&page.buffer)
            };
        } else {
            output.modified = Some(self.resources.modified_buffers.contains(&page.buffer));
        }
        let waiting_for_rows = output
            .display
            .as_ref()
            .is_some_and(|display| display.loading && display.rows.is_empty());
        if !waiting_for_rows || page.input.selection.primary.is_none() {
            if let Some(selection) = &output.selection {
                page.input.selection.clone_from(selection);
            }
            if let Some(display) = &output.display {
                page.input.selection.reconcile(&display.rows);
                if page.input.selection != *input.view_selection() || output.selection.is_some() {
                    output.selection = Some(page.input.selection.clone());
                }
            }
        } else {
            output.selection = None;
        }
        if page.input.selection.primary != input.view_selection().primary
            && let Some(pending) = page.commit_source.take()
        {
            self.services.borrow_mut().cancel_query(pending.handle);
        }
        if let Some(watches) = output.watches.take() {
            Self::observe_queries(&mut self.services.borrow_mut(), &mut page.watches, watches)?;
        }
        let runtime = self.scripts.as_mut().unwrap();
        let mut requests = Vec::new();
        for request in std::mem::take(&mut output.requests) {
            match request {
                HostRequest::Schedule { after_ms } => {
                    page.wake_at =
                        Some(Instant::now() + Duration::from_millis(after_ms.clamp(10, 60_000)));
                }
                HostRequest::ConfigureView { ref options } => {
                    runtime.validate_view_options_value(Some(&page.source.kind), options)?;
                    let change = options.as_object().unwrap();
                    input
                        .options
                        .as_object_mut()
                        .unwrap()
                        .extend(change.clone());
                    page.options.extend(change.clone());
                }
                _ => requests.push(request),
            }
        }
        let metadata = Some(page.metadata(input.options));
        let mut document = None;
        for request in requests {
            match self.page_document_request(request)? {
                documents::PageEffect::Request(request) => output.requests.push(request),
                documents::PageEffect::Document(opened) => {
                    document = Some(opened);
                    break;
                }
            }
        }
        let page = self.resources.active.get_mut(&id).unwrap();
        let patch_revision = if page.source.kind == "git" {
            page.patches.update(&output.requests)
        } else {
            None
        };
        Ok(ResourceUpdate {
            kind: ResourceUpdateKind::Page,
            subscription_id: id,
            generation,
            document_scoped,
            output,
            cache: Default::default(),
            metadata,
            patch_revision,
            document,
        })
    }

    fn dispatch_page(&mut self, id: u64, event: &str, argument: Value) -> Result<()> {
        self.dispatch_page_event(id, event, argument, None, None)
    }

    fn dispatch_page_event(
        &mut self,
        id: u64,
        event: &str,
        argument: Value,
        command: Option<&PageCommandInput>,
        prompt_query: Option<Query>,
    ) -> Result<()> {
        let runtime = self
            .scripts
            .as_ref()
            .context("plugin runtime unavailable")?;
        let page = self
            .resources
            .active
            .get(&id)
            .context("page subscription is closed")?;
        let kind = page.source.kind.clone();
        let fields = runtime.context_fields(&kind, event);
        let refresh_fields = runtime.context_fields(&kind, "refresh");
        let refresh_context_current = refresh_fields
            .iter()
            .filter(|field| field.starts_with("text."))
            .all(|field| fields.contains(field));
        if event == "render"
            && fields.iter().any(|field| field == "text.symbols")
            && refresh_fields.iter().any(|field| field == "text.symbols")
            && let Some(text) = page
                .text_source
                .and_then(|buffer| self.buffers.get_mut(&buffer))
                .and_then(Buffer::text)
            && !text.poll_outline_syntax()
        {
            return Ok(());
        }
        let options = page.effective_options(runtime);
        let mut input = PluginInput {
            buffer_id: page.buffer,
            view_id: page.view,
            generation: self.generation,
            preview_debounce: runtime.editor_options().preview_debounce,
            preview_area: Some(Rect {
                x: 0,
                y: 0,
                width: page.input.preview_width,
                height: page.input.preview_height,
            }),
            event: event.into(),
            argument,
            query: prompt_query
                .or_else(|| {
                    page.input
                        .filter
                        .as_ref()
                        .map(|filter| page_query(filter.kind, filter.text.clone()))
                })
                .or_else(|| (event == "filter_changed").then(Query::default)),
            options,
            mode: page.input.mode.clone(),
            selection: command
                .map_or(&page.input.selection, |command| &command.selection)
                .clone(),
            view_selection: command
                .filter(|command| command.selection != page.input.selection)
                .map(|_| page.input.selection.clone()),
            register: command.map_or(page.input.register, |command| command.register),
            context: PluginContext::default(),
        };
        input.context = self.plugin_context(page.text_source, &fields);
        let mut output = self.scripts.as_mut().unwrap().dispatch(&kind, &input)?;
        if refresh_context_current {
            self.resources.active.get_mut(&id).unwrap().text_version =
                input.context.text.as_ref().map(|text| text.version);
        }
        let changed_buffer = (output.changed && event != "render").then_some(input.buffer_id);
        let command_error = output.command_error.take();
        self.pending_operations
            .extend(std::mem::take(&mut output.pending_operations));
        let mut completed = std::mem::take(&mut output.completed_operations);
        let opened = completed.iter().position(|completion| {
            matches!(
                completion.result,
                zess_plugin::OperationResult::DocumentOpened
            )
        });
        let mut update = match self.page_output(id, input, output) {
            Ok(update) => update,
            Err(error) => {
                for completion in &mut completed {
                    completion.result = zess_plugin::OperationResult::Failed {
                        message: format!("{error:#}"),
                    };
                }
                self.complete_operations(completed);
                return Err(error);
            }
        };
        if let Some(index) = opened {
            let completion = completed.remove(index);
            let outcome = match update.document.take() {
                Some(document) => self.resources.page_outcome(id, Vec::new(), Some(document)),
                None => zess_protocol::CommandOutcome::Failed {
                    message: "missing opened page document".into(),
                },
            };
            self.finish_operation(completion.operation_id, outcome);
        }
        self.complete_operations(completed);
        if command.is_some()
            && let Some(operation_id) = self.current_operation.clone()
            && let Some(outcome) = self.resources.take_page_effects(&mut update)
        {
            self.finish_operation(operation_id, outcome);
        }
        self.resources.updates.push(update);
        if let Some(buffer) = changed_buffer {
            let siblings = self
                .resources
                .active
                .iter()
                .filter_map(|(&other, page)| {
                    (other != id && page.buffer == buffer).then_some(other)
                })
                .collect::<Vec<_>>();
            for other in siblings {
                self.dispatch_page(other, "render", Value::Null)?;
            }
        }
        if let Some(message) = command_error {
            bail!(message);
        }
        Ok(())
    }

    pub(super) fn submit_page_prompt(
        &mut self,
        subscription_id: u64,
        input: &PageCommandInput,
        purpose: &str,
        confirmation: Option<&str>,
        text: &str,
        cancelled: bool,
    ) -> Result<()> {
        self.dispatch_page_event(
            subscription_id,
            "layer_result",
            layer_result(purpose, text, cancelled, confirmation),
            Some(input),
            None,
        )
    }

    pub(crate) fn page_request(
        &mut self,
        subscription_id: u64,
        input: &PageCommandInput,
        request: HostRequest,
    ) -> Result<()> {
        let page = self
            .resources
            .active
            .get(&subscription_id)
            .context("page subscription is closed")?;
        match request {
            HostRequest::BufferEvent {
                kind,
                event,
                argument,
            } => {
                anyhow::ensure!(page.source.kind == kind, "command requires a {kind} buffer");
                if kind == "files" && matches!(event.as_str(), "left" | "right" | "toggle") {
                    let page = self.resources.active.get_mut(&subscription_id).unwrap();
                    page.input.generation = page.input.generation.max(input.generation);
                }
                let result =
                    self.dispatch_page_event(subscription_id, &event, argument, Some(input), None);
                let current = &mut self
                    .resources
                    .active
                    .get_mut(&subscription_id)
                    .unwrap()
                    .input;
                if current.generation == input.generation {
                    current.register = None;
                }
                result
            }
            HostRequest::Action { name, .. } if matches!(name.as_str(), "write" | "write-quit") => {
                self.dispatch_page(subscription_id, "save", Value::Null)?;
                if name == "write-quit"
                    && !self
                        .resources
                        .modified_buffers
                        .contains(&self.resources.active[&subscription_id].buffer)
                {
                    self.page_request(subscription_id, input, HostRequest::Close)?;
                }
                Ok(())
            }
            HostRequest::Yank { values, register } => self.yank(values, register.unwrap_or('"')),
            request => {
                let generation = page.input.generation;
                let document_scoped = page.text_source.is_some();
                let (requests, document, metadata) =
                    if let HostRequest::ConfigureView { options } = request {
                        let runtime = self
                            .scripts
                            .as_ref()
                            .context("plugin runtime unavailable")?;
                        runtime.validate_view_options_value(Some(&page.source.kind), &options)?;
                        let page = self.resources.active.get_mut(&subscription_id).unwrap();
                        page.options.extend(options.as_object().unwrap().clone());
                        let metadata = page.metadata(page.effective_options(runtime));
                        (Vec::new(), None, Some(metadata))
                    } else {
                        match self.page_document_request(request)? {
                            documents::PageEffect::Request(request) => (vec![request], None, None),
                            documents::PageEffect::Document(document) => {
                                (Vec::new(), Some(document), None)
                            }
                        }
                    };
                let page = self.resources.active.get_mut(&subscription_id).unwrap();
                let patch_revision = if page.source.kind == "git" {
                    page.patches.update(&requests)
                } else {
                    None
                };
                let mut update = ResourceUpdate {
                    kind: ResourceUpdateKind::Page,
                    subscription_id,
                    generation,
                    document_scoped,
                    patch_revision,
                    metadata,
                    document,
                    cache: Default::default(),
                    output: PluginOutput {
                        requests,
                        ..PluginOutput::default()
                    },
                };
                if self.current_operation.is_some()
                    && let Some(outcome) = self.resources.take_page_effects(&mut update)
                {
                    self.operation_outcome = Some(outcome);
                }
                self.resources.updates.push(update);
                Ok(())
            }
        }
    }

    pub(crate) fn handle_resource(&mut self, request: ResourceRequest) {
        let (id, generation, result) = match request {
            ResourceRequest::CacheInterest { entries } => {
                self.resources.cache.set_interest(entries);
                return;
            }
            ResourceRequest::Subscribe {
                subscription_id,
                page_id,
                clone_from,
                source,
                input,
            } => (
                subscription_id,
                input.generation,
                self.open_resource(subscription_id, page_id, clone_from, source, input),
            ),
            ResourceRequest::Update {
                subscription_id,
                event,
                input,
            } => {
                let Some(page) = self.resources.active.get_mut(&subscription_id) else {
                    return;
                };
                if input.generation < page.input.generation {
                    return;
                }
                let generation = input.generation;
                if event.supersedes_source_navigation()
                    && let Some(pending) = page.commit_source.take()
                {
                    self.services.borrow_mut().cancel_query(pending.handle);
                }
                if matches!(event, ResourceEvent::Register) {
                    page.input.generation = generation;
                    page.input.register = input.register;
                } else {
                    page.input = input;
                }
                let (name, argument, prompt_query) = match event {
                    ResourceEvent::Register => return,
                    ResourceEvent::OpenSource {
                        path,
                        line,
                        fullscreen,
                    } => {
                        let result = self.open_page_worktree_source(
                            &path,
                            line,
                            if fullscreen {
                                SnapshotPresentation::Fullscreen
                            } else {
                                SnapshotPresentation::Editor
                            },
                        );
                        self.publish_source_navigation(subscription_id, generation, result);
                        return;
                    }
                    ResourceEvent::CommitSource {
                        commit,
                        path,
                        line,
                        fullscreen,
                    } => {
                        let result = self
                            .services
                            .borrow_mut()
                            .start_query(zess_services::QueryRequest::Blob { commit, path });
                        match result {
                            Ok(handle) => {
                                page.commit_source = Some(PendingSource {
                                    handle,
                                    generation,
                                    line,
                                    fullscreen,
                                })
                            }
                            Err(error) => self.publish_source_navigation(
                                subscription_id,
                                generation,
                                Err(error),
                            ),
                        }
                        return;
                    }

                    ResourceEvent::Select { navigation } => (
                        "select",
                        json!(SelectionChange {
                            navigation: Some(navigation)
                        }),
                        None,
                    ),
                    ResourceEvent::MiniBufferChanged {
                        purpose,
                        text,
                        query_kind,
                    } => (
                        "minibuffer_changed",
                        json!({"purpose": purpose, "text": text}),
                        Some(page_query(query_kind, text)),
                    ),
                    ResourceEvent::FilterChanged => ("filter_changed", Value::Null, None),
                    ResourceEvent::ModeChanged => ("mode_changed", Value::Null, None),
                    ResourceEvent::Resize => ("render", Value::Null, None),
                    ResourceEvent::Refresh => ("refresh", Value::Null, None),
                };
                (
                    subscription_id,
                    generation,
                    self.dispatch_page_event(subscription_id, name, argument, None, prompt_query),
                )
            }
            ResourceRequest::Discard { page_id } => {
                self.discard_page(page_id);
                return;
            }
            ResourceRequest::Release { subscription_id } => {
                self.release_page(subscription_id);
                return;
            }
        };
        if let Err(error) = result {
            self.resources.publish_error(id, generation, error);
        }
    }

    pub(super) fn reload_resource_pages(&mut self) -> Result<()> {
        self.resources.cache.invalidate_all();
        let ids = self.resources.active.keys().copied().collect::<Vec<_>>();
        let mut failure = None;
        for id in ids {
            let page = self.resources.active.get_mut(&id).unwrap();
            page.config_generation = self.generation;
            let generation = page.input.generation;
            let result = if self
                .scripts
                .as_ref()
                .unwrap()
                .is_native_buffer(&page.source.kind)
            {
                self.dispatch_page(id, "render", Value::Null)
            } else {
                page.watches.clear();
                page.wake_at = None;
                let argument = page.source.argument.clone();
                let restore_filter = page.input.filter.is_some();
                let selection = page.input.selection.clone();
                self.initialize_resource_page(id, "init", argument, restore_filter, Some(selection))
            };
            if let Err(error) = result {
                let kind = &self.resources.active[&id].source.kind;
                let error = error.context(format!(
                    "could not refresh {kind} page after installing the configuration"
                ));
                failure.get_or_insert_with(|| anyhow::anyhow!("{error:#}"));
                self.resources.publish_error(id, generation, error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn tick_resources(&mut self, filesystem_changed: bool) {
        self.resources.cache.poll();
        self.poll_page_sources();
        let now = Instant::now();
        let mut events = Vec::new();
        for (&id, page) in &mut self.resources.active {
            let queries = page
                .watches
                .iter_mut()
                .filter_map(|(query, observation)| {
                    if observation.changed() {
                        observation.acknowledge();
                        Some(query.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            let refresh_fs = filesystem_changed
                && self.scripts.as_ref().is_some_and(|runtime| {
                    runtime
                        .context_fields(&page.source.kind, "fs_changed")
                        .iter()
                        .any(|field| field == "fs.revision")
                });
            if refresh_fs || !queries.is_empty() {
                events.push((
                    id,
                    "fs_changed",
                    if refresh_fs {
                        Value::Null
                    } else {
                        json!({"queries": queries})
                    },
                ));
            }
            let mut text = page
                .text_source
                .and_then(|buffer| self.buffers.get_mut(&buffer))
                .and_then(Buffer::text);
            let version = text.as_ref().map(|text| text.version());
            if version != page.text_version
                && let Some(runtime) = &self.scripts
            {
                let fields = runtime.context_fields(&page.source.kind, "refresh");
                if fields.iter().any(|field| field.starts_with("text."))
                    && (!fields.iter().any(|field| field == "text.symbols")
                        || text.as_mut().is_none_or(|text| text.poll_outline_syntax()))
                {
                    events.push((id, "refresh", Value::Null));
                }
            }
            if page.wake_at.is_some_and(|when| when <= now) {
                page.wake_at = None;
                events.push((id, "tick", Value::Null));
            }
        }
        for (id, name, argument) in events {
            if let Err(error) = self.dispatch_page(id, name, argument) {
                let generation = self.resources.active[&id].input.generation;
                self.resources.publish_error(id, generation, error);
            }
        }
    }

    pub(crate) fn take_resource_updates(&mut self) -> Vec<ResourceUpdate> {
        let mut updates = std::mem::take(&mut self.resources.updates);
        for update in &mut updates {
            if matches!(update.kind, ResourceUpdateKind::Page)
                && let Some(page) = self.resources.active.get(&update.subscription_id)
            {
                self.resources.cache.prepare(page, update);
            }
        }
        updates
    }

    pub(crate) fn take_resource_invalidations(
        &mut self,
    ) -> Vec<(ResourceCacheInvalidation, PageCacheEntry)> {
        self.resources.cache.take_invalidated()
    }
}
