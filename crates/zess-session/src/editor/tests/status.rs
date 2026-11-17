use super::*;

#[test]
fn a_status_component_can_fail_and_be_replaced_without_losing_buffer_edits() {
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
    let id = session.focus;
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.right[0].text, "cached status");
    for key in ["i", "x", "Esc"] {
        session.input(key);
    }
    assert!(view_snapshot(&mut session, id).statusline.is_none());
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
    let error = session.command("config-reload").unwrap_err();
    assert!(
        format!("{error:#}").contains("unknown statusline component missing-component"),
        "{error:#}"
    );
    fs::write(
        &config,
        "statusline-layout text\n  right \"reloaded status\"\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.right[0].text, "reloaded status");
    let Buffer::Text(text) = &session.buffers[&session.views[&session.focus].location.buffer]
    else {
        panic!("text buffer was replaced");
    };
    assert_eq!(text.text(), "x");
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
    let id = session.focus;
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.right[0].text, "A:1:1");
    fs::write(
        &config,
        "set-status-formats! : obj 'text \"B:{selections}\"\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.right[0].text, "B:1");
    fs::write(
        &config,
        "set-status-formats! : obj 'text \"changed\"\nset-status-formats! : obj 'pending \"{unknown}\"\n",
    )
    .unwrap();
    let error = session.command("config-reload").unwrap_err();
    assert!(
        format!("{error:#}").contains("unknown field {unknown}"),
        "{error:#}"
    );
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.right[0].text, "B:1");
    fs::write(&config, "").unwrap();
    session.command("config-reload").unwrap();
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.right[0].text, "1 sel  1:1  UTF-8");
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
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert!(
        [&line.left, &line.center, &line.right]
            .into_iter()
            .flatten()
            .all(|span| !span.text.contains("ignored"))
    );
    assert_eq!(line.right[0].text, "1 pending · 1 selected / 2 items");
    assert!(line.left.iter().all(|span| !span.text.contains("[+]")));

    fs::write(&config, "set-view-options! 'files : obj 'status-formats : obj 'pending \"P:{count}\" 'items \"{selected}/{items}\"\n").unwrap();
    session.command("config-reload").unwrap();
    settle(&mut session);
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.right[0].text, "P:1 · 1/2");
    assert_eq!(session.views[&id].location.display.status, "1 pending");

    fs::write(&config, r#"require "zess/status"
register-statusline "legacy"
  lambda : context
    statusline ""
      hash-ref context 'plugin_status
      status-format context 'pending : hash 'count : hash-ref (hash-ref (hash-ref context 'plugin_status_data) 'files) 'pending_count
set-view-options! 'files : obj 'statusline "legacy" 'status-formats : obj 'pending "queued={count}"
"#).unwrap();
    session.command("config-reload").unwrap();
    settle(&mut session);
    let line = view_snapshot(&mut session, id).statusline.unwrap();
    assert_eq!(line.center[0].text, "1 pending");
    assert_eq!(line.right[0].text, "queued=1");
}
