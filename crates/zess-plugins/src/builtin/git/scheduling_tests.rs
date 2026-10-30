use super::*;
use std::{cell::RefCell, rc::Rc, time::Duration};
use zess_plugin::{ItemSelection, PluginContext};
use zess_services::NativeServices;

pub(super) fn input() -> PluginInput {
    PluginInput {
        buffer_id: 1,
        view_id: 1,
        generation: 1,
        preview_debounce: Duration::from_secs(60),
        preview_area: None,
        event: "fs_changed".into(),
        argument: serde_json::Value::Null,
        query: None,
        options: serde_json::Value::Null,
        mode: "normal".into(),
        selection: ItemSelection::default(),
        register: None,
        context: PluginContext::default(),
    }
}

fn services() -> Services {
    Rc::new(RefCell::new(
        NativeServices::new(std::env::current_dir().unwrap()).unwrap(),
    ))
}

fn next_handle(services: &Services) -> u64 {
    services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null)["next_handle"]
        .as_u64()
        .unwrap()
}

#[test]
fn filesystem_notifications_reuse_the_running_file_list_query() {
    let services = services();
    let mut git = Git::new(services.clone(), Page::Changes);
    git.models.insert(
        1,
        Model {
            result: LoadResult::Ready,
            ..Model::default()
        },
    );
    git.views.insert(
        (1, 1),
        GitView {
            task: Some(Task::start(&services, QueryRequest::Status {}).unwrap()),
            ..GitView::default()
        },
    );
    let before = next_handle(&services);

    git.update(&input()).unwrap();

    assert_eq!(next_handle(&services), before + 1);
}

#[test]
fn a_failed_file_list_keeps_its_error_visible_across_navigation() {
    let mut git = Git::new(services(), Page::CommitFiles);
    let mut input = input();
    input.event = "init".into();
    input.argument = json!({"commit": "not-a-commit"});
    git.update(&input).unwrap();
    input.event = "tick".into();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(git.models[&1].result, LoadResult::Failed(_)) {
        assert!(Instant::now() < deadline, "commit query did not finish");
        git.update(&input).unwrap();
        std::thread::yield_now();
    }
    let LoadResult::Failed(error) = &git.models[&1].result else {
        unreachable!();
    };
    let error = error.clone();
    assert!(error.starts_with("git.commit-files:"));
    for event in ["select", "tick", "render"] {
        input.event = event.into();
        let output = git.update(&input).unwrap();
        let display = output.display.unwrap();
        assert!(display.header.ends_with(&error));
        assert_eq!(display.status["error"], error);
        assert!(!display.loading);
        assert!(
            !output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::Message { .. }))
        );
    }
}
