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
    session.frame();
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
fn clicking_an_item_loads_its_preview_without_waiting() {
    let fixture = Fixture::new();
    let mut session = delayed_file_browser(&fixture);
    let source = session.focus;
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(source))
        .unwrap();
    let column = region.body.x as u16;
    let row = (region.body.y + 2) as u16;
    session.mouse(
        frame.mouse_context,
        zess_ui::MouseEvent {
            kind: zess_ui::MouseKind::Press,
            column,
            row,
        },
    );
    wait_until(&mut session, |session| {
        matches!(&session.buffers[&preview_buffer(session, source)],
            Buffer::PreviewText { text, .. } if text.text() == "c")
    });
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
