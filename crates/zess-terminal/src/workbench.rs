use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use zess_filter::QueryKind;
use zess_plugin::{CommandCount, HostRequest, Navigation, PreviewArea};
use zess_protocol::{
    BufferKind, CommandAction, CommandTarget, SessionSnapshot, ViewSelection, ViewSnapshot,
    bindings::BindingCommand,
    document::DocumentEvent,
    resource::{
        FilterKind, PageCacheEntry, PageDocument, PageFilter, PageId, ResourceCacheInvalidation,
        ResourceUpdate,
    },
    workspace_view::{
        Axis, Direction, DocumentLayer, DocumentPane, DocumentPanes, DocumentViewState, PanePages,
        WorkspaceView,
    },
};
use zess_text::{ClipboardRead, TextCommand, TextEffect, TextPromptKind};
use zess_ui::{MouseEvent, MouseKind};

use crate::{
    documents::{DocumentUpdate, Documents},
    editor::{ClientEditor, ClientInput, PromptEvent},
    pages::{PageMouseHit, Pages},
};

mod events;
pub(super) mod host;
mod layer;
use layer::Draft;
mod panes;
mod presentation;
use presentation::DocumentHits;

#[cfg(test)]
mod tests;

pub(super) enum Effect {
    Command(CommandAction),
    OpenDocument { view_id: u64, path: Option<String> },
    ClipboardRead { id: u64, primary: bool },
    ClipboardWrite { primary: bool, text: String },
    Notice(String),
}

#[derive(Clone, Copy)]
enum TextTarget {
    Document(DocumentPane),
    Preview(u64),
    Draft,
}

impl From<DocumentPane> for TextTarget {
    fn from(pane: DocumentPane) -> Self {
        Self::Document(pane)
    }
}

enum PromptTarget {
    Text(TextTarget, TextPromptKind),
    Filter(Option<PageFilter>),
    Page {
        purpose: String,
        confirmation: Option<String>,
    },
}

#[derive(Default)]
pub(super) struct Workbench {
    pub(super) editor: ClientEditor,
    pub(super) documents: Documents,
    pub(super) pages: Pages,
    other_pages: BTreeMap<u64, Pages>,
    page_drag: Option<u64>,
    panes: Option<DocumentPanes>,
    hits: DocumentHits,
    pub(crate) statuslines: crate::statusline::Statuslines,
    width: u16,
    height: u16,
    // A view can arrive before its document is fully transferred.
    positions: BTreeMap<(u64, u64), DocumentViewState>,
    host_document: Option<u64>,
    discarding_scratch: BTreeMap<u64, u64>,
    page_document: Option<DocumentLayer>,
    initial_lines: BTreeMap<(u64, u64), usize>,
    prompt: Option<PromptTarget>,
    clipboard: Option<(TextTarget, ClipboardRead)>,
    draft: Option<Draft>,
    submitted_prompt: Option<(u64, String)>,
    page_count: CommandCount,
}

fn host_view(state: &SessionSnapshot) -> Option<&ViewSnapshot> {
    state
        .views
        .iter()
        .find(|view| Some(view.view_id) == state.focused_view)
}

fn is_document(state: &SessionSnapshot, buffer: u64) -> bool {
    state.buffers.iter().any(|summary| {
        summary.id == buffer
            && matches!(summary.kind, BufferKind::Text { .. } | BufferKind::Snapshot)
    })
}

fn position(view: &ViewSnapshot) -> Option<DocumentViewState> {
    let ViewSelection::Text { ranges, primary } = &view.selection else {
        return None;
    };
    Some(DocumentViewState {
        ranges: ranges.clone(),
        primary: *primary,
        mode: view.mode.clone(),
        scroll: view.scroll,
        horizontal_scroll: view.horizontal_scroll,
    })
}

fn document_area(width: u16, height: u16) -> PreviewArea {
    PreviewArea {
        x: 0,
        y: 0,
        width: width.into(),
        height: height.saturating_sub(1).into(),
    }
}

fn page_count_digit(name: &str) -> Option<u8> {
    name.strip_prefix("data.count-")?.parse().ok()
}

impl Workbench {
    fn active_document(&self, state: &SessionSnapshot) -> Option<DocumentPane> {
        if state.prompt.is_some() || self.pages.scope().is_some() {
            return None;
        }
        if host_view(state).is_some()
            && self.host_document.is_none()
            && self.page_document.is_none()
        {
            return None;
        }
        let pane = self.panes.as_ref()?.active();
        (is_document(state, pane.buffer_id)
            || self
                .page_document
                .as_ref()
                .is_some_and(|layer| layer.buffer_id == pane.buffer_id))
        .then_some(pane)
    }

    fn command_count(&self, state: &SessionSnapshot) -> CommandCount {
        if let Some(draft) = &self.draft {
            draft.text.view(layer::VIEW).command_count()
        } else if let Some(pane) = self.active_document(state) {
            self.documents.command_count(pane.buffer_id, pane.view_id)
        } else if self.pages.scope().is_some() {
            self.page_count
        } else {
            CommandCount::default()
        }
    }

    fn cancel_active_command(&mut self, state: &SessionSnapshot) {
        if let Some(draft) = &mut self.draft {
            draft.text.cancel_pending_command(layer::VIEW);
        } else if let Some(pane) = self.active_document(state) {
            self.documents
                .edit_existing_view(pane.buffer_id, pane.view_id, |text| {
                    text.cancel_pending_command(pane.view_id);
                });
        } else if self.pages.scope().is_some() {
            self.page_count.clear();
            self.pages.take_register();
            if let Some((text, view)) = self.pages.preview_text() {
                text.cancel_pending_command(view);
            }
        }
    }

    fn key_context(&self, state: &SessionSnapshot) -> (Vec<String>, String) {
        if let Some(draft) = &self.draft {
            return (vec!["global".into(), "text".into()], draft.mode().into());
        }
        if let Some(scope) = self.pages.scope() {
            let kind = self.pages.kind().unwrap_or(scope);
            let mut scopes = vec!["global".into()];
            if kind == "preview-items" {
                scopes.extend(["data".into(), "preview".into()]);
            } else if matches!(kind, "diff" | "preview" | "preview-rows") {
                if kind == "preview" {
                    scopes.push("text".into());
                }
                scopes.push("preview".into());
            } else {
                scopes.push("data".into());
            }
            if scopes.last().is_none_or(|last| last != kind) {
                scopes.push(kind.to_owned());
            }
            if scope != kind {
                scopes.push(scope.to_owned());
            }
            return (scopes, self.pages.mode().unwrap_or("normal").into());
        }
        if let Some(pane) = self.active_document(state) {
            let scopes = state
                .views
                .iter()
                .find(|view| view.buffer_id == pane.buffer_id)
                .map(|view| view.key_scopes.clone())
                .filter(|scopes| !scopes.is_empty())
                .unwrap_or_else(|| vec!["global".into(), "text".into()]);
            return (
                scopes,
                self.documents
                    .mode(pane.buffer_id, pane.view_id)
                    .unwrap_or("normal")
                    .into(),
            );
        }
        let scopes = host_view(state)
            .map(|view| view.key_scopes.clone())
            .unwrap_or_else(|| vec!["global".into()]);
        let mode = if state.prompt.is_some() {
            "prompt"
        } else {
            host_view(state).map_or("normal", |view| view.mode.as_str())
        };
        (scopes, mode.into())
    }

    fn target(&self, state: &SessionSnapshot, register: Option<char>) -> Result<CommandTarget> {
        ensure!(
            !self.pages.verifying_file_rows(),
            "wait for Files to finish loading"
        );
        if let Some(target) = self.pages.command_target(register) {
            return Ok(target);
        }
        self.view_target(state)
    }

    fn view_target(&self, state: &SessionSnapshot) -> Result<CommandTarget> {
        if let Some(pane) = self.active_document(state) {
            if let Some(view) = state
                .views
                .iter()
                .find(|view| view.buffer_id == pane.buffer_id)
            {
                return Ok(CommandTarget::View {
                    view_id: view.view_id,
                    buffer_id: view.buffer_id,
                });
            }
            return Ok(CommandTarget::Document {
                buffer_id: pane.buffer_id,
            });
        }
        let view = host_view(state).context("no workspace view is active")?;
        Ok(CommandTarget::View {
            view_id: view.view_id,
            buffer_id: view.buffer_id,
        })
    }

    fn with_active_text<T>(
        &mut self,
        target: TextTarget,
        state: &SessionSnapshot,
        apply: impl FnOnce(&mut zess_text::TextBuffer, u64) -> T,
    ) -> Option<T> {
        match target {
            TextTarget::Draft => Some(apply(&mut self.draft.as_mut()?.text, layer::VIEW)),
            TextTarget::Preview(id)
                if self.draft.is_none() && self.pages.subscription() == Some(id) =>
            {
                let (text, view) = self.pages.preview_text()?;
                Some(apply(text, view))
            }
            TextTarget::Document(pane)
                if self.draft.is_none()
                    && self.active_document(state) == Some(pane)
                    && !self.documents.is_reloading(pane.buffer_id) =>
            {
                self.documents
                    .edit_existing_view(pane.buffer_id, pane.view_id, |text| {
                        apply(text, pane.view_id)
                    })
            }
            _ => None,
        }
    }

    fn reconcile_text_prompt(&mut self, state: &SessionSnapshot) {
        let Some(PromptTarget::Text(target, kind)) = self.prompt else {
            return;
        };
        let pending = self.with_active_text(target, state, |text, view| text.pending_prompt(view));
        if pending == Some(Some(kind)) {
            return;
        }
        if let TextTarget::Document(pane) = target {
            self.documents
                .edit_existing_view(pane.buffer_id, pane.view_id, |text| {
                    if text.pending_prompt(pane.view_id) == Some(kind) {
                        text.cancel_pending_command(pane.view_id);
                    }
                });
        }
        self.prompt = None;
        self.editor.close_external_prompt();
    }

    pub(super) fn synchronize(&mut self, state: &SessionSnapshot) {
        self.pages.set_workspace(state.workspace_root.clone());
        for pages in self.other_pages.values_mut() {
            pages.set_workspace(state.workspace_root.clone());
        }
        let documents = state
            .buffers
            .iter()
            .filter_map(|summary| {
                matches!(summary.kind, BufferKind::Text { .. } | BufferKind::Snapshot)
                    .then_some(summary.id)
            })
            .collect::<Vec<_>>();
        if let Some(layer) = &self.page_document
            && !documents.contains(&layer.buffer_id)
        {
            self.page_document_closed(layer.buffer_id);
        }
        self.documents.retain(&documents);
        self.positions
            .retain(|(buffer, _), _| documents.contains(buffer));
        self.initial_lines
            .retain(|(buffer, _), _| documents.contains(buffer));
        let host = host_view(state);
        let host_text = host.filter(|view| documents.contains(&view.buffer_id));
        if let Some(view) = host_text {
            let mut host_document = self.host_document;
            while let Some(scratch) = host_document
                && !documents.contains(&scratch)
                && let Some(&replacement) = self.discarding_scratch.get(&scratch)
            {
                host_document = Some(replacement);
            }
            if host_document == Some(view.buffer_id) {
                self.host_document = Some(view.buffer_id);
            } else if self.host_document != Some(view.buffer_id) && self.page_document.is_none() {
                let panes = self
                    .panes
                    .get_or_insert_with(|| DocumentPanes::new(view.view_id, view.buffer_id));
                panes.activate_buffer(view.buffer_id);
                self.host_document = Some(view.buffer_id);
            }
        } else if host.is_some() {
            self.host_document = None;
        }
        if let Some(panes) = &mut self.panes {
            let replacement = host_text.map(|view| view.buffer_id).or(state
                .workspace
                .current
                .filter(|buffer| documents.contains(buffer)));
            if let Some(replacement) = replacement {
                for pane in panes.panes() {
                    if !documents.contains(&pane.buffer_id) {
                        panes.replace_buffer(pane.buffer_id, replacement);
                    }
                }
            }
            let active = panes.active().buffer_id;
            self.pages
                .set_source_buffer(documents.contains(&active).then_some(active));
            for pane in panes.panes() {
                if !documents.contains(&pane.buffer_id) {
                    continue;
                }
                self.documents.open(pane.buffer_id);
                if self
                    .documents
                    .view_state(pane.buffer_id, pane.view_id)
                    .is_none()
                    && let Some(initial) = state
                        .views
                        .iter()
                        .find(|v| v.buffer_id == pane.buffer_id)
                        .and_then(position)
                {
                    self.positions
                        .entry((pane.buffer_id, pane.view_id))
                        .or_insert(initial);
                }
            }
        }
        self.reconcile_text_prompt(state);
    }

    pub(super) fn restore(&mut self, saved: &WorkspaceView, state: &SessionSnapshot) -> Result<()> {
        let previous_view = self.active_pane().map_or(0, |pane| pane.view_id);
        let command_origins: BTreeMap<_, _> = self
            .other_pages
            .iter()
            .chain([(&previous_view, &self.pages)])
            .filter_map(|(&view_id, pages)| Some((view_id, pages.command_origin()?)))
            .collect();
        self.pages = self.pages.sibling();
        self.pages.restore_page_watermarks(&saved.page_watermarks);
        self.pages.take_requests();
        self.other_pages.clear();
        let closed_layer = saved
            .page_document
            .filter(|layer| !is_document(state, layer.buffer_id));
        if let Some(mut panes) = saved.text_panes.clone() {
            if let Some(layer) = closed_layer {
                panes.replace_buffer(layer.buffer_id, layer.previous.buffer_id);
            }
            let restored = panes.panes();
            if restored
                .iter()
                .all(|pane| is_document(state, pane.buffer_id))
            {
                let previous = self.panes.replace(panes);
                for pane in previous.iter().flat_map(DocumentPanes::panes) {
                    if !restored.iter().any(|other| other.view_id == pane.view_id) {
                        self.remove_pane_view(pane.view_id);
                    }
                }
            }
        }
        for view in &saved.documents {
            if is_document(state, view.buffer_id) {
                self.documents.open(view.buffer_id);
                self.positions
                    .insert((view.buffer_id, view.view_id), view.state.clone());
            }
        }
        self.page_document = saved.page_document;
        let active = self.active_pane().map_or(0, |pane| pane.view_id);
        if let Some(pane) = saved.pages.iter().find(|pane| pane.view_id == active) {
            self.pages
                .restore(pane.pages.clone(), command_origins.get(&active).copied());
        }
        if let Some(panes) = &self.panes {
            self.pages.set_source_buffer(Some(panes.active().buffer_id));
            for pane in panes
                .panes()
                .into_iter()
                .filter(|pane| pane.view_id != active)
            {
                if let Some(saved) = saved
                    .pages
                    .iter()
                    .find(|saved| saved.view_id == pane.view_id)
                {
                    let mut pages = self.pages.sibling();
                    pages.set_focused(false);
                    pages.set_source_buffer(Some(pane.buffer_id));
                    pages.restore(
                        saved.pages.clone(),
                        command_origins.get(&pane.view_id).copied(),
                    );
                    self.other_pages.insert(pane.view_id, pages);
                }
            }
        }
        if let Some(layer) = closed_layer {
            self.page_document_closed(layer.buffer_id);
        }
        self.draft = None;
        if let Some(draft) = &saved.draft {
            self.restore_draft(draft)?;
        }
        self.reconcile_text_prompt(state);
        Ok(())
    }

    pub(super) fn workspace_view_ready(&self) -> bool {
        self.positions.is_empty() && self.initial_lines.is_empty()
    }

    pub(super) fn workspace_view(&self, _state: &SessionSnapshot) -> WorkspaceView {
        let ids = self
            .panes
            .as_ref()
            .map(|panes| {
                panes
                    .panes()
                    .into_iter()
                    .map(|p| p.view_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut pages = self
            .other_pages
            .iter()
            .map(|(&view_id, pages)| (view_id, pages))
            .chain([(
                self.active_pane().map_or(0, |pane| pane.view_id),
                &self.pages,
            )])
            .map(|(view_id, pages)| PanePages {
                view_id,
                pages: pages.snapshot(),
            })
            .filter(|pane| pane.pages.positions().next().is_some())
            .collect::<Vec<_>>();
        pages.sort_unstable_by_key(|pane| pane.view_id);
        WorkspaceView {
            text_panes: self.panes.clone(),
            documents: self.documents.view_states(&ids),
            pages,
            page_watermarks: self.pages.page_watermarks(),
            page_document: self.page_document.clone(),
            draft: self.draft.as_ref().map(Draft::snapshot),
        }
    }

    pub(super) fn focus_opened(&mut self, state: &SessionSnapshot) {
        if let Some(view) = host_view(state).filter(|v| is_document(state, v.buffer_id)) {
            self.pages.return_to_document();
            self.page_document = None;
            let panes = self
                .panes
                .get_or_insert_with(|| DocumentPanes::new(view.view_id, view.buffer_id));
            panes.activate_buffer(view.buffer_id);
            self.documents.open(view.buffer_id);
            self.pages.set_source_buffer(Some(view.buffer_id));
            self.host_document = Some(view.buffer_id);
        }
    }

    fn discard_unused_scratch(
        &mut self,
        buffer_id: u64,
        replacement: u64,
        state: &SessionSnapshot,
    ) -> Option<Effect> {
        let in_use = self
            .panes
            .as_ref()
            .is_some_and(|panes| panes.panes().iter().any(|pane| pane.buffer_id == buffer_id))
            || self
                .page_document
                .as_ref()
                .is_some_and(|layer| layer.previous.buffer_id == buffer_id);
        let discard = !in_use
            && state.buffers.iter().any(|buffer| {
                buffer.id == buffer_id
                    && matches!(&buffer.kind, BufferKind::Text { path: None })
                    && !buffer.modified
            })
            && self.documents.modified(buffer_id) != Some(true);
        if !discard {
            return None;
        }
        self.discarding_scratch.insert(buffer_id, replacement);
        Some(Effect::Command(CommandAction::DiscardScratch {
            buffer_id,
            replacement,
        }))
    }

    pub(super) fn document_opened(
        &mut self,
        view_id: u64,
        buffer_id: u64,
        state: &SessionSnapshot,
    ) -> Option<Effect> {
        if !is_document(state, buffer_id) {
            return None;
        }
        let panes = self.panes.as_mut()?;
        let previous = panes
            .panes()
            .into_iter()
            .find(|pane| pane.view_id == view_id);
        if let Some(layer) = &self.page_document
            && layer.previous.view_id == view_id
        {
            return self.discard_unused_scratch(buffer_id, layer.previous.buffer_id, state);
        }
        let Some(previous) = previous else {
            let replacement = panes.active().buffer_id;
            return self.discard_unused_scratch(buffer_id, replacement, state);
        };
        panes.set_buffer(view_id, buffer_id);
        self.documents.open(buffer_id);
        if let Some(pages) = self.pages_mut(view_id) {
            pages.return_to_document();
            pages.set_source_buffer(Some(buffer_id));
        }
        self.reconcile_text_prompt(state);
        self.discard_unused_scratch(previous.buffer_id, buffer_id, state)
    }

    pub(super) fn scratch_discard_result(&mut self, buffer_id: u64) {
        self.discarding_scratch.remove(&buffer_id);
    }

    pub(super) fn detach_synchronized(&self) -> bool {
        !self.documents.has_pending()
            && !self.documents.has_closed_edits()
            && self.submitted_prompt.is_none()
            && self.discarding_scratch.is_empty()
    }

    pub(super) fn set_pages_available(&mut self, available: bool) {
        self.pages.set_files_available(available);
        for pages in self.other_pages.values_mut() {
            pages.set_files_available(available);
        }
    }

    pub(super) fn reconnect(&mut self) {
        self.documents.reconnect();
        self.pages.clear_cache();
        if matches!(
            self.prompt,
            Some(PromptTarget::Page {
                confirmation: Some(_),
                ..
            })
        ) {
            self.prompt = None;
            self.editor.close_external_prompt();
        }
        self.pages.take_requests();
        let mut panes = self
            .other_pages
            .values_mut()
            .chain([&mut self.pages])
            .collect::<Vec<_>>();
        panes.sort_unstable_by_key(|pages| pages.active_page_id());
        for pages in panes {
            pages.reconnect();
        }
    }

    pub(super) fn poll_syntax(&mut self) -> bool {
        let mut changed = self.documents.poll_syntax() | self.pages.poll_syntax();
        for pages in self.other_pages.values_mut() {
            changed |= pages.poll_syntax();
        }
        changed
    }

    pub(super) fn apply_document(
        &mut self,
        event: DocumentEvent,
        state: &SessionSnapshot,
    ) -> Result<Vec<Effect>> {
        let DocumentUpdate { notice, close } = self.documents.receive(event);
        let mut effects = notice.map(Effect::Notice).into_iter().collect::<Vec<_>>();
        for ((buffer, view), position) in std::mem::take(&mut self.positions) {
            if self.documents.contains(buffer) {
                if let Err(error) = self.documents.restore_view(buffer, view, &position) {
                    effects.push(Effect::Notice(format!(
                        "Could not restore document position: {error}"
                    )));
                }
            } else {
                self.positions.insert((buffer, view), position);
            }
        }
        for ((buffer, view), line) in std::mem::take(&mut self.initial_lines) {
            if self.documents.contains(buffer) {
                self.documents.edit(buffer, view, |doc| {
                    doc.goto_line(view, line);
                    Ok(TextEffect::default())
                })?;
            } else {
                self.initial_lines.insert((buffer, view), line);
            }
        }
        if let Some(pane) = close
            && self
                .panes
                .as_ref()
                .is_some_and(|panes| panes.panes().contains(&pane))
        {
            self.close_document(pane, false, state, &mut effects)?;
        }
        self.reconcile_text_prompt(state);
        Ok(effects)
    }

    fn close_document(
        &mut self,
        pane: DocumentPane,
        force: bool,
        state: &SessionSnapshot,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        if self
            .page_document
            .as_ref()
            .is_some_and(|layer| layer.buffer_id == pane.buffer_id)
        {
            effects.push(Effect::Command(CommandAction::ClosePageDocument {
                buffer_id: pane.buffer_id,
                force,
            }));
            return Ok(());
        }
        if self.close_pane(pane.view_id) {
            return Ok(());
        }
        let target = state
            .views
            .iter()
            .find(|v| v.buffer_id == pane.buffer_id)
            .map_or(
                CommandTarget::Document {
                    buffer_id: pane.buffer_id,
                },
                |v| CommandTarget::View {
                    view_id: v.view_id,
                    buffer_id: v.buffer_id,
                },
            );
        let action = CommandAction::Request {
            target,
            request: HostRequest::Action {
                name: if force { "quit!" } else { "quit" }.into(),
                argument: json!({}),
            },
        };
        effects.push(Effect::Command(action));
        Ok(())
    }

    fn text_effect(
        &mut self,
        target: impl Into<TextTarget>,
        effect: TextEffect,
        effects: &mut Vec<Effect>,
    ) {
        let target = target.into();
        if let Some(message) = effect.message {
            effects.push(Effect::Notice(message));
        }
        if let Some(text) = effect.clipboard {
            effects.push(Effect::ClipboardWrite {
                primary: false,
                text,
            });
        }
        if let Some(text) = effect.primary_clipboard {
            effects.push(Effect::ClipboardWrite {
                primary: true,
                text,
            });
        }
        if let Some(read) = effect.clipboard_read {
            effects.push(Effect::ClipboardRead {
                id: read.id,
                primary: read.register == '*',
            });
            self.clipboard = Some((target, read));
        }
        if let Some(kind) = effect.prompt {
            self.prompt = Some(PromptTarget::Text(target, kind));
            self.editor.open_prompt(
                format!("{}: ", kind.label()),
                String::new(),
                vec!["global".into()],
                None,
            );
        }
    }

    fn apply_paste_clipboard(
        &mut self,
        id: u64,
        result: Result<String>,
        state: &SessionSnapshot,
    ) -> Result<Vec<Effect>> {
        let mut effects = Vec::new();
        if let Some((target, read)) = self.clipboard.take_if(|(_, read)| read.id == id)
            && let Some(outcome) = self.with_active_text(target, state, |buffer, view| {
                if !read.is_pending_in(buffer, view) {
                    return Ok(TextEffect::default());
                }
                buffer.complete_clipboard_read(view, read, result?)
            })
        {
            match outcome {
                Ok(effect) => self.text_effect(target, effect, &mut effects),
                Err(error) => effects.push(Effect::Notice(format!("Clipboard: {error}"))),
            }
        }
        Ok(effects)
    }

    fn notify_prompt(
        &mut self,
        event: PromptEvent,
        state: &SessionSnapshot,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        let Some(target) = &self.prompt else {
            return Ok(());
        };
        if self.submitted_prompt.is_some() {
            return Ok(());
        }
        let text = self.editor.prompt_text().to_owned();
        let query = self.editor.query_kind().unwrap_or_default();
        let query_kind = match query {
            QueryKind::Plain => FilterKind::Plain,
            QueryKind::Glob => FilterKind::Glob,
            QueryKind::Regex => FilterKind::Regex,
        };
        match *target {
            PromptTarget::Text(target, kind) => {
                let result = self.with_active_text(target, state, |buffer, view| match event {
                    PromptEvent::Changed => buffer.preview_prompt(view, kind, &text),
                    PromptEvent::Submit => buffer.submit_prompt(view, kind, &text),
                    PromptEvent::Cancel => {
                        buffer.cancel_pending_command(view);
                        Ok(TextEffect::default())
                    }
                });
                match result {
                    None => {
                        self.reconcile_text_prompt(state);
                        return Ok(());
                    }
                    Some(Ok(effect)) => self.text_effect(target, effect, effects),
                    Some(Err(error)) => {
                        self.editor.set_prompt_error(error.to_string());
                        return Ok(());
                    }
                }
            }
            PromptTarget::Filter(ref previous) => match event {
                PromptEvent::Cancel => self.pages.filter(previous.clone()),
                PromptEvent::Changed | PromptEvent::Submit => {
                    if let Err(error) = zess_filter::Filter::compile(
                        &zess_filter::Query {
                            kind: query,
                            text: text.clone(),
                        },
                        zess_filter::Case::Sensitive,
                    ) {
                        self.editor.set_prompt_error(error.to_string());
                        return Ok(());
                    }
                    self.pages.filter((!text.is_empty()).then_some(PageFilter {
                        kind: query_kind,
                        text,
                    }));
                }
            },
            PromptTarget::Page {
                ref purpose,
                ref confirmation,
            } => match event {
                PromptEvent::Changed => self.pages.prompt_changed(purpose, text, query_kind),
                _ => {
                    if let Some(action) = self.pages.submit_input(
                        purpose,
                        confirmation.clone(),
                        &text,
                        matches!(event, PromptEvent::Cancel),
                    ) {
                        if let CommandAction::PagePrompt {
                            subscription_id, ..
                        } = &action
                        {
                            self.submitted_prompt = Some((*subscription_id, purpose.clone()));
                        }
                        effects.push(Effect::Command(action));
                    }
                }
            },
        }
        if matches!(self.prompt, Some(PromptTarget::Filter(_)))
            && !matches!(event, PromptEvent::Changed)
        {
            self.pages.set_mode("normal");
        }
        if !matches!(event, PromptEvent::Changed) && self.submitted_prompt.is_none() {
            self.editor.reset_input();
            self.prompt = None;
        }
        Ok(())
    }

    fn plugin_prompt(
        &mut self,
        purpose: String,
        text: String,
        key_scope: Option<String>,
        confirmation: Option<String>,
    ) {
        let mut scopes = vec!["global".into(), "minibuffer".into()];
        if let Some(scope) = key_scope {
            scopes.push(scope)
        }
        self.pages.focus_changed();
        let query = (purpose == "find").then_some(QueryKind::Plain);
        let label = if query.is_some() {
            purpose.clone()
        } else {
            format!("{purpose}: ")
        };
        self.editor.open_prompt(label, text, scopes, query);
        self.prompt = Some(PromptTarget::Page {
            purpose,
            confirmation,
        });
    }

    pub(super) fn yank(&self, register: char, values: Vec<String>) -> Result<Vec<Effect>> {
        let mut effects = Vec::new();
        let count = values.len();
        if let Some(text) = self.documents.yank(register, values)? {
            effects.push(Effect::ClipboardWrite {
                primary: register == '*',
                text,
            });
        }
        effects.push(Effect::Notice(format!("yanked {count} selection(s)")));
        Ok(effects)
    }

    fn host_request(
        &mut self,
        request: HostRequest,
        state: &SessionSnapshot,
        register: Option<char>,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        match request {
            HostRequest::Action { name, argument } => {
                self.native(&name, argument, state, effects)?
            }
            HostRequest::MiniBuffer {
                purpose,
                text,
                key_scope,
                confirmation,
            } if self.pages.subscription().is_some() => {
                self.plugin_prompt(purpose, text, key_scope, confirmation)
            }
            HostRequest::Layer {
                purpose,
                text,
                inline,
            } if self.pages.subscription().is_some() => self.open_layer(purpose, text, inline)?,
            HostRequest::Message { text } => effects.push(Effect::Notice(text)),
            HostRequest::Yank { values, register } => {
                effects.extend(self.yank(register.unwrap_or('"'), values)?);
            }
            HostRequest::FocusText { buffer_id, .. }
                if self.discarding_scratch.contains_key(&buffer_id) => {}
            HostRequest::FocusText { buffer_id, line } if is_document(state, buffer_id) => {
                self.pages.return_to_document();
                if let Some(panes) = &mut self.panes {
                    panes.activate_buffer(buffer_id);
                    let view = panes.active().view_id;
                    self.documents.open(buffer_id);
                    if self.documents.contains(buffer_id) {
                        self.documents.edit(buffer_id, view, |text| {
                            text.goto_line(view, line);
                            Ok(TextEffect::default())
                        })?;
                    } else {
                        self.initial_lines.insert((buffer_id, view), line);
                    }
                }
            }
            request => {
                if !self.pages.request(&request, 1) {
                    let mut target = self.target(state, register)?;
                    if let HostRequest::BufferEvent { kind, event, .. } = &request {
                        self.pages.prepare_files_command(&mut target, kind, event);
                    }
                    effects.push(Effect::Command(CommandAction::Request { target, request }));
                }
            }
        }
        Ok(())
    }

    fn background_page(&self, subscription_id: u64) -> Option<u64> {
        self.other_pages
            .iter()
            .find(|(_, pages)| pages.subscription() == Some(subscription_id))
            .map(|(&view, _)| view)
    }

    fn modal_input_active(&self) -> bool {
        self.prompt.is_some()
            || self.editor.prompt_active()
            || self.draft.is_some()
            || self.editor.help_focused()
    }

    fn page_publication_request(
        &mut self,
        subscription_id: u64,
        request: HostRequest,
        state: &SessionSnapshot,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        let foreground = self.pages.subscription() == Some(subscription_id);
        match &request {
            HostRequest::Message { .. } | HostRequest::Yank { .. } => {
                self.host_request(request, state, None, effects)?;
            }
            HostRequest::MiniBuffer { purpose, .. } | HostRequest::Layer { purpose, .. }
                if !foreground
                    || (self.modal_input_active()
                        && !self.submitted_prompt.as_ref().is_some_and(|(id, pending)| {
                            *id == subscription_id && pending == purpose
                        })) =>
            {
                let origin = if foreground {
                    Some(&self.pages)
                } else {
                    self.background_page(subscription_id)
                        .map(|view| &self.other_pages[&view])
                };
                if let Some(CommandTarget::Page { input, .. }) =
                    origin.and_then(|pages| pages.command_target(None))
                {
                    effects.push(Effect::Command(CommandAction::PagePrompt {
                        subscription_id,
                        input,
                        purpose: purpose.clone(),
                        confirmation: match request {
                            HostRequest::MiniBuffer { confirmation, .. } => confirmation,
                            _ => None,
                        },
                        text: String::new(),
                        cancelled: true,
                    }));
                }
            }
            _ if foreground => {
                self.host_request(request, state, None, effects)?;
            }
            _ => {
                let Some(target) = self
                    .background_page(subscription_id)
                    .and_then(|view| self.other_pages[&view].command_target(None))
                else {
                    return Ok(());
                };
                effects.push(Effect::Command(CommandAction::Request { target, request }));
            }
        }
        Ok(())
    }

    fn page_effects(&mut self, state: &SessionSnapshot, effects: &mut Vec<Effect>) -> Result<()> {
        for request in self.pages.take_effects() {
            self.host_request(request, state, None, effects)?;
        }
        effects.extend(self.pages.take_actions().into_iter().map(Effect::Command));
        for pages in self.other_pages.values_mut() {
            for request in pages.take_effects() {
                if let HostRequest::Message { text } = request {
                    effects.push(Effect::Notice(text));
                }
            }
            effects.extend(pages.take_actions().into_iter().map(Effect::Command));
        }
        Ok(())
    }

    pub(super) fn apply_request(
        &mut self,
        request: HostRequest,
        state: &SessionSnapshot,
    ) -> Result<Vec<Effect>> {
        let mut effects = Vec::new();
        self.host_request(request, state, None, &mut effects)?;
        self.page_effects(state, &mut effects)?;
        Ok(effects)
    }

    fn page_document_opened(
        &mut self,
        subscription_id: u64,
        document: &PageDocument,
        state: &SessionSnapshot,
    ) -> Result<bool> {
        let owner = self.background_page(subscription_id);
        if (owner.is_none() && self.pages.subscription() != Some(subscription_id))
            || (document.fullscreen
                && (self.page_document.is_some() || (owner.is_some() && self.modal_input_active())))
        {
            return Ok(false);
        }
        let view = host_view(state).map_or(1, |view| view.view_id);
        let panes = self
            .panes
            .get_or_insert_with(|| DocumentPanes::new(view, document.buffer_id));
        let previous = owner
            .and_then(|view| panes.panes().into_iter().find(|pane| pane.view_id == view))
            .unwrap_or_else(|| panes.active());
        panes.set_buffer(previous.view_id, document.buffer_id);
        if document.fullscreen {
            self.focus_pane(previous.view_id);
            self.page_document = Some(DocumentLayer {
                buffer_id: document.buffer_id,
                previous,
            });
            self.pages.suspend();
        } else {
            self.pages_mut(previous.view_id)
                .unwrap()
                .return_to_document();
        }
        self.documents.open(document.buffer_id);
        if self.documents.contains(document.buffer_id) {
            self.documents
                .edit(document.buffer_id, previous.view_id, |text| {
                    text.goto_line(previous.view_id, document.line);
                    Ok(TextEffect::default())
                })?;
        } else {
            self.initial_lines
                .insert((document.buffer_id, previous.view_id), document.line);
        }
        Ok(true)
    }

    pub(super) fn apply_page_result(
        &mut self,
        page_id: PageId,
        origin: u64,
        navigation: Vec<Navigation>,
        document: Option<PageDocument>,
        state: &SessionSnapshot,
    ) -> Result<Option<Vec<Effect>>> {
        let Some(subscription_id) = self
            .other_pages
            .values()
            .chain([&self.pages])
            .find_map(|pages| pages.command_result_subscription(page_id, origin))
        else {
            return Ok(None);
        };
        let mut effects = Vec::new();
        let mut applied = false;
        for navigation in navigation {
            let pages = if let Some(view) = self.background_page(subscription_id) {
                self.other_pages.get_mut(&view).unwrap()
            } else if self.pages.subscription() == Some(subscription_id) {
                &mut self.pages
            } else {
                break;
            };
            applied |= pages.request(&HostRequest::Navigate { navigation }, 1);
        }
        if let Some(document) = document
            && is_document(state, document.buffer_id)
        {
            applied |= self.page_document_opened(subscription_id, &document, state)?;
        }
        if !applied {
            return Ok(None);
        }
        self.page_effects(state, &mut effects)?;
        self.reconcile_text_prompt(state);
        Ok(Some(effects))
    }

    pub(super) fn invalidate_resource_cache(
        &mut self,
        entries: &[PageCacheEntry],
        reason: ResourceCacheInvalidation,
    ) {
        for pages in self.other_pages.values_mut().chain([&mut self.pages]) {
            pages.invalidate_cache(entries, reason);
        }
    }

    pub(super) fn apply_resource(
        &mut self,
        update: ResourceUpdate,
        state: &SessionSnapshot,
    ) -> Result<Vec<Effect>> {
        let mut effects = Vec::new();
        let subscription_id = update.subscription_id;
        let searching_diff = self.pages.subscription() == Some(subscription_id)
            && matches!(&self.prompt, Some(PromptTarget::Page { purpose, .. }) if purpose == "diff-search")
            && self.pages.diff_search_active();
        let publication = match self.background_page(subscription_id) {
            Some(view) => self.other_pages.get_mut(&view).unwrap().apply(update),
            None => self.pages.apply(update),
        };
        if searching_diff && !self.pages.diff_search_active() {
            self.prompt = None;
            self.editor.close_external_prompt();
        }
        for request in publication.requests {
            self.page_publication_request(subscription_id, request, state, &mut effects)?;
        }
        if let Some(document) = publication.document
            && !self.page_document_opened(subscription_id, &document, state)?
        {
            if document.fullscreen {
                effects.push(Effect::Command(CommandAction::ClosePageDocument {
                    buffer_id: document.buffer_id,
                    force: false,
                }));
            }
            self.page_effects(state, &mut effects)?;
            return Ok(effects);
        }
        self.page_effects(state, &mut effects)?;
        self.reconcile_text_prompt(state);
        Ok(effects)
    }

    pub(super) fn page_document_closed(&mut self, id: u64) {
        if self
            .page_document
            .as_ref()
            .is_some_and(|layer| layer.buffer_id == id)
        {
            let layer = self.page_document.take().unwrap();
            if let Some(panes) = &mut self.panes {
                panes.replace_buffer(id, layer.previous.buffer_id);
            }
            self.focus_pane(layer.previous.view_id);
            self.pages.resume();
        }
    }

    fn native(
        &mut self,
        name: &str,
        argument: Value,
        state: &SessionSnapshot,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        if self.layer_command(name, effects)? {
            return Ok(());
        }
        let doc = self.active_document(state);
        if let Some(pane) = doc
            && let Some(command) = TextCommand::parse(name)
        {
            let effect = self
                .documents
                .execute(pane.buffer_id, pane.view_id, command)?;
            self.text_effect(pane, effect, effects);
            return Ok(());
        }
        let page = self.pages.scope().is_some();
        if page {
            if let Some(digit) = page_count_digit(name) {
                self.page_count.push(digit);
                return Ok(());
            }
            let count = std::mem::take(&mut self.page_count).value().unwrap_or(1);
            if let Some(result) = self.pages.text_command(name, count) {
                self.text_effect(
                    TextTarget::Preview(self.pages.subscription().unwrap()),
                    result?,
                    effects,
                );
                return Ok(());
            }
            if name == "data.filter" {
                let previous = self.pages.current_filter().cloned();
                let (kind, text) =
                    previous
                        .as_ref()
                        .map_or((QueryKind::Plain, String::new()), |f| {
                            (
                                match f.kind {
                                    FilterKind::Plain => QueryKind::Plain,
                                    FilterKind::Glob => QueryKind::Glob,
                                    FilterKind::Regex => QueryKind::Regex,
                                },
                                f.text.clone(),
                            )
                        });
                self.pages.set_mode("filter");
                self.prompt = Some(PromptTarget::Filter(previous));
                self.editor
                    .open_prompt("filter".into(), text, vec!["global".into()], Some(kind));
                return Ok(());
            }
            if self.pages.command(name, count) {
                if matches!(name, "q" | "q!" | "quit" | "quit!")
                    && self.pages.is_empty()
                    && let Some(pane) = self.active_pane()
                {
                    self.close_pane(pane.view_id);
                }
                return Ok(());
            }
        }
        let arg = argument
            .get("argument")
            .and_then(Value::as_str)
            .unwrap_or("");
        if name == "exit"
            && let Some(notice) = self.documents.closed_notice()
        {
            anyhow::bail!("{notice}");
        }
        match name {
            "open" | "new"
                if let Some(pane) = self.active_pane()
                    && (host_view(state).is_none() || self.host_document.is_some()) =>
            {
                ensure!(
                    self.page_document.is_none() && self.draft.is_none(),
                    "close the current layer before switching buffers"
                );
                effects.push(Effect::OpenDocument {
                    view_id: pane.view_id,
                    path: (name == "open").then(|| arg.trim().to_owned()),
                });
            }
            "reload" if let Some(pane) = doc => {
                if !self.documents.is_reloading(pane.buffer_id) {
                    self.documents.queue_reload(pane.buffer_id)?;
                    effects.push(Effect::Command(CommandAction::Request {
                        target: CommandTarget::Document {
                            buffer_id: pane.buffer_id,
                        },
                        request: HostRequest::Action {
                            name: name.into(),
                            argument,
                        },
                    }));
                }
            }
            "write" | "write-quit" if doc.is_some() => {
                let pane = doc.unwrap();
                if !self.documents.is_reloading(pane.buffer_id) {
                    let path = (!arg.is_empty()).then(|| arg.to_owned());
                    let close_view = (name == "write-quit").then_some(pane.view_id);
                    self.documents.save(pane.buffer_id, path, close_view)?;
                }
            }
            "quit" | "quit!" if doc.is_some() => {
                self.close_document(doc.unwrap(), name == "quit!", state, effects)?
            }
            "vsplit" | "hsplit" if doc.is_some() || page => {
                ensure!(
                    self.page_document.is_none() && self.draft.is_none(),
                    "close the layer before splitting"
                );
                let axis = if name == "vsplit" {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                };
                if matches!(self.prompt, Some(PromptTarget::Filter(_))) {
                    self.pages.set_mode("normal");
                    self.editor.reset_input();
                    self.prompt = None;
                }
                ensure!(self.prompt.is_none(), "close the prompt before splitting");
                self.cancel_active_command(state);
                self.split_pane(axis, state)?;
            }
            "focus-next" | "focus-left" | "focus-right" | "focus-up" | "focus-down"
                if self.panes.is_some() && (doc.is_some() || page) =>
            {
                if self.page_document.is_none() && self.draft.is_none() && self.prompt.is_none() {
                    self.focus_pane_by_command(name);
                }
            }
            "buffer" | "buffer-next" | "buffer-previous" if doc.is_some() => {
                let active = doc.unwrap();
                let buffers: Vec<_> = state
                    .buffers
                    .iter()
                    .filter_map(|buffer| {
                        (is_document(state, buffer.id)
                            && !self.discarding_scratch.contains_key(&buffer.id))
                        .then_some(buffer.id)
                    })
                    .collect();
                let buffer = match name {
                    "buffer" => arg.parse::<u64>().context("expected a buffer ID")?,
                    "buffer-next" => *buffers
                        .iter()
                        .find(|&&id| id > active.buffer_id)
                        .or(buffers.first())
                        .context("no document available")?,
                    _ => *buffers
                        .iter()
                        .rev()
                        .find(|&&id| id < active.buffer_id)
                        .or(buffers.last())
                        .context("no document available")?,
                };
                ensure!(
                    buffers.contains(&buffer),
                    "buffer {buffer} is not available"
                );
                self.panes.as_mut().unwrap().activate_buffer(buffer);
                self.documents.open(buffer);
                self.pages.set_source_buffer(Some(buffer));
            }
            "document.keep-local" | "document.use-workspace" => {
                let keep_local = name == "document.keep-local";
                if !arg.is_empty() {
                    let buffer = arg.parse().context("expected a closed buffer ID")?;
                    effects.push(Effect::Notice(
                        self.documents.recover_closed(buffer, keep_local)?,
                    ));
                    return Ok(());
                }
                let pane = doc.context("no document is active")?;
                self.documents.resolve(pane.buffer_id, keep_local)?;
            }
            _ => {
                let target = if name == "exit" {
                    self.view_target(state)?
                } else {
                    self.target(state, None)?
                };
                effects.push(Effect::Command(CommandAction::Request {
                    target,
                    request: HostRequest::Action {
                        name: name.into(),
                        argument,
                    },
                }));
            }
        }
        Ok(())
    }

    fn command(
        &mut self,
        name: String,
        argument: Value,
        state: &SessionSnapshot,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        let binding = self
            .editor
            .catalog()
            .and_then(|catalog| catalog.implementation(&name))
            .cloned()
            .with_context(|| format!("unknown command: {name}"))?;
        let native = match &binding {
            BindingCommand::Native => Some(name.as_str()),
            BindingCommand::Request(HostRequest::Action { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let register = match native {
            Some(name) if name == "data.select-register" || page_count_digit(name).is_some() => {
                None
            }
            _ => self.pages.take_register(),
        };
        match binding {
            BindingCommand::Native => self.native(&name, argument, state, effects),
            BindingCommand::Request(HostRequest::Action {
                name,
                argument: registered,
            }) => {
                let argument = if registered.is_null() {
                    argument
                } else {
                    registered
                };
                self.native(&name, argument, state, effects)
            }
            BindingCommand::Request(request) => {
                self.host_request(request, state, register, effects)
            }
            BindingCommand::BufferEvent {
                ref kind,
                ref event,
            } if self.pages.scope() == Some(kind.as_str())
                && self.pages.command(&format!("{kind}.{event}"), 1) =>
            {
                Ok(())
            }
            BindingCommand::BufferEvent { .. } | BindingCommand::Script => {
                let mut target = self.target(state, register)?;
                if matches!(target, CommandTarget::Page { .. })
                    && let BindingCommand::BufferEvent { kind, event } = &binding
                {
                    self.pages.prepare_files_command(&mut target, kind, event);
                    self.pages.allow_preview_focus(kind, event);
                }
                effects.push(Effect::Command(CommandAction::Invoke {
                    target,
                    name,
                    argument,
                }));
                Ok(())
            }
        }
    }

    fn apply_key(&mut self, key: &str, state: &SessionSnapshot, width: u16) -> Result<Vec<Effect>> {
        let mut effects = Vec::new();
        self.clipboard = None;
        if self.submitted_prompt.is_some() {
            return Ok(effects);
        }
        let editor_focused = self.editor.help_focused() || self.editor.prompt_active();
        if !editor_focused {
            if self.layer_pending(key, &mut effects)? {
                return Ok(effects);
            }
            if let Some(pane) = self.active_document(state)
                && self
                    .documents
                    .has_pending_input(pane.buffer_id, pane.view_id)
            {
                let effect = self.documents.input(pane.buffer_id, pane.view_id, key)?;
                self.text_effect(pane, effect, &mut effects);
                return Ok(effects);
            }
            if self
                .pages
                .preview_text()
                .is_some_and(|(text, id)| text.has_pending_input(id))
                && let Some(effect) = self.pages.text_input(key)
            {
                self.text_effect(
                    TextTarget::Preview(self.pages.subscription().unwrap()),
                    effect?,
                    &mut effects,
                );
                return Ok(effects);
            }
            if self.pages.feed_key(key) {
                self.page_effects(state, &mut effects)?;
                return Ok(effects);
            }
        }
        let (scopes, mode) = self.key_context(state);
        match self.editor.handle_key(
            key,
            &scopes.iter().map(String::as_str).collect::<Vec<_>>(),
            &mode,
            width,
            self.height,
        )? {
            ClientInput::Consumed if !editor_focused && self.editor.prompt_active() => {
                self.cancel_active_command(state);
            }
            ClientInput::CancelledKeySequence if !editor_focused => {
                self.cancel_active_command(state);
            }
            ClientInput::Consumed | ClientInput::CancelledKeySequence => {}
            ClientInput::Prompt(event) => self.notify_prompt(event, state, &mut effects)?,
            ClientInput::Command { name, argument } => {
                self.command(name, argument, state, &mut effects)?
            }
            ClientInput::Unbound => {
                if self.layer_input(key, &mut effects)? {
                    return Ok(effects);
                }
                if let Some(pane) = self.active_document(state) {
                    if self.documents.is_reloading(pane.buffer_id) {
                        return Ok(effects);
                    }
                    if matches!(mode.as_str(), "normal" | "select")
                        && let Some(digit) = key.parse::<u8>().ok().filter(|n| *n < 10)
                    {
                        self.documents.edit(pane.buffer_id, pane.view_id, |b| {
                            b.push_command_count(pane.view_id, digit);
                            Ok(TextEffect::default())
                        })?;
                    } else if mode == "insert" {
                        let effect = self.documents.input(pane.buffer_id, pane.view_id, key)?;
                        self.text_effect(pane, effect, &mut effects);
                    } else {
                        self.cancel_active_command(state);
                    }
                } else if mode == "insert"
                    && let Some(effect) = self.pages.text_input(key)
                {
                    self.text_effect(
                        TextTarget::Preview(self.pages.subscription().unwrap()),
                        effect?,
                        &mut effects,
                    );
                } else if state.prompt.is_some() && key.chars().count() == 1 {
                    self.native(
                        "prompt.insert-text",
                        json!({"text":key}),
                        state,
                        &mut effects,
                    )?;
                } else if self.pages.scope().is_some()
                    && let Ok(digit) = key.parse::<u8>()
                    && digit < 10
                {
                    self.page_count.push(digit);
                }
            }
        }
        if !editor_focused && (self.editor.prompt_active() || self.editor.help_focused()) {
            if self.editor.help_focused() {
                self.pages.take_register();
            }
            self.pages.focus_changed();
        }
        self.page_effects(state, &mut effects)?;
        Ok(effects)
    }

    fn apply_paste(&mut self, text: &str, state: &SessionSnapshot) -> Result<Vec<Effect>> {
        let mut effects = Vec::new();
        self.clipboard = None;
        if self.submitted_prompt.is_some() {
            return Ok(effects);
        }
        if self.editor.paste(text) {
            if self.editor.external_prompt() {
                self.notify_prompt(PromptEvent::Changed, state, &mut effects)?;
            }
        } else if self.layer_paste(text, &mut effects)? {
            return Ok(effects);
        } else if let Some(result) = self.pages.paste(text) {
            self.text_effect(
                TextTarget::Preview(self.pages.subscription().unwrap()),
                result?,
                &mut effects,
            );
        } else if let Some(pane) = self.active_document(state) {
            let effect = self.documents.paste(pane.buffer_id, pane.view_id, text)?;
            self.text_effect(pane, effect, &mut effects);
        } else if state.prompt.is_some() {
            self.native(
                "prompt.insert-text",
                json!({"text":text}),
                state,
                &mut effects,
            )?;
        }
        self.page_effects(state, &mut effects)?;
        Ok(effects)
    }

    fn apply_mouse(
        &mut self,
        event: &MouseEvent,
        state: &SessionSnapshot,
        width: u16,
        height: u16,
    ) -> Result<Vec<Effect>> {
        let mut effects = Vec::new();
        self.clipboard = None;
        if self.submitted_prompt.is_some() {
            return Ok(effects);
        }
        if !self.editor.mouse(event, width, height)
            && !self.layer_mouse(event, state.editor_options.scroll_lines)
        {
            if let Some(hit) = self.page_mouse(event, state.editor_options.scroll_lines) {
                if matches!(event.kind, MouseKind::Press | MouseKind::DoubleClick) {
                    self.page_count.clear();
                }
                if let PageMouseHit::Command(name) = hit {
                    self.command(name, json!({}), state, &mut effects)?;
                }
            } else {
                self.mouse_text(event, state, &mut effects)?;
            }
        }
        self.page_effects(state, &mut effects)?;
        Ok(effects)
    }
}
