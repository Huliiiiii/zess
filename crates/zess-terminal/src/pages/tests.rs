use super::*;
use hegel::{TestCase, generators as gs};
use zess_git::{DiffSide, DiffSource, DiffTarget, FileDiff, GitDiff};
use zess_plugin::{
    FileSnapshot, PluginOutput, PreviewContent, PreviewLayout, PreviewPane, SnapshotId,
};
use zess_ui::DisplayRow;

mod preview_cache;

fn rows(id: u64, generation: u64, names: &[&str]) -> ResourceUpdate {
    ResourceUpdate {
        cache: Default::default(),
        kind: ResourceUpdateKind::Page,
        document_scoped: false,
        subscription_id: id,
        generation,
        metadata: None,
        document: None,
        patch_revision: None,
        output: PluginOutput {
            display: Some(Display {
                rows: names
                    .iter()
                    .map(|&name| DisplayRow {
                        id: name.into(),
                        text: name.into(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            selection: Some(ItemSelection {
                primary: names.first().map(|name| (*name).to_owned()),
                selected: names
                    .first()
                    .map(|name| (*name).to_owned())
                    .into_iter()
                    .collect(),
                anchor: names.first().map(|name| (*name).to_owned()),
            }),
            ..Default::default()
        },
    }
}

#[test]
fn git_shows_one_loading_indicator_after_the_wait_threshold() {
    use std::time::{Duration, Instant};
    use zess_protocol::resource::PageMetadata;

    let mut pages = Pages::default();
    pages.resize(100, 12);
    pages.command("git", 1);
    let id = pages.subscription().unwrap();
    let config = StatusConfig {
        builtin_statuslines: ["default".into()].into(),
        ..Default::default()
    };
    let mut statuslines = Statuslines::default();
    let now = Instant::now();
    let delay = Duration::from_millis(200);
    let mut render = |pages: &mut Pages, at| {
        let mut frame = Frame {
            width: 100,
            height: 12,
            ..Default::default()
        };
        statuslines.begin_frame(at);
        assert!(pages.render(&mut frame, Some(&config), &mut statuslines, true));
        statuslines.finish_frame();
        frame
            .rows
            .into_iter()
            .flat_map(|row| row.spans)
            .map(|span| span.text)
            .collect::<String>()
    };

    assert!(!render(&mut pages, now).contains("Loading…"));
    assert_eq!(
        render(&mut pages, now + delay).matches("Loading…").count(),
        1
    );

    let mut update = rows(id, 0, &["note.txt"]);
    update.metadata = Some(PageMetadata {
        view_id: 12,
        buffer_id: 7,
        options: json!({"status-formats": {"loading": "GENERIC", "git-refreshing": "WAIT "}}),
    });
    pages.apply(update);
    let loaded = render(&mut pages, now + delay);
    assert!(loaded.contains("note.txt"));
    assert!(!loaded.contains("Loading…"));

    let mut refreshing = rows(id, 0, &[]);
    let display = refreshing.output.display.as_mut().unwrap();
    display.loading = true;
    display.status = json!({"git": {
        "state": "refreshing", "query_id": 1, "additions": 2, "deletions": 1,
    }});
    pages.apply(refreshing);
    let refresh_started = now + delay;
    assert!(!render(&mut pages, refresh_started).contains("WAIT"));
    let before_threshold = render(&mut pages, refresh_started + Duration::from_millis(199));
    assert!(before_threshold.contains("note.txt"));
    assert!(before_threshold.contains("+2  -1"));
    assert!(!before_threshold.contains("WAIT"));
    assert!(!before_threshold.contains("GENERIC"));

    let refreshing = render(&mut pages, refresh_started + delay);
    assert!(refreshing.contains("note.txt"));
    assert_eq!(refreshing.matches("WAIT +2  -1").count(), 1);
    assert!(!refreshing.contains("GENERIC"));

    let mut replacement = rows(id, 0, &[]);
    let display = replacement.output.display.as_mut().unwrap();
    display.loading = true;
    display.status = json!({"git": {
        "state": "refreshing", "query_id": 2, "additions": 2, "deletions": 1,
    }});
    pages.apply(replacement);
    assert!(!render(&mut pages, refresh_started + delay).contains("WAIT"));
    pages.apply(rows(id, 0, &["updated.txt"]));
    let settled = render(&mut pages, refresh_started + delay * 2);
    assert!(settled.contains("updated.txt"));
    assert!(!settled.contains("WAIT"));
    assert!(!settled.contains("GENERIC"));
}

#[test]
fn page_statusline_reflects_local_selection_before_the_server_replies() {
    use zess_protocol::resource::PageMetadata;

    let mut pages = Pages::default();
    pages.resize(80, 24);
    pages.command("files", 1);
    let mut update = rows(pages.subscription().unwrap(), 0, &["first", "second"]);
    update.metadata = Some(PageMetadata {
        view_id: 12,
        buffer_id: 7,
        options: json!({"status-formats": {"items": "{selected} of {items}"}}),
    });
    pages.apply(update);
    pages.command("data.select-mode", 1);
    pages.command("data.move-down", 1);

    let config = StatusConfig {
        builtin_statuslines: ["default".into()].into(),
        mode_labels: [("select".into(), "SELECT".into())].into(),
        ..Default::default()
    };
    let mut statuslines = Statuslines::default();
    let mut frame = Frame {
        width: 80,
        height: 24,
        ..Default::default()
    };
    assert!(pages.render(&mut frame, Some(&config), &mut statuslines, true));
    let status = frame
        .rows
        .last()
        .unwrap()
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(status.contains("SELECT"));
    assert!(status.contains("2 of 2"));
    assert!(statuslines.requests().is_empty());
}

#[test]
fn browser_rows_and_status_share_the_right_inset_beside_a_preview() {
    use zess_protocol::resource::PageMetadata;
    use zess_ui::{DisplayCell, Span};

    let mut pages = Pages::default();
    pages.resize(81, 8);
    pages.command("git", 1);
    let mut update = rows(pages.subscription().unwrap(), 0, &["note.txt"]);
    update.metadata = Some(PageMetadata {
        view_id: 12,
        buffer_id: 7,
        options: Value::Null,
    });
    let display = update.output.display.as_mut().unwrap();
    display.status = json!({"git": {"additions": 15517, "deletions": 9778}});
    display.rows[0].style = "modified".into();
    display.rows[0].cells = [
        ("M", "modified"),
        ("+1", "git.added"),
        ("-1", "git.deleted"),
    ]
    .into_iter()
    .map(|(text, style)| DisplayCell {
        spans: vec![Span {
            base_style: None,
            text: text.into(),
            style: style.into(),
        }],
    })
    .collect();
    update.output.requests.push(HostRequest::Preview {
        path: "note.txt".into(),
        content: PreviewContent::Text {
            text: "preview".into(),
        },
        focus: false,
    });
    pages.apply(update);
    let config = StatusConfig {
        builtin_statuslines: ["default".into()].into(),
        ..Default::default()
    };
    let mut statuslines = Statuslines::default();
    for (width, expected) in [
        (81, "note.txt                      M  +1  -1 "),
        (25, "note.txt    "),
        (9, "not "),
        (3, "n"),
    ] {
        pages.resize(width, 8);
        let mut frame = Frame {
            width,
            height: 8,
            ..Default::default()
        };
        assert!(pages.render(&mut frame, Some(&config), &mut statuslines, true));
        let row = &frame.rows[0].spans;
        let divider = row.iter().position(|span| span.style == "window").unwrap();
        let body = &row[..divider];
        let text: String = body.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(text, expected);
        assert_eq!(body[0].style, "selection.modified");
        if width == 81 {
            for (text, style) in [
                ("+1", "selection.git.added"),
                ("-1", "selection.git.deleted"),
            ] {
                assert!(
                    body.iter()
                        .any(|span| span.text == text && span.style == style)
                );
            }
            let status = &frame.rows.last().unwrap().spans;
            let divider = status
                .iter()
                .position(|span| span.style == "window")
                .unwrap();
            let text: String = status[..divider]
                .iter()
                .map(|span| span.text.as_str())
                .collect();
            assert_eq!(text.len(), 40);
            assert!(text.ends_with("-9778 "), "{text:?}");
        }
    }
}

#[test]
fn moving_to_another_item_keeps_an_earlier_preview_from_taking_focus() {
    let mut pages = Pages::default();
    pages.resize(80, 24);
    pages.command("git", 1);
    let id = pages.subscription().unwrap();
    pages.apply(rows(id, 0, &["first", "second"]));
    pages.command("data.move-down", 1);
    let mut old = rows(id, 0, &["first", "second"]);
    old.output.requests.push(HostRequest::Preview {
        path: "first".into(),
        content: PreviewContent::Text {
            text: "first file".into(),
        },
        focus: true,
    });
    pages.apply(old);
    assert_eq!(pages.scope(), Some("git"));
    assert_eq!(
        pages.snapshot().stack[0].selection.primary.as_deref(),
        Some("second")
    );
    assert!(pages.snapshot().stack[0].preview.is_none());
}

#[test]
fn reopening_files_shows_cached_rows_before_the_new_subscription_finishes_loading() {
    let mut pages = Pages::default();
    pages.resize(80, 24);
    pages.command("files", 1);
    let previous = pages.subscription().unwrap();
    pages.apply(rows(previous, 0, &["cached"]));
    pages.command("quit", 1);
    pages.command("files", 1);
    let current = pages.subscription().unwrap();
    pages.apply(rows(previous, 0, &["late"]));
    let mut loading = rows(current, 0, &[]);
    loading.output.display.as_mut().unwrap().loading = true;
    pages.apply(loading);
    let mut frame = Frame {
        width: 80,
        height: 24,
        ..Default::default()
    };
    assert!(pages.render(&mut frame, None, &mut Default::default(), true));
    let text: String = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .map(|span| span.text.as_str())
        .collect();
    assert!(text.contains("cached"));
    assert!(!text.contains("late"));
}

#[test]
fn returning_to_a_restored_tab_recovers_its_child_page_and_return_point() {
    let mut pages = Pages::default();
    pages.open("files", json!({}));
    let files = pages.active_page_id().unwrap();
    pages.open("git-history", json!({}));
    let history = pages.active_page_id().unwrap();
    pages.open("git-commit-files", json!({"commit": "abc"}));
    let commit_files = pages.active_page_id().unwrap();
    pages.apply(rows(pages.subscription().unwrap(), 0, &["file:note.txt"]));

    let switch = |current: &str, tab_kind: &str, source: &str, target: &str, kind: &str| {
        HostRequest::Navigate {
            navigation: Navigation::Page {
                group: "git".into(),
                current: current.into(),
                page: target.into(),
                kind: kind.into(),
                argument: json!({}),
                source: Some(PageNavigationSource {
                    kind: source.into(),
                    tab_kind: tab_kind.into(),
                }),
            },
        }
    };
    assert!(pages.request(
        &switch(
            "history",
            "git-history",
            "git-commit-files",
            "changes",
            "git"
        ),
        1
    ));
    assert_eq!(pages.scope(), Some("git"));
    let saved = serde_json::to_value(pages.snapshot()).unwrap();
    let mut restored = Pages::default();
    restored.restore(serde_json::from_value(saved).unwrap(), None);
    restored.take_requests();

    assert!(restored.request(
        &switch("changes", "git", "git", "history", "git-history"),
        1
    ));
    assert!(restored.take_requests().into_iter().any(|message| matches!(
        message,
        ClientMessage::Resource {
            request: ResourceRequest::Subscribe {
                page_id, input, ..
            }
        } if page_id == commit_files && input.selection.primary.as_deref() == Some("file:note.txt")
    )));
    assert!(restored.command("q", 1));
    assert_eq!(restored.active_page_id(), Some(history));
    assert!(restored.command("q", 1));
    assert_eq!(restored.active_page_id(), Some(files));
}

fn publish_preview(pages: &mut Pages, preview: HostRequest) {
    let mut update = rows(pages.subscription().unwrap(), 0, &["file"]);
    update.output.requests.push(preview);
    pages.apply(update);
}

fn render_preview(pages: &mut Pages) -> Frame {
    let mut frame = Frame {
        width: 60,
        height: 8,
        ..Frame::default()
    };
    assert!(pages.render(&mut frame, None, &mut Default::default(), true));
    frame
}

#[test]
fn text_preview_rendering_and_hit_testing_follow_view_options() {
    use zess_protocol::editor_events::{Cursor, EditorSelection};

    for (options, expected_gutter) in [
        (json!({"gutter": false, "header-style": "keyword"}), None),
        (
            json!({"gutter-left-padding": 4, "header-style": "keyword"}),
            Some("    1 "),
        ),
    ] {
        let mut pages = Pages::default();
        pages.resize(60, 8);
        pages.command("files", 1);
        publish_preview(
            &mut pages,
            HostRequest::PreviewGroup {
                panes: vec![PreviewPane::View {
                    id: "file".into(),
                    path: "file.txt".into(),
                    header: "File".into(),
                    loading: false,
                    content: PreviewContent::Text {
                        text: "abcdef\nsecond\n".into(),
                    },
                    key_scope: None,
                }],
                layout: PreviewLayout::Rows,
                focus: true,
            },
        );
        let config = StatusConfig {
            view_options: [("preview".into(), options)].into(),
            ..Default::default()
        };
        let mut frame = Frame {
            width: 60,
            height: 8,
            ..Default::default()
        };
        assert!(pages.render(&mut frame, Some(&config), &mut Default::default(), true));
        assert!(
            frame.rows[0]
                .spans
                .iter()
                .any(|span| { span.text.trim_end() == "File" && span.style == "keyword" })
        );
        assert_eq!(
            frame.rows[1]
                .spans
                .iter()
                .find(|span| span.style == "linenr.current")
                .map(|span| span.text.as_str()),
            expected_gutter
        );
        let body_x = 30 + expected_gutter.map_or(0, str::len) as u16;
        assert_eq!(frame.cursor, Some((body_x, 1)));
        assert!(matches!(
            pages.mouse(
                &MouseEvent {
                    kind: MouseKind::Press,
                    column: body_x + 2,
                    row: 1,
                },
                3,
            ),
            Some(PageMouseHit::Handled)
        ));
        assert_eq!(
            pages.event_selection(),
            Some(EditorSelection::Text {
                cursors: vec![Cursor {
                    row: 0,
                    column: 2,
                    anchor_row: 0,
                    anchor_column: 2,
                }],
            })
        );
    }
}

#[test]
fn passive_file_previews_wrap_at_the_pane_edge_without_a_gutter() {
    use zess_protocol::resource::PageMetadata;

    let text = "1234567890".repeat(10);
    for (options, wrap) in [(Value::Null, true), (json!({"preview-wrap": false}), false)] {
        let mut pages = Pages::default();
        pages.resize(60, 8);
        pages.command("files", 1);
        let mut update = rows(pages.subscription().unwrap(), 0, &["note.txt"]);
        update.metadata = Some(PageMetadata {
            view_id: 12,
            buffer_id: 7,
            options,
        });
        update.output.requests.push(HostRequest::PreviewGroup {
            panes: vec![PreviewPane::Surface {
                id: "main".into(),
                path: "note.txt".into(),
                header: String::new(),
                loading: false,
                content: PreviewContent::Text { text: text.clone() },
            }],
            layout: PreviewLayout::Rows,
            focus: false,
        });
        pages.apply(update);
        let frame = render_preview(&mut pages);
        assert_eq!(frame.cursor, None);
        for (index, row) in frame.rows.iter().enumerate() {
            let divider = row
                .spans
                .iter()
                .position(|span| span.style == "window")
                .unwrap();
            let preview = &row.spans[divider + 1..];
            let line: String = preview.iter().map(|span| span.text.as_str()).collect();
            let expected = match index {
                0 => &text[..30],
                1 if wrap => &text[30..60],
                2 if wrap => &text[60..90],
                3 if wrap => &text[90..],
                _ => "",
            };
            assert_eq!(line.trim_end(), expected);
            assert!(preview.iter().all(|span| span.style.is_empty()));
        }
    }
}

#[test]
fn opening_a_snapshot_preview_uses_its_cursor_line() {
    let mut pages = Pages::default();
    pages.resize(80, 24);
    pages.open("git-commit-files", json!({}));
    publish_preview(
        &mut pages,
        HostRequest::Preview {
            path: "historical.txt".into(),
            content: PreviewContent::Snapshot {
                snapshot: FileSnapshot {
                    id: SnapshotId {
                        repository: "repo".into(),
                        commit: "revision".into(),
                        path: "archive/historical.txt".into(),
                        blob: "blob".into(),
                    },
                    text: "start\nsecond\nthird\n".into(),
                },
            },
            focus: true,
        },
    );
    pages.take_requests();
    pages.text_command("text.move-down", 2).unwrap().unwrap();
    assert!(pages.command("preview.open-source", 1));
    assert!(matches!(
        pages.take_requests().as_slice(),
        [ClientMessage::Resource {
            request: ResourceRequest::Update {
                event: ResourceEvent::CommitSource {
                    commit, path, line: 2, fullscreen: false,
                },
                ..
            }
        }] if commit == "revision" && path == "archive/historical.txt"
    ));
}

fn git_preview(id: u64, revision: Option<u64>) -> ResourceUpdate {
    let mut update = rows(id, 0, &["file"]);
    update.patch_revision = revision;
    update.output.requests.push(HostRequest::PreviewGroup {
        panes: vec![PreviewPane::View {
            id: "unstaged".into(),
            path: "file".into(),
            header: String::new(),
            loading: false,
            key_scope: None,
            content: PreviewContent::GitDiff {
                diff: GitDiff {
                    source: DiffSource::Changes {
                        side: DiffSide::Unstaged,
                    },
                    files: vec![FileDiff {
                        target: DiffTarget {
                            path: "file".into(),
                            original_path: None,
                        },
                        raw: "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-before\n+after\n".into(),
                        rendered: None,
                    }],
                },
            },
        }],
        layout: PreviewLayout::Columns,
        focus: true,
    });
    update
}

#[test]
fn raw_diff_preview_uses_the_configured_gutter() {
    let mut pages = Pages::default();
    pages.resize(60, 8);
    pages.command("git", 1);
    pages.apply(git_preview(pages.subscription().unwrap(), None));

    for (options, expected) in [
        (json!({"gutter": false}), "-before"),
        (json!({"gutter-left-padding": 4}), "    1 -before"),
    ] {
        let config = StatusConfig {
            view_options: [("diff".into(), options)].into(),
            ..Default::default()
        };
        let mut frame = Frame {
            width: 60,
            height: 8,
            ..Default::default()
        };
        assert!(pages.render(&mut frame, Some(&config), &mut Default::default(), true));
        let row = frame
            .rows
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .find(|row| row.contains("-before"))
            .unwrap();
        assert_eq!(row.split_once('│').unwrap().1.trim_end(), expected);
    }
}

#[derive(Clone, Copy, Debug)]
enum GitPreviewEvent {
    Reconnect,
    Reopen,
    Split,
    CurrentPreview,
    RetiredPreview,
    PendingPreview,
    PreviewWithoutPatch,
    Message,
}

#[hegel::test(derandomize = true)]
fn staging_requires_a_preview_published_by_the_live_subscription(tc: TestCase) {
    use GitPreviewEvent::*;

    let events = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            Reconnect,
            Reopen,
            Split,
            CurrentPreview,
            RetiredPreview,
            PendingPreview,
            PreviewWithoutPatch,
            Message,
        ]))
        .max_size(24),
    );
    let mut pages = Pages::default();
    pages.resize(80, 24);
    pages.command("git", 1);
    let mut id = pages.subscription().unwrap();
    pages.apply(git_preview(id, Some(1)));

    let mut retired = None;
    let mut patch_available = true;
    for event in events {
        tc.note(&format!("{event:?}"));
        match event {
            Reconnect | Reopen | Split => {
                match event {
                    Reconnect => pages.reconnect(),
                    Reopen => {
                        assert!(pages.command("quit", 1));
                        assert!(pages.command("git", 1));
                    }
                    Split => pages = pages.split().unwrap(),
                    _ => unreachable!(),
                }
                retired = Some(id);
                id = pages.subscription().unwrap();
                patch_available = false;
                assert!(
                    render_preview(&mut pages)
                        .rows
                        .iter()
                        .any(|row| { row.spans.iter().any(|span| span.text.contains("after")) })
                );
            }
            CurrentPreview => {
                pages.apply(git_preview(id, Some(1)));
                patch_available = true;
            }
            RetiredPreview => {
                if let Some(retired) = retired {
                    pages.apply(git_preview(retired, Some(1)));
                }
            }
            PendingPreview | Message => {
                let mut update = rows(id, 0, &["file"]);
                update.output.requests.push(match event {
                    PendingPreview => {
                        patch_available = false;
                        HostRequest::PreviewPending
                    }
                    _ => HostRequest::Message {
                        text: "Done".into(),
                    },
                });
                pages.apply(update);
            }
            PreviewWithoutPatch => {
                pages.apply(git_preview(id, None));
                patch_available = false;
            }
        }
        assert!(pages.command("diff.apply-selection", 1));
        let actions = pages.take_actions();
        if patch_available {
            assert!(matches!(
                actions.as_slice(),
                [CommandAction::ApplyPagePatch {
                    subscription_id, request,
                }] if *subscription_id == id && request.revision == 1
            ));
        } else {
            assert!(actions.is_empty(), "emitted a patch from an old preview");
        }
    }
}

#[test]
fn returning_to_the_page_preserves_focus_when_the_preview_refreshes() {
    let mut pages = Pages::default();
    pages.resize(80, 24);
    pages.command("files", 1);
    let preview = |text: &str| HostRequest::Preview {
        path: "file".into(),
        content: PreviewContent::Text { text: text.into() },
        focus: true,
    };
    publish_preview(&mut pages, preview("initial"));
    assert_eq!(pages.scope(), Some("preview"));
    pages.command("preview.return", 1);
    publish_preview(&mut pages, preview("refreshed"));
    assert_eq!(pages.scope(), Some("files"));
    pages.command("preview.focus", 1);
    assert_eq!(pages.preview_text().unwrap().0.text(), "refreshed");

    pages.command("preview.return", 1);
    pages.take_requests();
    pages.resize(100, 24);
    let requests = pages.take_requests();
    let [
        ClientMessage::Resource {
            request:
                ResourceRequest::Update {
                    subscription_id,
                    input,
                    event: ResourceEvent::Resize,
                },
        },
    ] = requests.as_slice()
    else {
        panic!("expected a resize update");
    };
    let mut update = rows(*subscription_id, input.generation, &["file"]);
    update.output.requests.push(preview("resized"));
    pages.apply(update);
    assert_eq!(pages.scope(), Some("files"));
    pages.command("preview.focus", 1);
    assert_eq!(pages.preview_text().unwrap().0.text(), "resized");
}

#[test]
fn late_results_for_a_closed_page_release_its_fullscreen_document() {
    let mut pages = Pages::default();
    pages.command("files", 1);
    let previous = pages.subscription().unwrap();
    pages.command("quit", 1);
    pages.command("files", 1);
    let current = pages.subscription().unwrap();
    assert_ne!(current, previous);

    let mut update = rows(previous, 0, &["file"]);
    update.output.requests.push(HostRequest::MiniBuffer {
        purpose: "rename".into(),
        text: "file".into(),
        key_scope: None,
        confirmation: None,
    });
    update.document = Some(PageDocument {
        buffer_id: 8,
        line: 0,
        fullscreen: true,
    });
    let publication = pages.apply(update);

    assert!(publication.requests.is_empty());
    assert!(publication.document.is_none());
    assert_eq!(pages.subscription(), Some(current));
    assert!(matches!(
        pages.take_actions().as_slice(),
        [CommandAction::ClosePageDocument {
            buffer_id: 8,
            force: false,
        }]
    ));
}

#[test]
fn source_navigation_tracks_its_target_across_local_interaction() {
    let mut pages = Pages::default();
    pages.resize(80, 24);
    pages.command("git", 1);
    let mut update = rows(pages.subscription().unwrap(), 0, &["file", "second"]);
    update.output.requests.push(HostRequest::Preview {
        path: "file".into(),
        content: PreviewContent::Diff {
            text:
                "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-before\n+after\n"
                    .into(),
        },
        focus: true,
    });
    pages.apply(update);
    assert_eq!(pages.scope(), Some("diff"));
    pages.take_requests();

    let request_source = |pages: &mut Pages, buffer_id, fullscreen: bool| {
        let command = if fullscreen {
            "diff.edit-source"
        } else {
            "preview.open-source"
        };
        assert!(pages.command(command, 1));
        let requests = pages.take_requests();
        let [
            ClientMessage::Resource {
                request:
                    ResourceRequest::Update {
                        subscription_id,
                        input,
                        event:
                            ResourceEvent::OpenSource {
                                fullscreen: requested,
                                ..
                            },
                    },
            },
        ] = requests.as_slice()
        else {
            panic!("expected a source request");
        };
        assert_eq!(*requested, fullscreen);
        ResourceUpdate {
            cache: Default::default(),
            kind: ResourceUpdateKind::SourceNavigation,
            subscription_id: *subscription_id,
            generation: input.generation,
            document_scoped: false,
            metadata: None,
            document: Some(PageDocument {
                buffer_id,
                line: 0,
                fullscreen,
            }),
            patch_revision: None,
            output: PluginOutput::default(),
        }
    };

    let response = request_source(&mut pages, 8, true);
    assert!(pages.command("preview.return", 1));
    assert!(pages.apply(response).document.is_none());
    assert_eq!(pages.scope(), Some("git"));
    assert!(matches!(
        pages.take_actions().as_slice(),
        [CommandAction::ClosePageDocument {
            buffer_id: 8,
            force: false,
        }]
    ));

    assert!(pages.command("preview.focus", 1));
    let response = request_source(&mut pages, 9, true);
    pages.resize(100, 30);
    assert_eq!(pages.apply(response).document.unwrap().buffer_id, 9);
    assert!(pages.take_actions().is_empty());

    pages.take_requests();
    let response = request_source(&mut pages, 10, false);
    assert!(pages.command("preview.return", 1));
    assert!(pages.command("data.move-down", 1));
    assert!(pages.apply(response).document.is_none());
    assert!(pages.take_actions().is_empty());
}

#[test]
fn refreshing_a_text_preview_preserves_the_dragged_selection() {
    use zess_protocol::editor_events::{Cursor, EditorSelection};

    let mut pages = Pages::default();
    pages.resize(60, 8);
    pages.command("files", 1);
    let preview = |text: &str| HostRequest::Preview {
        path: "file.txt".into(),
        content: PreviewContent::Text { text: text.into() },
        focus: true,
    };
    publish_preview(&mut pages, preview("one\n界abc\n"));
    let (body_x, body_y) = render_preview(&mut pages).cursor.unwrap();
    pages.mouse(
        &MouseEvent {
            kind: MouseKind::Press,
            column: body_x + 2,
            row: body_y + 1,
        },
        3,
    );
    pages.mouse(
        &MouseEvent {
            kind: MouseKind::Drag,
            column: body_x + 4,
            row: body_y + 1,
        },
        3,
    );
    pages.mouse(
        &MouseEvent {
            kind: MouseKind::Release,
            column: body_x + 4,
            row: body_y + 1,
        },
        3,
    );

    let selected = Some(EditorSelection::Text {
        cursors: vec![Cursor {
            row: 1,
            column: 3,
            anchor_row: 1,
            anchor_column: 1,
        }],
    });
    assert_eq!(pages.event_selection(), selected);
    publish_preview(&mut pages, preview("changed first line\n界abc\n"));
    assert_eq!(pages.event_selection(), selected);
}

#[test]
fn restoring_a_preview_waits_for_its_content_before_scrolling() {
    use zess_plugin::{PreviewLayout, PreviewPane};

    let mut pages = Pages::default();
    pages.resize(60, 8);
    pages.command("files", 1);
    let text: String = (0..30).map(|n| format!("preview line {n}\n")).collect();
    let preview = |loading: bool| HostRequest::PreviewGroup {
        panes: vec![PreviewPane::View {
            id: "file".into(),
            path: "file.txt".into(),
            header: "File".into(),
            loading,
            content: PreviewContent::Text {
                text: if loading { String::new() } else { text.clone() },
            },
            key_scope: None,
        }],
        focus: false,
        layout: PreviewLayout::Rows,
    };
    publish_preview(&mut pages, preview(false));
    let frame = render_preview(&mut pages);
    pages.mouse(
        &MouseEvent {
            kind: MouseKind::ScrollDown,
            column: frame.width - 1,
            row: 2,
        },
        9,
    );
    let workspace = pages.snapshot();
    assert_eq!(workspace.stack[0].preview.as_ref().unwrap().scroll, 9);
    assert_eq!(pages.scope(), Some("files"));

    pages.restore(workspace, None);
    publish_preview(&mut pages, preview(true));
    render_preview(&mut pages);
    publish_preview(&mut pages, preview(false));
    let frame = render_preview(&mut pages);
    assert!(
        frame.rows[1]
            .spans
            .iter()
            .any(|span| span.text.contains("preview line 9"))
    );
    assert_eq!(
        pages.snapshot().stack[0].preview.as_ref().unwrap().scroll,
        9
    );
}
