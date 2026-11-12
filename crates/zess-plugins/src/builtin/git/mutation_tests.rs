use super::scheduling_tests::input;
use super::*;
use std::{cell::RefCell, fs, process::Command, rc::Rc, time::Duration};
use zess_services::NativeServices;

struct Workspace(std::path::PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zess-git-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let workspace = Self(path);
        workspace.git(&["init", "--quiet"]);
        workspace
    }
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Tests",
                "-c",
                "user.email=tests@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn plugin(&self) -> Git {
        Git::new(
            Rc::new(RefCell::new(NativeServices::new(self.0.clone()).unwrap())),
            Page::Changes,
        )
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn ready(git: &mut Git, input: &mut PluginInput) -> PluginOutput {
    input.event = "tick".into();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = git.update(input).unwrap();
        if matches!(git.models[&1].mutation, MutationState::Idle { .. })
            && git.views[&(1, 1)].load.is_none()
        {
            return output;
        }
        assert!(Instant::now() < deadline, "Git operation did not complete");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn initialize(git: &mut Git) -> PluginInput {
    let mut input = input();
    input.event = "init".into();
    git.update(&input).unwrap();
    ready(git, &mut input);
    input
}

#[cfg(target_os = "linux")]
#[test]
fn changes_with_non_utf8_paths_keep_other_files_actionable() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let workspace = Workspace::new();
    workspace.git(&["config", "status.renames", "true"]);
    let raw_source = workspace.0.join(OsStr::from_bytes(b"raw-source-\xff"));
    let raw_target = workspace.0.join(OsStr::from_bytes(b"raw-target-\xff"));
    for (path, text) in [
        (&raw_source, "rename with a raw source\n"),
        (
            &workspace.0.join("visible-source"),
            "rename with a raw destination\n",
        ),
        (
            &workspace.0.join("good-source"),
            "rename with valid paths\n",
        ),
        (&workspace.0.join("modified"), "before\n"),
    ] {
        fs::write(path, text).unwrap();
    }
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::rename(raw_source, workspace.0.join("visible-target")).unwrap();
    fs::rename(workspace.0.join("visible-source"), raw_target).unwrap();
    fs::rename(
        workspace.0.join("good-source"),
        workspace.0.join("good-target"),
    )
    .unwrap();
    workspace.git(&["add", "-A"]);
    fs::write(workspace.0.join("modified"), "before\nafter\n").unwrap();
    fs::write(workspace.0.join(OsStr::from_bytes(b"note-\xff")), "raw\n").unwrap();
    fs::write(workspace.0.join("note-\u{fffd}"), "unicode\n").unwrap();

    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    let display = ready(&mut git, &mut input).display.unwrap();
    assert_eq!(display.header, "Skipped 3 Git entries with non-UTF-8 paths");
    assert_eq!(
        display
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["file:good-target", "file:modified", "file:note-\u{fffd}"]
    );
    assert!(display.rows[0].text.contains("good-source"));
    assert!(
        display.rows[1]
            .cells
            .iter()
            .any(|cell| { cell.spans.iter().any(|span| span.text == "+1") })
    );

    input.selection = browser::single(Some("file:note-\u{fffd}".into()));
    input.event = "stage".into();
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    assert_eq!(workspace.git(&["show", ":note-\u{fffd}"]), "unicode\n");
    assert_eq!(
        workspace.git(&[
            "-c",
            "core.quotepath=true",
            "ls-files",
            "--others",
            "--exclude-standard"
        ]),
        "\"note-\\377\"\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn commit_files_with_non_utf8_paths_keep_other_files_and_counts() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let workspace = Workspace::new();
    let raw_source = workspace.0.join(OsStr::from_bytes(b"raw-source-\xff"));
    fs::write(&raw_source, "rename from raw source\n").unwrap();
    fs::write(workspace.0.join("modified"), "before\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::rename(raw_source, workspace.0.join("visible-target")).unwrap();
    fs::write(workspace.0.join("modified"), "before\nafter\n").unwrap();
    fs::write(
        workspace.0.join(OsStr::from_bytes(b"raw-file-\xff")),
        "new\n",
    )
    .unwrap();
    workspace.git(&["add", "-A"]);
    workspace.git(&["commit", "--quiet", "-m", "modify and rename"]);

    let mut git = Git::new(workspace.plugin().services, Page::CommitFiles);
    let mut input = input();
    input.event = "init".into();
    let oid = workspace.git(&["rev-parse", "HEAD"]);
    input.argument = json!({"commit": oid.trim()});
    git.update(&input).unwrap();
    let display = ready(&mut git, &mut input).display.unwrap();
    assert!(
        display
            .header
            .ends_with("Skipped 2 Git entries with non-UTF-8 paths")
    );
    assert_eq!(display.rows.len(), 1);
    assert_eq!(display.rows[0].id, "file:modified");
    assert!(
        display.rows[0]
            .cells
            .iter()
            .any(|cell| { cell.spans.iter().any(|span| span.text == "+1") })
    );
}

#[test]
fn directory_operations_include_changes_hidden_by_the_filter() {
    let workspace = Workspace::new();
    fs::create_dir(workspace.0.join("dir")).unwrap();
    for path in ["dir/visible.txt", "dir/hidden.txt", "outside.txt"] {
        fs::write(workspace.0.join(path), path).unwrap();
    }
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.event = "filter_changed".into();
    input.query = Some(zess_filter::Query {
        text: "visible".into(),
        ..zess_filter::Query::default()
    });
    git.update(&input).unwrap();
    input.selection = browser::single(Some("directory:dir".into()));
    input.event = "stage".into();
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    assert_eq!(
        workspace.git(&["diff", "--cached", "--name-only"]),
        "dir/hidden.txt\ndir/visible.txt\n"
    );
    input.event = "unstage".into();
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    assert_eq!(workspace.git(&["diff", "--cached", "--name-only"]), "");
    assert_eq!(
        fs::read_to_string(workspace.0.join("dir/hidden.txt")).unwrap(),
        "dir/hidden.txt"
    );
    assert_eq!(
        fs::read_to_string(workspace.0.join("outside.txt")).unwrap(),
        "outside.txt"
    );
}

#[test]
fn discard_requires_confirmation_and_preserves_the_staged_version() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "head\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(workspace.0.join("file"), "index\n").unwrap();
    workspace.git(&["add", "."]);
    fs::write(workspace.0.join("file"), "worktree\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.selection = browser::single(Some("file:file".into()));
    input.event = "discard".into();
    let output = git.update(&input).unwrap();
    let purpose = output
        .requests
        .iter()
        .find_map(|request| match request {
            HostRequest::MiniBuffer { purpose, text, .. } => {
                assert!(text.is_empty());
                Some(purpose.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        purpose,
        "Restore working tree from index: \"file\". Type yes to confirm"
    );
    input.event = "layer_result".into();
    input.argument = json!({"purpose":purpose,"text":"yes","cancelled":true});
    git.update(&input).unwrap();
    assert!(matches!(
        git.models[&1].mutation,
        MutationState::Idle { .. }
    ));
    assert_eq!(
        fs::read_to_string(workspace.0.join("file")).unwrap(),
        "worktree\n"
    );
    input.event = "discard".into();
    git.update(&input).unwrap();
    input.event = "layer_result".into();
    input.argument = json!({"purpose":purpose,"text":"yes","cancelled":false});
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    assert_eq!(
        fs::read_to_string(workspace.0.join("file")).unwrap(),
        "index\n"
    );
    assert_eq!(workspace.git(&["show", ":file"]), "index\n");
    assert_eq!(git.models[&1].entries[0].worktree, " ");
}

#[test]
fn directory_selection_includes_collapsed_children_once_and_warns_before_deleting_untracked_files()
{
    let workspace = Workspace::new();
    fs::create_dir_all(workspace.0.join("dir/nested")).unwrap();
    fs::write(workspace.0.join("dir/nested/new"), "new\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    git.views
        .get_mut(&(1, 1))
        .unwrap()
        .tree
        .collapsed
        .insert("dir".into());
    input.selection.primary = Some("directory:dir".into());
    input.selection.selected = vec!["directory:dir".into(), "file:dir/nested/new".into()];
    input.event = "discard".into();
    let output = git.update(&input).unwrap();
    let HostRequest::MiniBuffer { purpose, .. } = output
        .requests
        .iter()
        .find(|request| matches!(request, HostRequest::MiniBuffer { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(
        purpose,
        "Delete untracked files: \"dir/nested/new\". Type yes to confirm"
    );
    let MutationIntent::ConfirmingDiscard(confirmation) = &git.views[&(1, 1)].mutation else {
        panic!("discard confirmation was not requested");
    };
    assert_eq!(confirmation.request.entries.len(), 1);
    assert!(workspace.0.join("dir/nested/new").exists());
    input.event = "layer_result".into();
    input.argument = json!({"purpose":purpose,"text":"yes","cancelled":false});
    git.update(&input).unwrap();
    let output = ready(&mut git, &mut input);
    assert!(!workspace.0.join("dir/nested/new").exists());
    assert!(output.display.unwrap().rows.is_empty());
    assert!(
        output
            .selection
            .is_some_and(|selection| selection.selected.is_empty())
    );
}

fn stage_without_delivering_completion(git: &mut Git, input: &mut PluginInput) {
    input.selection = browser::single(Some("file:file".into()));
    input.event = "stage".into();
    git.update(input).unwrap();
    let MutationState::Writing { mutation, .. } = &mut git.models.get_mut(&1).unwrap().mutation
    else {
        panic!("the mutation did not start");
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = mutation.task.poll() {
            assert_eq!(result, &Ok(()));
            return;
        }
        assert!(Instant::now() < deadline, "Git mutation did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn ignoring_after_staging_waits_for_the_status_refresh() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "head\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(workspace.0.join("file"), "worktree\n").unwrap();
    fs::write(workspace.0.join("local"), "local\n").unwrap();
    let exclude = workspace.0.join(".git/info/exclude");
    let mut expected = fs::read(&exclude).unwrap();
    expected.extend_from_slice(b"/local\n");
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    stage_without_delivering_completion(&mut git, &mut input);
    input.selection = browser::single(Some("file:local".into()));
    input.event = "exclude".into();
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    assert_eq!(fs::read(exclude).unwrap(), expected);
    assert_eq!(workspace.git(&["check-ignore", "local"]), "local\n");
    assert_eq!(workspace.git(&["show", ":file"]), "worktree\n");
}

#[test]
fn unstaging_after_staging_completes_uses_the_refreshed_index_status() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "head\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(workspace.0.join("file"), "worktree\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    stage_without_delivering_completion(&mut git, &mut input);
    input.event = "unstage".into();
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    assert_eq!(workspace.git(&["show", ":file"]), "head\n");
    assert_eq!(
        fs::read_to_string(workspace.0.join("file")).unwrap(),
        "worktree\n"
    );
}

#[test]
fn switching_pages_is_handled_when_the_same_update_receives_a_mutation_result() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "new\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    stage_without_delivering_completion(&mut git, &mut input);
    input.event = "page-history".into();
    let output = git.update(&input).unwrap();
    let navigations = output
        .requests
        .iter()
        .filter_map(|request| match request {
            HostRequest::Navigate {
                navigation:
                    Navigation::Page {
                        group,
                        current,
                        page,
                        kind,
                        ..
                    },
            } => Some((
                group.as_str(),
                current.as_str(),
                page.as_str(),
                kind.as_str(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(navigations, [("git", "changes", "history", "git-history")]);
}

#[test]
fn opening_after_staging_completes_focuses_the_refreshed_staged_diff() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "new\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    stage_without_delivering_completion(&mut git, &mut input);
    input.event = "open".into();
    let mut output = git.update(&input).unwrap();
    input.event = "tick".into();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for request in output.requests {
            if let HostRequest::PreviewGroup {
                panes, focus: true, ..
            } = request
            {
                assert_eq!(panes.len(), 1);
                let PreviewPane::View {
                    id,
                    content: PreviewContent::GitDiff { diff },
                    loading,
                    ..
                } = &panes[0]
                else {
                    panic!("expected the refreshed Git diff");
                };
                assert_eq!(id, "staged");
                assert!(!loading);
                assert_eq!(
                    diff.source,
                    DiffSource::Changes {
                        side: DiffSide::Staged
                    }
                );
                assert_eq!(diff.files.len(), 1);
                assert_eq!(diff.files[0].target.path, "file");
                let additions = diff.files[0]
                    .raw
                    .lines()
                    .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
                    .collect::<Vec<_>>();
                assert_eq!(additions, ["+new"]);
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the refreshed diff did not receive focus"
        );
        std::thread::yield_now();
        output = git.update(&input).unwrap();
    }
}

#[test]
fn navigation_during_a_mutation_refresh_keeps_the_previous_diff_until_settled() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "new\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.selection = browser::single(Some("file:file".into()));
    input.event = "preview".into();
    let mut output = git.update(&input).unwrap();
    input.event = "tick".into();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(panes) = output.requests.iter().find_map(|request| match request {
            HostRequest::PreviewGroup { panes, .. } => Some(panes),
            _ => None,
        }) {
            assert!(
                matches!(&panes[..], [PreviewPane::View { id, path, content: PreviewContent::GitDiff { diff }, .. }]
                if id == "unstaged" && path == "file" && diff.files.len() == 1 && diff.files[0].target.path == "file")
            );
            break;
        }
        assert!(Instant::now() < deadline, "initial diff did not finish");
        std::thread::yield_now();
        output = git.update(&input).unwrap();
    }
    input.event = "select".into();
    let navigation_started = Instant::now();
    git.update(&input).unwrap();
    stage_without_delivering_completion(&mut git, &mut input);
    input.event = "select".into();
    let output = git.update(&input).unwrap();
    assert!(
        !output
            .requests
            .iter()
            .any(|request| matches!(request, HostRequest::PreviewGroup { .. }))
    );
    input.event = "fs_changed".into();
    let mut output = git.update(&input).unwrap();
    input.event = "tick".into();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            !output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::PreviewGroup { .. }))
        );
        assert!(
            output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::PreviewPending))
        );
        if git.views[&(1, 1)].load.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "Git status did not refresh");
        std::thread::yield_now();
        output = git.update(&input).unwrap();
    }
    let minimum_delay = (navigation_started + input.preview_debounce)
        .saturating_duration_since(Instant::now())
        .as_millis() as u64;
    assert!(minimum_delay > 0);
    assert!(output.requests.iter().any(|request| matches!(request,
        HostRequest::Schedule { after_ms } if *after_ms >= minimum_delay
    )));
    assert_eq!(workspace.git(&["show", ":file"]), "new\n");
}

#[test]
fn leaving_during_a_status_refresh_preserves_the_pending_diff_refresh() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "new\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    assert!(matches!(git.models[&1].result, LoadResult::Ready));
    let view = git.views.get_mut(&(1, 1)).unwrap();
    view.diff_refresh = DiffRefresh::WaitingForStatus {
        epoch: 0,
        focus: None,
    };
    view.load = Some(GitLoad {
        epoch: 0,
        task: Task::start(&git.services, QueryRequest::Status {}).unwrap(),
        generation: git.models[&1].load_generation,
        published_revision: git.models[&1].revision,
    });

    input.event = "view_leaving".into();
    git.update(&input).unwrap();

    assert!(git.views[&(1, 1)].load.is_none());
    assert!(matches!(
        git.views[&(1, 1)].diff_refresh,
        DiffRefresh::WaitingForStatus { .. }
    ));
}

#[test]
fn an_older_commit_query_cannot_replace_the_newer_shared_result() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("old"), "first\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "first"]);
    let first = workspace.git(&["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(workspace.0.join("new"), "second\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "second"]);
    let second = workspace.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let services = Rc::new(RefCell::new(
        NativeServices::new(workspace.0.clone()).unwrap(),
    ));
    let mut git = Git::new(services, Page::CommitFiles);
    let mut input = input();
    input.event = "init".into();
    input.argument = json!({"commit": first});
    git.update(&input).unwrap();
    input.view_id = 2;
    input.argument = json!({"commit": second});
    git.update(&input).unwrap();
    input.event = "tick".into();
    input.argument = serde_json::Value::Null;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = git.update(&input).unwrap();
        if git.views[&(1, input.view_id)].load.is_none() {
            assert_eq!(
                output
                    .display
                    .unwrap()
                    .rows
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>(),
                ["file:new"]
            );
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    input.view_id = 1;
    loop {
        let output = git.update(&input).unwrap();
        if git.views[&(1, input.view_id)].load.is_none() {
            assert_eq!(
                output
                    .display
                    .unwrap()
                    .rows
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>(),
                ["file:new"]
            );
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn leaving_after_confirming_discard_preserves_the_started_write() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "untracked\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.selection = browser::single(Some("file:file".into()));
    input.event = "discard".into();
    let output = git.update(&input).unwrap();
    let purpose = output
        .requests
        .iter()
        .find_map(|request| match request {
            HostRequest::MiniBuffer { purpose, .. } => Some(purpose.clone()),
            _ => None,
        })
        .unwrap();
    input.event = "layer_result".into();
    input.argument = json!({"purpose": purpose, "text": "yes"});
    git.update(&input).unwrap();
    input.event = "view_leaving".into();
    git.update(&input).unwrap();
    input.event = "tick".into();
    ready(&mut git, &mut input);
    assert!(!workspace.0.join("file").exists());
    assert!(matches!(git.views[&(1, 1)].mutation, MutationIntent::Idle));
}

#[test]
fn concurrent_views_can_finish_their_initial_status_load() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "untracked\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = input();
    input.event = "init".into();
    for view in [1, 2] {
        input.view_id = view;
        git.update(&input).unwrap();
    }
    input.event = "tick".into();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for view in [1, 2] {
            input.view_id = view;
            git.update(&input).unwrap();
        }
        if git.views.values().all(|view| view.load.is_none()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "concurrent views did not finish loading"
        );
        std::thread::yield_now();
    }
    for view in [1, 2] {
        input.view_id = view;
        let output = git.update(&input).unwrap();
        assert_eq!(
            output
                .display
                .unwrap()
                .rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["file:file"]
        );
    }
}

#[test]
fn leaving_cancels_an_unconfirmed_discard() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "untracked\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.selection = browser::single(Some("file:file".into()));
    input.event = "discard".into();
    input.context.operation_id = Some("discard-operation".into());
    let output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["discard-operation"]);
    assert!(output.completed_operations.is_empty());
    input.context.operation_id = None;
    let purpose = output
        .requests
        .iter()
        .find_map(|request| match request {
            HostRequest::MiniBuffer { purpose, .. } => Some(purpose.clone()),
            _ => None,
        })
        .unwrap();
    input.event = "view_leaving".into();
    let output = git.update(&input).unwrap();
    assert!(output.completed_operations.is_empty());
    let mut outputs = git.poll_operations();
    assert_eq!(outputs.len(), 1);
    let output = outputs.pop().unwrap();
    assert!(output.requests.is_empty());
    assert_eq!(output.completed_operations.len(), 1);
    assert_eq!(
        output.completed_operations[0].operation_id,
        "discard-operation"
    );
    assert!(matches!(
        output.completed_operations[0].result,
        OperationResult::Cancelled
    ));
    input.event = "layer_result".into();
    input.argument = json!({"purpose": purpose, "text": "yes"});
    let Err(error) = git.update(&input) else {
        panic!("expected an expired confirmation");
    };
    assert!(error.to_string().contains("confirmation expired"));
    assert_eq!(
        fs::read_to_string(workspace.0.join("file")).unwrap(),
        "untracked\n"
    );
}

struct CleanFilterGate(std::path::PathBuf);

impl CleanFilterGate {
    fn failing(workspace: &Workspace) -> Self {
        fs::write(
            workspace.0.join(".git/info/attributes"),
            "a filter=record\nb filter=blocked\n",
        )
        .unwrap();
        workspace.git(&[
            "config",
            "filter.record.clean",
            "sh -c 'test -f .git/filter-finished || touch .git/concurrent-write; cat'",
        ]);
        workspace.git(&["config", "filter.blocked.clean", "sh .git/blocked-clean"]);
        workspace.git(&["config", "filter.blocked.required", "true"]);
        fs::write(
            workspace.0.join(".git/blocked-clean"),
            "touch .git/filter-entered\nwhile [ -d .git ] && [ ! -f .git/filter-release ]; do sleep 0.01; done\ntouch .git/filter-finished\nexit 1\n",
        )
        .unwrap();
        Self(workspace.0.join(".git/filter-release"))
    }

    fn wait_until_entered(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.0.with_file_name("filter-entered").exists() {
            assert!(Instant::now() < deadline, "the clean filter did not start");
            std::thread::yield_now();
        }
    }

    fn release(&self) {
        fs::write(&self.0, "release").unwrap();
    }
}

impl Drop for CleanFilterGate {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, "release");
    }
}

struct QueryWorkerGate {
    tasks: Vec<Task>,
    directory: tempfile::TempDir,
}

impl QueryWorkerGate {
    fn occupy(services: &Services) -> Self {
        // Keep synchronization writes outside the repository's watches.
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("pager");
        let release = directory.path().join("release");
        fs::write(
            &script,
            "touch \"${1}/ready-$$\"\nwhile [ -d \"$1\" ] && [ ! -f \"$2\" ]; do sleep 0.01; done\ncat\n",
        )
        .unwrap();
        let request = QueryRequest::DiffFiles {
            source: DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            files: vec![DiffTarget {
                path: "a".into(),
                original_path: None,
            }],
            render: DiffRenderOptions {
                pager: Some(zess_git::PagerCommand {
                    program: "sh".into(),
                    args: vec![
                        script.to_str().unwrap().into(),
                        directory.path().to_str().unwrap().into(),
                        release.to_str().unwrap().into(),
                    ],
                }),
                ..DiffRenderOptions::default()
            },
        };
        // NativeServices has four query workers. Hold all four at real pager
        // processes so refresh/stage can establish a waiting intent deterministically.
        let mut gate = Self {
            tasks: (0..4)
                .map(|_| Task::start(services, request.clone()).unwrap())
                .collect(),
            directory,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            for task in &mut gate.tasks {
                assert!(task.poll().unwrap().is_none());
            }
            let blocked = fs::read_dir(gate.directory.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name.to_string_lossy().starts_with("ready-"))
                .count();
            if blocked == gate.tasks.len() {
                return gate;
            }
            assert!(
                Instant::now() < deadline,
                "query workers did not reach their gates"
            );
            std::thread::yield_now();
        }
    }
}

impl Drop for QueryWorkerGate {
    fn drop(&mut self) {
        let _ = fs::write(self.directory.path().join("release"), "release");
    }
}

#[test]
fn a_waiting_view_preserves_another_views_write_and_its_failure() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("a"), "head a\n").unwrap();
    fs::write(workspace.0.join("b"), "head b\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(workspace.0.join("a"), "worktree a\n").unwrap();
    fs::write(workspace.0.join("b"), "worktree b\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    let gate = CleanFilterGate::failing(&workspace);
    input.view_id = 2;
    input.event = "render".into();
    git.update(&input).unwrap();

    let query_gate = QueryWorkerGate::occupy(&git.services);
    input.view_id = 1;
    input.selection = browser::single(Some("file:a".into()));
    input.event = "refresh".into();
    git.update(&input).unwrap();
    input.event = "stage".into();
    input.context.operation_id = Some("waiting-stage".into());
    let output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["waiting-stage"]);
    assert!(output.completed_operations.is_empty());
    input.view_id = 2;
    input.selection = browser::single(Some("file:b".into()));
    input.event = "stage".into();
    input.context.operation_id = Some("failing-stage".into());
    let output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["failing-stage"]);
    assert!(output.completed_operations.is_empty());
    input.context.operation_id = None;
    gate.wait_until_entered();
    drop(query_gate);
    input.view_id = 1;
    input.selection = browser::single(Some("file:a".into()));
    input.event = "tick".into();
    let deadline = Instant::now() + Duration::from_secs(10);
    while git.views[&(1, 1)].load.is_some() {
        git.update(&input).unwrap();
        assert!(
            Instant::now() < deadline,
            "the waiting status did not finish"
        );
        std::thread::yield_now();
    }
    assert!(matches!(
        git.views[&(1, 1)].mutation,
        MutationIntent::WaitingForStatus { .. }
    ));
    gate.release();

    let mut errors = Vec::new();
    let mut completions = Vec::new();
    loop {
        let update = git.update(&input).unwrap();
        for output in std::iter::once(update).chain(git.poll_operations()) {
            completions.extend(output.completed_operations);
            errors.extend(
                output
                    .requests
                    .into_iter()
                    .filter_map(|request| match request {
                        HostRequest::Message { text } => Some(text),
                        _ => None,
                    }),
            );
        }
        if matches!(git.models[&1].mutation, MutationState::Idle { .. })
            && matches!(git.views[&(1, 1)].mutation, MutationIntent::Idle)
            && git.views[&(1, 1)].load.is_none()
        {
            break;
        }
        assert!(Instant::now() < deadline, "the queued write did not finish");
        std::thread::yield_now();
    }
    assert_eq!(completions.len(), 2);
    assert!(
        completions
            .iter()
            .any(|completion| completion.operation_id == "waiting-stage"
                && matches!(completion.result, OperationResult::Completed))
    );
    assert!(
        completions
            .iter()
            .any(|completion| completion.operation_id == "failing-stage"
                && matches!(completion.result, OperationResult::Failed { .. }))
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("git add -A:"), "{errors:?}");
    assert_eq!(workspace.git(&["show", ":a"]), "worktree a\n");
    assert_eq!(workspace.git(&["show", ":b"]), "head b\n");
    assert!(!workspace.0.join(".git/concurrent-write").exists());
}

#[test]
fn another_view_waits_for_status_after_a_write_before_unstaging() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "head\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(workspace.0.join("file"), "worktree\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.view_id = 2;
    input.event = "refresh".into();
    git.update(&input).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let load = git.views.get_mut(&(1, 2)).unwrap().load.as_mut().unwrap();
        if let Some(progress) = load.task.status_progress().unwrap() {
            let entries = progress.entries;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].index, " ");
            assert_eq!(entries[0].worktree, "M");
            break;
        }
        // Advance registration and start the worker, then pause after its status
        // entries arrive and before publishing the complete result. If a worker
        // finishes between these polls, start another request to reach that pause.
        if load.task.poll().unwrap().is_some() {
            git.update(&input).unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "the old status did not produce progress"
        );
        std::thread::yield_now();
    }
    input.view_id = 1;
    stage_without_delivering_completion(&mut git, &mut input);
    assert_eq!(workspace.git(&["show", ":file"]), "worktree\n");
    input.event = "view_leaving".into();
    git.update(&input).unwrap();

    // Completion on the leaving View cannot start a fresh status request there.
    // The other View still owns a status response from before the write.
    assert!(matches!(
        git.models[&1].mutation,
        MutationState::RefreshingStatus { .. }
    ));
    input.view_id = 2;
    input.event = "unstage".into();
    let mut output = git.update(&input).unwrap();
    input.event = "tick".into();
    loop {
        assert!(
            !output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::Message { .. }))
        );
        if matches!(git.models[&1].mutation, MutationState::Idle { .. })
            && matches!(git.views[&(1, 2)].mutation, MutationIntent::Idle)
            && git.views[&(1, 2)].load.is_none()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second View did not finish unstaging"
        );
        std::thread::yield_now();
        output = git.update(&input).unwrap();
    }
    assert_eq!(workspace.git(&["show", ":file"]), "head\n");
    assert_eq!(
        fs::read_to_string(workspace.0.join("file")).unwrap(),
        "worktree\n"
    );
    let rows = output.display.unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "file:file");
    assert_eq!(rows[0].cells[0].spans[0].text, " ");
    assert_eq!(rows[0].cells[0].spans[1].text, "M");
}

#[test]
fn pager_configuration_rejects_shell_strings_and_invalid_command_objects() {
    let workspace = Workspace::new();
    let mut git = workspace.plugin();
    for pager in [
        json!("cat -n"),
        json!({ "args": [] }),
        json!({ "program": "  ", "args": [] }),
        json!({ "program": "cat", "args": "-n" }),
        json!({ "program": "cat", "args": [1] }),
        json!({ "program": "cat", "args": ["\0"] }),
    ] {
        let mut input = input();
        input.options = json!({ "diff-pager": pager });
        assert!(
            git.update(&input).is_err(),
            "accepted invalid pager: {}",
            input.options
        );
    }
}

#[test]
fn a_started_write_reports_its_actual_failure_after_its_buffer_is_removed() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("a"), "head a\n").unwrap();
    fs::write(workspace.0.join("b"), "head b\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(workspace.0.join("b"), "worktree b\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    let gate = CleanFilterGate::failing(&workspace);
    input.event = "stage".into();
    input.selection = browser::single(Some("file:b".into()));
    input.context.operation_id = Some("blocked-stage".into());
    let output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["blocked-stage"]);
    assert!(output.completed_operations.is_empty());
    gate.wait_until_entered();
    assert!(git.poll_operations().is_empty());
    let revision = git.services.borrow().fs_revision();
    git.remove_buffer(1);
    assert!(git.poll_operations().is_empty());
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(10);
    let completions = loop {
        let completions = git
            .poll_operations()
            .into_iter()
            .flat_map(|output| output.completed_operations)
            .collect::<Vec<_>>();
        if !completions.is_empty() {
            break completions;
        }
        assert!(Instant::now() < deadline, "write completion did not arrive");
        std::thread::yield_now();
    };
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].operation_id, "blocked-stage");
    assert!(
        matches!(&completions[0].result, OperationResult::Failed { message } if message.starts_with("git add -A:"))
    );
    assert_eq!(workspace.git(&["show", ":b"]), "head b\n");
    assert!(git.poll_operations().is_empty());
    assert_eq!(git.services.borrow().fs_revision(), revision + 1);
}

#[test]
fn a_background_completion_refreshes_status_and_invalidates_the_cache_once() {
    for operation_id in [Some("stage-operation"), None] {
        let workspace = Workspace::new();
        fs::write(workspace.0.join("file"), "worktree\n").unwrap();
        let mut git = workspace.plugin();
        let mut input = initialize(&mut git);
        input.context.operation_id = operation_id.map(str::to_owned);
        stage_without_delivering_completion(&mut git, &mut input);

        let revision = git.services.borrow().fs_revision();
        let outputs = git.poll_operations();
        let completions = outputs
            .into_iter()
            .flat_map(|output| output.completed_operations)
            .collect::<Vec<_>>();
        assert_eq!(completions.len(), usize::from(operation_id.is_some()));
        if let Some(operation_id) = operation_id {
            assert_eq!(completions[0].operation_id, operation_id);
            assert!(matches!(completions[0].result, OperationResult::Completed));
        }
        assert_eq!(git.services.borrow().fs_revision(), revision + 1);
        assert!(git.poll_operations().is_empty());

        input.context.operation_id = None;
        let output = ready(&mut git, &mut input);
        let rows = output.display.unwrap().rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "file:file");
        assert_eq!(rows[0].cells[0].spans[0].text, "A");
        assert_eq!(rows[0].cells[0].spans[1].text, " ");
        assert!(output.completed_operations.is_empty());
        assert!(git.poll_operations().is_empty());
        assert_eq!(git.services.borrow().fs_revision(), revision + 1);
        assert_eq!(workspace.git(&["show", ":file"]), "worktree\n");
    }
}

#[test]
fn removing_the_buffer_does_not_reprocess_a_write_that_already_completed() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "worktree\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.context.operation_id = Some("stage-operation".into());
    stage_without_delivering_completion(&mut git, &mut input);
    let revision = git.services.borrow().fs_revision();
    let outputs = git.poll_operations();
    assert!(matches!(
        outputs[0].completed_operations.as_slice(),
        [OperationCompletion { operation_id, result: OperationResult::Completed }]
            if operation_id == "stage-operation"
    ));
    git.remove_buffer(1);
    assert!(git.poll_operations().is_empty());
    assert_eq!(git.services.borrow().fs_revision(), revision + 1);
}

#[test]
fn leaving_cancels_a_historical_file_waiting_for_its_contents() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("a"), "committed\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    let commit = workspace.git(&["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(workspace.0.join("a"), "worktree\n").unwrap();
    let mut git = workspace.plugin();
    git.page = Page::CommitFiles;
    let mut input = input();
    input.event = "init".into();
    input.argument = json!({ "commit": commit });
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    let _gate = QueryWorkerGate::occupy(&git.services);
    input.selection = browser::single(Some("file:a".into()));
    input.event = "edit".into();
    input.context.operation_id = Some("historical-file".into());
    let output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["historical-file"]);
    assert!(output.completed_operations.is_empty());
    assert!(
        !output
            .requests
            .iter()
            .any(|request| matches!(request, HostRequest::OpenSnapshot { .. }))
    );
    input.event = "view_leaving".into();
    input.context.operation_id = None;
    let output = git.update(&input).unwrap();
    assert!(output.completed_operations.is_empty());
    let mut outputs = git.poll_operations();
    assert_eq!(outputs.len(), 1);
    let output = outputs.pop().unwrap();
    assert!(output.requests.is_empty());
    assert_eq!(output.completed_operations.len(), 1);
    assert_eq!(
        output.completed_operations[0].operation_id,
        "historical-file"
    );
    assert!(matches!(
        output.completed_operations[0].result,
        OperationResult::Cancelled
    ));
    assert!(git.poll_operations().is_empty());
}

#[test]
fn opening_a_historical_file_reports_completion_with_the_loaded_snapshot() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("a"), "committed\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    let commit = workspace.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let mut git = workspace.plugin();
    git.page = Page::CommitFiles;
    let mut input = input();
    input.event = "init".into();
    input.argument = json!({ "commit": commit });
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    input.selection = browser::single(Some("file:a".into()));
    input.event = "edit".into();
    input.context.operation_id = Some("historical-file".into());
    let mut output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["historical-file"]);
    input.event = "tick".into();
    input.context.operation_id = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while output.completed_operations.is_empty() {
        assert!(
            Instant::now() < deadline,
            "snapshot completion did not arrive"
        );
        std::thread::yield_now();
        output = git.update(&input).unwrap();
    }
    assert_eq!(output.completed_operations.len(), 1);
    assert_eq!(
        output.completed_operations[0].operation_id,
        "historical-file"
    );
    assert!(matches!(
        output.completed_operations[0].result,
        OperationResult::Completed
    ));
    assert!(output.requests.iter().any(|request| matches!(request, HostRequest::OpenSnapshot { snapshot, .. } if snapshot.text == "committed\n")));
}

#[test]
fn write_completions_are_separate_from_yank_requests() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("file"), "worktree\n").unwrap();
    let mut git = workspace.plugin();
    let mut input = initialize(&mut git);
    input.context.operation_id = Some("stage-operation".into());
    stage_without_delivering_completion(&mut git, &mut input);
    input.context.operation_id = None;
    input.event = "tick".into();
    let update = git.update(&input).unwrap();
    assert!(update.completed_operations.is_empty());
    input.context.operation_id = Some("yank-operation".into());
    let mut queued = PluginOutput::default();
    git.views
        .get_mut(&(1, 1))
        .unwrap()
        .yanks
        .queue(
            &git.services,
            Event::YankPath,
            &input,
            vec![Target {
                path: "file".into(),
                name: "file".into(),
                kind: TargetKind::File {
                    source: Source::Worktree,
                    original_path: None,
                },
            }],
            &mut queued,
        )
        .unwrap();
    assert_eq!(queued.pending_operations, ["yank-operation"]);
    let outputs = git.poll_operations();
    assert_eq!(outputs.len(), 2);
    let write = outputs
        .iter()
        .find(|output| {
            output
                .completed_operations
                .iter()
                .any(|completion| completion.operation_id == "stage-operation")
        })
        .unwrap();
    assert!(write.requests.is_empty());
    assert_eq!(write.completed_operations.len(), 1);
    assert!(matches!(
        write.completed_operations[0].result,
        OperationResult::Completed
    ));
    let yank = outputs
        .iter()
        .find(|output| {
            output
                .completed_operations
                .iter()
                .any(|completion| completion.operation_id == "yank-operation")
        })
        .unwrap();
    assert_eq!(yank.completed_operations.len(), 1);
    assert!(
        matches!(yank.requests.as_slice(), [HostRequest::Yank { values, .. }] if values == &["file"])
    );
    assert!(git.poll_operations().is_empty());
}

#[test]
fn replacing_a_snapshot_keeps_the_cancelled_operation_separate_from_the_new_open() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("a"), "committed\n").unwrap();
    workspace.git(&["add", "."]);
    workspace.git(&["commit", "--quiet", "-m", "baseline"]);
    let commit = workspace.git(&["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(workspace.0.join("a"), "worktree\n").unwrap();
    let mut git = workspace.plugin();
    git.page = Page::CommitFiles;
    let mut input = input();
    input.event = "init".into();
    input.argument = json!({ "commit": commit });
    git.update(&input).unwrap();
    ready(&mut git, &mut input);
    let gate = QueryWorkerGate::occupy(&git.services);
    input.selection = browser::single(Some("file:a".into()));
    input.event = "edit".into();
    input.context.operation_id = Some("old-snapshot".into());
    let output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["old-snapshot"]);
    input.context.operation_id = Some("new-snapshot".into());
    let output = git.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["new-snapshot"]);
    assert!(output.completed_operations.is_empty());
    let outputs = git.poll_operations();
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].requests.is_empty());
    assert_eq!(outputs[0].completed_operations.len(), 1);
    assert_eq!(
        outputs[0].completed_operations[0].operation_id,
        "old-snapshot"
    );
    assert!(matches!(
        outputs[0].completed_operations[0].result,
        OperationResult::Cancelled
    ));
    drop(gate);
    input.event = "tick".into();
    input.context.operation_id = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    let output = loop {
        let output = git.update(&input).unwrap();
        if !output.completed_operations.is_empty() {
            break output;
        }
        assert!(
            Instant::now() < deadline,
            "replacement snapshot did not arrive"
        );
        std::thread::yield_now();
    };
    assert_eq!(output.completed_operations.len(), 1);
    assert_eq!(output.completed_operations[0].operation_id, "new-snapshot");
    assert!(matches!(
        output.completed_operations[0].result,
        OperationResult::Completed
    ));
    assert!(output.requests.iter().any(|request| matches!(request, HostRequest::OpenSnapshot { snapshot, .. } if snapshot.text == "committed\n")));
    assert!(git.poll_operations().is_empty());
}
