use zess_ui::{MouseContext, MouseEvent, MouseKind};

use super::*;

#[derive(PartialEq)]
pub(super) enum Content {
    Text {
        version: u64,
        positions: Vec<Vec<usize>>,
    },
    Items {
        ids: Vec<String>,
    },
    Diff {
        scroll: usize,
        presentation: diff::Presentation,
    },
    Rows,
}

#[derive(PartialEq)]
pub(super) struct Region {
    pub target: RenderTarget,
    pub buffer: BufferId,
    pub pane: Rect,
    pub body: Rect,
    pub content: Content,
}

impl Session {
    pub fn invalidate_mouse(&mut self) {
        self.mouse_drag = None;
        self.mouse_regions.clear();
        self.mouse_context.content_revision = self.mouse_context.content_revision.wrapping_add(1);
        self.mouse_context.layout_revision = self.mouse_context.layout_revision.wrapping_add(1);
    }

    pub(super) fn mouse_enabled(&self) -> bool {
        self.scripts
            .as_ref()
            .map(|runtime| runtime.editor_options())
            .unwrap_or_default()
            .mouse
    }

    pub(super) fn record_command_mouse(&mut self) {
        self.command_mouse = self
            .active_prompt()
            .filter(|prompt| matches!(prompt.kind, PromptKind::Command))
            .map(|prompt| {
                self.command_grid(
                    prompt,
                    usize::from(self.width),
                    usize::from(self.height.saturating_sub(1)),
                )
            });
    }

    pub fn mouse(&mut self, context: MouseContext, event: MouseEvent) {
        let stale = if matches!(event.kind, MouseKind::ScrollUp | MouseKind::ScrollDown) {
            context.layout_revision != self.mouse_context.layout_revision
        } else {
            context.content_revision != self.mouse_context.content_revision
        };
        if !self.mouse_enabled() || stale {
            self.mouse_drag = None;
            return;
        }
        if let Some(prompt) = self.active_prompt() {
            if !matches!(prompt.kind, PromptKind::Command) {
                self.mouse_drag = None;
                return;
            }
            if !matches!(event.kind, MouseKind::Press | MouseKind::DoubleClick) {
                return;
            }
            if self.height > 0 && event.row == self.height - 1 && event.column < self.width {
                return;
            }
            let Some(grid) = &self.command_mouse else {
                return;
            };
            match grid.hit(
                usize::from(event.column),
                usize::from(event.row),
                usize::from(self.width),
            ) {
                Some(Some(index)) => {
                    let name = grid.entries[index].name.clone();
                    let prefix = grid.prefix.clone();
                    let prompt = self.prompt.as_mut().unwrap();
                    prompt.text = name;
                    prompt.cursor = prompt.text.len();
                    prompt.completion = Some(Completion { prefix, index });
                    prompt.error = None;
                    return;
                }
                Some(None) => return,
                None => {
                    self.prompt = None;
                    self.pending.clear();
                    self.command_mouse = None;
                }
            }
        }
        if event.kind == MouseKind::Release {
            self.mouse_drag = None;
            return;
        }
        let before = self.selection_snapshot();
        if let Err(error) = self.handle_mouse(event) {
            self.message = format!("mouse: {error:#}");
            self.mouse_drag = None;
        }
        let after = self.selection_snapshot();
        if before != after
            && let Err(error) = self.emit_event("selection-changed", after)
        {
            self.message = format!("event: {error:#}");
        }
    }

    fn clear_mouse_input(&mut self, id: ViewId) {
        self.pending.clear();
        let view = self.views.get_mut(&id).unwrap();
        view.count.clear();
        view.register = RegisterInput::None;
        view.jump = None;
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> Result<()> {
        let x = usize::from(event.column);
        let y = usize::from(event.row);
        let Some(region) = self
            .mouse_regions
            .iter()
            .rposition(|region| region.pane.contains(x, y))
        else {
            return Ok(());
        };
        let buffer = self.mouse_regions[region].buffer;
        let id = match self.mouse_regions[region].target.clone() {
            RenderTarget::View(id) => id,
            RenderTarget::Surface { page, slot } => {
                if self
                    .views
                    .surface(page, &slot)
                    .is_none_or(|surface| surface.buffer != buffer)
                {
                    return Ok(());
                }
                self.mouse_drag = None;
                match event.kind {
                    MouseKind::Press | MouseKind::DoubleClick => {
                        let main = self.views.main(page);
                        self.focus_view(main)?;
                        self.clear_mouse_input(main);
                        self.views.previews_mut(page).unwrap().active = slot;
                    }
                    MouseKind::ScrollUp | MouseKind::ScrollDown => {
                        let lines = self
                            .scripts
                            .as_ref()
                            .map(|runtime| runtime.editor_options())
                            .unwrap_or_default()
                            .scroll_lines as isize;
                        let lines = if event.kind == MouseKind::ScrollUp {
                            -lines
                        } else {
                            lines
                        };
                        self.scroll_surface(page, &slot, 0, lines);
                    }
                    MouseKind::Drag | MouseKind::Release => {}
                }
                return Ok(());
            }
        };
        if !self
            .views
            .get(&id)
            .is_some_and(|view| view.location.buffer == buffer)
        {
            return Ok(());
        }
        if matches!(event.kind, MouseKind::Press | MouseKind::DoubleClick) {
            self.mouse_drag = None;
            if self.help.as_ref().is_none_or(|help| help.view != id) {
                self.focus_view(id)?;
            }
        }
        self.clear_mouse_input(id);
        match event.kind {
            MouseKind::ScrollUp | MouseKind::ScrollDown => {
                self.mouse_drag = None;
                let lines = self
                    .scripts
                    .as_ref()
                    .map(|runtime| runtime.editor_options())
                    .unwrap_or_default()
                    .scroll_lines as isize;
                let lines = if event.kind == MouseKind::ScrollUp {
                    -lines
                } else {
                    lines
                };
                if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
                    text.scroll_view(id, 0, lines);
                } else {
                    let view = &self.views[&id];
                    let limit = match &self.buffers[&buffer] {
                        Buffer::Data { .. } | Buffer::Items { .. } => {
                            zess_ui::scroll_limit_with_margin(
                                view.location.display.rows.len(),
                                view.viewport_height,
                                5,
                            )
                        }
                        Buffer::PreviewDiff { diff, .. } => diff
                            .display_rows(view.location.diff_presentation)
                            .saturating_sub(view.viewport_height.max(1)),
                        Buffer::PreviewRows { .. } => view
                            .location
                            .display
                            .rows
                            .len()
                            .saturating_sub(view.viewport_height.max(1)),
                        Buffer::Text(_) | Buffer::Snapshot { .. } | Buffer::PreviewText { .. } => {
                            unreachable!("text buffers scroll through their text view")
                        }
                    };
                    let view = self.views.get_mut(&id).unwrap();
                    view.location.scroll =
                        view.location.scroll.saturating_add_signed(lines).min(limit);
                    view.location.manual_viewport = true;
                }
            }
            MouseKind::Press | MouseKind::DoubleClick | MouseKind::Drag => {
                let region = &self.mouse_regions[region];
                if !region.body.contains(x, y)
                    || (event.kind == MouseKind::Drag && self.mouse_drag != Some(id))
                {
                    return Ok(());
                }
                let row = y - region.body.y;
                let column = x - region.body.x;
                enum Target {
                    Text(usize),
                    Item(String),
                    Diff(usize, diff::Presentation),
                    None,
                }
                let target = match &region.content {
                    Content::Text { positions, .. } => positions
                        .get(row)
                        .and_then(|line| line.get(column))
                        .copied()
                        .map_or(Target::None, Target::Text),
                    Content::Items { ids } => ids
                        .get(row)
                        .filter(|id| !id.is_empty())
                        .cloned()
                        .map_or(Target::None, Target::Item),
                    Content::Diff {
                        scroll,
                        presentation,
                    } => Target::Diff(scroll + row, *presentation),
                    Content::Rows => Target::None,
                };
                if matches!(target, Target::None) {
                    return Ok(());
                }
                self.clipboard_read = None;
                self.message.clear();
                let extend = event.kind == MouseKind::Drag;
                match target {
                    Target::Text(position) => {
                        self.buffers
                            .get_mut(&buffer)
                            .and_then(Buffer::text)
                            .unwrap()
                            .select_at(id, position, extend);
                    }
                    Target::Diff(row, presentation) => {
                        let Buffer::PreviewDiff { diff, .. } = &self.buffers[&buffer] else {
                            return Ok(());
                        };
                        let view = self.views.get_mut(&id).unwrap();
                        let row = match presentation {
                            diff::Presentation::Raw => Some(row),
                            diff::Presentation::Rendered if diff.mapped_rendering() => Some(row),
                            diff::Presentation::Rendered => {
                                diff.raw_selection_at_rendered(row).row()
                            }
                        };
                        if let Some(row) = row
                            && view.location.diff_selection.select_at(
                                diff.index(view.location.diff_presentation),
                                row,
                                extend,
                            )
                        {
                            view.location.manual_viewport = false;
                        }
                    }
                    Target::Item(target) => {
                        let view = self.views.get_mut(&id).unwrap();
                        view.jump = None;
                        view.count.clear();
                        let ids = view.item_ids();
                        if extend {
                            let anchor = view
                                .location
                                .selection
                                .anchor
                                .as_ref()
                                .and_then(|anchor| ids.iter().position(|id| id == anchor));
                            let current = ids.iter().position(|id| id == &target);
                            if let (Some(anchor), Some(current)) = (anchor, current) {
                                view.location.selection.selected =
                                    ids[anchor.min(current)..=anchor.max(current)].to_vec();
                            }
                        } else {
                            view.location.selection.anchor = Some(target.clone());
                            view.location.selection.selected = vec![target.clone()];
                        }
                        view.location.selection.primary = Some(target);
                        self.dispatch(
                            id,
                            "select",
                            serde_json::to_value(SelectionChange {
                                navigation: Some(ItemNavigation::Jump),
                            })?,
                        )?;
                    }
                    Target::None => unreachable!(),
                }
                self.mouse_drag = Some(id);
                if event.kind == MouseKind::DoubleClick
                    && self.focus == id
                    && let Some(view) = self.views.get(&id)
                {
                    let kind = match &self.buffers[&view.location.buffer] {
                        Buffer::Text(_) => "text",
                        Buffer::Snapshot { .. } => "snapshot",
                        Buffer::PreviewText { .. } => "preview",
                        Buffer::PreviewDiff { .. } => "diff",
                        Buffer::PreviewRows { kind, .. } => kind.scope(),
                        Buffer::Data { kind, .. } | Buffer::Items { kind, .. } => kind,
                    };
                    let options = self.effective_view_options(id, kind);
                    if let Some(command) =
                        options.get("double-click-command").and_then(Value::as_str)
                    {
                        self.mouse_drag = None;
                        self.execute_command(command, json!({}))?;
                    }
                }
            }
            MouseKind::Release => unreachable!("release is handled before hit testing"),
        }
        Ok(())
    }
}
