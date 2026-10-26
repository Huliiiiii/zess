use super::*;

#[test]
fn totals_count_files_once_and_survive_tree_and_filter_changes() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(fixture.0.join("src/a.rs"), "old\n").unwrap();
    fs::write(fixture.0.join("src/b.rs"), "old\n").unwrap();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "initial"]);
    fs::write(fixture.0.join("src/a.rs"), "new\nextra\n").unwrap();
    fs::write(fixture.0.join("src/b.rs"), "new\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let expected =
        json!({"git": {"state": "ready", "additions": 3, "deletions": 2, "unknown_files": 0}});
    assert_eq!(session.views[&source].location.display.status, expected);
    let revision = session.views[&source].location.display.rows_revision;

    session.input("j");
    settle(&mut session);
    assert_eq!(
        session.views[&source].location.display.rows_revision,
        revision
    );
    assert_eq!(session.views[&source].location.display.status, expected);

    for key in ["g", "g", "h"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(session.views[&source].location.display.rows.len(), 1);
    assert_eq!(session.views[&source].location.display.status, expected);
    assert_ne!(
        session.views[&source].location.display.rows_revision,
        revision
    );

    for key in ["/", "a", ".", "r", "s", "Enter"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(session.views[&source].location.display.status, expected);
    assert!(
        !session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.id == "file:src/b.rs")
    );
}

#[test]
fn totals_keep_known_counts_and_identify_files_without_line_statistics() {
    let (fixture, _) = repository();
    fs::write(fixture.0.join("binary.dat"), [0, 1, 2]).unwrap();
    fs::write(fixture.0.join("new.txt"), "first\nsecond").unwrap();
    git(&fixture, &["add", "binary.dat"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);

    assert_eq!(
        session.views[&session.focus].location.display.status,
        json!({"git": {"state": "ready", "additions": 3, "deletions": 1, "unknown_files": 1}}),
    );
    let (rows, _) = session.render_view(session.focus, 90, 12);
    let statusline = &rows.last().unwrap().spans;
    assert_eq!(
        statusline
            .iter()
            .filter(|span| span.style == "git.added" || span.style == "git.deleted")
            .map(|span| (span.text.as_str(), span.style.as_str()))
            .collect::<Vec<_>>(),
        [("+3", "git.added"), ("-1", "git.deleted")]
    );
    assert_eq!(
        statusline
            .iter()
            .find(|span| span.text.trim() == "1 uncounted")
            .unwrap()
            .text,
        "  1 uncounted"
    );
}

#[test]
fn history_totals_follow_the_selected_commit_after_returning_from_a_preview() {
    let (fixture, path) = repository();
    fs::write(path, "new\nextra\n").unwrap();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "second"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    let source = session.focus;
    assert_eq!(
        session.views[&source].location.display.status,
        json!({"git": {"state": "ready", "additions": 2, "deletions": 1, "unknown_files": 0}}),
    );
    let (rows, _) = session.render_view(source, 60, 12);
    let statusline = &rows.last().unwrap().spans;
    assert_eq!(
        statusline
            .iter()
            .filter(|span| span.style == "git.added" || span.style == "git.deleted")
            .map(|span| (span.text.as_str(), span.style.as_str()))
            .collect::<Vec<_>>(),
        [("+2", "git.added"), ("-1", "git.deleted")]
    );
    assert_eq!(
        statusline
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        format!("{:<54}+2  -1", " NOR  Git")
    );

    let summary = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members[1]
        .view()
        .unwrap()
        .id;
    session.focus_view(summary).unwrap();
    session.input("Esc");
    session.input("j");
    settle(&mut session);

    assert_eq!(
        session.views[&source].location.display.status,
        json!({"git": {"state": "ready", "additions": 1, "deletions": 0, "unknown_files": 0}}),
    );
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
    assert_eq!(session.focus, source);
}
