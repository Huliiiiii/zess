use zess_ui::{ColumnAlignment, DisplayCell, Span};

use super::*;

#[test]
fn diff_selection_survives_changes_in_focus() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let preview = session.preview_target(source).unwrap();
    let selection = session.views[&preview].location.diff_selection.clone();
    assert!(selection.row().is_some());
    for focus in [preview, source] {
        session.focus_view(focus).unwrap();
        let snapshot = session.snapshot();
        assert_eq!(snapshot.focused_view, Some(focus));
        assert_eq!(
            session.views[&preview].location.diff_selection.row(),
            selection.row()
        );
        assert_eq!(
            std::mem::discriminant(&session.views[&preview].location.diff_selection),
            std::mem::discriminant(&selection)
        );
    }
}

fn counted_files() -> Fixture {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    for (path, removed) in [
        ("dir/alpha/a.txt", 100),
        ("dir/alpha/b.txt", 1),
        ("dir/beta/c.txt", 10),
        ("dir/beta/d.txt", 2),
    ] {
        let path = fixture.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "old\n".repeat(removed)).unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
    for (path, added) in [
        ("dir/alpha/a.txt", 1),
        ("dir/alpha/b.txt", 100),
        ("dir/beta/c.txt", 2),
        ("dir/beta/d.txt", 10),
    ] {
        fs::write(fixture.0.join(path), "new\n".repeat(added)).unwrap();
    }
    fixture
}

#[test]
fn reloading_alignment_updates_the_tree_without_rebuilding_rows() {
    let fixture = counted_files();
    let config = fixture.0.join(".git/grid-config.w");
    fs::write(&config, "").unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["g", "g"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).text, "▾ dir");
    let target = session.preview_target(source).unwrap();
    let revision = session.views[&source].location.display.rows_revision;

    for (alignment, expected) in [
        ("all", ColumnAlignment::All),
        ("none", ColumnAlignment::None),
        ("consecutive", ColumnAlignment::Consecutive),
    ] {
        fs::write(
            &config,
            format!("set-view-options! \"git\" : obj 'stats-alignment \"{alignment}\"\n"),
        )
        .unwrap();
        session.command("config-reload").unwrap();
        assert_eq!(
            session.views[&source].location.display.rows_revision,
            revision
        );
        assert_eq!(session.preview_target(source), Some(target));
        assert_eq!(
            view_snapshot(&mut session, source).column_alignment,
            expected
        );
        assert_eq!(session.focus, source);
    }
}

#[test]
fn invalid_alignment_keeps_the_builtin_configuration_at_startup() {
    let fixture = counted_files();
    let config = fixture.0.join(".git/grid-config.w");
    fs::write(
        &config,
        "set-view-options! \"git\" : obj 'stats-alignment #f\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert_eq!(
        session.message,
        "reload failed: view options for git: stats-alignment must be \"none\", \"consecutive\", or \"all\""
    );
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(
        view_snapshot(&mut session, source).column_alignment,
        ColumnAlignment::All
    );
}

#[test]
fn invalid_reload_preserves_the_open_views_and_their_alignment() {
    let fixture = counted_files();
    let config = fixture.0.join(".git/grid-config.w");
    fs::write(
        &config,
        "set-view-options! \"git\" : obj 'stats-alignment \"consecutive\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.command("vsplit").unwrap();
    let split = session.focus;
    session
        .request(
            split,
            HostRequest::ConfigureView {
                options: json!({"stats-alignment": "none"}),
            },
        )
        .unwrap();
    session.command("git.refresh").unwrap();
    settle(&mut session);
    let selection = session.views[&split].location.selection.clone();
    let preview = session.preview_target(split).unwrap();

    fs::write(
        config,
        "set-view-options! \"git\" : obj 'stats-alignment #f\n",
    )
    .unwrap();
    let error = session.command("config-reload").unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("reload failed: view options for git"));
    assert!(error.contains("stats-alignment must be"));
    assert_eq!(session.focus, split);
    assert_eq!(session.preview_target(split), Some(preview));
    assert_eq!(session.views[&split].location.selection, selection);

    session.command("git.refresh").unwrap();
    settle(&mut session);
    assert_eq!(
        session.views[&source].location.display.column_alignment,
        ColumnAlignment::Consecutive
    );
    assert_eq!(
        session.views[&split].location.display.column_alignment,
        ColumnAlignment::None
    );
}

#[test]
fn commit_files_use_their_own_alignment() {
    let fixture = counted_files();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "replace lines"]);
    let config = fixture.0.join(".git/grid-config.w");
    fs::write(
        &config,
        "set-view-options! \"git\" : obj 'stats-alignment \"none\"\n\
         set-view-options! \"git-commit-files\" : obj 'stats-alignment \"consecutive\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    for key in ["C-w", "2", "Enter"] {
        session.input(key);
        settle(&mut session);
    }
    let source = session.focus;
    assert_eq!(
        view_snapshot(&mut session, source).column_alignment,
        ColumnAlignment::Consecutive
    );
}

#[test]
fn unaligned_counts_are_republished_after_file_changes() {
    let fixture = counted_files();
    let config = fixture.0.join(".git/grid-config.w");
    fs::write(
        &config,
        "set-view-options! \"git\" : obj 'stats-alignment \"none\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let before = view_snapshot(&mut session, source);
    fs::write(fixture.0.join("dir/beta/d.txt"), "new\n".repeat(1000)).unwrap();
    session.command("git.refresh").unwrap();
    settle(&mut session);
    let after = view_snapshot(&mut session, source);
    assert_eq!(after.column_alignment, ColumnAlignment::None);
    assert_ne!(after.version, before.version);
    let row = item_rows(&after.content)
        .iter()
        .find(|row| row.id == "file:dir/beta/d.txt")
        .unwrap();
    assert!(row.cells.iter().flatten().any(|span| span.text == "+1000"));
}

#[test]
fn changing_alignment_remeasures_unchanged_rows() {
    let rows = [("first", ["+1", "-100"]), ("second", ["+100", "-1"])]
        .into_iter()
        .map(|(text, cells)| DisplayRow {
            text: text.into(),
            cells: cells
                .into_iter()
                .map(|text| DisplayCell {
                    spans: vec![Span {
                        base_style: None,
                        text: text.into(),
                        style: String::new(),
                    }],
                })
                .collect(),
            ..DisplayRow::default()
        })
        .collect();
    let mut display = Display {
        rows,
        rows_revision: Some(1),
        ..Display::default()
    };
    let mut location = ViewLocation::new(1);
    location.set_display(display.clone());
    assert_eq!(location.columns.layout(0, 30).cell_widths(), [4, 4]);
    assert_eq!(location.columns.layout(1, 30).cell_widths(), [4, 4]);

    display.column_alignment = ColumnAlignment::None;
    location.set_display(display);
    assert_eq!(location.columns.layout(0, 30).cell_widths(), [2, 4]);
    assert_eq!(location.columns.layout(1, 30).cell_widths(), [4, 2]);
}
