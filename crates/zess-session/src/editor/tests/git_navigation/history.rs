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
    let view = view_snapshot(&mut session, target);
    assert_eq!(
        view.selection,
        zess_protocol::ViewSelection::Items {
            ids: vec!["two".into()],
            primary: Some("two".into()),
        }
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
        let info = view_snapshot(&mut session, info);
        let info_rows = item_rows(&info.content);
        assert_eq!(info_rows[0].text, oid);
        assert_eq!(info_rows[0].style, "constant");
        let summary = view_snapshot(&mut session, summary);
        let rows = item_rows(&summary.content);
        assert_eq!(rows[0].text, "4 files, 1 binary file");
        assert_eq!(rows[2].text.trim(), "added.txt");
        assert!(
            rows[0]
                .cells
                .iter()
                .flatten()
                .any(|span| span.text == "+4" && span.style == "git.added")
        );
        assert!(
            rows[0]
                .cells
                .iter()
                .flatten()
                .any(|span| span.text == "-1" && span.style == "git.deleted")
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
fn an_unjournaled_historical_open_keeps_its_document_when_the_page_detaches() {
    use zess_protocol::{
        CommandAction, CommandOutcome, CommandTarget,
        resource::{PageCommandInput, PageId, PageInput, PageSource, ResourceRequest},
    };

    let (fixture, _) = repository();
    let commit = git(&fixture, &["rev-parse", "HEAD"]);
    let mut session = fixture.session(&[]);
    let page_id = PageId {
        context_epoch: 0,
        sequence: 1,
    };
    let selection = ItemSelection {
        primary: Some("file:note.txt".into()),
        selected: vec!["file:note.txt".into()],
        anchor: Some("file:note.txt".into()),
    };
    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id: 1,
        page_id,
        clone_from: None,
        source: PageSource {
            kind: "git-commit-files".into(),
            argument: json!({ "commit": commit }),
        },
        input: PageInput {
            generation: 0,
            selection: selection.clone(),
            register: None,
            mode: "normal".into(),
            filter: None,
            source_buffer: None,
            width: 40,
            active: true,
            preview_width: 39,
            preview_height: 22,
        },
    });
    wait_until(&mut session, |session| {
        session.take_resource_updates().iter().any(|update| {
            update.output.display.as_ref().is_some_and(|display| {
                !display.loading && display.rows.iter().any(|row| row.id == "file:note.txt")
            })
        })
    });

    let operation = "client:1";
    if let Some(outcome) = session.begin_operation(
        operation,
        &CommandAction::Invoke {
            target: CommandTarget::Page {
                subscription_id: 1,
                input: PageCommandInput {
                    generation: 0,
                    selection,
                    register: None,
                },
            },
            name: "git-commit-files.edit-layer".into(),
            argument: json!({}),
        },
    ) {
        session.finish_operation(operation.into(), outcome);
    }
    wait_until(&mut session, |session| {
        session
            .operation_completions
            .iter()
            .any(|(id, _)| id == operation)
    });
    session.detach_resources(None, |_| false);

    let completions = session.take_operation_completions();
    let [
        (
            id,
            CommandOutcome::PageApplied {
                page_id: origin,
                document: Some(document),
                ..
            },
        ),
    ] = completions.as_slice()
    else {
        panic!("the historical open must complete with its document");
    };
    assert_eq!(id, operation);
    assert_eq!(*origin, page_id);
    assert!(document.fullscreen);
    assert_eq!(
        session.document_snapshot(document.buffer_id).unwrap().text,
        "historical\n"
    );
}

#[test]
fn resizing_a_resource_page_preserves_the_pending_historical_source_request() {
    use zess_protocol::resource::{
        PageInput, PageSource, ResourceEvent, ResourceRequest, ResourceUpdateKind,
    };

    let (fixture, _) = repository();
    let commit = git(&fixture, &["rev-parse", "HEAD"]);
    let mut session = fixture.session(&[]);
    let mut input = PageInput {
        generation: 0,
        selection: ItemSelection {
            primary: Some("file:note.txt".into()),
            selected: vec!["file:note.txt".into()],
            anchor: Some("file:note.txt".into()),
        },
        register: None,
        mode: "normal".into(),
        filter: None,
        source_buffer: None,
        width: 40,
        active: false,
        preview_width: 39,
        preview_height: 22,
    };
    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id: 7,
        page_id: zess_protocol::resource::PageId {
            context_epoch: 0,
            sequence: 7,
        },
        clone_from: None,
        source: PageSource {
            kind: "git".into(),
            argument: Value::Null,
        },
        input: input.clone(),
    });
    wait_until(&mut session, |session| {
        session
            .take_resource_updates()
            .iter()
            .any(|update| update.patch_revision.is_some())
    });

    input.generation = 1;
    session.handle_resource(ResourceRequest::Update {
        subscription_id: 7,
        input: input.clone(),
        event: ResourceEvent::CommitSource {
            commit: commit.clone(),
            path: "note.txt".into(),
            line: 1,
            fullscreen: true,
        },
    });
    input.generation = 2;
    input.preview_height = 29;
    session.handle_resource(ResourceRequest::Update {
        subscription_id: 7,
        input,
        event: ResourceEvent::Resize,
    });
    wait_until(&mut session, |session| {
        let update = session
            .take_resource_updates()
            .into_iter()
            .find(|update| matches!(update.kind, ResourceUpdateKind::SourceNavigation));
        let Some(update) = update else {
            return false;
        };
        assert_eq!(update.subscription_id, 7);
        assert_eq!(update.generation, 1);
        let document = update.document.unwrap();
        assert_eq!(document.line, 1);
        assert!(document.fullscreen);
        let Buffer::Snapshot { id, text } = &session.buffers[&document.buffer_id] else {
            panic!("expected a historical file");
        };
        assert_eq!(id.commit, commit);
        assert_eq!(text.text(), "historical\n");
        true
    });
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
