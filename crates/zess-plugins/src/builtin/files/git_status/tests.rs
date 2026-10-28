use super::*;
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use zess_plugin::{BufferPlugin, ItemSelection, PluginContext};
use zess_services::NativeServices;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zess-files-git-status-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn plugin(&self) -> (super::super::Files, PluginInput) {
        let services = Rc::new(RefCell::new(NativeServices::new(self.0.clone()).unwrap()));
        (
            super::super::Files::new(services),
            PluginInput {
                buffer_id: 1,
                view_id: 1,
                generation: 1,
                preview_debounce: Duration::ZERO,
                preview_area: None,
                event: "render".into(),
                argument: serde_json::Value::Null,
                query: None,
                options: serde_json::Value::Null,
                mode: "normal".into(),
                selection: ItemSelection::default(),
                register: None,
                context: PluginContext::default(),
            },
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn git(path: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .env("GIT_AUTHOR_NAME", "Zess Test")
        .env("GIT_AUTHOR_EMAIL", "zess@example.invalid")
        .env("GIT_COMMITTER_NAME", "Zess Test")
        .env("GIT_COMMITTER_EMAIL", "zess@example.invalid")
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn settle(plugin: &mut super::super::Files, input: &PluginInput) -> PluginOutput {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = plugin.update(input).unwrap();
        assert!(
            !output
                .requests
                .iter()
                .any(|request| matches!(request, zess_plugin::HostRequest::Message { .. })),
            "{}",
            serde_json::to_string(&output).unwrap()
        );
        if !output.display.as_ref().unwrap().loading {
            return output;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn changed_paths_decorate_existing_files_and_their_directories() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join("folder/note.txt"), "before\n").unwrap();
    fs::write(fixture.0.join("deleted.txt"), "before\n").unwrap();
    fs::write(fixture.0.join("replaced"), "before\n").unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("folder/note.txt"), "after\n").unwrap();
    fs::remove_file(fixture.0.join("deleted.txt")).unwrap();
    fs::remove_file(fixture.0.join("replaced")).unwrap();
    fs::create_dir(fixture.0.join("replaced")).unwrap();
    fs::write(fixture.0.join("replaced/new.txt"), "new\n").unwrap();
    fs::write(fixture.0.join("new.txt"), "new\n").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    input.event = "navigate".into();
    input.argument = serde_json::json!({"path":"folder/note.txt"});
    plugin.update(&input).unwrap();
    input.event = "render".into();
    let output = settle(&mut plugin, &input);
    let rows = output.display.unwrap().rows;
    assert!(!rows.iter().any(|row| row.id == "deleted.txt"));
    for (path, label, status, style) in [
        ("folder", "▾ folder", "M", "modified"),
        ("folder/note.txt", "  note.txt", "M", "modified"),
        ("replaced", "▸ replaced", "M", "modified"),
    ] {
        let row = rows.iter().find(|row| row.id == path).unwrap();
        assert_eq!(row.text, label);
        assert_eq!(row.style, style);
        assert_eq!(row.cells.len(), 1);
        assert_eq!(row.cells[0].spans[0].text, status);
    }
    let untracked = rows.iter().find(|row| row.id == "new.txt").unwrap();
    assert_eq!(untracked.text, "  new.txt");
    assert_eq!(untracked.style, "git.added");
    assert!(untracked.cells.is_empty());
    assert!(
        output
            .watches
            .unwrap()
            .iter()
            .any(|watch| watch.operation == "git.decorations")
    );
}

#[test]
fn disabling_or_leaving_cancels_pending_decorations() {
    let fixture = Fixture::new();
    let (plugin, input) = fixture.plugin();
    let mut status = GitStatus::default();
    for event in [Event::Unknown, Event::ViewLeaving] {
        status.state =
            State::Loading(Task::start(&plugin.services, QueryRequest::GitDecorations {}).unwrap());
        let mut output = PluginOutput::default();
        status
            .update(
                &plugin.services,
                event,
                &input,
                event == Event::ViewLeaving,
                &mut output,
            )
            .unwrap();
        assert!(matches!(status.state, State::Unloaded));
        assert!(output.watches.unwrap_or_default().is_empty());
        assert_eq!(
            plugin
                .services
                .borrow_mut()
                .call("service.stats", serde_json::Value::Null)["jobs"],
            0
        );
    }
}

#[test]
fn filesystem_refreshes_do_not_restart_decoration_queries() {
    let fixture = Fixture::new();
    let (plugin, mut input) = fixture.plugin();
    let mut status = GitStatus {
        state: State::Ready,
        ..GitStatus::default()
    };
    input.argument =
        serde_json::json!({"queries":[{"operation":"fs.list","argument":{"path":""}}]});
    status
        .update(
            &plugin.services,
            Event::FsChanged,
            &input,
            true,
            &mut PluginOutput::default(),
        )
        .unwrap();
    assert!(matches!(status.state, State::Ready));
    input.argument = serde_json::json!({"queries":[{"operation":"git.decorations","argument":{}}]});
    status
        .update(
            &plugin.services,
            Event::FsChanged,
            &input,
            true,
            &mut PluginOutput::default(),
        )
        .unwrap();
    assert!(matches!(status.state, State::Loading(_)));
}

#[test]
fn disabling_decorations_restores_plain_file_rows() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "new\n").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    let rows = settle(&mut plugin, &input).display.unwrap().rows;
    let row = rows.iter().find(|row| row.id == "note.txt").unwrap();
    assert!(row.cells.is_empty());
    assert_eq!(row.style, "git.added");
    input.options = serde_json::json!({"git-status":false});
    let output = plugin.update(&input).unwrap();
    let rows = output.display.unwrap().rows;
    let row = rows.iter().find(|row| row.id == "note.txt").unwrap();
    assert!(row.cells.is_empty());
    assert!(row.style.is_empty());
    assert!(
        output
            .watches
            .unwrap()
            .iter()
            .all(|watch| watch.operation != "git.decorations")
    );
    input.options = serde_json::json!({"git-status":"false"});
    assert_eq!(
        plugin.update(&input).err().unwrap().to_string(),
        "git-status must be a boolean"
    );
}

#[test]
fn non_git_directories_keep_their_plain_file_rows() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "plain\n").unwrap();
    let (mut plugin, input) = fixture.plugin();
    let rows = settle(&mut plugin, &input).display.unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, "  note.txt");
    assert!(rows[0].cells.is_empty());
}

#[test]
fn broken_repositories_report_the_error_without_hiding_files() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join(".git"), "gitdir: missing\n").unwrap();
    fs::write(fixture.0.join("note.txt"), "plain\n").unwrap();
    let (mut plugin, input) = fixture.plugin();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut error = None;
    let mut visible = false;
    loop {
        let output = plugin.update(&input).unwrap();
        for request in output.requests {
            if let zess_plugin::HostRequest::Message { text } = request {
                error = Some(text);
            }
        }
        visible |= output
            .display
            .unwrap()
            .rows
            .iter()
            .any(|row| row.id == "note.txt");
        if error.is_some() && visible {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(error.unwrap().starts_with("Git status:"));
    assert!(matches!(
        plugin.views[&(1, 1)].git_status.state,
        State::Ready
    ));
    let before = plugin
        .services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null)["next_handle"]
        .as_u64()
        .unwrap();
    let output = plugin.update(&input).unwrap();
    assert!(
        !output
            .requests
            .iter()
            .any(|request| matches!(request, zess_plugin::HostRequest::Message { .. }))
    );
    let after = plugin
        .services
        .borrow_mut()
        .call("service.stats", serde_json::Value::Null)["next_handle"]
        .as_u64()
        .unwrap();
    assert_eq!(after, before + 1);
}
