use super::*;

#[test]
fn toggle_uses_the_configured_flat_view_as_its_starting_point() {
    let fixture = nested_changes();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-view-options! \"git\" : obj 'git-view \"flat\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .all(|row| row.style != "directory")
    );

    session.command("git.toggle-view").unwrap();
    assert_eq!(session.views[&source].location.options["git-view"], "tree");
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.style == "directory")
    );
}

#[test]
fn changes_view_switches_between_tree_and_flat_without_losing_selection_or_filter() {
    let fixture = nested_changes();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session
        .request(
            source,
            HostRequest::ConfigureView {
                options: json!({"stats-alignment":"none"}),
            },
        )
        .unwrap();
    session.input("/");
    for key in "modified".chars() {
        session.input(&key.to_string());
    }
    session.input("Enter");
    let selected = selected_row(&session, source).id.clone();
    assert_eq!(selected, "file:dir/modified.txt");
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.style == "directory")
    );

    session.command("git.toggle-view").unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(selected_row(&session, source).id, selected);
    assert_eq!(session.views[&source].location.options["git-view"], "flat");
    assert_eq!(
        session.views[&source].location.options["stats-alignment"],
        "none"
    );
    let rows = &session.views[&source].location.display.rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, selected);
    assert_eq!(rows[0].depth, 0);

    session.command("git.toggle-view").unwrap();
    assert_eq!(selected_row(&session, source).id, selected);
    assert_eq!(session.views[&source].location.options["git-view"], "tree");
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.style == "directory")
    );
}

#[test]
fn commit_files_view_switches_between_tree_and_flat() {
    let fixture = nested_changes();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-view-options! \"git-commit-files\" : obj 'stats-alignment \"all\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("git").unwrap();
    settle(&mut session);
    for key in ["C-w", "2", "Enter"] {
        session.input(key);
        settle(&mut session);
    }
    let source = session.focus;
    let selected = selected_row(&session, source).id.clone();
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.style == "directory")
    );

    session.command("git-commit-files.toggle-view").unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(selected_row(&session, source).id, selected);
    assert_eq!(session.views[&source].location.options["git-view"], "flat");
    assert_eq!(
        session.views[&source].location.options,
        json!({"git-view":"flat"})
    );
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .all(|row| row.depth == 0 && row.style != "directory")
    );

    fs::write(
        &config,
        "set-view-options! \"git-commit-files\" : obj 'stats-alignment \"none\"\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    settle(&mut session);
    assert_eq!(
        session.effective_view_options(source, "git-commit-files")["stats-alignment"],
        "none"
    );
    assert_eq!(
        session.views[&source].location.options,
        json!({"git-view":"flat"})
    );

    session.command("git-commit-files.toggle-view").unwrap();
    assert_eq!(selected_row(&session, source).id, selected);
    assert_eq!(session.views[&source].location.options["git-view"], "tree");
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.style == "directory")
    );
}
