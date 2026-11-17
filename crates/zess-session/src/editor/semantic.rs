use super::*;
use zess_plugin::{StatusContent, StatusContext, StatusPosition};
use zess_protocol::{
    PromptSnapshot, SemanticRow, SessionSnapshot, SourceHighlight, SurfaceSnapshot, TextMetrics,
    TextRange, ViewArea, ViewContent, ViewSelection, ViewSnapshot, VisibleTextLine,
};

fn text_content(lines: Vec<zess_text::SourceLine>) -> ViewContent {
    ViewContent::Text {
        lines: lines
            .into_iter()
            .map(|line| VisibleTextLine {
                line: line.line,
                first_char: line.first_char,
                first_column: line.first_column,
                end_of_line: line.end_of_line,
                text: line.text,
                highlights: line
                    .highlights
                    .into_iter()
                    .map(|(range, style)| SourceHighlight {
                        start: range.start,
                        end: range.end,
                        style,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn text_metrics(text: &TextBuffer) -> TextMetrics {
    TextMetrics {
        line_count: text.line_count(),
        tab_width: text.tab_width(),
    }
}

fn item_content(display: &Display) -> ViewContent {
    ViewContent::Items {
        rows: display
            .rows
            .iter()
            .map(|row| SemanticRow {
                line_number: None,
                id: row.id.clone(),
                text: row.text.clone(),
                cells: row.cells.iter().map(|cell| cell.spans.clone()).collect(),
                style: row.style.clone(),
                depth: row.depth,
            })
            .collect(),
    }
}

fn diff_content(diff: &diff::Diff, presentation: diff::Presentation) -> ViewContent {
    if presentation == diff::Presentation::Rendered
        && let Some(rendered) = &diff.rendered
    {
        return ViewContent::Items {
            rows: rendered
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| SemanticRow {
                    line_number: None,
                    id: index.to_string(),
                    text: String::new(),
                    cells: vec![row.spans.clone()],
                    style: String::new(),
                    depth: 0,
                })
                .collect(),
        };
    }
    ViewContent::Items {
        rows: diff
            .index
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| SemanticRow {
                id: index.to_string(),
                text: diff.display_line(index),
                line_number: row.new_line.or(row.old_line),
                cells: vec![],
                style: match row.kind {
                    Some(zess_git::patch::LineKind::Addition) => "git.added",
                    Some(zess_git::patch::LineKind::Deletion) => "git.deleted",
                    None if row.hunk.is_some() => "keyword",
                    _ => "",
                }
                .into(),
                depth: 0,
            })
            .collect(),
    }
}

struct ViewBody {
    width: usize,
    height: usize,
    right_inset: usize,
}

impl ViewBody {
    fn new(width: usize, height: usize, kind: &str, display: &Display, status: bool) -> Self {
        let right_inset = usize::from(matches!(kind, "files" | "git" | "git-commit-files"))
            .min(width.saturating_sub(1));
        Self {
            width: width - right_inset,
            height: height
                .saturating_sub(usize::from(status))
                .saturating_sub(usize::from(
                    !display.header.is_empty() || !display.tabs.is_empty(),
                )),
            right_inset,
        }
    }

    fn gutter(&self, options: &Value, digits: usize) -> usize {
        if options.get("gutter") == Some(&Value::Bool(false)) {
            return 0;
        }
        let padding = options
            .get("gutter-left-padding")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .min(self.width as u64) as usize;
        (digits + padding + 1).min(self.width.saturating_sub(1))
    }
}

fn layout_diff_view(view: &mut View, diff: &diff::Diff, body: &ViewBody, options: &Value) {
    let presentation = view.location.diff_presentation;
    let index = diff.index(presentation);
    if view.location.scroll_mode == ScrollMode::FollowSelection
        && (presentation == diff::Presentation::Raw
            || diff.rendered.is_none()
            || diff.mapped_rendering())
        && let Some(rows) = view.location.diff_selection.rows(index)
    {
        view.location.scroll = view.location.scroll.min(*rows.start());
        if body.height > 0
            && let Some(row) = view.location.diff_selection.row()
            && row >= view.location.scroll + body.height
        {
            view.location.scroll = row + 1 - body.height;
        }
    }
    let gutter = if presentation == diff::Presentation::Rendered && diff.rendered.is_some() {
        0
    } else {
        body.gutter(options, diff.line_number_width)
    };
    view.location.scroll = view.location.scroll.min(
        diff.display_rows(presentation)
            .saturating_sub(body.height.max(1)),
    );
    view.location.horizontal_scroll = view.location.horizontal_scroll.min(
        diff.display_width(presentation)
            .saturating_sub(body.width - gutter),
    );
}

fn layout_surface_rows(
    viewport: &mut zess_text::PreviewViewport,
    width: usize,
    height: usize,
    buffer: &Buffer,
) {
    let (len, limit) = match buffer {
        Buffer::PreviewRows { display, .. } => {
            let limit = if viewport.horizontal_scroll == 0 {
                0
            } else {
                zess_ui::RowColumns::measure(&display.rows, display.column_alignment)
                    .horizontal_scroll_limit(width)
            };
            (display.rows.len(), limit)
        }
        Buffer::PreviewDiff { diff, .. } => {
            (diff.index.rows.len(), diff.width.saturating_sub(width))
        }
        _ => unreachable!("surface rows must belong to a row or diff preview"),
    };
    viewport.width = width;
    viewport.height = height;
    viewport.scroll = viewport.scroll.min(len.saturating_sub(height.max(1)));
    viewport.horizontal_scroll = viewport.horizontal_scroll.min(limit);
}

impl Session {
    fn layout_with_layers(&self) -> Vec<(RenderTarget, Rect)> {
        if let Some(help) = &self.help {
            return vec![(
                RenderTarget::View(help.view),
                Rect {
                    x: 0,
                    y: 0,
                    width: usize::from(self.width),
                    height: usize::from(self.height.saturating_sub(1)),
                },
            )];
        }
        let mut layout = Vec::new();
        for (mut target, mut rect) in self.visible_rectangles() {
            while let RenderTarget::View(parent) = target {
                let Some(child) = self.views.child(parent) else {
                    break;
                };
                if self.views.presentation(child) == LayerPresentation::Inline {
                    layout.push((target, rect));
                    let view = &self.views[&parent];
                    let index = view
                        .location
                        .selection
                        .primary
                        .as_ref()
                        .and_then(|id| {
                            view.location
                                .display
                                .rows
                                .iter()
                                .position(|row| &row.id == id)
                        })
                        .unwrap_or(0);
                    let height = rect.height.saturating_sub(1);
                    let overlay = height.min(2);
                    rect.y +=
                        (index.saturating_sub(view.location.scroll) + 1).min(height - overlay);
                    rect.height = overlay;
                }
                target = RenderTarget::View(child);
            }
            layout.push((target, rect));
        }
        layout
    }

    pub(super) fn view_metadata(&self, id: ViewId) -> (String, String, bool) {
        let location = &self.views[&id].location;
        match &self.buffers[&location.buffer] {
            Buffer::Text(text) => (
                "text".to_owned(),
                text.path()
                    .map(|path| {
                        path.strip_prefix(&self.workspace)
                            .unwrap_or(path)
                            .display()
                            .to_string()
                    })
                    .unwrap_or_else(|| match self.views.layer_content(id) {
                        Some(LayerContent::Draft { purpose }) => purpose.clone(),
                        _ => "[scratch]".into(),
                    }),
                text.modified(),
            ),
            Buffer::Snapshot { id, .. } => (
                "snapshot".into(),
                format!("{}@{}", id.path, &id.commit[..8.min(id.commit.len())]),
                false,
            ),
            Buffer::PreviewText { path, .. } => ("preview".into(), path.clone(), false),
            Buffer::PreviewDiff { path, .. } => ("diff".into(), path.clone(), false),
            Buffer::Data { kind, modified, .. } => (
                kind.clone(),
                if location.display.title.is_empty() {
                    kind.clone()
                } else {
                    location.display.title.clone()
                },
                *modified,
            ),
            Buffer::Items { kind, path, .. } => (
                kind.clone(),
                if location.display.title.is_empty() {
                    path.clone()
                } else {
                    location.display.title.clone()
                },
                false,
            ),
            Buffer::PreviewRows { kind, path, .. } => (
                kind.scope().into(),
                if location.display.title.is_empty() {
                    path.clone()
                } else {
                    location.display.title.clone()
                },
                false,
            ),
        }
    }

    pub(super) fn layout_preview_view(&mut self, id: ViewId) {
        let Some((_, area)) = self
            .layout_with_layers()
            .into_iter()
            .find(|(target, _)| *target == RenderTarget::View(id))
        else {
            return;
        };
        let (kind, _, _) = self.view_metadata(id);
        let mut options = self.effective_view_options(id, &kind);
        if self.views.presentation(id) == LayerPresentation::Inline {
            options["gutter"] = Value::Bool(false);
        }
        let view = self.views.get_mut(&id).unwrap();
        let body = ViewBody::new(
            area.width,
            area.height,
            &kind,
            &view.location.display,
            options.get("statusline").and_then(Value::as_str) != Some(""),
        );
        view.viewport_width = body.width;
        view.viewport_height = body.height;
        match self.buffers.get_mut(&view.location.buffer).unwrap() {
            Buffer::PreviewText { text, .. } => {
                let gutter = body.gutter(&options, text.line_count().to_string().len());
                text.layout_text_view(id, body.width - gutter, body.height);
            }
            Buffer::PreviewDiff { diff, .. } => layout_diff_view(view, diff, &body, &options),
            Buffer::PreviewRows { display, .. } => {
                view.location.scroll = view
                    .location
                    .scroll
                    .min(display.rows.len().saturating_sub(body.height.max(1)));
                view.location.horizontal_scroll = view
                    .location
                    .horizontal_scroll
                    .min(view.horizontal_scroll_limit());
            }
            _ => {}
        }
    }

    fn view_snapshot(&mut self, id: ViewId, area: Option<Rect>) -> ViewSnapshot {
        let view = &self.views[&id];
        let buffer_id = view.location.buffer;
        let (kind, title, modified) = self.view_metadata(id);
        let mut options = self.effective_view_options(id, &kind);
        if self.views.presentation(id) == LayerPresentation::Inline {
            options["gutter"] = Value::Bool(false);
        }
        let key_scopes = self
            .view_key_context(id)
            .0
            .into_iter()
            .map(str::to_owned)
            .collect();
        let view = self.views.get_mut(&id).unwrap();
        let display = &view.location.display;
        let status_name = options
            .get("statusline")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let width = area.map_or(0, |r| r.width);
        let body = ViewBody::new(
            width,
            area.map_or(0, |r| r.height),
            &kind,
            display,
            !status_name.is_empty(),
        );
        if area.is_some() {
            view.viewport_width = body.width;
            view.viewport_height = body.height;
        }
        let mut context = StatusContext {
            kind: kind.clone(),
            title: title.clone(),
            active: self.focus == id,
            modified,
            loading: display.loading,
            plugin_status: display.status.clone(),
            plugin_status_data: display.status_data.clone(),
            view_id: id,
            buffer_id,
            options: options.clone(),
            width,
            right_inset: body.right_inset,
            ..StatusContext::default()
        };
        let mut metrics = None;
        let mut mode = view.location.mode.as_str().to_owned();
        let mut version = display.rows_revision;
        let mut scroll = view.location.scroll;
        let mut horizontal_scroll = view.location.horizontal_scroll;
        let (content, selection) = match self.buffers.get_mut(&buffer_id).unwrap() {
            Buffer::Text(text)
            | Buffer::Snapshot { text, .. }
            | Buffer::PreviewText { text, .. } => {
                let digits = text.line_count().to_string().len();
                let gutter = body.gutter(&options, digits);
                let lines = if area.is_some() {
                    text.view_source_lines(id, body.width - gutter, body.height)
                } else {
                    Vec::new()
                };
                metrics = Some(text_metrics(text));
                version = Some(text.version());
                let state = text.view(id);
                scroll = state.scroll();
                horizontal_scroll = state.horizontal_scroll();
                mode = state.mode().to_owned();
                let (row, column) = state.cursor();
                context.content = StatusContent::Text {
                    position: StatusPosition {
                        row: row + 1,
                        column: column + 1,
                    },
                    selection_count: state.selection_count(),
                    encoding: "UTF-8".into(),
                };
                (
                    text_content(lines),
                    ViewSelection::Text {
                        ranges: state
                            .selection()
                            .iter()
                            .map(|r| TextRange {
                                anchor: r.anchor,
                                head: r.head,
                            })
                            .collect(),
                        primary: state.selection().primary_index(),
                    },
                )
            }
            Buffer::PreviewDiff { diff, .. } => {
                let presentation = view.location.diff_presentation;
                let index = diff.index(presentation);
                let mapped = presentation == diff::Presentation::Raw
                    || diff.rendered.is_none()
                    || diff.mapped_rendering();
                let selection = mapped
                    .then(|| view.location.diff_selection.rows(index))
                    .flatten();
                let primary = mapped.then(|| view.location.diff_selection.row()).flatten();
                let position = primary.and_then(|row| index.rows.get(row));
                context.content = StatusContent::Diff {
                    old_line: position.and_then(|r| r.old_line),
                    new_line: position.and_then(|r| r.new_line),
                    selection_count: usize::from(position.is_some()),
                };
                context.mode = Some(
                    match (mapped, &view.location.diff_selection) {
                        (false, _) => "browse",
                        (true, diff::Selection::Block { .. }) => "hunk",
                        (true, diff::Selection::Line { .. }) => "line",
                        (true, diff::Selection::Range { .. }) => "range",
                    }
                    .into(),
                );
                if area.is_some() {
                    layout_diff_view(view, diff, &body, &options);
                    scroll = view.location.scroll;
                    horizontal_scroll = view.location.horizontal_scroll;
                }
                let ids = selection
                    .into_iter()
                    .flatten()
                    .filter(|row| view.location.diff_selection.contains(index, *row))
                    .map(|row| row.to_string())
                    .collect();
                (
                    diff_content(diff, presentation),
                    ViewSelection::Items {
                        ids,
                        primary: primary.map(|row| row.to_string()),
                    },
                )
            }
            Buffer::Data { .. } | Buffer::Items { .. } | Buffer::PreviewRows { .. } => {
                let display = match &self.buffers[&buffer_id] {
                    Buffer::PreviewRows { display, .. } => display,
                    _ => &view.location.display,
                };
                let primary = view.location.selection.primary.clone();
                if area.is_some() {
                    let current = primary
                        .as_ref()
                        .and_then(|id| display.rows.iter().position(|r| &r.id == id))
                        .unwrap_or(0);
                    scroll = if matches!(&self.buffers[&buffer_id], Buffer::PreviewRows { .. }) {
                        scroll.min(display.rows.len().saturating_sub(body.height.max(1)))
                    } else if view.location.scroll_mode == ScrollMode::FollowSelection
                        && !self.buffers[&buffer_id].is_preview()
                    {
                        zess_ui::scroll_with_margin(scroll, current, body.height, 5)
                    } else {
                        scroll.min(zess_ui::scroll_limit_with_margin(
                            display.rows.len(),
                            body.height,
                            5,
                        ))
                    };
                    view.location.scroll = scroll;
                    if horizontal_scroll > 0 {
                        horizontal_scroll = horizontal_scroll.min(view.horizontal_scroll_limit());
                        view.location.horizontal_scroll = horizontal_scroll;
                    }
                }
                context.content = StatusContent::Items {
                    selection_count: view.location.selection.selected.len(),
                    primary: primary.clone(),
                    item_count: display.rows.iter().filter(|row| !row.id.is_empty()).count(),
                };
                (
                    item_content(display),
                    ViewSelection::Items {
                        ids: view.location.selection.selected.clone(),
                        primary,
                    },
                )
            }
        };
        context.mode.get_or_insert_with(|| mode.clone());
        let statusline = if status_name.is_empty() || area.is_none() {
            None
        } else {
            self.scripts.as_mut().and_then(|runtime| {
                match runtime.format_statusline(status_name, &context) {
                    Ok(line) => Some(line),
                    Err(error) => {
                        self.message = format!("statusline: {error:#}");
                        None
                    }
                }
            })
        };
        ViewSnapshot {
            view_id: id,
            buffer_id,
            kind,
            title,
            version,
            mode,
            content,
            selection,
            scroll,
            horizontal_scroll,
            text_metrics: metrics,
            options: Some(options),
            key_scopes,
            statusline,
            header: view.location.display.header.clone(),
            tabs: view.location.display.tabs.clone(),
            loading: view.location.display.loading,
            column_alignment: view.location.display.column_alignment,
        }
    }

    fn surface_geometry(&self, page: PageId, slot: &str, area: Rect) -> (usize, bool) {
        let page_id = self.views.main(page);
        let soft_wrap = matches!(
            &self.buffers[&self.views[&page_id].location.buffer],
            Buffer::Data { kind, .. } if kind == "files"
        ) && self
            .effective_view_options(page_id, "files")
            .get("preview-wrap")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let surface = self.views.surface(page, slot).unwrap();
        let height = area
            .height
            .saturating_sub(usize::from(!surface.header.is_empty() || surface.loading));
        (height, soft_wrap)
    }

    pub(super) fn layout_preview_surface(&mut self, page: PageId, slot: &str, area: Rect) {
        let (height, soft_wrap) = self.surface_geometry(page, slot, area);
        let surface = self.views.surface_mut(page, slot).unwrap();
        let buffer = self.buffers.get_mut(&surface.buffer).unwrap();
        match buffer {
            Buffer::PreviewText { text, .. } => {
                text.layout_preview(&mut surface.viewport, area.width, height, soft_wrap);
            }
            buffer => layout_surface_rows(&mut surface.viewport, area.width, height, buffer),
        }
    }

    fn surface_snapshot(&mut self, page: PageId, slot: &str, area: Rect) -> SurfaceSnapshot {
        let page_id = self.views.main(page);
        let (height, soft_wrap) = self.surface_geometry(page, slot, area);
        let surface = self.views.surface_mut(page, slot).unwrap();
        let width = area.width;
        let viewport = &mut surface.viewport;
        let mut metrics = None;
        let mut column_alignment = Default::default();
        let buffer = self.buffers.get_mut(&surface.buffer).unwrap();
        let content = match buffer {
            Buffer::PreviewText { text, .. } => {
                metrics = Some(text_metrics(text));
                text_content(text.surface_source_lines(viewport, width, height, soft_wrap))
            }
            Buffer::PreviewRows { display, .. } => {
                column_alignment = display.column_alignment;
                item_content(display)
            }
            Buffer::PreviewDiff { diff, .. } => diff_content(diff, diff::Presentation::Raw),
            _ => unreachable!("surface content must be a preview buffer"),
        };
        if !matches!(buffer, Buffer::PreviewText { .. }) {
            layout_surface_rows(viewport, width, height, buffer);
        }
        SurfaceSnapshot {
            page_id,
            slot_id: slot.to_owned(),
            buffer_id: surface.buffer,
            header: surface.header.clone(),
            loading: surface.loading,
            content,
            text_metrics: metrics,
            column_alignment,
            soft_wrap,
            scroll: viewport.scroll,
            horizontal_scroll: viewport.horizontal_scroll,
        }
    }

    pub fn snapshot(&mut self) -> SessionSnapshot {
        if let Err(error) = self.invalidate_prompt() {
            self.message = format!("prompt: {error:#}");
        }
        self.synchronize_key_sequence();
        let targets = self.layout_with_layers();
        let ids = self.views.iter().map(|(&id, _)| id).collect::<Vec<_>>();
        let mut views = ids
            .into_iter()
            .map(|id| {
                let area = targets
                    .iter()
                    .find(|(target, _)| *target == RenderTarget::View(id))
                    .map(|(_, r)| *r);
                self.view_snapshot(id, area)
            })
            .collect::<Vec<_>>();
        views.sort_by_key(|view| view.view_id);
        let mut surfaces = Vec::new();
        let mut layout = Vec::new();
        for (target, area) in targets {
            let (view_id, page_id, slot_id, buffer_id) = match target {
                RenderTarget::View(id) => (Some(id), None, None, self.views[&id].location.buffer),
                RenderTarget::Surface { page, slot } => {
                    let surface = self.surface_snapshot(page, &slot, area);
                    let id = (None, Some(surface.page_id), Some(slot), surface.buffer_id);
                    surfaces.push(surface);
                    id
                }
            };
            layout.push(ViewArea {
                view_id,
                page_id,
                slot_id,
                buffer_id,
                x: area.x,
                y: area.y,
                width: area.width,
                height: area.height,
            });
        }
        let prompt = self.active_prompt().map(|prompt| PromptSnapshot {
            kind: match &prompt.kind {
                PromptKind::Command => "command".into(),
                PromptKind::Text(kind) => kind.label().into(),
                PromptKind::DiffSearch(_) => "diff_search".into(),
                PromptKind::Filter(_) => "filter".into(),
                PromptKind::MiniBuffer { purpose, .. } => purpose.clone(),
            },
            label: prompt.label(),
            owner_buffer: prompt.target.buffer,
            text: prompt.text.clone(),
            cursor: prompt.cursor,
            error: prompt.error.clone(),
            owner_view: prompt.target.view,
        });
        SessionSnapshot {
            editor_hooks: zess_protocol::editor_events::EditorHooks {
                input: self
                    .scripts
                    .as_ref()
                    .is_some_and(|runtime| runtime.has_event("input")),
                selection_changed: self
                    .scripts
                    .as_ref()
                    .is_some_and(|runtime| runtime.has_event("selection-changed")),
            },
            workspace_root: self.workspace().to_string_lossy().into_owned(),
            editor_options: self
                .scripts
                .as_ref()
                .map(ScriptRuntime::editor_options)
                .unwrap_or_default(),
            text_options: self
                .scripts
                .as_ref()
                .map(|runtime| runtime.view_options("text"))
                .unwrap_or_default(),
            styles: self.session_styles(),
            views,
            surfaces,
            layout,
            focused_view: self.views.contains_key(&self.focus).then_some(self.focus),
            prompt,
            status: self.message.clone(),
            modified: self.modified(),
            buffers: self
                .buffers
                .iter()
                .map(|(&id, buffer)| {
                    let (kind, modified) = match buffer {
                        Buffer::Text(text) => (
                            zess_protocol::BufferKind::Text {
                                path: text.path().map(|path| path.to_string_lossy().into_owned()),
                            },
                            text.modified(),
                        ),
                        Buffer::Data { kind, modified, .. } => (
                            zess_protocol::BufferKind::Data { name: kind.clone() },
                            *modified,
                        ),
                        Buffer::Snapshot { .. } => (zess_protocol::BufferKind::Snapshot, false),
                        Buffer::PreviewText { .. }
                        | Buffer::PreviewDiff { .. }
                        | Buffer::PreviewRows { .. } => (zess_protocol::BufferKind::Preview, false),
                        Buffer::Items { kind, .. } => (
                            zess_protocol::BufferKind::Items { name: kind.clone() },
                            false,
                        ),
                    };
                    zess_protocol::BufferSummary { id, kind, modified }
                })
                .collect(),
            workspace: self.client_workspace.clone(),
            client_actions: self.client_actions.clone(),
        }
    }
}
