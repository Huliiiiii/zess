use super::*;

fn delayed_file_browser(fixture: &Fixture) -> Session {
    let workspace = fixture.0.join("workspace");
    fs::create_dir(&workspace).unwrap();
    for name in ["a", "b", "c"] {
        fs::write(workspace.join(name), name).unwrap();
    }
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 60000\nset-view-options! \"files\" : obj 'git-status #f\n",
    )
    .unwrap();
    let mut session = Session::new(workspace, &[], Some(config)).unwrap();
    session.resize(100, 24);
    session.command("files").unwrap();
    wait_until(&mut session, |session| {
        session.views[&session.focus].item_ids().len() == 3
    });
    session.snapshot();
    session
}

#[test]
fn item_jumps_load_the_selected_preview_without_waiting() {
    for (keys, expected) in [
        (vec!["g", "g"], "a"),
        (vec!["g", "e"], "c"),
        (vec!["2", "j"], "c"),
        (vec!["1", "j"], "b"),
        (vec!["C-d"], "c"),
        (vec!["g", "w", "a", "b"], "b"),
    ] {
        let fixture = Fixture::new();
        let mut session = delayed_file_browser(&fixture);
        let source = session.focus;
        for key in keys {
            session.input(key);
        }
        wait_until(&mut session, |session| {
            matches!(&session.buffers[&preview_buffer(session, source)],
                Buffer::PreviewText { text, .. } if text.text() == expected)
        });
    }
}

#[test]
fn single_item_navigation_restarts_the_delay_at_the_boundary() {
    let fixture = Fixture::new();
    let mut session = delayed_file_browser(&fixture);
    let source = session.focus;
    session.input("j");
    wait_until(&mut session, |session| {
        matches!(&session.buffers[&preview_buffer(session, source)],
            Buffer::PreviewText { text, .. } if text.text() == "b")
    });
    let previous = preview_buffer(&session, source);
    for (key, expected) in [("j", "c"), ("j", "c"), ("k", "b"), ("k", "a"), ("k", "a")] {
        let before = session.views[&source].wake_at;
        session.input(key);
        assert_eq!(
            session.views[&source].location.selection.primary.as_deref(),
            Some(expected)
        );
        if expected != "b" {
            let after = session.views[&source].wake_at.unwrap();
            if let Some(before) = before {
                assert!(after > before);
            }
            assert!(after > Instant::now() + Duration::from_secs(50));
        }
        assert_eq!(preview_buffer(&session, source), previous);
        assert!(matches!(&session.buffers[&previous],
            Buffer::PreviewText { text, .. } if text.text() == "b"));
        assert_eq!(
            session
                .services
                .borrow_mut()
                .call("service.stats", Value::Null)["jobs"],
            0
        );
    }
}

#[test]
fn preview_debounce_configuration_survives_failed_reload() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 275\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    assert_eq!(
        session
            .scripts
            .as_ref()
            .unwrap()
            .editor_options()
            .preview_debounce,
        Duration::from_millis(275)
    );

    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 0\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    assert_eq!(
        session
            .scripts
            .as_ref()
            .unwrap()
            .editor_options()
            .preview_debounce,
        Duration::ZERO
    );

    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms -1\n",
    )
    .unwrap();
    let error = session.command("config-reload").unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        format!(
            "reload failed: load plugin {path}: {path}:1:1: Error: TypeMismatch: preview-debounce-ms must be a non-negative integer",
            path = config.display()
        )
    );
    assert_eq!(
        session
            .scripts
            .as_ref()
            .unwrap()
            .editor_options()
            .preview_debounce,
        Duration::ZERO
    );
}

#[test]
fn configuration_reload_publishes_mouse_settings_and_preserves_the_view() {
    use zess_protocol::{CommandAction, CommandOutcome, CommandTarget};

    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "content").unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-editor-options! : obj 'mouse #f 'scroll-lines 5\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[path], Some(config.clone())).unwrap();
    let id = session.focus;
    let mut client = session.snapshot();
    assert_eq!(client.workspace_root, fixture.0.to_string_lossy());
    assert!(!client.editor_options.mouse);
    fs::write(
        &config,
        "set-editor-options! : obj 'mouse #t 'scroll-lines 5\nset-view-options! 'text : obj 'header-style \"keyword\"\n",
    )
    .unwrap();
    let outcome = session.begin_operation(
        "valid-config",
        &CommandAction::CommandLine {
            text: "config-reload".into(),
        },
    );
    assert_eq!(
        outcome,
        Some(CommandOutcome::Message {
            message: "user plugins reloaded".into()
        })
    );
    let updated = session.snapshot();
    zess_protocol::SessionDelta::between(&client, &updated).apply(&mut client);
    assert!(client.editor_options.mouse);
    assert_eq!(client.editor_options.scroll_lines, 5);
    assert_eq!(client.text_options["header-style"], "keyword");
    assert_eq!(session.focus, id);
    assert!(updated.status.is_empty());
    fs::write(
        &config,
        "set-editor-options! : obj 'scroll-lines 6\nset-editor-options! : obj 'mouse 3\n",
    )
    .unwrap();
    let target = CommandTarget::Document {
        buffer_id: client.workspace.current.unwrap(),
    };
    let actions = [
        CommandAction::Request {
            target: target.clone(),
            request: HostRequest::Action {
                name: "config-reload".into(),
                argument: Value::Null,
            },
        },
        CommandAction::CommandLine {
            text: "config-reload".into(),
        },
        CommandAction::Command {
            name: "config-reload".into(),
            argument: Value::Null,
        },
        CommandAction::Invoke {
            target,
            name: "config-reload".into(),
            argument: Value::Null,
        },
    ];
    let status = format!(
        "reload failed: load plugin {config}: {config}:2:1: Error: TypeMismatch: mouse must be a boolean",
        config = config.display()
    );
    let outcomes = actions.map(|action| {
        let outcome = session.begin_operation("invalid-config", &action);
        let failed = session.snapshot();
        assert_eq!(failed.editor_options, client.editor_options);
        assert_eq!(failed.text_options, client.text_options);
        assert_eq!(failed.focused_view, Some(id));
        assert_ne!(failed.status, "user plugins reloaded");
        outcome
    });
    assert_eq!(
        outcomes,
        std::array::from_fn(|_| Some(CommandOutcome::Failed {
            message: status.clone()
        }))
    );
}

#[test]
fn reload_reports_hook_failure_after_installing_the_configuration() {
    use zess_protocol::{CommandAction, CommandOutcome};

    let fixture = Fixture::new();
    let config = fixture.write("config.w", "");
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    fs::write(
        &config,
        "set-editor-options! : obj 'mouse #f\nregister-event \"reload\"\n  lambda : argument\n    error \"hook rejected configuration\"\n",
    )
    .unwrap();
    assert!(matches!(
        session.begin_operation("failed-hook", &CommandAction::CommandLine { text: "config-reload".into() }),
        Some(CommandOutcome::Failed { message }) if message.starts_with("reload hook failed after installing the configuration") && message.contains("hook rejected configuration")
    ));
    let state = session.snapshot();
    assert!(!state.editor_options.mouse);
    assert_ne!(state.status, "user plugins reloaded");
}
