use super::*;

#[test]
fn reopening_git_preserves_the_current_page_and_preview_focus() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    let source = session.focus;
    let buffer = session.views[&source].location.buffer;
    let wake_at = session.views[&source].wake_at;
    session.input("space");
    session.input("g");
    assert_eq!(session.views[&source].location.buffer, buffer);
    assert_eq!(session.views[&source].wake_at, wake_at);
    settle(&mut session);
    for page in 0..3 {
        if page == 1 {
            session.command("git.page-history").unwrap();
            settle(&mut session);
        } else if page == 2 {
            session.input("Enter");
            settle(&mut session);
        }
        let buffer = session.views[&source].location.buffer;
        let selection = session.views[&source].location.selection.primary.clone();
        let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
        for preview in [false, true] {
            if preview {
                session.focus_preview(session.focus).unwrap();
            }
            let focus = session.focus;
            session.command("git").unwrap();
            assert_eq!(session.focus, focus);
            assert_eq!(session.views[&source].location.buffer, buffer);
            assert_eq!(session.views[&source].location.selection.primary, selection);
            assert_eq!(
                session.buffers.keys().copied().collect::<HashSet<_>>(),
                buffers
            );
        }
        session.focus_view(source).unwrap();
    }
    session.command("vsplit").unwrap();
    settle(&mut session);
    let split = session.focus;
    let buffer = session.views[&split].location.buffer;
    session.focus_preview(session.focus).unwrap();
    let focus = session.focus;
    session.command("git").unwrap();
    assert_eq!(session.focus, focus);
    assert_eq!(session.views[&split].location.buffer, buffer);
    assert!(session.views.page(split).navigation.plugin.is_none());
    session.command("files").unwrap();
    settle(&mut session);
    session.command("q").unwrap();
    settle(&mut session);
    assert_eq!(session.views[&split].location.buffer, buffer);
    session.command("git").unwrap();
    assert_eq!(session.views[&split].location.buffer, buffer);
}

#[test]
fn reopening_a_plugin_with_arguments_applies_the_new_request() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a"), "first").unwrap();
    fs::write(fixture.0.join("z"), "last").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    assert_eq!(selected_row(&session, session.focus).id, "a");
    session
        .request(
            session.focus,
            HostRequest::OpenBuffer {
                kind: "files".into(),
                argument: json!({"path":"z"}),
            },
        )
        .unwrap();
    settle(&mut session);
    assert_eq!(selected_row(&session, session.focus).id, "z");
}

#[test]
fn preview_commands_update_the_containing_page_and_release_retired_slots() {
    let scripts = Fixture::new();
    for browser in ["files", "git"] {
        for focus in [false, true] {
            let (fixture, _) = repository();
            let mut session = fixture.session(&[]);
            let source = session.focus;
            let origin = session.views[&source].location.buffer;
            let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
            session.command(browser).unwrap();
            settle(&mut session);
            session.focus_preview(session.focus).unwrap();
            if browser == "git" {
                assert_ne!(session.focus, source);
            } else {
                assert_eq!(session.focus, source);
            }
            session
                .scripts
                .as_mut()
                .unwrap()
                .load(&scripts.write(
                    "preview-commands.w",
                    &format!(
                        r#"
require "zess/buffer"
require "zess/ui"
register-command "trio" "Show three previews"
  lambda : argument
    request-preview-group
      list
        preview-pane "one" "one" : preview-text "one"
        preview-pane "two" "two" : preview-text "two"
        preview-pane "three" "three" : preview-text "three"
      preview-layout-rows
      . #f
register-command "pair" "Show paired previews"
  lambda : argument
    request-preview-group
      list
        preview-pane "before" "before" : preview-text "old\nsecond"
        preview-pane "after" "after" : preview-text "new\nsecond"
      preview-layout-rows
      . {}
"#,
                        if focus { "#t" } else { "#f" }
                    ),
                ))
                .unwrap();
            session.command("trio").unwrap();
            let retired = &session
                .views
                .page(source)
                .previews
                .as_ref()
                .unwrap()
                .members[2]
                .view()
                .unwrap();
            let retired_buffer = retired.location.buffer;
            let retired = retired.id;
            session.focus_view(retired).unwrap();
            session.command("pair").unwrap();

            assert!(!session.views.contains_key(&retired));
            assert!(!session.buffers.contains_key(&retired_buffer));
            let group = session.views.page(source).previews.as_ref().unwrap();
            assert_eq!(
                group
                    .members
                    .iter()
                    .map(|slot| slot.id.as_str())
                    .collect::<Vec<_>>(),
                ["before", "after"]
            );
            assert_eq!(
                session.focus,
                if focus {
                    session.preview_target(source).unwrap()
                } else {
                    source
                }
            );
            for (slot, expected) in group.members.iter().zip(["old\nsecond", "new\nsecond"]) {
                let Buffer::PreviewText { text, .. } =
                    &session.buffers[&slot.view().unwrap().location.buffer]
                else {
                    panic!("expected script preview")
                };
                assert_eq!(text.text(), expected);
                assert!(
                    session.views.page_id(slot.view().unwrap().id) == session.views.page_id(source)
                );
            }
            session.frame();
            session.input("j");
            session.frame();
            session.command("q").unwrap();
            assert!(!session.exiting());
            assert_eq!(session.focus, source);
            assert_eq!(session.views[&source].location.buffer, origin);
            assert_eq!(
                session.buffers.keys().copied().collect::<HashSet<_>>(),
                buffers
            );
        }
    }
}

#[test]
fn help_returns_to_the_page_when_its_original_diff_pane_is_retired() {
    for pinned in [false, true] {
        let (fixture, path) = repository();
        git(&fixture, &["add", "note.txt"]);
        fs::write(&path, "another worktree change\n").unwrap();
        let mut session = fixture.session(&[]);
        let source = session.focus;
        let origin = session.views[&source].location.buffer;
        session.command("git").unwrap();
        settle(&mut session);
        let staged = session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .iter()
            .find(|slot| slot.id == "staged")
            .unwrap()
            .view()
            .unwrap()
            .id;
        session.focus_view(staged).unwrap();
        session.command("help").unwrap();
        let help = session.focus;
        if pinned {
            session.input("p");
            assert!(session.help.is_none());
        }
        git(&fixture, &["reset", "--quiet", "HEAD", "--", "note.txt"]);
        wait_until(&mut session, |session| !session.views.contains_key(&staged));

        assert_eq!(session.focus, help);
        session.frame();
        session.input("Esc");
        assert_eq!(session.focus, source);
        assert!(!session.views.contains_key(&help));
        assert_eq!(selected_row(&session, source).id, "file:note.txt");
        session.frame();
        session.command("q").unwrap();
        assert_eq!(session.views[&source].location.buffer, origin);
        assert!(!session.exiting());
    }
}

#[test]
fn closing_git_returns_to_the_editor_that_opened_it() {
    for source in ["default", "new", "file"] {
        let (fixture, path) = repository();
        let mut session = fixture.session(if source == "file" {
            std::slice::from_ref(&path)
        } else {
            &[]
        });
        if source == "new" {
            input_command(&mut session, "new");
        }
        let editor = session.focus;
        let buffer = session.views[&editor].location.buffer;
        let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
        input_command(&mut session, "git");
        settle(&mut session);
        assert_ne!(session.views[&editor].location.buffer, buffer);

        input_command(&mut session, "q");
        assert!(!session.exiting(), "{source}");
        assert_eq!(session.focus, editor);
        assert_eq!(session.views[&editor].location.buffer, buffer);
        assert!(preview_groups(&session).next().is_none());
        assert_eq!(
            session.buffers.keys().copied().collect::<HashSet<_>>(),
            buffers
        );
        input_command(&mut session, "q");
        assert!(!session.exiting(), "{source}");
    }
}

#[test]
fn closing_git_preserves_unsaved_scratch_text_and_selection() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    for key in ["i", "n", "o", "t", "e", "Esc", "h"] {
        session.input(key);
    }
    let selection = serde_json::to_value(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .selections(editor),
    )
    .unwrap();
    input_command(&mut session, "git");
    settle(&mut session);
    session.focus_preview(session.focus).unwrap();
    input_command(&mut session, "q");
    assert!(!session.exiting());
    assert_eq!(session.views[&editor].location.buffer, buffer);
    let text = session.buffers.get_mut(&buffer).unwrap().text().unwrap();
    assert_eq!(text.text(), "note");
    assert_eq!(
        serde_json::to_value(text.selections(editor)).unwrap(),
        selection
    );
    assert!(text.modified());
}

#[test]
fn quitting_a_git_preview_returns_to_the_original_editor() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    input_command(&mut session, "git");
    settle(&mut session);
    session.focus_preview(session.focus).unwrap();
    assert_ne!(session.focus, editor);
    input_command(&mut session, "q");
    assert!(!session.exiting());
    assert_eq!(session.focus, editor);
    assert_eq!(session.views[&editor].location.buffer, buffer);
    assert!(preview_groups(&session).next().is_none());
    assert_eq!(session.views.values().count(), 1);
    input_command(&mut session, "q");
    assert!(!session.exiting());
}

#[test]
fn switching_git_pages_and_browsers_preserves_the_editor_return_point() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
    input_command(&mut session, "git");
    settle(&mut session);
    session.command("git.page-history").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    assert_eq!(selected_row(&session, editor).id, "file:note.txt");
    session.focus_preview(session.focus).unwrap();
    input_command(&mut session, "files");
    settle(&mut session);
    assert_eq!(session.focus, editor);
    input_command(&mut session, "q");
    assert!(!session.exiting());
    assert_eq!(session.views[&editor].location.buffer, buffer);
    assert_eq!(
        session.buffers.keys().copied().collect::<HashSet<_>>(),
        buffers
    );
}

#[test]
fn closing_navigation_pages_returns_to_the_same_file_editor() {
    for command in ["files", "outline"] {
        let (fixture, path) = repository();
        let mut session = fixture.session(&[path]);
        if command == "outline" {
            session.command("lang rust").unwrap();
        }
        let editor = session.focus;
        let buffer = session.views[&editor].location.buffer;
        input_command(&mut session, command);
        settle(&mut session);
        input_command(&mut session, "q");
        assert!(!session.exiting(), "{command}");
        assert_eq!(session.focus, editor);
        assert_eq!(session.views[&editor].location.buffer, buffer);
    }
}

#[test]
fn opening_the_source_from_git_reuses_its_existing_file_buffer() {
    let (fixture, path) = repository();
    let mut session = fixture.session(std::slice::from_ref(&path));
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    input_command(&mut session, "git");
    settle(&mut session);
    session.command("git.edit").unwrap();
    assert_eq!(session.focus, editor);
    assert_eq!(session.views[&editor].location.buffer, buffer);
    let text = session.buffers.get_mut(&buffer).unwrap().text().unwrap();
    assert_eq!(text.text(), "!working\n");
    assert!(text.modified());
    assert!(preview_groups(&session).next().is_none());
}

#[test]
fn closing_git_keeps_a_split_browser_available() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    input_command(&mut session, "git");
    settle(&mut session);
    session.command("vsplit").unwrap();
    settle(&mut session);
    let split = session.focus;
    let git = session.views[&split].location.buffer;
    session.focus_view(editor).unwrap();
    input_command(&mut session, "q");
    assert!(!session.exiting());
    assert_eq!(session.views[&editor].location.buffer, buffer);
    assert_eq!(session.views[&split].location.buffer, git);
    session.focus_view(split).unwrap();
    assert_eq!(selected_row(&session, split).id, "file:note.txt");
    session.command("git.page-history").unwrap();
    settle(&mut session);
    assert_eq!(
        selected_row(&session, split).id,
        super::git(&fixture, &["rev-parse", "HEAD"])
    );
}

#[test]
fn quitting_a_cloned_git_preview_closes_only_its_split() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session.command("git").unwrap();
    settle(&mut session);
    let buffer = session.views[&source].location.buffer;
    let original = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .map(|member| member.view().unwrap().id)
        .collect::<Vec<_>>();
    session.command("vsplit").unwrap();
    settle(&mut session);
    let split = session.focus;
    assert!(session.views.page(split).navigation.plugin.is_none());
    let previews = session
        .views
        .page(split)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .map(|member| member.view().unwrap().id)
        .collect::<Vec<_>>();
    session.focus_preview(session.focus).unwrap();
    session.command("q").unwrap();
    settle(&mut session);

    assert!(!session.exiting());
    assert_eq!(session.focus, source);
    assert!(!session.views.contains_key(&split));
    assert!(previews.iter().all(|id| !session.views.contains_key(id)));
    assert_eq!(session.views[&source].location.buffer, buffer);
    assert_eq!(selected_row(&session, source).id, "file:note.txt");
    assert_eq!(
        session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .iter()
            .map(|member| member.view().unwrap().id)
            .collect::<Vec<_>>(),
        original
    );
}

#[test]
fn closing_git_in_a_split_preserves_its_files_browser_state() {
    let (fixture, _) = repository();
    fs::create_dir(fixture.0.join("dir")).unwrap();
    fs::write(fixture.0.join("dir/child.txt"), "child\n").unwrap();
    let mut session = fixture.session(&[]);
    input_command(&mut session, "files");
    settle(&mut session);
    let source = session.focus;
    let row = session.views[&source]
        .location
        .display
        .rows
        .iter()
        .position(|row| row.id == "dir")
        .unwrap();
    for _ in 0..row {
        session.input("j");
    }
    assert_eq!(selected_row(&session, source).id, "dir");
    session.input("l");
    settle(&mut session);
    for key in ["/", "d", "i", "r", "Enter"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(session.views[&source].location.display.header, "/dir");
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.id == "dir/child.txt")
    );

    input_command(&mut session, "vsplit");
    settle(&mut session);
    let split = session.focus;
    let buffer = session.views[&split].location.buffer;
    assert_ne!(source, split);
    let before = session.views[&split]
        .location
        .display
        .rows
        .iter()
        .map(|row| (row.id.clone(), row.text.clone(), row.depth))
        .collect::<Vec<_>>();
    input_command(&mut session, "git");
    settle(&mut session);
    let git = session.views[&split].location.buffer;
    assert_ne!(git, buffer);
    input_command(&mut session, "q");
    settle(&mut session);
    session
        .execute_command("data.refresh", serde_json::json!({}))
        .unwrap();
    settle(&mut session);

    assert_eq!(session.focus, split);
    assert_eq!(session.views[&split].location.buffer, buffer);
    assert!(!session.buffers.contains_key(&git));
    assert_eq!(session.views[&split].location.display.header, "/dir");
    assert_eq!(
        session.views[&split]
            .location
            .display
            .rows
            .iter()
            .map(|row| (row.id.clone(), row.text.clone(), row.depth))
            .collect::<Vec<_>>(),
        before
    );
    assert_eq!(session.views[&source].location.buffer, buffer);
    assert_eq!(session.views[&source].location.display.header, "/dir");
}

#[test]
fn pending_file_operations_block_return_to_the_editor() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "working\n").unwrap();
    let mut session = fixture.session(&[]);
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    input_command(&mut session, "files");
    settle(&mut session);
    for key in ["a", "n", "e", "w", ".", "t", "x", "t", "Enter"] {
        session.input(key);
    }
    assert!(!fixture.0.join("new.txt").exists());
    session.focus_preview(session.focus).unwrap();
    let preview = session.focus;
    assert!(session.command("q").is_err());
    assert_eq!(session.focus, preview);
    assert!(session.views.page(editor).previews.as_ref().is_some());
    assert_ne!(session.views[&editor].location.buffer, buffer);
    assert!(!session.exiting());
    session
        .execute_command("preview.return", serde_json::json!({}))
        .unwrap();
    assert_eq!(session.focus, editor);
    session.command("w").unwrap();
    wait_until(&mut session, |_| fixture.0.join("new.txt").exists());
    settle(&mut session);
    input_command(&mut session, "q");
    assert_eq!(session.views[&editor].location.buffer, buffer);
    assert!(!session.exiting());
}

#[test]
fn cycling_buffers_from_git_uses_the_original_editor_buffer() {
    for command in ["bn", "bp"] {
        let (fixture, _) = repository();
        let mut session = fixture.session(&[]);
        let editor = session.focus;
        let first = session.views[&editor].location.buffer;
        for key in ["i", "x", "Esc"] {
            session.input(key);
        }
        session.command("new").unwrap();
        let second = session.views[&editor].location.buffer;
        assert_ne!(first, second);
        input_command(&mut session, "git");
        settle(&mut session);
        session.focus_preview(session.focus).unwrap();
        session.command(command).unwrap();
        assert_eq!(session.focus, editor);
        assert_eq!(session.views[&editor].location.buffer, first);
        assert!(preview_groups(&session).next().is_none());
        assert!(!session.exiting());
    }
}

#[test]
fn cycling_buffers_from_a_split_keeps_the_original_git_page_open() {
    for command in ["bn", "bp"] {
        let (fixture, _) = repository();
        let mut session = fixture.session(&[]);
        let editor = session.focus;
        let first = session.views[&editor].location.buffer;
        for key in ["i", "x", "Esc"] {
            session.input(key);
        }
        input_command(&mut session, "new");
        let second = session.views[&editor].location.buffer;
        input_command(&mut session, "git");
        settle(&mut session);
        let git = session.views[&editor].location.buffer;
        input_command(&mut session, "vsplit");
        settle(&mut session);
        let split = session.focus;
        assert_ne!(split, editor);
        assert_eq!(session.views[&split].location.buffer, git);

        input_command(&mut session, command);
        assert!(!session.exiting(), "{command}");
        assert_eq!(session.focus, split);
        assert_eq!(
            session.views[&split].location.buffer,
            if command == "bn" { first } else { second },
            "{command}"
        );
        assert_eq!(session.views[&editor].location.buffer, git);
        assert_eq!(selected_row(&session, editor).id, "file:note.txt");

        session.focus_view(editor).unwrap();
        assert_eq!(session.views[&editor].location.buffer, git);
        input_command(&mut session, "q");
        assert!(!session.exiting());
        assert_eq!(session.views[&editor].location.buffer, second);
    }
}
