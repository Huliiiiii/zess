use super::*;

#[test]
fn diff_selection_is_highlighted_only_in_the_focused_view() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let preview = session.preview_target(source).unwrap();
    let selected = session.views[&preview].location.diff_selection.row();
    let selection_mode = std::mem::discriminant(&session.views[&preview].location.diff_selection);
    assert!(selected.is_some());
    let (rows, cursor) = session.render_view(preview, 80, 12);
    assert!(cursor.is_none());
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .all(|span| { !span.style.starts_with("selection") && span.style != "linenr.current" })
    );
    session.focus_view(preview).unwrap();
    let (rows, _) = session.render_view(preview, 80, 12);
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .any(|span| { span.style.starts_with("selection") })
    );
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        selected
    );
    assert_eq!(
        std::mem::discriminant(&session.views[&preview].location.diff_selection),
        selection_mode
    );
    session.focus_view(source).unwrap();
    let (rows, _) = session.render_view(preview, 80, 12);
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .all(|span| { !span.style.starts_with("selection") && span.style != "linenr.current" })
    );
    assert_eq!(
        session.views[&preview].location.diff_selection.row(),
        selected
    );
    assert_eq!(
        std::mem::discriminant(&session.views[&preview].location.diff_selection),
        selection_mode
    );
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

fn addition_columns(session: &mut Session, view: ViewId) -> Vec<usize> {
    let (rows, _) = session.render_view(view, 48, 16);
    let location = &session.views[&view].location;
    let header =
        usize::from(!location.display.header.is_empty() || !location.display.tabs.is_empty());
    [
        "dir/alpha/a.txt",
        "dir/alpha/b.txt",
        "dir/beta/c.txt",
        "dir/beta/d.txt",
    ]
    .map(|path| {
        let index = location
            .display
            .rows
            .iter()
            .position(|row| row.id == format!("file:{path}"))
            .unwrap();
        rows[header + index - location.scroll]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .find('+')
            .unwrap()
    })
    .to_vec()
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
        ("all", [39, 37, 39, 38]),
        ("none", [39, 39, 40, 40]),
        ("consecutive", [39, 37, 40, 39]),
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
        assert_eq!(addition_columns(&mut session, source), expected);
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
    assert_eq!(addition_columns(&mut session, source), [39, 37, 39, 38]);
}

#[test]
fn invalid_reload_preserves_the_open_views_and_their_alignment() {
    use zess_ui::ColumnAlignment;

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
    let actual = &session.views[&split].location.selection;
    assert_eq!(actual.primary, selection.primary);
    assert_eq!(actual.selected, selection.selected);
    assert_eq!(actual.anchor, selection.anchor);

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
    assert_eq!(addition_columns(&mut session, source), [39, 37, 40, 39]);
}

#[test]
fn unaligned_counts_use_refreshed_widths() {
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
    assert_eq!(addition_columns(&mut session, source), [39, 39, 40, 40]);

    fs::write(fixture.0.join("dir/beta/d.txt"), "new\n".repeat(1000)).unwrap();
    session.command("git.refresh").unwrap();
    settle(&mut session);
    assert_eq!(addition_columns(&mut session, source), [39, 39, 40, 38]);
}

#[test]
fn unaligned_counts_yield_to_names_in_narrow_views() {
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
    let (rows, _) = session.render_view(source, 12, 16);
    let location = &session.views[&source].location;
    for path in [
        "dir/alpha/a.txt",
        "dir/alpha/b.txt",
        "dir/beta/c.txt",
        "dir/beta/d.txt",
    ] {
        let index = location
            .display
            .rows
            .iter()
            .position(|row| row.id == format!("file:{path}"))
            .unwrap();
        let line = rows[index + 1 - location.scroll]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(!line.contains('+'), "{line:?}");
        assert_eq!(UnicodeWidthStr::width(line.as_str()), 12);
    }
}
