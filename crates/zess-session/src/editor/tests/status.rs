use super::*;
use zess_plugin::StatusContext;

#[test]
fn layout_errors_keep_the_last_status_until_a_valid_reload() {
    let fixture = Fixture::new();
    let config = fixture.write(
        "config.w",
        r#"statusline-layout text
  right dynamic-part
register-statusline-component 'dynamic-part
  lambda : context
    if (get context 'modified #f)
      error "component failed"
      . "cached status"
"#,
    );
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.resize(80, 10);
    assert!(rendered(&mut session).contains("cached status"));
    for key in ["i", "x", "Esc"] {
        session.input(key);
    }
    assert!(rendered(&mut session).contains("cached status"));
    assert!(
        session.message.contains("component failed"),
        "{}",
        session.message
    );
    fs::write(
        &config,
        "statusline-layout text\n  right missing-component\n",
    )
    .unwrap();
    assert!(!session.reload());
    assert!(
        session
            .message
            .contains("unknown statusline component missing-component"),
        "{}",
        session.message
    );
    fs::write(
        &config,
        "statusline-layout text\n  right \"reloaded status\"\n",
    )
    .unwrap();
    assert!(session.reload(), "{}", session.message);
    assert!(rendered(&mut session).contains("reloaded status"));
    let Buffer::Text(text) = &session.buffers[&session.views[&session.focus].location.buffer]
    else {
        panic!("text buffer was replaced");
    };
    assert_eq!(text.text(), "x");
}

#[test]
fn layouts_fit_wide_and_narrow_views_and_an_empty_formatter_stays_hidden() {
    let fixture = Fixture::new();
    let config = fixture.write(
        "config.w",
        r#"configure-editor
  mouse #f
statusline-layout text
  left mode " " title modified loading
  center "中心"
  right
    join " · " position encoding
"#,
    );
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    for width in [1, 2, 3, 12, 80] {
        let (rows, _) = session.render_view(session.focus, width, 10);
        let spans = &rows.last().unwrap().spans;
        assert_eq!(
            spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                .sum::<usize>(),
            width
        );
        assert!(session.message.is_empty(), "{}", session.message);
        if width == 80 {
            let text = spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            assert!(text.contains("中心"), "{text}");
            assert!(text.ends_with("1:1 · UTF-8"), "{text}");
        }
    }
    fs::write(&config, "statusline-layout text\n  left \"should stay hidden\"\nconfigure-views 'text\n  statusline \"\"\n").unwrap();
    assert!(session.reload(), "{}", session.message);
    assert!(!rendered(&mut session).contains("should stay hidden"));
}

#[test]
fn global_status_formats_follow_the_current_config_after_reload() {
    let fixture = Fixture::new();
    let config = fixture.write(
        "config.w",
        "set-status-formats! : obj 'text \"A:{position}\"\n",
    );
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.resize(80, 10);
    assert!(rendered(&mut session).contains("A:1:1"));
    fs::write(
        &config,
        "set-status-formats! : obj 'text \"B:{selections}\"\n",
    )
    .unwrap();
    assert!(session.reload(), "{}", session.message);
    assert!(rendered(&mut session).contains("B:1"));
    fs::write(
        &config,
        "set-status-formats! : obj 'text \"changed\"\nset-status-formats! : obj 'pending \"{unknown}\"\n",
    )
    .unwrap();
    assert!(!session.reload());
    assert!(
        session.message.contains("unknown field {unknown}"),
        "{}",
        session.message
    );
    assert!(rendered(&mut session).contains("B:1"));
    fs::write(&config, "").unwrap();
    assert!(session.reload(), "{}", session.message);
    let frame = rendered(&mut session);
    assert!(frame.contains("1 sel  1:1  UTF-8"), "{frame}");
    assert!(!frame.contains("B:1"), "{frame}");
}

#[test]
fn file_status_formats_reload_without_changing_legacy_status_text() {
    let fixture = Fixture::new();
    fixture.write("note.txt", "content");
    let config = fixture.write("config.w", "");
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.resize(100, 12);
    session.command("files").unwrap();
    settle(&mut session);
    let id = session.focus;
    let view = session.views.get_mut(&id).unwrap();
    view.location.selection = ItemSelection {
        primary: Some("note.txt".into()),
        selected: vec!["note.txt".into()],
        anchor: Some("note.txt".into()),
    };
    session.command("files.delete").unwrap();
    session.command("files.toggle_ignored").unwrap();
    settle(&mut session);
    let view = &session.views[&id];
    assert_eq!(view.location.display.status, "1 pending");
    assert_eq!(
        view.location.display.status_data,
        json!({"files": {"pending_count": 1}})
    );
    let (rows, _) = session.render_view(id, 100, 10);
    let status = rows
        .last()
        .unwrap()
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(!status.contains("ignored"), "{status}");
    assert!(
        status.ends_with("1 pending · 1 selected / 2 items "),
        "{status}"
    );

    fs::write(&config, "set-view-options! 'files : obj 'status-formats : obj 'pending \"P:{count}\" 'items \"{selected}/{items}\"\n").unwrap();
    assert!(session.reload(), "{}", session.message);
    settle(&mut session);
    let (rows, _) = session.render_view(id, 100, 10);
    let status = rows
        .last()
        .unwrap()
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(status.ends_with("P:1 · 1/2 "), "{status}");
    assert_eq!(session.views[&id].location.display.status, "1 pending");

    fs::write(&config, r#"require "zess/status"
register-statusline "legacy"
  lambda : context
    statusline ""
      hash-ref context 'plugin_status
      status-format context 'pending : hash 'count : hash-ref (hash-ref (hash-ref context 'plugin_status_data) 'files) 'pending_count
set-view-options! 'files : obj 'statusline "legacy" 'status-formats : obj 'pending "queued={count}"
"#).unwrap();
    assert!(session.reload(), "{}", session.message);
    settle(&mut session);
    let (rows, _) = session.render_view(id, 100, 10);
    let status = rows
        .last()
        .unwrap()
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(status.contains("1 pending"), "{status}");
    assert!(status.ends_with("queued=1 "), "{status}");
}

#[test]
fn styled_regions_and_padding_inherit_the_current_status_background() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let id = session.focus;
    for active in [true, false] {
        let spans = session.statusline(
            id,
            "default",
            StatusContext {
                mode: Some("normal".into()),
                active,
                width: 24,
                plugin_status: json!({"git": {"additions": 123456, "deletions": 4}}),
                ..StatusContext::default()
            },
            24,
            active,
        );
        let base = if active { "status" } else { "status.inactive" };
        assert!(
            spans
                .iter()
                .all(|span| span.base_style.as_deref() == Some(base))
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "+123456" && span.style == "git.added")
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "-4" && span.style == "git.deleted")
        );
        assert_eq!(
            spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                .sum::<usize>(),
            24
        );
    }
}

#[test]
fn cached_status_spans_follow_focus_changes_when_the_formatter_is_unavailable() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let id = session.focus;
    session.statusline(
        id,
        "default",
        StatusContext {
            title: "cached".into(),
            active: true,
            width: 24,
            ..StatusContext::default()
        },
        24,
        true,
    );
    let spans = session.statusline(id, "missing", StatusContext::default(), 12, false);
    assert_eq!(session.message, "statusline: unknown statusline: missing");
    assert!(
        spans
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status.inactive"))
    );
    assert!(
        spans
            .iter()
            .any(|span| span.text == "cached" && span.style.is_empty())
    );
    session.scripts = None;
    let spans = session.statusline(id, "default", StatusContext::default(), 8, false);
    assert!(
        spans
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status.inactive"))
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum::<usize>(),
        8
    );
}

#[test]
fn explicit_status_roles_survive_focus_changes_in_cached_regions() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let id = session.focus;
    session.views.get_mut(&id).unwrap().location.statusline = vec![Span {
        text: "explicit".into(),
        style: "status".into(),
        base_style: Some("status".into()),
    }];
    session.scripts = None;
    let spans = session.statusline(id, "default", StatusContext::default(), 8, false);
    assert_eq!(spans[0].text, "explicit");
    assert_eq!(spans[0].style, "status");
    assert_eq!(spans[0].base_style.as_deref(), Some("status.inactive"));
}

#[test]
fn text_statusline_keeps_integer_positions_when_resized() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    let frame = session.frame();
    assert!(
        frame
            .rows
            .last()
            .unwrap()
            .spans
            .iter()
            .any(|span| span.text == "1 sel  1:1  UTF-8")
    );
    for width in [1, 2, 3, 80] {
        session.resize(width, 24);
        let frame = session.frame();
        assert_eq!(session.message, "");
        let length = frame
            .rows
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum::<usize>();
        assert_eq!(length, usize::from(frame.width));
    }
}
