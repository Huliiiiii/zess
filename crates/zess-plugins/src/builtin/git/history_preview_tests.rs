use super::*;
use std::{cell::RefCell, rc::Rc, time::Duration};
use zess_plugin::{ItemSelection, PluginContext};
use zess_services::NativeServices;

fn services() -> Services {
    Rc::new(RefCell::new(
        NativeServices::new(std::env::current_dir().unwrap()).unwrap(),
    ))
}

fn input() -> PluginInput {
    PluginInput {
        buffer_id: 1,
        view_id: 1,
        generation: 1,
        preview_debounce: Duration::from_secs(60),
        preview_area: None,
        event: "select".into(),
        argument: serde_json::Value::Null,
        query: None,
        options: serde_json::Value::Null,
        mode: "normal".into(),
        selection: ItemSelection::default(),
        register: None,
        context: PluginContext::default(),
    }
}

fn output() -> PluginOutput {
    PluginOutput {
        display: Some(Display::default()),
        ..PluginOutput::default()
    }
}

fn commit(oid: &str) -> Commit {
    Commit {
        oid: oid.into(),
        author: "author".into(),
        date: "2026-01-01".into(),
        message: "message".into(),
    }
}

fn next_handle(services: &Services) -> u64 {
    services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null)["next_handle"]
        .as_u64()
        .unwrap()
}

#[test]
fn opening_a_commit_reuses_only_its_own_published_files() {
    let services = services();
    let first = commit("1111111111111111111111111111111111111111");
    let second = commit("2222222222222222222222222222222222222222");
    let mut history = Git::new(services.clone(), Page::History);
    history.models.insert(
        1,
        Model {
            commits: vec![commit(&first.oid), commit(&second.oid)],
            result: LoadResult::Ready,
            ..Model::default()
        },
    );
    history.views.insert(
        (1, 1),
        GitView {
            history: HistoryPreview {
                state: QueryState::Idle {
                    query: Some(first.oid.clone()),
                },
                published: Some(HistorySummary {
                    commit: first.oid.clone(),
                    info: vec![],
                    summary: vec![],
                    files: Some(CommitFileList {
                        files: vec![CommitFile {
                            path: "one".into(),
                            status: "A".into(),
                            original_path: None,
                            additions: None,
                            deletions: None,
                        }],
                        path_warning: Some("Skipped 1 Git entry with a non-UTF-8 path".into()),
                        line_count_error: Some("counts unavailable".into()),
                    }),
                }),
                ..HistoryPreview::default()
            },
            ..GitView::default()
        },
    );
    let mut input = input();
    input.event = "open".into();
    input.selection = browser::single(Some(first.oid.clone()));
    let first_output = history.update(&input).unwrap();
    let first_argument = first_output
        .requests
        .into_iter()
        .find_map(|request| match request {
            HostRequest::Navigate {
                navigation: Navigation::Push { argument, .. },
            } => Some(argument),
            _ => None,
        })
        .unwrap();
    let mut files = Git::new(services, Page::CommitFiles);
    input.event = "view_entered".into();
    input.argument = first_argument;
    input.selection = ItemSelection::default();
    let output = files.update(&input).unwrap();
    let display = output.display.unwrap();
    assert_eq!(display.rows[0].id, "file:one");
    assert!(
        display
            .header
            .ends_with("Skipped 1 Git entry with a non-UTF-8 path")
    );
    assert_eq!(
        display.status["error"],
        "git.commit-files: counts unavailable"
    );

    input.event = "open".into();
    input.argument = serde_json::Value::Null;
    input.selection = browser::single(Some(second.oid));
    let second_output = history.update(&input).unwrap();
    let second_argument = second_output
        .requests
        .into_iter()
        .find_map(|request| match request {
            HostRequest::Navigate {
                navigation: Navigation::Push { argument, .. },
            } => Some(argument),
            _ => None,
        })
        .unwrap();
    assert!(second_argument["files"].is_null());
}

#[test]
fn repeated_steps_at_the_same_commit_cancel_the_unpublished_summary() {
    let services = services();
    let input = input();
    let selected = commit("1111111111111111111111111111111111111111");
    let task = Task::start(
        &services,
        QueryRequest::CommitFiles {
            commit: selected.oid.clone(),
        },
    )
    .unwrap();
    let mut preview = HistoryPreview {
        state: QueryState::Running {
            query: selected.oid.clone(),
            task,
        },
        published: Some(HistorySummary {
            commit: "previous".into(),
            files: None,
            info: Vec::new(),
            summary: vec![DisplayRow {
                text: "previous summary".into(),
                ..DisplayRow::default()
            }],
        }),
        ..HistoryPreview::default()
    };
    let now = Instant::now();
    preview
        .navigation
        .update(Event::Select, &input, now)
        .unwrap();
    let before = next_handle(&services);
    for elapsed in [10, 20, 79] {
        preview
            .advance(
                &services,
                Event::Select,
                &input,
                Some(&selected),
                &mut output(),
                now + Duration::from_secs(elapsed),
            )
            .unwrap();
        assert!(matches!(preview.state, QueryState::Waiting { ready_at, .. }
            if ready_at == now + Duration::from_secs(elapsed) + input.preview_debounce));
        assert_eq!(
            preview
                .published
                .as_ref()
                .map(|summary| summary.commit.as_str()),
            Some("previous")
        );
        assert_eq!(
            preview.published.as_ref().unwrap().summary[0].text,
            "previous summary"
        );
    }
    let stats = services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null);
    assert_eq!(stats["jobs"], 0);
    assert_eq!(stats["next_handle"], before + 1);
}

#[test]
fn navigation_keeps_the_previous_commit_visible_until_the_last_selection_settles() {
    let services = services();
    let input = input();
    let first = commit("1111111111111111111111111111111111111111");
    let last = commit("2222222222222222222222222222222222222222");
    let now = Instant::now();
    let mut preview = HistoryPreview {
        published: Some(HistorySummary {
            commit: "previous commit".into(),
            files: None,
            info: vec![DisplayRow {
                text: "previous info".into(),
                ..DisplayRow::default()
            }],
            summary: vec![DisplayRow {
                text: "previous summary".into(),
                ..DisplayRow::default()
            }],
        }),
        ..HistoryPreview::default()
    };
    let before = next_handle(&services);
    for (event, commit, elapsed) in [
        (Event::Select, &first, 0),
        (Event::Select, &last, 10),
        (Event::FilterChanged, &last, 20),
        (Event::MiniBufferChanged, &last, 30),
        (Event::parse("tick"), &last, 89),
    ] {
        let mut output = output();
        preview
            .advance(
                &services,
                event,
                &input,
                Some(commit),
                &mut output,
                now + Duration::from_secs(elapsed),
            )
            .unwrap();
        assert_eq!(
            preview.published.as_ref().unwrap().info[0].text,
            "previous info"
        );
        assert_eq!(
            preview.published.as_ref().unwrap().summary[0].text,
            "previous summary"
        );
        for request in output.requests {
            if let HostRequest::PreviewGroup { panes, .. } = request {
                assert!(panes.iter().all(|pane| matches!(pane, PreviewPane::View { path, .. } if path == "previous commit")));
                assert!(
                    matches!(&panes[1], PreviewPane::View { content: PreviewContent::Items { rows, .. }, .. } if rows[0].text == "previous summary")
                );
            }
        }
    }

    assert!(matches!(
        preview.state,
        QueryState::Waiting { ready_at, .. } if ready_at == now + Duration::from_secs(90)
    ));
    assert_eq!(preview.state.query(), Some(&last.oid));
    // The leading step starts one request; the final selection still waits.
    assert_eq!(next_handle(&services), before + 2);
}

#[test]
fn ticks_start_the_last_selected_commit_once_after_the_delay() {
    let services = services();
    let input = input();
    let selected = commit("1111111111111111111111111111111111111111");
    let now = Instant::now();
    let mut preview = HistoryPreview {
        state: QueryState::Waiting {
            query: selected.oid.clone(),
            ready_at: now,
        },
        ..HistoryPreview::default()
    };
    let before = next_handle(&services);
    for _ in 0..2 {
        preview
            .advance(
                &services,
                Event::parse("tick"),
                &input,
                Some(&selected),
                &mut output(),
                now,
            )
            .unwrap();
    }

    assert!(!matches!(preview.state, QueryState::Waiting { .. }));
    assert_eq!(preview.state.query(), Some(&selected.oid));
    assert_eq!(next_handle(&services), before + 2);
}

#[test]
fn an_explicit_preview_starts_without_waiting_for_navigation_to_settle() {
    let services = services();
    let input = input();
    let selected = commit("1111111111111111111111111111111111111111");
    let now = Instant::now();
    let mut preview = HistoryPreview::default();
    let mut output = output();
    preview
        .advance(
            &services,
            Event::Preview,
            &input,
            Some(&selected),
            &mut output,
            now,
        )
        .unwrap();

    assert!(!matches!(preview.state, QueryState::Waiting { .. }));
    assert_eq!(preview.state.query(), Some(&selected.oid));
    assert!(
        output
            .requests
            .iter()
            .any(|request| matches!(request, HostRequest::PreviewGroup { focus: true, .. }))
    );
}

#[test]
fn leaving_the_view_releases_the_running_commit_query() {
    let services = services();
    let input = input();
    let selected = commit("1111111111111111111111111111111111111111");
    let task = Task::start(
        &services,
        QueryRequest::CommitFiles {
            commit: selected.oid.clone(),
        },
    )
    .unwrap();
    let mut preview = HistoryPreview {
        state: QueryState::Running {
            query: selected.oid.clone(),
            task,
        },
        ..HistoryPreview::default()
    };
    let mut output = output();
    preview
        .advance(
            &services,
            Event::ViewLeaving,
            &input,
            Some(&selected),
            &mut output,
            Instant::now(),
        )
        .unwrap();

    assert!(matches!(preview.state, QueryState::Idle { query: None }));
    assert_eq!(
        services
            .borrow_mut()
            .call("service.stats", serde_json::Value::Null)["jobs"],
        0
    );
    assert!(output.requests.is_empty());
}

#[test]
fn an_empty_completed_source_clears_the_previous_commit() {
    let services = services();
    let input = input();
    let mut preview = HistoryPreview {
        published: Some(HistorySummary {
            commit: "previous commit".into(),
            files: None,
            info: vec![DisplayRow {
                text: "previous info".into(),
                ..DisplayRow::default()
            }],
            summary: vec![DisplayRow {
                text: "previous summary".into(),
                ..DisplayRow::default()
            }],
        }),
        ..HistoryPreview::default()
    };
    let mut loading = output();
    loading.display.as_mut().unwrap().loading = true;
    preview
        .advance(
            &services,
            Event::FilterChanged,
            &input,
            None,
            &mut loading,
            Instant::now(),
        )
        .unwrap();
    assert_eq!(
        preview.published.as_ref().unwrap().summary[0].text,
        "previous summary"
    );
    preview
        .advance(
            &services,
            Event::parse("tick"),
            &input,
            None,
            &mut output(),
            Instant::now(),
        )
        .unwrap();

    assert!(preview.published.is_none());
    assert_eq!(
        preview.totals.status(),
        json!({"git": {
            "state": "ready",
            "additions": 0,
            "deletions": 0,
            "unknown_files": 0,
        }})
    );
}

#[test]
fn jumping_to_a_commit_starts_its_summary_during_navigation() {
    let services = services();
    let mut input = input();
    let first = commit("1111111111111111111111111111111111111111");
    let last = commit("2222222222222222222222222222222222222222");
    let now = Instant::now();
    let mut preview = HistoryPreview::default();
    preview
        .advance(
            &services,
            Event::Select,
            &input,
            Some(&first),
            &mut output(),
            now,
        )
        .unwrap();
    preview
        .advance(
            &services,
            Event::Select,
            &input,
            Some(&last),
            &mut output(),
            now + Duration::from_secs(1),
        )
        .unwrap();
    assert!(matches!(preview.state, QueryState::Waiting { .. }));
    input.argument = serde_json::to_value(zess_plugin::SelectionChange {
        navigation: Some(zess_plugin::ItemNavigation::Jump),
    })
    .unwrap();
    let before = next_handle(&services);
    preview
        .advance(
            &services,
            Event::Select,
            &input,
            Some(&last),
            &mut output(),
            now + Duration::from_secs(2),
        )
        .unwrap();
    assert!(!matches!(preview.state, QueryState::Waiting { .. }));
    assert_eq!(preview.state.query(), Some(&last.oid));
    assert_eq!(next_handle(&services), before + 2);
}
