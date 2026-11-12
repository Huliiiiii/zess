use super::*;
use zess_protocol::{CommandAction, CommandOutcome, ViewContent, ViewSelection};

fn normalized(mut frame: Frame) -> Frame {
    for row in &mut frame.rows {
        let mut spans: Vec<Span> = Vec::new();
        for span in std::mem::take(&mut row.spans) {
            if span.text.is_empty() {
                continue;
            }
            if let Some(previous) = spans.last_mut().filter(|previous| {
                previous.style == span.style && previous.base_style == span.base_style
            }) {
                previous.text.push_str(&span.text);
            } else {
                spans.push(span);
            }
        }
        row.spans = spans;
    }
    frame
}

#[test]
fn cached_text_renders_syntax_unicode_and_selection_after_horizontal_scrolling() {
    let fixture = Fixture::new();
    let path = fixture.write("source.w", "\tdefine 文 a\u{301}🙂 \"hello\" ; comment\n");
    let mut session = fixture.session(&[path]);
    session.resize(45, 8);
    for key in ["v", "l"] {
        session.input(key);
    }
    let view = session.focus;
    let buffer = session.views[&view].location.buffer;
    session
        .buffers
        .get_mut(&buffer)
        .and_then(Buffer::text)
        .unwrap()
        .scroll_view(view, 4, 0);
    let frame = session.frame();
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.style == "keyword")
    );
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.style == "selection")
    );
    let state = session.snapshot_with_frame(&frame);
    assert!(
        state
            .render_plan
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| matches!(span.content, zess_ui::RenderContent::Text { .. }))
    );
    assert_eq!(
        normalized(zess_protocol::render_snapshot(&state).unwrap()),
        normalized(frame)
    );
}

#[test]
fn search_prompts_keep_the_cached_document_as_the_render_source() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "source text\n");
    let mut session = fixture.session(&[path]);
    session.input("/");
    let frame = session.frame();
    let state = session.snapshot_with_frame(&frame);
    assert!(state.prompt.is_some());
    assert!(
        state
            .render_plan
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| matches!(span.content, zess_ui::RenderContent::Text { .. }))
    );
    assert_eq!(
        normalized(zess_protocol::render_snapshot(&state).unwrap()),
        normalized(frame)
    );
}

#[test]
fn cached_file_browser_renders_item_cells_and_wrapped_preview_text() {
    let fixture = Fixture::new();
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&fixture.0)
            .status()
            .unwrap()
            .success()
    );
    fixture.write("note.w", &"define 界a\u{301}🙂 \"hello\" ".repeat(30));
    assert!(
        std::process::Command::new("git")
            .args(["add", "note.w"])
            .current_dir(&fixture.0)
            .status()
            .unwrap()
            .success()
    );
    let mut session = fixture.session(&[]);
    session.resize(100, 12);
    session.command("files").unwrap();
    settle(&mut session);
    session
        .dispatch(session.focus, "navigate", json!({"path":"note.w"}))
        .unwrap();
    settle(&mut session);
    session.frame();
    session.input("J");
    let frame = session.frame();
    let state = session.snapshot_with_frame(&frame);
    let surface = &state.surfaces[0];
    assert!(surface.scroll > 0);
    assert!(state.render_plan.rows.iter().flat_map(|row| &row.spans).any(|span| matches!(span.content, zess_ui::RenderContent::Text { buffer_id, .. } if buffer_id == surface.buffer_id)));
    assert!(
        state
            .render_plan
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| matches!(
                span.content,
                zess_ui::RenderContent::Item { cell: Some(_), .. }
            ))
    );
    assert_eq!(
        normalized(zess_protocol::render_snapshot(&state).unwrap()),
        normalized(frame)
    );
}

#[test]
fn cached_diffs_render_visible_fragments_after_horizontal_scrolling() {
    let fixture = Fixture::new();
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&fixture.0)
            .status()
            .unwrap()
            .success()
    );
    fixture.write("note.txt", "original\n");
    assert!(
        std::process::Command::new("git")
            .args(["add", "note.txt"])
            .current_dir(&fixture.0)
            .status()
            .unwrap()
            .success()
    );
    fixture.write("note.txt", &format!("{}界\n", "changed ".repeat(100)));
    let mut session = fixture.session(&[]);
    session.resize(100, 12);
    session.command("git").unwrap();
    settle(&mut session);
    session.focus_preview(session.focus).unwrap();
    session.frame();
    session
        .execute_command("preview.move-right", json!({}))
        .unwrap();
    let frame = session.frame();
    let state = session.snapshot_with_frame(&frame);
    let view = state
        .views
        .iter()
        .find(|view| view.kind == "diff" && view.horizontal_scroll > 0)
        .unwrap();
    let ViewContent::Items { rows } = &view.content else {
        panic!("expected diff rows")
    };
    assert!(
        rows.iter()
            .all(|row| UnicodeWidthStr::width(row.text.as_str()) <= usize::from(frame.width))
    );
    assert!(state.render_plan.rows.iter().flat_map(|row| &row.spans).any(|span| matches!(&span.content, zess_ui::RenderContent::Item {source: zess_ui::ContentSource::View {view_id}, ..} if *view_id == view.view_id)));
    assert_eq!(
        normalized(zess_protocol::render_snapshot(&state).unwrap()),
        normalized(frame)
    );
}

#[test]
fn preview_source_maps_remain_attached_to_each_viewport() {
    let mut text = TextBuffer::new("abcdefg");
    let mut narrow = zess_text::PreviewViewport::default();
    let mut wide = zess_text::PreviewViewport::default();
    text.render_preview(&mut narrow, 3, 1, true);
    text.render_preview(&mut wide, 5, 1, true);
    assert_eq!(narrow.hit_positions, vec![vec![0, 1, 2]]);
    assert_eq!(wide.hit_positions, vec![vec![0, 1, 2, 3, 4]]);
    assert_eq!(
        text.preview_visible_source_lines(&narrow, true),
        vec![(0, 0, "abc".into())]
    );
    assert_eq!(
        text.preview_visible_source_lines(&wide, true),
        vec![(0, 0, "abcde".into())]
    );
}

#[test]
fn pasted_text_stays_in_insert_mode_and_undoes_as_one_edit() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "original");
    let mut session = fixture.session(&[path]);
    session.input("i");
    let view = session.focus;
    let buffer = session.views[&view].location.buffer;
    assert_eq!(
        session.begin_operation(
            "paste",
            &CommandAction::Paste {
                text: "界\ntext".into()
            }
        ),
        Some(CommandOutcome::Applied)
    );
    let text = session
        .buffers
        .get_mut(&buffer)
        .and_then(Buffer::text)
        .unwrap();
    assert_eq!(text.text(), "界\ntextoriginal");
    assert_eq!(text.view(view).mode(), "insert");
    session.execute_command("text.undo", json!({})).unwrap();
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .text(),
        "original"
    );
}

#[test]
fn pasting_into_a_command_prompt_does_not_submit_line_breaks() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.input(":");
    assert_eq!(
        session.begin_operation(
            "paste",
            &CommandAction::Paste {
                text: "write\nnew\r\nexit".into()
            }
        ),
        Some(CommandOutcome::Applied)
    );
    let prompt = session.active_prompt().unwrap();
    assert!(matches!(prompt.kind, PromptKind::Command));
    assert_eq!(prompt.text, "write new exit");
    assert_eq!(prompt.cursor, "write new exit".len());
    assert!(!session.exiting());
    assert!(session.client_actions.is_empty());
}

#[test]
fn clipboard_paste_operations_finish_only_after_the_read_reply() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    for key in ["\"", "+"] {
        session.input(key);
    }
    assert_eq!(
        session.begin_operation(
            "paste",
            &CommandAction::Command {
                name: "text.paste-after".into(),
                argument: json!({})
            }
        ),
        None
    );
    let (id, primary) = session.clipboard_request().unwrap();
    assert!(!primary);
    session.complete_clipboard_read(id, Some("clipboard".into()), None);
    assert_eq!(
        session.take_operation_completions(),
        vec![("paste".into(), CommandOutcome::Applied)]
    );
    let buffer = session.views[&session.focus].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .text(),
        "clipboard"
    );
    for key in ["\"", "+"] {
        session.input(key);
    }
    assert_eq!(
        session.begin_operation(
            "failed-paste",
            &CommandAction::Command {
                name: "text.paste-after".into(),
                argument: json!({})
            }
        ),
        None
    );
    let (id, _) = session.clipboard_request().unwrap();
    session.complete_clipboard_read(id, None, Some("unavailable".into()));
    assert_eq!(
        session.take_operation_completions(),
        vec![(
            "failed-paste".into(),
            CommandOutcome::Failed {
                message: "unavailable".into()
            }
        )]
    );
    for key in ["\"", "+"] {
        session.input(key);
    }
    assert_eq!(
        session.begin_operation(
            "cancelled-paste",
            &CommandAction::Command {
                name: "text.paste-after".into(),
                argument: json!({})
            }
        ),
        None
    );
    assert_eq!(
        session.begin_operation(
            "move",
            &CommandAction::Command {
                name: "text.move-left".into(),
                argument: json!({})
            }
        ),
        Some(CommandOutcome::Applied)
    );
    assert!(
        matches!(session.take_operation_completions().as_slice(), [(operation_id, CommandOutcome::Failed {..})] if operation_id == "cancelled-paste")
    );
}

#[test]
fn cancelled_clipboard_reads_cannot_change_text_after_late_replies() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "original");
    let mut session = fixture.session(&[path]);
    let buffer = session.views[&session.focus].location.buffer;
    for key in ["\"", "+"] {
        session.input(key);
    }
    assert_eq!(
        session.begin_operation(
            "paste",
            &CommandAction::Command {
                name: "text.paste-after".into(),
                argument: json!({})
            }
        ),
        None
    );
    let (read_id, _) = session.clipboard_request().unwrap();

    session.cancel_clipboard_operations("attached client changed");
    assert!(session.clipboard_request().is_none());
    let mut completed = session.take_operation_completions();
    completed.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        completed,
        vec![(
            "paste".into(),
            CommandOutcome::Failed {
                message: "attached client changed".into()
            }
        ),]
    );
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .text(),
        "original"
    );
    assert!(!session.modified());

    session.complete_clipboard_read(read_id, Some("late reply".into()), None);
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .text(),
        "original"
    );
    assert!(session.take_operation_completions().is_empty());
}

#[test]
fn read_only_text_rejects_paste() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "source");
    let mut session = fixture.session(&[path]);
    let buffer = session.views[&session.focus].location.buffer;
    session
        .buffers
        .get_mut(&buffer)
        .and_then(Buffer::text)
        .unwrap()
        .set_readonly(true);
    assert!(matches!(
        session.begin_operation(
            "paste",
            &CommandAction::Paste {
                text: "replacement".into()
            }
        ),
        Some(CommandOutcome::Failed { .. })
    ));
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .text(),
        "source"
    );
}

#[test]
fn snapshots_keep_text_view_identity_and_selection_after_editing() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "hello\n");
    let mut session = fixture.session(&[path]);
    let before = session.snapshot();
    for key in ["i", "Ω", "Esc", "l"] {
        session.input(key);
    }
    let after = session.snapshot();
    assert_eq!(after.focused_view, before.focused_view);
    assert_eq!(after.views[0].view_id, before.views[0].view_id);
    assert_eq!(after.views[0].buffer_id, before.views[0].buffer_id);
    assert_eq!(after.views[0].mode, "normal");
    let ViewContent::Text { lines } = &after.views[0].content else {
        panic!("expected text")
    };
    assert_eq!(lines[0].line, 0);
    assert_eq!(lines[0].first_char, 0);
    assert_eq!(lines[0].text.trim_end_matches('\n'), "Ωhello");
    assert!(
        matches!(&after.views[0].selection, ViewSelection::Text { ranges, primary: 0 } if ranges.len() == 1 && ranges[0].anchor == 1 && ranges[0].head == 2)
    );
    assert!(after.modified);
}

#[test]
fn snapshots_do_not_transfer_hidden_parts_of_long_text_lines() {
    let fixture = Fixture::new();
    let path = fixture.write(
        "large.txt",
        &"x".repeat(zess_protocol::MAX_MESSAGE_BYTES + 1),
    );
    let mut session = fixture.session(&[path]);
    session.resize(20, 6);
    let snapshot = session.snapshot();
    let ViewContent::Text { lines } = &snapshot.views[0].content else {
        panic!("expected text")
    };
    assert_eq!(lines[0].first_char, 0);
    assert!(!lines[0].text.is_empty());
    assert!(lines[0].text.len() <= 20);
    assert!(serde_json::to_vec(&snapshot).unwrap().len() <= zess_protocol::MAX_MESSAGE_BYTES);
}

#[test]
fn saving_rejects_a_stale_buffer_version() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "old");
    let mut session = fixture.session(&[path.clone()]);
    let buffer_id = session.views[&session.focus].location.buffer;
    let version = match &session.buffers[&buffer_id] {
        Buffer::Text(text) => text.version(),
        _ => unreachable!(),
    };
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    let outcome =
        session.begin_operation("stale-save", &CommandAction::Save { buffer_id, version });
    assert!(matches!(outcome, Some(CommandOutcome::Failed { .. })));
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    let version = match &session.buffers[&buffer_id] {
        Buffer::Text(text) => text.version(),
        _ => unreachable!(),
    };
    assert_eq!(
        session.begin_operation("save", &CommandAction::Save { buffer_id, version }),
        Some(CommandOutcome::Saved { buffer_id, version })
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "!old");
}

#[test]
fn tracked_prompt_submission_returns_the_save_error() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    for key in [":", "w"] {
        session.input(key);
    }
    assert_eq!(
        session.tracked_input("save", "Enter"),
        Some(CommandOutcome::Failed {
            message: "buffer has no file name".into()
        })
    );
    assert!(session.take_operation_completions().is_empty());
}

#[test]
fn workspace_changes_are_retained_after_client_confirmation() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let original = session.client_workspace.current.unwrap();
    session.command("new").unwrap();
    let current = session.views[&session.focus].location.buffer;
    assert_eq!(session.client_workspace.buffers, [original]);
    let actions = session
        .client_actions
        .iter()
        .map(|action| action.id)
        .collect::<Vec<_>>();
    for id in actions {
        assert_eq!(
            session.begin_operation(
                "workspace",
                &CommandAction::AcknowledgeClientAction { action_id: id }
            ),
            Some(CommandOutcome::Applied)
        );
    }
    assert_eq!(session.client_workspace.buffers, [current]);
    assert_eq!(session.client_workspace.current, Some(current));
}

#[test]
fn a_background_data_action_changes_its_origin_selection() {
    let fixture = Fixture::new();
    fixture.write("a.txt", "a");
    fixture.write("b.txt", "b");
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let origin = session.focus;
    assert_eq!(selected_row(&session, origin).id, "a.txt");
    session.command("vsplit").unwrap();
    let focus = session.focus;
    session
        .request(
            origin,
            HostRequest::Action {
                name: "data.move-down".into(),
                argument: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(session.focus, focus);
    assert_eq!(selected_row(&session, origin).id, "b.txt");
    assert_eq!(selected_row(&session, focus).id, "a.txt");
}

#[test]
fn a_background_navigation_close_preserves_the_focused_help() {
    let fixture = Fixture::new();
    fixture.write("note.txt", "abcd");
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    let origin = session.focus;
    session.command("vsplit").unwrap();
    session.command("help").unwrap();
    let help = session.focus;
    assert!(session.help.is_some());
    session
        .request(
            origin,
            HostRequest::Action {
                name: "quit".into(),
                argument: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(session.focus, help);
    assert!(session.help.is_some());
    assert!(matches!(
        session.buffers[&session.views[&origin].location.buffer],
        Buffer::Text(_)
    ));
}

#[test]
fn a_background_text_action_edits_its_origin_buffer() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "abcd");
    let mut session = fixture.session(&[path]);
    let origin = session.focus;
    let buffer = session.views[&origin].location.buffer;
    session.command("vsplit").unwrap();
    session.command("new").unwrap();
    let focus = session.focus;
    session
        .request(
            origin,
            HostRequest::Action {
                name: "text.delete-without-yank".into(),
                argument: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(session.focus, focus);
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "bcd"
    );
}

#[test]
fn a_background_new_command_replaces_its_origin_view() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "abcd");
    let mut session = fixture.session(&[path]);
    let origin = session.focus;
    let buffer = session.views[&origin].location.buffer;
    session.command("vsplit").unwrap();
    let focus = session.focus;
    session
        .request(
            origin,
            HostRequest::Action {
                name: "new".into(),
                argument: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(session.focus, focus);
    assert_eq!(session.views[&focus].location.buffer, buffer);
    assert_ne!(session.views[&origin].location.buffer, buffer);
}

#[test]
fn a_background_plugin_open_uses_its_origin_view() {
    let fixture = Fixture::new();
    fixture.write("note.txt", "abcd");
    let mut session = fixture.session(&[]);
    let origin = session.focus;
    session.command("vsplit").unwrap();
    let focus = session.focus;
    let buffer = session.views[&focus].location.buffer;
    session
        .request(
            origin,
            HostRequest::OpenBuffer {
                kind: "files".into(),
                argument: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(session.focus, focus);
    assert_eq!(session.views[&focus].location.buffer, buffer);
    assert!(
        matches!(&session.buffers[&session.views[&origin].location.buffer], Buffer::Data { kind, .. } if kind == "files")
    );
}

#[test]
fn a_background_write_saves_its_origin_buffer() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "abcd");
    let mut session = fixture.session(std::slice::from_ref(&path));
    let origin = session.focus;
    session.input("i");
    session.input("x");
    session.input("Esc");
    session.command("vsplit").unwrap();
    session.command("new").unwrap();
    let focus = session.focus;
    session
        .request(
            origin,
            HostRequest::Action {
                name: "write".into(),
                argument: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(session.focus, focus);
    assert_eq!(fs::read_to_string(path).unwrap(), "xabcd");
}

#[test]
fn a_background_quit_targets_its_own_buffer_without_changing_focus() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    for key in ["i", "x", "Esc"] {
        session.input(key);
    }
    let origin = session.focus;
    let buffer = session.views[&origin].location.buffer;
    session.command("vsplit").unwrap();
    session.command("new").unwrap();
    let focus = session.focus;
    session
        .request(
            origin,
            HostRequest::Action {
                name: "quit".into(),
                argument: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(session.focus, focus);
    assert!(session.client_actions.iter().any(|action| matches!(action.kind, zess_protocol::ClientActionKind::Close { buffer_id, force: false } if buffer_id == buffer)));
}

#[test]
fn closing_a_modified_buffer_requires_force() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.input("i");
    session.input("x");
    session.input("Esc");
    let buffer = session.views[&session.focus].location.buffer;
    session.command("q").unwrap();
    let action = session
        .client_actions
        .iter()
        .find(|action| matches!(action.kind, zess_protocol::ClientActionKind::Close { .. }))
        .unwrap()
        .id;
    assert!(
        session
            .close_client_buffer(action, buffer, None, false)
            .is_err()
    );
    assert!(session.modified());
    session.command("q!").unwrap();
    let action = session
        .client_actions
        .iter()
        .find(|action| matches!(action.kind, zess_protocol::ClientActionKind::Close { .. }))
        .unwrap()
        .id;
    session
        .close_client_buffer(action, buffer, None, true)
        .unwrap();
    assert!(!session.modified());
    assert!(!session.buffers.contains_key(&buffer));
}

#[test]
fn closing_the_last_file_opens_a_scratch_buffer() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "note");
    let mut session = fixture.session(&[path]);
    let buffer = session.views[&session.focus].location.buffer;
    session.command("q").unwrap();
    let action = session
        .client_actions
        .iter()
        .find(|action| matches!(action.kind, zess_protocol::ClientActionKind::Close { .. }))
        .unwrap()
        .id;
    session
        .close_client_buffer(action, buffer, None, false)
        .unwrap();
    assert!(!session.buffers.contains_key(&buffer));
    let current = session.views[&session.focus].location.buffer;
    assert!(
        matches!(&session.buffers[&current], Buffer::Text(text) if text.path().is_none() && !text.modified())
    );
}

#[test]
fn closing_a_buffer_replaces_it_in_every_split() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "note");
    let mut session = fixture.session(&[path]);
    let buffer = session.views[&session.focus].location.buffer;
    session.command("vsplit").unwrap();
    session.command("q").unwrap();
    let action = session
        .client_actions
        .iter()
        .find(|action| matches!(action.kind, zess_protocol::ClientActionKind::Close { .. }))
        .unwrap()
        .id;
    session
        .close_client_buffer(action, buffer, None, false)
        .unwrap();
    assert!(!session.buffers.contains_key(&buffer));
    let replacement = session.client_workspace.current.unwrap();
    let mut pages = Vec::new();
    session.layout.as_ref().unwrap().leaves(&mut pages);
    assert_eq!(pages.len(), 2);
    assert!(
        pages
            .into_iter()
            .all(|page| session.views[&session.views.main(page)].location.buffer == replacement)
    );
}
