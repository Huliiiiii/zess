use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result, ensure};
use zess_plugin::CommandCount;
use zess_protocol::{
    document::{
        DocumentEvent, DocumentHistoryAction, DocumentRequest, DocumentSnapshot, DocumentStream,
    },
    workspace_view::{DocumentPane, DocumentView, DocumentViewState},
};
use zess_text::{
    DocumentStep, InsertHistory, Registers, RenderResult, TextBuffer, TextCommand, TextEffect,
};

#[cfg(test)]
mod tests;

enum Action {
    Step(DocumentStep),
    Save {
        path: Option<String>,
        close_view: Option<u64>,
        history_revision: usize,
    },
}

enum Dispatch {
    Queued,
    Sent(u64),
    // A replay must use the original base version.
    Replay(u64),
}

struct Pending {
    sequence: u64,
    action: Action,
    dispatch: Dispatch,
}

enum Recovery {
    Ready,
    AwaitingSnapshot,
    Conflict(DocumentSnapshot),
}

enum Reload {
    Queued,
    Submitted,
    AwaitingSnapshot,
}

struct Replica {
    text: TextBuffer,
    views: BTreeSet<u64>,
    version: u64,
    modified: bool,
    last_sequence: u64,
    pending: VecDeque<Pending>,
    observed: Option<DocumentSnapshot>,
    recovery: Recovery,
    reload: Option<Reload>,
}

impl Replica {
    fn new(snapshot: &DocumentSnapshot, mut text: TextBuffer) -> Result<Self> {
        text.restore_history(&snapshot.history)?;
        Ok(Self {
            text,
            views: BTreeSet::new(),
            version: snapshot.version,
            modified: snapshot.modified,
            last_sequence: 0,
            pending: VecDeque::new(),
            observed: None,
            recovery: Recovery::Ready,
            reload: None,
        })
    }

    fn queue(&mut self, action: Action) {
        self.last_sequence += 1;
        self.pending.push_back(Pending {
            sequence: self.last_sequence,
            action,
            dispatch: Dispatch::Queued,
        });
    }

    fn queue_recorded_steps(&mut self) {
        for step in self.text.take_document_steps() {
            self.queue(Action::Step(step));
        }
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty() || self.text.has_document_steps()
    }

    fn modified(&self) -> bool {
        self.modified
            || self.text.has_document_steps()
            || self.pending.iter().any(|p| {
                matches!(
                    p.action,
                    Action::Step(
                        DocumentStep::Edit { .. }
                            | DocumentStep::History(
                                DocumentHistoryAction::Undo | DocumentHistoryAction::Redo
                            )
                    )
                )
            })
    }

    fn command_ready(&self) -> bool {
        !self.has_pending() && matches!(self.reload, None | Some(Reload::Queued))
    }

    fn metadata(&mut self, snapshot: &DocumentSnapshot) -> Option<String> {
        self.text.set_replica_path(snapshot.path.as_deref());
        if self.text.readonly() != snapshot.readonly {
            self.text.set_readonly(snapshot.readonly);
        }
        let language = snapshot.language.as_deref();
        if self.text.language_id() != language
            && let Err(error) = self.text.set_language(language)
        {
            self.text
                .set_language(None)
                .expect("plain text requires no language resources");
            return Some(error.to_string());
        }
        None
    }

    fn applied_snapshot(&mut self, snapshot: &DocumentSnapshot) -> Result<()> {
        self.text.replace_from_replica(&snapshot.text);
        self.text.restore_history(&snapshot.history)?;
        self.version = snapshot.version;
        self.modified = snapshot.modified;
        Ok(())
    }

    fn observe(&mut self, snapshot: DocumentSnapshot) -> Result<bool> {
        if !matches!(self.recovery, Recovery::Ready) {
            self.recovery = Recovery::Conflict(snapshot);
            return Ok(true);
        }
        if self
            .pending
            .front()
            .is_some_and(|p| !matches!(p.dispatch, Dispatch::Queued))
        {
            // A reconnect snapshot may already include our unacknowledged edit.
            // The replayed receipt distinguishes that from an independent edit.
            self.observed = Some(snapshot);
        } else if !self.has_pending() {
            if snapshot.version != self.version {
                self.applied_snapshot(&snapshot)?;
            } else {
                self.modified = snapshot.modified;
            }
        } else if snapshot.version != self.version {
            self.recovery = Recovery::Conflict(snapshot);
            return Ok(true);
        }
        Ok(false)
    }

    fn sent_action(&self, sequence: u64) -> Option<&Action> {
        let pending = self.pending.front()?;
        (pending.sequence == sequence && matches!(pending.dispatch, Dispatch::Sent(_)))
            .then_some(&pending.action)
    }

    fn reset_dispatch(&mut self) {
        if let Some(pending) = self.pending.front_mut()
            && let Dispatch::Sent(version) = pending.dispatch
        {
            pending.dispatch = Dispatch::Replay(version);
        }
    }

    fn complete(&mut self, version: u64, modified: bool) -> Result<bool> {
        self.pending.pop_front();
        self.version = version;
        self.modified = modified;
        if let Some(snapshot) = self.observed.take()
            && snapshot.version > version
        {
            return self.observe(snapshot);
        }
        Ok(false)
    }

    fn request(&mut self, buffer_id: u64) -> Option<DocumentRequest> {
        if !matches!(self.recovery, Recovery::Ready) {
            return None;
        }
        if self.pending.is_empty() {
            self.queue_recorded_steps();
        }
        let pending = self.pending.front_mut()?;
        let version = match pending.dispatch {
            Dispatch::Queued => self.version,
            Dispatch::Replay(version) => version,
            Dispatch::Sent(_) => return None,
        };
        pending.dispatch = Dispatch::Sent(version);
        Some(match &pending.action {
            Action::Step(DocumentStep::Edit { changes, origin }) => DocumentRequest::Edit {
                buffer_id,
                sequence: pending.sequence,
                base_version: version,
                changes: changes.clone(),
                origin: origin.clone(),
            },
            Action::Step(DocumentStep::History(action)) => DocumentRequest::History {
                buffer_id,
                sequence: pending.sequence,
                base_version: version,
                action: action.clone(),
            },
            Action::Save { path, .. } => DocumentRequest::Save {
                buffer_id,
                sequence: pending.sequence,
                version,
                path: path.clone(),
            },
        })
    }

    fn ensure_view(&mut self, view: u64) {
        if self.views.insert(view) {
            self.text.ensure_view(view);
        }
    }

    fn edit<T>(&mut self, edit: impl FnOnce(&mut TextBuffer) -> T) -> T {
        if !self.text.has_document_steps() {
            self.text.track_changes();
        }
        let result = edit(&mut self.text);
        if !self.text.has_document_steps() {
            self.text.take_document_steps();
        }
        result
    }

    fn remove_view(&mut self, view: u64) {
        if self.views.remove(&view) {
            self.edit(|text| text.remove_view(view));
        }
        for pending in &mut self.pending {
            if let Action::Save { close_view, .. } = &mut pending.action
                && *close_view == Some(view)
            {
                *close_view = None;
            }
        }
    }
}

#[derive(Default)]
pub(super) struct DocumentUpdate {
    pub(super) notice: Option<String>,
    pub(super) close: Option<DocumentPane>,
}

enum SubscriptionRead {
    Queued,
    Waiting,
    Settled,
}

#[derive(Default)]
pub(crate) struct Documents {
    copies: BTreeMap<u64, Replica>,
    closed: BTreeMap<u64, String>,
    // Open documents remain subscribed until their workspace buffer closes.
    subscriptions: BTreeMap<u64, SubscriptionRead>,
    stream: DocumentStream,
    registers: Registers,
    history: InsertHistory,
}

fn conflict_notice(buffer: u64) -> String {
    format!(
        "Document {buffer} changed in the workspace. Use :document.keep-local to keep your edits or :document.use-workspace to load the workspace version."
    )
}

impl Documents {
    pub(crate) fn draft(&self, value: &str) -> TextBuffer {
        let mut text = TextBuffer::new(value);
        text.set_registers(self.registers.clone());
        text.set_insert_history(self.history.clone());
        text
    }

    pub(crate) fn open(&mut self, buffer: u64) {
        self.subscriptions
            .entry(buffer)
            .or_insert(SubscriptionRead::Queued);
    }

    pub(crate) fn remove_view(&mut self, view: u64) {
        for doc in self.copies.values_mut() {
            doc.remove_view(view);
        }
    }

    pub(crate) fn retain(&mut self, buffers: &[u64]) {
        self.subscriptions.retain(|id, _| buffers.contains(id));
        self.copies.retain(|id, doc| {
            if buffers.contains(id) {
                return true;
            }
            if doc.has_pending() {
                self.closed.insert(*id, doc.text.text());
            }
            false
        });
    }

    pub(crate) fn has_closed_edits(&self) -> bool {
        !self.closed.is_empty()
    }

    pub(crate) fn closed_notice(&self) -> Option<String> {
        let (buffer, _) = self.closed.first_key_value()?;
        Some(format!(
            "Closed {buffer} · copy :document.keep-local {buffer} · discard :document.use-workspace {buffer}"
        ))
    }

    pub(crate) fn recover_closed(&mut self, buffer: u64, keep_local: bool) -> Result<String> {
        let text = self
            .closed
            .get(&buffer)
            .with_context(|| format!("no local edits to recover for closed buffer {buffer}"))?;
        if keep_local {
            self.registers.write('"', vec![text.clone()])?;
            Ok(format!(
                "Copied buffer {buffer} to the unnamed register. Paste with p."
            ))
        } else {
            self.closed.remove(&buffer);
            Ok(format!("Discarded local edits for closed buffer {buffer}."))
        }
    }

    pub(crate) fn reconnect(&mut self) {
        self.stream = DocumentStream::default();
        for read in self.subscriptions.values_mut() {
            *read = SubscriptionRead::Queued;
        }
        for doc in self.copies.values_mut() {
            doc.reset_dispatch();
            doc.observed = None;
        }
    }

    pub(crate) fn requests(&mut self) -> Vec<DocumentRequest> {
        let mut requests = Vec::new();
        for (&buffer_id, read) in &mut self.subscriptions {
            if matches!(read, SubscriptionRead::Queued) {
                *read = SubscriptionRead::Waiting;
                requests.push(DocumentRequest::Read { buffer_id });
            }
            if let Some(doc) = self.copies.get_mut(&buffer_id)
                && let Some(request) = doc.request(buffer_id)
            {
                requests.extend(request.into_wire_requests());
            }
        }
        requests
    }

    pub(super) fn receive(&mut self, event: DocumentEvent) -> DocumentUpdate {
        let Some(event) = self.stream.receive(event) else {
            return DocumentUpdate::default();
        };
        match self.apply_event(event) {
            Ok(update) => update,
            Err(error) => DocumentUpdate {
                notice: Some(format!("{error:#}")),
                close: None,
            },
        }
    }

    fn apply_event(&mut self, event: DocumentEvent) -> Result<DocumentUpdate> {
        let mut update = DocumentUpdate::default();
        update.notice = match event {
            DocumentEvent::Snapshot(snapshot) => {
                let id = snapshot.buffer_id;
                let Some(read) = self.subscriptions.get_mut(&id) else {
                    return Ok(update);
                };
                if matches!(read, SubscriptionRead::Waiting) {
                    *read = SubscriptionRead::Settled;
                }
                if let Some(doc) = self.copies.get_mut(&id) {
                    let notice = doc.metadata(&snapshot);
                    if matches!(doc.reload, Some(Reload::AwaitingSnapshot)) {
                        doc.reload = None;
                        doc.applied_snapshot(&snapshot)?;
                        notice
                    } else if doc.observe(snapshot)? {
                        Some(conflict_notice(id))
                    } else {
                        notice
                    }
                } else {
                    let mut doc = Replica::new(&snapshot, self.draft(&snapshot.text))?;
                    let notice = doc.metadata(&snapshot);
                    self.copies.insert(id, doc);
                    notice
                }
            }
            DocumentEvent::Applied {
                buffer_id,
                sequence,
                version,
                modified,
            } => {
                let Some(doc) = self.copies.get_mut(&buffer_id) else {
                    return Ok(update);
                };
                if !matches!(doc.sent_action(sequence), Some(Action::Step(_))) {
                    return Ok(update);
                }
                doc.complete(version, modified)?
                    .then(|| conflict_notice(buffer_id))
            }
            DocumentEvent::Saved {
                buffer_id,
                sequence,
                version,
            } => {
                let Some(doc) = self.copies.get_mut(&buffer_id) else {
                    return Ok(update);
                };
                let Some(&Action::Save {
                    close_view,
                    history_revision,
                    ..
                }) = doc.sent_action(sequence)
                else {
                    return Ok(update);
                };
                doc.text.mark_history_saved(history_revision);
                let conflicted = doc.complete(version, false)?;
                if let Some(view_id) = close_view
                    && !conflicted
                    && !doc.modified()
                {
                    update.close = Some(DocumentPane { buffer_id, view_id });
                }
                conflicted.then(|| conflict_notice(buffer_id))
            }
            DocumentEvent::Conflict {
                buffer_id,
                sequence,
                current,
            } => {
                let Some(doc) = self.copies.get_mut(&buffer_id) else {
                    return Ok(update);
                };
                if doc.sent_action(sequence).is_none() {
                    return Ok(update);
                }
                doc.reset_dispatch();
                doc.observed = None;
                doc.metadata(&current);
                doc.recovery = Recovery::Conflict(current);
                Some(conflict_notice(buffer_id))
            }
            DocumentEvent::Failed {
                buffer_id,
                sequence,
                message,
            } => {
                if sequence == 0 {
                    if let Some(read @ SubscriptionRead::Waiting) =
                        self.subscriptions.get_mut(&buffer_id)
                    {
                        *read = SubscriptionRead::Settled;
                    }
                    if let Some(doc) = self.copies.get_mut(&buffer_id)
                        && matches!(doc.reload, Some(Reload::AwaitingSnapshot))
                    {
                        doc.reload = None;
                    }
                    update.notice = Some(message);
                    return Ok(update);
                }
                let Some(doc) = self.copies.get_mut(&buffer_id) else {
                    return Ok(update);
                };
                let Some(action) = doc.sent_action(sequence) else {
                    return Ok(update);
                };
                match action {
                    Action::Step(_) => {
                        doc.reset_dispatch();
                        doc.recovery = Recovery::AwaitingSnapshot;
                        self.subscriptions
                            .insert(buffer_id, SubscriptionRead::Queued);
                    }
                    Action::Save { .. } => {
                        doc.pending.pop_front();
                        if let Some(snapshot) = doc.observed.take()
                            && doc.observe(snapshot)?
                        {
                            update.notice =
                                Some(format!("{message}. {}", conflict_notice(buffer_id)));
                            return Ok(update);
                        }
                    }
                }
                Some(message)
            }
            DocumentEvent::Chunk { .. } => unreachable!("DocumentStream assembles chunked events"),
        };
        Ok(update)
    }

    pub(crate) fn contains(&self, buffer: u64) -> bool {
        self.copies.contains_key(&buffer)
    }

    pub(crate) fn is_waiting_for_document(&self, buffer: u64) -> bool {
        !self.copies.contains_key(&buffer)
            && self
                .subscriptions
                .get(&buffer)
                .is_some_and(|read| !matches!(read, SubscriptionRead::Settled))
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.copies
            .values()
            .any(|doc| doc.has_pending() || doc.reload.is_some())
    }

    pub(crate) fn command_ready(&self, buffer: u64) -> bool {
        self.copies.get(&buffer).is_none_or(Replica::command_ready)
    }

    pub(super) fn workspace_command_ready(&self) -> bool {
        // Reloads awaiting dispatch follow the command queue's order.
        !self.has_closed_edits() && self.copies.values().all(Replica::command_ready)
    }

    pub(crate) fn is_reloading(&self, buffer: u64) -> bool {
        self.copies
            .get(&buffer)
            .is_some_and(|doc| doc.reload.is_some())
    }

    pub(super) fn queue_reload(&mut self, buffer: u64) -> Result<()> {
        let doc = self
            .copies
            .get_mut(&buffer)
            .context("document has not loaded")?;
        doc.queue_recorded_steps();
        doc.reload = Some(Reload::Queued);
        Ok(())
    }

    pub(super) fn cancel_queued_reload(&mut self, buffer: u64) {
        if let Some(doc) = self.copies.get_mut(&buffer)
            && matches!(doc.reload, Some(Reload::Queued))
        {
            doc.reload = None;
        }
    }

    pub(super) fn reload_submitted(&mut self, buffer: u64) {
        if let Some(doc) = self.copies.get_mut(&buffer)
            && matches!(doc.reload, Some(Reload::Queued))
        {
            doc.reload = Some(Reload::Submitted);
        }
    }

    pub(super) fn reload_result(&mut self, buffer: u64) {
        if let Some(doc) = self.copies.get_mut(&buffer)
            && matches!(doc.reload, Some(Reload::Submitted))
        {
            // Results and snapshots share the same ordered connection. A snapshot
            // received after the result reflects the completed command.
            // Read explicitly because reload can leave the document version unchanged.
            doc.reload = Some(Reload::AwaitingSnapshot);
            self.subscriptions.insert(buffer, SubscriptionRead::Queued);
        }
    }

    pub(crate) fn resolve(&mut self, buffer: u64, keep_local: bool) -> Result<()> {
        let doc = self
            .copies
            .get_mut(&buffer)
            .context("document has not loaded")?;
        ensure!(
            matches!(doc.recovery, Recovery::Conflict(_)),
            "no document conflict to resolve"
        );
        let Recovery::Conflict(remote) = std::mem::replace(&mut doc.recovery, Recovery::Ready)
        else {
            unreachable!()
        };
        doc.text.take_document_steps();
        doc.pending.clear();
        doc.observed = None;
        let local = keep_local.then(|| {
            let views = doc
                .views
                .iter()
                .map(|&id| (id, doc.text.view(id).workspace_state()))
                .collect::<Vec<_>>();
            (doc.text.text(), views)
        });
        doc.applied_snapshot(&remote)?;
        if let Some((local, views)) = local
            && local != remote.text
        {
            doc.text.track_changes();
            doc.text.replace_from_replica(&local);
            for step in doc.text.take_document_steps() {
                doc.queue(Action::Step(step));
            }
            for (id, state) in views {
                doc.text.restore_view(
                    id,
                    &state.ranges,
                    state.primary,
                    &state.mode,
                    state.scroll,
                    state.horizontal_scroll,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn edit(
        &mut self,
        buffer: u64,
        view: u64,
        edit: impl FnOnce(&mut TextBuffer) -> Result<TextEffect>,
    ) -> Result<TextEffect> {
        let doc = self
            .copies
            .get_mut(&buffer)
            .context("document has not loaded")?;
        doc.ensure_view(view);
        doc.edit(edit)
    }

    pub(crate) fn text(&self, buffer: u64) -> Option<&TextBuffer> {
        Some(&self.copies.get(&buffer)?.text)
    }

    pub(crate) fn edit_existing_view<T>(
        &mut self,
        buffer: u64,
        view: u64,
        edit: impl FnOnce(&mut TextBuffer) -> T,
    ) -> Option<T> {
        let doc = self.copies.get_mut(&buffer)?;
        doc.views.contains(&view).then(|| doc.edit(edit))
    }

    pub(crate) fn yank(&self, register: char, values: Vec<String>) -> Result<Option<String>> {
        self.registers.write(register, values)
    }

    pub(crate) fn poll_syntax(&mut self) -> bool {
        let mut changed = false;
        for doc in self.copies.values_mut() {
            changed |= doc.text.poll_syntax();
        }
        changed
    }

    fn edit_input(
        &mut self,
        buffer: u64,
        view: u64,
        edit: impl FnOnce(&mut TextBuffer) -> Result<TextEffect>,
    ) -> Result<TextEffect> {
        if self.is_reloading(buffer) {
            return Ok(TextEffect::default());
        }
        self.edit(buffer, view, edit)
    }

    pub(crate) fn execute(
        &mut self,
        buffer: u64,
        view: u64,
        command: TextCommand,
    ) -> Result<TextEffect> {
        self.edit_input(buffer, view, |text| text.execute(view, command))
    }

    pub(crate) fn input(&mut self, buffer: u64, view: u64, key: &str) -> Result<TextEffect> {
        self.edit_input(buffer, view, |text| text.input_text(view, key))
    }

    pub(crate) fn paste(&mut self, buffer: u64, view: u64, value: &str) -> Result<TextEffect> {
        self.edit_input(buffer, view, |text| text.paste_text(view, value))
    }

    pub(super) fn save(
        &mut self,
        buffer: u64,
        path: Option<String>,
        close_view: Option<u64>,
    ) -> Result<()> {
        let doc = self
            .copies
            .get_mut(&buffer)
            .context("document has not loaded")?;
        ensure!(!doc.text.readonly(), "buffer is read-only");
        ensure!(
            matches!(doc.recovery, Recovery::Ready),
            "resolve the document conflict before saving"
        );
        if !doc.text.has_document_steps() {
            doc.text.track_changes();
        }
        doc.text.finish_document_edit();
        doc.queue_recorded_steps();
        doc.queue(Action::Save {
            path,
            close_view,
            history_revision: doc.text.history_revision(),
        });
        Ok(())
    }

    pub(crate) fn render(
        &mut self,
        buffer: u64,
        view: u64,
        width: usize,
        height: usize,
        focused: bool,
    ) -> Option<RenderResult> {
        let doc = self.copies.get_mut(&buffer)?;
        doc.ensure_view(view);
        Some(doc.text.render(view, width, height, focused))
    }

    pub(crate) fn line_count(&self, buffer: u64) -> Option<usize> {
        Some(self.copies.get(&buffer)?.text.line_count())
    }

    pub(crate) fn modified(&self, buffer: u64) -> Option<bool> {
        self.copies.get(&buffer).map(Replica::modified)
    }

    fn view(&self, buffer: u64, view: u64) -> Option<&zess_text::TextView> {
        let doc = self.copies.get(&buffer)?;
        doc.views.contains(&view).then(|| doc.text.view(view))
    }

    pub(crate) fn mode(&self, buffer: u64, view: u64) -> Option<&str> {
        Some(self.view(buffer, view)?.mode())
    }

    pub(super) fn command_count(&self, buffer: u64, view: u64) -> CommandCount {
        self.view(buffer, view)
            .map_or_else(CommandCount::default, zess_text::TextView::command_count)
    }

    pub(crate) fn position(&self, buffer: u64, view: u64) -> Option<(usize, usize)> {
        Some(self.view(buffer, view)?.cursor())
    }

    pub(crate) fn selection_count(&self, buffer: u64, view: u64) -> Option<usize> {
        Some(self.view(buffer, view)?.selection_count())
    }

    pub(crate) fn has_pending_input(&self, buffer: u64, view: u64) -> bool {
        self.copies
            .get(&buffer)
            .is_some_and(|doc| doc.text.has_pending_input(view))
    }

    pub(crate) fn restore_view(
        &mut self,
        buffer: u64,
        view: u64,
        state: &DocumentViewState,
    ) -> Result<()> {
        let doc = self
            .copies
            .get_mut(&buffer)
            .context("document has not loaded")?;
        doc.ensure_view(view);
        doc.text.restore_view(
            view,
            &state.ranges,
            state.primary,
            &state.mode,
            state.scroll,
            state.horizontal_scroll,
        )
    }

    pub(crate) fn view_state(&self, buffer: u64, view: u64) -> Option<DocumentViewState> {
        Some(self.view(buffer, view)?.workspace_state())
    }

    pub(crate) fn view_states(&self, live_views: &[u64]) -> Vec<DocumentView> {
        let mut states = Vec::new();
        for (&buffer_id, doc) in &self.copies {
            for &view_id in &doc.views {
                if live_views.contains(&view_id)
                    && let Some(state) = self.view_state(buffer_id, view_id)
                {
                    states.push(DocumentView {
                        buffer_id,
                        view_id,
                        state,
                    });
                }
            }
        }
        states
    }
}
