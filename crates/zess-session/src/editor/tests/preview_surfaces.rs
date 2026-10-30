use zess_ui::{MouseEvent, MouseKind};

use super::*;

fn mouse(session: &mut Session, kind: MouseKind, rect: Rect, row: usize) {
    let frame = session.frame();
    session.mouse(
        frame.mouse_context,
        MouseEvent {
            kind,
            column: rect.x as u16,
            row: (rect.y + row) as u16,
        },
    );
}

#[test]
fn file_previews_keep_keyboard_focus_in_the_browser() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "preview\n".repeat(60)).unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let selection = session.selection_snapshot();
    let target = session.active_preview(source).unwrap();
    assert!(matches!(target, RenderTarget::Surface { .. }));
    assert_eq!(session.views.values().count(), 1);
    for command in [
        "files.preview",
        "preview.focus",
        "focus-right",
        "focus-left",
        "focus-next",
    ] {
        session.execute_command(command, json!({})).unwrap();
        settle(&mut session);
        assert_eq!(session.focus, source, "{command}");
    }
    for key in ["h", "j", "k", "l", "w"] {
        session.input("C-w");
        session.input(key);
        assert_eq!(session.focus, source, "C-w {key}");
    }
    session.input("J");
    assert_eq!(preview_surface(&session, source).viewport.scroll, 1);
    session.input("K");
    assert_eq!(preview_surface(&session, source).viewport.scroll, 0);
    assert_eq!(session.selection_snapshot(), selection);
    let rows = preview_rows(&mut session, source);
    assert_eq!(rows[0].spans[0].text, "preview");
    assert!(rows.iter().flat_map(|row| &row.spans).all(|span| !matches!(
        span.style.as_str(),
        "selection" | "cursor" | "linenr" | "status.normal"
    )));
    assert_eq!(session.active_preview(source), Some(target));
    assert_eq!(session.views.values().count(), 1);
    session.input("Enter");
    assert_eq!(session.focus, source);
    assert!(matches!(
        session.buffers[&session.views[&source].location.buffer],
        Buffer::Text(_)
    ));
    assert!(session.active_preview(source).is_none());
}

#[test]
fn file_preview_wrap_is_enabled_by_default_and_can_be_disabled() {
    let fixture = Fixture::new();
    let content = "abcdefghijklmnopqrstuvwxyz".repeat(30);
    fs::write(fixture.0.join("note.txt"), &content).unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-view-options! \"files\" : obj 'preview-wrap #f\n",
    )
    .unwrap();
    for (config, wrap) in [(None, true), (Some(config.clone()), false)] {
        let mut session = Session::new(fixture.0.clone(), &[], config).unwrap();
        session.resize(80, 10);
        session.command("files").unwrap();
        settle(&mut session);
        let source = session.focus;
        session
            .dispatch(source, "navigate", json!({"path":"note.txt"}))
            .unwrap();
        settle(&mut session);
        let rows = preview_rows(&mut session, source);
        let width = preview_surface(&session, source).viewport.width;
        let text = |row: &FrameRow| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        };
        assert_eq!(text(&rows[0]), content[..width]);
        if wrap {
            assert_eq!(text(&rows[1]), content[width..width * 2]);
            session.input("J");
            assert_eq!(preview_surface(&session, source).viewport.scroll, 1);
            let rows = preview_rows(&mut session, source);
            assert_eq!(text(&rows[0]), content[width..width * 2]);
        } else {
            assert!(rows[1].spans.is_empty());
            session.input("J");
            assert_eq!(preview_surface(&session, source).viewport.scroll, 0);
        }
    }
}

#[test]
fn surface_mouse_events_belong_to_their_page_and_leave_file_selection_unchanged() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "first\n".repeat(60)).unwrap();
    fs::write(fixture.0.join("b.txt"), "second").unwrap();
    let mut session = fixture.session(&[]);
    session.resize(160, 16);
    session.command("files").unwrap();
    settle(&mut session);
    let left = session.focus;
    let left_page = session.views.page_id(left);
    session.command("vsplit").unwrap();
    settle(&mut session);
    let right = session.focus;
    let selection = serde_json::to_value(&session.views[&left].location.selection).unwrap();
    session.frame();
    let rect = session.mouse_regions.iter().find(|region| matches!(&region.target, RenderTarget::Surface { page, .. } if *page == left_page)).unwrap().body;
    mouse(&mut session, MouseKind::ScrollDown, rect, 0);
    assert_eq!(session.focus, right);
    assert_eq!(preview_surface(&session, left).viewport.scroll, 3);
    assert_eq!(preview_surface(&session, right).viewport.scroll, 0);
    mouse(&mut session, MouseKind::Press, rect, 1);
    assert_eq!(session.focus, left);
    mouse(&mut session, MouseKind::Drag, rect, 4);
    mouse(&mut session, MouseKind::DoubleClick, rect, 2);
    assert_eq!(session.views.values().count(), 2);
    assert_eq!(
        serde_json::to_value(&session.views[&left].location.selection).unwrap(),
        selection
    );
    assert_eq!(session.views[&left].layer, None);
    let frame = session.frame();
    session.mouse(
        frame.mouse_context,
        MouseEvent {
            kind: MouseKind::Press,
            column: u16::MAX,
            row: u16::MAX,
        },
    );
    assert_eq!(session.focus, left);
    assert_eq!(session.views.values().count(), 2);
    let main = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(left))
        .unwrap()
        .body;
    mouse(&mut session, MouseKind::Press, main, 1);
    settle(&mut session);
    assert_eq!(selected_row(&session, left).id, "b.txt");
    assert_eq!(preview_text(&session, left), "second");
    assert_eq!(preview_text(&session, right), "first\n".repeat(60));
}

#[test]
fn file_preview_help_uses_the_browser_context() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let target = session.active_preview(source).unwrap();
    session
        .execute_command("preview.focus", serde_json::json!({}))
        .unwrap();
    session.command("help").unwrap();
    let help = rendered(&mut session);
    assert!(help.contains("Files"), "{help}");
    assert!(help.contains("Open selected file"), "{help}");
    session.input("Esc");
    assert_eq!(session.focus, source);
    assert_eq!(session.active_preview(source), Some(target));
}

#[test]
fn file_layers_preserve_the_browser_surface_until_the_layer_closes() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let target = session.active_preview(source).unwrap();
    let buffer = preview_buffer(&session, source);
    session.input("e");
    let layer = session.focus;
    assert_ne!(layer, source);
    assert!(session.views.references_buffer(buffer));
    for command in ["focus-right", "focus-next"] {
        session.command(command).unwrap();
        assert_eq!(session.focus, layer);
    }
    session.command("q").unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(session.active_preview(source), Some(target));
    assert_eq!(preview_text(&session, source), "content");
    session.command("new").unwrap();
    assert!(!session.buffers.contains_key(&buffer));
    assert_eq!(session.views.values().count(), 1);
}

#[test]
fn focus_requests_choose_an_interactive_pane_in_mixed_preview_groups() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"
require "zess/buffer"
require "zess/ui"
register-command "mixed" "Show a surface and an interactive preview"
  lambda : argument
    request-preview-group
      list
        preview-surface "passive" "passive.txt" : preview-text "passive"
        preview-pane "interactive" "interactive.txt" : preview-text "interactive"
      preview-layout-rows
      . #t
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    let source = session.focus;
    session.command("mixed").unwrap();
    let interactive = session.focus;
    assert_ne!(interactive, source);
    assert_eq!(session.views.values().count(), 2);
    assert_eq!(
        session.views.page(source).previews.as_ref().unwrap().active,
        "interactive"
    );
    let surface = session
        .views
        .surface(session.views.page_id(source), "passive")
        .unwrap();
    assert!(session.views.references_buffer(surface.buffer));
    session
        .execute_command("preview.return", serde_json::json!({}))
        .unwrap();
    assert_eq!(session.focus, source);
    session
        .execute_command("preview.focus", serde_json::json!({}))
        .unwrap();
    assert_eq!(session.focus, interactive);
}

#[test]
fn unwrapped_previews_preserve_their_viewport_until_replacement_contents_shrink() {
    let fixture = Fixture::new();
    let content = |label| {
        (0..70)
            .map(|index| format!("{label}{index:02} {}\n", "x".repeat(100)))
            .collect::<String>()
    };
    fs::write(fixture.0.join("a.txt"), content("a")).unwrap();
    fs::write(fixture.0.join("b.txt"), content("b")).unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-view-options! \"files\" : obj 'preview-wrap #f\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let target = session.active_preview(source).unwrap();
    let buffer = preview_buffer(&session, source);
    for key in ["J", "J", "J", "L", "L"] {
        session.input(key);
    }
    assert_eq!(preview_surface(&session, source).viewport.scroll, 3);
    assert_eq!(
        preview_surface(&session, source).viewport.horizontal_scroll,
        2
    );
    session.input("j");
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "b.txt");
    assert_eq!(session.active_preview(source), Some(target.clone()));
    assert_eq!(preview_buffer(&session, source), buffer);
    let rows = preview_rows(&mut session, source);
    assert!(
        rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .starts_with("3 ")
    );
    assert_eq!(preview_surface(&session, source).viewport.scroll, 3);
    assert_eq!(
        preview_surface(&session, source).viewport.horizontal_scroll,
        2
    );
    fs::write(fixture.0.join("b.txt"), "short").unwrap();
    session.input("R");
    settle(&mut session);
    assert_eq!(preview_rows(&mut session, source)[0].spans[0].text, "short");
    assert_eq!(preview_surface(&session, source).viewport.scroll, 0);
    assert_eq!(
        preview_surface(&session, source).viewport.horizontal_scroll,
        0
    );
    assert_eq!(session.active_preview(source), Some(target));
    assert_eq!(session.focus, source);
    assert_eq!(session.views.values().count(), 1);
}

#[test]
fn consecutive_wheel_events_from_one_displayed_frame_keep_scrolling_the_surface() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "preview\n".repeat(60)).unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let selection = session.selection_snapshot();
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| matches!(region.target, RenderTarget::Surface { .. }))
        .unwrap()
        .body;
    for expected in [3, 6, 9] {
        session.mouse(
            frame.mouse_context,
            MouseEvent {
                kind: MouseKind::ScrollDown,
                column: region.x as u16,
                row: region.y as u16,
            },
        );
        let next = session.frame();
        assert_eq!(preview_surface(&session, source).viewport.scroll, expected);
        assert_eq!(
            next.mouse_context.layout_revision,
            frame.mouse_context.layout_revision
        );
        assert_eq!(session.focus, source);
        assert_eq!(session.selection_snapshot(), selection);
    }
}

#[test]
fn clicking_a_surface_finishes_pending_browser_input_before_the_next_key() {
    let fixture = Fixture::new();
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        fs::write(fixture.0.join(name), "preview").unwrap();
    }
    for double_click in [false, true] {
        let mut session = fixture.session(&[]);
        session.command("files").unwrap();
        settle(&mut session);
        let source = session.focus;
        for prefix in [&["C-w"][..], &["3"][..], &["\""][..], &["g", "w"][..]] {
            session
                .execute_command("data.goto-first", serde_json::json!({}))
                .unwrap();
            settle(&mut session);
            assert_eq!(selected_row(&session, source).id, "a.txt");
            for key in prefix {
                session.input(key);
            }
            assert!(
                !session.pending.is_empty()
                    || !session.views[&source].count.is_empty()
                    || !matches!(session.views[&source].register, RegisterInput::None)
                    || session.views[&source].jump.is_some(),
                "{prefix:?}"
            );
            let frame = session.frame();
            let region = session
                .mouse_regions
                .iter()
                .find(|region| matches!(region.target, RenderTarget::Surface { .. }))
                .unwrap()
                .body;
            session.mouse(
                frame.mouse_context,
                MouseEvent {
                    kind: if double_click {
                        MouseKind::DoubleClick
                    } else {
                        MouseKind::Press
                    },
                    column: region.x as u16,
                    row: region.y as u16,
                },
            );
            assert_eq!(selected_row(&session, source).id, "a.txt");
            session.input("j");
            assert_eq!(
                selected_row(&session, source).id,
                "b.txt",
                "double click: {double_click}; prefix: {prefix:?}"
            );
            assert_eq!(session.focus, source);
        }
    }
}
