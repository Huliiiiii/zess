use super::*;
use zess_protocol::{
    CommandAction, CommandOutcome, CommandTarget,
    resource::{PageCommandInput, PageId, PageInput, PageSource, ResourceEvent, ResourceRequest},
};

fn open_git_page(session: &mut Session, subscription_id: u64) -> PageInput {
    let input = PageInput {
        generation: 1,
        mode: "normal".into(),
        selection: ItemSelection::default(),
        register: None,
        filter: None,
        source_buffer: None,
        width: 40,
        active: true,
        preview_width: 39,
        preview_height: 15,
    };
    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id,
        page_id: PageId {
            context_epoch: 0,
            sequence: subscription_id,
        },
        clone_from: None,
        source: PageSource {
            kind: "git".into(),
            argument: json!({}),
        },
        input: input.clone(),
    });
    wait_until(session, |session| {
        session.take_resource_updates().into_iter().any(|update| {
            update.output.display.is_some_and(|display| {
                !display.loading && display.rows.iter().any(|row| row.id == "file:note.txt")
            })
        })
    });
    input
}

#[test]
fn staging_the_selected_file_finishes_after_its_page_detaches() {
    let (fixture, _) = repository();
    fixture.write("another.txt", "untracked");
    let mut session = fixture.session(&[]);
    let subscription_id = 7;
    let mut input = open_git_page(&mut session, subscription_id);
    input.selection = ItemSelection {
        primary: Some("file:note.txt".into()),
        anchor: Some("file:note.txt".into()),
        selected: vec!["file:note.txt".into()],
    };
    let command_input = PageCommandInput {
        generation: input.generation + 1,
        selection: input.selection.clone(),
        register: None,
    };
    input.generation += 1;
    session.handle_resource(ResourceRequest::Update {
        subscription_id,
        input,
        event: ResourceEvent::Select {
            navigation: ItemNavigation::Step,
        },
    });
    assert_eq!(
        session.begin_operation(
            "stage",
            &CommandAction::Invoke {
                target: CommandTarget::Page {
                    subscription_id,
                    input: command_input,
                },
                name: "git.stage".into(),
                argument: json!({})
            }
        ),
        None
    );
    wait_until(&mut session, |session| {
        session.take_resource_updates().into_iter().any(|update| {
            update.output.display.is_some_and(|display| {
                display.loading && display.status["git"]["write_epoch"].is_u64()
            })
        })
    });
    session.detach_resources(None, |_| false);
    let completion = std::cell::Cell::new(None);
    wait_until(&mut session, |session| {
        let outcome = session
            .take_operation_completions()
            .into_iter()
            .find_map(|(id, outcome)| (id == "stage").then_some(outcome));
        let found = outcome.is_some();
        completion.set(outcome);
        found
    });
    assert_eq!(completion.into_inner(), Some(CommandOutcome::Applied));
    assert_eq!(
        git(&fixture, &["diff", "--cached", "--name-only"]),
        "note.txt"
    );
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
}

#[test]
fn staging_uses_the_invoked_paths_after_git_status_replaces_the_selection() {
    let (fixture, path) = repository();
    fixture.write("another.txt", "untracked");
    fixture.write("surviving.txt", "untracked");
    let mut session = fixture.session(&[]);
    let subscription_id = 7;
    let mut input = open_git_page(&mut session, subscription_id);
    let selection = ItemSelection {
        primary: Some("file:note.txt".into()),
        anchor: Some("file:note.txt".into()),
        selected: vec!["file:note.txt".into(), "file:surviving.txt".into()],
    };
    let generation = input.generation + 1;
    let stage = |selection| CommandAction::Invoke {
        target: CommandTarget::Page {
            subscription_id,
            input: PageCommandInput {
                generation,
                selection,
                register: None,
            },
        },
        name: "git.stage".into(),
        argument: json!({}),
    };
    let stale = stage(selection.clone());
    input.selection = selection;
    input.generation = generation;
    session.handle_resource(ResourceRequest::Update {
        subscription_id,
        input: input.clone(),
        event: ResourceEvent::Select {
            navigation: ItemNavigation::Step,
        },
    });
    fs::write(path, "historical\n").unwrap();
    session.services.borrow_mut().invalidate_fs();
    wait_until(&mut session, |session| {
        session.take_resource_updates().into_iter().any(|update| {
            update
                .output
                .selection
                .is_some_and(|selection| selection.primary.as_deref() == Some("file:another.txt"))
                && update.output.display.is_some_and(|display| {
                    !display.loading && display.rows.iter().all(|row| row.id != "file:note.txt")
                })
        })
    });
    let outcome = session.begin_operation("stage-stale", &stale);
    assert!(
        matches!(outcome, Some(CommandOutcome::Failed { ref message })
            if message == "Selected path is unavailable: note.txt"),
        "{outcome:?}"
    );
    assert_eq!(git(&fixture, &["diff", "--cached", "--name-only"]), "");

    input.selection = ItemSelection {
        primary: Some("file:surviving.txt".into()),
        anchor: Some("file:surviving.txt".into()),
        selected: vec!["file:surviving.txt".into()],
    };
    let surviving = stage(input.selection.clone());
    input.generation += 1;
    input.register = Some('a');
    session.take_resource_updates();
    session.handle_resource(ResourceRequest::Update {
        subscription_id,
        input,
        event: ResourceEvent::Register,
    });
    assert_eq!(session.begin_operation("stage-surviving", &surviving), None);
    assert!(session.take_resource_updates().into_iter().any(|update| {
        update
            .output
            .selection
            .is_some_and(|selection| selection.primary.as_deref() == Some("file:another.txt"))
    }));
    wait_until(&mut session, |session| {
        session
            .take_operation_completions()
            .contains(&("stage-surviving".into(), CommandOutcome::Applied))
    });
    assert_eq!(
        git(&fixture, &["diff", "--cached", "--name-only"]),
        "surviving.txt"
    );
}

fn diff_side(
    session: &Session,
    slot: &super::super::super::pages::Slot,
) -> Option<zess_git::DiffSide> {
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&slot.buffer()] else {
        return None;
    };
    match diff.source {
        Some(zess_git::DiffSource::Changes { side }) => Some(side),
        _ => None,
    }
}

fn selected_diff_row(session: &mut Session, content: &str) {
    let view = session.focus;
    let buffer = session.views[&view].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff")
    };
    let row = diff.raw.lines().position(|line| line == content).unwrap();
    let selection = &mut session
        .views
        .get_mut(&view)
        .unwrap()
        .location
        .diff_selection;
    *selection = diff::Selection::Line { head: None };
    assert!(selection.select_at(&diff.index, row, false));
}

fn finish_patch(session: &mut Session) {
    wait_until(session, |session| session.git_patch.is_none());
    settle(session);
    assert_eq!(session.message, "Git index updated");
}

#[test]
fn a_selected_hunk_can_be_staged_from_the_diff_and_unstaged_from_the_index_pane() {
    let (fixture, path) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    settle(&mut session);
    assert_ne!(session.focus, source);
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    assert_eq!(
        session.begin_operation(
            "stage-hunk",
            &zess_protocol::CommandAction::Command {
                name: "diff.apply-selection".into(),
                argument: json!({}),
            }
        ),
        None
    );
    finish_patch(&mut session);
    assert_eq!(
        session.take_operation_completions(),
        vec![("stage-hunk".into(), zess_protocol::CommandOutcome::Applied)]
    );
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");
    assert_ne!(session.focus, source);
    assert!(
        session
            .views
            .page(source)
            .previews
            .as_ref()
            .unwrap()
            .members
            .iter()
            .any(|member| diff_side(&session, member) == Some(zess_git::DiffSide::Staged))
    );
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    finish_patch(&mut session);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");
    session.input("Esc");
    assert_eq!(session.focus, source);
}

#[test]
fn staging_one_diff_line_keeps_unselected_changes_out_of_the_index() {
    let (fixture, path) = repository();
    fs::write(&path, "historical\nfirst addition\nsecond addition\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    selected_diff_row(&mut session, "+first addition");
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    finish_patch(&mut session);
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "historical\nfirst addition"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "historical\nfirst addition\nsecond addition\n"
    );
}

#[test]
fn hunk_navigation_selects_change_blocks_and_line_mode_moves_between_rows() {
    let (fixture, path) = repository();
    let baseline = (0..25).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &baseline).unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "many lines"]);
    fs::write(
        &path,
        baseline
            .replace("line 1\n", "first\n")
            .replace("line 22\n", "second\n"),
    )
    .unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    let target = session.focus;
    let buffer = session.views[&target].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff")
    };
    let blocks = diff.index.blocks.clone();
    assert_eq!(blocks.len(), 2);
    assert!(matches!(
        session.views[&target].location.diff_selection,
        diff::Selection::Block { .. }
    ));
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .rows(&diff.index)
            .unwrap(),
        blocks[0]
    );
    session.input("j");
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        *blocks[1].start()
    );
    session.input("{");
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        *blocks[0].start()
    );
    session.input("a");
    assert!(matches!(
        session.views[&target].location.diff_selection,
        diff::Selection::Line { .. }
    ));
    session.input("l");
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        *blocks[0].start() + 1
    );
    session.input("v");
    session.input("j");
    session.input("Esc");
    assert_eq!(session.focus, target);
    assert!(matches!(
        session.views[&target].location.diff_selection,
        diff::Selection::Line { .. }
    ));
    session.input("Esc");
    assert_ne!(session.focus, target);
}

#[test]
fn a_failed_index_update_leaves_the_diff_available_for_retry() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    fs::write(fixture.0.join(".git/index.lock"), "held").unwrap();
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    wait_until(&mut session, |session| session.git_patch.is_none());
    settle(&mut session);
    assert_eq!(
        session.message,
        format!(
            "git apply: fatal: Unable to create '{}': File exists.\n\nAnother git process seems to be running in this repository, or the lock file may be stale",
            fixture.0.join(".git/index.lock").display()
        )
    );
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    assert!(matches!(
        session.buffers[&session.views[&session.focus].location.buffer],
        Buffer::PreviewDiff { .. }
    ));
    fs::remove_file(fixture.0.join(".git/index.lock")).unwrap();
    session
        .execute_command("diff.select-hunk", json!({}))
        .unwrap();
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    finish_patch(&mut session);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
}

#[test]
fn visual_line_selection_stages_only_the_selected_additions() {
    let (fixture, path) = repository();
    fs::write(&path, "historical\nfirst\nsecond\nthird\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    selected_diff_row(&mut session, "+first");
    for key in ["v", "j"] {
        session.input(key);
    }
    session
        .execute_command("diff.apply-selection", json!({}))
        .unwrap();
    finish_patch(&mut session);
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "historical\nfirst\nsecond"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "historical\nfirst\nsecond\nthird\n"
    );
}

#[test]
fn diff_source_edits_use_an_independent_file_layer_and_saving_keeps_it_open() {
    let (fixture, path) = repository();
    fs::write(&path, "historical\nfirst addition\nsecond addition\n").unwrap();
    let mut session = fixture.session(std::slice::from_ref(&path));
    let original = session.views[&session.focus].location.buffer;
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    let diff_view = session.focus;
    let diff_buffer = session.views[&diff_view].location.buffer;
    selected_diff_row(&mut session, "+second addition");
    session.input("e");
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    assert_ne!(editor, diff_view);
    assert_ne!(buffer, original);
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("expected writable file")
    };
    assert_eq!(text.view(editor).cursor().0, 2);
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    session.command("write").unwrap();
    assert_eq!(session.focus, editor);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "historical\nfirst addition\n!second addition\n"
    );
    session.command("quit").unwrap();
    assert_eq!(session.focus, diff_view);
    assert_eq!(session.views[&diff_view].location.buffer, diff_buffer);
    assert!(!session.buffers.contains_key(&buffer));
    assert!(session.views.page(source).previews.as_ref().is_some());
    let Buffer::Text(text) = &session.buffers[&original] else {
        panic!("expected original editor")
    };
    assert_eq!(text.text(), "historical\nfirst addition\nsecond addition\n");
}

#[test]
fn diff_search_previews_and_cancels_rows_and_shares_the_text_search_pattern() {
    let (fixture, path) = repository();
    fs::write(&path, "historical\nfirst addition\nsecond addition\n").unwrap();
    let mut session = fixture.session(std::slice::from_ref(&path));
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    let target = session.focus;
    let buffer = session.views[&target].location.buffer;
    selected_diff_row(&mut session, "+first addition");
    let initial = session.views[&target]
        .location
        .diff_selection
        .row()
        .unwrap();
    for key in ["/", "a", "d", "d", "i", "t", "i", "o", "n"] {
        session.input(key);
    }
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        initial + 1
    );
    session.input("Enter");
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        initial + 1
    );
    assert_eq!(
        session.registers.search_pattern().as_deref(),
        Some("addition")
    );
    session.input("n");
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        initial
    );
    session.input("N");
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        initial + 1
    );
    session.snapshot();
    let scroll = session.views[&target].location.scroll;
    for key in ["?", "h", "i", "s", "t"] {
        session.input(key);
    }
    assert!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap()
            < initial
    );
    session.input("Esc");
    assert_eq!(
        session.views[&target]
            .location
            .diff_selection
            .row()
            .unwrap(),
        initial + 1
    );
    assert_eq!(session.views[&target].location.scroll, scroll);
    assert_eq!(
        session.registers.search_pattern().as_deref(),
        Some("addition")
    );
    assert_eq!(session.views[&target].location.buffer, buffer);
    for key in ["g", "f"] {
        session.input(key);
    }
    let editor = session.focus;
    let text = session
        .buffers
        .get_mut(&session.views[&editor].location.buffer)
        .unwrap()
        .text()
        .unwrap();
    text.goto_line(editor, 0);
    session.input("n");
    let text = session
        .buffers
        .get_mut(&session.views[&editor].location.buffer)
        .unwrap()
        .text()
        .unwrap();
    assert_eq!(text.view(editor).cursor().0, 1);
    for key in ["/", "h", "i", "s", "t", "Enter"] {
        session.input(key);
    }
    session.command("git").unwrap();
    settle(&mut session);
    session.input("Enter");
    session.input("n");
    let view = &session.views[&session.focus];
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&view.location.buffer] else {
        panic!("expected diff")
    };
    assert_eq!(
        diff.line(view.location.diff_selection.row().unwrap()),
        " historical"
    );
}

#[test]
fn removing_a_diff_pane_keeps_its_unsaved_file_layer_open_and_returns_to_changes() {
    let (fixture, path) = repository();
    fs::write(&path, "staged\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "worktree\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let unstaged = session
        .views
        .page(source)
        .previews
        .as_ref()
        .unwrap()
        .members
        .iter()
        .find(|member| diff_side(&session, member) == Some(zess_git::DiffSide::Unstaged))
        .unwrap()
        .view()
        .unwrap()
        .id;
    session.focus_view(unstaged).unwrap();
    session.input("e");
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    git(&fixture, &["add", "note.txt"]);
    wait_until(&mut session, |session| {
        !session.views.contains_key(&unstaged)
    });
    assert_eq!(session.focus, editor);
    assert_eq!(session.views[&editor].location.buffer, buffer);
    assert_eq!(session.views.parent(editor), Some(source));
    assert_eq!(session.views.child(source), Some(editor));
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("expected editor")
    };
    assert_eq!(text.text(), "!worktree\n");
    assert!(text.modified());
    assert!(session.command("quit").is_err());
    assert_eq!(session.focus, editor);
    session.command("write").unwrap();
    assert_eq!(session.focus, editor);
    assert_eq!(fs::read_to_string(&path).unwrap(), "!worktree\n");
    session.command("quit").unwrap();
    assert_eq!(session.focus, source);
    assert!(session.views.child(source).is_none());
    assert!(!session.buffers.contains_key(&buffer));
}

#[test]
fn staged_source_navigation_accounts_for_unstaged_insertions_before_the_selected_line() {
    let (fixture, path) = repository();
    fs::write(&path, "A\nB\nC\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(&path, "A\nB*\nC\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "X\nA\nB*\nC\n").unwrap();
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
        .find(|member| diff_side(&session, member) == Some(zess_git::DiffSide::Staged))
        .unwrap()
        .view()
        .unwrap()
        .id;
    session.focus_view(staged).unwrap();
    selected_diff_row(&mut session, "+B*");
    session.input("e");
    let editor = session.focus;
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("expected editor")
    };
    assert_eq!(text.view(editor).cursor().0, 2);
    assert_eq!(text.text().lines().nth(2), Some("B*"));
    session.command("quit").unwrap();
    assert_eq!(session.focus, staged);
    for key in ["g", "f"] {
        session.input(key);
    }
    assert_eq!(session.focus, source);
    let Buffer::Text(text) = &session.buffers[&session.views[&source].location.buffer] else {
        panic!("expected source")
    };
    assert_eq!(text.view(source).cursor().0, 2);
    assert_eq!(text.text().lines().nth(2), Some("B*"));
}

#[test]
fn staged_source_navigation_waits_while_the_unstaged_pane_is_loading() {
    let (fixture, path) = repository();
    fs::write(&path, "staged\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "worktree\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let group = session.views.page(source).previews.as_ref().unwrap();
    let staged = group
        .members
        .iter()
        .find(|member| diff_side(&session, member) == Some(zess_git::DiffSide::Staged))
        .unwrap()
        .view()
        .unwrap()
        .id;
    let layout = group.layout;
    let panes = group
        .members
        .iter()
        .map(|member| {
            let Buffer::PreviewDiff { path, diff } =
                &session.buffers[&member.view().unwrap().location.buffer]
            else {
                panic!("expected diff preview")
            };
            let loading = diff_side(&session, member) == Some(zess_git::DiffSide::Unstaged);
            PreviewPane::View {
                id: member.id.clone(),
                path: path.clone(),
                content: PreviewContent::GitDiff {
                    diff: zess_git::GitDiff {
                        source: diff.source.clone().unwrap(),
                        files: diff.files.clone(),
                    },
                },
                header: member.view().unwrap().location.display.header.clone(),
                key_scope: member.view().unwrap().location.options["key-scope"]
                    .as_str()
                    .map(str::to_owned),
                loading,
            }
        })
        .collect();
    session
        .show_preview_group(session.views.page_id(source), panes, layout, false)
        .unwrap();
    assert!(!session.views[&source].location.display.loading);
    assert!(!session.views[&staged].location.display.loading);
    session.focus_view(staged).unwrap();
    for keys in [vec!["e"], vec!["g", "f"]] {
        for key in keys {
            session.input(key);
        }
        assert_eq!(session.focus, staged);
        assert!(session.views.child(staged).is_none());
        assert_eq!(
            session.message,
            "error: wait for Git previews to finish loading"
        );
    }
}

#[test]
fn split_diffs_are_retained_until_their_last_view_or_browser_return_point_is_released() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("Enter");
    let original = session.focus;
    let buffer = session.views[&original].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff")
    };
    let patch = diff.raw.clone();
    session.command("vsplit").unwrap();
    let independent = session.focus;
    assert_eq!(session.views[&independent].location.buffer, buffer);
    session.command("git").unwrap();
    settle(&mut session);
    session.focus_view(source).unwrap();
    session.command("quit").unwrap();
    assert!(!session.views.contains_key(&original));
    assert!(
        !session
            .views
            .values()
            .any(|view| view.location.buffer == buffer)
    );
    assert!(session.buffers.contains_key(&buffer));
    session.focus_view(independent).unwrap();
    session.command("quit").unwrap();
    assert_eq!(session.focus, independent);
    assert_eq!(session.views[&independent].location.buffer, buffer);
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected restored diff")
    };
    assert_eq!(diff.raw, patch);
    let view = view_snapshot(&mut session, independent);
    assert!(
        item_rows(&view.content)
            .iter()
            .any(|row| row.line_number == Some(1) && row.text == "+working")
    );
    session.command("new").unwrap();
    assert!(!session.buffers.contains_key(&buffer));
}

#[test]
fn a_diff_without_text_lines_has_no_selection_or_source_jump() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("f"), "source text\n").unwrap();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session
        .show_preview(
            session.views.page_id(source),
            "f".into(),
            PreviewContent::Diff {
                text: "diff --git a/f b/f\nBinary files a/f and b/f differ\n".into(),
            },
            true,
        )
        .unwrap();
    let target = session.focus;
    for key in ["v", "j", "k", "g", "g", "g", "e"] {
        session.input(key);
    }
    let snapshot = session.selection_snapshot();
    assert_eq!(snapshot["diff_selection"]["row"], Value::Null);
    assert_eq!(snapshot["diff_selection"]["start_row"], Value::Null);
    assert_eq!(snapshot["diff_selection"]["end_row"], Value::Null);
    let error = session
        .execute_command("diff.edit-source", json!({}))
        .unwrap_err();
    assert_eq!(error.to_string(), "this diff has no selected source line");
    assert_eq!(session.focus, target);
    let view = view_snapshot(&mut session, target);
    assert_eq!(
        item_rows(&view.content)[1].text,
        "Binary files a/f and b/f differ"
    );
    assert_eq!(
        view.selection,
        zess_protocol::ViewSelection::Items {
            ids: vec![],
            primary: None
        }
    );
}

#[test]
fn diff_boundary_navigation_preserves_ranges_and_skips_metadata() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session
        .show_preview(
            session.views.page_id(source),
            "f".into(),
            PreviewContent::Diff {
                text: "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n@@ -10 +10 @@\n-last\n+end\n\\ No newline at end of file\n".into(),
            },
            true,
        )
        .unwrap();
    let target = session.focus;
    session.input("a");
    for key in ["g", "g"] {
        session.input(key);
    }
    assert_eq!(
        session.views[&target].location.diff_selection.row(),
        Some(4)
    );
    for key in ["v", "g", "e"] {
        session.input(key);
    }
    let buffer = session.views[&target].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected diff")
    };
    let selection = &session.views[&target].location.diff_selection;
    assert_eq!(selection.row(), Some(9));
    assert_eq!(selection.rows(&diff.index).unwrap(), 4..=9);
    assert!(matches!(selection, diff::Selection::Range { .. }));
    assert!(!selection.contains(&diff.index, 7));
    for key in ["2", "g", "g"] {
        session.input(key);
    }
    let view = &session.views[&target];
    assert_eq!(view.location.diff_selection.row(), Some(5));
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Range { .. }
    ));
    assert!(view.count.is_empty());
    for key in ["a", "g", "e"] {
        session.input(key);
    }
    let selection = &session.views[&target].location.diff_selection;
    assert_eq!(selection.row(), Some(8));
    assert!(matches!(selection, diff::Selection::Block { .. }));
}

#[test]
fn diff_half_page_navigation_uses_the_viewport_and_preserves_line_mode() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 12);
    let source = session.focus;
    let lines = (0..40)
        .map(|line| format!("+line {line}\n"))
        .collect::<String>();
    session
        .show_preview(
            session.views.page_id(source),
            "f".into(),
            PreviewContent::Diff {
                text: format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,40 @@\n{lines}"),
            },
            true,
        )
        .unwrap();
    let target = session.focus;
    session.input("a");
    session.snapshot();
    let half_page = (session.views[&target].viewport_height / 2).max(1);
    session.input("C-d");
    let view = &session.views[&target];
    assert_eq!(view.location.diff_selection.row(), Some(4 + half_page));
    assert!(view.location.scroll > 0);
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Line { .. }
    ));
    for key in ["2", "C-d"] {
        session.input(key);
    }
    let view = &session.views[&target];
    assert_eq!(view.location.diff_selection.row(), Some(4 + 3 * half_page));
    assert!(view.count.is_empty());
    for key in ["3", "C-u"] {
        session.input(key);
    }
    let view = &session.views[&target];
    assert_eq!(view.location.diff_selection.row(), Some(4));
    assert!(matches!(
        view.location.diff_selection,
        diff::Selection::Line { .. }
    ));
    assert!(view.count.is_empty());
}
