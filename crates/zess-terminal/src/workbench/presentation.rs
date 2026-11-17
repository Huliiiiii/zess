use std::time::Instant;

use crate::presentation::{gutter_width, header_style};
use crate::statusline::{LoadingScope, Target};
use anyhow::Result;
use serde_json::Value;
use zess_plugin::{PreviewArea, StatusContent, StatusContext, StatusPosition};
use zess_protocol::{BufferKind, SessionSnapshot, workspace_view::DocumentPane};
use zess_text::TextEffect;
use zess_ui::{
    CursorShape, Frame, FrameRow, MouseEvent, MouseKind, Span, clip_spans, fit, spans_width,
};

use super::{Effect, Workbench};

struct Region {
    document: DocumentPane,
    pane: PreviewArea,
    body: PreviewArea,
    positions: Vec<Vec<usize>>,
    double_click_command: Option<String>,
}

#[derive(Default)]
pub(super) struct DocumentHits {
    regions: Vec<Region>,
    pub(super) drag: Option<DocumentPane>,
}

fn span(text: impl Into<String>, style: &str) -> Span {
    Span {
        text: text.into(),
        style: style.into(),
        base_style: None,
    }
}

fn relative_title<'a>(title: &'a str, workspace: &str) -> &'a str {
    let workspace = workspace.trim_end_matches(['/', '\\']);
    title
        .strip_prefix(workspace)
        .and_then(|suffix| suffix.strip_prefix(['/', '\\']))
        .unwrap_or(title)
}

fn add_rows(frame: &mut Frame, area: PreviewArea, rows: Vec<FrameRow>) {
    for (index, row) in rows.into_iter().enumerate().take(area.height) {
        let Some(out) = frame.rows.get_mut(area.y + index) else {
            break;
        };
        let used = spans_width(&out.spans);
        if area.x > used {
            out.spans.push(span(
                format!("{}│", " ".repeat(area.x - used - 1)),
                "window",
            ));
        }
        out.spans.extend(clip_spans(row.spans, area.width));
    }
}

impl Workbench {
    fn render_text(
        &mut self,
        state: &SessionSnapshot,
        frame: &mut Frame,
        area: PreviewArea,
        document: DocumentPane,
        focused: bool,
    ) {
        let DocumentPane { buffer_id, view_id } = document;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let view = state
            .views
            .iter()
            .find(|v| v.buffer_id == buffer_id && v.view_id == view_id)
            .or_else(|| state.views.iter().find(|v| v.buffer_id == buffer_id));
        let config = self.editor.catalog().map(|catalog| &catalog.status);
        let kind = view.map_or("text", |view| view.kind.as_str());
        let options = view
            .and_then(|view| view.options.as_ref())
            .or_else(|| config.and_then(|config| config.view_options.get(kind)))
            .unwrap_or(&state.text_options);
        let option = |name: &str| options.get(name);
        let status_name = option("statusline")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let status = !status_name.is_empty();
        let status_target = Target::Document(view_id);
        let loading = self.statuslines.show_loading(
            &status_target,
            self.documents
                .is_waiting_for_document(buffer_id)
                .then_some(LoadingScope::View(buffer_id)),
        );
        let header = view.map_or("", |v| v.header.as_str());
        let header_rows = usize::from(!header.is_empty() && area.height > usize::from(status));
        let height = area
            .height
            .saturating_sub(usize::from(status) + header_rows);
        let lines = self.documents.line_count(buffer_id).unwrap_or(1);
        let gutter = gutter_width(Some(options), lines.to_string().len(), area.width);
        let caret = focused && !self.editor.prompt_active() && !self.editor.help_focused();
        let mut rows = Vec::with_capacity(area.height);
        if header_rows > 0 {
            rows.push(FrameRow {
                spans: vec![span(fit(header, area.width), header_style(Some(options)))],
            });
        }
        if let Some(mut render) =
            self.documents
                .render(buffer_id, view_id, area.width - gutter, height, caret)
        {
            let position = self
                .documents
                .position(buffer_id, view_id)
                .unwrap_or_default();
            for (i, mut row) in render.rows.into_iter().enumerate().take(height) {
                if gutter > 0 {
                    let line = render.scroll + i + 1;
                    row.spans.insert(
                        0,
                        span(
                            fit(&format!("{line:>digits$} ", digits = gutter - 1), gutter),
                            if focused && position.0 + 1 == line {
                                "linenr.current"
                            } else {
                                "linenr"
                            },
                        ),
                    );
                }
                rows.push(row);
            }
            self.hits.regions.push(Region {
                document,
                pane: area,
                body: PreviewArea {
                    x: area.x + gutter,
                    y: area.y + header_rows,
                    width: area.width - gutter,
                    height,
                },
                positions: std::mem::take(&mut render.hit_positions),
                double_click_command: option("double-click-command")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
            if caret {
                frame.cursor = render.cursor.map(|(x, y)| {
                    (
                        (usize::from(x) + area.x + gutter) as u16,
                        (usize::from(y) + area.y + header_rows) as u16,
                    )
                });
            }
        } else if loading && !status && height > 0 {
            rows.push(FrameRow {
                spans: vec![span("Loading…", "comment")],
            });
        }
        rows.resize_with(area.height - usize::from(status), FrameRow::default);
        let mode = self.documents.mode(buffer_id, view_id).unwrap_or("normal");
        if status {
            let title = view
                .map(|view| view.title.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    state
                        .buffers
                        .iter()
                        .find(|s| s.id == buffer_id)
                        .and_then(|s| match &s.kind {
                            BufferKind::Text { path } => path.as_deref(),
                            _ => None,
                        })
                })
                .map(|title| relative_title(title, &state.workspace_root))
                .unwrap_or("[scratch]");
            let content = self.documents.position(buffer_id, view_id).map_or(
                StatusContent::None,
                |(row, column)| StatusContent::Text {
                    position: StatusPosition {
                        row: row + 1,
                        column: column + 1,
                    },
                    selection_count: self
                        .documents
                        .selection_count(buffer_id, view_id)
                        .unwrap_or(1),
                    encoding: "UTF-8".into(),
                },
            );
            let context = StatusContext {
                kind: kind.to_owned(),
                title: title.to_owned(),
                view_id,
                buffer_id,
                active: focused && !self.editor.help_focused(),
                modified: self.documents.modified(buffer_id).unwrap_or(false),
                loading,
                width: area.width,
                mode: Some(mode.into()),
                options: options.clone(),
                content,
                ..Default::default()
            };
            let mut spans = self.statuslines.render(
                status_target,
                config,
                status_name,
                context,
                view.and_then(|view| view.statusline.as_ref()),
            );
            if spans.is_empty() && loading {
                spans.push(span("Loading…", "status"));
            }
            rows.push(FrameRow { spans });
        }
        if focused {
            frame.cursor_shape = match mode {
                "insert" => CursorShape::Bar,
                "select" => CursorShape::Underline,
                _ => CursorShape::Block,
            };
            if let Some(text) = self.documents.text(buffer_id)
                && self.documents.mode(buffer_id, view_id).is_some()
            {
                frame.prompt = text.pending_input(view_id);
            }
        }
        add_rows(frame, area, rows);
    }

    pub(crate) fn render(
        &mut self,
        state: &SessionSnapshot,
        width: u16,
        height: u16,
    ) -> Result<Frame> {
        self.width = width;
        self.height = height;
        let source_height = self.editor.source_height(height);
        self.resize_pages(width, source_height);
        self.hits.regions.clear();
        let mut frame = Frame {
            width,
            height: source_height,
            styles: state.styles.clone(),
            mouse_enabled: state.editor_options.mouse,
            status: state.status.clone(),
            ..Frame::default()
        };
        self.statuslines.begin_frame(Instant::now());
        let rectangles = self.pane_rectangles(width, source_height);
        let active = self.active_pane();
        if rectangles.is_empty() {
            let config = self.editor.catalog().map(|catalog| &catalog.status);
            if !self
                .pages
                .render(&mut frame, config, &mut self.statuslines, true)
            {
                frame = super::host::render(state, width, source_height)?;
            }
        } else if rectangles.len() > 1
            || self.active_document(state).is_some()
            || self.pages.subscription().is_some()
        {
            frame.rows.resize_with(
                usize::from(source_height.saturating_sub(1)),
                FrameRow::default,
            );
            for (pane, area) in rectangles {
                let focused = active == Some(pane);
                let pages = if focused {
                    Some(&mut self.pages)
                } else {
                    self.other_pages.get_mut(&pane.view_id)
                };
                if let Some(pages) = pages.filter(|pages| pages.subscription().is_some()) {
                    let mut pane_frame = Frame {
                        width: area.width as u16,
                        height: area.height as u16 + 1,
                        styles: std::mem::take(&mut frame.styles),
                        ..Frame::default()
                    };
                    let config = self.editor.catalog().map(|catalog| &catalog.status);
                    pages.render(&mut pane_frame, config, &mut self.statuslines, focused);
                    frame.styles = pane_frame.styles;
                    if focused {
                        frame.cursor = pane_frame
                            .cursor
                            .map(|(x, y)| (x + area.x as u16, y + area.y as u16));
                        frame.cursor_shape = pane_frame.cursor_shape;
                        frame.prompt = pane_frame.prompt;
                    }
                    add_rows(&mut frame, area, pane_frame.rows);
                } else {
                    self.render_text(state, &mut frame, area, pane, focused);
                }
            }
        } else {
            frame = super::host::render(state, width, source_height)?;
        }
        self.statuslines.finish_frame();
        self.render_layer(&mut frame);
        if let Some(notice) = self.documents.closed_notice() {
            frame.status = notice;
        } else if self
            .active_document(state)
            .is_some_and(|pane| self.documents.is_reloading(pane.buffer_id))
        {
            frame.status = "Reload pending. Editing and saving are paused.".into();
        }
        frame.height = height;
        frame
            .rows
            .resize_with(usize::from(height.saturating_sub(1)), FrameRow::default);
        if !self.editor.prompt_active() && !self.editor.help_focused() {
            let count = self.command_count(state);
            if !count.is_empty() {
                frame.prompt = if frame.prompt.is_empty() {
                    count.to_string()
                } else {
                    format!("{count} · {}", frame.prompt)
                };
            }
        }
        let (scopes, mode) = self.key_context(state);
        self.editor.render(
            &mut frame,
            &scopes.iter().map(String::as_str).collect::<Vec<_>>(),
            &mode,
        );
        Ok(frame)
    }

    pub(super) fn mouse_text(
        &mut self,
        event: &MouseEvent,
        state: &SessionSnapshot,
        effects: &mut Vec<Effect>,
    ) -> Result<bool> {
        if !state.editor_options.mouse {
            return Ok(true);
        }
        if event.kind == MouseKind::Release {
            return Ok(self.hits.drag.take().is_some());
        }
        let (x, y) = (usize::from(event.column), usize::from(event.row));
        let Some(hit) = self.hits.regions.iter().find(|r| r.pane.contains(x, y)) else {
            return Ok(false);
        };
        let doc = hit.document;
        match event.kind {
            MouseKind::ScrollUp | MouseKind::ScrollDown => {
                self.hits.drag = None;
                let count = state.editor_options.scroll_lines.min(isize::MAX as usize) as isize;
                self.documents.edit(doc.buffer_id, doc.view_id, |text| {
                    text.scroll_view(
                        doc.view_id,
                        0,
                        if event.kind == MouseKind::ScrollUp {
                            -count
                        } else {
                            count
                        },
                    );
                    Ok(TextEffect::default())
                })?;
            }
            MouseKind::Press | MouseKind::DoubleClick | MouseKind::Drag => {
                let extend = event.kind == MouseKind::Drag;
                if extend && self.hits.drag != Some(doc) {
                    return Ok(true);
                }
                let position = hit
                    .body
                    .contains(x, y)
                    .then(|| {
                        hit.positions
                            .get(y - hit.body.y)
                            .and_then(|row| row.get(x - hit.body.x))
                            .copied()
                    })
                    .flatten();
                let command = if event.kind == MouseKind::DoubleClick && position.is_some() {
                    hit.double_click_command.clone()
                } else {
                    None
                };
                if !extend {
                    self.hits.drag = None;
                    self.focus_pane(doc.view_id);
                }
                if let Some(position) = position
                    && !self.documents.is_reloading(doc.buffer_id)
                {
                    self.documents.edit(doc.buffer_id, doc.view_id, |text| {
                        text.select_at(doc.view_id, position, extend);
                        Ok(TextEffect::default())
                    })?;
                    if let Some(command) = command {
                        self.command(command, serde_json::json!({}), state, effects)?;
                    } else {
                        self.hits.drag = Some(doc);
                    }
                }
            }
            MouseKind::Release => {}
        }
        Ok(true)
    }
}
