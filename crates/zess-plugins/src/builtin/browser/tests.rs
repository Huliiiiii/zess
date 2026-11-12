use super::*;
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use zess_plugin::{ItemNavigation, ItemSelection, PluginContext, SelectionChange};
use zess_services::NativeServices;
use zess_ui::Display;

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

fn query() -> PreviewQuery {
    PreviewQuery {
        request: QueryRequest::List {
            path: String::new(),
        },
        path: String::new(),
    }
}

fn output() -> PluginOutput {
    PluginOutput {
        display: Some(Display::default()),
        ..PluginOutput::default()
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "zess-browser-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for name in ["first", "last", "cold"] {
            fs::write(root.join(name), name).unwrap();
        }
        Self(root)
    }

    fn services(&self) -> Services {
        Rc::new(RefCell::new(NativeServices::new(self.0.clone()).unwrap()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn file_query(path: &str) -> PreviewQuery {
    PreviewQuery {
        request: QueryRequest::Preview {
            path: path.into(),
            show_ignored: false,
        },
        path: path.into(),
    }
}

fn warm(services: &Services, query: &PreviewQuery) -> zess_services::Observation {
    let observation = services
        .borrow_mut()
        .observe(query.request.operation(), &query.request.argument())
        .unwrap();
    let mut task = Task::start(services, query.request.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while task.poll().unwrap().is_none() {
        assert!(Instant::now() < deadline, "cache did not warm");
        std::thread::yield_now();
    }
    observation
}

fn next_handle(services: &Services) -> u64 {
    services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null)["next_handle"]
        .as_u64()
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn directory_previews_report_non_utf8_names_without_hiding_other_entries() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("dir")).unwrap();
    fs::write(fixture.0.join("dir/readable"), "good\n").unwrap();
    fs::write(fixture.0.join(OsStr::from_bytes(b"dir/raw-\xff")), "raw\n").unwrap();
    let services = fixture.services();
    let query = file_query("dir");
    let _observation = warm(&services, &query);
    let mut browser = Browser::cloned();
    let mut output = output();
    browser
        .update_surface(
            &services,
            Event::Preview,
            &input(),
            &mut output,
            Some(query),
        )
        .unwrap();

    let rows = output
        .requests
        .into_iter()
        .find_map(|request| match request {
            HostRequest::PreviewGroup { mut panes, .. } => match panes.pop()? {
                PreviewPane::Surface {
                    content: PreviewContent::Directory { rows, .. },
                    ..
                } => Some(rows),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].text,
        "Skipped 1 directory entry with a non-UTF-8 name"
    );
    assert!(rows[0].id.is_empty());
    assert_eq!(rows[1].id, "dir/readable");
}

#[test]
fn observed_files_publish_immediately_without_starting_queries() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let _observations = [
        warm(&services, &file_query("first")),
        warm(&services, &file_query("last")),
    ];
    let mut browser = Browser::cloned();
    let input = input();
    let before = next_handle(&services);
    for path in ["first", "last", "first"] {
        let mut output = output();
        browser
            .update_surface(
                &services,
                Event::Select,
                &input,
                &mut output,
                Some(file_query(path)),
            )
            .unwrap();
        assert!(output.requests.iter().any(|request| matches!(request,
            HostRequest::PreviewGroup { panes, focus: false, .. }
                if matches!(&panes[..], [PreviewPane::Surface { path: actual, content: PreviewContent::Text { text }, .. }]
                    if actual == path && text == path)
        )));
        assert!(
            !output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::Schedule { .. }))
        );
        assert!(!output.display.unwrap().loading);
    }
    assert_eq!(next_handle(&services), before + 1);

    let mut output = output();
    browser
        .update(
            &services,
            Event::Select,
            &input,
            &mut output,
            Some(file_query("last")),
        )
        .unwrap();
    assert!(output.requests.iter().any(|request| matches!(request,
        HostRequest::Preview { path, content: PreviewContent::Text { text }, .. }
            if path == "last" && text == "last"
    )));
}

#[test]
fn settled_cached_files_do_not_start_queries_for_unchanged_selection_or_notifications() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let _observation = warm(&services, &file_query("first"));
    let mut browser = Browser::cloned();
    let input = input();
    browser
        .update_surface(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(file_query("first")),
        )
        .unwrap();
    let before = next_handle(&services);
    for event in [
        Event::Select,
        Event::Left,
        Event::Right,
        Event::Unknown,
        Event::FsChanged,
    ] {
        let mut output = output();
        browser
            .update_surface(
                &services,
                event,
                &input,
                &mut output,
                Some(file_query("first")),
            )
            .unwrap();
        assert!(
            !output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::Schedule { .. }))
        );
        assert!(!output.display.unwrap().loading);
    }
    assert_eq!(next_handle(&services), before + 1);
}

#[test]
fn invalidated_snapshots_are_refreshed_instead_of_republished_from_cache() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let _observation = warm(&services, &file_query("first"));
    let mut browser = Browser::cloned();
    let input = input();
    browser
        .update_surface(
            &services,
            Event::Preview,
            &input,
            &mut output(),
            Some(file_query("first")),
        )
        .unwrap();
    fs::write(fixture.0.join("first"), "updated").unwrap();
    services.borrow_mut().invalidate_fs();
    let before = next_handle(&services);
    let mut publication = output();
    browser
        .update_surface(
            &services,
            Event::FsChanged,
            &input,
            &mut publication,
            Some(file_query("first")),
        )
        .unwrap();
    assert_eq!(next_handle(&services), before + 2);
    assert!(!publication.requests.iter().any(|request| matches!(request,
        HostRequest::PreviewGroup { panes, .. }
            if matches!(&panes[..], [PreviewPane::Surface { content: PreviewContent::Text { text }, .. }] if text == "first")
    )));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !publication.requests.iter().any(|request| matches!(request,
        HostRequest::PreviewGroup { panes, .. }
            if matches!(&panes[..], [PreviewPane::Surface { content: PreviewContent::Text { text }, .. }] if text == "updated")
    )) {
        assert!(Instant::now() < deadline, "refreshed preview did not finish");
        browser.update_surface(&services, Event::Unknown, &input, &mut publication, Some(file_query("first"))).unwrap();
        std::thread::yield_now();
    }
}

#[test]
fn uncached_files_keep_the_previous_preview_until_the_last_navigation_delay_expires() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let _observation = warm(&services, &file_query("first"));
    let mut browser = Browser::cloned();
    let mut input = input();
    input.preview_debounce = Duration::from_millis(150);
    browser
        .update_surface(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(file_query("first")),
        )
        .unwrap();
    let before = next_handle(&services);
    let mut pending = output();
    browser
        .update_surface(
            &services,
            Event::Select,
            &input,
            &mut pending,
            Some(file_query("last")),
        )
        .unwrap();
    assert!(
        !pending
            .requests
            .iter()
            .any(|request| matches!(request, HostRequest::PreviewGroup { .. }))
    );
    assert!(pending.display.unwrap().loading);
    let now = Instant::now();
    for (event, path, elapsed) in [
        (Event::Select, "last", 0),
        (Event::Select, "cold", 20),
        (Event::Select, "cold", 40),
        (Event::FsChanged, "cold", 60),
        (Event::Unknown, "cold", 189),
    ] {
        let preview = browser
            .advance(
                &services,
                event,
                &input,
                &mut output(),
                Some(file_query(path)),
                now + Duration::from_millis(elapsed),
            )
            .unwrap();
        assert!(preview.is_none());
        assert!(matches!(&browser.content, Some(PreviewContent::Text { text }) if text == "first"));
    }
    assert!(
        matches!(browser.state, QueryState::Waiting { ready_at, .. } if ready_at == now + Duration::from_millis(190))
    );
    assert_eq!(next_handle(&services), before + 1);
    let mut preview = browser
        .advance(
            &services,
            Event::Unknown,
            &input,
            &mut output(),
            Some(file_query("cold")),
            now + Duration::from_millis(190),
        )
        .unwrap();
    assert_eq!(next_handle(&services), before + 3);

    let deadline = Instant::now() + Duration::from_secs(5);
    while preview.is_none() {
        assert!(Instant::now() < deadline, "preview query did not finish");
        preview = browser
            .advance(
                &services,
                Event::Unknown,
                &input,
                &mut output(),
                Some(file_query("cold")),
                now + Duration::from_millis(190),
            )
            .unwrap();
        std::thread::yield_now();
    }
    let preview = preview.unwrap();
    assert_eq!(preview.path, "cold");
    assert!(matches!(preview.content, PreviewContent::Text { text } if text == "cold"));
}

#[test]
fn navigation_cancels_an_unpublished_focus_request_at_the_same_selection() {
    let services = services();
    let input = input();
    let mut browser = Browser {
        state: QueryState::Idle {
            query: Some(query()),
        },
        content: Some(PreviewContent::Directory {
            rows: Vec::new(),
            column_alignment: Default::default(),
        }),
        content_query: Some(query()),
        focus_on_ready: true,
        ..Browser::cloned()
    };

    browser
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(query()),
            Instant::now(),
        )
        .unwrap();

    assert!(!browser.take_focus_request());
}

#[test]
fn refreshing_cached_content_keeps_the_focus_request_until_publication() {
    let services = services();
    let input = input();
    let now = Instant::now();
    let mut browser = Browser {
        state: QueryState::Waiting {
            query: query(),
            ready_at: now + input.preview_debounce,
        },
        content: Some(PreviewContent::Directory {
            rows: Vec::new(),
            column_alignment: Default::default(),
        }),
        content_query: Some(query()),
        ..Browser::cloned()
    };

    browser
        .advance(
            &services,
            Event::Preview,
            &input,
            &mut output(),
            Some(query()),
            now,
        )
        .unwrap();
    assert!(browser.focus_on_ready);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(browser.state, QueryState::Idle { .. }) {
        assert!(Instant::now() < deadline, "preview query did not finish");
        browser
            .advance(
                &services,
                Event::Unknown,
                &input,
                &mut output(),
                Some(query()),
                now,
            )
            .unwrap();
        std::thread::yield_now();
    }

    assert!(browser.focus_on_ready);
    assert!(browser.take_focus_request());
    assert!(!browser.take_focus_request());
}

#[test]
fn completed_source_changes_refresh_the_settled_directory_preview() {
    let services = services();
    let input = input();
    let mut browser = Browser::cloned();
    for (event, changed, text) in [
        (Event::Preview, false, "before"),
        (Event::Unknown, true, "after"),
    ] {
        let mut output = output();
        output.changed = changed;
        browser
            .show_content(&services, event, &input, &mut output, "directory", || {
                PreviewContent::Directory {
                    rows: vec![DisplayRow {
                        id: "child".into(),
                        text: text.into(),
                        ..DisplayRow::default()
                    }],
                    column_alignment: Default::default(),
                }
            })
            .unwrap();
        assert!(output.requests.iter().any(|request| matches!(request,
            HostRequest::Preview { content: PreviewContent::Directory { rows, .. }, .. }
            if rows[0].text == text
        )));
    }
}

#[test]
fn loaded_directories_publish_immediately_when_the_selection_changes() {
    let services = services();
    let input = input();
    let mut browser = Browser::cloned();
    let mut builds = 0;
    let mut publications = Vec::new();
    for (event, path) in [
        (Event::Select, "first"),
        (Event::Select, "last"),
        (Event::Select, "last"),
        (Event::Unknown, "last"),
    ] {
        let mut output = output();
        browser
            .show_content(&services, event, &input, &mut output, path, || {
                builds += 1;
                PreviewContent::Directory {
                    rows: Vec::new(),
                    column_alignment: Default::default(),
                }
            })
            .unwrap();
        assert!(!output.display.as_ref().unwrap().loading);
        for request in output.requests {
            match request {
                HostRequest::Preview { path, .. } => publications.push(path),
                HostRequest::Schedule { .. } => panic!("loaded directory was delayed"),
                _ => {}
            }
        }
    }

    assert_eq!(builds, 2);
    assert_eq!(publications, ["first", "last"]);
}

#[test]
fn repeated_steps_at_the_same_item_cancel_the_unpublished_preview() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let input = input();
    let task = Task::start(&services, file_query("first").request).unwrap();
    let cancelled = task.handle.unwrap();
    let mut browser = Browser {
        state: QueryState::Running {
            query: file_query("first"),
            task,
        },
        content: Some(PreviewContent::Text {
            text: "previous".into(),
        }),
        ..Browser::cloned()
    };
    let now = Instant::now();
    browser
        .navigation
        .update(Event::Select, &input, now)
        .unwrap();
    let before = next_handle(&services);
    for elapsed in [10, 20, 79] {
        let publication = browser
            .advance(
                &services,
                Event::Select,
                &input,
                &mut output(),
                Some(file_query("first")),
                now + Duration::from_secs(elapsed),
            )
            .unwrap();
        assert!(publication.is_none());
        assert!(matches!(browser.state, QueryState::Waiting { ready_at, .. }
            if ready_at == now + Duration::from_secs(elapsed) + input.preview_debounce));
        assert!(
            matches!(&browser.content, Some(PreviewContent::Text { text }) if text == "previous")
        );
    }
    assert!(services.borrow_mut().poll_query(cancelled).is_err());
    assert_eq!(next_handle(&services), before + 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    let publication = loop {
        if let Some(publication) = browser
            .advance(
                &services,
                Event::Unknown,
                &input,
                &mut output(),
                Some(file_query("first")),
                now + Duration::from_secs(139),
            )
            .unwrap()
        {
            break publication;
        }
        assert!(Instant::now() < deadline, "settled preview did not finish");
        std::thread::yield_now();
    };
    assert_eq!(publication.path, "first");
    assert!(matches!(publication.content, PreviewContent::Text { text } if text == "first"));
    assert_eq!(next_handle(&services), before + 3);
}

#[test]
fn a_single_step_starts_immediately_without_a_duplicate_after_the_quiet_period() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let mut browser = Browser::cloned();
    let mut input = input();
    input.argument = serde_json::to_value(SelectionChange {
        navigation: Some(ItemNavigation::Step),
    })
    .unwrap();
    let now = Instant::now();
    let before = next_handle(&services);
    browser
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(file_query("first")),
            now,
        )
        .unwrap();
    assert!(!matches!(browser.state, QueryState::Waiting { .. }));
    assert_eq!(next_handle(&services), before + 2);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        browser
            .advance(
                &services,
                Event::Unknown,
                &input,
                &mut output(),
                Some(file_query("first")),
                now + input.preview_debounce,
            )
            .unwrap();
        if matches!(browser.state, QueryState::Idle { .. }) {
            break;
        }
        assert!(Instant::now() < deadline, "preview query did not finish");
        std::thread::yield_now();
    }
    assert!(matches!(&browser.content, Some(PreviewContent::Text { text }) if text == "first"));
    assert_eq!(next_handle(&services), before + 3);
}

#[test]
fn jumping_starts_the_selected_preview_without_waiting() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let mut browser = Browser::cloned();
    let mut input = input();
    input.argument = serde_json::to_value(SelectionChange {
        navigation: Some(ItemNavigation::Jump),
    })
    .unwrap();
    let mut output = output();
    browser
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output,
            Some(file_query("cold")),
            Instant::now(),
        )
        .unwrap();

    assert!(matches!(
        browser.state,
        QueryState::Running { .. } | QueryState::Idle { .. }
    ));
    assert_eq!(browser.state.query(), Some(&file_query("cold")));
}

#[test]
fn changing_only_the_selection_preserves_pending_preview_work() {
    let services = services();
    let mut input = input();
    input.argument = serde_json::to_value(SelectionChange { navigation: None }).unwrap();
    let deadline = Instant::now() + input.preview_debounce;
    let mut browser = Browser {
        state: QueryState::Waiting {
            query: query(),
            ready_at: deadline,
        },
        ..Browser::cloned()
    };
    browser
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(query()),
            Instant::now(),
        )
        .unwrap();
    assert!(matches!(browser.state, QueryState::Waiting { ready_at, .. } if ready_at == deadline));

    browser.state = QueryState::Running {
        query: query(),
        task: Task::start(&services, query().request).unwrap(),
    };
    browser
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(query()),
            Instant::now(),
        )
        .unwrap();
    assert!(!matches!(browser.state, QueryState::Waiting { .. }));
}

#[test]
fn navigation_input_restarts_the_preview_delay_even_when_the_query_is_unchanged() {
    let services = services();
    let input = input();
    let mut browser = Browser {
        content: Some(PreviewContent::Text {
            text: "previous preview".into(),
        }),
        initialized: true,
        ..Browser::default()
    };
    let now = Instant::now();
    for event in [
        Event::Select,
        Event::Left,
        Event::Right,
        Event::FilterChanged,
        Event::MiniBufferChanged,
    ] {
        browser
            .navigation
            .update(Event::Select, &input, now)
            .unwrap();
        browser.state = QueryState::Waiting {
            query: query(),
            ready_at: now,
        };
        let mut output = output();
        let preview = browser
            .advance(&services, event, &input, &mut output, Some(query()), now)
            .unwrap();

        assert!(matches!(
            browser.state,
            QueryState::Waiting { ready_at, .. } if ready_at == now + input.preview_debounce
        ));
        assert!(matches!(
            browser.content.as_ref(),
            Some(PreviewContent::Text { text }) if text == "previous preview"
        ));
        assert!(preview.is_none());
    }
}

#[test]
fn background_ticks_preserve_the_preview_deadline() {
    let services = services();
    let input = input();
    let now = Instant::now();
    let deadline = now + Duration::from_secs(30);
    let mut browser = Browser {
        state: QueryState::Waiting {
            query: query(),
            ready_at: deadline,
        },
        ..Browser::cloned()
    };
    for elapsed in [0, 1, 29] {
        let mut output = output();
        browser
            .advance(
                &services,
                Event::parse("tick"),
                &input,
                &mut output,
                Some(query()),
                now + Duration::from_secs(elapsed),
            )
            .unwrap();

        assert!(matches!(
            browser.state,
            QueryState::Waiting { ready_at, .. } if ready_at == deadline
        ));
    }
}

#[test]
fn a_background_tick_starts_the_preview_when_the_delay_expires() {
    let services = services();
    let input = input();
    let now = Instant::now();
    let mut browser = Browser {
        state: QueryState::Waiting {
            query: query(),
            ready_at: now,
        },
        ..Browser::cloned()
    };
    browser
        .advance(
            &services,
            Event::parse("tick"),
            &input,
            &mut output(),
            Some(query()),
            now,
        )
        .unwrap();

    assert!(!matches!(browser.state, QueryState::Waiting { .. }));
    assert_eq!(browser.state.query(), Some(&query()));
    assert!(matches!(browser.state, QueryState::Running { .. }) || browser.content.is_some());
}

#[test]
fn an_explicit_preview_starts_before_the_delay_expires() {
    let services = services();
    let input = input();
    let now = Instant::now();
    let mut browser = Browser {
        state: QueryState::Waiting {
            query: query(),
            ready_at: now + input.preview_debounce,
        },
        ..Browser::cloned()
    };
    let preview = browser
        .advance(
            &services,
            Event::Preview,
            &input,
            &mut output(),
            Some(query()),
            now,
        )
        .unwrap();

    assert!(!matches!(browser.state, QueryState::Waiting { .. }));
    assert_eq!(browser.state.query(), Some(&query()));
    assert!(browser.focus_on_ready || preview.is_some_and(|preview| preview.focus));
}

#[test]
fn filesystem_changes_preserve_the_navigation_delay() {
    let services = services();
    let input = input();
    let now = Instant::now();
    let deadline = now + input.preview_debounce;
    let mut browser = Browser {
        state: QueryState::Waiting {
            query: query(),
            ready_at: deadline,
        },
        ..Browser::cloned()
    };
    browser
        .advance(
            &services,
            Event::FsChanged,
            &input,
            &mut output(),
            Some(query()),
            now,
        )
        .unwrap();

    assert!(matches!(
        browser.state,
        QueryState::Waiting { ready_at, .. } if ready_at == deadline
    ));
}

#[test]
fn filesystem_changes_reuse_the_running_query() {
    let services = services();
    let input = input();
    let task = Task::start(&services, query().request).unwrap();
    let mut browser = Browser {
        state: QueryState::Running {
            query: query(),
            task,
        },
        ..Browser::cloned()
    };
    let before = services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null)["next_handle"]
        .as_u64()
        .unwrap();
    browser
        .advance(
            &services,
            Event::FsChanged,
            &input,
            &mut output(),
            Some(query()),
            Instant::now(),
        )
        .unwrap();
    let after = services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null)["next_handle"]
        .as_u64()
        .unwrap();

    // Each stats call consumes one handle at the dynamic service boundary.
    assert_eq!(after, before + 1);
    assert_eq!(browser.state.query(), Some(&query()));
}

#[test]
fn changing_the_query_cancels_old_work_and_keeps_the_previous_content() {
    let services = services();
    let input = input();
    let task = Task::start(&services, query().request).unwrap();
    let handle = task.handle.unwrap();
    let mut browser = Browser {
        state: QueryState::Running {
            query: query(),
            task,
        },
        content: Some(PreviewContent::Text {
            text: "previous preview".into(),
        }),
        content_query: Some(query()),
        focus_on_ready: true,
        ..Browser::cloned()
    };
    let next = PreviewQuery {
        request: QueryRequest::Preview {
            path: String::new(),
            show_ignored: false,
        },
        path: String::new(),
    };
    let now = Instant::now();
    browser
        .navigation
        .update(Event::Select, &input, now)
        .unwrap();
    let preview = browser
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(next.clone()),
            now,
        )
        .unwrap();

    assert!(services.borrow_mut().poll_query(handle).is_err());
    assert!(matches!(
        browser.state,
        QueryState::Waiting { ready_at, .. } if ready_at == now + input.preview_debounce
    ));
    assert_eq!(browser.state.query(), Some(&next));
    assert!(matches!(
        browser.content.as_ref(),
        Some(PreviewContent::Text { text }) if text == "previous preview"
    ));
    assert_eq!(browser.content_query, Some(query()));
    assert!(!browser.focus_on_ready);
    assert!(preview.is_none());
}

#[test]
fn leaving_the_view_cancels_running_preview_work() {
    let services = services();
    let input = input();
    let task = Task::start(&services, query().request).unwrap();
    let handle = task.handle.unwrap();
    let mut browser = Browser {
        state: QueryState::Running {
            query: query(),
            task,
        },
        focus_on_ready: true,
        ..Browser::cloned()
    };
    let mut output = output();
    browser
        .advance(
            &services,
            Event::ViewLeaving,
            &input,
            &mut output,
            Some(query()),
            Instant::now(),
        )
        .unwrap();

    assert!(services.borrow_mut().poll_query(handle).is_err());
    assert!(matches!(browser.state, QueryState::Idle { .. }));
    assert!(!browser.focus_on_ready);
    assert!(
        !output
            .requests
            .iter()
            .any(|request| matches!(request, HostRequest::Schedule { .. }))
    );
}

#[test]
fn separate_previews_keep_independent_navigation_delays() {
    let services = services();
    let input = input();
    let mut first = Browser::cloned();
    let mut second = Browser::cloned();
    let now = Instant::now();
    for browser in [&mut first, &mut second] {
        browser
            .advance(
                &services,
                Event::Select,
                &input,
                &mut output(),
                Some(query()),
                now,
            )
            .unwrap();
    }
    for browser in [&mut first, &mut second] {
        browser
            .advance(
                &services,
                Event::Select,
                &input,
                &mut output(),
                Some(file_query("last")),
                now + Duration::from_secs(1),
            )
            .unwrap();
    }
    first
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(file_query("last")),
            now + Duration::from_secs(10),
        )
        .unwrap();

    assert!(matches!(
        first.state,
        QueryState::Waiting { ready_at, .. } if ready_at == now + Duration::from_secs(70)
    ));
    assert!(matches!(
        second.state,
        QueryState::Waiting { ready_at, .. } if ready_at == now + Duration::from_secs(61)
    ));
}

#[test]
fn batch_diff_notifications_match_the_complete_request() {
    use zess_git::{DiffSide, DiffSource, DiffTarget};
    let services = services();
    let mut input = input();
    let query = PreviewQuery {
        request: QueryRequest::DiffFiles {
            source: DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            files: vec![DiffTarget {
                path: "first.txt".into(),
                original_path: None,
            }],
            render: zess_git::DiffRenderOptions::default(),
        },
        path: "directory".into(),
    };
    let now = Instant::now();
    let mut browser = Browser {
        state: QueryState::Idle {
            query: Some(query.clone()),
        },
        ..Browser::cloned()
    };
    input.argument = serde_json::json!({"queries":[{
        "operation":"git.diff-files",
        "argument":{"source":{"kind":"changes", "side":"staged"}, "files":[{"path":"first.txt", "original_path":null}]}
    }]});
    browser
        .advance(
            &services,
            Event::FsChanged,
            &input,
            &mut output(),
            Some(query.clone()),
            now,
        )
        .unwrap();
    assert!(matches!(browser.state, QueryState::Idle { .. }));
    let before = next_handle(&services);
    input.argument = serde_json::json!({"queries":[{"operation":"git.diff-files", "argument":query.request.argument()}]});
    browser
        .advance(
            &services,
            Event::FsChanged,
            &input,
            &mut output(),
            Some(query.clone()),
            now,
        )
        .unwrap();
    assert_eq!(next_handle(&services), before + 2);
    let deadline = now + input.preview_debounce;
    browser.state = QueryState::Waiting {
        query: query.clone(),
        ready_at: deadline,
    };
    browser
        .advance(
            &services,
            Event::FsChanged,
            &input,
            &mut output(),
            Some(query),
            now,
        )
        .unwrap();
    assert!(matches!(browser.state, QueryState::Waiting { ready_at, .. } if ready_at == deadline));
}

#[test]
fn zero_delay_starts_each_selected_item_immediately() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let mut browser = Browser::cloned();
    let mut input = input();
    input.preview_debounce = Duration::ZERO;
    let now = Instant::now();
    let before = next_handle(&services);
    for path in ["first", "last", "cold"] {
        browser
            .advance(
                &services,
                Event::Select,
                &input,
                &mut output(),
                Some(file_query(path)),
                now,
            )
            .unwrap();
        assert!(!matches!(browser.state, QueryState::Waiting { .. }));
        assert_eq!(browser.state.query(), Some(&file_query(path)));
    }
    assert_eq!(next_handle(&services), before + 4);
}

#[test]
fn returning_to_a_view_starts_a_new_navigation_round() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let mut browser = Browser::cloned();
    let input = input();
    let now = Instant::now();
    for (event, path) in [
        (Event::Select, "first"),
        (Event::Select, "last"),
        (Event::ViewLeaving, "last"),
        (Event::Select, "cold"),
    ] {
        browser
            .advance(
                &services,
                event,
                &input,
                &mut output(),
                Some(file_query(path)),
                now,
            )
            .unwrap();
    }
    assert!(!matches!(browser.state, QueryState::Waiting { .. }));
    assert_eq!(browser.state.query(), Some(&file_query("cold")));
}

#[test]
fn filesystem_refreshes_of_the_leading_item_wait_for_navigation_to_settle() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let _observation = warm(&services, &file_query("first"));
    let mut browser = Browser::cloned();
    let input = input();
    let now = Instant::now();
    browser
        .advance(
            &services,
            Event::Select,
            &input,
            &mut output(),
            Some(file_query("first")),
            now,
        )
        .unwrap();
    assert!(matches!(browser.state, QueryState::Idle { .. }));
    fs::write(fixture.0.join("first"), "updated").unwrap();
    services.borrow_mut().invalidate_fs();
    let before = next_handle(&services);
    let update = browser
        .advance(
            &services,
            Event::FsChanged,
            &input,
            &mut output(),
            Some(file_query("first")),
            now + Duration::from_secs(1),
        )
        .unwrap();
    assert!(update.is_none());
    assert!(matches!(browser.state, QueryState::Waiting { ready_at, .. }
        if ready_at == now + input.preview_debounce));
    assert_eq!(next_handle(&services), before + 1);
    assert!(matches!(&browser.content, Some(PreviewContent::Text { text }) if text == "first"));
}

#[test]
fn returning_to_a_cancelled_item_publishes_only_its_final_request() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let mut input = input();
    input.preview_debounce = Duration::from_millis(150);
    let task = Task::start(&services, file_query("first").request).unwrap();
    let cancelled = task.handle.unwrap();
    let mut browser = Browser {
        state: QueryState::Running {
            query: file_query("first"),
            task,
        },
        content: Some(PreviewContent::Text {
            text: "previous".into(),
        }),
        ..Browser::cloned()
    };
    let now = Instant::now();
    browser
        .navigation
        .update(Event::Select, &input, now)
        .unwrap();
    let before = next_handle(&services);
    for (path, elapsed) in [("last", 10), ("first", 20)] {
        let publication = browser
            .advance(
                &services,
                Event::Select,
                &input,
                &mut output(),
                Some(file_query(path)),
                now + Duration::from_millis(elapsed),
            )
            .unwrap();
        assert!(publication.is_none());
        assert!(
            matches!(&browser.content, Some(PreviewContent::Text { text }) if text == "previous")
        );
    }
    assert!(services.borrow_mut().poll_query(cancelled).is_err());
    assert_eq!(next_handle(&services), before + 1);
    assert!(matches!(browser.state, QueryState::Waiting { ready_at, .. }
        if ready_at == now + Duration::from_millis(170)));
    let deadline = Instant::now() + Duration::from_secs(5);
    let publication = loop {
        if let Some(publication) = browser
            .advance(
                &services,
                Event::Unknown,
                &input,
                &mut output(),
                Some(file_query("first")),
                now + Duration::from_millis(170),
            )
            .unwrap()
        {
            break publication;
        }
        assert!(Instant::now() < deadline, "final preview did not finish");
        std::thread::yield_now();
    };
    assert_eq!(publication.path, "first");
    assert!(matches!(publication.content, PreviewContent::Text { text } if text == "first"));
    assert_eq!(next_handle(&services), before + 3);
}
