use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zess_platform::{NativeFileSystem, SystemProcessRunner, WatchBackend, WatchEvent, WatchHandle};

use super::{NativeServices, Observation, QueryRequest};

mod git_queries;
mod observations;

struct QuietWatch;

impl WatchHandle for QuietWatch {
    fn refresh(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

impl WatchBackend for QuietWatch {
    fn watch(
        &self,
        _path: &Path,
        _recursive: bool,
        _callback: Box<dyn Fn(anyhow::Result<WatchEvent>) + Send>,
    ) -> anyhow::Result<Box<dyn WatchHandle>> {
        Ok(Box::new(Self))
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "zess-services-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn services(&self) -> NativeServices {
        NativeServices::new(self.0.clone()).unwrap()
    }

    fn services_without_watch_events(&self) -> NativeServices {
        NativeServices::with_backends(
            self.0.clone(),
            Arc::new(NativeFileSystem),
            Arc::new(SystemProcessRunner),
            Arc::new(QuietWatch),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn names(result: &Value) -> Vec<&str> {
    result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect()
}

fn hunk_lines(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .skip_while(|line| !line.starts_with("@@ "))
        .collect()
}

const TIMEOUT: Duration = Duration::from_secs(5);

struct ReadProbe {
    reads: Arc<AtomicUsize>,
    entered: Receiver<String>,
    resumed: Receiver<()>,
    release: Option<Sender<()>>,
    blocked_reads: usize,
}

impl ReadProbe {
    fn install(services: &mut NativeServices, blocked_reads: usize) -> Self {
        let reads = Arc::new(AtomicUsize::new(0));
        let (entered_sender, entered) = mpsc::channel();
        let (resumed_sender, resumed) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let wait = Mutex::new(wait);
        let count = reads.clone();
        services.workspace.read_hook = Some(Arc::new(move |path| {
            let index = count.fetch_add(1, Ordering::SeqCst);
            let _ = entered_sender.send(path.to_owned());
            if index < blocked_reads {
                wait.lock().unwrap().recv_timeout(TIMEOUT).unwrap();
                let _ = resumed_sender.send(());
            }
        }));
        Self {
            reads,
            entered,
            resumed,
            release: Some(release),
            blocked_reads,
        }
    }

    fn wait_for_read(&self, services: &mut NativeServices, handle: u64) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.entered.try_recv() {
                Ok(path) => return path,
                Err(mpsc::TryRecvError::Disconnected) => panic!("read hook stopped"),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match services.poll_query(handle).unwrap() {
                super::QueryPoll::Pending => {}
                super::QueryPoll::Ready(result) => panic!("read did not block: {result:?}"),
            }
            assert!(Instant::now() < deadline, "read did not start");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            for _ in 0..self.blocked_reads {
                let _ = release.send(());
            }
        }
    }

    fn count(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Drop for ReadProbe {
    fn drop(&mut self) {
        self.release();
    }
}

fn start(services: &mut NativeServices, operation: &str, argument: Value) -> u64 {
    let result = services.call(
        "job.start",
        json!({"operation":operation, "argument":argument}),
    );
    result["handle"]
        .as_u64()
        .unwrap_or_else(|| panic!("{result}"))
}

fn ready(services: &mut NativeServices, handle: u64) -> Value {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let result = services.call("job.poll", json!({"handle":handle}));
        match result["status"].as_str() {
            Some("ready") => {
                assert!(result["result"]["error"].is_null(), "{result}");
                return result["result"].clone();
            }
            Some("pending") => assert!(Instant::now() < deadline, "job {handle} timed out"),
            _ => panic!("unexpected job result: {result}"),
        }
        thread::yield_now();
    }
}

fn observe(services: &mut NativeServices, operation: &str, argument: &Value) -> Observation {
    let mut observation = services.observe(operation, argument).unwrap();
    let query = services
        .acquire_key(super::watcher::QueryKey::from_call(operation, argument).unwrap())
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while query.registration().ready() != Ok(true) {
        services.poll_changes().unwrap();
        assert!(
            Instant::now() < deadline,
            "watch registration did not finish"
        );
        thread::sleep(Duration::from_millis(1));
    }
    observation.changed();
    observation.acknowledge();
    observation
}

fn wait_for_change_before(
    services: &mut NativeServices,
    observation: &mut Observation,
    deadline: Instant,
) {
    while !observation.changed() {
        services.poll_changes().unwrap();
        assert!(Instant::now() < deadline, "filesystem change timed out");
        thread::sleep(Duration::from_millis(5));
    }
    observation.acknowledge();
}

fn wait_for_change(services: &mut NativeServices, observation: &mut Observation) {
    wait_for_change_before(services, observation, Instant::now() + TIMEOUT);
}

fn wait_for_query_result(
    services: &mut NativeServices,
    observation: &mut Observation,
    read: impl Fn(&mut NativeServices) -> Value,
    expected: impl Fn(&Value) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        wait_for_change_before(services, observation, deadline);
        let result = read(services);
        if expected(&result) {
            return;
        }
        // A notification can still be from the preceding operation.
        assert!(Instant::now() < deadline, "query stayed stale: {result}");
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn git_decorations_are_empty_in_an_ordinary_directory() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note"), "ordinary file\n").unwrap();
    let mut services = fixture.services();
    let handle = start(&mut services, "git.decorations", json!({}));
    let result = ready(&mut services, handle);
    assert_eq!(result["kind"], "git_decorations", "{result}");
    assert_eq!(result["entries"], json!([]), "{result}");
    assert!(services.call("git.status", json!({}))["error"].is_string());
}

#[test]
fn git_decorations_report_a_corrupt_index() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join(".git/index"), "corrupt index").unwrap();
    let mut services = fixture.services();
    let handle = start(&mut services, "git.decorations", json!({}));
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match services.poll_query(handle).unwrap() {
            super::QueryPoll::Pending => {}
            super::QueryPoll::Ready(result) => {
                let error = result.unwrap_err();
                assert!(
                    error.starts_with(&format!("read Git status in {}:", fixture.0.display())),
                    "{error}"
                );
                break;
            }
        }
        assert!(Instant::now() < deadline, "decoration query timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn git_decorations_report_staging_from_a_background_query() {
    use super::{QueryPoll, QueryResult};
    use zess_git::{FileStatus, StagingStatus};

    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    for name in ["staged", "unstaged", "both", "old"] {
        fs::write(fixture.0.join(name), "baseline\n").unwrap();
    }
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    for name in ["staged", "both"] {
        fs::write(fixture.0.join(name), "index\n").unwrap();
    }
    git(&fixture.0, &["add", "staged", "both"]);
    for name in ["unstaged", "both"] {
        fs::write(fixture.0.join(name), "worktree\n").unwrap();
    }
    fs::write(fixture.0.join("untracked"), [0, 255]).unwrap();
    git(&fixture.0, &["mv", "old", "renamed"]);

    let mut services = fixture.services();
    let handle = services
        .start_query(QueryRequest::GitDecorations {})
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let entries = loop {
        match services.poll_query(handle).unwrap() {
            QueryPoll::Pending => {}
            QueryPoll::Ready(result) => {
                let QueryResult::GitDecorations { entries } = result.unwrap() else {
                    panic!("wrong result kind");
                };
                break entries;
            }
        }
        assert!(Instant::now() < deadline, "decoration query timed out");
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(entries.len(), 5);
    for (path, expected) in [
        ("staged", StagingStatus::Staged),
        ("unstaged", StagingStatus::Unstaged),
        ("both", StagingStatus::Both),
        ("untracked", StagingStatus::Untracked),
        ("renamed", StagingStatus::Staged),
    ] {
        let entry = entries.iter().find(|entry| entry.path == path).unwrap();
        assert_eq!(entry.staging_status, expected, "{path}");
        if path == "untracked" {
            assert!(matches!(entry.status, FileStatus::Added));
        } else {
            assert!(matches!(entry.status, FileStatus::Modified));
        }
        assert_eq!(
            entry.original_path.as_deref(),
            (path == "renamed").then_some("old")
        );
    }
}

#[test]
fn git_decorations_share_status_observations_in_a_linked_worktree() {
    let fixture = Fixture::new();
    let repository = fixture.0.join("repository");
    let linked = fixture.0.join("linked");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    fs::write(repository.join("note"), "baseline\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "baseline"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let mut services = NativeServices::new(linked.clone()).unwrap();
    let mut decorations = observe(&mut services, "git.decorations", &json!({}));
    let mut status = observe(&mut services, "git.status", &json!({}));
    let decorations_query = services
        .acquire_query(&QueryRequest::GitDecorations {})
        .unwrap();
    let status_query = services.acquire_query(&QueryRequest::Status {}).unwrap();
    assert!(Arc::ptr_eq(&decorations_query, &status_query));
    fs::write(linked.join("note"), "changed\n").unwrap();
    wait_for_change(&mut services, &mut decorations);
    assert!(status.changed());
    let result = services.call("git.decorations", json!({}));
    assert_eq!(result["kind"], "git_decorations", "{result}");
    assert_eq!(result["entries"][0]["path"], "note", "{result}");
    assert_eq!(
        result["entries"][0]["staging_status"], "unstaged",
        "{result}"
    );
    assert!(result["entries"][0].get("line_stats").is_none(), "{result}");

    git(&linked, &["add", "note"]);
    wait_for_change(&mut services, &mut decorations);
    let result = services.call("git.decorations", json!({}));
    assert_eq!(result["entries"][0]["staging_status"], "staged", "{result}");
}

#[test]
fn git_decorations_retry_outdated_results_and_release_cancelled_queries() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note"), "baseline\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 1);
    let handle = start(&mut services, "git.decorations", json!({}));
    assert_eq!(
        probe.wait_for_read(&mut services, handle),
        "git.decorations"
    );
    let mut observation = observe(&mut services, "git.status", &json!({}));
    fs::write(fixture.0.join("note"), "changed\n").unwrap();
    wait_for_change(&mut services, &mut observation);
    probe.release();
    let result = ready(&mut services, handle);
    assert_eq!(result["entries"].as_array().unwrap().len(), 1, "{result}");
    assert_eq!(result["entries"][0]["path"], "note", "{result}");
    assert_eq!(
        result["entries"][0]["staging_status"], "unstaged",
        "{result}"
    );
    assert!(probe.count() >= 2);

    drop(observation);
    let cancelled = services
        .start_query(QueryRequest::GitDecorations {})
        .unwrap();
    services.cancel_query(cancelled);
    assert!(services.poll_query(cancelled).is_err());
    assert_eq!(
        services.call("service.stats", json!({}))["observed_queries"],
        0
    );
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
}

#[test]
fn directory_listings_default_non_string_paths_to_the_workspace_root() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "text").unwrap();
    let mut services = fixture.services();

    for argument in [json!({}), json!({"path":null}), json!({"path":false})] {
        let listing = services.call("fs.list", argument.clone());
        assert_eq!(listing["path"], "");
        assert_eq!(names(&listing), ["note.txt"]);
        let handle = start(&mut services, "fs.list", argument);
        let listing = ready(&mut services, handle);
        assert_eq!(listing["path"], "");
        assert_eq!(names(&listing), ["note.txt"]);
    }
}

#[test]
fn directory_previews_default_non_boolean_ignore_options_to_false() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(fixture.0.join("ignored.txt"), "hidden").unwrap();
    let mut services = fixture.services();

    for argument in [
        json!({"path":""}),
        json!({"path":"", "show_ignored":null}),
        json!({"path":"", "show_ignored":"true"}),
    ] {
        let preview = services.call("fs.preview", argument.clone());
        assert_eq!(names(&preview), [".gitignore"]);
        let handle = start(&mut services, "fs.preview", argument);
        let preview = ready(&mut services, handle);
        assert_eq!(names(&preview), [".gitignore"]);
    }
}

#[test]
fn git_status_lists_changes_when_the_palette_cannot_be_read() {
    use zess_platform::{
        Cancellation, NativeFileSystem, NativeWatchBackend, OwnedProcess, ProcessRunner,
        ProcessSpec, SystemProcessRunner,
    };

    struct UnavailablePalette;

    impl ProcessRunner for UnavailablePalette {
        fn output(
            &self,
            spec: &ProcessSpec,
            input: Option<&[u8]>,
            cancellation: &dyn Cancellation,
        ) -> anyhow::Result<std::process::Output> {
            if spec.args.iter().any(|arg| arg == "--type=color") {
                anyhow::bail!("reading the palette failed");
            }
            SystemProcessRunner.output(spec, input, cancellation)
        }

        fn spawn(&self, spec: &ProcessSpec) -> anyhow::Result<OwnedProcess> {
            SystemProcessRunner.spawn(spec)
        }
    }

    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "text\n").unwrap();
    let mut services = NativeServices::with_backends(
        fixture.0.clone(),
        Arc::new(NativeFileSystem),
        Arc::new(UnavailablePalette),
        Arc::new(NativeWatchBackend),
    )
    .unwrap();

    let handle = start(&mut services, "git.status", json!({}));
    let status = ready(&mut services, handle);
    assert_eq!(status["entries"][0]["path"], "note.txt");
    assert_eq!(status["entries"][0]["untracked"], true);
}

#[test]
fn git_status_ignores_arguments() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "text").unwrap();
    let mut services = fixture.services();

    for argument in [
        Value::Null,
        json!(false),
        json!(7),
        json!("unused"),
        json!([]),
        json!({"unused":true}),
    ] {
        let status = services.call("git.status", argument.clone());
        assert_eq!(status["entries"].as_array().unwrap().len(), 1);
        assert_eq!(status["entries"][0]["path"], "note.txt");
        assert_eq!(status["entries"][0]["untracked"], true);
        assert_eq!(
            status["entries"][0]["line_stats"],
            json!({"additions":1,"deletions":0})
        );
        let handle = start(&mut services, "git.status", argument);
        let status = ready(&mut services, handle);
        assert_eq!(status["entries"].as_array().unwrap().len(), 1);
        assert_eq!(status["entries"][0]["path"], "note.txt");
        assert_eq!(status["entries"][0]["untracked"], true);
        assert_eq!(
            status["entries"][0]["line_stats"],
            json!({"additions":1,"deletions":0})
        );
    }
}

#[test]
fn untracked_line_counts_distinguish_text_binary_and_oversized_files() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(fixture.0.join(".gitattributes"), "forced.bin binary\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "rules"]);
    for (path, content) in [
        ("empty.txt", b"".as_slice()),
        ("text.txt", b"first\nsecond\n".as_slice()),
        ("no-newline.txt", b"first\nsecond".as_slice()),
        ("binary.dat", b"\0binary".as_slice()),
        ("forced.bin", b"plain text\n".as_slice()),
        ("ignored.txt", b"ignored\n".as_slice()),
    ] {
        fs::write(fixture.0.join(path), content).unwrap();
    }
    fs::write(fixture.0.join("large.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let mut services = fixture.services();
    let handle = start(&mut services, "git.status", json!({}));
    let status = ready(&mut services, handle);
    let entries = status["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 6, "{status}");
    for (path, expected) in [
        ("empty.txt", json!({"additions":0,"deletions":0})),
        ("text.txt", json!({"additions":2,"deletions":0})),
        ("no-newline.txt", json!({"additions":2,"deletions":0})),
        ("binary.dat", Value::Null),
        ("forced.bin", Value::Null),
        ("large.txt", Value::Null),
    ] {
        let entry = entries.iter().find(|entry| entry["path"] == path).unwrap();
        assert_eq!(entry["untracked"], true);
        assert_eq!(entry["line_stats"], expected, "{path}: {entry}");
    }
}

#[cfg(unix)]
#[test]
fn untracked_counts_use_nested_workspace_paths_and_symbolic_link_content() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::create_dir_all(fixture.0.join("nested/deeper")).unwrap();
    fs::write(fixture.0.join("outside.txt"), "outside\n".repeat(4)).unwrap();
    fs::write(fixture.0.join("nested/deeper/new\tname\n.txt"), "one\ntwo").unwrap();
    std::os::unix::fs::symlink("../outside.txt", fixture.0.join("nested/link")).unwrap();
    let mut services = NativeServices::new(fixture.0.join("nested")).unwrap();
    let status = services.call("git.status", json!({}));
    let entries = status["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{status}");
    assert_eq!(entries[0]["path"], "deeper/new\tname\n.txt");
    assert_eq!(
        entries[0]["line_stats"],
        json!({"additions":2,"deletions":0})
    );
    assert_eq!(entries[1]["path"], "link");
    assert_eq!(
        entries[1]["line_stats"],
        json!({"additions":1,"deletions":0})
    );
}

#[test]
fn external_edits_refresh_untracked_line_counts() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("new.txt"), "first\n").unwrap();
    let mut services = fixture.services();
    let mut observation = observe(&mut services, "git.status", &json!({}));
    let status = services.call("git.status", json!({}));
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":1,"deletions":0})
    );

    fs::write(fixture.0.join("new.txt"), "first\nsecond").unwrap();
    wait_for_change(&mut services, &mut observation);
    let status = services.call("git.status", json!({}));
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":2,"deletions":0})
    );
}

#[test]
fn status_rename_settings_override_diff_rename_settings() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("before.txt"), "old\nkeep1\nkeep2\nkeep3\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "base"]);
    fs::rename(fixture.0.join("before.txt"), fixture.0.join("after.txt")).unwrap();
    fs::write(fixture.0.join("after.txt"), "new\nkeep1\nkeep2\nkeep3\n").unwrap();
    git(&fixture.0, &["add", "--all"]);
    git(&fixture.0, &["config", "diff.renames", "false"]);
    git(&fixture.0, &["config", "status.renames", "true"]);
    let mut services = fixture.services();
    let status = services.call("git.status", json!({}));
    assert_eq!(status["entries"].as_array().unwrap().len(), 1);
    assert_eq!(status["entries"][0]["path"], "after.txt");
    assert_eq!(status["entries"][0]["original_path"], "before.txt");
    assert_eq!(status["entries"][0]["index"], "R");
    assert_eq!(status["entries"][0]["worktree"], " ");

    git(&fixture.0, &["config", "diff.renames", "true"]);
    git(&fixture.0, &["config", "status.renames", "false"]);
    let status = services.call("git.status", json!({}));
    let entries = status["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{status}");
    assert_eq!(entries[0]["path"], "after.txt");
    assert_eq!(entries[0]["index"], "A");
    assert_eq!(entries[1]["path"], "before.txt");
    assert_eq!(entries[1]["index"], "D");
    assert!(entries.iter().all(|entry| entry["original_path"].is_null()));
}

#[test]
fn status_copy_settings_override_disabled_diff_rewrites() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    let content = "one\ntwo\nthree\nfour\nfive\nsix\n";
    fs::write(fixture.0.join("source.txt"), content).unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "base"]);
    fs::write(fixture.0.join("copy.txt"), content).unwrap();
    fs::write(fixture.0.join("source.txt"), format!("{content}seven\n")).unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["config", "diff.renames", "false"]);
    git(&fixture.0, &["config", "status.renames", "copies"]);
    let mut services = fixture.services();
    let status = services.call("git.status", json!({}));
    let entries = status["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{status}");
    assert_eq!(entries[0]["path"], "copy.txt");
    assert_eq!(entries[0]["index"], "C");
    assert_eq!(entries[0]["original_path"], "source.txt");
    assert_eq!(entries[1]["path"], "source.txt");
    assert_eq!(entries[1]["index"], "M");
}

#[test]
fn conflicted_files_keep_their_unmerged_status() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet", "-b", "main"]);
    fs::write(fixture.0.join("conflict.txt"), "base\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "base"]);
    git(&fixture.0, &["checkout", "--quiet", "-b", "other"]);
    fs::write(fixture.0.join("conflict.txt"), "other\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "other"]);
    git(&fixture.0, &["checkout", "--quiet", "main"]);
    fs::write(fixture.0.join("conflict.txt"), "main\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "main"]);
    let merge = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "merge", "other"])
        .current_dir(&fixture.0)
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1));
    let mut services = fixture.services();
    let status = services.call("git.status", json!({}));
    assert_eq!(status["entries"].as_array().unwrap().len(), 1, "{status}");
    assert_eq!(status["entries"][0]["path"], "conflict.txt");
    assert_eq!(status["entries"][0]["index"], "U");
    assert_eq!(status["entries"][0]["worktree"], "U");
    assert!(status["entries"][0]["line_stats"].is_null(), "{status}");
}

#[test]
fn changed_file_line_counts_compare_working_content_to_the_commit() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("mixed.txt"), "baseline\nkeep\n").unwrap();
    fs::write(fixture.0.join("cancelled.txt"), "baseline\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("mixed.txt"), "staged\nkeep\n").unwrap();
    fs::write(fixture.0.join("cancelled.txt"), "staged\n").unwrap();
    git(&fixture.0, &["add", "."]);
    fs::write(fixture.0.join("mixed.txt"), "baseline\nkeep\nextra\n").unwrap();
    fs::write(fixture.0.join("cancelled.txt"), "baseline\n").unwrap();
    let mut services = fixture.services();
    let handle = start(&mut services, "git.status", json!({}));
    let status = ready(&mut services, handle);
    let entries = status["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["path"], "cancelled.txt");
    assert_eq!(entries[0]["index"], "M");
    assert_eq!(entries[0]["worktree"], "M");
    assert_eq!(
        entries[0]["line_stats"],
        json!({"additions":0,"deletions":0})
    );
    assert_eq!(entries[1]["path"], "mixed.txt");
    assert_eq!(entries[1]["index"], "M");
    assert_eq!(entries[1]["worktree"], "M");
    assert_eq!(
        entries[1]["line_stats"],
        json!({"additions":1,"deletions":0})
    );
}

#[test]
fn unborn_file_line_counts_compare_working_content_to_an_empty_tree() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "staged\n").unwrap();
    git(&fixture.0, &["add", "."]);
    fs::write(fixture.0.join("note.txt"), "first\nsecond").unwrap();
    let mut services = fixture.services();
    let handle = start(&mut services, "git.status", json!({}));
    let status = ready(&mut services, handle);
    assert_eq!(status["entries"][0]["path"], "note.txt");
    assert_eq!(status["entries"][0]["index"], "A");
    assert_eq!(status["entries"][0]["worktree"], "M");
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":2,"deletions":0})
    );
}

#[test]
fn sha256_repositories_report_working_and_committed_line_counts() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet", "--object-format=sha256"]);
    fs::write(fixture.0.join("note.txt"), "first\nsecond\n").unwrap();
    git(&fixture.0, &["add", "."]);
    fs::write(fixture.0.join("note.txt"), "first\nsecond\nthird\n").unwrap();
    let mut services = fixture.services();
    let status = services.call("git.status", json!({}));
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":3,"deletions":0})
    );

    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "root"]);
    let root = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    let files = services.call("git.commit-files", json!({"commit":root}));
    assert_eq!(files["files"][0]["additions"], 3);
    assert_eq!(files["files"][0]["deletions"], 0);

    fs::write(fixture.0.join("note.txt"), "first\nthird\nfourth\n").unwrap();
    let status = services.call("git.status", json!({}));
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":1,"deletions":1})
    );
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "change"]);
    let head = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    let files = services.call("git.commit-files", json!({"commit":head}));
    assert_eq!(files["files"][0]["additions"], 1);
    assert_eq!(files["files"][0]["deletions"], 1);
}

#[test]
fn binary_changes_have_no_numeric_line_counts() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("binary.dat"), b"\0old").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("binary.dat"), b"\0new").unwrap();
    let mut services = fixture.services();
    let status = services.call("git.status", json!({}));
    assert_eq!(status["entries"][0]["path"], "binary.dat");
    assert!(status["entries"][0]["line_stats"].is_null());
}

#[test]
fn renamed_and_deleted_file_line_counts_preserve_unusual_paths() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::create_dir(fixture.0.join("dir")).unwrap();
    let original = "dir/old\tname\n.txt";
    let path = "dir/new\tname\n.txt";
    fs::write(
        fixture.0.join(original),
        "old\nkeep1\nkeep2\nkeep3\nkeep4\n",
    )
    .unwrap();
    fs::write(fixture.0.join("dir/deleted.txt"), "first\nsecond\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::rename(fixture.0.join(original), fixture.0.join(path)).unwrap();
    fs::write(fixture.0.join(path), "new\nkeep1\nkeep2\nkeep3\nkeep4\n").unwrap();
    fs::remove_file(fixture.0.join("dir/deleted.txt")).unwrap();
    git(&fixture.0, &["add", "--all"]);
    let mut services = NativeServices::new(fixture.0.join("dir")).unwrap();
    let status = services.call("git.status", json!({}));
    let entries = status["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["path"], "deleted.txt");
    assert_eq!(
        entries[0]["line_stats"],
        json!({"additions":0,"deletions":2})
    );
    assert_eq!(entries[1]["path"], "new\tname\n.txt");
    assert_eq!(entries[1]["original_path"], "old\tname\n.txt");
    assert_eq!(
        entries[1]["line_stats"],
        json!({"additions":1,"deletions":1})
    );
}

#[test]
fn renaming_a_file_without_changing_its_content_has_zero_line_counts() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("before.txt"), "first\nsecond\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "base"]);
    fs::rename(fixture.0.join("before.txt"), fixture.0.join("after.txt")).unwrap();
    git(&fixture.0, &["add", "--all"]);
    let mut services = fixture.services();
    let status = services.call("git.status", json!({}));
    assert_eq!(status["entries"].as_array().unwrap().len(), 1);
    assert_eq!(status["entries"][0]["path"], "after.txt");
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":0,"deletions":0})
    );
}

#[test]
fn git_diffs_ignore_non_string_original_paths() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "old\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("note.txt"), "new\n").unwrap();
    let mut services = fixture.services();

    for original in [Value::Null, json!(false), json!({"path":"unused"})] {
        let argument = json!({"path":"note.txt", "original_path":original, "unused":true});
        let diff = services.call("git.diff", argument.clone());
        assert_eq!(diff["path"], "note.txt");
        assert_eq!(
            hunk_lines(diff["text"].as_str().unwrap()),
            ["@@ -1 +1 @@", "-old", "+new"]
        );
        let handle = start(&mut services, "git.diff", argument);
        let diff = ready(&mut services, handle);
        assert_eq!(diff["path"], "note.txt");
        assert_eq!(
            hunk_lines(diff["text"].as_str().unwrap()),
            ["@@ -1 +1 @@", "-old", "+new"]
        );
    }
}

#[test]
fn git_diffs_reject_unknown_sides() {
    let fixture = Fixture::new();
    let mut services = fixture.services();
    let argument = json!({"path":"note.txt", "side":"both"});
    assert_eq!(
        services.call("git.diff", argument.clone())["error"],
        "unknown variant `both`, expected `staged` or `unstaged`"
    );
    let background = services.call(
        "job.start",
        json!({"operation":"git.diff", "argument":argument}),
    );
    assert_eq!(
        background["error"],
        "unknown variant `both`, expected `staged` or `unstaged`"
    );
}

#[test]
fn observations_preserve_their_permissive_argument_defaults() {
    let fixture = Fixture::new();
    let mut services = fixture.services();
    let _listing = observe(&mut services, "fs.list", &json!({"path":false}));
    let _preview = observe(
        &mut services,
        "fs.preview",
        &json!({"path":".", "show_ignored":"true"}),
    );
    assert_eq!(
        services.call("service.stats", json!({}))["observed_queries"],
        1
    );
}

#[test]
fn git_object_observations_keep_commit_file_lists_and_blobs_distinct() {
    let fixture = Fixture::new();
    let mut services = fixture.services();
    let _files = observe(&mut services, "git.commit-files", &json!({"commit":"HEAD"}));
    let _blob = observe(&mut services, "git.blob", &json!({"commit":"HEAD"}));
    assert_eq!(
        services.call("service.stats", json!({}))["observed_queries"],
        2
    );
}

#[cfg(unix)]
#[test]
fn directory_listings_keep_directories_first_and_preserve_entry_kinds() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("z-directory")).unwrap();
    fs::write(fixture.0.join("a-file"), "text").unwrap();
    std::os::unix::fs::symlink("a-file", fixture.0.join("m-link")).unwrap();
    let listing = fixture.services().call("fs.list", json!({}));

    assert_eq!(names(&listing), ["z-directory", "a-file", "m-link"]);
    let kinds = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["directory", "file", "symlink"]);
}

#[test]
fn observations_share_query_watches_and_release_them_with_the_last_consumer() {
    let fixture = Fixture::new();
    let mut services = fixture.services();
    let stats = services.call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 0);
    assert_eq!(stats["watched_paths"], 0);

    services.call("fs.preview", json!({"path":""}));
    let stats = services.call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 0);
    assert_eq!(stats["watched_paths"], 0);
    let listing = observe(&mut services, "fs.list", &json!({"path":"."}));
    let stats = services.call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 1);
    assert!(stats["watched_paths"].as_u64().unwrap() > 0);
    let preview = observe(&mut services, "fs.preview", &json!({"path":"./"}));
    let shared = services.call("service.stats", json!({}));
    assert_eq!(shared["observed_queries"], 1);
    assert_eq!(shared["watched_paths"], stats["watched_paths"]);
    drop(listing);
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        stats["watched_paths"]
    );
    drop(preview);
    let stats = services.call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 0);
    assert_eq!(stats["watched_paths"], 0);
}

#[test]
fn consumers_acknowledge_changes_independently_without_losing_newer_changes() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "first").unwrap();
    let mut services = fixture.services();
    let mut first = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    let mut second = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    fs::write(fixture.0.join("note.txt"), "second").unwrap();
    wait_for_change(&mut services, &mut second);
    assert!(first.changed());
    assert!(first.changed());
    assert!(!second.changed());

    fs::write(fixture.0.join("note.txt"), "third").unwrap();
    wait_for_change(&mut services, &mut second);
    first.acknowledge();
    assert!(first.changed());
    first.acknowledge();
    assert!(!first.changed());
}

#[test]
fn new_consumers_receive_only_changes_after_they_subscribe() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "first").unwrap();
    let mut services = fixture.services();
    let mut original = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    fs::write(fixture.0.join("note.txt"), "second").unwrap();
    wait_for_change(&mut services, &mut original);

    let mut new = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    assert!(!new.changed());
    fs::write(fixture.0.join("note.txt"), "third").unwrap();
    wait_for_change(&mut services, &mut original);
    assert!(new.changed());
    new.acknowledge();
    assert!(!new.changed());
}

#[test]
fn file_changes_invalidate_only_their_own_cached_previews() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("first.txt"), "old").unwrap();
    fs::write(fixture.0.join("second.txt"), "retained").unwrap();
    let mut services = fixture.services();
    let probe = ReadProbe::install(&mut services, 0);
    let mut first = observe(&mut services, "fs.preview", &json!({"path":"first.txt"}));
    let mut second = observe(&mut services, "fs.preview", &json!({"path":"second.txt"}));
    services.call("fs.preview", json!({"path":"first.txt"}));
    services.call("fs.preview", json!({"path":"second.txt"}));
    assert_eq!(probe.count(), 2);
    let unpolled = start(&mut services, "fs.preview", json!({"path":"first.txt"}));

    fs::write(fixture.0.join("first.txt"), "new").unwrap();
    wait_for_change(&mut services, &mut first);
    assert!(!second.changed());
    assert_eq!(services.fs_revision(), 0);
    assert_eq!(services.call("service.stats", json!({}))["cached_paths"], 1);
    assert_eq!(
        services.call("fs.preview", json!({"path":"second.txt"}))["text"],
        "retained"
    );
    assert_eq!(probe.count(), 2);
    assert_eq!(ready(&mut services, unpolled)["text"], "new");
    assert_eq!(
        services.call("fs.preview", json!({"path":"first.txt"}))["text"],
        "new"
    );
    assert_eq!(
        services.call("fs.preview", json!({"path":"second.txt"}))["text"],
        "retained"
    );
    assert_eq!(
        probe
            .entered
            .try_iter()
            .filter(|path| path == "second.txt")
            .count(),
        1
    );
}

#[test]
fn cached_queries_reuse_live_snapshots_without_starting_work() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "cached").unwrap();
    let mut services = fixture.services();
    let request = QueryRequest::Preview {
        path: "note.txt".into(),
        show_ignored: false,
    };
    assert!(services.cached_query(&request).unwrap().is_none());
    assert!(
        services
            .cached_query(&QueryRequest::Status {})
            .unwrap()
            .is_none()
    );
    assert!(services.scheduler.is_none());
    let probe = ReadProbe::install(&mut services, 0);
    let _observation = observe(&mut services, "fs.preview", &request.argument());
    let handle = services.start_query(request.clone()).unwrap();
    assert_eq!(ready(&mut services, handle)["text"], "cached");
    let before = services.call("service.stats", json!({}));

    for _ in 0..3 {
        let result = services
            .cached_query(&request)
            .unwrap()
            .unwrap()
            .into_value(0);
        assert_eq!(result["text"], "cached");
        assert_eq!(result["path"], "note.txt");
    }

    let after = services.call("service.stats", json!({}));
    for key in ["jobs", "observed_queries", "watched_paths", "cached_paths"] {
        assert_eq!(after[key], before[key]);
    }
    assert_eq!(probe.count(), 1);
    services.invalidate_fs();
    assert!(services.cached_query(&request).unwrap().is_none());
    assert_eq!(services.filesystem.jobs(), 0);
    assert_eq!(probe.count(), 1);
}

#[test]
fn directory_entries_use_workspace_relative_paths() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("nested/deeper")).unwrap();
    fs::write(fixture.0.join("nested/deeper/file"), "").unwrap();
    let mut services = fixture.services();
    let result = services.call("fs.list", json!({"path": "nested/deeper"}));
    assert_eq!(result["entries"][0]["path"], "nested/deeper/file");
    for (path, expected) in [
        ("nested/deeper/", "nested/deeper/file"),
        ("./nested/deeper", "./nested/deeper/file"),
    ] {
        let result = services.call("fs.list", json!({"path": path}));
        assert_eq!(result["entries"][0]["path"], expected);
    }
    #[cfg(windows)]
    {
        let result = services.call("fs.list", json!({"path": "nested\\deeper"}));
        assert_eq!(result["entries"][0]["path"], "nested/deeper/file");
    }
}

#[cfg(unix)]
#[test]
fn directory_listing_rejects_non_utf8_filenames() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::new();
    fs::write(fixture.0.join(std::ffi::OsStr::from_bytes(b"\xff")), "").unwrap();
    let result = fixture.services().call("fs.list", json!({}));
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("filename that is not UTF-8")
    );
}

#[cfg(unix)]
#[test]
fn directory_entry_paths_preserve_literal_backslashes() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("nested")).unwrap();
    fs::write(fixture.0.join("nested/a\\b"), "").unwrap();
    let mut services = fixture.services();
    let result = services.call("fs.list", json!({"path": "nested"}));
    assert_eq!(result["entries"][0]["path"], "nested/a\\b");
}

#[test]
fn cached_directory_queries_preserve_each_requests_filter_and_path() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join(".gitignore"), "*.tmp\n").unwrap();
    fs::write(fixture.0.join("hidden.tmp"), "ignored").unwrap();
    fs::write(fixture.0.join("visible.txt"), "visible").unwrap();
    let mut services = fixture.services();
    let _observation = observe(&mut services, "fs.list", &json!({"path":""}));
    let probe = ReadProbe::install(&mut services, 0);
    assert_eq!(services.call("fs.list", json!({}))["kind"], "directory");

    for (request, expected) in [
        (
            QueryRequest::List { path: "".into() },
            vec![".gitignore", "hidden.tmp", "visible.txt"],
        ),
        (
            QueryRequest::Preview {
                path: ".".into(),
                show_ignored: false,
            },
            vec![".gitignore", "visible.txt"],
        ),
        (
            QueryRequest::Preview {
                path: "./".into(),
                show_ignored: true,
            },
            vec![".gitignore", "hidden.tmp", "visible.txt"],
        ),
    ] {
        let result = services
            .cached_query(&request)
            .unwrap()
            .unwrap()
            .into_value(0);
        assert_eq!(names(&result), expected);
        assert_eq!(result["path"], request.argument()["path"]);
    }
    assert_eq!(probe.count(), 1);
}

#[test]
fn filesystem_reads_reject_stale_snapshots_before_changes_are_polled() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services();
    let request = QueryRequest::Preview {
        path: "note.txt".into(),
        show_ignored: false,
    };
    let argument = request.argument();
    let _observation = observe(&mut services, "fs.preview", &argument);
    let probe = ReadProbe::install(&mut services, 0);
    assert_eq!(services.call("fs.preview", argument.clone())["text"], "old");
    assert!(services.cached_query(&request).unwrap().is_some());
    let ready_before_change = services.start_query(request.clone()).unwrap();

    let change = |services: &NativeServices, text| {
        fs::write(fixture.0.join("note.txt"), text).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while services.cached_query(&request).unwrap().is_some() {
            assert!(
                Instant::now() < deadline,
                "change was not delivered to the watcher"
            );
            thread::sleep(Duration::from_millis(1));
        }
    };

    change(&services, "new");
    assert_eq!(services.filesystem.len(), 1);
    assert_eq!(probe.count(), 1);
    assert_eq!(ready(&mut services, ready_before_change)["text"], "new");

    change(&services, "newer");
    let started_after_change = services.start_query(request.clone()).unwrap();
    assert_eq!(ready(&mut services, started_after_change)["text"], "newer");

    change(&services, "latest");
    assert_eq!(services.call("fs.preview", argument)["text"], "latest");
}

#[test]
fn filesystem_poll_releases_cached_previews_after_the_last_observer_is_dropped() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services();
    let request = QueryRequest::Preview {
        path: "note.txt".into(),
        show_ignored: false,
    };
    let first = observe(&mut services, "fs.preview", &request.argument());
    let last = observe(&mut services, "fs.preview", &request.argument());
    assert_eq!(
        services.call("fs.preview", request.argument())["text"],
        "old"
    );

    drop(first);
    services.poll_changes().unwrap();
    assert_eq!(services.filesystem.len(), 1);
    assert!(services.cached_query(&request).unwrap().is_some());

    drop(last);
    assert!(services.cached_query(&request).unwrap().is_none());
    services.poll_changes().unwrap();
    assert_eq!(services.queries.watched_paths(), 0);
    assert_eq!(services.filesystem.len(), 0);
    assert_eq!(services.queries.len(), 0);
}

#[test]
fn unobserved_previews_release_their_snapshots_after_completion() {
    let fixture = Fixture::new();
    for path in ["synchronous.txt", "background.txt"] {
        fs::write(fixture.0.join(path), path).unwrap();
    }
    let mut services = fixture.services();

    let result = services.call("fs.preview", json!({"path":"synchronous.txt"}));
    assert_eq!(result["text"], "synchronous.txt");
    assert_eq!(services.call("service.stats", json!({}))["cached_paths"], 0);

    let handle = start(
        &mut services,
        "fs.preview",
        json!({"path":"background.txt"}),
    );
    assert_eq!(ready(&mut services, handle)["text"], "background.txt");
    assert_eq!(services.call("service.stats", json!({}))["cached_paths"], 0);
}

#[test]
fn cached_queries_preserve_filesystem_request_errors() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "text").unwrap();
    let mut services = fixture.services();
    let _observation = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    services.call("fs.preview", json!({"path":"note.txt"}));
    for request in [
        QueryRequest::List {
            path: "note.txt".into(),
        },
        QueryRequest::Preview {
            path: "note.txt/".into(),
            show_ignored: false,
        },
        QueryRequest::List { path: "../".into() },
    ] {
        assert!(services.cached_query(&request).is_err());
    }
}

#[test]
fn cached_queries_leave_pending_registrations_and_other_consumers_intact() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "new").unwrap();
    for index in 0..4 {
        fs::write(fixture.0.join(format!("{index}.txt")), index.to_string()).unwrap();
    }
    let mut services = fixture.services();
    let request = QueryRequest::Preview {
        path: "note.txt".into(),
        show_ignored: false,
    };
    let mut probe = ReadProbe::install(&mut services, 4);
    let mut handles = Vec::new();
    let mut observations = Vec::new();
    for index in 0..4 {
        let argument = json!({"path":format!("{index}.txt")});
        observations.push(observe(&mut services, "fs.preview", &argument));
        let handle = start(&mut services, "fs.preview", argument);
        probe.wait_for_read(&mut services, handle);
        handles.push(handle);
    }

    let _observation = services.observe("fs.preview", &request.argument()).unwrap();
    let cancelled = services.start_query(request.clone()).unwrap();
    let retained = services.start_query(request.clone()).unwrap();
    services.cancel_query(cancelled);
    let query = services.acquire_query(&request).unwrap();
    let registration = query.registration();
    assert_eq!(registration.ready(), Ok(false));
    let before = services.call("service.stats", json!({}));

    assert!(services.cached_query(&request).unwrap().is_none());
    assert!(Arc::ptr_eq(&registration, &query.registration()));
    assert_eq!(registration.ready(), Ok(false));
    let after = services.call("service.stats", json!({}));
    for key in ["jobs", "observed_queries", "watched_paths", "cached_paths"] {
        assert_eq!(after[key], before[key]);
    }
    assert_eq!(probe.count(), 4);

    probe.release();
    for (index, handle) in handles.into_iter().enumerate() {
        assert_eq!(ready(&mut services, handle)["text"], index.to_string());
    }
    assert_eq!(ready(&mut services, retained)["text"], "new");
    assert!(services.poll_query(cancelled).is_err());
    assert_eq!(probe.count(), 5);
}

#[test]
fn resubscribing_refreshes_caches_changed_while_the_query_was_unobserved() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services();
    let observation = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    assert_eq!(
        services.call("fs.preview", json!({"path":"note.txt"}))["text"],
        "old"
    );
    drop(observation);
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
    fs::write(fixture.0.join("note.txt"), "new").unwrap();

    let _observation = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    assert_eq!(
        services.call("fs.preview", json!({"path":"note.txt"}))["text"],
        "new"
    );
}

#[test]
fn directory_observations_follow_recreated_directories() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("folder");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("old.txt"), "old").unwrap();
    let mut services = fixture.services();
    let mut observation = observe(&mut services, "fs.list", &json!({"path":"folder"}));
    assert_eq!(
        names(&services.call("fs.list", json!({"path":"folder"}))),
        ["old.txt"]
    );

    fs::remove_dir_all(&directory).unwrap();
    wait_for_change(&mut services, &mut observation);
    assert!(services.call("fs.list", json!({"path":"folder"}))["error"].is_string());
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("new.txt"), "new").unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        names(&services.call("fs.list", json!({"path":"folder"}))),
        ["new.txt"]
    );

    fs::write(directory.join("new.txt"), "updated").unwrap();
    wait_for_query_result(
        &mut services,
        &mut observation,
        |services| services.call("fs.list", json!({"path":"folder"})),
        |listing| listing["entries"][0]["size"] == 7,
    );
}

#[test]
fn git_jobs_retry_if_their_snapshot_predates_an_observed_change() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 1);
    let handle = start(&mut services, "git.status", json!({}));
    assert_eq!(probe.wait_for_read(&mut services, handle), "git.status");
    let mut observation = observe(&mut services, "git.status", &json!({}));
    let query = services.acquire_query(&QueryRequest::Status {}).unwrap();
    let registration = query.registration();
    fs::write(fixture.0.join("note.txt"), "new\nextra\n").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while query.cache_is_current(&registration) {
        assert!(
            Instant::now() < deadline,
            "watcher did not receive the change"
        );
        thread::sleep(Duration::from_millis(1));
    }
    drop(query);
    assert!(!observation.changed());
    assert!(services.status_progress(handle).unwrap().is_none());
    probe.release();

    let result = ready(&mut services, handle);
    assert!(observation.changed());
    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    assert_eq!(result["entries"][0]["path"], "note.txt");
    assert_eq!(result["entries"][0]["worktree"], "M");
    assert_eq!(
        result["entries"][0]["line_stats"],
        json!({"additions":2,"deletions":1})
    );
    assert!(probe.count() >= 2);
    assert_eq!(
        services.call("service.stats", json!({}))["observed_queries"],
        1
    );
    drop(observation);
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
}

#[test]
fn status_lists_changed_files_before_counting_their_lines() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "old\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("note.txt"), "new\nextra\n").unwrap();
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 1);
    let handle = start(&mut services, "git.status", json!({}));
    assert_eq!(probe.wait_for_read(&mut services, handle), "git.status");
    let entries = services.status_progress(handle).unwrap().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "note.txt");
    assert_eq!(entries[0].worktree, "M");
    assert!(entries[0].line_stats.is_none());
    assert!(services.status_progress(handle).unwrap().is_none());
    assert!(matches!(
        services.poll_query(handle).unwrap(),
        super::QueryPoll::Pending
    ));
    probe.release();
    let result = ready(&mut services, handle);
    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    assert_eq!(result["entries"][0]["path"], entries[0].path);
    assert_eq!(result["entries"][0]["worktree"], entries[0].worktree);
    assert_eq!(
        result["entries"][0]["line_stats"],
        json!({"additions":2,"deletions":1})
    );
    assert_eq!(probe.count(), 1);
}

#[test]
fn cancelling_status_releases_its_unpublished_file_list() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("new.txt"), "new\n").unwrap();
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 1);
    let handle = start(&mut services, "git.status", json!({}));
    probe.wait_for_read(&mut services, handle);
    services.cancel_query(handle);
    assert_eq!(services.call("service.stats", json!({}))["jobs"], 0);
    assert!(services.status_progress(handle).is_err());
    probe.release();
}

#[test]
fn file_observations_follow_replaced_parent_directories() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("folder");
    let replacement = fixture.0.join("replacement");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("note.txt"), "old").unwrap();
    let mut services = fixture.services();
    let mut observation = observe(
        &mut services,
        "fs.preview",
        &json!({"path":"folder/note.txt"}),
    );
    assert_eq!(
        services.call("fs.preview", json!({"path":"folder/note.txt"}))["text"],
        "old"
    );

    fs::create_dir(&replacement).unwrap();
    fs::write(replacement.join("note.txt"), "new").unwrap();
    fs::rename(&directory, fixture.0.join("previous")).unwrap();
    fs::rename(&replacement, &directory).unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        services.call("fs.preview", json!({"path":"folder/note.txt"}))["text"],
        "new"
    );

    fs::write(directory.join("note.txt"), "updated").unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        services.call("fs.preview", json!({"path":"folder/note.txt"}))["text"],
        "updated"
    );
}

#[cfg(unix)]
#[test]
fn file_observations_follow_retargeted_symbolic_link_directories() {
    let fixture = Fixture::new();
    for (directory, text) in [("first", "old"), ("second", "new")] {
        fs::create_dir(fixture.0.join(directory)).unwrap();
        fs::write(fixture.0.join(directory).join("note.txt"), text).unwrap();
    }
    std::os::unix::fs::symlink("first", fixture.0.join("alias")).unwrap();
    let mut services = fixture.services();
    let mut observation = observe(
        &mut services,
        "fs.preview",
        &json!({"path":"alias/note.txt"}),
    );
    assert_eq!(
        services.call("fs.preview", json!({"path":"alias/note.txt"}))["text"],
        "old"
    );

    std::os::unix::fs::symlink("second", fixture.0.join("next-alias")).unwrap();
    fs::rename(fixture.0.join("next-alias"), fixture.0.join("alias")).unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        services.call("fs.preview", json!({"path":"alias/note.txt"}))["text"],
        "new"
    );

    fs::write(fixture.0.join("second/note.txt"), "updated").unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        services.call("fs.preview", json!({"path":"alias/note.txt"}))["text"],
        "updated"
    );
}

#[cfg(unix)]
#[test]
fn directory_observations_follow_retargeted_symbolic_links() {
    let fixture = Fixture::new();
    for directory in ["first", "second"] {
        fs::create_dir(fixture.0.join(directory)).unwrap();
    }
    fs::write(fixture.0.join("first/old.txt"), "old").unwrap();
    fs::write(fixture.0.join("second/new.txt"), "new").unwrap();
    std::os::unix::fs::symlink("first", fixture.0.join("alias")).unwrap();
    let mut services = fixture.services();
    let mut observation = observe(&mut services, "fs.list", &json!({"path":"alias"}));
    assert_eq!(
        names(&services.call("fs.list", json!({"path":"alias"}))),
        ["old.txt"]
    );

    std::os::unix::fs::symlink("second", fixture.0.join("next-alias")).unwrap();
    fs::rename(fixture.0.join("next-alias"), fixture.0.join("alias")).unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        names(&services.call("fs.list", json!({"path":"alias"}))),
        ["new.txt"]
    );

    fs::write(fixture.0.join("second/later.txt"), "later").unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        names(&services.call("fs.list", json!({"path":"alias"}))),
        ["later.txt", "new.txt"]
    );
}

#[cfg(unix)]
#[test]
fn file_observations_follow_retargeted_intermediate_symbolic_links() {
    let fixture = Fixture::new();
    for (directory, text) in [("first", "old"), ("second", "new")] {
        fs::create_dir(fixture.0.join(directory)).unwrap();
        fs::write(fixture.0.join(directory).join("note.txt"), text).unwrap();
    }
    fs::create_dir(fixture.0.join("links")).unwrap();
    std::os::unix::fs::symlink("../first", fixture.0.join("links/current")).unwrap();
    std::os::unix::fs::symlink("links/current", fixture.0.join("alias")).unwrap();
    let mut services = fixture.services();
    let mut observation = observe(
        &mut services,
        "fs.preview",
        &json!({"path":"alias/note.txt"}),
    );
    assert_eq!(
        services.call("fs.preview", json!({"path":"alias/note.txt"}))["text"],
        "old"
    );

    std::os::unix::fs::symlink("../second", fixture.0.join("links/next")).unwrap();
    fs::rename(
        fixture.0.join("links/next"),
        fixture.0.join("links/current"),
    )
    .unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        services.call("fs.preview", json!({"path":"alias/note.txt"}))["text"],
        "new"
    );
}

#[test]
fn diff_files_keep_requested_order_and_include_untracked_contents() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("tracked.txt"), "baseline\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("tracked.txt"), "changed\n").unwrap();
    fs::write(fixture.0.join("new\tfile.txt"), "untracked\n").unwrap();
    let mut services = fixture.services();
    let source = json!({"kind":"changes", "side":"unstaged"});
    let files = json!([
        {"path":"new\tfile.txt", "original_path":null},
        {"path":"tracked.txt", "original_path":null}
    ]);
    let handle = start(
        &mut services,
        "git.diff-files",
        json!({"source":source, "files":files}),
    );
    let result = ready(&mut services, handle);
    assert_eq!(result["kind"], "git_diff");
    assert_eq!(result["diff"]["source"], source);
    let diffs = result["diff"]["files"].as_array().unwrap();
    assert_eq!(diffs.len(), 2);
    for index in 0..diffs.len() {
        assert_eq!(diffs[index]["target"], files[index]);
    }
    assert_eq!(
        hunk_lines(diffs[0]["raw"].as_str().unwrap()),
        ["@@ -0,0 +1 @@", "+untracked"]
    );
    assert_eq!(
        hunk_lines(diffs[1]["raw"].as_str().unwrap()),
        ["@@ -1 +1 @@", "-baseline", "+changed"]
    );
}

#[test]
fn changing_diff_attributes_refreshes_the_configured_rendering() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "raw-old\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("note.txt"), "raw-new\n").unwrap();
    git(&fixture.0, &["config", "diff.external", ""]);
    git(&fixture.0, &["config", "pager.diff", "false"]);
    git(
        &fixture.0,
        &["config", "diff.converted.textconv", "sed s/raw/converted/g"],
    );
    let argument = json!({
        "source":{"kind":"changes", "side":"unstaged"},
        "files":[{"path":"note.txt", "original_path":null}],
    });
    let mut services = fixture.services();
    let mut observation = observe(&mut services, "git.diff-files", &argument);
    let before = services.call("git.diff-files", argument.clone());
    assert!(before["diff"]["files"][0]["rendered"].is_null());
    fs::write(
        fixture.0.join(".gitattributes"),
        "note.txt diff=converted\n",
    )
    .unwrap();
    wait_for_change(&mut services, &mut observation);
    let after = services.call("git.diff-files", argument);
    assert_eq!(
        after["diff"]["files"][0]["raw"],
        before["diff"]["files"][0]["raw"]
    );
    let rendered = after["diff"]["files"][0]["rendered"]["text"]
        .as_str()
        .unwrap();
    let lines = zess_ui::AnsiText::parse(rendered)
        .rows
        .into_iter()
        .map(|row| {
            row.spans
                .into_iter()
                .map(|span| span.text)
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(lines.iter().any(|line| line == "-converted-old"));
    assert!(lines.iter().any(|line| line == "+converted-new"));
}

#[test]
fn commit_diff_files_preserve_rename_targets_without_filesystem_watches() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("old.txt"), "baseline\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    git(&fixture.0, &["mv", "old.txt", "renamed.txt"]);
    fs::write(fixture.0.join("added.txt"), "historical\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "rename and add"]);
    let mut services = fixture.services();
    let oid = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    fs::write(fixture.0.join("renamed.txt"), "working tree\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("missing", fixture.0.join("old.txt")).unwrap();
    let source = json!({"kind":"commit", "commit":oid});
    let files = json!([
        {"path":"renamed.txt", "original_path":"old.txt"},
        {"path":"added.txt", "original_path":null}
    ]);
    let request = json!({"source":source, "files":files});
    let _observation = observe(&mut services, "git.diff-files", &request);
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
    let handle = start(&mut services, "git.diff-files", request);
    let result = ready(&mut services, handle);
    assert_eq!(result["diff"]["source"], source);
    let diffs = result["diff"]["files"].as_array().unwrap();
    assert_eq!(diffs.len(), 2);
    assert_eq!(diffs[0]["target"], files[0]);
    let rename = diffs[0]["raw"].as_str().unwrap();
    assert!(rename.lines().any(|line| line == "rename from old.txt"));
    assert!(rename.lines().any(|line| line == "rename to renamed.txt"));
    assert!(!rename.contains("working tree"));
    assert_eq!(diffs[1]["target"], files[1]);
    assert_eq!(
        hunk_lines(diffs[1]["raw"].as_str().unwrap()),
        ["@@ -0,0 +1 @@", "+historical"]
    );
}

#[test]
fn diff_files_observe_all_targets_original_paths_and_index_changes() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    for path in ["old.txt", "other.txt"] {
        fs::write(fixture.0.join(path), "baseline\n").unwrap();
    }
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    git(&fixture.0, &["mv", "old.txt", "renamed.txt"]);
    let mut services = fixture.services();
    let request = json!({
        "source":{"kind":"changes", "side":"unstaged"},
        "files":[
            {"path":"renamed.txt", "original_path":"old.txt"},
            {"path":"other.txt", "original_path":null}
        ]
    });
    let mut observation = observe(&mut services, "git.diff-files", &request);
    for path in ["renamed.txt", "other.txt", "old.txt"] {
        fs::write(fixture.0.join(path), "changed\n").unwrap();
        wait_for_change(&mut services, &mut observation);
    }
    git(&fixture.0, &["add", "."]);
    wait_for_change(&mut services, &mut observation);
    drop(observation);
    let deadline = Instant::now() + TIMEOUT;
    while services.call("service.stats", json!({}))["watched_paths"] != 0 {
        services.poll_changes().unwrap();
        assert!(Instant::now() < deadline, "diff watches were not released");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn diff_files_observations_distinguish_source_order_and_rename_identity() {
    use zess_git::{DiffSide, DiffSource, DiffTarget};

    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    let mut services = fixture.services();
    let request = QueryRequest::DiffFiles {
        render: zess_git::DiffRenderOptions::default(),
        source: DiffSource::Changes {
            side: DiffSide::Unstaged,
        },
        files: vec![
            DiffTarget {
                path: "first.txt".into(),
                original_path: None,
            },
            DiffTarget {
                path: "second.txt".into(),
                original_path: None,
            },
        ],
    };
    let original = services.acquire_query(&request).unwrap();
    assert!(Arc::ptr_eq(
        &original,
        &services.acquire_query(&request).unwrap()
    ));
    let QueryRequest::DiffFiles { source, files, .. } = request else {
        unreachable!();
    };
    let mut reversed = files.clone();
    reversed.reverse();
    let mut renamed = files.clone();
    renamed[0].original_path = Some("old.txt".into());
    for request in [
        QueryRequest::DiffFiles {
            render: zess_git::DiffRenderOptions::default(),
            source: DiffSource::Changes {
                side: DiffSide::Staged,
            },
            files: files.clone(),
        },
        QueryRequest::DiffFiles {
            render: zess_git::DiffRenderOptions::default(),
            source: DiffSource::Commit {
                commit: "a".repeat(40),
            },
            files,
        },
        QueryRequest::DiffFiles {
            render: zess_git::DiffRenderOptions::default(),
            source: source.clone(),
            files: reversed,
        },
        QueryRequest::DiffFiles {
            render: zess_git::DiffRenderOptions::default(),
            source,
            files: renamed,
        },
    ] {
        assert!(!Arc::ptr_eq(
            &original,
            &services.acquire_query(&request).unwrap()
        ));
    }
}

#[test]
fn diff_file_observations_share_filesystem_watches_across_display_widths() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    let mut services = fixture.services();
    let mut request = QueryRequest::DiffFiles {
        source: zess_git::DiffSource::Changes {
            side: zess_git::DiffSide::Unstaged,
        },
        files: vec![zess_git::DiffTarget {
            path: "note.txt".into(),
            original_path: None,
        }],
        render: zess_git::DiffRenderOptions {
            file_header: false,
            width: Some(44),
            pager: None,
        },
    };
    let original = services.acquire_query(&request).unwrap();
    let QueryRequest::DiffFiles { render, .. } = &mut request else {
        unreachable!();
    };
    render.width = Some(120);
    assert!(Arc::ptr_eq(
        &original,
        &services.acquire_query(&request).unwrap()
    ));
}

#[test]
fn asynchronous_diff_renderers_receive_the_requested_view_width() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "old\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("note.txt"), "new\n").unwrap();
    fs::write(
        fixture.0.join("renderer.sh"),
        "printf 'external:%s:%s\\n' \"$COLUMNS\" \"$DFT_WIDTH\"\n",
    )
    .unwrap();
    git(&fixture.0, &["config", "diff.external", "sh ./renderer.sh"]);
    git(&fixture.0, &["config", "pager.diff", "false"]);
    let mut services = fixture.services();
    let request = |width| {
        json!({
            "source":{"kind":"changes", "side":"unstaged"},
            "files":[{"path":"note.txt", "original_path":null}],
            "render":{"width":width},
        })
    };
    let narrow = start(&mut services, "git.diff-files", request(44));
    let wide = start(&mut services, "git.diff-files", request(120));
    let narrow = ready(&mut services, narrow);
    let wide = ready(&mut services, wide);
    for (result, expected) in [(&narrow, "external:44:44\n"), (&wide, "external:120:120\n")] {
        assert_eq!(result["diff"]["files"][0]["rendered"]["text"], expected);
    }
    assert_eq!(
        narrow["diff"]["files"][0]["raw"],
        wide["diff"]["files"][0]["raw"]
    );
    assert!(
        wide["diff"]["files"][0]["raw"]
            .as_str()
            .unwrap()
            .contains("-old\n+new\n")
    );
}

#[test]
fn zero_diff_width_is_rejected() {
    let fixture = Fixture::new();
    let mut services = fixture.services();
    let result = services.call(
        "git.diff-files",
        json!({
            "source":{"kind":"changes", "side":"unstaged"},
            "files":[],
            "render":{"width":0},
        }),
    );
    assert_eq!(result["error"], "diff width must be positive");
}

#[test]
fn an_explicit_diff_pager_replaces_the_configured_external_renderer() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "old\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("note.txt"), "new\n").unwrap();
    fs::write(
        fixture.0.join("renderer.sh"),
        "touch external-started\nprintf 'external\\n'\n",
    )
    .unwrap();
    git(&fixture.0, &["config", "diff.external", "sh ./renderer.sh"]);
    git(&fixture.0, &["config", "pager.diff", "false"]);
    let mut services = fixture.services();
    let mut request = json!({
        "source":{"kind":"changes", "side":"unstaged"},
        "files":[{"path":"note.txt", "original_path":null}],
        "render":{"width":120, "pager":{"program":"sh", "args":["-c", "printf 'override:%s\\n' \"$COLUMNS\"; cat"]}},
    });
    let rendered = services.call("git.diff-files", request.clone());
    let file = &rendered["diff"]["files"][0];
    let raw = file["raw"].as_str().unwrap();
    assert_eq!(file["rendered"]["text"], format!("override:120\n{raw}"));
    assert!(!fixture.0.join("external-started").exists());
    request["render"]["pager"] = json!({"program":"cat", "args":[]});
    let plain = services.call("git.diff-files", request.clone());
    assert_eq!(plain["diff"]["files"][0]["rendered"]["text"], raw);
    assert_eq!(plain["diff"]["files"][0]["raw"], raw);
    assert!(!fixture.0.join("external-started").exists());
    request["render"]["pager"] = serde_json::Value::Null;
    let automatic = services.call("git.diff-files", request);
    assert_eq!(
        automatic["diff"]["files"][0]["rendered"]["text"],
        "external\n"
    );
    assert_eq!(automatic["diff"]["files"][0]["raw"], raw);
    assert!(fixture.0.join("external-started").exists());
}

#[test]
fn empty_diff_pager_commands_are_rejected() {
    let fixture = Fixture::new();
    let mut services = fixture.services();
    for pager in ["", " \n "] {
        let result = services.call(
            "git.diff-files",
            json!({
                "source":{"kind":"changes", "side":"unstaged"},
                "files":[],
                "render":{"pager":{"program":pager, "args":[]}},
            }),
        );
        assert_eq!(result["error"], "diff pager program must not be empty");
    }
}

#[test]
fn diff_observations_ignore_other_files_and_keep_watching_after_status_is_released() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old\n").unwrap();
    fs::write(fixture.0.join("other.txt"), "old\n").unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    let mut services = fixture.services();
    let mut diff = observe(&mut services, "git.diff", &json!({"path":"note.txt"}));
    let mut status = observe(&mut services, "git.status", &json!({}));
    fs::write(fixture.0.join("other.txt"), "changed\n").unwrap();
    wait_for_change(&mut services, &mut status);
    assert!(!diff.changed());
    drop(status);

    fs::write(fixture.0.join("replacement.txt"), "new\n").unwrap();
    fs::rename(
        fixture.0.join("replacement.txt"),
        fixture.0.join("note.txt"),
    )
    .unwrap();
    wait_for_change(&mut services, &mut diff);
    let result = services.call("git.diff", json!({"path":"note.txt"}));
    assert_eq!(result["path"], "note.txt");
    assert_eq!(
        hunk_lines(result["text"].as_str().unwrap()),
        ["@@ -1 +1 @@", "-old", "+new"]
    );
    drop(diff);
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
}

#[test]
fn external_changes_replace_inflight_previews_without_publishing_old_contents() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 1);
    let request = QueryRequest::Preview {
        path: "note.txt".into(),
        show_ignored: false,
    };
    let original = services.start_query(request.clone()).unwrap();
    probe.wait_for_read(&mut services, original);
    let mut observation = observe(&mut services, "fs.preview", &request.argument());
    let query = services.acquire_query(&request).unwrap();
    let registration = query.registration();

    fs::write(fixture.0.join("note.txt"), "new").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while query.cache_is_current(&registration) {
        assert!(
            Instant::now() < deadline,
            "change was not delivered to the watcher"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(!observation.changed());
    let fresh = services.start_query(request).unwrap();
    assert_eq!(ready(&mut services, fresh)["text"], "new");
    probe.release();
    probe.resumed.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(ready(&mut services, original)["text"], "new");
    assert_eq!(
        services.call("fs.preview", json!({"path":"note.txt"}))["text"],
        "new"
    );
    assert!(probe.count() >= 2);
}

#[test]
fn git_lock_files_notify_status_observers_only_when_published_to_metadata() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    let mut services = fixture.services();
    let mut observation = observe(&mut services, "git.status", &json!({}));
    let _preview = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    assert_eq!(
        services.call("fs.preview", json!({"path":"note.txt"}))["text"],
        "old"
    );

    fs::write(fixture.0.join(".git/index.lock"), "index").unwrap();
    let deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < deadline {
        services.poll_changes().unwrap();
        assert!(!observation.changed());
        thread::sleep(Duration::from_millis(5));
    }

    fs::rename(
        fixture.0.join(".git/index.lock"),
        fixture.0.join(".git/index"),
    )
    .unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(services.call("service.stats", json!({}))["cached_paths"], 1);
}

#[test]
fn external_changes_to_workspace_lock_files_refresh_cached_previews() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("Cargo.lock"), "old").unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    let mut services = fixture.services();
    let mut observation = observe(&mut services, "fs.preview", &json!({"path":"Cargo.lock"}));
    assert_eq!(
        services.call("fs.preview", json!({"path":"Cargo.lock"}))["text"],
        "old"
    );

    fs::write(fixture.0.join("Cargo.lock"), "new").unwrap();
    wait_for_change(&mut services, &mut observation);
    assert_eq!(
        services.call("fs.preview", json!({"path":"Cargo.lock"}))["text"],
        "new"
    );
}

#[test]
fn ancestor_ignore_changes_refresh_directory_previews() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("subdir");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("ignored-later.txt"), "generated").unwrap();
    fs::write(directory.join("visible.txt"), "visible").unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    let mut services = NativeServices::new(directory).unwrap();
    let mut observation = observe(&mut services, "fs.preview", &json!({"path":""}));
    let preview = |services: &mut NativeServices| services.call("fs.preview", json!({"path":""}));
    assert_eq!(
        names(&preview(&mut services)),
        ["ignored-later.txt", "visible.txt"]
    );

    fs::write(fixture.0.join("unrelated.txt"), "unrelated").unwrap();
    let deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < deadline {
        services.poll_changes().unwrap();
        assert!(!observation.changed());
        thread::sleep(Duration::from_millis(5));
    }

    fs::write(fixture.0.join(".gitignore"), "ignored-later.txt\n").unwrap();
    wait_for_query_result(&mut services, &mut observation, preview, |result| {
        names(result) == ["visible.txt"]
    });

    fs::write(fixture.0.join(".gitignore"), "").unwrap();
    wait_for_query_result(&mut services, &mut observation, preview, |result| {
        names(result) == ["ignored-later.txt", "visible.txt"]
    });

    fs::write(fixture.0.join("next-ignore"), "ignored-later.txt\n").unwrap();
    fs::rename(fixture.0.join("next-ignore"), fixture.0.join(".gitignore")).unwrap();
    wait_for_query_result(&mut services, &mut observation, preview, |result| {
        names(result) == ["visible.txt"]
    });

    fs::remove_file(fixture.0.join(".gitignore")).unwrap();
    wait_for_query_result(&mut services, &mut observation, preview, |result| {
        names(result) == ["ignored-later.txt", "visible.txt"]
    });
}

#[test]
fn ignore_rules_above_the_repository_notify_directory_observers_but_not_git_status() {
    let fixture = Fixture::new();
    let repository = fixture.0.join("repository");
    let directory = repository.join("subdir");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("ignored-later.txt"), "generated").unwrap();
    fs::write(directory.join("visible.txt"), "visible").unwrap();
    git(&repository, &["init", "--quiet"]);
    let mut services = NativeServices::new(directory).unwrap();
    let mut preview = observe(&mut services, "fs.preview", &json!({"path":""}));
    let mut status = observe(&mut services, "git.status", &json!({}));
    assert_eq!(
        names(&services.call("fs.preview", json!({"path":""}))),
        ["ignored-later.txt", "visible.txt"]
    );

    fs::write(fixture.0.join(".gitignore"), "ignored-later.txt\n").unwrap();
    wait_for_change(&mut services, &mut preview);
    assert!(!status.changed());
    assert_eq!(
        names(&services.call("fs.preview", json!({"path":""}))),
        ["visible.txt"]
    );
    assert_eq!(
        services.call("git.status", json!({}))["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    fs::write(repository.join(".gitignore"), "visible.txt\n").unwrap();
    wait_for_change(&mut services, &mut status);
    wait_for_change(&mut services, &mut preview);
    assert!(names(&services.call("fs.preview", json!({"path":""}))).is_empty());
    let entries = services.call("git.status", json!({}));
    assert_eq!(entries["entries"].as_array().unwrap().len(), 1);
    assert_eq!(entries["entries"][0]["path"], "ignored-later.txt");
}

#[test]
fn external_worktree_commits_notify_status_observers_opened_in_a_subdirectory() {
    let fixture = Fixture::new();
    let repository = fixture.0.join("repository");
    let linked = fixture.0.join("linked");
    fs::create_dir_all(repository.join("folder")).unwrap();
    fs::write(repository.join("folder/note.txt"), "text").unwrap();
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "baseline"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let mut services = NativeServices::new(linked.join("folder")).unwrap();
    let mut observation = observe(&mut services, "git.status", &json!({}));
    let _preview = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    assert_eq!(
        services.call("fs.preview", json!({"path":"note.txt"}))["text"],
        "text"
    );

    git(
        &linked,
        &["commit", "--quiet", "--allow-empty", "-m", "external"],
    );
    wait_for_change(&mut services, &mut observation);
    assert_eq!(services.call("service.stats", json!({}))["cached_paths"], 1);
}

#[test]
fn reading_files_and_git_status_does_not_trigger_filesystem_invalidation() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "text").unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    let mut services = fixture.services();
    let mut observation = observe(&mut services, "git.status", &json!({}));
    let mut preview = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    fs::write(fixture.0.join("note.txt"), "text").unwrap();
    wait_for_change(&mut services, &mut observation);
    wait_for_change(&mut services, &mut preview);
    let settled = Instant::now() + Duration::from_millis(150);
    while Instant::now() < settled {
        services.poll_changes().unwrap();
        observation.changed();
        observation.acknowledge();
        preview.changed();
        preview.acknowledge();
        thread::sleep(Duration::from_millis(5));
    }
    let index = fixture.0.join(".git/index");
    let index_contents = fs::read(&index).unwrap();
    let index_modified = fs::metadata(&index).unwrap().modified().unwrap();

    let deadline = Instant::now() + Duration::from_millis(250);

    while Instant::now() < deadline {
        assert!(services.call("git.status", json!({}))["error"].is_null());
        assert_eq!(
            services.call("fs.preview", json!({"path":"note.txt"}))["text"],
            "text"
        );
        services.poll_changes().unwrap();
        assert!(!observation.changed());
        assert!(!preview.changed());
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read(&index).unwrap(), index_contents);
    assert_eq!(
        fs::metadata(index).unwrap().modified().unwrap(),
        index_modified
    );
}

#[test]
fn directory_previews_do_not_remove_ignored_entries_from_listings() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join(".gitignore"), "*.tmp\n").unwrap();
    fs::write(fixture.0.join("visible.txt"), "visible").unwrap();
    fs::write(fixture.0.join("hidden.tmp"), "ignored").unwrap();
    let mut services = fixture.services();
    let preview = services.call("fs.preview", json!({"path":""}));
    assert_eq!(preview["kind"], "directory");
    assert_eq!(names(&preview), ["folder", ".gitignore", "visible.txt"]);
    let listing = services.call("fs.list", json!({"path":""}));
    assert_eq!(
        names(&listing),
        ["folder", ".gitignore", "hidden.tmp", "visible.txt"]
    );
    let visible = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "visible.txt")
        .unwrap();
    assert_eq!(visible["size"], 7);
    let preview = services.call("fs.preview", json!({"path":"", "show_ignored":true}));
    assert_eq!(preview["entries"], listing["entries"]);
}

#[test]
fn bounded_previews_preserve_utf8_and_report_unsupported_content() {
    let fixture = Fixture::new();
    let mut text = "a".repeat(1024 * 1024 - 1);
    text.push('中');
    fs::write(fixture.0.join("large.txt"), text).unwrap();
    fs::write(fixture.0.join("binary.bin"), b"a\0b").unwrap();
    fs::write(fixture.0.join("image.svg"), "<svg/>").unwrap();
    let mut services = fixture.services();
    let result = services.call("fs.preview", json!({"path":"large.txt"}));
    assert_eq!(result["kind"], "text");
    assert_eq!(result["text"].as_str().unwrap().len(), 1024 * 1024 - 1);
    assert_eq!(result["truncated"], true);
    for path in ["binary.bin", "image.svg"] {
        let result = services.call("fs.preview", json!({"path":path}));
        assert_eq!(result["kind"], "unsupported");
    }
}

#[test]
fn concurrent_listings_and_previews_share_a_read_and_keep_their_own_display_options() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join(".gitignore"), "*.tmp\n").unwrap();
    fs::write(fixture.0.join("hidden.tmp"), "ignored").unwrap();
    fs::write(fixture.0.join("visible.txt"), "visible").unwrap();
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 1);
    let listing = start(&mut services, "fs.list", json!({"path":""}));
    assert_eq!(probe.wait_for_read(&mut services, listing), "");
    let preview = start(&mut services, "fs.preview", json!({"path":"."}));
    let unfiltered = start(
        &mut services,
        "fs.preview",
        json!({"path":"./", "show_ignored":true}),
    );
    probe.release();

    let listing_result = ready(&mut services, listing);
    let preview_result = ready(&mut services, preview);
    let unfiltered_result = ready(&mut services, unfiltered);
    assert_eq!(
        names(&listing_result),
        [".gitignore", "hidden.tmp", "visible.txt"]
    );
    assert_eq!(names(&preview_result), [".gitignore", "visible.txt"]);
    assert_eq!(names(&unfiltered_result), names(&listing_result));
    for (result, handle, path) in [
        (&listing_result, listing, ""),
        (&preview_result, preview, "."),
        (&unfiltered_result, unfiltered, "./"),
    ] {
        assert_eq!(result["handle"], handle);
        assert_eq!(result["path"], path);
    }
    assert_eq!(probe.count(), 1);
}

#[test]
fn cancelling_one_subscriber_preserves_the_other_subscribers_read() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "shared").unwrap();
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 1);
    let cancelled = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    assert_eq!(probe.wait_for_read(&mut services, cancelled), "note.txt");
    let retained = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    assert_eq!(
        services.call("job.cancel", json!({"handle":cancelled}))["status"],
        "cancelled"
    );
    probe.release();

    let result = ready(&mut services, retained);
    assert_eq!(result["text"], "shared");
    assert_eq!(result["handle"], retained);
    assert!(services.call("job.poll", json!({"handle":cancelled}))["error"].is_string());
    assert_eq!(probe.count(), 1);
}

#[test]
fn subscribing_after_the_last_cancellation_starts_a_fresh_read() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services_without_watch_events();
    let mut probe = ReadProbe::install(&mut services, 1);
    let _observation = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    let cancelled = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    probe.wait_for_read(&mut services, cancelled);
    services.call("job.cancel", json!({"handle":cancelled}));
    fs::write(fixture.0.join("note.txt"), "new").unwrap();

    let fresh = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    assert_eq!(ready(&mut services, fresh)["text"], "new");
    probe.release();
    probe.resumed.recv_timeout(TIMEOUT).unwrap();
    let cached = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    assert_eq!(ready(&mut services, cached)["text"], "new");
    assert_eq!(probe.count(), 2);
}

#[test]
fn invalidation_keeps_subscribers_and_discards_a_late_old_snapshot() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services_without_watch_events();
    let mut probe = ReadProbe::install(&mut services, 1);
    let _observation = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    let original = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    probe.wait_for_read(&mut services, original);
    fs::write(fixture.0.join("note.txt"), "new").unwrap();
    assert_eq!(services.call("fs.invalidate", json!({}))["revision"], 1);

    let fresh = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    assert_eq!(ready(&mut services, fresh)["text"], "new");
    probe.release();
    probe.resumed.recv_timeout(TIMEOUT).unwrap();
    let result = ready(&mut services, original);
    assert_eq!(result["text"], "new");
    assert_eq!(result["handle"], original);
    assert_eq!(
        services.call("fs.preview", json!({"path":"note.txt"}))["text"],
        "new"
    );
    assert_eq!(probe.count(), 2);
}

#[test]
fn invalidation_refreshes_a_completed_subscription_that_has_not_been_polled() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services_without_watch_events();
    let probe = ReadProbe::install(&mut services, 0);
    let first = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    let unpolled = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    assert_eq!(ready(&mut services, first)["text"], "old");
    fs::write(fixture.0.join("note.txt"), "new").unwrap();
    services.call("fs.invalidate", json!({}));

    let result = ready(&mut services, unpolled);
    assert_eq!(result["text"], "new");
    assert_eq!(result["handle"], unpolled);
    assert_eq!(probe.count(), 2);
}

#[test]
fn synchronous_previews_wait_for_the_existing_background_read() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "shared").unwrap();
    thread::scope(|scope| {
        let mut services = fixture.services();
        let mut probe = ReadProbe::install(&mut services, 1);
        let handle = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
        probe.wait_for_read(&mut services, handle);
        let (started_sender, started) = mpsc::channel();
        let (result_sender, result) = mpsc::channel();
        let reader = scope.spawn(move || {
            started_sender.send(()).unwrap();
            let preview = services.call("fs.preview", json!({"path":"note.txt"}));
            let _ = result_sender.send(preview);
            services
        });
        started.recv_timeout(TIMEOUT).unwrap();
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        probe.release();
        let synchronous = result.recv_timeout(TIMEOUT).unwrap();
        let mut services = reader.join().unwrap();
        assert_eq!(synchronous["text"], "shared");
        let asynchronous = ready(&mut services, handle);
        assert_eq!(asynchronous["text"], "shared");
        assert_eq!(asynchronous["handle"], handle);
        assert_ne!(synchronous["handle"], asynchronous["handle"]);
        assert_eq!(probe.count(), 1);
    });
}

#[test]
fn applying_a_directory_rename_refreshes_previews_and_listing_metadata() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("before")).unwrap();
    fs::write(fixture.0.join("before/note.txt"), "old").unwrap();
    let mut services = fixture.services();
    assert_eq!(
        names(&services.call("fs.list", json!({"path":""}))),
        ["before"]
    );
    assert_eq!(
        services.call("fs.list", json!({"path":"before"}))["entries"][0]["size"],
        3
    );
    assert_eq!(
        services.call("fs.preview", json!({"path":"before/note.txt"}))["text"],
        "old"
    );
    fs::write(fixture.0.join("before/note.txt"), "updated text").unwrap();
    let applied = services.call(
        "fs.apply",
        json!({"operations":[{"kind":"rename", "path":"before", "to":"after"}]}),
    );
    assert_eq!(applied["remaining"], json!([]));
    assert_eq!(applied["applied"].as_array().unwrap().len(), 1);

    let root = services.call("fs.list", json!({"path":""}));
    assert_eq!(names(&root), ["after"]);
    assert_eq!(root["entries"][0]["kind"], "directory");
    let directory = services.call("fs.preview", json!({"path":"after"}));
    assert_eq!(directory["entries"][0]["path"], "after/note.txt");
    assert_eq!(directory["entries"][0]["size"], 12);
    assert_eq!(
        services.call("fs.preview", json!({"path":"after/note.txt"}))["text"],
        "updated text"
    );
    assert!(services.call("fs.preview", json!({"path":"before/note.txt"}))["error"].is_string());
}

#[test]
fn filesystem_subscriptions_retry_when_the_worker_queue_is_full() {
    let fixture = Fixture::new();
    for index in 0..44 {
        fs::write(fixture.0.join(format!("{index}.txt")), index.to_string()).unwrap();
    }
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 4);
    let _observations = (0..44)
        .map(|index| {
            observe(
                &mut services,
                "fs.preview",
                &json!({"path":format!("{index}.txt")}),
            )
        })
        .collect::<Vec<_>>();
    let mut handles = Vec::new();
    for index in 0..4 {
        handles.push(start(
            &mut services,
            "fs.preview",
            json!({"path":format!("{index}.txt")}),
        ));
        probe.wait_for_read(&mut services, *handles.last().unwrap());
    }
    for index in 4..44 {
        handles.push(start(
            &mut services,
            "fs.preview",
            json!({"path":format!("{index}.txt")}),
        ));
    }
    assert_eq!(
        services.call("job.poll", json!({"handle":handles.last().unwrap()}))["status"],
        "pending"
    );
    probe.release();

    for (index, handle) in handles.into_iter().enumerate() {
        let result = ready(&mut services, handle);
        assert_eq!(result["text"], index.to_string());
        assert_eq!(result["handle"], handle);
    }
    assert_eq!(probe.count(), 44);
}

#[test]
fn git_queries_wait_for_worker_capacity_without_publishing_progress() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    for index in 0..36 {
        fs::write(fixture.0.join(format!("{index}.txt")), "text").unwrap();
    }
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 4);
    let _git = observe(&mut services, "git.status", &json!({}));
    let _files = (0..36)
        .map(|index| {
            observe(
                &mut services,
                "fs.preview",
                &json!({"path":format!("{index}.txt")}),
            )
        })
        .collect::<Vec<_>>();
    for index in 0..36 {
        let handle = start(
            &mut services,
            "fs.preview",
            json!({"path":format!("{index}.txt")}),
        );
        if index < 4 {
            probe.wait_for_read(&mut services, handle);
        }
    }
    let handle = start(&mut services, "git.status", json!({}));
    assert!(matches!(
        services.poll_query(handle).unwrap(),
        super::QueryPoll::Pending
    ));
    assert!(services.status_progress(handle).unwrap().is_none());

    probe.release();
    let status = ready(&mut services, handle);
    assert_eq!(status["entries"].as_array().unwrap().len(), 36);
    assert!(services.status_progress(handle).is_err());
}

#[test]
fn cached_previews_preserve_directory_requirements_in_path_aliases() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "text").unwrap();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join("folder/child.txt"), "child").unwrap();
    let mut services = fixture.services();
    assert_eq!(
        services.call("fs.preview", json!({"path":"note.txt"}))["text"],
        "text"
    );

    for path in ["note.txt/", "note.txt/."] {
        let synchronous = services.call("fs.preview", json!({"path":path}));
        assert_eq!(synchronous["error"], format!("not a directory: {path}"));
        let handle = start(&mut services, "fs.preview", json!({"path":path}));
        let deadline = Instant::now() + TIMEOUT;
        let asynchronous = loop {
            let result = services.call("job.poll", json!({"handle":handle}));
            if result["status"] == "ready" {
                break result["result"].clone();
            }
            assert_eq!(result["status"], "pending", "{result}");
            assert!(Instant::now() < deadline, "job {handle} timed out");
            thread::yield_now();
        };
        assert_eq!(asynchronous["error"], format!("not a directory: {path}"));
    }

    assert_eq!(
        services.call("fs.preview", json!({"path":"./note.txt"}))["text"],
        "text"
    );
    let file = start(&mut services, "fs.preview", json!({"path":"./note.txt"}));
    assert_eq!(ready(&mut services, file)["text"], "text");
    for path in ["folder/", "folder/."] {
        let synchronous = services.call("fs.preview", json!({"path":path}));
        assert_eq!(synchronous["kind"], "directory");
        assert_eq!(names(&synchronous), ["child.txt"]);
        let handle = start(&mut services, "fs.preview", json!({"path":path}));
        let asynchronous = ready(&mut services, handle);
        assert_eq!(asynchronous["kind"], "directory");
        assert_eq!(names(&asynchronous), ["child.txt"]);
    }
}

#[test]
fn historical_previews_read_commit_objects_instead_of_working_files() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    let path = "note\nname.txt";
    fs::write(fixture.0.join(path), "original snapshot\n").unwrap();
    git(&fixture.0, &["add", "--", path]);
    git(&fixture.0, &["commit", "--quiet", "-m", "initial\n\nbody"]);
    let mut services = fixture.services();
    let log = services.call("git.log", json!({}));
    let oid = log["commits"][0]["oid"].as_str().unwrap().to_owned();
    assert_eq!(log["commits"][0]["message"], "initial\n\nbody\n");
    let files = services.call("git.commit-files", json!({"commit":oid}));
    assert_eq!(files["files"][0]["path"], path);
    assert_eq!(files["files"][0]["status"], "A");
    fs::remove_file(fixture.0.join(path)).unwrap();
    git(&fixture.0, &["add", "--", path]);
    git(&fixture.0, &["commit", "--quiet", "-m", "delete"]);
    let snapshot = services.call("git.blob", json!({"commit":oid,"path":path}));
    assert_eq!(snapshot["kind"], "snapshot");
    assert_eq!(snapshot["snapshot"]["text"], "original snapshot\n");
    assert_eq!(snapshot["snapshot"]["id"]["commit"], oid);
    let head = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    let files = services.call("git.commit-files", json!({"commit":head}));
    assert_eq!(files["files"][0]["status"], "D");
    assert_eq!(files["files"][0]["additions"], 0);
    assert_eq!(files["files"][0]["deletions"], 1);
    let absent = services.call("git.blob", json!({"commit":head,"path":path}));
    assert_eq!(absent["kind"], "unsupported");
    assert!(!fixture.0.join(path).exists());
}

#[test]
fn historical_previews_resolve_paths_relative_to_a_nested_workspace() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet", "--object-format=sha256"]);
    fs::create_dir(fixture.0.join("nested")).unwrap();
    let path = "note\tname\n.txt";
    fs::write(fixture.0.join("nested").join(path), "historical content\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "base"]);
    let mut services = NativeServices::new(fixture.0.join("nested")).unwrap();
    let oid = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    fs::remove_file(fixture.0.join("nested").join(path)).unwrap();
    let snapshot = services.call("git.blob", json!({"commit":oid, "path":path}));
    assert_eq!(snapshot["kind"], "snapshot", "{snapshot}");
    assert_eq!(snapshot["snapshot"]["text"], "historical content\n");
    assert_eq!(snapshot["snapshot"]["id"]["path"], path);
    assert_eq!(
        snapshot["snapshot"]["id"]["repository"],
        fixture.0.to_str().unwrap()
    );
    assert_eq!(snapshot["snapshot"]["id"]["commit"], oid);
    assert_eq!(
        snapshot["snapshot"]["id"]["blob"].as_str().unwrap().len(),
        64
    );
}

#[test]
fn historical_previews_read_objects_from_a_linked_worktree() {
    let fixture = Fixture::new();
    let repository = fixture.0.join("repository");
    let linked = fixture.0.join("linked");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "--quiet", "--object-format=sha256"]);
    fs::write(repository.join("note.txt"), "original\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "base"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    fs::write(linked.join("note.txt"), "linked\n").unwrap();
    git(&linked, &["add", "."]);
    git(&linked, &["commit", "--quiet", "-m", "linked"]);
    let mut services = NativeServices::new(linked.clone()).unwrap();
    let log = services.call("git.log", json!({}));
    let oid = log["commits"][0]["oid"].as_str().unwrap();
    let snapshot = services.call("git.blob", json!({"commit":oid,"path":"note.txt"}));
    assert_eq!(snapshot["kind"], "snapshot", "{snapshot}");
    assert_eq!(snapshot["snapshot"]["text"], "linked\n");
    assert_eq!(
        snapshot["snapshot"]["id"]["repository"],
        linked.to_str().unwrap()
    );
    let files = services.call("git.commit-files", json!({"commit":oid}));
    assert_eq!(files["files"][0]["additions"], 1, "{files}");
    assert_eq!(files["files"][0]["deletions"], 1);
}

#[cfg(unix)]
#[test]
fn historical_previews_reject_binary_large_and_symbolic_link_objects() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("binary.dat"), b"\0binary").unwrap();
    fs::write(fixture.0.join("large.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    std::os::unix::fs::symlink("binary.dat", fixture.0.join("link")).unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "objects"]);
    let mut services = fixture.services();
    let oid = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    for (path, message) in [
        ("binary.dat", "Binary preview is not implemented"),
        (
            "large.txt",
            "Historical file exceeds the preview size limit",
        ),
        ("link", "Preview unavailable for this Git object"),
    ] {
        let preview = services.call("git.blob", json!({"commit":oid,"path":path}));
        assert_eq!(preview["kind"], "unsupported", "{preview}");
        assert_eq!(preview["message"], message);
    }
}

#[test]
fn root_commit_files_distinguish_text_line_counts_from_binary_files() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("text.txt"), "first\nsecond\n").unwrap();
    fs::write(fixture.0.join("binary.dat"), [0, 1, 2]).unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "root"]);
    let mut services = fixture.services();
    let oid = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    let files = services.call("git.commit-files", json!({"commit":oid}));
    assert_eq!(files["files"].as_array().unwrap().len(), 2);
    assert_eq!(files["files"][0]["path"], "binary.dat");
    assert_eq!(files["files"][0]["status"], "A");
    assert!(files["files"][0]["additions"].is_null());
    assert!(files["files"][0]["deletions"].is_null());
    assert_eq!(files["files"][1]["path"], "text.txt");
    assert_eq!(files["files"][1]["additions"], 2);
    assert_eq!(files["files"][1]["deletions"], 0);
}

#[test]
fn commit_line_counts_are_relative_to_the_workspace() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::create_dir(fixture.0.join("nested")).unwrap();
    fs::write(fixture.0.join("nested/note.txt"), "first\nsecond\n").unwrap();
    fs::write(fixture.0.join("outside.txt"), "outside\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "root"]);
    let mut services = NativeServices::new(fixture.0.join("nested")).unwrap();
    let oid = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    let files = services.call("git.commit-files", json!({"commit":oid}));
    assert_eq!(files["files"].as_array().unwrap().len(), 1);
    assert_eq!(files["files"][0]["path"], "note.txt");
    assert_eq!(files["files"][0]["additions"], 2);
    assert_eq!(files["files"][0]["deletions"], 0);
}

#[test]
fn root_commit_diffs_compare_against_an_empty_tree() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("text.txt"), "first\nsecond\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "root"]);
    let mut services = fixture.services();
    let oid = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    let handle = start(
        &mut services,
        "git.commit-diff",
        json!({"commit":oid, "path":"text.txt", "unused":true}),
    );
    let patch = ready(&mut services, handle);
    let text = patch["text"].as_str().unwrap();
    assert_eq!(patch["path"], "text.txt");
    assert_eq!(
        text.lines().take(2).collect::<Vec<_>>(),
        ["diff --git a/text.txt b/text.txt", "new file mode 100644"]
    );
    assert_eq!(hunk_lines(text), ["@@ -0,0 +1,2 @@", "+first", "+second"]);
}

#[test]
fn renamed_commit_files_preserve_paths_and_line_counts() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    let original = "old\tname\n.txt";
    let path = "new\tname\n.txt";
    fs::write(
        fixture.0.join(original),
        "old\nkeep1\nkeep2\nkeep3\nkeep4\nkeep5\n",
    )
    .unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "base"]);
    fs::rename(fixture.0.join(original), fixture.0.join(path)).unwrap();
    fs::write(
        fixture.0.join(path),
        "new\nkeep1\nkeep2\nkeep3\nkeep4\nkeep5\n",
    )
    .unwrap();
    git(&fixture.0, &["add", "--all"]);
    git(&fixture.0, &["commit", "--quiet", "-m", "rename"]);
    let mut services = fixture.services();
    let oid = services.call("git.log", json!({}))["commits"][0]["oid"].clone();
    let handle = start(&mut services, "git.commit-files", json!({"commit":oid}));
    let files = ready(&mut services, handle);
    assert_eq!(files["files"].as_array().unwrap().len(), 1);
    assert_eq!(files["files"][0]["path"], path);
    assert_eq!(files["files"][0]["original_path"], original);
    assert!(
        files["files"][0]["status"]
            .as_str()
            .unwrap()
            .starts_with('R')
    );
    assert_eq!(files["files"][0]["additions"], 1);
    assert_eq!(files["files"][0]["deletions"], 1);
    let patch = services.call(
        "git.commit-diff",
        json!({"commit":oid, "path":path, "original_path":original}),
    );
    let text = patch["text"].as_str().unwrap();
    assert_eq!(patch["path"], path);
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("rename "))
            .collect::<Vec<_>>(),
        [
            "rename from \"old\\tname\\n.txt\"",
            "rename to \"new\\tname\\n.txt\""
        ],
    );
    assert_eq!(
        hunk_lines(text),
        [
            "@@ -1,4 +1,4 @@",
            "-old",
            "+new",
            " keep1",
            " keep2",
            " keep3"
        ]
    );
}

#[test]
fn cancelling_a_historical_diff_keeps_other_commit_results_independent() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "old\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "old"]);
    fs::write(fixture.0.join("note.txt"), "new\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "new"]);
    let mut services = fixture.services();
    let log = services.call("git.log", json!({}));
    let older = log["commits"][1]["oid"].as_str().unwrap();
    let newer = log["commits"][0]["oid"].as_str().unwrap();
    let old = services
        .start_query(super::QueryRequest::CommitDiff {
            commit: older.into(),
            path: "note.txt".into(),
            original_path: None,
        })
        .unwrap();
    services.cancel_query(old);
    fs::remove_file(fixture.0.join("note.txt")).unwrap();
    let new = services
        .start_query(super::QueryRequest::CommitDiff {
            commit: newer.into(),
            path: "note.txt".into(),
            original_path: None,
        })
        .unwrap();
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
    let patch = ready(&mut services, new);
    assert_eq!(patch["path"], "note.txt");
    assert_eq!(
        hunk_lines(patch["text"].as_str().unwrap()),
        ["@@ -1 +1 @@", "-old", "+new"]
    );
    assert!(services.poll_query(old).is_err());
}

#[test]
fn merge_changes_compare_against_the_first_parent() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet", "-b", "main"]);
    fs::write(fixture.0.join("base.txt"), "base").unwrap();
    git(&fixture.0, &["add", "base.txt"]);
    git(&fixture.0, &["commit", "--quiet", "-m", "base"]);
    git(&fixture.0, &["checkout", "--quiet", "-b", "other"]);
    fs::write(fixture.0.join("other.txt"), "other").unwrap();
    git(&fixture.0, &["add", "other.txt"]);
    git(&fixture.0, &["commit", "--quiet", "-m", "other"]);
    git(&fixture.0, &["checkout", "--quiet", "main"]);
    fs::write(fixture.0.join("main.txt"), "main").unwrap();
    git(&fixture.0, &["add", "main.txt"]);
    git(&fixture.0, &["commit", "--quiet", "-m", "main"]);
    git(
        &fixture.0,
        &["merge", "--quiet", "--no-ff", "other", "-m", "merge"],
    );
    let mut services = fixture.services();
    let log = services.call("git.log", json!({}));
    let oid = log["commits"][0]["oid"].as_str().unwrap();
    let files = services.call("git.commit-files", json!({"commit":oid}));
    assert_eq!(files["files"].as_array().unwrap().len(), 1, "{files}");
    assert_eq!(files["files"][0]["path"], "other.txt");
    assert_eq!(files["files"][0]["status"], "A");
    assert_eq!(files["files"][0]["additions"], 1);
    assert_eq!(files["files"][0]["deletions"], 0);
    let patch = services.call("git.commit-diff", json!({"commit":oid, "path":"other.txt"}));
    let text = patch["text"].as_str().unwrap();
    assert_eq!(patch["path"], "other.txt");
    assert_eq!(
        hunk_lines(text),
        ["@@ -0,0 +1 @@", "+other", "\\ No newline at end of file"]
    );
}

#[test]
fn changes_during_watch_preparation_are_included_in_the_first_snapshot() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    let mut services = fixture.services();
    let _observation = services
        .observe("fs.preview", &json!({"path":"note.txt"}))
        .unwrap();
    let handle = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    fs::write(fixture.0.join("note.txt"), "new").unwrap();

    assert_eq!(ready(&mut services, handle)["text"], "new");
}

#[test]
fn failed_watch_registration_is_reported_once_and_explicit_retries_keep_the_original_failure() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join(".git"), "invalid gitfile").unwrap();
    let mut services = fixture.services();
    let observation = services.observe("git.status", &json!({})).unwrap();
    let failed = start(&mut services, "git.status", json!({}));
    let expected = format!(
        "invalid Git directory file: {}",
        fixture.0.join(".git").display()
    );
    let deadline = Instant::now() + TIMEOUT;
    let error = loop {
        if let Err(error) = services.poll_changes() {
            break error;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(error.to_string(), expected);
    services.poll_changes().unwrap();

    fs::remove_file(fixture.0.join(".git")).unwrap();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "new\n").unwrap();
    let retry = start(&mut services, "git.status", json!({}));
    match services.poll_query(failed).unwrap() {
        super::QueryPoll::Ready(Err(error)) => assert_eq!(error, expected),
        _ => panic!("the failed registration was replaced by the retry"),
    }
    let status = ready(&mut services, retry);
    assert_eq!(status["entries"][0]["path"], "note.txt");
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":1,"deletions":0})
    );
    services.poll_changes().unwrap();
    drop(observation);
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
}

#[test]
fn released_subscriptions_do_not_install_queued_watches() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    for index in 0..4 {
        fs::write(fixture.0.join(format!("{index}.txt")), index.to_string()).unwrap();
    }
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 4);
    let observations = (0..4)
        .map(|index| {
            observe(
                &mut services,
                "fs.preview",
                &json!({"path":format!("{index}.txt")}),
            )
        })
        .collect::<Vec<_>>();
    let mut handles = Vec::new();
    for index in 0..4 {
        let handle = start(
            &mut services,
            "fs.preview",
            json!({"path":format!("{index}.txt")}),
        );
        probe.wait_for_read(&mut services, handle);
        handles.push(handle);
    }
    let watched = services.call("service.stats", json!({}))["watched_paths"].clone();
    let pending = services.observe("git.status", &json!({})).unwrap();
    let cancelled = start(&mut services, "git.status", json!({}));
    services.cancel_query(cancelled);
    drop(pending);
    services.poll_changes().unwrap();
    probe.release();
    for (index, handle) in handles.into_iter().enumerate() {
        assert_eq!(ready(&mut services, handle)["text"], index.to_string());
    }
    services.poll_changes().unwrap();
    let stats = services.call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 4);
    assert_eq!(stats["watched_paths"], watched);
    assert!(services.poll_query(cancelled).is_err());
    drop(observations);
    assert_eq!(
        services.call("service.stats", json!({}))["watched_paths"],
        0
    );
}

#[cfg(unix)]
#[test]
fn successful_preview_retries_preserve_the_original_registration_failure() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    std::os::unix::fs::symlink("missing.txt", &path).unwrap();
    let mut services = fixture.services();
    let failed = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    let deadline = Instant::now() + TIMEOUT;
    let error = loop {
        if let Err(error) = services.poll_changes() {
            break error.to_string();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(error, "broken symbolic link in workspace path");

    fs::remove_file(&path).unwrap();
    fs::write(&path, "recovered").unwrap();
    let retried = services.call("fs.preview", json!({"path":"note.txt"}));
    assert_eq!(retried["text"], "recovered");
    match services.poll_query(failed).unwrap() {
        super::QueryPoll::Ready(Err(error)) => {
            assert_eq!(error, "broken symbolic link in workspace path")
        }
        _ => panic!("the successful retry replaced the original failure"),
    }
}

#[test]
fn initial_watch_registration_does_not_report_unchanged_directory_contents() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut services = fixture.services();
    let mut observation = services.observe("fs.list", &json!({"path":""})).unwrap();
    let handle = start(&mut services, "fs.list", json!({"path":""}));

    assert_eq!(names(&ready(&mut services, handle)), ["note.txt"]);
    assert!(!observation.changed());
}

#[test]
fn file_changes_are_reported_while_background_workers_are_busy() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "old").unwrap();
    for index in 0..4 {
        fs::write(fixture.0.join(format!("{index}.txt")), index.to_string()).unwrap();
    }
    let mut services = fixture.services();
    let mut probe = ReadProbe::install(&mut services, 4);
    let mut changed = observe(&mut services, "fs.preview", &json!({"path":"note.txt"}));
    let _observations = (0..4)
        .map(|index| {
            observe(
                &mut services,
                "fs.preview",
                &json!({"path":format!("{index}.txt")}),
            )
        })
        .collect::<Vec<_>>();
    let mut handles = Vec::new();
    for index in 0..4 {
        let handle = start(
            &mut services,
            "fs.preview",
            json!({"path":format!("{index}.txt")}),
        );
        probe.wait_for_read(&mut services, handle);
        handles.push(handle);
    }

    fs::write(fixture.0.join("note.txt"), "new").unwrap();
    wait_for_change(&mut services, &mut changed);
    let refreshed = start(&mut services, "fs.preview", json!({"path":"note.txt"}));
    assert!(matches!(
        services.poll_query(refreshed).unwrap(),
        super::QueryPoll::Pending
    ));
    assert_eq!(probe.count(), 4);

    probe.release();
    for (index, handle) in handles.into_iter().enumerate() {
        assert_eq!(ready(&mut services, handle)["text"], index.to_string());
    }
    assert_eq!(ready(&mut services, refreshed)["text"], "new");
}
