use super::*;
use zess_protocol::{SemanticRow, SessionSnapshot, ViewContent};
use zess_ui::{ContentSource, RenderContent, RenderPlan, RenderRow, RenderSpan};

struct BoundGlyph {
    text: String,
    source: RenderContent,
}

type Bindings = HashMap<(usize, usize), BoundGlyph>;

impl Session {
    pub(super) fn presentation_plan(&self, frame: &Frame, state: &SessionSnapshot) -> RenderPlan {
        let mut bindings = Bindings::new();
        let mut text_sources = HashMap::new();
        for (buffer, content) in state
            .views
            .iter()
            .map(|view| (view.buffer_id, &view.content))
            .chain(
                state
                    .surfaces
                    .iter()
                    .map(|surface| (surface.buffer_id, &surface.content)),
            )
        {
            if let ViewContent::Text { lines } = content {
                for line in lines {
                    let mut position = line.first_char;
                    for glyph in line.text.graphemes(true) {
                        let end = position + glyph.chars().count();
                        text_sources.insert((buffer, position), (end, glyph));
                        position = end;
                    }
                }
            }
        }
        for region in self.mouse_regions.iter().chain(&self.presentation_regions) {
            bindings.retain(|&(row, column), _| !contains(region.pane, row, column));
            let mut region_bindings = Bindings::new();
            let source = match &region.target {
                RenderTarget::View(view_id) => ContentSource::View { view_id: *view_id },
                RenderTarget::Surface { page, slot } => ContentSource::Surface {
                    page_id: self.views.main(*page),
                    slot_id: slot.clone(),
                },
            };
            let content = match &source {
                ContentSource::View { view_id } => state
                    .views
                    .iter()
                    .find(|view| view.view_id == *view_id)
                    .map(|view| &view.content),
                ContentSource::Surface { page_id, slot_id } => state
                    .surfaces
                    .iter()
                    .find(|surface| surface.page_id == *page_id && surface.slot_id == *slot_id)
                    .map(|surface| &surface.content),
            };
            match &region.content {
                mouse::Content::Text { positions, .. } => {
                    bind_text(
                        &mut region_bindings,
                        &text_sources,
                        region.buffer,
                        region.body,
                        positions,
                    );
                }
                mouse::Content::Items { ids } => {
                    let RenderTarget::View(view_id) = region.target else {
                        continue;
                    };
                    let view = &self.views[&view_id];
                    let Some(ViewContent::Items { rows }) = content else {
                        continue;
                    };
                    for (offset, row_id) in ids.iter().enumerate() {
                        if row_id.is_empty() {
                            continue;
                        }
                        let Some(row) = rows.iter().find(|row| row.id == *row_id) else {
                            continue;
                        };
                        let Some(index) = view
                            .location
                            .display
                            .rows
                            .iter()
                            .position(|row| row.id == *row_id)
                        else {
                            continue;
                        };
                        let layout = view.location.columns.layout(index, region.body.width);
                        bind_item(
                            &mut region_bindings,
                            &source,
                            row,
                            region.body.x,
                            region.body.y + offset,
                            view.location.horizontal_scroll,
                            &layout,
                        );
                    }
                }
                mouse::Content::Diff { scroll, .. } => {
                    let Some(ViewContent::Items { rows }) = content else {
                        continue;
                    };
                    for (offset, row) in rows.iter().enumerate() {
                        if row.id != (scroll + offset).to_string() {
                            continue;
                        }
                        bind_item_part(
                            &mut region_bindings,
                            &source,
                            row,
                            None,
                            &row.text,
                            region.body.x as isize,
                            region.body.y + offset,
                        );
                    }
                }
                mouse::Content::Rows => {
                    if let (RenderTarget::Surface { page, slot }, Some(ViewContent::Text { .. })) =
                        (&region.target, content)
                    {
                        if let Some(surface) = self.views.surface(*page, slot) {
                            bind_text(
                                &mut region_bindings,
                                &text_sources,
                                region.buffer,
                                region.body,
                                &surface.viewport.hit_positions,
                            );
                        }
                    }
                    if let (
                        RenderTarget::Surface { page, slot },
                        Some(ViewContent::Items { rows }),
                    ) = (&region.target, content)
                    {
                        let Some(surface) = self.views.surface(*page, slot) else {
                            continue;
                        };
                        let display = match &self.buffers[&region.buffer] {
                            Buffer::PreviewRows { display, .. } => display,
                            Buffer::PreviewDiff { .. } => {
                                for (offset, row) in rows.iter().enumerate() {
                                    bind_item_part(
                                        &mut region_bindings,
                                        &source,
                                        row,
                                        None,
                                        &row.text,
                                        region.body.x as isize,
                                        region.body.y + offset,
                                    );
                                }
                                bindings.extend(region_bindings.into_iter().filter(
                                    |((row, column), _)| contains(region.body, *row, *column),
                                ));
                                continue;
                            }
                            _ => continue,
                        };
                        let columns =
                            zess_ui::RowColumns::measure(&display.rows, display.column_alignment);
                        for (offset, row) in rows.iter().enumerate() {
                            let layout =
                                columns.layout(surface.viewport.scroll + offset, region.body.width);
                            bind_item(
                                &mut region_bindings,
                                &source,
                                row,
                                region.body.x,
                                region.body.y + offset,
                                surface.viewport.horizontal_scroll,
                                &layout,
                            );
                        }
                    }
                }
            }
            bindings.extend(
                region_bindings
                    .into_iter()
                    .filter(|((row, column), _)| contains(region.body, *row, *column)),
            );
        }
        let rows = frame
            .rows
            .iter()
            .enumerate()
            .map(|(row, rendered)| {
                let mut column = 0;
                let mut spans = Vec::new();
                for span in &rendered.spans {
                    for glyph in span.text.graphemes(true) {
                        let content = bindings
                            .get(&(row, column))
                            .filter(|bound| bound.text == glyph)
                            .map(|bound| bound.source.clone())
                            .unwrap_or_else(|| RenderContent::Literal {
                                text: glyph.to_owned(),
                            });
                        push_span(&mut spans, &span.base_style, &span.style, content);
                        column += UnicodeWidthStr::width(glyph);
                    }
                }
                RenderRow { spans }
            })
            .collect();
        RenderPlan {
            width: frame.width,
            height: frame.height,
            styles: frame.styles.clone(),
            mouse_context: frame.mouse_context,
            mouse_enabled: frame.mouse_enabled,
            rows,
            cursor: frame.cursor,
            cursor_shape: frame.cursor_shape.clone(),
            status: frame.status.clone(),
            prompt: frame.prompt.clone(),
        }
    }
}

fn bind_text(
    bindings: &mut Bindings,
    sources: &HashMap<(u64, usize), (usize, &str)>,
    buffer_id: u64,
    body: Rect,
    positions: &[Vec<usize>],
) {
    for (row, positions) in positions.iter().enumerate().take(body.height) {
        for (column, position) in positions.iter().enumerate().take(body.width) {
            if let Some((end, text)) = sources.get(&(buffer_id, *position)) {
                bindings.insert(
                    (body.y + row, body.x + column),
                    BoundGlyph {
                        text: (*text).to_owned(),
                        source: RenderContent::Text {
                            buffer_id,
                            start_char: *position,
                            end_char: *end,
                        },
                    },
                );
            }
        }
    }
}

fn bind_item(
    bindings: &mut Bindings,
    source: &ContentSource,
    row: &SemanticRow,
    x: usize,
    y: usize,
    horizontal_scroll: usize,
    layout: &zess_ui::RowLayout<'_>,
) {
    bind_item_part(
        bindings,
        source,
        row,
        None,
        &row.text,
        x as isize + (row.depth.min(30) * 2) as isize - horizontal_scroll as isize,
        y,
    );
    let mut column = x + layout.text_width();
    for (cell, width) in layout.cell_widths().iter().enumerate() {
        let Some(spans) = row.cells.get(cell) else {
            column += layout.gap() + width;
            continue;
        };
        let text = spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        let used = UnicodeWidthStr::width(text.as_str());
        column += layout.gap() + width.saturating_sub(used);
        bind_item_part(bindings, source, row, Some(cell), &text, column as isize, y);
        column += used;
    }
}

fn bind_item_part(
    bindings: &mut Bindings,
    source: &ContentSource,
    row: &SemanticRow,
    cell: Option<usize>,
    text: &str,
    mut column: isize,
    y: usize,
) {
    let mut position = 0;
    for glyph in text.graphemes(true) {
        let end = position + glyph.chars().count();
        if column >= 0 {
            bindings.insert(
                (y, column as usize),
                BoundGlyph {
                    text: glyph.to_owned(),
                    source: RenderContent::Item {
                        source: source.clone(),
                        row_id: row.id.clone(),
                        cell,
                        start_char: position,
                        end_char: end,
                    },
                },
            );
        }
        column += UnicodeWidthStr::width(glyph) as isize;
        position = end;
    }
}

fn push_span(
    spans: &mut Vec<RenderSpan>,
    base_style: &Option<String>,
    style: &str,
    content: RenderContent,
) {
    if let Some(previous) = spans
        .last_mut()
        .filter(|span| span.base_style == *base_style && span.style == style)
    {
        match (&mut previous.content, &content) {
            (RenderContent::Literal { text: previous }, RenderContent::Literal { text }) => {
                previous.push_str(text);
                return;
            }
            (
                RenderContent::Text {
                    buffer_id: previous_buffer,
                    end_char: previous_end,
                    ..
                },
                RenderContent::Text {
                    buffer_id,
                    start_char,
                    end_char,
                },
            ) if previous_buffer == buffer_id && *previous_end == *start_char => {
                *previous_end = *end_char;
                return;
            }
            (
                RenderContent::Item {
                    source: previous_source,
                    row_id: previous_row,
                    cell: previous_cell,
                    end_char: previous_end,
                    ..
                },
                RenderContent::Item {
                    source,
                    row_id,
                    cell,
                    start_char,
                    end_char,
                },
            ) if previous_source == source
                && previous_row == row_id
                && previous_cell == cell
                && *previous_end == *start_char =>
            {
                *previous_end = *end_char;
                return;
            }
            _ => {}
        }
    }
    spans.push(RenderSpan {
        base_style: base_style.clone(),
        style: style.to_owned(),
        content,
    });
}

fn contains(area: Rect, row: usize, column: usize) -> bool {
    row >= area.y && row < area.y + area.height && column >= area.x && column < area.x + area.width
}
