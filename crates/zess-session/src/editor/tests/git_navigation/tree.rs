use super::*;

fn nested_files() -> Fixture {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::create_dir_all(fixture.0.join("src/nested")).unwrap();
    for path in ["src/a.rs", "src/nested/b.rs", "root.txt"] {
        fs::write(fixture.0.join(path), format!("content of {path}\n")).unwrap();
    }
    fixture
}

#[test]
fn directory_navigation_keeps_tree_selection_and_opens_leaf_diffs() {
    let fixture = nested_files();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.text == "▾ src" && row.depth == 0)
    );
    assert_eq!(selected_row(&session, source).id, "file:src/nested/b.rs");
    session.input("h");
    assert_eq!(selected_row(&session, source).text, "▾ nested");
    session.input("h");
    assert_eq!(selected_row(&session, source).text, "▸ nested");
    assert!(
        !session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.id == "file:src/nested/b.rs")
    );
    session.input("e");
    assert_eq!(session.focus, source);
    assert!(session.views.child(source).is_none());
    session.input("l");
    assert_eq!(selected_row(&session, source).text, "▾ nested");
    session.input("l");
    assert_eq!(selected_row(&session, source).id, "file:src/nested/b.rs");
    session.input("Enter");
    settle(&mut session);
    let target = session.preview_target(source).unwrap();
    assert_eq!(session.focus, target);
    let Buffer::PreviewDiff { diff, .. } =
        &session.buffers[&session.views[&target].location.buffer]
    else {
        panic!("expected diff")
    };
    let blob = git(&fixture, &["hash-object", "src/nested/b.rs"]);
    assert_eq!(
        diff.raw,
        format!(
            "diff --git a/src/nested/b.rs b/src/nested/b.rs\nnew file mode 100644\nindex 0000000..{}\n--- /dev/null\n+++ b/src/nested/b.rs\n@@ -0,0 +1 @@\n+content of src/nested/b.rs\n",
            &blob[..7]
        )
    );
    session.input("Esc");
    assert_eq!(session.focus, source);
    session.input("h");
    session.input("Enter");
    settle(&mut session);
    assert_eq!(selected_row(&session, source).text, "▾ nested");
    assert_eq!(session.focus, session.preview_target(source).unwrap());
    session.input("Esc");
    session.input("h");
    assert_eq!(selected_row(&session, source).text, "▸ nested");
    assert_eq!(session.focus, source);
    session.input("h");
    assert_eq!(selected_row(&session, source).text, "▾ src");
}

#[test]
fn tree_expansion_and_local_flat_projection_remain_independent_after_reload() {
    let fixture = nested_files();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("h");
    session.input("h");
    assert_eq!(selected_row(&session, source).text, "▸ nested");
    session.command("vsplit").unwrap();
    let flat = session.focus;
    session
        .request(
            flat,
            HostRequest::ConfigureView {
                options: json!({"git-view":"flat"}),
            },
        )
        .unwrap();
    session.command("git.refresh").unwrap();
    settle(&mut session);
    assert!(
        session.views[&flat]
            .location
            .display
            .rows
            .iter()
            .all(|row| row.depth == 0 && row.style != "directory")
    );
    assert!(
        session.views[&flat]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.id == "file:src/nested/b.rs")
    );
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.text == "▸ nested")
    );
    session.command("config-reload").unwrap();
    settle(&mut session);
    assert_eq!(session.views[&flat].location.options["git-view"], "flat");
    assert!(
        session.views[&flat]
            .location
            .display
            .rows
            .iter()
            .all(|row| row.depth == 0 && row.style != "directory")
    );
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.text == "▸ nested")
    );
    session.focus_view(source).unwrap();
    assert_eq!(selected_row(&session, source).text, "▸ nested");
}

#[test]
fn changes_can_be_configured_flat_while_commit_files_keep_the_default_tree() {
    let fixture = nested_files();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "nested files"]);
    fs::write(fixture.0.join("src/a.rs"), "changed\n").unwrap();
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
            .all(|row| row.depth == 0 && row.style != "directory")
    );
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| { row.id == "file:src/a.rs" && trailing_text(row) == "+1 -1" })
    );
    for key in ["C-w", "2", "Enter"] {
        session.input(key);
        settle(&mut session);
    }
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.text == "▾ src")
    );
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| {
                row.depth == 2 && row.id == "file:src/nested/b.rs" && trailing_text(row) == "+1 -0"
            })
    );
}

#[test]
fn commit_file_counts_follow_tree_and_flat_projections() {
    let fixture = nested_files();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "nested files"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    for key in ["C-w", "2", "Enter"] {
        session.input(key);
        settle(&mut session);
    }
    let source = session.focus;
    insta::assert_json_snapshot!(
        "commit_files_tree",
        session.views[&source].location.display.rows
    );
    let (rows, _) = session.render_view(source, 40, 12);
    let line = rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .nth(3)
        .unwrap();
    assert_eq!(line, "    b.rs                      A  +1  -0 ");
    assert_eq!(UnicodeWidthStr::width(line.as_str()), 40);
    session
        .request(
            source,
            HostRequest::ConfigureView {
                options: json!({"git-view":"flat"}),
            },
        )
        .unwrap();
    session.input("j");
    settle(&mut session);
    insta::assert_json_snapshot!(
        "commit_files_flat",
        session.views[&source].location.display.rows
    );
    let (rows, _) = session.render_view(source, 40, 12);
    let line = rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .nth(3)
        .unwrap();
    assert_eq!(line, "src/nested/b.rs               A  +1  -0 ");
    assert_eq!(UnicodeWidthStr::width(line.as_str()), 40);
}

#[test]
fn line_counts_align_right_and_yield_to_file_names_in_narrow_panes() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("v");
    for (width, expected) in [
        (1, "n"),
        (4, "not "),
        (6, "note. "),
        (9, "note.txt "),
        (12, "note.txt    "),
        (16, "note.txt        "),
        (24, "note.txt                "),
        (40, "note.txt                      M  +1  -1 "),
    ] {
        let (rows, _) = session.render_view(source, width, 4);
        let row = &rows[1];
        let line = row
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(line, expected);
        assert_eq!(UnicodeWidthStr::width(line.as_str()), width);
        let styled = if width > 1 {
            assert_eq!(row.spans.last().unwrap().text, " ");
            assert!(row.spans.last().unwrap().style.is_empty());
            &row.spans[..row.spans.len() - 1]
        } else {
            row.spans.as_slice()
        };
        assert!(styled.iter().all(|span| matches!(
            span.style.as_str(),
            "selection.modified" | "selection.git.added" | "selection.git.deleted"
        )));
    }
    assert_eq!(selected_row(&session, source).id, "file:note.txt");
}

#[test]
fn external_file_changes_refresh_line_counts_in_both_projections() {
    let (fixture, path) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(selected_row(&session, source).text, "note.txt");
    assert_eq!(trailing_text(selected_row(&session, source)), "+1 -1");
    session.command("vsplit").unwrap();
    let flat = session.focus;
    session
        .request(
            flat,
            HostRequest::ConfigureView {
                options: json!({"git-view":"flat"}),
            },
        )
        .unwrap();
    settle(&mut session);
    fs::write(&path, "historical\nextra\nsecond\n").unwrap();
    wait_until(&mut session, |session| {
        [source, flat].into_iter().all(|view| {
            session.views[&view]
                .location
                .display
                .rows
                .iter()
                .any(|row| row.text == "note.txt" && trailing_text(row) == "+2 -0")
        })
    });
    assert_eq!(selected_row(&session, source).style, "modified");
    assert_eq!(selected_row(&session, flat).style, "modified");
}

#[test]
fn invalid_projection_configuration_keeps_the_builtin_tree_at_startup() {
    let fixture = nested_files();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "set-view-options! \"git\" : obj 'git-view \"other\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert_eq!(
        session.message,
        "reload failed: view options for git: git-view must be \"tree\" or \"flat\""
    );
    session.command("git").unwrap();
    settle(&mut session);
    assert!(
        session.views[&session.focus]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.style == "directory")
    );
}

#[test]
fn configured_git_colors_are_shared_by_counts_and_diff_lines() {
    use zess_ui::{StyleAttribute, StyleColor};

    let (fixture, _) = repository();
    git(&fixture, &["config", "--add", "color.diff.new", "blue"]);
    git(
        &fixture,
        &["config", "--add", "color.diff.new", "#abc bold"],
    );
    git(&fixture, &["config", "color.diff.old", "214 52 italic"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let frame = session.frame();
    let added = &frame.styles["git.added"];
    let deleted = &frame.styles["git.deleted"];
    assert_eq!(
        added.foreground,
        Some(StyleColor::Rgb {
            red: 170,
            green: 187,
            blue: 204
        })
    );
    assert_eq!(added.attributes, [StyleAttribute::Bold]);
    assert_eq!(deleted.foreground, Some(StyleColor::Indexed(214)));
    assert_eq!(deleted.background, Some(StyleColor::Indexed(52)));
    assert_eq!(deleted.attributes, [StyleAttribute::Italic]);
    for role in ["git.added", "git.deleted"] {
        let selected = &frame.styles[&format!("selection.{role}")];
        assert_eq!(selected.foreground, frame.styles[role].foreground);
        assert_eq!(selected.attributes, frame.styles[role].attributes);
        assert_eq!(selected.background, Some(StyleColor::Indexed(8)));
    }
    let spans = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .collect::<Vec<_>>();
    for (text, role) in [
        ("+1", "selection.git.added"),
        ("-1", "selection.git.deleted"),
    ] {
        assert!(
            spans
                .iter()
                .any(|span| span.text == text && span.style == role),
            "missing {text:?} with {role:?}: {frame:?}"
        );
    }
    let source = session.focus;
    let target = session.preview_target(source).unwrap();
    session.focus_preview(session.focus).unwrap();
    session
        .execute_command("diff.select-hunk", serde_json::json!({}))
        .unwrap();
    let frame = session.frame();
    let spans = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .collect::<Vec<_>>();
    for (text, role) in [
        ("+working", "selection.git.added"),
        ("-historical", "selection.git.deleted"),
    ] {
        assert!(
            spans
                .iter()
                .any(|span| span.text == text && span.style == role),
            "missing {text:?} with {role:?}: {frame:?}"
        );
    }
    let Buffer::PreviewDiff { diff, .. } =
        &session.buffers[&session.views[&target].location.buffer]
    else {
        panic!("expected Git diff");
    };
    assert!(!diff.raw.contains('\u{1b}'));
}
