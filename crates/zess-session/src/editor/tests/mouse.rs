use zess_ui::{MouseEvent, MouseKind};

use super::*;

fn gesture(session: &mut Session, frame: &Frame, kind: MouseKind, column: u16, row: u16) {
    session.mouse(frame.mouse_context, MouseEvent { kind, column, row });
}

#[test]
fn clicks_follow_rendered_graphemes_after_horizontal_scrolling() {
    let fixture = Fixture::new();
    let path = fixture.0.join("unicode.txt");
    fs::write(&path, "\t界a\u{301}🙂Z0123456789\n").unwrap();
    let mut session = fixture.session(&[path]);
    let id = session.focus;
    let buffer = session.views[&id].location.buffer;
    session.resize(10, 8);
    let frame = session.frame();
    assert_eq!(frame.rows[0].spans[0].text.chars().count(), 3);
    gesture(&mut session, &frame, MouseKind::Press, 8, 0);
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .selections(id)[0]
            .column,
        1
    );
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 9, 0);
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .selections(id)[0]
            .column,
        2
    );
    session
        .buffers
        .get_mut(&buffer)
        .and_then(Buffer::text)
        .unwrap()
        .scroll_view(id, 4, 0);
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 3, 0);
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .selections(id)[0]
            .column,
        1
    );
}

#[test]
fn dragging_extends_the_text_selection_until_release() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    let id = session.focus;
    let buffer = session.views[&id].location.buffer;
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 4, 0);
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Drag, 7, 0);
    let cursors = session
        .buffers
        .get_mut(&buffer)
        .and_then(Buffer::text)
        .unwrap()
        .selections(id);
    assert_eq!((cursors[0].anchor_column, cursors[0].column), (1, 4));
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Release, 7, 0);
    gesture(&mut session, &frame, MouseKind::Drag, 8, 0);
    let cursor = session
        .buffers
        .get_mut(&buffer)
        .and_then(Buffer::text)
        .unwrap()
        .selections(id)
        .remove(0);
    assert_eq!((cursor.anchor_column, cursor.column), (1, 4));
}

#[test]
fn wheel_scrolls_the_hovered_pane_without_moving_focus() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(
        &path,
        (0..50)
            .map(|line| format!("row {line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let mut session = fixture.session(&[path]);
    session.command("vsplit").unwrap();
    let focused = session.focus;
    let (RenderTarget::View(hovered), rect) = session
        .visible_rectangles()
        .into_iter()
        .find(|(_, rect)| rect.x == 0)
        .unwrap()
    else {
        panic!("expected text view")
    };
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::ScrollDown, 5, 0);
    session.frame();
    let (rows, _) = session.render_view(hovered, rect.width, rect.height);
    assert_eq!(
        rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "  4 row 3"
    );
    assert_eq!(session.focus, focused);
    gesture(&mut session, &frame, MouseKind::Press, 5, 0);
    assert_eq!(
        session.focus, focused,
        "stale row positions must not focus a pane"
    );
    // Multiple ticks from one displayed frame remain valid while the pane layout is stable.
    gesture(&mut session, &frame, MouseKind::ScrollDown, 5, 0);
    session.frame();
    let (rows, _) = session.render_view(hovered, rect.width, rect.height);
    assert_eq!(
        rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "  7 row 6"
    );
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 5, 0);
    assert_ne!(session.focus, focused);
}

#[test]
fn stale_coordinates_cannot_select_after_resize_or_reconnection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    let before = session.selection_snapshot();
    let old = session.frame();
    session.resize(100, 30);
    session.frame();
    gesture(&mut session, &old, MouseKind::Press, 6, 0);
    assert_eq!(session.selection_snapshot(), before);
    let old = session.frame();
    session.invalidate_mouse();
    session.frame();
    gesture(&mut session, &old, MouseKind::Press, 6, 0);
    assert_eq!(session.selection_snapshot(), before);
}

#[test]
fn item_dragging_selects_ids_across_complete_rows() {
    let fixture = Fixture::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(fixture.0.join(name), "").unwrap();
    }
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let id = session.focus;
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(id))
        .unwrap();
    let (x, y) = (region.body.x as u16, region.body.y as u16);
    let ids = match &region.content {
        super::super::mouse::Content::Items { ids } => ids.clone(),
        _ => panic!("file list has no items"),
    };
    assert!(ids.len() >= 3);
    gesture(&mut session, &frame, MouseKind::Press, x + 20, y);
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Drag, x + 2, y + 2);
    assert_eq!(session.views[&id].location.selection.selected, ids[..3]);
    assert_eq!(
        session.views[&id].location.selection.primary.as_ref(),
        ids.get(2)
    );
}

#[test]
fn search_prompts_block_clicks_on_the_underlying_document() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    let before = session.selection_snapshot();
    session.input("/");
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 6, 0);
    assert_eq!(session.selection_snapshot(), before);
    assert!(session.active_prompt().is_some());
}

#[test]
fn configuration_reload_updates_mouse_settings_and_preserves_the_view() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(
        &path,
        (0..30)
            .map(|line| format!("row {line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-editor-options! : obj 'mouse #f 'scroll-lines 5\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[path], Some(config.clone())).unwrap();
    let id = session.focus;
    let frame = session.frame();
    assert!(!frame.mouse_enabled);
    let before = session.selection_snapshot();
    gesture(&mut session, &frame, MouseKind::Press, 5, 0);
    assert_eq!(session.selection_snapshot(), before);
    fs::write(
        &config,
        "set-editor-options! : obj 'mouse #t 'scroll-lines 5\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    let frame = session.frame();
    assert!(frame.mouse_enabled);
    assert_eq!(session.focus, id);
    gesture(&mut session, &frame, MouseKind::ScrollDown, 5, 0);
    let (rows, _) = session.render_view(id, 80, 23);
    assert_eq!(
        rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "  6 row 5"
    );
    fs::write(&config, "set-editor-options! : obj 'mouse 3\n").unwrap();
    session.command("config-reload").unwrap();
    assert_eq!(
        session.message,
        format!(
            "reload failed: load plugin {config}: {config}:1:1: Error: TypeMismatch: mouse must be a boolean",
            config = config.display()
        )
    );
    assert!(session.frame().mouse_enabled);
    assert_eq!(session.focus, id);
}

#[test]
fn unavailable_plugin_runtime_keeps_mouse_selection_available() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.scripts = None;
    let frame = session.frame();
    assert!(frame.mouse_enabled);
    gesture(&mut session, &frame, MouseKind::Press, 5, 0);
    let id = session.focus;
    let buffer = session.views[&id].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .selections(id)[0]
            .column,
        2
    );
}

#[test]
fn clicks_in_insert_mode_place_text_at_the_clicked_character() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.input("i");
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 5, 0);
    session.input("X");
    let id = session.focus;
    let buffer = session.views[&id].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .text(),
        "abXcdef\n"
    );
}

#[test]
fn headers_gutters_and_status_lines_are_excluded_from_text_selection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    let id = session.focus;
    session.views.get_mut(&id).unwrap().location.display.header = "Header".into();
    let before = session.selection_snapshot();
    let frame = session.frame();
    for (column, row) in [(5, 0), (0, 1), (5, frame.height - 2)] {
        gesture(&mut session, &frame, MouseKind::Press, column, row);
        assert_eq!(session.selection_snapshot(), before);
    }
    gesture(&mut session, &frame, MouseKind::Press, 5, 1);
    let buffer = session.views[&id].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .selections(id)[0]
            .column,
        2
    );
}

#[test]
fn double_clicks_expand_and_collapse_the_clicked_directory() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join("folder/child.txt"), "child\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let id = session.focus;
    for expanded in [true, false] {
        let frame = session.frame();
        let region = session
            .mouse_regions
            .iter()
            .find(|region| region.target == RenderTarget::View(id))
            .unwrap();
        let super::super::mouse::Content::Items { ids } = &region.content else {
            panic!("file tree has no item targets");
        };
        let row = ids.iter().position(|id| id == "folder").unwrap();
        let (x, y) = (region.body.x as u16, (region.body.y + row) as u16);
        gesture(&mut session, &frame, MouseKind::Press, x, y);
        assert_eq!(
            session.views[&id]
                .item_ids()
                .contains(&"folder/child.txt".to_owned()),
            !expanded
        );
        let frame = session.frame();
        gesture(&mut session, &frame, MouseKind::DoubleClick, x, y);
        settle(&mut session);
        assert_eq!(
            session.views[&id]
                .item_ids()
                .contains(&"folder/child.txt".to_owned()),
            expanded
        );
        assert_eq!(
            session.views[&id].location.selection.primary.as_deref(),
            Some("folder")
        );
        assert_eq!(session.focus, id);
    }
}

#[test]
fn double_clicking_a_file_selects_it_without_opening_an_editor() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "note\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let id = session.focus;
    let buffer = session.views[&id].location.buffer;
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(id))
        .unwrap();
    let super::super::mouse::Content::Items { ids } = &region.content else {
        unreachable!()
    };
    let row = ids.iter().position(|id| id == "note.txt").unwrap();
    let (x, y) = (region.body.x as u16, (region.body.y + row) as u16);
    gesture(&mut session, &frame, MouseKind::DoubleClick, x, y);
    assert_eq!(session.focus, id);
    assert_eq!(session.views[&id].location.buffer, buffer);
    assert_eq!(
        session.views[&id].location.selection.primary.as_deref(),
        Some("note.txt")
    );
}

#[test]
fn double_clicks_without_a_command_keep_text_selection_and_dragging() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    let id = session.focus;
    let buffer = session.views[&id].location.buffer;
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::DoubleClick, 4, 0);
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Drag, 7, 0);
    let cursors = session
        .buffers
        .get_mut(&buffer)
        .and_then(Buffer::text)
        .unwrap()
        .selections(id);
    assert_eq!((cursors[0].anchor_column, cursors[0].column), (1, 4));
}

#[test]
fn double_clicks_run_the_clicked_views_command() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.command("vsplit").unwrap();
    let focused = session.focus;
    let clicked = session
        .visible_rectangles()
        .into_iter()
        .filter_map(|(target, _)| match target {
            RenderTarget::View(id) => Some(id),
            _ => None,
        })
        .find(|id| *id != focused)
        .unwrap();
    session.views.get_mut(&clicked).unwrap().location.options["double-click-command"] =
        json!("text.move-right");
    let buffer = session.views[&clicked].location.buffer;
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(clicked))
        .unwrap();
    let (x, y) = (region.body.x as u16, region.body.y as u16);
    gesture(&mut session, &frame, MouseKind::DoubleClick, x, y);
    assert_eq!(session.focus, clicked);
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .selections(clicked)[0]
            .column,
        1
    );
}

#[test]
fn stale_double_clicks_cannot_change_selection_or_run_commands() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    let id = session.focus;
    session.views.get_mut(&id).unwrap().location.options["double-click-command"] =
        json!("text.move-right");
    let before = session.selection_snapshot();
    let frame = session.frame();
    session.invalidate_mouse();
    session.frame();
    gesture(&mut session, &frame, MouseKind::DoubleClick, 5, 0);
    assert_eq!(session.selection_snapshot(), before);
}

#[test]
fn narrow_preview_rows_keep_joined_emoji_whole() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session
        .show_preview(
            session.views.page_id(source),
            "folder".into(),
            PreviewContent::Directory {
                column_alignment: Default::default(),
                rows: vec![DisplayRow {
                    text: "a👩‍👩‍👧‍👦bcdef".into(),
                    ..DisplayRow::default()
                }],
            },
            false,
        )
        .unwrap();
    let preview = session.preview_target(source).unwrap();
    let (rows, _) = session.render_view(preview, 7, 3);
    assert_eq!(
        rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "a👩‍👩‍👧‍👦bcde"
    );
}

#[test]
fn clicking_a_command_candidate_selects_it_until_enter() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(25, 8);
    session.input(":");
    let candidates = session.command_candidates("");
    let target = candidates
        .iter()
        .position(|candidate| candidate.name == "help")
        .unwrap();
    assert!(target > 6);
    for _ in 0..=target {
        session.input("Tab");
    }
    assert_ne!(session.prompt.as_ref().unwrap().text, "help");
    let frame = session.frame();
    let row = frame.rows.len() - 2;
    assert_eq!(frame.rows[row].spans[0].style, "popup");
    assert_eq!(frame.rows[row].spans[0].text, "help");
    assert_eq!(
        frame.rows[row]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "help"
    );
    gesture(&mut session, &frame, MouseKind::Press, 1, row as u16);
    assert!(session.help.is_none());
    assert_eq!(session.prompt.as_ref().unwrap().text, "help");
    session.input("Enter");
    assert!(session.help.is_some());
}

#[test]
fn clicking_outside_command_candidates_cancels_and_selects_the_text() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.resize(80, 24);
    session.input(":");
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 5, 0);
    assert!(session.active_prompt().is_none());
    assert!(session.pending.is_empty());
    let id = session.focus;
    let buffer = session.views[&id].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .unwrap()
            .selections(id)[0]
            .column,
        2
    );
}

#[test]
fn clicks_on_empty_command_grid_space_do_not_select_a_candidate() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input(":");
    for key in "help".chars() {
        session.input(&key.to_string());
    }
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 79, 22);
    assert_eq!(session.prompt.as_ref().unwrap().text, "help");
    assert!(session.prompt.as_ref().unwrap().completion.is_none());
}

#[test]
fn clicking_outside_an_empty_command_popup_cancels_the_prompt() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input(":");
    for key in "zzzzzzzz".chars() {
        session.input(&key.to_string());
    }
    let frame = session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 5, 0);
    assert!(session.active_prompt().is_none());
}

#[test]
fn clicking_alias_documentation_keeps_the_command_prompt_open() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "register-command-alias \"语言\" \"set-language\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.resize(80, 24);
    session.input(":");
    session.input("语");
    let frame = session.frame();
    let row = frame.rows.len() - 2;
    assert_eq!(frame.rows[row].spans.len(), 1);
    assert_eq!(frame.rows[row].spans[0].style, "popup.secondary");
    assert_eq!(
        frame.rows[row].spans[0].text.trim_end(),
        "Aliases: lang, 语言"
    );
    gesture(&mut session, &frame, MouseKind::Press, 1, row as u16);
    assert_eq!(session.prompt.as_ref().unwrap().text, "语");
    assert!(session.prompt.as_ref().unwrap().completion.is_none());
}

#[test]
fn stale_command_popup_clicks_cannot_select_new_candidates() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input(":");
    let old = session.frame();
    session.input("h");
    session.frame();
    gesture(&mut session, &old, MouseKind::Press, 1, 22);
    assert_eq!(session.prompt.as_ref().unwrap().text, "h");
    assert!(session.prompt.as_ref().unwrap().completion.is_none());
}

#[test]
fn clicks_from_an_unchanged_command_popup_remain_valid() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input(":");
    for key in "hlp".chars() {
        session.input(&key.to_string());
    }
    let frame = session.frame();
    session.frame();
    gesture(&mut session, &frame, MouseKind::Press, 1, 22);
    assert_eq!(session.prompt.as_ref().unwrap().text, "help");
}

#[test]
fn clicking_the_command_input_keeps_the_prompt_open() {
    let fixture = Fixture::new();
    for input in ["help", "zzzzzzzz", "help argument"] {
        let mut session = fixture.session(&[]);
        session.resize(80, 24);
        session.input(":");
        for key in input.chars() {
            session.input(&key.to_string());
        }
        let frame = session.frame();
        assert_eq!(frame.prompt, format!(":{input}"));
        gesture(&mut session, &frame, MouseKind::Press, 1, 23);
        assert_eq!(session.active_prompt().unwrap().text, input);
    }
}
