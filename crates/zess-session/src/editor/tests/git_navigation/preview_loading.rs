use super::*;

#[test]
fn reading_a_refreshed_diff_focuses_the_preview_when_it_is_published() {
    let fixture = nested_changes();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["g", "e"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(
        session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .len(),
        2
    );

    session.command("git.refresh").unwrap();
    session.command("git.preview").unwrap();
    settle(&mut session);

    assert!(
        session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .iter()
            .any(|member| member.view().unwrap().id == session.focus)
    );
    session.input("Esc");
    assert_eq!(session.focus, source);
    session.dispatch(source, "tick", Value::Null).unwrap();
    assert_eq!(session.focus, source);
}

#[test]
fn continuous_file_navigation_preserves_the_entire_previous_diff_group() {
    let fixture = nested_changes();
    let config = fixture.0.join(".git/preview-config.w");
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 0\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    for key in ["g", "e"] {
        session.input(key);
    }
    settle(&mut session);
    let source = session.focus;
    session
        .request(
            source,
            HostRequest::ConfigureView {
                options: json!({"git-view":"flat"}),
            },
        )
        .unwrap();
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "file:dir/modified.txt");
    assert_eq!(
        session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .len(),
        2
    );
    let previous = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .map(|member| {
            let buffer = session.views[&member.view().unwrap().id].location.buffer;
            let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
                panic!("expected a diff preview");
            };
            (member.view().unwrap().id, buffer, diff.raw.clone())
        })
        .collect::<Vec<_>>();
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 60000\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    let before = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}))["next_handle"]
        .as_u64()
        .unwrap();

    for key in ["h", "j", "k", "j", "j", "h", "l"] {
        session.input(key);
        session.dispatch(source, "tick", Value::Null).unwrap();
        assert!(selected_row(&session, source).id.starts_with("file:"));
        assert_eq!(
            session
                .views
                .page(source)
                .previews
                .as_ref()
                .unwrap()
                .members
                .len(),
            previous.len(),
            "key={key}, selection={}",
            selected_row(&session, source).id,
        );
        for (view, buffer, text) in &previous {
            assert_eq!(session.views[view].location.buffer, *buffer);
            let Buffer::PreviewDiff { diff, .. } = &session.buffers[buffer] else {
                panic!("navigation replaced the previous diff");
            };
            assert_eq!(&diff.raw, text);
        }
    }
    let after = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}))["next_handle"]
        .as_u64()
        .unwrap();
    assert_eq!(after, before + 1);
    assert_eq!(session.focus, source);
}
