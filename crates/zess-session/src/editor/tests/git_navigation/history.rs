use super::*;

fn pane_rows(session: &Session, source: ViewId, name: &str) -> Vec<DisplayRow> {
    let target = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .find(|member| member.id == name)
        .unwrap()
        .view()
        .unwrap()
        .id;
    let display = match &session.buffers[&session.views[&target].location.buffer] {
        Buffer::PreviewRows {
            kind: PreviewRowKind::Content,
            display,
            ..
        }
        | Buffer::Items {
            role: ItemsRole::Preview,
            display,
            ..
        } => display,
        _ => panic!("expected structured history pane"),
    };
    display.rows.clone()
}

fn commit_info(fixture: &Fixture, oid: &str) -> String {
    git(
        fixture,
        &[
            "show",
            "--no-patch",
            "--format=%H%nAuthor: %an%nDate:   %aI%n%n%B",
            oid,
        ],
    )
}

#[test]
fn history_file_selection_survives_republication_and_tracks_the_focused_row() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    for name in ["one", "two"] {
        fs::write(fixture.0.join(name), "content\n").unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "files"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    session.command("git.page-history").unwrap();
    settle(&mut session);
    let source = session.focus;
    let target = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members[1]
        .view()
        .unwrap()
        .id;
    session.focus_view(target).unwrap();
    assert_eq!(
        session.views[&target].location.selection.primary.as_deref(),
        Some("one")
    );
    session.input("j");
    assert_eq!(
        session.views[&target].location.selection.primary.as_deref(),
        Some("two")
    );
    session.dispatch(source, "preview", Value::Null).unwrap();
    settle(&mut session);
    assert_eq!(session.focus, target);
    assert_eq!(
        session.views[&target].location.selection.primary.as_deref(),
        Some("two")
    );
    let (rows, _) = session.render_view(target, 60, 10);
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.text.contains("two") && span.style.starts_with("selection."))
    );
}

#[test]
fn changing_commits_starts_the_file_selection_at_the_first_file() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    for name in ["one", "two"] {
        fs::write(fixture.0.join(name), "first\n").unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "first"]);
    for name in ["one", "two"] {
        fs::write(fixture.0.join(name), "second\n").unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "second"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    session.command("git.page-history").unwrap();
    settle(&mut session);
    let source = session.focus;
    let target = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members[1]
        .view()
        .unwrap()
        .id;
    session.focus_view(target).unwrap();
    session.input("j");
    assert_eq!(
        session.views[&target].location.selection.primary.as_deref(),
        Some("two")
    );
    session.focus_view(source).unwrap();
    session.input("j");
    settle(&mut session);
    assert_eq!(
        session.views[&target].location.selection.primary.as_deref(),
        Some("one")
    );
}

#[test]
fn history_shows_commit_information_and_file_line_counts_in_fixed_vertical_panes() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::create_dir(fixture.0.join("dir")).unwrap();
    fs::write(fixture.0.join("base.txt"), "one\ntwo\n").unwrap();
    fs::write(fixture.0.join("dir/change.txt"), "old\nsame\n").unwrap();
    fs::write(fixture.0.join("image.bin"), b"\0old").unwrap();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("base.txt"), "one\nnew\ntwo\n").unwrap();
    fs::write(fixture.0.join("dir/change.txt"), "new\nsame\n").unwrap();
    fs::write(fixture.0.join("image.bin"), b"\0new").unwrap();
    fs::write(fixture.0.join("added.txt"), "a\nb\n").unwrap();
    git(&fixture, &["add", "."]);
    git(
        &fixture,
        &[
            "commit",
            "--quiet",
            "-m",
            "tip summary",
            "-m",
            "More details",
        ],
    );
    let oid = git(&fixture, &["rev-parse", "HEAD"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    let source = session.focus;
    settle(&mut session);
    assert_eq!(session.focus, source);
    let info = pane_rows(&session, source, "commit-info")
        .iter()
        .map(|row| row.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(info, commit_info(&fixture, &oid));
    let summary = pane_rows(&session, source, "commit-summary");
    assert_eq!(
        summary
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        [
            "4 files, 1 binary file",
            "",
            "added.txt",
            "base.txt",
            "dir/change.txt",
            "image.bin"
        ]
    );
    fn cells(row: &DisplayRow) -> Vec<(&str, &str)> {
        row.cells
            .iter()
            .flat_map(|cell| &cell.spans)
            .map(|span| (span.text.as_str(), span.style.as_str()))
            .collect()
    }
    assert_eq!(
        cells(&summary[0]),
        [("+4", "git.added"), ("-1", "git.deleted")]
    );
    assert_eq!(
        cells(&summary[2]),
        [
            ("A", "git.added"),
            ("+2", "git.added"),
            ("-0", "git.deleted")
        ]
    );
    assert_eq!(
        cells(&summary[3]),
        [
            ("M", "modified"),
            ("+1", "git.added"),
            ("-0", "git.deleted")
        ]
    );
    assert_eq!(
        cells(&summary[4]),
        [
            ("M", "modified"),
            ("+1", "git.added"),
            ("-1", "git.deleted")
        ]
    );
    assert_eq!(
        cells(&summary[5]),
        [("M", "modified"), ("binary", "comment")]
    );
    for width in [80, 400] {
        session.resize(width, 40);
        let layout = panes(&session);
        let group = &session.views.page(source).previews.as_ref().unwrap();
        assert_eq!(group.members.len(), 2);
        let info = group.members[0].view().unwrap().id;
        let summary = group.members[1].view().unwrap().id;
        let (_, ax, ay, aw, ah) = layout.iter().copied().find(|(id, ..)| *id == info).unwrap();
        let (_, bx, by, bw, _) = layout
            .iter()
            .copied()
            .find(|(id, ..)| *id == summary)
            .unwrap();
        assert_eq!((ax, aw), (bx, bw));
        assert_eq!(ay + ah, by);
        let (rows, _) = session.render_view(info, aw, ah);
        assert_eq!(rows[1].spans[0].text, oid);
        assert_eq!(rows[1].spans[0].style, "constant");
        let (rows, _) = session.render_view(summary, bw, 12);
        let first = rows[1]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(first.starts_with("4 files, 1 binary file"));
        assert_eq!(rows[3].spans[0].text.trim_end(), "added.txt");
        assert!(
            rows[1]
                .spans
                .iter()
                .any(|span| span.text.trim() == "+4" && span.style == "git.added")
        );
        assert!(
            rows[1]
                .spans
                .iter()
                .any(|span| span.text.trim() == "-1" && span.style == "git.deleted")
        );
    }
}

#[test]
fn rapid_history_selection_keeps_the_latest_summary_without_stealing_help_focus() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    for name in ["first", "second", "third"] {
        fs::write(fixture.0.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
        git(&fixture, &["add", "."]);
        git(&fixture, &["commit", "--quiet", "-m", name]);
    }
    let first = git(&fixture, &["rev-parse", "HEAD~2"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    let source = session.focus;
    for key in ["j", "j", "space", "?"] {
        session.input(key);
    }
    let help = session.focus;
    settle(&mut session);
    assert_eq!(session.focus, help);
    assert!(session.help.is_some());
    assert_eq!(
        session.views[&source].location.selection.primary.as_deref(),
        Some(first.as_str())
    );
    let info = pane_rows(&session, source, "commit-info")
        .iter()
        .map(|row| row.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let summary = pane_rows(&session, source, "commit-summary");
    assert_eq!(info, commit_info(&fixture, &first));
    assert_eq!(
        summary
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["1 file", "", "first.txt"]
    );
    assert_eq!(
        session
            .services
            .borrow_mut()
            .call("service.stats", json!({}))["jobs"],
        0
    );
    session.input("q");
    assert_eq!(session.focus, source);
}

#[test]
fn historical_diff_source_opens_the_complete_readonly_blob_from_that_commit() {
    let (fixture, path) = repository();
    let oid = git(&fixture, &["rev-parse", "HEAD"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    assert_ne!(session.focus, source);
    for key in ["g", "f"] {
        session.input(key);
    }
    settle(&mut session);
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Snapshot { id, text } = &session.buffers[&buffer] else {
        panic!("{}", session.message)
    };
    assert_eq!(id.commit, oid);
    assert_eq!(text.text(), "historical\n");
    assert!(session.command("write note.txt").is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "working\n");
    assert!(preview_groups(&session).next().is_none());
}

#[test]
fn quitting_either_history_pane_returns_to_the_original_editor() {
    for pane in 0..2 {
        let (fixture, _) = repository();
        let mut session = fixture.session(&[]);
        let source = session.focus;
        let buffer = session.views[&source].location.buffer;
        let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
        session.command("git").unwrap();
        session.command("git.page-history").unwrap();
        settle(&mut session);
        let preview = session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members[pane]
            .view()
            .unwrap()
            .id;
        session.focus_view(preview).unwrap();
        session.command("quit").unwrap();
        settle(&mut session);

        assert!(!session.exiting());
        assert_eq!(session.focus, source);
        assert_eq!(session.views[&source].location.buffer, buffer);
        assert!(preview_groups(&session).next().is_none());
        assert_eq!(session.views.values().count(), 1);
        assert_eq!(
            session.buffers.keys().copied().collect::<HashSet<_>>(),
            buffers
        );
        assert_eq!(
            session
                .services
                .borrow_mut()
                .call("service.stats", json!({}))["jobs"],
            0
        );
    }
}

#[test]
fn empty_history_finishes_loading_both_preview_panes() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    let source = session.focus;
    settle(&mut session);
    assert!(session.views[&source].location.display.rows.is_empty());
    assert!(!session.views[&source].location.display.loading);
    let group = &session.views.page(source).previews.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    assert!(group.members.iter().all(|member| {
        !session.views[&member.view().unwrap().id]
            .location
            .display
            .loading
    }));
    assert_eq!(session.focus, source);
}
