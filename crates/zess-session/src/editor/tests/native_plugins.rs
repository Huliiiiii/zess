use super::*;

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

fn page(session: &mut Session) {
    session.input("C-w");
    session.input("]");
    settle(session);
}

fn preview_text(session: &Session, source: ViewId) -> String {
    let buffer = preview_buffer(session, source);
    match &session.buffers[&buffer] {
        Buffer::PreviewText { text, .. } => text.text(),
        Buffer::PreviewDiff { diff, .. } => diff.raw.clone(),
        _ => panic!("{}", session.message),
    }
}

fn commit_diff(session: &Session, source: ViewId) -> &str {
    let target = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .find(|member| member.id == "main")
        .unwrap()
        .view()
        .unwrap()
        .id;
    let Buffer::PreviewDiff { path, diff } =
        &session.buffers[&session.views[&target].location.buffer]
    else {
        panic!("expected historical file diff")
    };
    assert_eq!(path, "note.txt");
    &diff.raw
}

#[test]
fn git_help_opens_from_browser_keys_and_restores_the_source_view() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "working\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "initial"]);
    fs::write(fixture.0.join("note.txt"), "changed\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    let source = session.focus;
    for (title, key, description) in [
        ("Git changes", "g f", "Open source file"),
        ("Git history", "Enter", "Browse commit files"),
        ("Git commit files", "g f", "Open historical file"),
    ] {
        for keys in [
            vec!["?"],
            vec!["space", "?"],
            vec![":", "h", "e", "l", "p", "Enter"],
        ] {
            for key in keys {
                session.input(key);
            }
            // Pending browser queries may finish while Help owns the focus.
            settle(&mut session);
            let layout = panes(&session);
            let help = session.help.as_ref().unwrap().view;
            assert_eq!(session.focus, help);
            let display = &session.views[&help].location.display;
            assert_eq!(display.header, "Help: Git", "{title}");
            assert_eq!(
                display.rows.iter().find(|row| row.id == key).unwrap().text,
                format!("{key:<20} {description}")
            );
            for key in ["C-w [", "C-w ]"] {
                assert_eq!(
                    display.rows.iter().find(|row| row.id == key).unwrap().text,
                    format!("{key:<20} Switch Git page")
                );
            }
            session.input("q");
            assert!(session.help.is_none());
            assert_eq!(session.focus, source);
            assert_eq!(panes(&session), layout);
        }
        if title == "Git changes" {
            page(&mut session);
        } else if title == "Git history" {
            session.input("Enter");
        }
    }
}

#[test]
fn git_file_layers_stay_open_after_saving_until_explicitly_closed() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    let path = fixture.0.join("note.txt");
    fs::write(&path, "original\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let layout = panes(&session);
    session.input("e");
    let layer = session.focus;
    let buffer = session.views[&layer].location.buffer;
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    session.command("write").unwrap();
    settle(&mut session);
    assert_eq!(fs::read_to_string(&path).unwrap(), "!original\n");
    assert_eq!(session.focus, layer);
    assert_eq!(session.views[&layer].location.buffer, buffer);
    assert_eq!(session.views.child(source), Some(layer));
    assert_eq!(panes(&session), layout);
    for key in ["i", "?", "Esc"] {
        session.input(key);
    }
    session.command("write-quit").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "?!original\n");
    assert_eq!(session.focus, source);
    assert!(!session.views.contains_key(&layer));
    assert!(!session.buffers.contains_key(&buffer));
    assert_eq!(panes(&session), layout);
}

#[test]
fn changes_show_only_the_diff_sides_present_in_the_selected_file() {
    for (staged, unstaged, expected) in [
        (false, true, vec!["unstaged"]),
        (true, false, vec!["staged"]),
        (true, true, vec!["unstaged", "staged"]),
    ] {
        let fixture = Fixture::new();
        git(&fixture, &["init", "--quiet"]);
        let path = fixture.0.join("note.txt");
        fs::write(&path, "baseline\n").unwrap();
        git(&fixture, &["add", "note.txt"]);
        git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
        if staged {
            fs::write(&path, "staged\n").unwrap();
            git(&fixture, &["add", "note.txt"]);
        }
        if unstaged {
            fs::write(&path, "worktree\n").unwrap();
        }
        let mut session = fixture.session(&[]);
        session.command("git").unwrap();
        settle(&mut session);
        let source = session.focus;
        let members = &session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members;
        assert_eq!(
            members
                .iter()
                .map(|member| member.id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(panes(&session).len(), expected.len() + 1);
        for member in members {
            let Buffer::PreviewDiff { diff, .. } =
                &session.buffers[&session.views[&member.view().unwrap().id].location.buffer]
            else {
                panic!("expected a diff preview")
            };
            let expected = if member.id == "staged" {
                git(
                    &fixture,
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--cached",
                        "--",
                        "note.txt",
                    ],
                )
            } else {
                git(
                    &fixture,
                    &["diff", "--no-ext-diff", "--no-textconv", "--", "note.txt"],
                )
            };
            assert_eq!(diff.raw, format!("{expected}\n"));
        }
        let target = members[0].view().unwrap().id;
        session.input("Enter");
        assert_eq!(session.focus, target);
        session.input("Esc");
        assert_eq!(session.focus, source);
    }
}

#[test]
fn removing_the_focused_diff_returns_focus_to_changes() {
    for removed in ["staged", "unstaged"] {
        let fixture = Fixture::new();
        git(&fixture, &["init", "--quiet"]);
        let path = fixture.0.join("note.txt");
        fs::write(&path, "baseline\n").unwrap();
        git(&fixture, &["add", "note.txt"]);
        git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
        fs::write(&path, "staged\n").unwrap();
        git(&fixture, &["add", "note.txt"]);
        fs::write(&path, "worktree\n").unwrap();
        let mut session = fixture.session(&[]);
        session.command("git").unwrap();
        settle(&mut session);
        let source = session.focus;
        let members = &session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members;
        let discarded = members
            .iter()
            .find(|member| member.id == removed)
            .unwrap()
            .view()
            .unwrap()
            .id;
        let retained = members
            .iter()
            .find(|member| member.id != removed)
            .unwrap()
            .view()
            .unwrap()
            .id;
        let discarded_buffer = session.views[&discarded].location.buffer;
        let retained_buffer = session.views[&retained].location.buffer;
        session.focus_view(discarded).unwrap();
        session.input("/");
        session.input("b");
        assert!(session.prompt.is_some());
        if removed == "staged" {
            git(&fixture, &["reset", "--quiet", "HEAD", "--", "note.txt"]);
        } else {
            git(&fixture, &["add", "note.txt"]);
        }
        wait_until(&mut session, |session| {
            session
                .views
                .page(source)
                .previews
                .as_ref()
                .unwrap()
                .members
                .len()
                == 1
        });
        settle(&mut session);
        assert_eq!(session.focus, source);
        assert!(session.prompt.is_none());
        assert!(!session.views.contains_key(&discarded));
        assert!(!session.buffers.contains_key(&discarded_buffer));
        assert_eq!(
            session
                .views
                .page(source)
                .previews
                .as_ref()
                .unwrap()
                .members[0]
                .view()
                .unwrap()
                .id,
            retained
        );
        assert_eq!(session.views[&retained].location.buffer, retained_buffer);
        assert_eq!(panes(&session).len(), 2);
        let stats = session
            .services
            .borrow_mut()
            .call("service.stats", json!({}));
        assert_eq!(stats["observed_queries"], 2);
        session.input("Enter");
        assert_eq!(session.focus, retained);
        session.input("Esc");
        assert_eq!(session.focus, source);
    }
}

#[test]
fn staged_and_unstaged_diffs_are_visible_together_and_keep_the_focused_pane() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    let path = fixture.0.join("note.txt");
    fs::write(&path, "baseline\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(&path, "staged\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "worktree\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(session.views[&source].location.display.rows.len(), 1);
    let group = &session.views.page(source).previews.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    let unstaged = group
        .members
        .iter()
        .find(|member| member.id == "unstaged")
        .unwrap()
        .view()
        .unwrap()
        .id;
    let staged = group
        .members
        .iter()
        .find(|member| member.id == "staged")
        .unwrap()
        .view()
        .unwrap()
        .id;
    for (view, args) in [
        (
            unstaged,
            vec!["diff", "--no-ext-diff", "--no-textconv", "--", "note.txt"],
        ),
        (
            staged,
            vec![
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--cached",
                "--",
                "note.txt",
            ],
        ),
    ] {
        let Buffer::PreviewDiff { diff, .. } =
            &session.buffers[&session.views[&view].location.buffer]
        else {
            panic!("{}", session.message)
        };
        assert_eq!(diff.raw, format!("{}\n", git(&fixture, &args)));
    }
    session.focus_view(staged).unwrap();
    for (width, columns) in [(241, false), (242, true), (80, false), (300, true)] {
        session.resize(width, 24);
        session.frame();
        let layout = panes(&session);
        let (_, ax, ay, aw, ah) = layout
            .iter()
            .copied()
            .find(|(id, ..)| *id == unstaged)
            .unwrap();
        let (_, bx, by, bw, bh) = layout
            .iter()
            .copied()
            .find(|(id, ..)| *id == staged)
            .unwrap();
        if columns {
            assert_eq!(ay, by);
            assert_eq!(ah, bh);
            assert_eq!(ax + aw + 1, bx);
        } else {
            assert_eq!(ax, bx);
            assert_eq!(aw, bw);
            assert_eq!(ay + ah, by);
        }
        assert_eq!(session.focus, staged);
        assert!(aw > 0 && bw > 0);
    }
}

#[test]
fn commit_navigation_preserves_the_history_location_and_selected_commit_diff() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    let path = fixture.0.join("note.txt");
    fs::write(&path, "old snapshot\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "older commit"]);
    let old = git(&fixture, &["rev-parse", "HEAD"]);
    fs::write(&path, "new snapshot\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "newer commit"]);
    fs::write(&path, "working copy\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    page(&mut session);
    let history = session.views[&source].location.buffer;
    assert!(matches!(&session.buffers[&history],Buffer::Data{kind,..} if kind=="git-history"));
    for key in ["/", "o", "l", "d", "e", "r", "Enter"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(
        session.views[&source].location.selection.primary.as_deref(),
        Some(old.as_str())
    );
    let selection = serde_json::to_value(&session.views[&source].location.selection).unwrap();
    let scroll = session.views[&source].location.scroll;
    session.input("Enter");
    settle(&mut session);
    let files = session.views[&source].location.buffer;
    assert_ne!(files, history);
    assert!(matches!(&session.buffers[&files],Buffer::Data{kind,..} if kind=="git-commit-files"));
    assert_eq!(
        commit_diff(&session, source),
        format!(
            "{}\n",
            git(&fixture, &["show", "--format=", &old, "--", "note.txt"])
        )
    );
    let layout = panes(&session);
    page(&mut session);
    page(&mut session);
    assert_eq!(session.views[&source].location.buffer, files);
    assert_eq!(panes(&session), layout);
    assert_eq!(
        commit_diff(&session, source),
        format!(
            "{}\n",
            git(&fixture, &["show", "--format=", &old, "--", "note.txt"])
        )
    );
    session.input("Esc");
    settle(&mut session);
    assert_eq!(session.views[&source].location.buffer, history);
    assert_eq!(session.views[&source].location.scroll, scroll);
    assert_eq!(
        serde_json::to_value(&session.views[&source].location.selection).unwrap(),
        selection
    );
    assert_eq!(session.views[&source].location.display.rows.len(), 1);
    let info = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .find(|member| member.id == "commit-info")
        .unwrap()
        .view()
        .unwrap()
        .id;
    let Buffer::PreviewRows {
        kind: PreviewRowKind::Content,
        path,
        display,
    } = &session.buffers[&session.views[&info].location.buffer]
    else {
        panic!("expected selected commit information")
    };
    assert_eq!(path, &old);
    assert_eq!(
        display
            .rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        git(
            &fixture,
            &[
                "show",
                "--no-patch",
                "--format=%H%nAuthor: %an%nDate:   %aI%n%n%B",
                &old
            ]
        )
    );
}

#[test]
fn historical_file_layers_are_readonly_and_release_their_state_on_close() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    let path = fixture.0.join("note.txt");
    fs::write(&path, "historical\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "initial"]);
    fs::write(&path, "working\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    page(&mut session);
    session.input("Enter");
    settle(&mut session);
    session.input("e");
    settle(&mut session);
    let layer = session.focus;
    let snapshot = session.views[&layer].location.buffer;
    assert!(matches!(
        session.buffers[&snapshot],
        Buffer::Snapshot { .. }
    ));
    assert_eq!(session.views.parent(layer), Some(source));
    assert!(session.command("write note.txt").is_err());
    assert!(session.command("reload").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");
    session.command("quit!").unwrap();
    assert!(!session.buffers.contains_key(&snapshot));
    assert_eq!(session.focus, source);
    session.input("g");
    session.input("f");
    settle(&mut session);
    let buffer = session.views[&session.focus].location.buffer;
    assert!(matches!(session.buffers[&buffer], Buffer::Snapshot { .. }));
    assert_eq!(panes(&session).len(), 1);
    assert!(session.command("write note.txt").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");
}

#[test]
fn returning_to_history_refreshes_commits_created_while_its_watches_were_paused() {
    let fixture = Fixture::new();
    git(&fixture, &["init", "--quiet"]);
    let path = fixture.0.join("note.txt");
    fs::write(&path, "first\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "first commit"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    page(&mut session);
    assert_eq!(session.views[&source].location.display.rows.len(), 1);
    page(&mut session);
    fs::write(&path, "second\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "second commit"]);
    page(&mut session);
    assert_eq!(session.views[&source].location.display.rows.len(), 2);
    assert_eq!(
        session.views[&source].location.display.rows[0].id,
        git(&fixture, &["rev-parse", "HEAD"])
    );
}

#[test]
fn a_rejected_rename_can_be_corrected_without_losing_its_source() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "a").unwrap();
    fs::write(fixture.0.join("b.txt"), "b").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    for key in ["r", "C-u", "b", ".", "t", "x", "t", "Enter"] {
        session.input(key);
    }
    assert!(session.prompt.as_ref().unwrap().error.is_some());
    assert!(!session.modified());
    for key in ["C-u", "c", ".", "t", "x", "t", "Enter"] {
        session.input(key);
    }
    assert!(session.prompt.is_none(), "{}", session.message);
    assert!(session.modified());
    session.command("write").unwrap();
    assert!(!fixture.0.join("a.txt").exists());
    assert_eq!(fs::read_to_string(fixture.0.join("c.txt")).unwrap(), "a");
    assert_eq!(fs::read_to_string(fixture.0.join("b.txt")).unwrap(), "b");
}

#[test]
fn structured_outline_symbols_keep_folding_and_source_navigation() {
    let fixture = Fixture::new();
    let path = fixture.0.join("example.rs");
    fs::write(&path, "fn first() {}\nmod nest {\n    fn inner() {}\n}\n").unwrap();
    let mut session = fixture.session(&[path]);
    let source = session.views[&session.focus].location.buffer;
    session.command("outline").unwrap();
    assert_eq!(
        session.views[&session.focus].location.display.rows.len(),
        3,
        "{}",
        session.message
    );
    session.input("j");
    session.input("h");
    assert_eq!(
        session.views[&session.focus].location.display.rows.len(),
        2,
        "{}",
        session.message
    );
    session.input("l");
    session.input("j");
    session.input("Enter");
    assert_eq!(session.views[&session.focus].location.buffer, source);
    let text = session
        .buffers
        .get_mut(&source)
        .and_then(Buffer::text)
        .unwrap();
    assert_eq!(text.view(session.focus).cursor().0, 2);
}

#[test]
fn native_file_operations_survive_configuration_reload_until_explicit_save() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    for key in ["a", "n", "e", "w", ".", "t", "x", "t", "Enter"] {
        session.input(key);
    }
    assert!(session.modified(), "{}", session.message);
    assert!(!fixture.0.join("new.txt").exists());
    let layout = panes(&session);
    session.command("config-reload").unwrap();
    settle(&mut session);
    assert!(session.modified(), "{}", session.message);
    assert_eq!(panes(&session), layout);
    assert_eq!(session.focus, source);
    assert!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.id == "new.txt" && row.text == "  + new.txt [new, unsaved]")
    );
    session.command("write").unwrap();
    assert!(fixture.0.join("new.txt").exists());
    assert!(!session.modified());
}

#[test]
fn directory_preview_keeps_the_first_entry_when_the_contents_fit_after_resize() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 5);
    let source = session.focus;
    let rows = (0..8)
        .map(|index| DisplayRow {
            id: index.to_string(),
            text: format!("child-{index}"),
            ..DisplayRow::default()
        })
        .collect();
    session
        .show_preview(
            session.views.page_id(source),
            "dir".into(),
            PreviewContent::Directory {
                rows,
                column_alignment: Default::default(),
            },
            false,
        )
        .unwrap();
    session.frame();
    session
        .execute_command("preview.move-down", serde_json::json!({}))
        .unwrap();
    let target = session.preview_target(source).unwrap();
    assert_eq!(session.views[&target].location.scroll, 1);
    session.resize(80, 24);
    for _ in 0..10 {
        session
            .execute_command("preview.move-down", serde_json::json!({}))
            .unwrap();
    }
    assert_eq!(session.views[&target].location.scroll, 0);
    let (rows, _) = session.render_view(target, 39, 23);
    assert_eq!(rows[0].spans[0].text, "child-0");
}

#[test]
fn git_page_commands_require_their_browser_context() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.views[&session.focus].location.buffer;
    let buffers = session.buffers.len();
    assert!(session.command("git.page-next").is_err());
    assert_eq!(session.views[&session.focus].location.buffer, source);
    assert_eq!(session.buffers.len(), buffers);
}

#[test]
fn rapid_file_navigation_keeps_the_previous_preview_until_the_latest_selection_loads() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "old\n".repeat(60)).unwrap();
    fs::write(fixture.0.join("b.txt"), "intermediate\n").unwrap();
    fs::write(fixture.0.join("c.txt"), "latest\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let target = session.active_preview(source).unwrap();
    session.frame();
    session
        .execute_command("preview.move-down", serde_json::json!({}))
        .unwrap();
    let before = session
        .services
        .borrow_mut()
        .call("service.stats", Value::Null)["next_handle"]
        .as_u64()
        .unwrap();
    session.input("j");
    assert_eq!(selected_row(&session, source).id, "b.txt");
    let leading = session
        .services
        .borrow_mut()
        .call("service.stats", Value::Null);
    // Stats itself consumes one handle; the other belongs to the leading preview.
    assert_eq!(leading["next_handle"].as_u64().unwrap(), before + 2);
    let previous = preview_text(&session, source);
    assert!(previous == "old\n".repeat(60) || previous == "intermediate\n");
    let scroll = preview_surface(&session, source).viewport.scroll;
    let old_buffer = preview_buffer(&session, source);
    let old_observations = session.views[&source].observations.len();
    session.input("j");
    assert_eq!(selected_row(&session, source).id, "c.txt");
    assert_eq!(preview_text(&session, source), previous);
    assert_eq!(preview_buffer(&session, source), old_buffer);
    assert_eq!(session.views[&source].observations.len(), old_observations);
    let trailing = session
        .services
        .borrow_mut()
        .call("service.stats", Value::Null);
    assert_eq!(trailing["jobs"], 0);
    assert_eq!(
        trailing["next_handle"].as_u64().unwrap(),
        leading["next_handle"].as_u64().unwrap() + 1
    );
    assert_eq!(preview_surface(&session, source).viewport.scroll, scroll);
    settle(&mut session);
    assert_eq!(session.active_preview(source), Some(target));
    assert_eq!(preview_text(&session, source), "latest\n");
}

#[test]
fn leaving_files_cancels_its_pending_file_selection() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "old\n").unwrap();
    fs::write(fixture.0.join("b.txt"), "latest\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("j");
    session.command("new").unwrap();
    settle(&mut session);
    assert!(session.active_preview(source).is_none());
    assert_eq!(
        session
            .services
            .borrow_mut()
            .call("service.stats", Value::Null)["jobs"],
        0
    );
    session.command("files").unwrap();
    settle(&mut session);
    session.input("j");
    settle(&mut session);
    assert_eq!(preview_text(&session, source), "latest\n");
}
