use super::*;

mod file_actions;
mod grid;
mod history;
mod multi_file_diff;
mod page_return;
mod preview_loading;
mod staging;
mod temporary_navigation;
mod toggle_view;
mod totals;
mod tree;

fn trailing_text(row: &DisplayRow) -> String {
    row.cells
        .iter()
        .skip(1)
        .map(|cell| {
            cell.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn git(fixture: &Fixture, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Zess Tests",
            "-c",
            "user.email=zess@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(&fixture.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn repository() -> (Fixture, PathBuf) {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "historical\n").unwrap();
    git(&fixture, &["init", "--quiet"]);
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "initial"]);
    fs::write(&path, "working\n").unwrap();
    (fixture, path)
}

fn nested_changes() -> Fixture {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::create_dir_all(fixture.0.join("dir/nested/deeper")).unwrap();
    for path in [
        "dir/modified.txt",
        "dir/nested/deleted.txt",
        "dir/unchanged.txt",
    ] {
        fs::write(fixture.0.join(path), "baseline\n").unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("dir/modified.txt"), "staged\n").unwrap();
    git(&fixture, &["add", "dir/modified.txt"]);
    fs::write(fixture.0.join("dir/modified.txt"), "worktree\n").unwrap();
    fs::remove_file(fixture.0.join("dir/nested/deleted.txt")).unwrap();
    fs::write(fixture.0.join("dir/nested/deeper/new.txt"), "untracked\n").unwrap();
    fixture
}

#[test]
fn directory_diffs_include_all_changed_descendants_of_collapsed_directories() {
    let fixture = nested_changes();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["g", "e", "h", "h"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "directory:dir");
    let group = session.views.page(source).previews.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    for (slot, side, expected) in [
        (
            "unstaged",
            zess_git::DiffSide::Unstaged,
            vec![
                "dir/nested/deeper/new.txt",
                "dir/nested/deleted.txt",
                "dir/modified.txt",
            ],
        ),
        (
            "staged",
            zess_git::DiffSide::Staged,
            vec!["dir/modified.txt"],
        ),
    ] {
        let member = group
            .members
            .iter()
            .find(|member| member.id == slot)
            .unwrap();
        let Buffer::PreviewDiff { path, diff } = &session.buffers[&member.buffer()] else {
            panic!("expected directory diff");
        };
        assert_eq!(path, "dir");
        assert_eq!(diff.source, Some(zess_git::DiffSource::Changes { side }));
        assert_eq!(
            diff.files
                .iter()
                .map(|file| file.target.path.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        let bodies = diff
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind == Some(zess_git::patch::LineKind::Addition))
            .map(|(index, _)| diff.line(index))
            .collect::<Vec<_>>();
        assert_eq!(
            bodies,
            if slot == "unstaged" {
                vec!["+untracked", "+worktree"]
            } else {
                vec!["+staged"]
            }
        );
    }
    assert_eq!(session.focus, source);
}

#[test]
fn returning_from_a_directory_diff_restores_the_single_file_preview_in_the_same_views() {
    let fixture = nested_changes();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["g", "e"] {
        session.input(key);
    }
    settle(&mut session);
    let previous = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .map(|member| {
            (
                member.id.clone(),
                member.view().unwrap().id,
                member.buffer(),
            )
        })
        .collect::<Vec<_>>();
    session.input("h");
    settle(&mut session);
    for key in ["g", "e"] {
        session.input(key);
    }
    settle(&mut session);
    let group = session.views.page(source).previews.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    for (slot, view, buffer) in previous {
        let member = group
            .members
            .iter()
            .find(|member| member.id == slot)
            .unwrap();
        assert_eq!(member.view().unwrap().id, view);
        assert_eq!(member.buffer(), buffer);
        let Buffer::PreviewDiff { path, diff } = &session.buffers[&buffer] else {
            panic!("expected file diff");
        };
        assert_eq!(path, "dir/modified.txt");
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].target.path, "dir/modified.txt");
    }
    assert_eq!(session.focus, source);
}

#[test]
fn quitting_a_directory_diff_returns_to_the_original_editor() {
    let fixture = nested_changes();
    let mut session = fixture.session(&[]);
    let buffer = session.views[&session.focus].location.buffer;
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["g", "e", "h"] {
        session.input(key);
    }
    session.command("git.preview").unwrap();
    settle(&mut session);
    assert_ne!(session.focus, source);
    session.command("quit").unwrap();
    assert_eq!(session.focus, source);
    settle(&mut session);
    assert_eq!(session.views[&source].location.buffer, buffer);
    assert!(preview_groups(&session).next().is_none());
    assert_eq!(session.views.values().count(), 1);
    assert!(!session.exiting());
}

#[test]
fn historical_directory_diffs_use_commit_files_after_the_directory_is_deleted() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::create_dir_all(fixture.0.join("archive/nested")).unwrap();
    fs::write(fixture.0.join("archive/nested/old.txt"), "historical\n").unwrap();
    fs::write(fixture.0.join("archive/top.txt"), "old top\n").unwrap();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "historical files"]);
    fs::remove_dir_all(fixture.0.join("archive")).unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["C-w", "2", "Enter"] {
        session.input(key);
        settle(&mut session);
    }
    for key in ["g", "e", "h"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).text, "▾ archive");
    let target = session.preview_target(source).unwrap();
    let Buffer::PreviewDiff { path, diff } =
        &session.buffers[&session.views[&target].location.buffer]
    else {
        panic!("expected historical directory diff");
    };
    assert_eq!(path, "archive");
    assert_eq!(
        diff.files
            .iter()
            .map(|file| file.target.path.as_str())
            .collect::<Vec<_>>(),
        ["archive/nested/old.txt", "archive/top.txt"]
    );
    assert!(!fixture.0.join("archive").exists());
    for key in ["g", "e", "Enter"] {
        session.input(key);
    }
    settle(&mut session);
    let target = session.preview_target(source).unwrap();
    assert_eq!(session.focus, target);
    let Buffer::PreviewDiff { path, diff } =
        &session.buffers[&session.views[&target].location.buffer]
    else {
        panic!("expected historical file diff")
    };
    assert_eq!(path, "archive/top.txt");
    assert_eq!(
        diff.raw.lines().skip(5).collect::<Vec<_>>(),
        ["@@ -0,0 +1 @@", "+old top"]
    );
}

#[test]
fn reading_a_diff_and_returning_to_changes_follow_configured_bindings() {
    let (fixture, _) = repository();
    for (read, back, custom) in [("Enter", "Esc", false), ("o", "q", true)] {
        let mut session = fixture.session(&[]);
        if custom {
            session
                .scripts
                .as_mut()
                .unwrap()
                .load_source(
                    "map-modal \"git\" \"o\" \"git.open\"\nmap-key \"git-diff\" \"normal\" \"q\" \"preview.return\"",
                    Path::new("custom-key.w"),
                )
                .unwrap();
        }
        session.command("git").unwrap();
        settle(&mut session);
        let source = session.focus;
        let source_buffer = session.views[&source].location.buffer;
        let preview = session.preview_target(source).unwrap();
        let layout = panes(&session);
        session.input(read);
        settle(&mut session);
        assert_eq!(session.focus, preview, "{}", session.message);
        let buffer = session.views[&preview].location.buffer;
        let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
            panic!("{}", session.message)
        };
        assert_eq!(
            diff.raw.lines().skip(4).collect::<Vec<_>>(),
            ["@@ -1 +1 @@", "-historical", "+working"]
        );
        session.input("v");
        session.input("Esc");
        assert_eq!(session.focus, preview);
        assert!(matches!(
            session.views[&preview].location.diff_selection,
            diff::Selection::Line { .. }
        ));
        session.input(back);
        assert_eq!(session.focus, source, "{}", session.message);
        assert_eq!(session.views[&source].location.buffer, source_buffer);
        assert_eq!(session.preview_target(source).unwrap(), preview);
        assert_eq!(panes(&session), layout);
    }
}

#[test]
fn enter_can_be_rebound_to_an_edit_layer_that_stays_open_after_write() {
    let (fixture, path) = repository();
    let mut session = fixture.session(&[]);
    session
        .scripts
        .as_mut()
        .unwrap()
        .load_source(
            "map-modal \"git\" \"Enter\" \"git.edit-layer\"",
            Path::new("custom-key.w"),
        )
        .unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    let layer = session.focus;
    assert_ne!(layer, source);
    assert_eq!(session.views[&source].layer, Some(layer));
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    input_command(&mut session, "w");
    settle(&mut session);
    assert_eq!(fs::read_to_string(&path).unwrap(), "!working\n");
    assert_eq!(session.focus, layer);
    assert_eq!(session.views[&source].layer, Some(layer));
    input_command(&mut session, "q");
    assert_eq!(session.focus, source);
    assert!(!session.views.contains_key(&layer));
}

#[test]
fn enter_reads_historical_diffs_and_escape_returns_through_the_history() {
    let (fixture, path) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("C-w");
    session.input("]");
    settle(&mut session);
    let history = session.views[&source].location.buffer;
    session.input("Enter");
    settle(&mut session);
    assert_eq!(session.focus, source);
    let files = session.views[&source].location.buffer;
    assert_ne!(files, history);
    session.input("Enter");
    settle(&mut session);
    let preview = session.preview_target(source).unwrap();
    assert_eq!(session.focus, preview);
    let buffer = session.views[&preview].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("{}", session.message)
    };
    assert_eq!(
        diff.raw.lines().skip(5).collect::<Vec<_>>(),
        ["@@ -0,0 +1 @@", "+historical"]
    );
    assert!(session.command("write note.txt").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");
    session.input("Esc");
    assert_eq!(session.focus, source);
    assert_eq!(session.views[&source].location.buffer, files);
    session.input("Esc");
    settle(&mut session);
    assert_eq!(session.views[&source].location.buffer, history);
}

#[test]
fn numbered_pages_select_changes_and_history_without_toggling_the_current_page() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let changes = session.views[&source].location.buffer;
    for key in ["C-w", "1"] {
        session.input(key);
    }
    assert_eq!(session.views[&source].location.buffer, changes);
    assert_eq!(view_rows(&mut session, source)[0], "[Changes]  History");
    session.input("C-w");
    assert_eq!(view_rows(&mut session, source)[0], "[1 Changes]  2 History");
    session.input("Esc");
    assert_eq!(view_rows(&mut session, source)[0], "[Changes]  History");
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    let history = session.views[&source].location.buffer;
    assert_ne!(history, changes);
    assert_eq!(
        session.views[&source]
            .location
            .display
            .tabs
            .iter()
            .map(|tab| (tab.label.as_str(), tab.command.as_str(), tab.active))
            .collect::<Vec<_>>(),
        [
            ("Changes", "git-history.page-changes", false),
            ("History", "git-history.page-history", true),
        ]
    );
    assert_eq!(view_rows(&mut session, source)[0], "Changes  [History]");
    session.input("C-w");
    assert_eq!(view_rows(&mut session, source)[0], "1 Changes  [2 History]");
    session.input("Esc");
    for key in ["C-w", "2"] {
        session.input(key);
    }
    assert_eq!(session.views[&source].location.buffer, history);
    for key in ["C-w", "1"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(session.views[&source].location.buffer, changes);
    assert_eq!(
        session.views[&source]
            .location
            .display
            .tabs
            .iter()
            .map(|tab| (tab.label.as_str(), tab.command.as_str(), tab.active))
            .collect::<Vec<_>>(),
        [
            ("Changes", "git.page-changes", true),
            ("History", "git.page-history", false),
        ]
    );
    let (_, _, _, width, height) = panes(&session)
        .into_iter()
        .find(|(id, ..)| *id == source)
        .unwrap();
    let (rows, _) = session.render_view(source, width, height);
    assert_eq!(rows[0].spans[0].text.trim_end(), "[Changes]  History");
    assert_eq!(rows[0].spans[0].style, "");
}

fn group_request(text: &str, loading: bool) -> HostRequest {
    HostRequest::PreviewGroup {
        panes: ["first", "second"]
            .into_iter()
            .map(|id| PreviewPane::View {
                id: id.into(),
                path: format!("{id}.txt"),
                header: id.into(),
                key_scope: None,
                loading,
                content: PreviewContent::Text { text: text.into() },
            })
            .collect(),
        layout: PreviewLayout::Responsive { min_pane_width: 60 },
        focus: false,
    }
}

#[test]
fn loading_updates_preserve_preview_group_members_and_geometry() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session.resize(300, 24);
    session.request(source, group_request("", true)).unwrap();
    let before = panes(&session);
    let members = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .map(|member| {
            (
                member.view().unwrap().id,
                session.views[&member.view().unwrap().id].location.buffer,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(before.len(), 3);
    assert!(
        members
            .iter()
            .all(|(id, _)| session.views[id].location.display.loading)
    );
    session
        .request(source, group_request("ready\n", false))
        .unwrap();
    assert_eq!(panes(&session), before);
    assert!(
        members
            .iter()
            .all(|(id, buffer)| session.views[id].location.buffer == *buffer
                && !session.views[id].location.display.loading)
    );
    session.resize(80, 24);
    let narrow = panes(&session);
    session
        .request(source, group_request("ready\n", true))
        .unwrap();
    session.frame();
    assert_eq!(panes(&session), narrow);
    assert_eq!(session.focus, source);
    for (view, _) in members {
        let buffer = session.views[&view].location.buffer;
        let Buffer::PreviewText { text, .. } = session.buffers.get_mut(&buffer).unwrap() else {
            panic!("expected text preview")
        };
        assert_eq!(text.text(), "ready\n");
    }
}

#[test]
fn quitting_either_diff_pane_closes_the_git_page_and_releases_its_previews() {
    let (fixture, path) = repository();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "another worktree change\n").unwrap();
    for slot in ["unstaged", "staged"] {
        let mut session = fixture.session(&[]);
        let buffer = session.views[&session.focus].location.buffer;
        session.command("git").unwrap();
        let source = session.focus;
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
        let members = session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .iter()
            .map(|member| {
                (
                    member.view().unwrap().id,
                    session.views[&member.view().unwrap().id].location.buffer,
                )
            })
            .collect::<Vec<_>>();
        let target = session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .iter()
            .find(|member| member.id == slot)
            .unwrap()
            .view()
            .unwrap()
            .id;
        session.focus_view(target).unwrap();
        session.command("quit").unwrap();
        settle(&mut session);
        assert_eq!(session.focus, source);
        assert_eq!(session.views[&source].location.buffer, buffer);
        assert!(!session.exiting());
        assert!(preview_groups(&session).next().is_none());
        assert_eq!(panes(&session).len(), 1);
        assert!(
            members
                .iter()
                .all(|(view, buffer)| !session.views.contains_key(view)
                    && !session.buffers.contains_key(buffer))
        );
        let stats = session
            .services
            .borrow_mut()
            .call("service.stats", json!({}));
        assert_eq!(stats["jobs"], 0);
        assert_eq!(stats["observed_queries"], 0);
        session.command("git").unwrap();
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
        assert_eq!(session.focus, source);
    }
}

#[test]
fn leaving_changes_releases_both_diff_previews_and_their_observations() {
    let (fixture, path) = repository();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "another worktree change\n").unwrap();
    for (command, file_queries) in [("edit note.txt", 1), ("quit", 0)] {
        let mut session = fixture.session(&[]);
        session.command("git").unwrap();
        settle(&mut session);
        let source = session.focus;
        let members = session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .iter()
            .map(|member| {
                (
                    member.view().unwrap().id,
                    session.views[&member.view().unwrap().id].location.buffer,
                )
            })
            .collect::<Vec<_>>();
        session.command(command).unwrap();
        assert!(preview_groups(&session).next().is_none());
        assert!(
            members
                .iter()
                .all(|(view, buffer)| !session.views.contains_key(view)
                    && !session.buffers.contains_key(buffer))
        );
        let stats = session
            .services
            .borrow_mut()
            .call("service.stats", json!({}));
        assert_eq!(stats["jobs"], 0);
        assert_eq!(stats["observed_queries"], file_queries);
        assert!(!session.exiting());
    }
}

#[test]
fn owned_preview_and_layer_buffers_cannot_be_selected_as_editor_buffers() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let owned = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .map(|member| session.views[&member.view().unwrap().id].location.buffer)
        .collect::<Vec<_>>();
    let layout = panes(&session);
    session.command("buffers").unwrap();
    let candidates = session
        .message
        .split_whitespace()
        .map(|id| id.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    for buffer in owned {
        assert!(!candidates.contains(&buffer));
        assert!(session.command(&format!("buffer {buffer}")).is_err());
        assert_eq!(session.focus, source);
        assert_eq!(panes(&session), layout);
    }
    session.input("e");
    let layer = session.focus;
    let temporary = session.views[&layer].location.buffer;
    session.command("buffers").unwrap();
    assert!(
        !session
            .message
            .split_whitespace()
            .any(|id| id == temporary.to_string())
    );
    assert!(session.command(&format!("buffer {temporary}")).is_err());
    assert_eq!(session.focus, layer);
    assert_eq!(panes(&session), layout);
}

#[test]
fn unchanged_preview_refresh_keeps_the_active_text_prompt() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session
        .request(source, group_request("one two\n", false))
        .unwrap();
    session.focus_preview().unwrap();
    let preview = session.focus;
    let buffer = session.views[&preview].location.buffer;
    let layout = panes(&session);
    session.input("s");
    session.input("o");
    assert!(session.prompt.is_some());
    session
        .request(source, group_request("one two\n", false))
        .unwrap();
    assert!(session.prompt.is_some());
    assert_eq!(session.focus, preview);
    assert_eq!(session.views[&preview].location.buffer, buffer);
    assert_eq!(panes(&session), layout);
    session.input("Enter");
    assert!(session.prompt.is_none());
    assert_eq!(session.message, "");
}

#[test]
fn refreshing_the_same_snapshot_preserves_its_reader_and_readonly_content() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    let snapshot = FileSnapshot {
        id: SnapshotId {
            repository: fixture.0.to_string_lossy().into_owned(),
            commit: "commit".into(),
            path: "note.txt".into(),
            blob: "blob".into(),
        },
        text: "first\nsecond\nthird\n".into(),
    };
    session
        .show_preview(
            session.views.page_id(source),
            "Snapshot".into(),
            PreviewContent::Snapshot {
                snapshot: snapshot.clone(),
            },
            true,
        )
        .unwrap();
    let preview = session.focus;
    let buffer = session.views[&preview].location.buffer;
    session.input("j");
    let layout = panes(&session);
    session
        .show_preview(
            session.views.page_id(source),
            "Snapshot".into(),
            PreviewContent::Snapshot {
                snapshot: snapshot.clone(),
            },
            false,
        )
        .unwrap();
    assert_eq!(session.focus, preview);
    assert_eq!(session.views[&preview].location.buffer, buffer);
    assert_eq!(panes(&session), layout);
    let Buffer::PreviewText {
        text,
        snapshot: identity,
        ..
    } = &mut session.buffers.get_mut(&buffer).unwrap()
    else {
        panic!("expected snapshot preview")
    };
    assert_eq!(identity.as_ref(), Some(&snapshot.id));
    assert_eq!(text.view(preview).cursor().0, 1);
    assert_eq!(text.text(), snapshot.text);
    assert!(session.command("write note.txt").is_err());
    assert!(!fixture.0.join("note.txt").exists());
}

#[test]
fn replacing_preview_contents_closes_a_text_prompt_whose_selection_was_invalidated() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session
        .request(source, group_request("one two\n", false))
        .unwrap();
    session.focus_preview().unwrap();
    session.input("s");
    session.input("o");
    assert!(session.prompt.is_some());
    session
        .request(source, group_request("x\n", false))
        .unwrap();
    assert!(session.prompt.is_none());
    session.input("Enter");
    session.frame();
    let buffer = session.views[&session.focus].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "x\n"
    );
}

#[test]
fn initial_diff_position_preserves_patch_headers_and_later_user_navigation() {
    let fixture = Fixture::new();
    for (line_count, height) in [(2, 20), (40, 8)] {
        let mut session = fixture.session(&[]);
        let source = session.focus;
        let body = (0..line_count)
            .map(|line| format!("+line {line}\n"))
            .collect::<String>();
        let patch = format!(
            "diff --git a/note.txt b/note.txt\nindex abc..def 100644\n--- a/note.txt\n+++ b/note.txt\n@@ -0,0 +1,{line_count} @@\n{body}"
        );
        session
            .show_preview(
                session.views.page_id(source),
                "note.txt".into(),
                PreviewContent::Diff {
                    text: patch.clone(),
                },
                true,
            )
            .unwrap();
        let target = session.focus;
        let buffer = session.views[&target].location.buffer;
        let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
            panic!("expected diff")
        };
        assert_eq!(
            session.views[&target].location.diff_selection.row(),
            Some(5)
        );
        assert_eq!(diff.raw, patch);
        let (rows, _) = session.render_view(target, 60, height);
        let first = rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(
            first.trim_end(),
            if line_count == 2 {
                "  diff --git a/note.txt b/note.txt".to_owned()
            } else {
                format!("   @@ -0,0 +1,{line_count} @@")
            }
        );
        let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
            panic!("expected diff")
        };
        let selection = &mut session
            .views
            .get_mut(&target)
            .unwrap()
            .location
            .diff_selection;
        selection.toggle(diff);
        selection.move_to(diff, 6);
        let updated_patch = format!(
            "{}+later\n",
            patch.replace(
                &format!("+1,{line_count} @@"),
                &format!("+1,{} @@", line_count + 1)
            )
        );
        session
            .show_preview(
                session.views.page_id(source),
                "note.txt".into(),
                PreviewContent::Diff {
                    text: updated_patch.clone(),
                },
                false,
            )
            .unwrap();
        let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
            panic!("expected diff")
        };
        assert_eq!(
            session.views[&target].location.diff_selection.row(),
            Some(6)
        );
        assert_eq!(diff.raw, updated_patch);
        assert_eq!(session.focus, target);
        assert_eq!(session.views[&target].location.buffer, buffer);
    }
}

#[test]
fn reloading_shared_text_closes_a_prompt_whose_selection_was_invalidated() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "one two\n").unwrap();
    let mut session = fixture.session(std::slice::from_ref(&path));
    session.command("vsplit").unwrap();
    session.input("s");
    session.input("o");
    assert!(session.prompt.is_some());
    fs::write(&path, "x\n").unwrap();
    session.command("reload").unwrap();
    session.frame();
    assert!(session.prompt.is_none());
    session.input("Enter");
    let buffer = session.views[&session.focus].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "x\n"
    );
}

#[test]
fn opening_files_from_help_pinned_beside_a_diff_keeps_its_previews_visible() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.resize(180, 40);
    session.command("git").unwrap();
    settle(&mut session);
    let git_source = session.focus;
    session.input("Enter");
    for key in ["space", "?", "p"] {
        session.input(key);
    }
    assert!(session.help.is_none());
    let files_source = session.focus;
    assert_ne!(files_source, git_source);
    session.command("files").unwrap();
    settle(&mut session);
    let target = session.active_preview(files_source).unwrap();
    assert!(
        session
            .visible_rectangles()
            .iter()
            .any(|(current, rect)| current == &target && rect.width > 0 && rect.height > 0)
    );
    let layout = panes(&session);
    for id in [git_source, files_source] {
        assert!(
            layout
                .iter()
                .any(|(view, _, _, width, height)| *view == id && *width > 0 && *height > 0)
        );
    }
    session.command("files.preview").unwrap();
    settle(&mut session);
    assert_eq!(session.focus, files_source);
    assert!(panes(&session).iter().any(|(id, ..)| *id == session.focus));
    assert_eq!(selected_row(&session, files_source).id, ".git");
    let buffer = preview_buffer(&session, files_source);
    assert!(
        matches!(&session.buffers[&buffer], Buffer::PreviewRows { kind: PreviewRowKind::Directory, path, .. } if path == ".git")
    );
}

#[test]
fn help_pinned_beside_staged_diff_survives_page_changes_and_closing_its_source() {
    let (fixture, path) = repository();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "another worktree change\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let staged = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .find(|member| member.id == "staged")
        .unwrap()
        .view()
        .unwrap()
        .id;
    session.focus_view(staged).unwrap();
    for key in ["space", "?", "p"] {
        session.input(key);
    }
    let help = session.focus;
    assert!(session.help.is_none());
    session.focus_view(source).unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    assert!(panes(&session).iter().any(|(id, ..)| *id == help));
    session.input("Enter");
    settle(&mut session);
    assert!(panes(&session).iter().any(|(id, ..)| *id == help));
    assert!(!session.views.contains_key(&staged));
    session.focus_view(help).unwrap();
    assert_eq!(session.focus, help);
    assert_eq!(session.views[&help].location.display.header, "Help: Diff");
    assert_eq!(view_rows(&mut session, help)[0], "Help: Diff");
    session.focus_view(source).unwrap();
    session.command("quit").unwrap();
    assert!(!session.exiting());
    assert_eq!(session.focus, source);
    assert!(preview_groups(&session).next().is_none());
    session.command("quit").unwrap();
    assert_eq!(session.focus, help);
    assert!(!session.views.contains_key(&source));
    assert!(preview_groups(&session).next().is_none());
    assert_eq!(
        panes(&session)
            .iter()
            .map(|(id, ..)| *id)
            .collect::<Vec<_>>(),
        [help]
    );
    session.frame();
}

#[test]
fn user_view_options_override_plugin_defaults_and_survive_selection_and_background_updates() {
    let (fixture, path) = repository();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"require "zess/ui"
require "zess/status"
define : custom-statusline context
  statusline "CUSTOM STATUS" "" ""
register-statusline "custom" custom-statusline
set-view-options! "git" : obj 'header-style "keyword"
register-command "customize-git"
  lambda : argument
    configure-view : obj 'key-scope "user/git" 'statusline "custom"
register-command "custom-key"
  lambda : argument
    request-message "USER KEY"
map-modal "user/git" "z" "custom-key"
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("git").unwrap();
    let source = session.focus;
    session.command("customize-git").unwrap();
    let options = json!({"key-scope":"user/git", "statusline":"custom"});
    settle(&mut session);
    assert_eq!(session.views[&source].location.options, options);
    let primary = session.views[&source].location.selection.primary.clone();
    session.input("j");
    assert_ne!(session.views[&source].location.selection.primary, primary);
    for key in ["/", "n", "o", "t", "e", "Enter", "R"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(selected_row(&session, source).id, "file:note.txt");
    assert_eq!(trailing_text(selected_row(&session, source)), "+1 -1");
    assert_eq!(session.views[&source].location.options, options);
    fs::write(&path, "refreshed worktree\n").unwrap();
    wait_until(&mut session, |session| {
        let target = session.preview_target(source).unwrap();
        matches!(&session.buffers[&session.views[&target].location.buffer], Buffer::PreviewDiff { diff, .. } if diff.raw.lines().skip(4).collect::<Vec<_>>() == ["@@ -1 +1 @@", "-historical", "+refreshed worktree"])
    });
    assert_eq!(session.views[&source].location.options, options);
    session.input("z");
    assert_eq!(session.message, "USER KEY");
    let (_, _, _, width, height) = panes(&session)
        .into_iter()
        .find(|(id, ..)| *id == source)
        .unwrap();
    let (rows, _) = session.render_view(source, width, height);
    assert_eq!(
        rows[0].spans[0].text.trim_end(),
        "[Changes]  History    /note"
    );
    assert_eq!(rows[0].spans[0].style, "keyword");
    assert_eq!(
        rows.last()
            .unwrap()
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "CUSTOM STATUS"
    );
}

#[test]
fn a_split_diff_window_can_browse_files_without_replacing_the_original_diff() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    let original = session.focus;
    let original_buffer = session.views[&original].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&original_buffer] else {
        panic!("expected diff")
    };
    let patch = diff.raw.clone();
    session.command("vsplit").unwrap();
    let independent = session.focus;
    assert_ne!(independent, original);
    assert_eq!(session.preview_source(independent), None);
    session.command("files").unwrap();
    settle(&mut session);
    let file_preview = session.active_preview(independent).unwrap();
    assert!(
        session
            .visible_rectangles()
            .iter()
            .any(|(target, _)| *target == file_preview)
    );
    assert_eq!(session.preview_target(source), Some(original));
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&original_buffer] else {
        panic!("expected diff")
    };
    assert_eq!(diff.raw, patch);
    session.focus_view(source).unwrap();
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(
        session.active_preview(independent),
        Some(file_preview.clone())
    );
    for id in [independent] {
        assert!(panes(&session).iter().any(|(view, ..)| *view == id));
    }
}
