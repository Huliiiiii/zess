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
        if git.models[&1].mutation.is_none() && git.views[&(1, 1)].task.is_none() {
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
    assert!(git.models[&1].mutation.is_none());
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
    assert_eq!(
        git.views[&(1, 1)]
            .discard
            .as_ref()
            .unwrap()
            .request
            .entries
            .len(),
        1
    );
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
    let task = git.models.get_mut(&1).unwrap().mutation.as_mut().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = task.poll() {
            assert_eq!(result, &Ok(()));
            return;
        }
        assert!(Instant::now() < deadline, "Git mutation did not finish");
        std::thread::yield_now();
    }
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
        if git.views[&(1, 1)].task.is_none() {
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
    view.refresh_diffs = true;
    view.task = Some(Task::start(&git.services, QueryRequest::Status {}).unwrap());

    input.event = "view_leaving".into();
    git.update(&input).unwrap();

    assert!(git.views[&(1, 1)].task.is_none());
    assert!(git.views[&(1, 1)].refresh_diffs);
}
