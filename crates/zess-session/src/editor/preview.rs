use super::commands::PreviewCommand;
use super::pages::{
    PreviewPublication, PreviewSlots, PreviewSurface, RenderTarget, Slot, SlotContent,
};
use super::*;
use zess_services::{QueryRequest, QueryResult};

enum SourceLine {
    ExplicitPath {
        path: String,
        line: usize,
    },
    Worktree {
        path: String,
        line: usize,
    },
    Commit {
        commit: String,
        path: String,
        line: usize,
    },
}

pub(super) struct PendingDiffSource {
    origin: ViewId,
    buffer: BufferId,
    handle: u64,
    presentation: SnapshotPresentation,
    line: usize,
}

pub(super) struct PendingPatch {
    source: ViewId,
    buffer: BufferId,
    task: zess_services::PatchTask,
}

impl Session {
    pub(super) fn preview_source(&self, target: ViewId) -> Option<ViewId> {
        let page = self.views.page(target);
        (page.main.id != target).then_some(page.main.id)
    }

    pub(super) fn preview_target(&self, source: ViewId) -> Option<ViewId> {
        self.views
            .previews(self.views.page_id(source))
            .and_then(|previews| {
                previews
                    .members
                    .iter()
                    .find(|slot| slot.id == previews.active)
                    .and_then(Slot::view)
                    .or_else(|| previews.members.iter().find_map(Slot::view))
                    .map(|view| view.id)
            })
    }

    pub(super) fn active_preview(&self, source: ViewId) -> Option<RenderTarget> {
        let page = self.views.page_id(source);
        let previews = self.views.previews(page)?;
        Some(
            previews
                .members
                .iter()
                .find(|slot| slot.id == previews.active)?
                .target(page),
        )
    }

    pub(super) fn load_plugin_file(&mut self, path: &str) -> Result<BufferId> {
        let path = self.plugin_file_path(path)?;
        self.load_file(&path)
    }

    pub(super) fn plugin_file_path(&self, path: &str) -> Result<PathBuf> {
        let absolute = self.absolute(Path::new(path));
        let absolute = absolute.canonicalize().unwrap_or(absolute);
        let authorized = self.buffers.values().any(|buffer| {
            matches!(buffer, Buffer::Text(text) if text.path() == Some(absolute.as_path()))
        });
        if !absolute.starts_with(&self.workspace) && !authorized {
            bail!("plugin file request is outside the workspace");
        }
        Ok(absolute)
    }

    pub(super) fn open_file_layer(
        &mut self,
        parent: ViewId,
        path: &str,
        line: usize,
    ) -> Result<()> {
        if self.views[&parent].layer.is_some() {
            bail!("view already has a layer");
        }
        self.focus_view(parent)?;
        let path = self.plugin_file_path(path)?;
        let buffer = self.open_file_buffer(&path)?;
        let child = self.create_page(buffer);
        let view = self.views.get_mut(&child).unwrap();
        view.parent = Some(parent);
        view.layer_content = Some(LayerContent::File);
        view.presentation = LayerPresentation::Fullscreen;
        self.views.get_mut(&parent).unwrap().layer = Some(child);
        self.focus_view(child)?;
        self.buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .goto_line(child, line);
        self.remember_text();
        Ok(())
    }

    fn retire_preview(&mut self, source: ViewId, id: ViewId, mut view: View) {
        if let Some(child) = view.layer.take() {
            let mut parent = source;
            while let Some(layer) = self.views[&parent].layer {
                parent = layer;
            }
            self.views.get_mut(&child).unwrap().parent = Some(parent);
            self.views.get_mut(&parent).unwrap().layer = Some(child);
        }
        self.retire_view(id, view);
    }

    fn retire_preview_slot(&mut self, source: ViewId, slot: Slot) {
        match slot.content {
            SlotContent::Surface(surface) => self.remove_unused_preview_buffer(surface.buffer),
            SlotContent::View { view, .. } => self.retire_preview(source, view.id, view),
        }
    }

    pub(super) fn remove_unused_preview_buffer(&mut self, buffer: BufferId) {
        if self.buffers.get(&buffer).is_some_and(Buffer::is_preview)
            && !self.views.references_buffer(buffer)
        {
            self.buffers.remove(&buffer);
        }
    }

    pub(super) fn close_preview(&mut self, page: PageId) -> Result<()> {
        let source = self.views.main(page);
        let focused = self
            .views
            .previews(page)
            .is_some_and(|previews| previews.contains(self.focus));
        if focused {
            self.cancel_input_context()?;
        }
        for slot in self.views.take_previews(page) {
            self.retire_preview_slot(source, slot);
        }
        if focused {
            self.focus = source;
        }
        Ok(())
    }

    pub(super) fn show_preview(
        &mut self,
        page: PageId,
        path: String,
        content: PreviewContent,
        focus: bool,
    ) -> Result<()> {
        self.show_preview_group(
            page,
            vec![PreviewPane::View {
                id: "main".into(),
                path,
                content,
                header: String::new(),
                key_scope: None,
                loading: false,
            }],
            PreviewLayout::Rows,
            focus,
        )
    }

    pub(super) fn show_preview_group(
        &mut self,
        page: PageId,
        panes: Vec<PreviewPane>,
        layout: PreviewLayout,
        focus: bool,
    ) -> Result<()> {
        let source = self.views.main(page);
        if panes.is_empty() {
            return self.close_preview(page);
        }
        let mut ids = HashSet::new();
        if panes.iter().any(|pane| !ids.insert(pane.id().to_owned())) {
            bail!("preview pane ids must be unique");
        }
        let active = self
            .views
            .previews(page)
            .map(|previews| previews.active.clone());
        let mut previous = self
            .views
            .previews(page)
            .map(|previews| {
                previews
                    .members
                    .iter()
                    .map(|slot| (slot.id.clone(), slot.view().map(|view| view.id)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut members = Vec::new();
        let mut retained_views = Vec::new();
        for pane in panes {
            let is_view = matches!(pane, PreviewPane::View { .. });
            let matching = previous
                .iter()
                .position(|(id, view)| id == pane.id() && view.is_some() == is_view);
            let reusable = matching.or_else(|| {
                previous
                    .iter()
                    .position(|(id, view)| !ids.contains(id) && view.is_some() == is_view)
            });
            let previous = reusable.map(|index| previous.remove(index));
            match pane {
                PreviewPane::Surface {
                    id,
                    path,
                    header,
                    loading,
                    content,
                } => {
                    let old = previous
                        .as_ref()
                        .and_then(|(slot, _)| self.views.surface(page, slot))
                        .map(|surface| surface.buffer);
                    let buffer = self.update_surface_content(old, path, content)?;
                    let viewport = previous
                        .and_then(|(slot, _)| self.views.surface_mut(page, &slot))
                        .map(|surface| std::mem::take(&mut surface.viewport))
                        .unwrap_or_default();
                    members.push((
                        id,
                        Some(SlotContent::Surface(PreviewSurface {
                            buffer,
                            header,
                            loading,
                            viewport,
                        })),
                    ));
                }
                PreviewPane::View {
                    id,
                    path,
                    header,
                    loading,
                    content,
                    key_scope,
                } => {
                    let target = self.update_preview_content(
                        page,
                        &id,
                        previous.and_then(|(_, view)| view),
                        path,
                        content,
                    )?;
                    let view = self.views.get_mut(&target).unwrap();
                    view.location.display.header = header;
                    view.location.display.loading = loading;
                    view.location.options["key-scope"] = json!(key_scope);
                    retained_views.push((members.len(), target));
                    members.push((id, None));
                }
            }
        }
        let removed_focus = self
            .views
            .previews(page)
            .is_some_and(|previews| previews.contains(self.focus))
            && !retained_views
                .iter()
                .any(|(_, target)| *target == self.focus);
        if removed_focus {
            self.cancel_input_context()?;
            self.focus = source;
        }
        let mut retired = self.views.take_previews(page);
        for (index, target) in retained_views {
            let position = retired
                .iter()
                .position(|slot| slot.view().is_some_and(|view| view.id == target))
                .unwrap();
            let slot = retired.remove(position);
            let SlotContent::View { view, .. } = slot.content else {
                unreachable!()
            };
            members[index].1 = Some(SlotContent::View { view });
        }
        let members = members
            .into_iter()
            .map(|(id, content)| Slot {
                id,
                content: content.unwrap(),
            })
            .collect::<Vec<_>>();
        let active = active
            .filter(|active| members.iter().any(|slot| &slot.id == active))
            .unwrap_or_else(|| members[0].id.clone());
        self.views.set_previews(
            page,
            PreviewSlots {
                publication: PreviewPublication::Ready,
                members,
                active,
                layout,
            },
        );
        for slot in retired {
            self.retire_preview_slot(source, slot);
        }
        if focus && self.focus == source {
            if let Some(target) = self.preview_target(source) {
                self.focus_view(target)?;
            }
        }
        self.invalidate_text_prompt();
        Ok(())
    }

    fn preview_buffer(&self, path: &str, content: PreviewContent) -> Result<Buffer> {
        Ok(match content {
            PreviewContent::Text { text } => {
                let mut text =
                    TextBuffer::new_preview_with_path(&text, &self.absolute(Path::new(&path)));
                text.set_readonly(true);
                Buffer::PreviewText {
                    path: path.to_owned(),
                    snapshot: None,
                    text,
                }
            }
            PreviewContent::Diff { text } => Buffer::PreviewDiff {
                path: path.to_owned(),
                diff: diff::Diff::new(text),
            },
            PreviewContent::GitDiff { diff } => Buffer::PreviewDiff {
                path: path.to_owned(),
                diff: diff::Diff::from_git(diff),
            },
            PreviewContent::Snapshot { snapshot } => {
                let mut text = TextBuffer::new_preview_with_path(
                    &snapshot.text,
                    &self.absolute(Path::new(&snapshot.id.path)),
                );
                text.set_readonly(true);
                Buffer::PreviewText {
                    path: snapshot.id.path.clone(),
                    snapshot: Some(snapshot.id),
                    text,
                }
            }
            PreviewContent::Directory {
                rows,
                column_alignment,
            } => Buffer::PreviewRows {
                kind: PreviewRowKind::Directory,
                path: path.to_owned(),
                display: Display {
                    title: path.to_owned(),
                    rows,
                    column_alignment,
                    ..Display::default()
                },
            },
            PreviewContent::Rows {
                rows,
                column_alignment,
            } => Buffer::PreviewRows {
                kind: PreviewRowKind::Content,
                path: path.to_owned(),
                display: Display {
                    title: path.to_owned(),
                    rows,
                    column_alignment,
                    ..Display::default()
                },
            },
            PreviewContent::Unsupported { message } => Buffer::PreviewRows {
                kind: PreviewRowKind::Unsupported,
                path: path.to_owned(),
                display: Display {
                    title: path.to_owned(),
                    rows: vec![DisplayRow {
                        text: message,
                        ..DisplayRow::default()
                    }],
                    ..Display::default()
                },
            },
        })
    }

    fn update_surface_content(
        &mut self,
        old: Option<BufferId>,
        path: String,
        content: PreviewContent,
    ) -> Result<BufferId> {
        if let Some(buffer) = old {
            let replacement = match (&self.buffers[&buffer], &content) {
                (
                    Buffer::PreviewText {
                        path: previous,
                        snapshot: None,
                        ..
                    },
                    PreviewContent::Text { text },
                ) if previous == &path => Some(text.as_str()),
                (
                    Buffer::PreviewText {
                        snapshot: Some(previous),
                        ..
                    },
                    PreviewContent::Snapshot { snapshot },
                ) if previous == &snapshot.id => Some(snapshot.text.as_str()),
                _ => None,
            };
            if let Some(contents) = replacement {
                self.buffers
                    .get_mut(&buffer)
                    .unwrap()
                    .text()
                    .unwrap()
                    .replace_readonly(contents)?;
            } else {
                let model = self.preview_buffer(&path, content)?;
                self.cancel_diff_source_for_buffer(buffer);
                self.buffers.insert(buffer, model);
            }
            Ok(buffer)
        } else {
            let model = self.preview_buffer(&path, content)?;
            Ok(self.add_buffer(model))
        }
    }

    fn update_preview_content(
        &mut self,
        page: PageId,
        slot: &str,
        target: Option<ViewId>,
        path: String,
        content: PreviewContent,
    ) -> Result<ViewId> {
        if matches!(
            content,
            PreviewContent::Diff { .. } | PreviewContent::GitDiff { .. }
        ) {
            if let Some(target) = target {
                let buffer = self.views[&target].location.buffer;
                if matches!(&self.buffers[&buffer], Buffer::PreviewDiff { path: old, diff } if old == &path && match &content {
                    PreviewContent::Diff { text } => diff.source.is_none() && diff.raw == *text,
                    PreviewContent::GitDiff { diff: incoming } => diff.matches_git(incoming),
                    _ => false,
                }) {
                    return Ok(target);
                }
                if self.prompt.as_ref().is_some_and(|prompt| {
                    prompt.view == target && matches!(prompt.kind, PromptKind::DiffSearch(_))
                }) {
                    self.prompt_command(PromptCommand::Cancel)?;
                }
            }
            if let Some(target) = target {
                self.cancel_diff_source_for_buffer(self.views[&target].location.buffer);
            }
            let diff = match content {
                PreviewContent::Diff { text } => diff::Diff::new(text),
                PreviewContent::GitDiff { diff } => diff::Diff::from_git(diff),
                _ => unreachable!(),
            };
            let reused = target.filter(|id| matches!(&self.buffers[&self.views[id].location.buffer], Buffer::PreviewDiff { path: old, diff: previous } if old == &path && previous.source == diff.source)).map(|id| self.views[&id].location.buffer);
            if let Some(buffer) = reused {
                let Buffer::PreviewDiff { diff: old, .. } = &self.buffers[&buffer] else {
                    unreachable!()
                };
                for view in self
                    .views
                    .values_mut()
                    .filter(|view| view.location.buffer == buffer)
                {
                    view.location.diff_selection.reconcile(old, &diff);
                }
            }
            let selection = diff.initial_selection();
            let first = diff
                .rows
                .iter()
                .position(|row| row.hunk.is_some())
                .unwrap_or(0);
            let buffer = if let Some(target) = target {
                let buffer = self.views[&target].location.buffer;
                if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
                    text.remove_view(target);
                }
                self.buffers
                    .insert(buffer, Buffer::PreviewDiff { path, diff });
                buffer
            } else {
                self.add_buffer(Buffer::PreviewDiff { path, diff })
            };
            let target = target.unwrap_or_else(|| self.create_preview(page, slot, buffer));
            for view in self
                .views
                .values_mut()
                .filter(|view| view.location.buffer == buffer)
            {
                if reused.is_none() {
                    view.location.diff_selection = selection.clone();
                    view.location.scroll = first;
                    view.location.horizontal_scroll = 0;
                }
                view.location.selection = ItemSelection::default();
            }
            return Ok(target);
        }
        if let Some(target) = target {
            self.cancel_diff_source_for_buffer(self.views[&target].location.buffer);
        }
        let reused = if let Some(target) = target {
            let buffer = self.views[&target].location.buffer;
            let contents = match (&self.buffers[&buffer], &content) {
                (
                    Buffer::PreviewText {
                        path: old,
                        snapshot: None,
                        ..
                    },
                    PreviewContent::Text { text },
                ) if old == &path => Some(text.as_str()),
                (
                    Buffer::PreviewText {
                        path: old,
                        snapshot: Some(id),
                        ..
                    },
                    PreviewContent::Snapshot { snapshot },
                ) if old == &snapshot.id.path && id == &snapshot.id => Some(snapshot.text.as_str()),
                _ => None,
            };
            if let Some(contents) = contents {
                let text = self.buffers.get_mut(&buffer).unwrap().text().unwrap();
                text.replace_readonly(contents)?;
                Some(target)
            } else {
                None
            }
        } else {
            None
        };
        let target = if let Some(target) = reused {
            target
        } else {
            let mut model = self.preview_buffer(&path, content)?;
            if let Some(target) = target {
                let buffer = self.views[&target].location.buffer;
                if let Some(text) = model.text() {
                    text.set_registers(self.registers.clone());
                    for (id, view) in self.views.iter() {
                        if view.location.buffer == buffer {
                            text.ensure_view(*id);
                        }
                    }
                }
                let same_rows = match (&self.buffers[&buffer], &model) {
                    (
                        Buffer::PreviewRows {
                            kind: old_kind,
                            path: old_path,
                            ..
                        },
                        Buffer::PreviewRows { kind, path, .. },
                    ) => old_kind == kind && old_path == path,
                    _ => false,
                };
                self.buffers.insert(buffer, model);
                if !same_rows {
                    for view in self
                        .views
                        .values_mut()
                        .filter(|view| view.location.buffer == buffer)
                    {
                        view.location.selection = ItemSelection::default();
                        view.location.scroll = 0;
                        view.location.horizontal_scroll = 0;
                        view.location.manual_viewport = false;
                    }
                }
                target
            } else {
                let buffer = self.add_buffer(model);
                self.create_preview(page, slot, buffer)
            }
        };
        let buffer = self.views[&target].location.buffer;
        let display = match &self.buffers[&buffer] {
            Buffer::PreviewRows { display, .. } => display.clone(),
            _ => Display {
                title: path,
                ..Display::default()
            },
        };
        for view in self
            .views
            .values_mut()
            .filter(|view| view.location.buffer == buffer)
        {
            let header = std::mem::take(&mut view.location.display.header);
            let loading = view.location.display.loading;
            view.location.set_display(display.clone());
            view.location.display.header = header;
            view.location.display.loading = loading;
            view.location.selection = ItemSelection::default();
        }
        Ok(target)
    }

    pub(super) fn open_browser_source(&mut self, source: ViewId, argument: Value) -> Result<()> {
        self.focus_view(source)?;
        if !matches!(
            &self.buffers[&self.views[&source].location.buffer],
            Buffer::Data { .. } | Buffer::Items { .. }
        ) {
            bail!("command requires an item browser");
        }
        let path = argument
            .get("path")
            .and_then(Value::as_str)
            .context("browser.open-source requires a path")?;
        let line = argument.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
        let buffer = self.load_plugin_file(path)?;
        self.switch_buffer(buffer)?;
        self.buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .goto_line(source, line);
        self.remember_text();
        Ok(())
    }

    pub(super) fn move_preview(&mut self, direction: ViewDirection) -> Result<()> {
        if let Some(RenderTarget::Surface { page, slot }) = self.active_preview(self.focus) {
            let (horizontal, vertical) = match direction {
                ViewDirection::Left => (-1, 0),
                ViewDirection::Right => (1, 0),
                ViewDirection::Up => (0, -1),
                ViewDirection::Down => (0, 1),
            };
            self.scroll_surface(page, &slot, horizontal, vertical);
            return Ok(());
        }
        let target = if self.buffers[&self.views[&self.focus].location.buffer].is_preview() {
            self.focus
        } else {
            let Some(target) = self.preview_target(self.focus) else {
                return Ok(());
            };
            if !self.views.contains_key(&target) {
                return Ok(());
            }
            target
        };
        // Input may arrive after an asynchronous preview replacement or resize,
        // before its first frame. Measure this view instead of using stale geometry.
        let rectangles = self.visible_rectangles();
        if let Some((_, rect)) = rectangles
            .into_iter()
            .find(|(id, _)| *id == RenderTarget::View(target))
        {
            self.render_view(target, rect.width, rect.height);
        }
        let (horizontal, vertical) = match direction {
            ViewDirection::Left => (-1, 0),
            ViewDirection::Right => (1, 0),
            ViewDirection::Up => (0, -1),
            ViewDirection::Down => (0, 1),
        };
        let buffer = self.views[&target].location.buffer;
        match self.buffers.get_mut(&buffer).unwrap() {
            Buffer::PreviewText { text, .. } => text.scroll_view(target, horizontal, vertical),
            Buffer::PreviewDiff { diff, .. } => {
                let view = self.views.get_mut(&target).unwrap();
                view.location.scroll = view
                    .location
                    .scroll
                    .saturating_add_signed(vertical)
                    .min(diff.rows.len().saturating_sub(view.viewport_height.max(1)));
                view.location.horizontal_scroll = view
                    .location
                    .horizontal_scroll
                    .saturating_add_signed(horizontal);
                view.location.manual_viewport = true;
            }
            Buffer::PreviewRows { .. } => {
                let view = self.views.get_mut(&target).unwrap();
                view.location.scroll = view.location.scroll.saturating_add_signed(vertical).min(
                    view.location
                        .display
                        .rows
                        .len()
                        .saturating_sub(view.viewport_height.max(1)),
                );
                if horizontal != 0 {
                    view.location.horizontal_scroll = view
                        .location
                        .horizontal_scroll
                        .saturating_add_signed(horizontal)
                        .min(view.horizontal_scroll_limit());
                }
                view.location.manual_viewport = true;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn scroll_surface(
        &mut self,
        page: PageId,
        slot: &str,
        horizontal: isize,
        vertical: isize,
    ) {
        if let Some(surface) = self.views.surface_mut(page, slot) {
            surface.viewport.scroll = surface.viewport.scroll.saturating_add_signed(vertical);
            surface.viewport.horizontal_scroll = surface
                .viewport
                .horizontal_scroll
                .saturating_add_signed(horizontal);
        }
        if let Some((_, rect)) = self.visible_rectangles().into_iter().find(|(target, _)| {
            *target
                == (RenderTarget::Surface {
                    page,
                    slot: slot.into(),
                })
        }) {
            self.render_surface_content(page, slot, rect);
        }
    }

    pub(super) fn focus_preview(&mut self) -> Result<()> {
        let Some(target) = self.preview_target(self.focus) else {
            return Ok(());
        };
        let Some(view) = self.views.get(&target) else {
            return Ok(());
        };
        if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] {
            let view = self.views.get_mut(&target).unwrap();
            let visible = view.location.scroll
                ..view
                    .location
                    .scroll
                    .saturating_add(view.viewport_height.max(1));
            if view.location.manual_viewport {
                view.location.diff_selection.focus_visible(diff, visible);
                view.location.manual_viewport = false;
            }
        }
        let view = &self.views[&target];
        match &self.buffers[&view.location.buffer] {
            Buffer::PreviewText { .. }
            | Buffer::PreviewRows { .. }
            | Buffer::PreviewDiff { .. } => self.focus_view(target)?,
            _ => {}
        }
        Ok(())
    }

    pub(super) fn begin_diff_search(&mut self, forward: bool) -> Result<()> {
        let view = &self.views[&self.focus];
        if !matches!(
            self.buffers[&view.location.buffer],
            Buffer::PreviewDiff { .. }
        ) {
            return Ok(());
        }
        self.prompt = Some(Prompt {
            kind: PromptKind::DiffSearch(diff::Search {
                forward,
                selection: view.location.diff_selection.clone(),
                scroll: view.location.scroll,
                horizontal_scroll: view.location.horizontal_scroll,
                manual_viewport: view.location.manual_viewport,
            }),
            text: String::new(),
            view: self.focus,
            completion: None,
            cursor: 0,
            error: None,
        });
        Ok(())
    }

    pub(super) fn search_diff(&mut self, forward: bool) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] else {
            return Ok(());
        };
        if let Some(pattern) = self
            .registers
            .search_pattern()
            .filter(|pattern| !pattern.is_empty())
        {
            if let Some(row) = diff.search(&pattern, view.location.diff_selection.row(), forward)? {
                view.location.diff_selection.set_head(Some(row));
                view.location.manual_viewport = false;
            }
        }
        Ok(())
    }

    pub(super) fn move_diff(&mut self, forward: bool, block: bool) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] {
            view.location.diff_selection.move_by(diff, forward, block);
            view.location.manual_viewport = false;
        }
        Ok(())
    }

    pub(super) fn navigate_diff(&mut self, command: PreviewCommand) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] else {
            return Ok(());
        };
        let count = std::mem::take(&mut view.count).value();
        match command {
            PreviewCommand::DiffGotoFirst | PreviewCommand::DiffGotoLast => {
                view.location.diff_selection.move_to_edge(
                    diff,
                    command == PreviewCommand::DiffGotoLast,
                    count,
                );
            }
            PreviewCommand::DiffPageHalfDown | PreviewCommand::DiffPageHalfUp => {
                let forward = command == PreviewCommand::DiffPageHalfDown;
                let page = zess_ui::HalfPageScroll::new(
                    view.viewport_height,
                    count.unwrap_or(1),
                    if forward {
                        zess_ui::ScrollDirection::Down
                    } else {
                        zess_ui::ScrollDirection::Up
                    },
                );
                let row = view
                    .location
                    .diff_selection
                    .row()
                    .unwrap_or(view.location.scroll);
                let target = if forward {
                    row.saturating_add(page.distance())
                } else {
                    row.saturating_sub(page.distance())
                };
                view.location.diff_selection.move_to(diff, target);
                if let Some(row) = view.location.diff_selection.row() {
                    view.location.scroll = page.viewport_start(view.location.scroll, row, 5);
                }
            }
            _ => unreachable!("only diff navigation commands are dispatched here"),
        }
        view.location.manual_viewport = false;
        Ok(())
    }

    pub(super) fn select_diff_hunk(&mut self) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] {
            view.location.diff_selection.select_block(diff);
        }
        Ok(())
    }

    pub(super) fn toggle_diff_selection(&mut self) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] {
            view.location.diff_selection.toggle(diff);
        }
        Ok(())
    }

    pub(super) fn range_diff_selection(&mut self) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        if matches!(
            self.buffers[&view.location.buffer],
            Buffer::PreviewDiff { .. }
        ) {
            view.location.diff_selection.toggle_range();
        }
        Ok(())
    }

    pub(super) fn return_from_preview(&mut self) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        if matches!(
            self.buffers[&view.location.buffer],
            Buffer::PreviewDiff { .. }
        ) && matches!(view.location.diff_selection, diff::Selection::Range { .. })
        {
            view.location.diff_selection.toggle_range();
            return Ok(());
        }
        if let Some(source) = self.preview_source(self.focus) {
            self.focus_view(source)?;
        }
        Ok(())
    }

    fn ensure_diff_ready(&self, id: ViewId) -> Result<()> {
        anyhow::ensure!(
            !self.views[&id].location.display.loading,
            "wait for the current diff to finish loading"
        );
        if let Some(previews) = self.views.previews(self.views.page_id(id))
            && previews.contains(id)
        {
            anyhow::ensure!(
                previews.publication == PreviewPublication::Ready,
                "wait for Git previews to finish loading"
            );
        }
        Ok(())
    }

    pub(super) fn apply_diff_selection(&mut self) -> Result<()> {
        use zess_git::PatchRequest;
        if self.git_patch.is_some() {
            bail!("a Git index update is already running");
        }
        let source = self
            .preview_source(self.focus)
            .context("staging requires a Git changes preview")?;
        self.ensure_diff_ready(self.focus)?;
        let view = &self.views[&self.focus];
        let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer] else {
            bail!("staging requires a raw diff");
        };
        let zess_git::DiffSource::Changes { side } =
            diff.source.as_ref().context("this diff cannot be staged")?
        else {
            bail!("historical diffs cannot be staged");
        };
        let side = *side;
        let range = view
            .location
            .diff_selection
            .rows(diff)
            .context("this diff has no selected text lines")?;
        let file = diff
            .file_at(*range.start())
            .context("selected line has no Git file")?;
        anyhow::ensure!(
            diff.file_at(*range.end())
                .is_some_and(|last| last.target == file.target),
            "cannot stage a selection across files"
        );
        let raw = file.raw.clone();
        let rows = vec![view.location.diff_selection.patch_rows(diff).unwrap()];
        // Validate before starting work so unsupported/binary patches and empty
        // selections fail immediately without an index operation.
        zess_git::patch::RawPatch::parse(&raw)?.selection(side, &rows)?;
        let task = self.services.borrow().start_patch(PatchRequest {
            path: file.target.path.clone(),
            original_path: file.target.original_path.clone(),
            side,
            raw,
            rows,
        })?;
        self.git_patch = Some(PendingPatch {
            source,
            buffer: self.views[&source].location.buffer,
            task,
        });
        self.message = match side {
            zess_git::DiffSide::Staged => "Unstaging selected changes…",
            zess_git::DiffSide::Unstaged => "Staging selected changes…",
        }
        .into();
        Ok(())
    }

    pub(super) fn poll_git_patch(&mut self) -> bool {
        let Some(result) = self
            .git_patch
            .as_ref()
            .and_then(|pending| pending.task.poll())
        else {
            return false;
        };
        let pending = self.git_patch.take().unwrap();
        let message = match result {
            Ok(()) => "Git index updated".into(),
            Err(error) => error,
        };
        if self
            .views
            .get(&pending.source)
            .is_some_and(|view| view.location.buffer == pending.buffer)
        {
            if let Err(error) = self.dispatch(pending.source, "refresh", Value::Null) {
                self.message = format!("{message}; refresh: {error:#}");
                return true;
            }
        }
        self.message = message;
        true
    }

    fn diff_source(&self, id: ViewId) -> Result<SourceLine> {
        self.ensure_diff_ready(id)?;
        let view = &self.views[&id];
        let Buffer::PreviewDiff { path, diff } = &self.buffers[&view.location.buffer] else {
            bail!("source requires a diff")
        };
        let row = view
            .location
            .diff_selection
            .row()
            .context("this diff has no selected source line")?;
        let mut line = diff.rows[row]
            .source_line
            .context("this diff has no selected source line")?;
        let Some(source) = &diff.source else {
            anyhow::ensure!(!path.is_empty(), "preview has no source path");
            return Ok(SourceLine::ExplicitPath {
                path: path.clone(),
                line,
            });
        };
        let file = diff.file_at(row).context("this diff has no Git source")?;
        if let Some(origin) = self.preview_source(id) {
            if *source
                == (zess_git::DiffSource::Changes {
                    side: zess_git::DiffSide::Staged,
                })
            {
                let group = self.views.previews(self.views.page_id(origin)).unwrap();
                anyhow::ensure!(
                    !group.members.iter().any(|member| member
                        .view()
                        .is_some_and(|view| view.location.display.loading)),
                    "wait for Git previews to finish loading"
                );
                for member in &group.members {
                    let Some(view) = member.view() else {
                        continue;
                    };
                    if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&view.location.buffer]
                        && diff.source
                            == Some(zess_git::DiffSource::Changes {
                                side: zess_git::DiffSide::Unstaged,
                            })
                    {
                        line = diff.map_old_line(Some(&file.target), line);
                    }
                }
            }
        }
        Ok(match source {
            zess_git::DiffSource::Changes { .. } => SourceLine::Worktree {
                path: file.target.path.clone(),
                line,
            },
            zess_git::DiffSource::Commit { commit } => SourceLine::Commit {
                commit: commit.clone(),
                path: file.target.path.clone(),
                line,
            },
        })
    }

    pub(super) fn edit_diff_source(&mut self) -> Result<()> {
        if !matches!(
            self.buffers[&self.views[&self.focus].location.buffer],
            Buffer::PreviewDiff { .. }
        ) {
            return Ok(());
        }
        self.open_diff_source(SnapshotPresentation::Fullscreen)
    }

    pub(super) fn open_worktree_source(
        &mut self,
        view: ViewId,
        path: String,
        line: usize,
        presentation: SnapshotPresentation,
    ) -> Result<()> {
        let absolute = self.plugin_file_path(&path)?;
        anyhow::ensure!(absolute.try_exists()?, "Git source file no longer exists");
        self.focus_view(view)?;
        match presentation {
            SnapshotPresentation::Fullscreen => self.open_file_layer(view, &path, line),
            SnapshotPresentation::Editor => self.open_preview_path(path, line),
        }
    }

    fn open_diff_source(&mut self, presentation: SnapshotPresentation) -> Result<()> {
        match self.diff_source(self.focus)? {
            SourceLine::Worktree { path, line } => {
                self.open_worktree_source(self.focus, path, line, presentation)
            }
            SourceLine::ExplicitPath { path, line } => match presentation {
                SnapshotPresentation::Fullscreen => self.open_file_layer(self.focus, &path, line),
                SnapshotPresentation::Editor => self.open_preview_path(path, line),
            },
            SourceLine::Commit { commit, path, line } => {
                self.cancel_diff_source(None);
                let handle = self
                    .services
                    .borrow_mut()
                    .start_query(QueryRequest::Blob { commit, path })?;
                self.pending_diff_source = Some(PendingDiffSource {
                    origin: self.focus,
                    buffer: self.views[&self.focus].location.buffer,
                    handle,
                    presentation,
                    line,
                });
                Ok(())
            }
        }
    }

    fn cancel_diff_source_for_buffer(&mut self, buffer: BufferId) {
        if self
            .pending_diff_source
            .as_ref()
            .is_some_and(|pending| pending.buffer == buffer)
        {
            self.cancel_diff_source(None);
        }
    }

    pub(super) fn cancel_diff_source(&mut self, origin: Option<ViewId>) {
        if self
            .pending_diff_source
            .as_ref()
            .is_some_and(|pending| origin.is_none_or(|origin| pending.origin == origin))
            && let Some(pending) = self.pending_diff_source.take()
        {
            self.services.borrow_mut().cancel_query(pending.handle);
        }
    }

    pub(super) fn poll_diff_source(&mut self) -> bool {
        let Some(pending) = &self.pending_diff_source else {
            return false;
        };
        if self.focus != pending.origin
            || !self
                .views
                .get(&pending.origin)
                .is_some_and(|view| view.location.buffer == pending.buffer)
        {
            self.cancel_diff_source(None);
            return false;
        }
        let result = self.services.borrow_mut().poll_query(pending.handle);
        let result = match result {
            Ok(zess_services::QueryPoll::Pending) => return false,
            Ok(zess_services::QueryPoll::Ready(result)) => result.map_err(anyhow::Error::msg),
            Err(error) => Err(error),
        };
        let pending = self.pending_diff_source.take().unwrap();
        let opened = match result {
            Ok(QueryResult::Snapshot { snapshot }) => {
                let source = match pending.presentation {
                    SnapshotPresentation::Fullscreen => pending.origin,
                    SnapshotPresentation::Editor => self
                        .preview_source(pending.origin)
                        .unwrap_or(pending.origin),
                };
                self.open_snapshot(source, snapshot, pending.presentation)
                    .map(|()| {
                        let id = self.focus;
                        let buffer = self.views[&id].location.buffer;
                        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
                            text.goto_line(id, pending.line);
                        }
                    })
            }
            Ok(QueryResult::Unsupported { message }) => Err(anyhow::anyhow!(message)),
            Ok(_) => Err(anyhow::anyhow!(
                "historical file query returned another kind of result"
            )),
            Err(error) => Err(error),
        };
        if let Err(error) = opened {
            self.message = format!("{error:#}");
        }
        true
    }

    pub(super) fn open_preview_source(&mut self) -> Result<()> {
        let view = &self.views[&self.focus];
        if let Buffer::PreviewText {
            snapshot: Some(id),
            text,
            ..
        } = &self.buffers[&view.location.buffer]
        {
            let snapshot = FileSnapshot {
                id: id.clone(),
                text: text.text(),
            };
            let source = self.preview_source(self.focus).unwrap_or(self.focus);
            return self.open_snapshot(source, snapshot, SnapshotPresentation::Editor);
        }
        let (path, line) = match &self.buffers[&view.location.buffer] {
            Buffer::PreviewDiff { .. } => {
                return self.open_diff_source(SnapshotPresentation::Editor);
            }
            Buffer::PreviewText { path, text, .. } => {
                (path.clone(), text.view(self.focus).cursor().0)
            }
            Buffer::PreviewRows {
                kind: PreviewRowKind::Directory,
                path,
                ..
            } => {
                let path = path.clone();
                if self.absolute(Path::new(&path)).is_dir() {
                    if let Some(source) = self.preview_source(self.focus) {
                        let view = &self.views[&source];
                        if let Buffer::Data { kind, .. } = &self.buffers[&view.location.buffer]
                            && kind == "files"
                        {
                            self.focus_view(source)?;
                            return self.dispatch(source, "navigate", json!({"path":path}));
                        }
                    }
                    return self.open_plugin("files", json!({"path":path}));
                }
                (path, 0)
            }
            _ => return Ok(()),
        };
        self.open_preview_path(path, line)
    }

    fn open_preview_path(&mut self, path: String, line: usize) -> Result<()> {
        if path.is_empty() {
            bail!("preview has no source path");
        }
        let source = self.views.main(self.views.page_id(self.focus));
        if matches!(
            self.buffers[&self.views[&source].location.buffer],
            Buffer::Data { .. } | Buffer::Items { .. }
        ) {
            self.focus_view(source)?;
            return self.open_browser_source(source, json!({"path":path, "line":line}));
        }
        self.request(self.focus, HostRequest::OpenFile { path, line })
    }
}
