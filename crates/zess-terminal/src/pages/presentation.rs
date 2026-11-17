mod diff_commands;
pub(super) use diff_commands::PreviewSource;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::Value;
use zess_git::patch::LineKind;
use zess_plugin::{
    PreviewArea, PreviewContent, PreviewLayout, PreviewPane, StatusConfig, StatusContent,
    StatusContext,
};
use zess_protocol::workspace_view::pages::PreviewPosition;
use zess_text::diff::{Diff, Presentation, Selection};
use zess_text::{PreviewViewport, TextBuffer, TextCommand, TextEffect};
use zess_ui::{
    DisplayRow, Frame, FrameRow, RowColumns, ScrollDirection, Span, TextStyle, align_row,
    clip_spans, fit, skip_columns, spans_width,
};

use super::Page;
use crate::presentation::{gutter_width, header_style};
use crate::statusline::{LoadingScope, Statuslines, Target};

fn span(text: impl Into<String>, style: &str) -> Span {
    Span {
        text: text.into(),
        style: style.into(),
        base_style: None,
    }
}

fn row(text: impl Into<String>, style: &str) -> FrameRow {
    FrameRow {
        spans: vec![span(text, style)],
    }
}

pub(crate) fn item_rows(
    rows: &[DisplayRow],
    columns: &RowColumns,
    viewport: &PreviewViewport,
    selected: &[String],
    jump: Option<&zess_ui::ItemJump>,
) -> Vec<FrameRow> {
    rows.iter()
        .enumerate()
        .skip(viewport.scroll)
        .take(viewport.height)
        .map(|(index, row)| {
            let style = if selected.contains(&row.id) {
                format!("selection.{}", row.style)
            } else {
                row.style.clone()
            };
            let mut spans = zess_ui::item_label(row, &style, jump);
            skip_columns(&mut spans, viewport.horizontal_scroll);
            FrameRow {
                spans: align_row(
                    spans,
                    &row.cells,
                    &style,
                    &columns.layout(index, viewport.width),
                ),
            }
        })
        .collect()
}

struct DiffPreview {
    diff: Rc<Diff>,
    presentation: Presentation,
    selection: Selection,
    follow_selection: bool,
    search: Option<diff_commands::Search>,
}

impl DiffPreview {
    fn new(diff: Diff) -> Self {
        let presentation = if diff.rendered.is_some() {
            Presentation::Rendered
        } else {
            Presentation::Raw
        };
        let selection = diff.index(presentation).initial_selection();
        Self {
            diff: Rc::new(diff),
            presentation,
            selection,
            follow_selection: true,
            search: None,
        }
    }

    fn activate(&mut self, viewport: &mut PreviewViewport) {
        if self.presentation == Presentation::Rendered && !self.diff.mapped_rendering() {
            if !self.follow_selection {
                self.selection = self.diff.raw_selection_at_rendered(viewport.scroll);
            }
            self.presentation = Presentation::Raw;
            viewport.scroll = self.selection.row().unwrap_or(0);
            viewport.horizontal_scroll = 0;
        } else if !self.follow_selection {
            self.selection.focus_visible(
                self.diff.index(self.presentation),
                viewport.scroll..viewport.scroll.saturating_add(viewport.height.max(1)),
            );
        }
        self.follow_selection = true;
    }

    fn deactivate(&mut self, viewport: &mut PreviewViewport) {
        if self.presentation == Presentation::Raw && self.diff.rendered.is_some() {
            viewport.scroll = self.diff.rendered_row_at_raw(viewport.scroll);
            self.presentation = Presentation::Rendered;
            viewport.horizontal_scroll = 0;
        }
    }

    fn command(&mut self, name: &str, count: usize, height: usize) -> bool {
        let index = self.diff.index(self.presentation);
        match name {
            "diff.move-next" | "diff.move-previous" | "diff.hunk-next" | "diff.hunk-previous" => {
                for _ in 0..count.max(1) {
                    self.selection.move_by(
                        index,
                        name.ends_with("next"),
                        name.starts_with("diff.hunk"),
                    );
                }
            }
            "diff.goto-first" | "diff.goto-last" => {
                self.selection.move_to_edge(
                    index,
                    name.ends_with("last"),
                    (count > 1).then_some(count),
                );
            }
            "diff.page-half-down" | "diff.page-half-up" => {
                let forward = name.ends_with("down");
                let direction = if forward {
                    ScrollDirection::Down
                } else {
                    ScrollDirection::Up
                };
                let distance =
                    zess_ui::HalfPageScroll::new(height, count.max(1), direction).distance();
                let head = self.selection.row().unwrap_or(0);
                self.selection.move_to(
                    index,
                    if forward {
                        head.saturating_add(distance)
                    } else {
                        head.saturating_sub(distance)
                    },
                );
            }
            "diff.select-hunk" => self.selection.select_block(index),
            "diff.toggle-selection" => self.selection.toggle(index),
            "diff.range-selection" => self.selection.toggle_range(),
            _ => return false,
        }
        self.follow_selection = true;
        true
    }

    fn render(
        &self,
        viewport: &mut PreviewViewport,
        focused: bool,
        interactive: bool,
        options: Option<&Value>,
        styles: &mut HashMap<String, TextStyle>,
    ) -> Vec<FrameRow> {
        let width = viewport.width;
        let height = viewport.height;
        let index = self.diff.index(self.presentation);
        if self.follow_selection
            && (self.presentation == Presentation::Raw || self.diff.mapped_rendering())
            && let Some(range) = self.selection.rows(index)
        {
            viewport.scroll = viewport.scroll.min(*range.start());
            if let Some(head) = self.selection.row() {
                viewport.scroll = viewport
                    .scroll
                    .max(head.saturating_add(1).saturating_sub(height.max(1)));
            }
        }
        viewport.scroll = viewport.scroll.min(
            self.diff
                .display_rows(self.presentation)
                .saturating_sub(height.max(1)),
        );
        let gutter = if interactive && self.presentation == Presentation::Raw {
            gutter_width(options, self.diff.line_number_width, width)
        } else {
            0
        };
        viewport.horizontal_scroll = viewport.horizontal_scroll.min(
            self.diff
                .display_width(self.presentation)
                .saturating_sub(width - gutter),
        );
        let columns = RowColumns::default();
        let range = viewport.scroll
            ..viewport
                .scroll
                .saturating_add(height)
                .min(self.diff.display_rows(self.presentation));
        if self.presentation == Presentation::Rendered
            && let Some(rendered) = &self.diff.rendered
        {
            styles.extend(rendered.styles.clone());
            return range
                .map(|index_row| {
                    let mut spans = rendered.rows[index_row].spans.clone();
                    let selected = focused
                        && self.diff.mapped_rendering()
                        && self.selection.contains(index, index_row);
                    if selected {
                        for span in &mut spans {
                            span.base_style = Some(std::mem::take(&mut span.style));
                            span.style = "diff.selection".into();
                        }
                    }
                    skip_columns(&mut spans, viewport.horizontal_scroll);
                    FrameRow {
                        spans: align_row(
                            spans,
                            &[],
                            if selected { "diff.selection" } else { "" },
                            &columns.layout(0, width),
                        ),
                    }
                })
                .collect();
        }
        range
            .map(|i| {
                let metadata = &self.diff.index.rows[i];
                let selected = focused && self.selection.contains(index, i);
                let base = match metadata.kind {
                    Some(LineKind::Addition) => "git.added",
                    Some(LineKind::Deletion) => "git.deleted",
                    None if metadata.hunk.is_some() => "keyword",
                    _ => "",
                };
                let style = if selected {
                    format!("selection.{base}")
                } else {
                    base.into()
                };
                let mut spans = vec![span(self.diff.display_line(i), &style)];
                skip_columns(&mut spans, viewport.horizontal_scroll);
                let mut spans = align_row(spans, &[], &style, &columns.layout(0, width - gutter));
                if gutter > 0 {
                    let line = metadata
                        .new_line
                        .or(metadata.old_line)
                        .map(|line| line.to_string())
                        .unwrap_or_default();
                    spans.insert(
                        0,
                        span(
                            fit(&format!("{line:>digits$} ", digits = gutter - 1), gutter),
                            if selected { "linenr.current" } else { "linenr" },
                        ),
                    );
                }
                FrameRow { spans }
            })
            .collect()
    }
}

// Each preview owns one buffer, so its local text view has a stable identity.
const PREVIEW_VIEW: u64 = 1;

#[derive(Clone)]
struct Items {
    primary: usize,
    anchor: usize,
    selecting: bool,
}

impl Items {
    fn choose(&mut self, target: usize, rows: &[DisplayRow], viewport: &mut PreviewViewport) {
        self.primary = target.min(rows.len().saturating_sub(1));
        if !self.selecting {
            self.anchor = self.primary;
        }
        viewport.scroll =
            zess_ui::scroll_with_margin(viewport.scroll, self.primary, viewport.height, 5);
    }

    fn command(
        &mut self,
        name: &str,
        count: usize,
        rows: &[DisplayRow],
        viewport: &mut PreviewViewport,
    ) -> bool {
        let target = match name {
            "data.move-up" => self.primary.saturating_sub(count),
            "data.move-down" => self.primary.saturating_add(count),
            "data.goto-first" => count.saturating_sub(1),
            "data.goto-last" => rows.len().saturating_sub(1),
            "data.page-half-up" => self
                .primary
                .saturating_sub((viewport.height / 2).max(1).saturating_mul(count)),
            "data.page-half-down" => self
                .primary
                .saturating_add((viewport.height / 2).max(1).saturating_mul(count)),
            "data.select-mode" => {
                self.selecting = !self.selecting;
                self.anchor = self.primary;
                return true;
            }
            "data.keep-primary" | "data.escape" if self.selecting => {
                self.selecting = false;
                self.anchor = self.primary;
                return true;
            }
            "data.select-all" => {
                self.selecting = true;
                self.anchor = 0;
                rows.len().saturating_sub(1)
            }
            _ => return false,
        };
        self.choose(target, rows, viewport);
        true
    }
}

enum Content {
    Text {
        buffer: TextBuffer,
        snapshot: Option<zess_plugin::SnapshotId>,
    },
    Rows {
        rows: Vec<DisplayRow>,
        columns: RowColumns,
        scope: &'static str,
        items: Option<Items>,
    },
    Diff(DiffPreview),
}

impl Content {
    fn new(content: PreviewContent, path: &str, root: &Path) -> Self {
        match content {
            PreviewContent::Text { text } => Self::Text {
                buffer: TextBuffer::new_remote_preview(&text, Path::new(path)),
                snapshot: None,
            },
            PreviewContent::Snapshot { snapshot } => Self::Text {
                buffer: TextBuffer::new_remote_preview(
                    &snapshot.text,
                    Path::new(&snapshot.id.path),
                ),
                snapshot: Some(snapshot.id),
            },
            PreviewContent::Diff { text } => Self::Diff(DiffPreview::new(Diff::new(text))),
            PreviewContent::GitDiff { diff } => {
                Self::Diff(DiffPreview::new(Diff::from_git(diff, root)))
            }
            PreviewContent::Rows {
                rows,
                column_alignment,
            } => Self::Rows {
                columns: RowColumns::measure(&rows, column_alignment),
                rows,
                scope: "preview",
                items: None,
            },
            PreviewContent::Items {
                rows,
                column_alignment,
            } => Self::Rows {
                columns: RowColumns::measure(&rows, column_alignment),
                rows,
                scope: "preview",
                items: Some(Items {
                    primary: 0,
                    anchor: 0,
                    selecting: false,
                }),
            },
            PreviewContent::Directory {
                rows,
                column_alignment,
            } => Self::Rows {
                columns: RowColumns::measure(&rows, column_alignment),
                rows,
                scope: "directory",
                items: None,
            },
            PreviewContent::Unsupported { message } => Self::Rows {
                rows: vec![DisplayRow {
                    text: message,
                    ..Default::default()
                }],
                columns: RowColumns::default(),
                scope: "preview",
                items: None,
            },
        }
    }

    fn scope(&self) -> &str {
        match self {
            Self::Text { .. } => "preview",
            Self::Rows { items: Some(_), .. } => "preview-items",
            Self::Rows { .. } => "preview-rows",
            Self::Diff(_) => "diff",
        }
    }
}

struct Pane {
    id: String,
    path: String,
    header: String,
    loading: bool,
    interactive: bool,
    key_scope: Option<String>,
    content: Content,
    viewport: PreviewViewport,
    area: Option<PreviewArea>,
    text_body: Option<PreviewArea>,
    cursor: Option<(u16, u16)>,
}

impl Pane {
    fn split(&self) -> anyhow::Result<Self> {
        let content = match &self.content {
            Content::Text { buffer, snapshot } => {
                let mut copy =
                    TextBuffer::new_remote_preview(&buffer.text(), Path::new(&self.path));
                if self.interactive {
                    let state = buffer.view(PREVIEW_VIEW).workspace_state();
                    copy.restore_view(
                        PREVIEW_VIEW,
                        &state.ranges,
                        state.primary,
                        &state.mode,
                        state.scroll,
                        state.horizontal_scroll,
                    )?;
                }
                Content::Text {
                    buffer: copy,
                    snapshot: snapshot.clone(),
                }
            }
            Content::Rows {
                rows,
                columns,
                scope,
                items,
            } => Content::Rows {
                rows: rows.clone(),
                columns: columns.clone(),
                scope,
                items: items.clone(),
            },
            Content::Diff(diff) => Content::Diff(DiffPreview {
                diff: diff.diff.clone(),
                presentation: diff.presentation,
                selection: diff.selection.clone(),
                follow_selection: diff.follow_selection,
                search: None,
            }),
        };
        Ok(Self {
            id: self.id.clone(),
            path: self.path.clone(),
            header: self.header.clone(),
            loading: self.loading,
            interactive: self.interactive,
            key_scope: self.key_scope.clone(),
            content,
            viewport: PreviewViewport {
                scroll: self.viewport.scroll,
                horizontal_scroll: self.viewport.horizontal_scroll,
                ..Default::default()
            },
            area: None,
            text_body: None,
            cursor: None,
        })
    }

    fn scroll(&mut self, horizontal: isize, vertical: isize) {
        if self.interactive
            && let Content::Text { buffer, .. } = &mut self.content
        {
            buffer.scroll_view(PREVIEW_VIEW, horizontal, vertical);
        }
        self.viewport.scroll = self.viewport.scroll.saturating_add_signed(vertical);
        self.viewport.horizontal_scroll = self
            .viewport
            .horizontal_scroll
            .saturating_add_signed(horizontal);
        if let Content::Diff(diff) = &mut self.content {
            diff.follow_selection = false;
        }
    }

    fn new(pane: PreviewPane, root: &Path, old: &mut Vec<Self>) -> Self {
        let (id, path, header, loading, content, interactive, key_scope) = match pane {
            PreviewPane::Surface {
                id,
                path,
                header,
                loading,
                content,
            } => (id, path, header, loading, content, false, None),
            PreviewPane::View {
                id,
                path,
                header,
                loading,
                content,
                key_scope,
            } => (id, path, header, loading, content, true, key_scope),
        };
        let previous = if let Some(index) = old
            .iter()
            .position(|pane| pane.id == id && pane.path == path && pane.interactive == interactive)
        {
            let mut pane = old.swap_remove(index);
            let incoming = match &content {
                PreviewContent::Text { text } => Some((text, None)),
                PreviewContent::Snapshot { snapshot } => Some((&snapshot.text, Some(&snapshot.id))),
                _ => None,
            };
            let retain_content = if let (Content::Text { buffer, snapshot }, Some((text, identity))) =
                (&mut pane.content, incoming)
                && snapshot.as_ref() == identity
            {
                buffer
                    .replace_readonly(text)
                    .expect("preview buffers are read-only");
                true
            } else if let Content::Diff(current) = &mut pane.content {
                let unchanged = match &content {
                    PreviewContent::Diff { text } => {
                        current.diff.source.is_none() && current.diff.raw == *text
                    }
                    PreviewContent::GitDiff { diff } => current.diff.matches_git(diff),
                    _ => false,
                };
                if !unchanged {
                    current.cancel_search(&mut pane.viewport);
                }
                unchanged
            } else {
                false
            };
            if retain_content {
                pane.area = None;
                pane.text_body = None;
                pane.header = header;
                pane.loading = loading;
                pane.key_scope = key_scope;
                return pane;
            }
            Some(pane)
        } else {
            None
        };
        let mut content = Content::new(content, &path, root);
        if interactive && let Content::Text { buffer, .. } = &mut content {
            buffer.ensure_view(PREVIEW_VIEW);
        }
        let mut viewport = PreviewViewport::default();
        if let Content::Diff(diff) = &content
            && diff.presentation == Presentation::Raw
        {
            viewport.scroll = diff
                .diff
                .index
                .rows
                .iter()
                .position(|row| row.hunk.is_some())
                .unwrap_or(0);
        }
        if let Some(previous) = previous {
            if let (Content::Diff(previous_diff), Content::Diff(next)) =
                (&previous.content, &mut content)
            {
                next.presentation = if previous_diff.presentation == Presentation::Rendered
                    && next.diff.rendered.is_none()
                {
                    Presentation::Raw
                } else {
                    previous_diff.presentation
                };
                next.selection = previous_diff.selection.clone();
                next.selection.reconcile(
                    &previous_diff.diff,
                    &next.diff,
                    previous_diff.presentation,
                    next.presentation,
                );
                next.follow_selection = previous_diff.follow_selection;
                viewport = previous.viewport;
            } else if let (Content::Rows { .. }, Content::Rows { .. }) =
                (&previous.content, &content)
            {
                viewport = previous.viewport;
            }
        }
        Self {
            id,
            content,
            cursor: None,
            path,
            header,
            loading,
            interactive,
            key_scope,
            viewport,
            area: None,
            text_body: None,
        }
    }

    fn render(
        &mut self,
        area: PreviewArea,
        focused: bool,
        wrap: bool,
        styles: &mut HashMap<String, TextStyle>,
        config: Option<&zess_plugin::StatusConfig>,
        statuslines: &mut crate::statusline::Statuslines,
        subscription: u64,
        generation: u64,
    ) -> Vec<FrameRow> {
        self.area = Some(area);
        self.cursor = None;
        self.text_body = None;
        if area.width == 0 || area.height == 0 {
            return Vec::new();
        }
        let kind = match &self.content {
            Content::Text { .. } => "preview",
            Content::Diff(_) => "diff",
            Content::Rows { scope, .. } => *scope,
        };
        let options = config.and_then(|config| config.view_options.get(kind));
        let status_target = Target::Preview {
            subscription,
            slot: self.id.clone(),
        };
        let loading = statuslines.show_loading(
            &status_target,
            self.loading.then_some(LoadingScope::View(generation)),
        );
        let mut rows = Vec::new();
        let header = if loading && !self.interactive {
            format!("{}  Loading…", self.header)
        } else {
            self.header.clone()
        };
        if !header.is_empty() {
            rows.push(row(fit(&header, area.width), header_style(options)));
        }
        let status_name = options
            .and_then(|options| options.get("statusline"))
            .and_then(Value::as_str)
            .unwrap_or("default");
        let status_height =
            usize::from(self.interactive && !status_name.is_empty() && rows.len() < area.height);
        self.viewport.width = area.width;
        self.viewport.height = area.height.saturating_sub(rows.len() + status_height);
        let height = self.viewport.height;
        match &mut self.content {
            Content::Text { buffer, .. } => {
                if self.interactive {
                    let gutter =
                        gutter_width(options, buffer.line_count().to_string().len(), area.width);
                    let body = PreviewArea {
                        x: area.x + gutter,
                        y: area.y + rows.len(),
                        width: area.width - gutter,
                        height,
                    };
                    self.text_body = Some(body);
                    let render = buffer.render(PREVIEW_VIEW, body.width, body.height, focused);
                    let line = buffer.view(PREVIEW_VIEW).cursor().0;
                    self.viewport.scroll = render.scroll;
                    self.viewport.horizontal_scroll = buffer.view(PREVIEW_VIEW).horizontal_scroll();
                    self.viewport.hit_positions = render.hit_positions;
                    self.cursor = render.cursor.map(|(x, y)| {
                        (
                            (body.x + usize::from(x)) as u16,
                            (body.y + usize::from(y)) as u16,
                        )
                    });
                    for (i, mut row) in render.rows.into_iter().enumerate() {
                        let number = render.scroll + i + 1;
                        if gutter > 0 {
                            row.spans.insert(
                                0,
                                span(
                                    fit(
                                        &format!("{number:>digits$} ", digits = gutter - 1),
                                        gutter,
                                    ),
                                    if focused && line + 1 == number {
                                        "linenr.current"
                                    } else {
                                        "linenr"
                                    },
                                ),
                            );
                        }
                        rows.push(row);
                    }
                } else {
                    rows.extend(buffer.render_preview(
                        &mut self.viewport,
                        area.width,
                        height,
                        wrap,
                    ));
                }
            }
            Content::Rows {
                rows: items,
                columns,
                items: selection,
                ..
            } => {
                self.viewport.scroll = self
                    .viewport
                    .scroll
                    .min(items.len().saturating_sub(height.max(1)));
                self.viewport.horizontal_scroll = self
                    .viewport
                    .horizontal_scroll
                    .min(columns.horizontal_scroll_limit(area.width));
                let selected = selection
                    .as_ref()
                    .filter(|_| focused)
                    .map(|selection| {
                        let range = selection.primary.min(selection.anchor)
                            ..=selection.primary.max(selection.anchor);
                        items
                            .get(range)
                            .unwrap_or_default()
                            .iter()
                            .map(|row| row.id.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                rows.extend(item_rows(items, columns, &self.viewport, &selected, None));
            }
            Content::Diff(diff) => {
                rows.extend(diff.render(
                    &mut self.viewport,
                    focused,
                    self.interactive,
                    options,
                    styles,
                ));
            }
        }
        rows.resize_with(area.height - status_height, FrameRow::default);
        if status_height > 0 {
            use zess_plugin::{StatusContent, StatusContext, StatusPosition};
            let (mode, content) = match &self.content {
                Content::Text { buffer, .. } => {
                    let view = buffer.view(PREVIEW_VIEW);
                    let (line, column) = view.cursor();
                    (
                        view.mode(),
                        StatusContent::Text {
                            position: StatusPosition {
                                row: line + 1,
                                column: column + 1,
                            },
                            selection_count: view.selection_count(),
                            encoding: "UTF-8".into(),
                        },
                    )
                }
                Content::Diff(diff) => {
                    let metadata = diff
                        .selection
                        .row()
                        .and_then(|i| diff.diff.index(diff.presentation).rows.get(i));
                    let mode = if diff.presentation == Presentation::Rendered
                        && !diff.diff.mapped_rendering()
                    {
                        "browse"
                    } else {
                        match &diff.selection {
                            Selection::Block { .. } => "hunk",
                            Selection::Line { .. } => "line",
                            Selection::Range { .. } => "range",
                        }
                    };
                    (
                        mode,
                        StatusContent::Diff {
                            old_line: metadata.and_then(|row| row.old_line),
                            new_line: metadata.and_then(|row| row.new_line),
                            selection_count: usize::from(metadata.is_some()),
                        },
                    )
                }
                Content::Rows {
                    rows,
                    items: Some(items),
                    ..
                } => (
                    if items.selecting { "select" } else { "normal" },
                    StatusContent::Items {
                        selection_count: if rows.is_empty() {
                            0
                        } else {
                            items.primary.abs_diff(items.anchor) + 1
                        },
                        primary: rows.get(items.primary).map(|row| row.id.clone()),
                        item_count: rows.len(),
                    },
                ),
                Content::Rows { .. } => ("normal", StatusContent::None),
            };
            rows.push(FrameRow {
                spans: statuslines.render(
                    status_target,
                    config,
                    status_name,
                    StatusContext {
                        kind: kind.into(),
                        title: self.path.clone(),
                        active: focused,
                        loading,
                        width: area.width,
                        mode: Some(mode.into()),
                        options: options.cloned().unwrap_or_default(),
                        content,
                        ..StatusContext::default()
                    },
                    None,
                ),
            });
        }
        rows
    }
}

pub(super) struct Previews {
    panes: Vec<Pane>,
    layout: PreviewLayout,
    active: usize,
    focused: bool,
    dragging: Option<String>,
    pending: bool,
    root: PathBuf,
}

impl Default for Previews {
    fn default() -> Self {
        Self {
            panes: Vec::new(),
            layout: PreviewLayout::Rows,
            active: 0,
            focused: false,
            dragging: None,
            pending: false,
            root: PathBuf::new(),
        }
    }
}

impl Previews {
    pub(super) fn split(&self) -> anyhow::Result<Self> {
        Ok(Self {
            panes: self
                .panes
                .iter()
                .map(Pane::split)
                .collect::<anyhow::Result<_>>()?,
            layout: self.layout,
            active: self.active,
            focused: self.focused,
            pending: self.pending,
            root: self.root.clone(),
            dragging: None,
        })
    }

    pub(super) fn focused_slot(&self) -> Option<&str> {
        self.focused
            .then(|| self.panes.get(self.active))?
            .map(|pane| pane.id.as_str())
    }

    pub(super) fn diff_search_active(&self) -> bool {
        self.focused
            && self.panes.get(self.active).is_some_and(
                |pane| matches!(&pane.content, Content::Diff(diff) if diff.search.is_some()),
            )
    }

    pub(super) fn event_selection(&self) -> Option<zess_protocol::editor_events::EditorSelection> {
        use zess_protocol::editor_events::{DiffSelection, EditorSelection};
        let pane = self.panes.get(self.active).filter(|_| self.focused)?;
        Some(match &pane.content {
            Content::Text { buffer, .. } => EditorSelection::Text {
                cursors: buffer.selections(PREVIEW_VIEW),
            },
            Content::Diff(preview) => {
                let rows = preview
                    .selection
                    .rows(preview.diff.index(preview.presentation));
                EditorSelection::Diff {
                    diff_selection: DiffSelection {
                        row: preview.selection.row(),
                        start_row: rows.as_ref().map(|range| *range.start()),
                        end_row: rows.as_ref().map(|range| *range.end()),
                    },
                }
            }
            Content::Rows { items, rows, .. } => {
                let mut selection = zess_plugin::ItemSelection::default();
                if let Some(items) = items {
                    selection.primary = rows.get(items.primary).map(|row| row.id.clone());
                    selection.anchor = rows.get(items.anchor).map(|row| row.id.clone());
                    let start = items.primary.min(items.anchor);
                    let end = items.primary.max(items.anchor);
                    if let Some(selected) = rows.get(start..=end) {
                        selection.selected = selected.iter().map(|row| row.id.clone()).collect();
                    }
                }
                EditorSelection::Items { selection }
            }
        })
    }

    pub(super) fn set_root(&mut self, path: &str) {
        self.root = path.into();
    }

    pub(super) fn scope(&self) -> Option<&str> {
        if !self.focused {
            return None;
        }
        let pane = self.panes.get(self.active)?;
        Some(
            pane.key_scope
                .as_deref()
                .unwrap_or_else(|| pane.content.scope()),
        )
    }

    pub(super) fn kind(&self) -> Option<&str> {
        if !self.focused {
            return None;
        }
        self.panes.get(self.active).map(|pane| pane.content.scope())
    }

    pub(super) fn mode(&self) -> Option<&str> {
        if !self.focused {
            return None;
        }
        match &self.panes.get(self.active)?.content {
            Content::Text { buffer, .. } => Some(buffer.view(PREVIEW_VIEW).mode()),
            Content::Rows {
                items: Some(items), ..
            } if items.selecting => Some("select"),
            _ => Some("normal"),
        }
    }

    pub(super) fn poll_syntax(&mut self) -> bool {
        let mut changed = false;
        for pane in &mut self.panes {
            if let Content::Text { buffer, .. } = &mut pane.content {
                changed |= buffer.poll_syntax()
            }
        }
        changed
    }

    pub(super) fn text_view(&mut self) -> Option<(&mut TextBuffer, u64)> {
        if !self.focused {
            return None;
        }
        if let Content::Text { buffer, .. } = &mut self.panes.get_mut(self.active)?.content {
            Some((buffer, PREVIEW_VIEW))
        } else {
            None
        }
    }

    pub(super) fn text_command(
        &mut self,
        name: &str,
        count: usize,
    ) -> Option<anyhow::Result<TextEffect>> {
        let command = TextCommand::parse(name)?;
        let (text, view) = self.text_view()?;
        if count > 1 {
            for digit in count.to_string().bytes() {
                text.push_command_count(view, digit - b'0');
            }
        }
        Some(text.execute(view, command))
    }

    pub(super) fn text_input(&mut self, key: &str) -> Option<anyhow::Result<TextEffect>> {
        let (text, view) = self.text_view()?;
        Some(text.input_text(view, key))
    }

    pub(super) fn paste(&mut self, text: &str) -> Option<anyhow::Result<TextEffect>> {
        let (buffer, view) = self.text_view()?;
        Some(buffer.paste_text(view, text))
    }

    pub(super) fn snapshot(&self) -> Option<PreviewPosition> {
        let pane = self.panes.get(self.active)?;
        Some(PreviewPosition {
            slot: pane.id.clone(),
            scroll: pane.viewport.scroll,
            horizontal_scroll: pane.viewport.horizontal_scroll,
            focused: self.focused,
        })
    }

    pub(super) fn restore(&mut self, position: &PreviewPosition) -> bool {
        let Some(index) = self
            .panes
            .iter()
            .position(|pane| pane.id == position.slot && !pane.loading)
        else {
            return false;
        };
        self.active = index;
        if position.focused {
            self.focus();
        } else {
            self.leave();
        }
        let pane = &mut self.panes[index];
        if pane.interactive
            && let Content::Text { buffer, .. } = &mut pane.content
        {
            let view = buffer.view(PREVIEW_VIEW);
            let offset = |value: usize| value.min(isize::MAX as usize) as isize;
            let horizontal = offset(position.horizontal_scroll) - offset(view.horizontal_scroll());
            let vertical = offset(position.scroll) - offset(view.scroll());
            buffer.scroll_view(PREVIEW_VIEW, horizontal, vertical);
        }
        pane.viewport.scroll = position.scroll;
        pane.viewport.horizontal_scroll = position.horizontal_scroll;
        if let Content::Diff(diff) = &mut pane.content {
            diff.follow_selection = false;
        }
        true
    }

    pub(super) fn leave(&mut self) -> bool {
        if !self.focused {
            return false;
        }
        if let Some(pane) = self.panes.get_mut(self.active)
            && let Content::Diff(diff) = &mut pane.content
        {
            diff.deactivate(&mut pane.viewport);
        }
        self.focused = false;
        true
    }

    pub(super) fn focus(&mut self) -> bool {
        let Some(pane) = self
            .panes
            .get_mut(self.active)
            .filter(|pane| pane.interactive)
        else {
            return false;
        };
        self.focused = true;
        if let Content::Diff(diff) = &mut pane.content {
            diff.activate(&mut pane.viewport);
        }
        true
    }

    pub(super) fn mouse(&mut self, event: &zess_ui::MouseEvent, scroll_lines: usize) -> bool {
        use zess_ui::MouseKind;

        let extend = event.kind == MouseKind::Drag;
        if event.kind == MouseKind::Release {
            return self.dragging.take().is_some();
        }
        if !extend {
            self.dragging = None;
        }
        let (x, y) = (usize::from(event.column), usize::from(event.row));
        let Some(index) = self
            .panes
            .iter()
            .position(|pane| pane.area.is_some_and(|area| area.contains(x, y)))
        else {
            return extend && self.dragging.is_some();
        };
        let pane = &mut self.panes[index];
        if matches!(event.kind, MouseKind::ScrollUp | MouseKind::ScrollDown) {
            let lines = scroll_lines.min(isize::MAX as usize) as isize;
            pane.scroll(
                0,
                if event.kind == MouseKind::ScrollUp {
                    -lines
                } else {
                    lines
                },
            );
            return true;
        }
        if extend && self.dragging.as_ref() != Some(&pane.id) {
            return true;
        }
        let area = pane.area.expect("hit testing found this pane");
        let body = area.y + usize::from(!pane.header.is_empty());
        let row = y
            .checked_sub(body)
            .filter(|&row| row < pane.viewport.height)
            .map(|row| pane.viewport.scroll + row);
        let diff_row = row.and_then(|row| match &pane.content {
            Content::Diff(diff)
                if diff.presentation == Presentation::Rendered && !diff.diff.mapped_rendering() =>
            {
                diff.diff.raw_selection_at_rendered(row).row()
            }
            _ => Some(row),
        });
        if !extend {
            if self.active != index {
                self.leave();
            }
            self.active = index;
            self.focus();
        }
        let pane = &mut self.panes[index];
        if !pane.interactive {
            return true;
        }
        let selected = match &mut pane.content {
            Content::Text { buffer, .. } => {
                let pos = pane
                    .text_body
                    .filter(|body| body.contains(x, y))
                    .and_then(|body| {
                        pane.viewport
                            .hit_positions
                            .get(y - body.y)?
                            .get(x - body.x)
                            .copied()
                    });
                if let Some(pos) = pos {
                    buffer.select_at(PREVIEW_VIEW, pos, extend);
                }
                pos.is_some()
            }
            Content::Diff(diff) => {
                let selected = diff_row.is_some_and(|row| {
                    diff.selection
                        .select_at(diff.diff.index(diff.presentation), row, extend)
                });
                if selected {
                    diff.follow_selection = true;
                }
                selected
            }
            Content::Rows {
                rows,
                items: Some(items),
                ..
            } => {
                if let Some(row) =
                    row.filter(|&row| rows.get(row).is_some_and(|item| !item.id.is_empty()))
                {
                    if !extend {
                        items.anchor = row;
                    }
                    items.primary = row;
                    true
                } else {
                    false
                }
            }
            Content::Rows { .. } => false,
        };
        if selected && !extend {
            self.dragging = Some(pane.id.clone());
        }
        true
    }

    pub(super) fn replace(&mut self, panes: Vec<PreviewPane>, layout: PreviewLayout, focus: bool) {
        let previous = self.snapshot();
        let mut old = std::mem::take(&mut self.panes);
        let dragged_diff = self.dragging.take().and_then(|id| {
            let pane = old.iter().find(|pane| pane.id == id)?;
            let Content::Diff(preview) = &pane.content else {
                return None;
            };
            Some((id, Rc::clone(&preview.diff)))
        });
        self.panes = panes
            .into_iter()
            .map(|pane| Pane::new(pane, &self.root, &mut old))
            .collect();
        self.dragging = dragged_diff.and_then(|(id, diff)| {
            self.panes
                .iter()
                .any(|pane| {
                    pane.id == id
                        && matches!(&pane.content, Content::Diff(preview) if Rc::ptr_eq(&preview.diff, &diff))
                })
                .then_some(id)
        });
        self.layout = layout;
        let retained = previous
            .as_ref()
            .and_then(|p| self.panes.iter().position(|pane| pane.id == p.slot));
        self.active = retained.unwrap_or(0);
        self.focused &=
            retained.is_some() && self.panes.get(self.active).is_some_and(|p| p.interactive);
        self.confirm(focus);
    }

    pub(super) fn confirm(&mut self, focus: bool) {
        self.pending = false;
        if focus {
            self.focus();
        }
    }

    pub(super) fn single(&mut self, path: String, content: PreviewContent, focus: bool) {
        self.replace(
            vec![PreviewPane::View {
                id: "main".into(),
                path,
                header: String::new(),
                loading: false,
                content,
                key_scope: None,
            }],
            PreviewLayout::Rows,
            focus,
        );
    }

    pub(super) fn configure(&mut self, header: String, key_scope: Option<String>) {
        for pane in &mut self.panes {
            pane.header.clone_from(&header);
            if pane.interactive {
                pane.key_scope.clone_from(&key_scope);
            }
        }
    }

    pub(super) fn pending(&mut self) {
        self.pending = true;
    }

    pub(super) fn command(&mut self, name: &str, count: usize) -> bool {
        let count = count.max(1).min(isize::MAX as usize) as isize;
        match name {
            "preview.focus" => self.focus(),
            "preview.return" => self.leave(),
            "preview.move-left" | "preview.move-right" | "preview.move-up"
            | "preview.move-down" => {
                let (horizontal, vertical) = match name {
                    "preview.move-left" => (-count, 0),
                    "preview.move-right" => (count, 0),
                    "preview.move-up" => (0, -count),
                    _ => (0, count),
                };
                if let Some(pane) = self.panes.get_mut(self.active) {
                    pane.scroll(horizontal, vertical);
                }
                true
            }
            _ if self.focused => {
                let Some(pane) = self.panes.get_mut(self.active) else {
                    return false;
                };
                match &mut pane.content {
                    Content::Diff(diff) => diff.command(name, count as usize, pane.viewport.height),
                    Content::Rows {
                        items: Some(selection),
                        rows,
                        ..
                    } => selection.command(name, count as usize, rows, &mut pane.viewport),
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

fn put_rows(frame: &mut Frame, area: PreviewArea, pane: Vec<FrameRow>) {
    for (offset, row) in pane.into_iter().take(area.height).enumerate() {
        let Some(out) = frame.rows.get_mut(area.y + offset) else {
            break;
        };
        let used = spans_width(&out.spans);
        if used < area.x {
            out.spans.push(span(
                format!("{}│", " ".repeat(area.x - used - 1)),
                "window",
            ));
        }
        out.spans.extend(clip_spans(row.spans, area.width));
    }
}

pub(super) fn render(
    frame: &mut Frame,
    page: &mut Page,
    config: Option<&StatusConfig>,
    statuslines: &mut Statuslines,
    subscription: u64,
    focused: bool,
) {
    let item_area = page.item_rows(frame.height);
    let Page {
        position,
        display,
        columns,
        item_count,
        identity,
        options,
        previews,
        modified,
        jump,
        generation,
        ..
    } = page;
    let kind = position.source.kind.as_str();
    let selection = &position.selection;
    let available = PreviewArea {
        x: 0,
        y: 0,
        width: frame.width.into(),
        height: usize::from(frame.height.saturating_sub(1)),
    };
    frame.rows = vec![FrameRow::default(); available.height];
    frame.cursor = None;
    let (main, preview) = if previews.panes.is_empty() {
        (available, None)
    } else {
        let (main, preview) = available.split_preview();
        (main, Some(preview))
    };
    if main.width != 0 && main.height != 0 {
        let inset = usize::from(matches!(kind, "files" | "git" | "git-commit-files"))
            .min(main.width.saturating_sub(1));
        let width = main.width - inset;
        let mut rows = Vec::new();
        let status_name = options
            .get("statusline")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let status = !status_name.is_empty();
        let mut labels: Vec<_> = display
            .tabs
            .iter()
            .map(|tab| {
                if tab.active {
                    format!("[{}]", tab.label)
                } else {
                    tab.label.clone()
                }
            })
            .collect();
        if !display.header.is_empty() {
            labels.push(display.header.clone());
        }
        if item_area.start != 0 {
            rows.push(row(
                fit(&labels.join("  "), width),
                header_style(Some(options)),
            ));
        }
        let viewport = PreviewViewport {
            scroll: position.scroll,
            horizontal_scroll: position.horizontal_scroll,
            width,
            height: item_area.len(),
            ..PreviewViewport::default()
        };
        let selected = if previews.focused {
            &[][..]
        } else {
            &selection.selected
        };
        rows.extend(item_rows(
            &display.rows,
            columns,
            &viewport,
            selected,
            jump.as_ref(),
        ));
        rows.resize_with(main.height - usize::from(status), FrameRow::default);
        if status {
            let status_target = Target::Page(subscription);
            let pending = if identity.is_none() {
                Some(LoadingScope::View(*generation))
            } else if display.loading {
                let git = &display.status["git"];
                Some(if let Some(epoch) = git["write_epoch"].as_u64() {
                    LoadingScope::GitWrite {
                        generation: *generation,
                        epoch,
                    }
                } else if let Some(query) = git["query_id"].as_u64() {
                    LoadingScope::GitQuery {
                        generation: *generation,
                        query,
                    }
                } else {
                    LoadingScope::View(*generation)
                })
            } else {
                None
            };
            let loading = statuslines.show_loading(&status_target, pending);
            let active = focused && !previews.focused;
            let title = if display.title.is_empty() {
                kind
            } else {
                &display.title
            };
            let mut spans = identity.as_ref().map_or_else(Vec::new, |identity| {
                let context = StatusContext {
                    kind: kind.into(),
                    title: title.into(),
                    active,
                    modified: *modified,
                    loading,
                    view_id: identity.view_id,
                    buffer_id: identity.buffer_id,
                    width: main.width,
                    right_inset: inset,
                    mode: Some(position.mode.clone()),
                    options: options.clone(),
                    plugin_status: display.status.clone(),
                    plugin_status_data: display.status_data.clone(),
                    content: StatusContent::Items {
                        selection_count: selection.selected.len(),
                        primary: selection.primary.clone(),
                        item_count: *item_count,
                    },
                };
                statuslines.render(status_target, config, status_name, context, None)
            });
            if spans.is_empty() {
                let style = if active { "status" } else { "status.inactive" };
                let indicator = if loading { "  Loading…" } else { "" };
                spans.push(span(
                    fit(&format!(" {title}{indicator}"), main.width),
                    style,
                ));
            }
            rows.push(FrameRow { spans });
        }
        put_rows(frame, main, rows);
    }
    if let Some(area) = preview {
        let areas = previews.layout.rectangles(area, previews.panes.len());
        let wrap = kind == "files"
            && options
                .get("preview-wrap")
                .and_then(Value::as_bool)
                .unwrap_or(true);
        for (index, (pane, area)) in previews.panes.iter_mut().zip(areas).enumerate() {
            let rows = pane.render(
                area,
                focused && previews.focused && previews.active == index,
                wrap,
                &mut frame.styles,
                config,
                statuslines,
                subscription,
                *generation,
            );
            if focused && previews.focused && previews.active == index && pane.cursor.is_some() {
                frame.cursor = pane.cursor;
            }
            put_rows(frame, area, rows);
        }
    }
}
