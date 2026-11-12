use super::*;

fn select_file_line(session: &mut Session, path: &str, content: &str, extend: bool) {
    let view = session.focus;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&session.views[&view].location.buffer]
    else {
        panic!("expected diff");
    };
    let row = diff
        .index
        .rows
        .iter()
        .enumerate()
        .find(|(row, _)| {
            diff.file_at(*row)
                .is_some_and(|file| file.target.path == path)
                && diff.line(*row) == content
        })
        .unwrap()
        .0;
    let selection = &mut session
        .views
        .get_mut(&view)
        .unwrap()
        .location
        .diff_selection;
    if !extend {
        *selection = diff::Selection::Line { head: None };
    }
    assert!(selection.select_at(&diff.index, row, extend));
}

fn matching_files() -> Fixture {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::create_dir(fixture.0.join("dir")).unwrap();
    for path in ["dir/first.txt", "dir/second.txt"] {
        fs::write(fixture.0.join(path), "baseline\n").unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
    for path in ["dir/first.txt", "dir/second.txt"] {
        fs::write(fixture.0.join(path), "changed\n").unwrap();
    }
    fixture
}

fn read_directory(session: &mut Session) -> ViewId {
    session.command("git").unwrap();
    settle(session);
    let source = session.focus;
    for key in ["g", "g"] {
        session.input(key);
    }
    settle(session);
    assert_eq!(selected_row(session, source).id, "directory:dir");
    session.input("Enter");
    settle(session);
    assert_ne!(session.focus, source);
    source
}

#[test]
fn directory_diff_refresh_and_source_edit_keep_the_selected_file_among_identical_lines() {
    let fixture = matching_files();
    let mut session = fixture.session(&[]);
    let source = read_directory(&mut session);
    let preview = session.focus;
    select_file_line(&mut session, "dir/second.txt", "+changed", false);
    fs::write(fixture.0.join("dir/middle.txt"), "changed\n").unwrap();
    session.dispatch(source, "refresh", Value::Null).unwrap();
    settle(&mut session);
    let view = &session.views[&preview];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff");
    };
    let row = view.location.diff_selection.row().unwrap();
    assert_eq!(diff.file_at(row).unwrap().target.path, "dir/second.txt");
    assert_eq!(diff.line(row), "+changed");
    session.input("e");
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("expected editor: {}", session.message);
    };
    assert_eq!(
        text.path(),
        Some(fixture.0.join("dir/second.txt").as_path())
    );
    assert_eq!(text.text(), "changed\n");
    assert_eq!(selected_row(&session, source).id, "directory:dir");
}

#[test]
fn a_cross_file_stage_selection_keeps_the_index_unchanged() {
    let fixture = matching_files();
    let mut session = fixture.session(&[]);
    read_directory(&mut session);
    select_file_line(&mut session, "dir/first.txt", "+changed", false);
    select_file_line(&mut session, "dir/second.txt", "+changed", true);
    let index = git(&fixture, &["write-tree"]);
    let error = session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap_err();
    assert_eq!(error.to_string(), "cannot stage a selection across files");
    assert!(session.git_patch.is_none());
    assert_eq!(git(&fixture, &["write-tree"]), index);
}

#[test]
fn staging_the_second_file_uses_its_own_patch_rows() {
    let fixture = matching_files();
    let mut session = fixture.session(&[]);
    read_directory(&mut session);
    select_file_line(&mut session, "dir/second.txt", "-baseline", false);
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":dir/first.txt"]), "baseline");
    assert_eq!(git(&fixture, &["show", ":dir/second.txt"]), "changed");
}

#[test]
fn a_deleted_diff_source_does_not_create_an_empty_editor() {
    let fixture = matching_files();
    fs::remove_file(fixture.0.join("dir/second.txt")).unwrap();
    let mut session = fixture.session(&[]);
    let source = read_directory(&mut session);
    let preview = session.focus;
    let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
    select_file_line(&mut session, "dir/second.txt", "-baseline", false);
    for command in ["diff.edit-source", "preview.open-source"] {
        let error = session.execute_command(command, json!({})).unwrap_err();
        assert_eq!(error.to_string(), "Git source file no longer exists");
        assert_eq!(session.focus, preview);
        assert!(session.views.child(preview).is_none());
        assert_eq!(
            session.buffers.keys().copied().collect::<HashSet<_>>(),
            buffers
        );
    }
    session.focus_view(source).unwrap();
    for key in ["j", "j"] {
        session.input(key);
    }
    assert_eq!(selected_row(&session, source).id, "file:dir/second.txt");
    for keys in [vec!["e"], vec!["g", "f"]] {
        for key in keys {
            session.input(key);
        }
        assert_eq!(session.focus, source);
        assert!(session.views.child(source).is_none());
        assert_eq!(session.message, "error: Git source file no longer exists");
        assert_eq!(
            session.buffers.keys().copied().collect::<HashSet<_>>(),
            buffers
        );
    }
}

#[test]
fn a_historical_directory_opens_the_selected_second_file_blob() {
    let fixture = matching_files();
    let oid = git(&fixture, &["rev-parse", "HEAD"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    session.command("git.page-history").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    settle(&mut session);
    for key in ["g", "g"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "directory:dir");
    session.input("Enter");
    settle(&mut session);
    let preview = session.focus;
    select_file_line(&mut session, "dir/second.txt", "+baseline", false);
    session.input("e");
    assert_eq!(session.focus, preview);
    settle(&mut session);
    let layer = session.focus;
    let Buffer::Snapshot { id, text } = &session.buffers[&session.views[&layer].location.buffer]
    else {
        panic!("expected snapshot: {}", session.message);
    };
    assert_eq!(id.commit, oid);
    assert_eq!(id.path, "dir/second.txt");
    assert_eq!(text.text(), "baseline\n");
    assert!(session.command("write").is_err());
    assert_eq!(session.views.parent(layer), Some(preview));
    session.command("quit").unwrap();
    assert_eq!(session.focus, preview);
    for key in ["g", "f"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(session.focus, source);
    let Buffer::Snapshot { id, text } = &session.buffers[&session.views[&source].location.buffer]
    else {
        panic!("expected snapshot");
    };
    assert_eq!(id.path, "dir/second.txt");
    assert_eq!(text.text(), "baseline\n");
}

#[test]
fn a_renamed_file_with_special_characters_keeps_one_source_target_for_all_patch_sections() {
    let fixture = matching_files();
    let renamed = "dir/renamed\tfile\n.txt";
    fs::write(
        fixture.0.join("dir/second.txt"),
        "baseline\nkeep1\nkeep2\nkeep3\nkeep4\n",
    )
    .unwrap();
    git(&fixture, &["add", "dir/second.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "rename baseline"]);
    git(&fixture, &["mv", "dir/second.txt", renamed]);
    fs::write(
        fixture.0.join(renamed),
        "renamed body\nkeep1\nkeep2\nkeep3\nkeep4\n",
    )
    .unwrap();
    git(&fixture, &["add", renamed]);
    let mut session = fixture.session(&[]);
    let source = read_directory(&mut session);
    let group = session.views.page(source).previews.as_ref().unwrap();
    let staged = group
        .members
        .iter()
        .find(|member| member.id == "staged")
        .unwrap()
        .view()
        .unwrap()
        .id;
    session.focus_view(staged).unwrap();
    select_file_line(&mut session, renamed, "+renamed body", false);
    let view = &session.views[&session.focus];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff");
    };
    let file = diff
        .file_at(view.location.diff_selection.row().unwrap())
        .unwrap();
    assert_eq!(file.target.original_path.as_deref(), Some("dir/second.txt"));
    assert_eq!(
        file.raw
            .lines()
            .filter(|line| line.starts_with("diff --git "))
            .count(),
        2
    );
    session.input("e");
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("expected renamed file editor: {}", session.message);
    };
    assert_eq!(text.path(), Some(fixture.0.join(renamed).as_path()));
    assert_eq!(text.text(), "renamed body\nkeep1\nkeep2\nkeep3\nkeep4\n");
}

#[test]
fn replacing_a_historical_diff_cancels_its_pending_source_read() {
    let fixture = matching_files();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    session.command("git.page-history").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    settle(&mut session);
    session.command("git-commit-files.preview").unwrap();
    settle(&mut session);
    let preview = session.focus;
    session.input("e");
    assert!(session.pending_diff_source.is_some());
    session
        .request(
            source,
            HostRequest::Preview {
                path: "replacement".into(),
                content: PreviewContent::Text {
                    text: "replacement".into(),
                },
                focus: false,
            },
        )
        .unwrap();
    assert!(session.pending_diff_source.is_none());
    settle(&mut session);
    assert_eq!(session.focus, preview);
    assert!(session.views.child(preview).is_none());
    assert!(
        !session
            .buffers
            .values()
            .any(|buffer| matches!(buffer, Buffer::Snapshot { .. }))
    );
}

#[test]
fn directory_navigation_keeps_the_old_group_until_the_latest_directory_is_ready() {
    let fixture = matching_files();
    fs::create_dir(fixture.0.join("other")).unwrap();
    fs::write(fixture.0.join("other/other.txt"), "other directory\n").unwrap();
    let config = fixture.0.join(".git/debounce.w");
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 0\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    let source = read_directory(&mut session);
    session.input("Esc");
    let preview = session.preview_target(source).unwrap();
    let buffer = session.views[&preview].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff");
    };
    let previous = diff.raw.clone();
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 60000\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    let rows = &session.views[&source].location.display.rows;
    let other = rows
        .iter()
        .position(|row| row.id == "directory:other")
        .unwrap();
    for _ in 0..other {
        session.input("j");
    }
    assert_eq!(selected_row(&session, source).id, "directory:other");
    session.dispatch(source, "tick", Value::Null).unwrap();
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected retained diff");
    };
    assert_eq!(diff.raw, previous);
    session.command("git.preview").unwrap();
    settle(&mut session);
    let Buffer::PreviewDiff { path, diff } =
        &session.buffers[&session.views[&session.focus].location.buffer]
    else {
        panic!("expected latest directory diff");
    };
    assert_eq!(path, "other");
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].target.path, "other/other.txt");
}

#[test]
fn a_split_diff_opens_its_selected_worktree_file_in_an_editor() {
    let fixture = matching_files();
    let mut session = fixture.session(&[]);
    read_directory(&mut session);
    let original = session.focus;
    select_file_line(&mut session, "dir/second.txt", "+changed", false);
    let buffer = session.views[&original].location.buffer;
    session.command("vsplit").unwrap();
    let independent = session.focus;
    assert_eq!(session.views[&independent].location.buffer, buffer);
    session.input("e");
    let layer = session.focus;
    assert_eq!(session.views.parent(layer), Some(independent));
    let Buffer::Text(text) = &session.buffers[&session.views[&layer].location.buffer] else {
        panic!("expected file layer: {}", session.message);
    };
    assert_eq!(
        text.path(),
        Some(fixture.0.join("dir/second.txt").as_path())
    );
    session.command("quit").unwrap();
    assert_eq!(session.focus, independent);
    for key in ["g", "f"] {
        session.input(key);
    }
    assert_eq!(session.focus, independent);
    let Buffer::Text(text) = &session.buffers[&session.views[&independent].location.buffer] else {
        panic!("expected file editor: {}", session.message);
    };
    assert_eq!(
        text.path(),
        Some(fixture.0.join("dir/second.txt").as_path())
    );
    assert_eq!(text.text(), "changed\n");
    assert_eq!(session.views[&original].location.buffer, buffer);
    assert!(matches!(
        session.buffers[&buffer],
        Buffer::PreviewDiff { .. }
    ));
}

#[test]
fn replacing_a_shared_diff_cancels_source_reads_from_its_other_views() {
    let fixture = matching_files();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    session.command("git.page-history").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    let original = session.focus;
    let buffer = session.views[&original].location.buffer;
    session.command("vsplit").unwrap();
    settle(&mut session);
    let independent = session.focus;
    assert_eq!(session.views[&independent].location.buffer, buffer);
    session.input("e");
    assert!(session.pending_diff_source.is_some());
    session
        .request(
            source,
            HostRequest::Preview {
                path: "replacement".into(),
                content: PreviewContent::Text {
                    text: "replacement".into(),
                },
                focus: false,
            },
        )
        .unwrap();
    assert_eq!(session.views[&original].location.buffer, buffer);
    assert_eq!(session.views[&independent].location.buffer, buffer);
    assert!(session.pending_diff_source.is_none());
    settle(&mut session);
    assert_eq!(session.focus, independent);
    assert!(session.views.child(independent).is_none());
    assert!(
        !session
            .buffers
            .values()
            .any(|buffer| matches!(buffer, Buffer::Snapshot { .. }))
    );
    let Buffer::PreviewText { text, .. } = &session.buffers[&buffer] else {
        panic!("expected shared replacement");
    };
    assert_eq!(text.text(), "replacement");
}

#[test]
fn script_diff_previews_open_their_explicit_source_path_without_git_metadata() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "prefix\nchanged\n").unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"
require "zess/buffer"
require "zess/ui"
register-command "raw-diff" "Read a patch with an explicit source file"
  lambda : argument
    request-preview "note.txt" (preview-diff "@@ -2 +2 @@\n-old\n+changed\n") #t
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("raw-diff").unwrap();
    let preview = session.focus;
    let buffer = session.views[&preview].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected script diff");
    };
    assert!(diff.source.is_none());
    assert!(
        session
            .execute_command("diff.apply-selection", json!({}))
            .is_err()
    );
    session.input("e");
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("expected file layer: {}", session.message);
    };
    assert_eq!(text.path(), Some(fixture.0.join("note.txt").as_path()));
    assert_eq!(text.view(editor).cursor().0, 1);
    session.command("quit").unwrap();
    assert_eq!(session.focus, preview);
    for key in ["g", "f"] {
        session.input(key);
    }
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("expected source editor: {}", session.message);
    };
    assert_eq!(text.path(), Some(fixture.0.join("note.txt").as_path()));
    assert_eq!(text.view(editor).cursor().0, 1);
    assert_eq!(text.text(), "prefix\nchanged\n");
}

#[test]
fn a_pending_directory_group_keeps_its_old_contents_readonly_until_publication() {
    let fixture = matching_files();
    fs::create_dir(fixture.0.join("other")).unwrap();
    fs::write(fixture.0.join("other/other.txt"), "other directory\n").unwrap();
    let config = fixture.0.join(".git/debounce.w");
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 0\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    let source = read_directory(&mut session);
    let preview = session.focus;
    select_file_line(&mut session, "dir/second.txt", "+changed", false);
    session.input("Esc");
    fs::write(
        &config,
        "set-editor-options! : obj 'preview-debounce-ms 60000\n",
    )
    .unwrap();
    session.command("config-reload").unwrap();
    let other = session.views[&source]
        .location
        .display
        .rows
        .iter()
        .position(|row| row.id == "directory:other")
        .unwrap();
    for _ in 0..other {
        session.input("j");
    }
    assert_eq!(selected_row(&session, source).id, "directory:other");
    let group = session.views.page(source).previews.as_ref().unwrap();
    assert!(group.publication == super::super::super::pages::PreviewPublication::Pending);
    assert!(!session.views[&preview].location.display.loading);
    let before = git(&fixture, &["write-tree"]);
    session.focus_view(preview).unwrap();
    for command in [
        "diff.apply-selection",
        "diff.edit-source",
        "preview.open-source",
    ] {
        let error = session.execute_command(command, json!({})).unwrap_err();
        assert_eq!(error.to_string(), "wait for Git previews to finish loading");
        assert_eq!(session.focus, preview);
        assert!(session.views.child(preview).is_none());
        assert!(session.git_patch.is_none());
    }
    assert_eq!(git(&fixture, &["write-tree"]), before);
    session.focus_view(source).unwrap();
    session.input("Enter");
    settle(&mut session);
    let group = session.views.page(source).previews.as_ref().unwrap();
    assert!(group.publication == super::super::super::pages::PreviewPublication::Ready);
    select_file_line(&mut session, "other/other.txt", "+other directory", false);
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    assert_eq!(session.message, "Git index updated");
    assert_eq!(
        git(&fixture, &["show", ":other/other.txt"]),
        "other directory"
    );
    assert_eq!(git(&fixture, &["show", ":dir/second.txt"]), "baseline");
}

#[test]
fn a_published_group_stays_actionable_while_only_the_status_list_refreshes() {
    let fixture = matching_files();
    let mut session = fixture.session(&[]);
    let source = read_directory(&mut session);
    let preview = session.focus;
    select_file_line(&mut session, "dir/second.txt", "-baseline", false);
    session
        .dispatch(
            source,
            "fs_changed",
            json!({"queries":[{"operation":"git.status", "argument":{}}]}),
        )
        .unwrap();
    assert!(session.views[&source].location.display.loading);
    assert!(
        session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .publication
            == super::super::super::pages::PreviewPublication::Ready
    );
    session.input("e");
    let layer = session.focus;
    assert_eq!(session.views.parent(layer), Some(preview));
    session.command("quit").unwrap();
    assert_eq!(session.focus, preview);
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    assert_eq!(session.message, "Git index updated");
    assert_eq!(git(&fixture, &["show", ":dir/second.txt"]), "changed");
    assert_eq!(git(&fixture, &["show", ":dir/first.txt"]), "baseline");
}

#[test]
fn script_diff_sources_keep_the_ability_to_open_a_file_that_does_not_yet_exist() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"
require "zess/buffer"
require "zess/ui"
register-command "raw-diff" "Read a patch for an explicit new file"
  lambda : argument
    request-preview "new.txt" (preview-diff "@@ -0,0 +1 @@\n+new\n") #t
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("raw-diff").unwrap();
    let preview = session.focus;
    session.input("e");
    let layer = session.focus;
    assert_ne!(layer, preview);
    assert_eq!(session.views.parent(layer), Some(preview));
    let Buffer::Text(text) = &session.buffers[&session.views[&layer].location.buffer] else {
        panic!("expected empty file layer: {}", session.message);
    };
    assert_eq!(text.path(), Some(fixture.0.join("new.txt").as_path()));
    assert_eq!(text.text(), "");
    assert!(!fixture.0.join("new.txt").exists());
    session.command("quit").unwrap();
    assert_eq!(session.focus, preview);
    for key in ["g", "f"] {
        session.input(key);
    }
    let Buffer::Text(text) = &session.buffers[&session.views[&session.focus].location.buffer]
    else {
        panic!("expected empty source editor: {}", session.message);
    };
    assert_eq!(text.path(), Some(fixture.0.join("new.txt").as_path()));
    assert_eq!(text.text(), "");
    assert!(!fixture.0.join("new.txt").exists());
}
