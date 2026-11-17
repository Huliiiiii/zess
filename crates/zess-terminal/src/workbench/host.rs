use anyhow::{Context, Result, bail, ensure};
use zess_protocol::{
    SemanticRow, SessionSnapshot, SurfaceSnapshot, ViewContent, ViewSelection, ViewSnapshot,
};
use zess_text::PreviewViewport;
use zess_ui::{
    ColumnAlignment, CursorShape, DisplayCell, DisplayRow, Frame, FrameRow, RowColumns, Span,
    clip_spans, fit, skip_columns, spans_width,
};

use crate::pages::item_rows;
use crate::presentation::{gutter_width, header_style};

#[cfg(test)]
mod tests;
mod text;

use text::TextViewport;

fn span(text: String, style: &str) -> Span {
    Span {
        text,
        style: style.to_owned(),
        base_style: None,
    }
}

fn row(text: String, style: &str) -> FrameRow {
    FrameRow {
        spans: vec![span(text, style)],
    }
}

fn selected_ids(selection: &ViewSelection, focused: bool) -> &[String] {
    match selection {
        ViewSelection::Items { ids, .. } if focused => ids,
        _ => &[],
    }
}

fn items(
    rows: &[SemanticRow],
    alignment: ColumnAlignment,
    viewport: &PreviewViewport,
    selected: &[String],
) -> Vec<FrameRow> {
    let display: Vec<_> = rows
        .iter()
        .map(|row| DisplayRow {
            id: row.id.clone(),
            text: row.text.clone(),
            style: row.style.clone(),
            depth: row.depth,
            cells: row
                .cells
                .iter()
                .map(|spans| DisplayCell {
                    spans: spans.clone(),
                })
                .collect(),
        })
        .collect();
    let columns = RowColumns::measure(&display, alignment);
    item_rows(&display, &columns, viewport, selected, None)
}

fn diff_rows(
    rows: &[SemanticRow],
    view: &ViewSnapshot,
    viewport: &PreviewViewport,
    selected: &[String],
) -> Vec<FrameRow> {
    let rendered = rows.iter().any(|row| !row.cells.is_empty());
    let gutter = if rendered {
        0
    } else {
        let digits = rows
            .iter()
            .filter_map(|row| row.line_number)
            .max()
            .unwrap_or_default()
            .to_string()
            .len();
        gutter_width(view.options.as_ref(), digits, viewport.width)
    };
    rows.iter()
        .skip(viewport.scroll)
        .take(viewport.height)
        .map(|source| {
            let selected = selected.contains(&source.id);
            let mut spans = source
                .cells
                .first()
                .cloned()
                .unwrap_or_else(|| vec![span(source.text.clone(), &source.style)]);
            if selected {
                for span in &mut spans {
                    if rendered {
                        span.base_style = Some(std::mem::take(&mut span.style));
                        span.style = "diff.selection".into();
                    } else {
                        span.style = format!("selection.{}", span.style);
                    }
                }
            }
            skip_columns(&mut spans, viewport.horizontal_scroll);
            let used = spans_width(&spans);
            let body_width = viewport.width - gutter;
            if used < body_width {
                let style = if selected && rendered {
                    "diff.selection"
                } else if selected {
                    "selection"
                } else {
                    &source.style
                };
                spans.push(span(" ".repeat(body_width - used), style));
            }
            let mut spans = clip_spans(spans, body_width);
            if gutter > 0 {
                let number = source
                    .line_number
                    .map(|number| number.to_string())
                    .unwrap_or_default();
                spans.insert(
                    0,
                    span(
                        fit(&format!("{number:>digits$} ", digits = gutter - 1), gutter),
                        if selected { "linenr.current" } else { "linenr" },
                    ),
                );
            }
            FrameRow { spans }
        })
        .collect()
}

type PaneRows = (Vec<FrameRow>, Option<(u16, u16)>);

fn render_view(
    view: &ViewSnapshot,
    width: usize,
    height: usize,
    focused: bool,
) -> Result<PaneRows> {
    if height == 0 || width == 0 {
        return Ok((Vec::new(), None));
    }
    let inset = usize::from(matches!(
        view.kind.as_str(),
        "files" | "git" | "git-commit-files"
    ))
    .min(width - 1);
    let width = width - inset;
    let options = view.options.as_ref();
    let option = |name: &str| {
        options
            .and_then(|options| options.get(name))
            .and_then(|v| v.as_str())
    };
    let status = option("statusline") != Some("");
    let body_end = height - usize::from(status);
    let mut rows = Vec::new();
    let mut header = view
        .tabs
        .iter()
        .map(|tab| {
            if tab.active {
                format!("[{}]", tab.label)
            } else {
                tab.label.clone()
            }
        })
        .collect::<Vec<_>>();
    if !view.header.is_empty() {
        header.push(view.header.clone())
    }
    if !header.is_empty() && body_end > 0 {
        rows.push(row(fit(&header.join("  "), width), header_style(options)));
    }
    let header_height = rows.len();
    let height = body_end - header_height;
    let (body, cursor) = match &view.content {
        ViewContent::Text { lines } => {
            let metrics = view
                .text_metrics
                .as_ref()
                .context("text view has no document metrics")?;
            ensure!(metrics.tab_width > 0, "text view has zero tab width");
            let gutter = gutter_width(options, metrics.line_count.to_string().len(), width);
            text::render(
                lines,
                TextViewport {
                    width,
                    height,
                    gutter,
                    tab_width: metrics.tab_width,
                    horizontal_scroll: view.horizontal_scroll,
                    soft_wrap: false,
                    selection: Some(&view.selection),
                    mode: &view.mode,
                    focused,
                    readonly: view.kind != "text",
                },
            )
        }
        ViewContent::Items { rows } => {
            let viewport = PreviewViewport {
                width,
                height,
                scroll: view.scroll,
                horizontal_scroll: view.horizontal_scroll,
                ..Default::default()
            };
            let selected = selected_ids(&view.selection, focused);
            let rows = if view.kind == "diff" {
                diff_rows(rows, view, &viewport, selected)
            } else {
                items(rows, view.column_alignment, &viewport, selected)
            };
            (rows, None)
        }
    };
    rows.extend(body);
    rows.resize_with(body_end, FrameRow::default);
    if status {
        let spans = view
            .statusline
            .as_ref()
            .map(|statusline| statusline.render(width + inset, inset, focused))
            .unwrap_or_default();
        rows.push(FrameRow { spans });
    }
    Ok((rows, cursor.map(|(x, y)| (x, y + header_height as u16))))
}

fn render_surface(surface: &SurfaceSnapshot, width: usize, height: usize) -> Result<Vec<FrameRow>> {
    let mut rows = Vec::new();
    let header = if surface.loading {
        format!("{}  Loading…", surface.header)
    } else {
        surface.header.clone()
    };
    if !header.is_empty() && height > 0 {
        rows.push(row(fit(&header, width), "ui.heading"));
    }
    let height = height - rows.len();
    match &surface.content {
        ViewContent::Items { rows: source } => {
            rows.extend(items(
                source,
                surface.column_alignment,
                &PreviewViewport {
                    width,
                    height,
                    scroll: surface.scroll,
                    horizontal_scroll: surface.horizontal_scroll,
                    ..Default::default()
                },
                &[],
            ));
        }
        ViewContent::Text { lines } => {
            let metrics = surface
                .text_metrics
                .as_ref()
                .context("text surface has no document metrics")?;
            ensure!(metrics.tab_width > 0, "text surface has zero tab width");
            let (body, _) = text::render(
                lines,
                TextViewport {
                    width,
                    height,
                    gutter: 0,
                    tab_width: metrics.tab_width,
                    horizontal_scroll: surface.horizontal_scroll,
                    soft_wrap: surface.soft_wrap,
                    selection: None,
                    mode: "normal",
                    focused: false,
                    readonly: true,
                },
            );
            rows.extend(body);
        }
    }
    Ok(rows)
}

fn paint(row: &mut FrameRow, pane: FrameRow, x: usize, width: usize) {
    if x == 0 && row.spans.is_empty() && pane.spans.is_empty() {
        return;
    }
    let mut original = std::mem::take(&mut row.spans);
    let used = spans_width(&original);
    let mut left = if x >= used {
        if x > used {
            original.push(span(format!("{}│", " ".repeat(x - used - 1)), "window"));
        }
        std::mem::take(&mut original)
    } else if x == 0 {
        Vec::new()
    } else {
        clip_spans(original.clone(), x)
    };
    left.extend(clip_spans(pane.spans, width));
    if used > x + width {
        skip_columns(&mut original, x + width);
        left.extend(original.into_iter().filter(|span| !span.text.is_empty()));
    }
    row.spans = left;
}

pub(crate) fn render(state: &SessionSnapshot, width: u16, height: u16) -> Result<Frame> {
    let body_height = usize::from(height.saturating_sub(1));
    let mut frame = Frame {
        width,
        height,
        rows: vec![FrameRow::default(); body_height],
        styles: state.styles.clone(),
        mouse_enabled: state.editor_options.mouse,
        status: state.status.clone(),
        ..Frame::default()
    };
    if let Some(id) = state.focused_view {
        ensure!(
            state.views.iter().any(|view| view.view_id == id),
            "focused view {id} is unavailable"
        );
    }
    let total_width = state
        .layout
        .iter()
        .map(|area| area.x.saturating_add(area.width))
        .max()
        .unwrap_or(0)
        .max(1);
    let total_height = state
        .layout
        .iter()
        .map(|area| area.y.saturating_add(area.height))
        .max()
        .unwrap_or(0)
        .max(1);
    for area in &state.layout {
        let scale_x = |x: usize| (x as u128 * u128::from(width) / total_width as u128) as usize;
        let scale_y = |y: usize| (y as u128 * body_height as u128 / total_height as u128) as usize;
        let x = scale_x(area.x);
        let y = scale_y(area.y);
        let w = scale_x(area.x.saturating_add(area.width)) - x;
        let h = scale_y(area.y.saturating_add(area.height)) - y;
        let (mut rows, cursor) = match (area.view_id, area.page_id, area.slot_id.as_deref()) {
            (Some(view_id), None, None) => {
                let view = state
                    .views
                    .iter()
                    .find(|view| view.view_id == view_id && view.buffer_id == area.buffer_id)
                    .with_context(|| {
                        format!(
                            "layout refers to missing view {view_id}, buffer {}",
                            area.buffer_id
                        )
                    })?;
                let focused = Some(view_id) == state.focused_view;
                if focused {
                    frame.cursor_shape = CursorShape::for_mode(&view.mode);
                }
                render_view(view, w, h, focused)?
            }
            (None, Some(page), Some(slot)) => {
                let surface = state
                    .surfaces
                    .iter()
                    .find(|s| {
                        s.page_id == page && s.slot_id == slot && s.buffer_id == area.buffer_id
                    })
                    .with_context(|| {
                        format!("layout refers to missing surface {slot} on page {page}")
                    })?;
                (render_surface(surface, w, h)?, None)
            }
            _ => bail!("layout area has no unique view or surface"),
        };
        rows.resize_with(h, FrameRow::default);
        for (output, pane) in frame.rows[y..y + h].iter_mut().zip(rows) {
            paint(output, pane, x, w);
        }
        if let Some((column, line)) = cursor {
            frame.cursor = Some((x as u16 + column, y as u16 + line));
        }
    }
    if let Some(prompt) = &state.prompt {
        ensure!(
            state
                .views
                .iter()
                .any(|view| view.view_id == prompt.owner_view
                    && view.buffer_id == prompt.owner_buffer),
            "prompt owner is unavailable"
        );
        ensure!(
            prompt.text.is_char_boundary(prompt.cursor),
            "prompt cursor is outside its input"
        );
        crate::editor::render_prompt(
            &mut frame,
            &prompt.label,
            &prompt.text,
            prompt.cursor,
            prompt.error.as_deref(),
        );
    }
    Ok(frame)
}
