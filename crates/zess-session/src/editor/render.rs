use super::*;

#[cfg(test)]
mod tests;

impl Session {
    pub(super) fn render_surface(&mut self, page: PageId, slot: &str, rect: Rect) -> Vec<FrameRow> {
        let rows = self.render_surface_content(page, slot, rect);
        if rect.width == 0 || rect.height == 0 {
            return rows;
        }
        let Some(surface) = self.views.surface(page, slot) else {
            return rows;
        };
        let buffer = surface.buffer;
        let content_height = surface.viewport.height;
        let header_height = rect.height.saturating_sub(content_height);
        self.mouse_regions.push(mouse::Region {
            target: RenderTarget::Surface {
                page,
                slot: slot.into(),
            },
            buffer,
            pane: rect,
            body: Rect {
                y: rect.y + header_height,
                height: content_height,
                ..rect
            },
            content: mouse::Content::Rows,
        });
        rows
    }

    pub(super) fn render_surface_content(
        &mut self,
        page: PageId,
        slot: &str,
        rect: Rect,
    ) -> Vec<FrameRow> {
        if rect.width == 0 || rect.height == 0 {
            return Vec::new();
        }
        let source = self.views.main(page);
        let soft_wrap = matches!(
            &self.buffers[&self.views[&source].location.buffer],
            Buffer::Data { kind, .. } if kind == "files"
        ) && self
            .effective_view_options(source, "files")
            .get("preview-wrap")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let Some(surface) = self.views.surface_mut(page, slot) else {
            return Vec::new();
        };
        let buffer = surface.buffer;
        let header = if surface.loading {
            format!("{}  Loading…", surface.header)
        } else {
            surface.header.clone()
        };
        let header_height = usize::from(!header.is_empty());
        let content_height = rect.height.saturating_sub(header_height);
        surface.viewport.width = rect.width;
        surface.viewport.height = content_height;
        let mut rows = Vec::new();
        if header_height > 0 {
            rows.push(FrameRow {
                spans: vec![Span {
                    base_style: None,
                    text: fit(&header, rect.width),
                    style: "ui.heading".into(),
                }],
            });
        }
        match self.buffers.get_mut(&buffer).unwrap() {
            Buffer::PreviewText { text, .. } => {
                rows.extend(text.render_preview(
                    &mut surface.viewport,
                    rect.width,
                    content_height,
                    soft_wrap,
                ));
            }
            Buffer::PreviewRows { display, .. } => {
                let viewport = &mut surface.viewport;
                viewport.scroll = viewport
                    .scroll
                    .min(display.rows.len().saturating_sub(content_height.max(1)));
                let columns = zess_ui::RowColumns::measure(&display.rows, display.column_alignment);
                viewport.horizontal_scroll = viewport
                    .horizontal_scroll
                    .min(columns.horizontal_scroll_limit(rect.width));
                rows.extend(preview_rows(
                    display,
                    &columns,
                    viewport.scroll,
                    viewport.horizontal_scroll,
                    rect.width,
                    content_height,
                ));
            }
            Buffer::PreviewDiff { diff, .. } => {
                let viewport = &mut surface.viewport;
                viewport.scroll = viewport
                    .scroll
                    .min(diff.index.rows.len().saturating_sub(content_height.max(1)));
                viewport.horizontal_scroll = viewport
                    .horizontal_scroll
                    .min(diff.width.saturating_sub(rect.width));
                for row in
                    viewport.scroll..(viewport.scroll + content_height).min(diff.index.rows.len())
                {
                    let style = match diff.index.rows[row].kind {
                        Some(zess_git::patch::LineKind::Addition) => "git.added",
                        Some(zess_git::patch::LineKind::Deletion) => "git.deleted",
                        _ if diff.index.rows[row].hunk.is_some() => "keyword",
                        _ => "",
                    };
                    let mut spans = vec![Span {
                        base_style: None,
                        text: diff.display_line(row),
                        style: style.into(),
                    }];
                    skip_columns(&mut spans, viewport.horizontal_scroll);
                    rows.push(FrameRow {
                        spans: clip_spans(spans, rect.width),
                    });
                }
            }
            _ => unreachable!("surface content must be a preview buffer"),
        };
        rows.resize_with(rect.height, FrameRow::default);
        rows
    }

    pub(super) fn render_view(
        &mut self,
        id: ViewId,
        width: usize,
        height: usize,
    ) -> (Vec<FrameRow>, Option<(u16, u16)>) {
        self.render_view_at(id, width, height, (0, 0))
    }

    pub(super) fn render_view_at(
        &mut self,
        id: ViewId,
        width: usize,
        height: usize,
        origin: (usize, usize),
    ) -> (Vec<FrameRow>, Option<(u16, u16)>) {
        if width == 0 || height == 0 {
            return (Vec::new(), None);
        }
        let buffer = self.views[&id].location.buffer;
        let layer = self.views[&id].layer;
        let active = self.focus == id;
        let caret = active
            && self.active_prompt().is_none()
            && self.help.is_none()
            && self.pending.is_empty();
        let (kind, title, modified) = match &self.buffers[&buffer] {
            Buffer::Text(text) => (
                "text".to_owned(),
                text.path()
                    .map(|path| {
                        path.strip_prefix(&self.workspace)
                            .unwrap_or(path)
                            .display()
                            .to_string()
                    })
                    .unwrap_or_else(|| match &self.views[&id].layer_content {
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
                if self.views[&id].location.display.title.is_empty() {
                    kind.clone()
                } else {
                    self.views[&id].location.display.title.clone()
                },
                *modified,
            ),
            Buffer::Items { kind, path, .. } => (
                kind.clone(),
                if self.views[&id].location.display.title.is_empty() {
                    path.clone()
                } else {
                    self.views[&id].location.display.title.clone()
                },
                false,
            ),
            Buffer::PreviewRows { kind, path, .. } => (
                kind.scope().into(),
                if self.views[&id].location.display.title.is_empty() {
                    path.clone()
                } else {
                    self.views[&id].location.display.title.clone()
                },
                false,
            ),
        };
        let options = self.effective_view_options(id, &kind);
        let pane_width = width;
        let right_inset = usize::from(matches!(
            kind.as_str(),
            "files" | "git" | "git-commit-files"
        ))
        .min(width.saturating_sub(1));
        let width = width - right_inset;
        let status_name = options
            .get("statusline")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let pane_height = height.saturating_sub(usize::from(!status_name.is_empty()));
        let view = &self.views[&id];
        let display = &view.location.display;
        let shortcuts = if display.tabs.is_empty() || self.pending.is_empty() {
            Vec::new()
        } else {
            self.scripts
                .as_ref()
                .map(|scripts| {
                    scripts.with_keymap(|keymap| {
                        let bindings = keymap.effective_bindings(
                            &["global", "data", &kind],
                            view.location.mode.as_str(),
                        );
                        display
                            .tabs
                            .iter()
                            .map(|tab| {
                                bindings
                                    .iter()
                                    .find(|binding| {
                                        binding.command == tab.command
                                            && binding.keys.len() == self.pending.len() + 1
                                            && binding.keys.starts_with(&self.pending)
                                    })
                                    .map(|binding| {
                                        format!("{} {}", binding.keys.last().unwrap(), tab.label)
                                    })
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_default()
        };
        let mut labels = display
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let label = shortcuts
                    .get(index)
                    .and_then(Option::as_ref)
                    .unwrap_or(&tab.label);
                if tab.active {
                    format!("[{label}]")
                } else {
                    label.clone()
                }
            })
            .collect::<Vec<_>>();
        if !display.header.is_empty() {
            labels.push(display.header.clone());
        }
        let header = labels.join("  ");
        let header_height = usize::from(!header.is_empty() && pane_height > 0);
        let content_height = pane_height.saturating_sub(header_height);
        let mut rows = Vec::new();
        if header_height > 0 {
            rows.push(FrameRow {
                spans: vec![Span {
                    base_style: None,
                    text: fit(&header, width),
                    style: options
                        .get("header-style")
                        .and_then(Value::as_str)
                        .unwrap_or("ui.heading")
                        .into(),
                }],
            });
        }
        let mut cursor = None;
        let mut mouse_content = mouse::Content::Rows;
        let mut mouse_gutter = 0;
        let mut context = json!({
            "kind": kind,
            "title": title,
            "active": active,
            "modified": modified,
            "loading": self.views[&id].location.display.loading,
            "plugin_status": self.views[&id].location.display.status,
            "view_id": id,
            "buffer_id": buffer,
        });
        context["options"] = options.clone();
        context["width"] = json!(width);
        let gutter_left_padding = options
            .get("gutter-left-padding")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .min(width as u64) as usize;
        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
            let gutter = if self.views[&id].presentation == LayerPresentation::Inline
                || options.get("gutter").is_some_and(|value| value == false)
            {
                0
            } else {
                (text.line_count().to_string().len() + gutter_left_padding + 1)
                    .min(width.saturating_sub(1))
            };
            let mut rendered = text.render(id, width - gutter, content_height, caret);
            mouse_gutter = gutter;
            mouse_content = mouse::Content::Text {
                version: text.version(),
                positions: std::mem::take(&mut rendered.hit_positions),
            };
            let view = text.view(id);
            let position = view.cursor();
            context["mode"] = json!(view.mode());
            context["selection_count"] = json!(view.selection_count());
            context["position"] = json!({"row":position.0+1,"column":position.1+1});
            context["encoding"] = json!("UTF-8");
            for (index, mut row) in rendered.rows.into_iter().enumerate() {
                if gutter > 0 {
                    let number = rendered.scroll + index + 1;
                    row.spans.insert(
                        0,
                        Span {
                            base_style: None,
                            text: fit(&format!("{number:>digits$} ", digits = gutter - 1), gutter),
                            style: if number == position.0 + 1 && active {
                                "linenr.current"
                            } else {
                                "linenr"
                            }
                            .into(),
                        },
                    );
                }
                rows.push(row);
            }
            if caret {
                cursor = rendered
                    .cursor
                    .map(|(x, y)| (x + gutter as u16, y + header_height as u16));
            }
        } else if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&buffer] {
            let view = self.views.get_mut(&id).unwrap();
            view.viewport_width = width;
            view.viewport_height = content_height;
            if matches!(
                view.location.diff_presentation,
                diff::Presentation::Rendered
            ) && let Some(rendered) = &diff.rendered
            {
                let index = diff.index(view.location.diff_presentation);
                if diff.mapped_rendering()
                    && !view.location.manual_viewport
                    && let Some(selection) = view.location.diff_selection.rows(index)
                {
                    if *selection.start() < view.location.scroll {
                        view.location.scroll = *selection.start();
                    }
                    if content_height > 0
                        && let Some(row) = view.location.diff_selection.row()
                        && row >= view.location.scroll + content_height
                    {
                        view.location.scroll = row + 1 - content_height;
                    }
                }
                view.location.scroll = view
                    .location
                    .scroll
                    .min(rendered.rows.len().saturating_sub(content_height.max(1)));
                view.location.horizontal_scroll = view
                    .location
                    .horizontal_scroll
                    .min(rendered.width.saturating_sub(width));
                let layout = zess_ui::RowColumns::default();
                mouse_content = mouse::Content::Diff {
                    scroll: view.location.scroll,
                    presentation: diff::Presentation::Rendered,
                };
                for row in rendered
                    .rows
                    .iter()
                    .enumerate()
                    .skip(view.location.scroll)
                    .take(content_height)
                {
                    let (number, row) = row;
                    let selected = active
                        && diff.mapped_rendering()
                        && view.location.diff_selection.contains(index, number);
                    let mut spans = row.spans.clone();
                    if selected {
                        for span in &mut spans {
                            span.base_style = Some(std::mem::take(&mut span.style));
                            span.style = "diff.selection".into();
                        }
                    }
                    skip_columns(&mut spans, view.location.horizontal_scroll);
                    rows.push(FrameRow {
                        spans: align_row(
                            spans,
                            &[],
                            if selected { "diff.selection" } else { "" },
                            &layout.layout(0, width),
                        ),
                    });
                }
                let position = diff
                    .mapped_rendering()
                    .then(|| view.location.diff_selection.row())
                    .flatten()
                    .and_then(|row| index.rows.get(row));
                let old = position.and_then(|row| row.old_line);
                let new = position.and_then(|row| row.new_line);
                context["mode"] = json!(if diff.mapped_rendering() {
                    match view.location.diff_selection {
                        diff::Selection::Block { .. } => "hunk",
                        diff::Selection::Line { .. } => "line",
                        diff::Selection::Range { .. } => "range",
                    }
                } else {
                    "browse"
                });
                context["selection_count"] = json!(usize::from(position.is_some()));
                context["source_position"] = json!({"old_line": old, "new_line": new});
                context["old_line"] = json!(old);
                context["new_line"] = json!(new);
            } else {
                let selection = view.location.diff_selection.rows(&diff.index);
                if !view.location.manual_viewport {
                    if let Some(selection) = &selection {
                        if *selection.start() < view.location.scroll {
                            view.location.scroll = *selection.start();
                        }
                        if content_height > 0
                            && let Some(row) = view.location.diff_selection.row()
                            && row >= view.location.scroll + content_height
                        {
                            view.location.scroll = row + 1 - content_height;
                        }
                    }
                }
                view.location.scroll = view
                    .location
                    .scroll
                    .min(diff.index.rows.len().saturating_sub(content_height.max(1)));
                let digits = diff.line_number_width;
                let gutter = if options.get("gutter").is_some_and(|value| value == false) {
                    0
                } else {
                    (digits + gutter_left_padding + 1).min(width.saturating_sub(1))
                };
                view.location.horizontal_scroll = view
                    .location
                    .horizontal_scroll
                    .min(diff.width.saturating_sub(width - gutter));
                mouse_gutter = gutter;
                mouse_content = mouse::Content::Diff {
                    scroll: view.location.scroll,
                    presentation: diff::Presentation::Raw,
                };
                for row in view.location.scroll
                    ..(view.location.scroll + content_height).min(diff.index.rows.len())
                {
                    let metadata = &diff.index.rows[row];
                    let base = match metadata.kind {
                        Some(zess_git::patch::LineKind::Addition) => "git.added",
                        Some(zess_git::patch::LineKind::Deletion) => "git.deleted",
                        _ if metadata.hunk.is_some() && metadata.kind.is_none() => "keyword",
                        _ => "",
                    };
                    let style = if active && view.location.diff_selection.contains(&diff.index, row)
                    {
                        format!("selection.{base}")
                    } else {
                        base.into()
                    };
                    let mut spans = vec![Span {
                        base_style: None,
                        text: diff.display_line(row),
                        style: style.clone(),
                    }];
                    skip_columns(&mut spans, view.location.horizontal_scroll);
                    let mut spans = align_row(
                        spans,
                        &[],
                        &style,
                        &zess_ui::RowColumns::default().layout(0, width - gutter),
                    );
                    if gutter > 0 {
                        let number = metadata
                            .new_line
                            .or(metadata.old_line)
                            .map(|line| line.to_string())
                            .unwrap_or_default();
                        spans.insert(
                            0,
                            Span {
                                base_style: None,
                                text: fit(
                                    &format!("{number:>digits$} ", digits = gutter - 1),
                                    gutter,
                                ),
                                style: if active
                                    && view.location.diff_selection.contains(&diff.index, row)
                                {
                                    "linenr.current"
                                } else {
                                    "linenr"
                                }
                                .into(),
                            },
                        );
                    }
                    rows.push(FrameRow { spans });
                }
                let position = view
                    .location
                    .diff_selection
                    .row()
                    .and_then(|row| diff.index.rows.get(row));
                let old = position.and_then(|row| row.old_line);
                let new = position.and_then(|row| row.new_line);
                context["mode"] = json!(match view.location.diff_selection {
                    diff::Selection::Block { .. } => "hunk",
                    diff::Selection::Line { .. } => "line",
                    diff::Selection::Range { .. } => "range",
                });
                context["selection_count"] = json!(usize::from(position.is_some()));
                context["source_position"] = json!({"old_line": old, "new_line": new});
                context["old_line"] = json!(old);
                context["new_line"] = json!(new);
            }
        } else if matches!(self.buffers[&buffer], Buffer::PreviewRows { .. }) {
            let view = self.views.get_mut(&id).unwrap();
            view.viewport_width = width;
            view.viewport_height = content_height;
            view.location.horizontal_scroll = view
                .location
                .horizontal_scroll
                .min(view.horizontal_scroll_limit());
            view.location.scroll = view.location.scroll.min(
                view.location
                    .display
                    .rows
                    .len()
                    .saturating_sub(content_height.max(1)),
            );
            rows.extend(preview_rows(
                &view.location.display,
                &view.location.columns,
                view.location.scroll,
                view.location.horizontal_scroll,
                width,
                content_height,
            ));
        } else {
            let view = self.views.get_mut(&id).unwrap();
            view.viewport_width = width;
            if view.location.horizontal_scroll > 0 {
                view.location.horizontal_scroll = view
                    .location
                    .horizontal_scroll
                    .min(view.horizontal_scroll_limit());
            }
            let current = view
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
            let help = kind == "help" && content_height > 0;
            let available = content_height.saturating_sub(usize::from(help));
            view.viewport_height = available;
            view.location.scroll = if view.location.manual_viewport {
                view.location.scroll.min(zess_ui::scroll_limit_with_margin(
                    view.location.display.rows.len(),
                    available,
                    5,
                ))
            } else {
                zess_ui::scroll_with_margin(view.location.scroll, current, available, 5)
            };
            let group = if help {
                view.location
                    .display
                    .rows
                    .iter()
                    .take(view.location.scroll.saturating_add(1))
                    .rfind(|row| row.id.is_empty())
            } else {
                None
            };
            let first = view.location.display.rows.get(view.location.scroll);
            let skip = view.location.scroll
                + usize::from(first.is_some_and(|row| help && row.id.is_empty()));
            if let Some(group) = group {
                rows.push(FrameRow {
                    spans: vec![Span {
                        base_style: None,
                        text: fit(&group.text, width),
                        style: group.style.clone(),
                    }],
                });
            }
            for (index, row) in view
                .location
                .display
                .rows
                .iter()
                .enumerate()
                .skip(skip)
                .take(available)
            {
                let style = if active && view.location.selection.selected.contains(&row.id) {
                    format!("selection.{}", row.style)
                } else {
                    row.style.clone()
                };
                let label = view
                    .jump
                    .as_ref()
                    .and_then(|jump| jump.targets.iter().position(|id| id == &row.id));
                let mut spans = Vec::new();
                let indent = "  ".repeat(row.depth.min(30));
                if let Some(index) = label {
                    spans.push(Span {
                        base_style: None,
                        text: indent,
                        style: style.clone(),
                    });
                    spans.push(Span {
                        base_style: None,
                        text: format!(
                            "{}{}",
                            (b'a' + (index / 26) as u8) as char,
                            (b'a' + (index % 26) as u8) as char
                        ),
                        style: "pending".into(),
                    });
                    let mut skipped = 0;
                    let rest = row
                        .text
                        .chars()
                        .skip_while(|ch| {
                            if skipped < 2 {
                                skipped += ch.width().unwrap_or(0);
                                true
                            } else {
                                false
                            }
                        })
                        .collect::<String>();
                    spans.push(Span {
                        base_style: None,
                        text: format!("{}{rest}", " ".repeat(skipped.saturating_sub(2))),
                        style: style.clone(),
                    });
                } else {
                    spans.push(Span {
                        base_style: None,
                        text: format!("{indent}{}", row.text),
                        style: style.clone(),
                    });
                }
                skip_columns(&mut spans, view.location.horizontal_scroll);
                let spans = align_row(
                    spans,
                    &row.cells,
                    &style,
                    &view.location.columns.layout(index, width),
                );
                rows.push(FrameRow { spans });
            }
            mouse_content = mouse::Content::Items {
                ids: group
                    .map(|_| String::new())
                    .into_iter()
                    .chain(
                        view.location
                            .display
                            .rows
                            .iter()
                            .skip(skip)
                            .take(available)
                            .map(|row| row.id.clone()),
                    )
                    .collect(),
            };
            context["mode"] = json!(view.location.mode.as_str());
            context["selection_count"] = json!(view.location.selection.selected.len());
            context["selected_count"] = json!(view.location.selection.selected.len());
            context["primary"] = json!(view.location.selection.primary);
            context["item_count"] = json!(view.item_ids().len());
        }
        rows.resize_with(pane_height, FrameRow::default);
        if !status_name.is_empty() {
            context["width"] = json!(pane_width);
            context["right_inset"] = json!(right_inset);
            rows.push(FrameRow {
                spans: self.statusline(id, status_name, context, pane_width, active),
            });
        }
        let has_pane_layer =
            layer.is_some_and(|child| self.views[&child].presentation != LayerPresentation::Inline);
        if !has_pane_layer {
            self.mouse_regions.push(mouse::Region {
                target: RenderTarget::View(id),
                buffer,
                pane: Rect {
                    x: origin.0,
                    y: origin.1,
                    width: pane_width,
                    height,
                },
                body: Rect {
                    x: origin.0 + mouse_gutter,
                    y: origin.1 + header_height,
                    width: width - mouse_gutter,
                    height: content_height,
                },
                content: mouse_content,
            });
        }
        if let Some(child) = layer {
            if self.views[&child].presentation == LayerPresentation::Inline {
                let available = content_height;
                let overlay_height = available.min(2);
                let view = &self.views[&id];
                let parent_row = view
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
                    .unwrap_or(0)
                    .saturating_sub(view.location.scroll);
                let start = (parent_row + 1).min(available.saturating_sub(overlay_height));
                let (overlay, position) = self.render_view_at(
                    child,
                    pane_width,
                    overlay_height,
                    (origin.0, origin.1 + start),
                );
                for (offset, row) in overlay.into_iter().enumerate() {
                    rows[start + offset] = row;
                }
                cursor = position.map(|(x, y)| (x, y + start as u16));
            } else {
                return self.render_view_at(child, pane_width, height, origin);
            }
        }
        if right_inset > 0 {
            for (index, row) in rows.iter_mut().enumerate() {
                let used = row
                    .spans
                    .iter()
                    .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                    .sum::<usize>();
                row.spans.push(Span {
                    base_style: None,
                    text: " ".repeat(pane_width.saturating_sub(used)),
                    style: if index == pane_height && !status_name.is_empty() {
                        if active { "status" } else { "status.inactive" }
                    } else {
                        ""
                    }
                    .into(),
                });
            }
        }
        (rows, cursor)
    }
}

fn align_row(
    mut spans: Vec<Span>,
    cells: &[zess_ui::DisplayCell],
    style: &str,
    layout: &zess_ui::RowLayout<'_>,
) -> Vec<Span> {
    let used = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.text.as_str()))
        .sum::<usize>();
    spans.push(Span {
        base_style: None,
        text: " ".repeat(layout.text_width().saturating_sub(used)),
        style: style.into(),
    });
    let mut spans = clip_spans(spans, layout.text_width());
    for (index, width) in layout.cell_widths().iter().enumerate() {
        let cell = cells
            .get(index)
            .map(|cell| cell.spans.as_slice())
            .unwrap_or_default();
        let used = cell
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum::<usize>();
        spans.push(Span {
            base_style: None,
            text: " ".repeat(layout.gap() + width.saturating_sub(used)),
            style: style.into(),
        });
        spans.extend(cell.iter().map(|span| Span {
            base_style: span.base_style.clone(),
            text: span.text.clone(),
            style: if span.style.is_empty() {
                style.into()
            } else if style == "selection" || style.starts_with("selection.") {
                format!("selection.{}", span.style)
            } else {
                span.style.clone()
            },
        }));
    }
    spans
}

fn skip_columns(spans: &mut [Span], mut skip: usize) {
    for span in spans {
        let mut start = 0;
        let mut padding = 0;
        for (index, ch) in span.text.char_indices() {
            if skip == 0 {
                break;
            }
            let cells = ch.width().unwrap_or(0);
            padding = cells.saturating_sub(skip);
            skip = skip.saturating_sub(cells);
            start = index + ch.len_utf8();
        }
        span.text.replace_range(..start, &" ".repeat(padding));
    }
}

fn preview_rows(
    display: &Display,
    columns: &zess_ui::RowColumns,
    scroll: usize,
    horizontal_scroll: usize,
    width: usize,
    height: usize,
) -> Vec<FrameRow> {
    display
        .rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(index, row)| {
            let mut spans = vec![Span {
                base_style: None,
                text: format!("{}{}", "  ".repeat(row.depth.min(30)), row.text),
                style: row.style.clone(),
            }];
            skip_columns(&mut spans, horizontal_scroll);
            FrameRow {
                spans: align_row(spans, &row.cells, &row.style, &columns.layout(index, width)),
            }
        })
        .collect()
}
