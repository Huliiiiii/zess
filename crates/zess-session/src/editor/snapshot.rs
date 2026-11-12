use super::*;
use zess_protocol::{
    CommandAction, CommandOutcome, PromptSnapshot, SemanticRow, SessionSnapshot, SurfaceSnapshot,
    TextRange, ViewArea, ViewContent, ViewSelection, ViewSnapshot, VisibleTextLine,
};

impl Session {
    pub fn cancel_clipboard_operations(&mut self, reason: &str) {
        self.cancel_clipboard_read(reason);
    }

    pub(super) fn complete_operations(
        &mut self,
        completions: Vec<zess_plugin::OperationCompletion>,
    ) {
        for completion in completions {
            let outcome = match completion.result {
                zess_plugin::OperationResult::Completed => CommandOutcome::Applied,
                zess_plugin::OperationResult::Failed { message } => {
                    CommandOutcome::Failed { message }
                }
                zess_plugin::OperationResult::Cancelled => CommandOutcome::Failed {
                    message: "operation cancelled".into(),
                },
            };
            self.finish_operation(completion.operation_id, outcome);
        }
    }

    pub(super) fn finish_operation(&mut self, operation_id: String, outcome: CommandOutcome) {
        self.pending_operations.remove(&operation_id);
        if self.current_operation.as_deref() == Some(operation_id.as_str()) {
            self.operation_outcome = Some(outcome);
        } else {
            self.operation_completions.push((operation_id, outcome));
        }
    }

    pub fn begin_operation(
        &mut self,
        operation_id: &str,
        action: &CommandAction,
    ) -> Option<CommandOutcome> {
        self.current_operation = Some(operation_id.to_owned());
        self.operation_outcome = None;
        self.cancel_clipboard_read("clipboard read cancelled by a later command");
        let outcome = self.operation(action);
        self.current_operation = None;
        if self.pending_operations.contains(operation_id) {
            None
        } else {
            Some(if outcome == CommandOutcome::Applied {
                self.operation_outcome.take().unwrap_or(outcome)
            } else {
                outcome
            })
        }
    }

    pub fn tracked_input(&mut self, operation_id: &str, key: &str) -> Option<CommandOutcome> {
        self.current_operation = Some(operation_id.to_owned());
        self.operation_outcome = None;
        let result = self.input_result(key);
        self.current_operation = None;
        if self.pending_operations.contains(operation_id) {
            None
        } else {
            Some(match result {
                Ok(()) => self
                    .operation_outcome
                    .take()
                    .unwrap_or(CommandOutcome::Applied),
                Err(error) => CommandOutcome::Failed {
                    message: format!("{error:#}"),
                },
            })
        }
    }

    pub fn take_operation_completions(&mut self) -> Vec<(String, CommandOutcome)> {
        std::mem::take(&mut self.operation_completions)
    }

    pub fn snapshot(&mut self) -> SessionSnapshot {
        let frame = self.frame();
        self.snapshot_with_frame(&frame)
    }

    pub fn snapshot_with_frame(&mut self, frame: &Frame) -> SessionSnapshot {
        let mut views = self
            .views
            .iter()
            .map(|(&id, view)| {
                let buffer = &self.buffers[&view.location.buffer];
                let (kind, title, version, mode, content, selection, scroll, horizontal_scroll) =
                    match buffer {
                        Buffer::Text(text)
                        | Buffer::Snapshot { text, .. }
                        | Buffer::PreviewText { text, .. } => {
                            let state = text.view(id);
                            let title = text
                                .path()
                                .map(|path| path.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "[scratch]".into());
                            let kind = match buffer {
                                Buffer::Text(_) => "text",
                                Buffer::Snapshot { .. } => "snapshot",
                                _ => "preview",
                            };
                            (
                                kind.to_owned(),
                                title,
                                Some(text.version()),
                                state.mode().to_owned(),
                                text_content(text.visible_source_lines(id)),
                                ViewSelection::Text {
                                    ranges: state
                                        .selection()
                                        .iter()
                                        .map(|range| TextRange {
                                            anchor: range.anchor,
                                            head: range.head,
                                        })
                                        .collect(),
                                    primary: state.selection().primary_index(),
                                },
                                state.scroll(),
                                state.horizontal_scroll(),
                            )
                        }
                        Buffer::PreviewDiff { path, diff } => {
                            let presentation = view.location.diff_presentation;
                            let index = diff.index(presentation);
                            let rows = if presentation == diff::Presentation::Rendered {
                                diff.rendered.as_ref().map(|rendered| {
                                    rendered
                                        .rows
                                        .iter()
                                        .enumerate()
                                        .skip(view.location.scroll)
                                        .take(view.viewport_height)
                                        .map(|(row, line)| {
                                            diff_row(
                                                row,
                                                line.spans.clone(),
                                                view.location.horizontal_scroll,
                                                view.viewport_width,
                                            )
                                        })
                                        .collect()
                                })
                            } else {
                                None
                            }
                            .unwrap_or_else(|| {
                                index
                                    .rows
                                    .iter()
                                    .enumerate()
                                    .skip(view.location.scroll)
                                    .take(view.viewport_height)
                                    .map(|(row, _)| {
                                        diff_row(
                                            row,
                                            vec![Span {
                                                base_style: None,
                                                text: diff.display_line(row),
                                                style: String::new(),
                                            }],
                                            view.location.horizontal_scroll,
                                            view.viewport_width,
                                        )
                                    })
                                    .collect()
                            });
                            let ids = view
                                .location
                                .diff_selection
                                .rows(index)
                                .into_iter()
                                .flatten()
                                .map(|row| row.to_string())
                                .collect();
                            (
                                "diff".into(),
                                path.clone(),
                                None,
                                view.location.mode.as_str().to_owned(),
                                ViewContent::Items { rows },
                                ViewSelection::Items {
                                    ids,
                                    primary: view
                                        .location
                                        .diff_selection
                                        .row()
                                        .map(|row| row.to_string()),
                                },
                                view.location.scroll,
                                view.location.horizontal_scroll,
                            )
                        }
                        Buffer::Data { kind, .. } | Buffer::Items { kind, .. } => (
                            kind.clone(),
                            view.location.display.title.clone(),
                            view.location.display.rows_revision,
                            view.location.mode.as_str().to_owned(),
                            rows_content(
                                &view.location.display,
                                view.location.scroll
                                    + usize::from(
                                        kind == "help"
                                            && view
                                                .location
                                                .display
                                                .rows
                                                .get(view.location.scroll)
                                                .is_some_and(|row| row.id.is_empty()),
                                    ),
                                view.viewport_height,
                            ),
                            ViewSelection::Items {
                                ids: view.location.selection.selected.clone(),
                                primary: view.location.selection.primary.clone(),
                            },
                            view.location.scroll,
                            view.location.horizontal_scroll,
                        ),
                        Buffer::PreviewRows {
                            kind,
                            path,
                            display,
                        } => (
                            kind.scope().into(),
                            path.clone(),
                            display.rows_revision,
                            view.location.mode.as_str().to_owned(),
                            rows_content(display, view.location.scroll, view.viewport_height),
                            ViewSelection::Items {
                                ids: view.location.selection.selected.clone(),
                                primary: view.location.selection.primary.clone(),
                            },
                            view.location.scroll,
                            view.location.horizontal_scroll,
                        ),
                    };
                ViewSnapshot {
                    view_id: id,
                    buffer_id: view.location.buffer,
                    kind,
                    title,
                    version,
                    mode,
                    content,
                    selection,
                    scroll,
                    horizontal_scroll,
                    status: view.location.statusline.clone(),
                    header: view.location.display.header.clone(),
                    tabs: view.location.display.tabs.clone(),
                    loading: view.location.display.loading,
                    column_alignment: view.location.display.column_alignment,
                }
            })
            .collect::<Vec<_>>();
        views.sort_by_key(|view| view.view_id);
        let mut surfaces = Vec::new();
        let layout = self
            .visible_rectangles()
            .into_iter()
            .filter_map(|(target, area)| {
                let (view_id, buffer_id, page_id, slot_id) = match target {
                    RenderTarget::View(id) => {
                        (Some(id), self.views[&id].location.buffer, None, None)
                    }
                    RenderTarget::Surface { page, slot } => {
                        let surface = self.views.surface(page, &slot)?;
                        let buffer = surface.buffer;
                        let scroll = surface.viewport.scroll;
                        let count = surface.viewport.height;
                        let content = match &self.buffers[&buffer] {
                            Buffer::PreviewText { text, .. } => {
                                let source = self.views.main(page);
                                let soft_wrap = matches!(
                                    &self.buffers[&self.views[&source].location.buffer],
                                    Buffer::Data { kind, .. } if kind == "files"
                                ) && self
                                    .effective_view_options(source, "files")
                                    .get("preview-wrap")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true);
                                text_content(
                                    text.preview_visible_source_lines(&surface.viewport, soft_wrap),
                                )
                            }
                            Buffer::PreviewRows { display, .. } => {
                                rows_content(display, scroll, count)
                            }
                            Buffer::PreviewDiff { diff, .. } => ViewContent::Items {
                                rows: (scroll..diff.index.rows.len())
                                    .take(count)
                                    .map(|row| {
                                        diff_row(
                                            row,
                                            vec![Span {
                                                base_style: None,
                                                text: diff.display_line(row),
                                                style: String::new(),
                                            }],
                                            surface.viewport.horizontal_scroll,
                                            surface.viewport.width,
                                        )
                                    })
                                    .collect(),
                            },
                            _ => unreachable!("surface content must be a preview buffer"),
                        };
                        let page_id = self.views.main(page);
                        surfaces.push(SurfaceSnapshot {
                            page_id,
                            slot_id: slot.clone(),
                            buffer_id: buffer,
                            header: surface.header.clone(),
                            loading: surface.loading,
                            content,
                            scroll,
                            horizontal_scroll: surface.viewport.horizontal_scroll,
                        });
                        (None, buffer, Some(page_id), Some(slot))
                    }
                };
                Some(ViewArea {
                    view_id,
                    page_id,
                    slot_id,
                    buffer_id,
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: area.height,
                })
            })
            .collect();
        let prompt = self.active_prompt().map(|prompt| PromptSnapshot {
            kind: match &prompt.kind {
                PromptKind::Command => "command".into(),
                PromptKind::Text(kind) => kind.label().into(),
                PromptKind::DiffSearch(_) => "diff_search".into(),
                PromptKind::Filter(_) => "filter".into(),
                PromptKind::MiniBuffer { purpose, .. } => purpose.clone(),
            },
            text: prompt.text.clone(),
            cursor: prompt.cursor,
            error: prompt.error.clone(),
            owner_view: prompt.target.view,
        });
        let mut state = SessionSnapshot {
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
            render_plan: Default::default(),
        };
        state.render_plan = self.presentation_plan(frame, &state);
        state
    }

    fn operation(&mut self, action: &CommandAction) -> CommandOutcome {
        let result = match action {
            CommandAction::AcknowledgeClientAction { action_id } => (|| {
                let index = self
                    .client_actions
                    .iter()
                    .position(|action| {
                        action.id == *action_id
                            && !matches!(action.kind, zess_protocol::ClientActionKind::Close { .. })
                    })
                    .context("workspace action is no longer pending")?;
                let action = self.client_actions.remove(index);
                self.client_workspace.apply(&action.kind);
                Ok(CommandOutcome::Applied)
            })(),
            CommandAction::CloseBuffer {
                action_id,
                buffer_id,
                replacement,
                force,
            } => self
                .close_client_buffer(*action_id, *buffer_id, *replacement, *force)
                .map(|replacement| CommandOutcome::BufferClosed {
                    buffer_id: *buffer_id,
                    replacement,
                }),
            CommandAction::ClientExit {
                action_id,
                buffer_id,
            } => self
                .client_exit(*action_id, *buffer_id)
                .map(|()| CommandOutcome::Applied),
            CommandAction::CommandLine { text } => {
                self.command(text).map(|()| CommandOutcome::Applied)
            }
            CommandAction::Command { name, argument } => self
                .execute_command(name, argument.clone())
                .map(|()| CommandOutcome::Applied),
            CommandAction::Save { buffer_id, version } => (|| {
                let Some(Buffer::Text(text)) = self.buffers.get_mut(buffer_id) else {
                    bail!("buffer {buffer_id} is not an editable text buffer");
                };
                anyhow::ensure!(
                    text.version() == *version,
                    "buffer version changed; refresh before saving"
                );
                text.save(None)?;
                self.services.borrow_mut().invalidate_fs();
                if let Err(error) = self.rebind_file_watch(*buffer_id) {
                    self.message = format!("buffer saved; file watch: {error:#}");
                }
                Ok(CommandOutcome::Saved {
                    buffer_id: *buffer_id,
                    version: *version,
                })
            })(),
            CommandAction::Paste { text } => {
                self.paste_text(text).map(|()| CommandOutcome::Applied)
            }
        };
        match result {
            Ok(outcome) => outcome,
            Err(error) => CommandOutcome::Failed {
                message: format!("{error:#}"),
            },
        }
    }

    fn paste_text(&mut self, text: &str) -> Result<()> {
        self.invalidate_prompt()?;
        self.cancel_clipboard_read("clipboard read cancelled by paste");
        self.key_sequence.clear();
        if let Some(prompt) = self.prompt.as_mut() {
            let text = text.replace("\r\n", "\n").replace(['\r', '\n'], " ");
            prompt.completion = None;
            prompt.error = None;
            prompt.text.insert_str(prompt.cursor, &text);
            prompt.cursor += text.len();
            self.update_prompt()
        } else {
            let effect = self
                .buffers
                .get_mut(&self.views[&self.focus].location.buffer)
                .and_then(Buffer::text)
                .context("paste requires an editable text buffer")?
                .paste_text(self.focus, text)?;
            self.text_effect(effect)
        }
    }
}

fn text_content(lines: Vec<(usize, usize, String)>) -> ViewContent {
    ViewContent::Text {
        lines: lines
            .into_iter()
            .map(|(line, first_char, text)| VisibleTextLine {
                line,
                first_char,
                text,
            })
            .collect(),
    }
}

fn rows_content(display: &Display, scroll: usize, count: usize) -> ViewContent {
    ViewContent::Items {
        rows: display
            .rows
            .iter()
            .skip(scroll)
            .take(count)
            .map(|row| SemanticRow {
                id: row.id.clone(),
                text: row.text.clone(),
                cells: row.cells.iter().map(|cell| cell.spans.clone()).collect(),
                style: row.style.clone(),
                depth: row.depth,
            })
            .collect(),
    }
}

fn diff_row(
    row: usize,
    mut spans: Vec<Span>,
    horizontal_scroll: usize,
    width: usize,
) -> SemanticRow {
    render::skip_columns(&mut spans, horizontal_scroll);
    let spans = clip_spans(spans, width);
    SemanticRow {
        id: row.to_string(),
        text: spans.into_iter().map(|span| span.text).collect(),
        cells: Vec::new(),
        style: String::new(),
        depth: 0,
    }
}
