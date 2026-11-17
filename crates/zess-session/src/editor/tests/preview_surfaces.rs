use super::*;

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
    let snapshot = session.snapshot();
    assert_eq!(snapshot.focused_view, Some(source));
    assert_eq!(snapshot.views.len(), 1);
    assert_eq!(snapshot.surfaces.len(), 1);
    assert_eq!(
        text_lines(&snapshot.surfaces[0].content)[0].text,
        "preview\n"
    );
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
        let surface = preview_surface_snapshot(&mut session, source);
        let lines = text_lines(&surface.content);
        let width = preview_surface(&session, source).viewport.width;
        assert_eq!(surface.soft_wrap, wrap);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].first_char, 0);
        assert_eq!(lines[0].text[..width], content[..width]);
        session.input("J");
        let surface = preview_surface_snapshot(&mut session, source);
        let line = &text_lines(&surface.content)[0];
        if wrap {
            assert_eq!(surface.scroll, 1);
            assert_eq!(line.first_char, width);
            assert_eq!(line.text[..width], content[width..width * 2]);
        } else {
            assert_eq!(surface.scroll, 0);
            assert_eq!(line.first_char, 0);
            assert_eq!(line.text, content[..width]);
        }
    }
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
    let help = session.focus;
    let help = view_snapshot(&mut session, help);
    let rows = item_rows(&help.content);
    for label in ["Files", "Open selected file"] {
        assert!(rows.iter().any(|row| row.text.contains(label)), "{rows:?}");
    }
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
    let surface = preview_surface_snapshot(&mut session, source);
    let line = &text_lines(&surface.content)[0];
    assert_eq!((surface.scroll, surface.horizontal_scroll), (3, 2));
    assert_eq!((line.line, line.first_column), (3, 2));
    assert!(line.text.starts_with("3 "));
    fs::write(fixture.0.join("b.txt"), "short").unwrap();
    session.input("R");
    settle(&mut session);
    let surface = preview_surface_snapshot(&mut session, source);
    assert_eq!((surface.scroll, surface.horizontal_scroll), (0, 0));
    assert_eq!(text_lines(&surface.content)[0].text, "short");
    assert_eq!(session.active_preview(source), Some(target));
    assert_eq!(session.focus, source);
    assert_eq!(session.views.values().count(), 1);
}
