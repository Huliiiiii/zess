use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::{
    PatchRequest,
    patch::{PatchRow, RawPatch},
};
use crate::{DiffSide, Execution};

#[test]
fn decorations_are_empty_outside_a_repository_but_status_reports_the_error() {
    let repo = Repository::new();
    fs::remove_dir_all(repo.0.join(".git")).unwrap();
    let repository = crate::Repository::open(&repo.0).unwrap();
    let (entries, warning) = repository.decorations(&Execute).unwrap();
    assert!(entries.is_empty());
    assert!(warning.is_none());
    assert!(repository.status(&Execute).is_err());
}

#[test]
fn decorations_report_corrupt_repository_metadata() {
    let repo = Repository::new();
    fs::write(repo.0.join(".git/config"), "[invalid\n").unwrap();
    let repository = crate::Repository::open(&repo.0).unwrap();
    let error = repository.decorations(&Execute).unwrap_err();
    assert_eq!(
        error.downcast_ref::<git2::Error>().unwrap().class(),
        git2::ErrorClass::Config
    );
}

#[test]
fn decorations_preserve_git_status_failures() {
    let repo = Repository::new();
    repo.commit("baseline\n");
    fs::write(repo.0.join(".git/index"), "corrupt index").unwrap();
    let repository = crate::Repository::open(&repo.0).unwrap();
    let error = repository.decorations(&Execute).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("read Git status in {}", repo.0.display())
    );
    assert!(error.chain().nth(1).is_some());
}

#[test]
fn decorations_preserve_staging_and_rename_identity_without_line_counts() {
    use crate::{FileStatus, StagingStatus};

    let repo = Repository::new();
    for name in ["staged", "unstaged", "both", "old name"] {
        fs::write(repo.0.join(name), "baseline\n").unwrap();
    }
    repo.git(&["add", "."]);
    repo.git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(repo.0.join("staged"), "index\n").unwrap();
    fs::write(repo.0.join("both"), "index\n").unwrap();
    repo.git(&["add", "staged", "both"]);
    fs::write(repo.0.join("both"), "worktree\n").unwrap();
    fs::write(repo.0.join("unstaged"), "worktree\n").unwrap();
    fs::write(repo.0.join("untracked"), [0, 255]).unwrap();
    repo.git(&["mv", "old name", "new name"]);
    let repository = crate::Repository::open(&repo.0).unwrap();
    let (entries, warning) = repository.decorations(&Execute).unwrap();
    assert!(warning.is_none());
    assert_eq!(entries.len(), 5);
    for (path, expected) in [
        ("staged", StagingStatus::Staged),
        ("unstaged", StagingStatus::Unstaged),
        ("both", StagingStatus::Both),
        ("untracked", StagingStatus::Untracked),
        ("new name", StagingStatus::Staged),
    ] {
        let entry = entries.iter().find(|entry| entry.path == path).unwrap();
        assert_eq!(entry.staging_status, expected, "{path}");
        assert!(matches!(
            entry.status,
            FileStatus::Modified | FileStatus::Added
        ));
        assert_eq!(
            entry.original_path.as_deref(),
            (path == "new name").then_some("old name")
        );
    }
    for entry in repository.status(&Execute).unwrap() {
        let decoration = entries.iter().find(|item| item.path == entry.path).unwrap();
        assert_eq!(entry.staging_status(), decoration.staging_status);
    }
}

#[test]
fn status_and_decorations_preserve_workspace_relative_git_paths() {
    let repo = Repository::new();
    fs::create_dir_all(repo.0.join("sub/deep")).unwrap();
    for name in ["sub/deep/old", "sub/deep/deleted", "outside"] {
        fs::write(repo.0.join(name), "baseline\n").unwrap();
    }
    repo.git(&["add", "."]);
    repo.git(&["commit", "--quiet", "-m", "baseline"]);
    repo.git(&["mv", "sub/deep/old", "sub/deep/renamed"]);
    fs::remove_file(repo.0.join("sub/deep/deleted")).unwrap();
    fs::write(repo.0.join("sub/deep/untracked"), "new\n").unwrap();
    fs::write(repo.0.join("outside"), "modified\n").unwrap();

    for (directory, prefix, count) in [("", "sub/", 4), ("sub", "", 3)] {
        let repository = crate::Repository::open(repo.0.join(directory)).unwrap();
        let entries = repository.status(&Execute).unwrap();
        let (decorations, warning) = repository.decorations(&Execute).unwrap();
        assert!(warning.is_none());
        assert_eq!(entries.len(), count);
        assert_eq!(decorations.len(), count);
        for name in ["deleted", "renamed", "untracked"] {
            let path = format!("{prefix}deep/{name}");
            let original = (name == "renamed").then(|| format!("{prefix}deep/old"));
            let entry = entries.iter().find(|entry| entry.path == path).unwrap();
            assert_eq!(entry.original_path, original);
            let decoration = decorations.iter().find(|entry| entry.path == path).unwrap();
            assert_eq!(decoration.original_path, original);
        }
    }
}

#[test]
fn status_resolves_the_reported_repository_root_before_projecting_paths() {
    struct ReportedRoot(PathBuf);
    impl Execution for ReportedRoot {
        fn check(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn output(
            &self,
            command: &zess_platform::ProcessSpec,
        ) -> anyhow::Result<std::process::Output> {
            let mut output = Execute.output(command)?;
            if command.args.iter().any(|arg| arg == "--show-toplevel") {
                output.stdout = format!("{}\n", self.0.display()).into_bytes();
            }
            Ok(output)
        }

        fn output_with_input(
            &self,
            command: &zess_platform::ProcessSpec,
            input: &[u8],
        ) -> anyhow::Result<std::process::Output> {
            Execute.output_with_input(command, input)
        }
    }

    let repo = Repository::new();
    repo.commit("baseline\n");
    fs::create_dir(repo.0.join("sub")).unwrap();
    fs::write(repo.0.join("file"), "modified\n").unwrap();
    let repository = crate::Repository::open(&repo.0).unwrap();
    let entries = repository
        .status(&ReportedRoot(repo.0.join("sub/..")))
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "file");
    assert_eq!(entries[0].line_stats.unwrap().additions, 1);
    assert_eq!(entries[0].line_stats.unwrap().deletions, 1);
}

#[cfg(unix)]
#[test]
fn git_queries_and_patch_staging_preserve_path_whitespace() {
    let fixture = Repository::new();
    let root = fixture.0.join(" repo \"quote\" \r\n");
    fs::create_dir(&root).unwrap();
    let repo = Repository(root);
    repo.git(&["init", "--quiet"]);
    let workspace_name = " scope \"quote\" \r\n";
    let path = " file \"quote\" \r\n";
    let target = format!("{workspace_name}/{path}");
    let decoy = format!("{}/{path}", workspace_name.trim_end());
    for file in [&target, &decoy] {
        let file = repo.0.join(file);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "old\n").unwrap();
    }
    repo.git(&["add", "."]);
    repo.git(&["commit", "--quiet", "-m", "baseline"]);
    let baseline = repo.git(&["rev-parse", "HEAD"]);
    fs::write(repo.0.join(&target), "old\nnew\n").unwrap();
    let workspace = repo.0.join(workspace_name);
    let repository = crate::Repository::open(&workspace).unwrap();
    let entries = repository.status(&Execute).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, path);
    assert_eq!(entries[0].line_stats.unwrap().additions, 1);
    let (decorations, warning) = repository.decorations(&Execute).unwrap();
    assert_eq!(decorations[0].path, path);
    assert!(warning.is_none());
    let raw = repository
        .diff(path, None, Some(DiffSide::Unstaged), &Execute)
        .unwrap();
    let added = row(&raw, "+new");
    repository
        .apply_patch(
            PatchRequest {
                path: path.into(),
                original_path: None,
                side: DiffSide::Unstaged,
                raw,
                rows: vec![added..=added],
            },
            &Execute,
        )
        .unwrap();
    assert_eq!(repo.git(&["show", &format!(":{target}")]), "old\nnew\n");
    assert_eq!(repo.git(&["show", &format!(":{decoy}")]), "old\n");

    repo.git(&["commit", "--quiet", "-m", "change"]);
    let commit = repo.git(&["rev-parse", "HEAD"]);
    let files = repository
        .commit_files(commit.trim_end(), &Execute)
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, path);
    assert_eq!(files[0].additions, Some(1));
    let crate::BlobRead::File(blob) = repository
        .blob(baseline.trim_end(), path, &Execute)
        .unwrap()
    else {
        panic!("baseline file unavailable");
    };
    assert_eq!(blob.bytes.as_slice(), b"old\n");
}

#[test]
fn git_queries_use_the_worktree_and_its_separate_metadata() {
    let fixture = Repository::new();
    fixture.commit("decoy\n");
    for metadata_name in [
        "git metadata",
        #[cfg(unix)]
        "git metadata ",
    ] {
        let root = fixture.0.join("working tree");
        let metadata = fixture.0.join(metadata_name);
        fs::create_dir(&root).unwrap();
        let repo = Repository(root);
        repo.git(&[
            "init",
            "--quiet",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
        ]);
        let gitfile = format!("gitdir: ../{metadata_name}\n");
        fs::write(repo.0.join(".git"), &gitfile).unwrap();
        let config = fs::read(metadata.join("config")).unwrap();
        fs::create_dir(repo.0.join("nested")).unwrap();
        fs::write(repo.0.join("nested/file"), "old\n").unwrap();
        repo.git(&["add", "."]);
        repo.git(&["commit", "--quiet", "-m", "baseline"]);
        let oid = repo.git(&["rev-parse", "HEAD"]);
        let oid = oid.strip_suffix('\n').unwrap();
        fs::write(repo.0.join("nested/file"), "old\nnew\n").unwrap();

        let repository = crate::Repository::open(repo.0.join("nested")).unwrap();
        let entries = repository.status(&Execute).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "file");
        assert_eq!(entries[0].line_stats.unwrap().additions, 1);
        let (decorations, warning) = repository.decorations(&Execute).unwrap();
        assert!(warning.is_none());
        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].path, "file");
        let files = repository.commit_files(oid, &Execute).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "file");
        assert_eq!(files[0].additions, Some(1));
        let crate::BlobRead::File(blob) = repository.blob(oid, "file", &Execute).unwrap() else {
            panic!("baseline file unavailable");
        };
        assert_eq!(blob.bytes.as_slice(), b"old\n");
        assert_eq!(fs::read_to_string(repo.0.join(".git")).unwrap(), gitfile);
        assert_eq!(fs::read(metadata.join("config")).unwrap(), config);
    }
}

#[test]
fn linked_worktree_queries_respect_its_configured_root() {
    let fixture = Repository::new();
    fixture.commit("old\n");
    fixture.git(&["worktree", "add", "--quiet", "-b", "linked", "linked"]);
    let linked = Repository(fixture.0.join("linked"));
    let worktree = linked.0.join("configured");
    fs::create_dir(&worktree).unwrap();
    fs::write(worktree.join("file"), "old\nnew\n").unwrap();
    linked.git(&["config", "extensions.worktreeConfig", "true"]);
    linked.git(&[
        "config",
        "--worktree",
        "core.worktree",
        "../../../linked/configured",
    ]);
    let oid = linked.git(&["rev-parse", "HEAD"]);
    let oid = oid.strip_suffix('\n').unwrap();

    let repository = crate::Repository::open(&worktree).unwrap();
    let entries = repository.status(&Execute).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "file");
    assert_eq!(entries[0].line_stats.unwrap().additions, 1);
    let (decorations, _) = repository.decorations(&Execute).unwrap();
    assert_eq!(decorations.len(), 1);
    assert_eq!(decorations[0].path, "file");
    let crate::BlobRead::File(blob) = repository.blob(oid, "file", &Execute).unwrap() else {
        panic!("baseline file unavailable");
    };
    assert_eq!(blob.bytes.as_slice(), b"old\n");
}

#[test]
fn commit_queries_find_a_bare_repository_inside_another_worktree() {
    let fixture = Repository::new();
    fixture.commit("bare content\n");
    let oid = fixture.git(&["rev-parse", "HEAD"]);
    let oid = oid.strip_suffix('\n').unwrap();
    let root = fixture.0.join("working tree");
    let metadata = fixture.0.join("git metadata");
    fs::create_dir(&root).unwrap();
    let repo = Repository(root);
    repo.git(&[
        "init",
        "--quiet",
        "--separate-git-dir",
        metadata.to_str().unwrap(),
    ]);
    repo.commit("outer content\n");
    repo.git(&[
        "clone",
        "--quiet",
        "--bare",
        fixture.0.to_str().unwrap(),
        "nested.git",
    ]);

    let bare = crate::Repository::open(repo.0.join("nested.git")).unwrap();
    let diff = bare.commit_diff(oid, "file", None, &Execute).unwrap();
    assert!(diff.lines().any(|line| line == "+bare content"));
}

#[test]
fn read_only_queries_do_not_run_required_worktree_filters() {
    let repo = Repository::new();
    repo.commit("baseline\n");
    fs::write(repo.0.join(".gitattributes"), "file filter=blocked\n").unwrap();
    repo.git(&[
        "config",
        "filter.blocked.clean",
        "zess-nonexistent-clean-filter",
    ]);
    repo.git(&[
        "config",
        "filter.blocked.process",
        "zess-nonexistent-process-filter",
    ]);
    repo.git(&["config", "filter.blocked.required", "true"]);
    fs::write(repo.0.join("file"), "modified\n").unwrap();

    let repository = crate::Repository::open(&repo.0).unwrap();
    let status = repository.status(&Execute).unwrap();
    assert!(status.iter().any(|entry| entry.path == "file"));
    let diff = repository
        .diff("file", None, Some(DiffSide::Unstaged), &Execute)
        .unwrap();
    assert!(diff.contains("-baseline\n+modified\n"), "{diff}");
    let commits = repository.log(&Execute).unwrap();
    assert_eq!(commits.len(), 1);
    let files = repository.commit_files(&commits[0].oid, &Execute).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "file");
}

#[test]
fn every_unmerged_index_state_is_a_conflict() {
    use std::io::Write;
    use std::process::Stdio;

    let repo = Repository::new();
    repo.commit("baseline\n");
    let oid = repo.git(&["rev-parse", "HEAD:file"]);
    let mut index = String::new();
    for (path, stages) in [
        ("DD", &[1][..]),
        ("AU", &[2][..]),
        ("UD", &[1, 2][..]),
        ("UA", &[3][..]),
        ("DU", &[1, 3][..]),
        ("AA", &[2, 3][..]),
        ("UU", &[1, 2, 3][..]),
    ] {
        for stage in stages {
            index.push_str(&format!("100644 {} {stage}\t{path}\n", oid.trim()));
        }
    }
    let mut child = Command::new("git")
        .args(["update-index", "--index-info"])
        .current_dir(&repo.0)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(index.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());

    let repository = crate::Repository::open(&repo.0).unwrap();
    let entries = repository.status(&Execute).unwrap();
    assert_eq!(entries.len(), 7);
    for entry in entries {
        assert_eq!(format!("{}{}", entry.index, entry.worktree), entry.path);
        assert_eq!(entry.staging_status(), crate::StagingStatus::Conflict);
    }
    let (decorations, _) = repository.decorations(&Execute).unwrap();
    assert_eq!(decorations.len(), 7);
    for entry in decorations {
        assert_eq!(entry.staging_status, crate::StagingStatus::Conflict);
    }
}

pub(super) struct Execute;
impl zess_platform::Cancellation for Execute {
    fn check(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
impl Execution for Execute {
    fn environment(&self, name: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
        use zess_platform::ProcessRunner;
        zess_platform::SystemProcessRunner.environment(name)
    }

    fn check(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn output(&self, command: &zess_platform::ProcessSpec) -> anyhow::Result<std::process::Output> {
        use zess_platform::ProcessRunner;
        zess_platform::SystemProcessRunner.output(command, None, self)
    }
    fn output_with_input(
        &self,
        command: &zess_platform::ProcessSpec,
        input: &[u8],
    ) -> anyhow::Result<std::process::Output> {
        use zess_platform::ProcessRunner;
        zess_platform::SystemProcessRunner.output(command, Some(input), self)
    }
}

#[test]
fn explicit_pager_renders_the_same_patch_as_the_operation_after_worktree_changes() {
    use std::cell::Cell;

    struct ChangeAfterDiff<'a> {
        path: &'a std::path::Path,
        changed: Cell<bool>,
    }

    impl Execution for ChangeAfterDiff<'_> {
        fn environment(&self, name: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
            Execute.environment(name)
        }

        fn check(&self) -> anyhow::Result<()> {
            Execute.check()
        }

        fn output(
            &self,
            command: &zess_platform::ProcessSpec,
        ) -> anyhow::Result<std::process::Output> {
            let raw_diff = command.args.iter().any(|arg| arg == "--no-ext-diff");
            let output = Execute.output(command)?;
            if raw_diff && !self.changed.replace(true) {
                fs::write(self.path, "worktree B\n")?;
            }
            Ok(output)
        }

        fn output_with_input(
            &self,
            command: &zess_platform::ProcessSpec,
            input: &[u8],
        ) -> anyhow::Result<std::process::Output> {
            Execute.output_with_input(command, input)
        }
    }

    let repo = Repository::new();
    repo.commit("baseline\n");
    fs::write(repo.0.join("file"), "worktree A\n").unwrap();
    repo.git(&["config", "diff.external", "false"]);
    let expected = repo.git(&[
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-renames",
        "--",
        "file",
    ]);
    let file = repo.0.join("file");
    let execution = ChangeAfterDiff {
        path: &file,
        changed: Cell::new(false),
    };
    let diff = crate::Repository::open(&repo.0)
        .unwrap()
        .diff_files(
            &crate::DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            &[crate::DiffTarget {
                path: "file".into(),
                original_path: None,
            }],
            &crate::DiffRenderOptions {
                pager: Some(crate::PagerCommand {
                    program: "sed".into(),
                    args: vec!["-n".into(), "p".into()],
                }),
                ..Default::default()
            },
            &execution,
        )
        .unwrap();

    assert!(execution.changed.get());
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].raw, expected);
    assert_eq!(diff.files[0].rendered.as_ref().unwrap().text, expected);
    assert_eq!(fs::read_to_string(file).unwrap(), "worktree B\n");
}

#[test]
fn configured_external_renderers_do_not_change_the_patch_used_for_staging() {
    let repo = Repository::new();
    repo.commit("old\n");
    fs::write(repo.0.join("file"), "new\n").unwrap();
    fs::write(
        repo.0.join("renderer.sh"),
        "printf '\\033[35mrenderer:%s\\033[0m\\n' \"$1\"\ncat \"$2\" \"$5\"\n",
    )
    .unwrap();
    repo.git(&["config", "diff.external", "sh ./renderer.sh"]);
    repo.git(&["config", "pager.diff", "false"]);
    let repository = crate::Repository::open(&repo.0).unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let rendered = repository
        .diff_renderer(&crate::DiffRenderOptions::default(), &Execute)
        .unwrap()
        .render(
            &repository,
            &crate::DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            &crate::DiffTarget {
                path: "file".into(),
                original_path: None,
            },
            &raw,
            &Execute,
        )
        .unwrap()
        .unwrap();
    assert!(rendered.text.contains("renderer:file"));
    assert!(rendered.text.contains("old\nnew\n"));
    assert!(raw.contains("@@ -1 +1 @@\n-old\n+new\n"));
    let start = row(&raw, "-old");
    let end = row(&raw, "+new");
    repo.apply(raw, DiffSide::Unstaged, vec![start..=end])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "new\n");
}

#[test]
fn configured_text_conversion_is_only_used_for_the_human_readable_rendering() {
    let repo = Repository::new();
    repo.commit("raw-old\n");
    fs::write(repo.0.join("file"), "raw-new\n").unwrap();
    fs::write(repo.0.join(".gitattributes"), "file diff=converted\n").unwrap();
    fs::write(
        repo.0.join("convert.sh"),
        "sed 's/raw/converted/g' \"$1\"\n",
    )
    .unwrap();
    repo.git(&["config", "diff.external", ""]);
    repo.git(&["config", "diff.converted.textconv", "sh ./convert.sh"]);
    repo.git(&["config", "pager.diff", "false"]);
    let repository = crate::Repository::open(&repo.0).unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let rendered = repository
        .diff_renderer(&crate::DiffRenderOptions::default(), &Execute)
        .unwrap()
        .render(
            &repository,
            &crate::DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            &crate::DiffTarget {
                path: "file".into(),
                original_path: None,
            },
            &raw,
            &Execute,
        )
        .unwrap()
        .unwrap();
    assert!(rendered.text.contains("converted-old"));
    assert!(rendered.text.contains("converted-new"));
    assert!(raw.contains("-raw-old\n+raw-new\n"));
}

#[test]
fn external_renderers_receive_empty_old_content_for_a_root_commit() {
    let repo = Repository::new();
    repo.commit("committed\n");
    fs::write(
        repo.0.join("renderer.sh"),
        "printf 'old:'; cat \"$2\"; printf 'new:'; cat \"$5\"\n",
    )
    .unwrap();
    repo.git(&["config", "diff.external", "sh ./renderer.sh"]);
    repo.git(&["config", "pager.diff", "false"]);
    let commit = repo.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let repository = crate::Repository::open(&repo.0).unwrap();
    let raw = repository
        .commit_diff(&commit, "file", None, &Execute)
        .unwrap();
    let rendered = repository
        .diff_renderer(&crate::DiffRenderOptions::default(), &Execute)
        .unwrap()
        .render(
            &repository,
            &crate::DiffSource::Commit { commit },
            &crate::DiffTarget {
                path: "file".into(),
                original_path: None,
            },
            &raw,
            &Execute,
        )
        .unwrap()
        .unwrap();
    assert_eq!(rendered.text, "old:new:committed\n");
    assert!(raw.contains("+committed\n"));
}

pub(super) struct Repository(pub(super) PathBuf);
impl Repository {
    pub(super) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zess-git-patch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "--quiet"]);
        repo
    }
    pub(super) fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Zess Tests",
                "-c",
                "user.email=zess@example.invalid",
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
    pub(super) fn commit(&self, content: &str) {
        fs::write(self.0.join("file"), content).unwrap();
        self.git(&["add", "file"]);
        self.git(&["commit", "--quiet", "-m", "baseline"]);
    }
    pub(super) fn diff(&self, side: DiffSide) -> String {
        crate::Repository::open(&self.0)
            .unwrap()
            .diff("file", None, Some(side), &Execute)
            .unwrap()
    }

    fn apply(
        &self,
        raw: String,
        side: DiffSide,
        rows: Vec<std::ops::RangeInclusive<PatchRow>>,
    ) -> Result<(), String> {
        crate::Repository::open(&self.0)
            .unwrap()
            .apply_patch(
                PatchRequest {
                    path: "file".into(),
                    original_path: None,
                    side,
                    raw,
                    rows,
                },
                &Execute,
            )
            .map_err(|error| format!("{error:#}"))
    }
}
impl Drop for Repository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn row(raw: &str, line: &str) -> PatchRow {
    PatchRow(raw.lines().position(|value| value == line).unwrap())
}

#[test]
fn selected_lines_move_between_worktree_and_index_without_changing_the_file() {
    let repo = Repository::new();
    repo.commit("first\nsecond\nthird\n");
    let working = "FIRST\nSECOND\nthird\n";
    fs::write(repo.0.join("file"), working).unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let start = row(&raw, "+FIRST");
    repo.apply(raw, DiffSide::Unstaged, vec![start..=start])
        .unwrap();
    assert_eq!(
        repo.git(&["show", ":file"]),
        "FIRST\nfirst\nsecond\nthird\n"
    );
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), working);
    let raw = repo.diff(DiffSide::Staged);
    let start = row(&raw, "+FIRST");
    repo.apply(raw, DiffSide::Staged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "first\nsecond\nthird\n");
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), working);
}

#[test]
fn a_later_hunk_can_be_staged_without_earlier_line_count_changes() {
    let repo = Repository::new();
    let baseline = (0..30).map(|n| format!("line {n}\n")).collect::<String>();
    repo.commit(&baseline);
    let working = baseline
        .replace("line 1\n", "extra\nlines\nline 1\n")
        .replace("line 25\n", "changed\n");
    fs::write(repo.0.join("file"), &working).unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let parsed = RawPatch::parse(&raw).unwrap();
    let rows = parsed.hunks().last().unwrap().rows();
    repo.apply(raw, DiffSide::Unstaged, vec![rows]).unwrap();
    assert_eq!(
        repo.git(&["show", ":file"]),
        baseline.replace("line 25\n", "changed\n")
    );
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), working);
}

#[test]
fn part_of_an_added_file_can_be_staged_and_unstaged() {
    let repo = Repository::new();
    fs::write(repo.0.join("file"), "first\nsecond\n").unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let start = row(&raw, "+first");
    repo.apply(raw, DiffSide::Unstaged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "first\n");
    let raw = repo.diff(DiffSide::Unstaged);
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .next()
        .unwrap()
        .rows();
    repo.apply(raw, DiffSide::Unstaged, vec![rows]).unwrap();
    let raw = repo.diff(DiffSide::Staged);
    let start = row(&raw, "+first");
    repo.apply(raw, DiffSide::Staged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "second\n");
}

#[test]
fn part_of_a_deleted_file_keeps_the_remaining_lines_in_the_index() {
    let repo = Repository::new();
    repo.commit("first\nsecond\n");
    fs::remove_file(repo.0.join("file")).unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let start = row(&raw, "-first");
    repo.apply(raw, DiffSide::Unstaged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "second\n");
    let raw = repo.diff(DiffSide::Staged);
    let start = row(&raw, "-first");
    repo.apply(raw, DiffSide::Staged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "first\nsecond\n");
    assert!(!repo.0.join("file").exists());
}

#[test]
fn stale_selections_are_rejected_before_the_index_is_changed() {
    let repo = Repository::new();
    repo.commit("old\n");
    fs::write(repo.0.join("file"), "first change\n").unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .next()
        .unwrap()
        .rows();
    fs::write(repo.0.join("file"), "second change\n").unwrap();
    let error = repo.apply(raw, DiffSide::Unstaged, vec![rows]).unwrap_err();
    assert_eq!(
        error,
        "Git diff changed; refreshed preview, select the changes again"
    );
    assert_eq!(repo.git(&["show", ":file"]), "old\n");
}

#[test]
fn hunk_staging_preserves_a_missing_final_newline() {
    let repo = Repository::new();
    repo.commit("old");
    fs::write(repo.0.join("file"), "new").unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .next()
        .unwrap()
        .rows();
    repo.apply(raw, DiffSide::Unstaged, vec![rows]).unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "new");
    let raw = repo.diff(DiffSide::Staged);
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .next()
        .unwrap()
        .rows();
    repo.apply(raw, DiffSide::Staged, vec![rows]).unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "old");
}

#[test]
fn unstaging_a_later_hunk_preserves_earlier_staged_insertions() {
    let repo = Repository::new();
    let baseline = (0..30).map(|n| format!("line {n}\n")).collect::<String>();
    repo.commit(&baseline);
    let staged = baseline
        .replace("line 1\n", "extra\nlines\nline 1\n")
        .replace("line 25\n", "changed\n");
    fs::write(repo.0.join("file"), &staged).unwrap();
    repo.git(&["add", "file"]);
    let raw = repo.diff(DiffSide::Staged);
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .last()
        .unwrap()
        .rows();
    repo.apply(raw, DiffSide::Staged, vec![rows]).unwrap();
    assert_eq!(
        repo.git(&["show", ":file"]),
        baseline.replace("line 1\n", "extra\nlines\nline 1\n")
    );
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), staged);
}

#[test]
fn a_rejected_patch_keeps_the_index_atomic() {
    let repo = Repository::new();
    repo.commit("before\n");
    fs::write(repo.0.join("file"), "after\n").unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .next()
        .unwrap()
        .rows();
    fs::write(repo.0.join(".git/index.lock"), "held").unwrap();
    assert!(repo.apply(raw, DiffSide::Unstaged, vec![rows]).is_err());
    assert_eq!(repo.git(&["show", ":file"]), "before\n");
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), "after\n");
}

#[test]
fn untracked_files_inside_a_nested_workspace_are_staged_in_that_directory() {
    let repo = Repository::new();
    fs::create_dir(repo.0.join("sub")).unwrap();
    let nested = Repository(repo.0.join("sub"));
    fs::write(nested.0.join("file"), "first\nsecond\n").unwrap();
    let raw = nested.diff(DiffSide::Unstaged);
    let start = row(&raw, "+first");
    nested
        .apply(raw, DiffSide::Unstaged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":sub/file"]), "first\n");
    assert_eq!(repo.git(&["ls-files"]), "sub/file\n");
}

#[cfg(unix)]
#[test]
fn partial_staging_preserves_the_mode_when_creating_an_index_file() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repository::new();
    fs::write(repo.0.join("file"), "first\nsecond\n").unwrap();
    fs::set_permissions(repo.0.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let start = row(&raw, "+first");
    repo.apply(raw, DiffSide::Unstaged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "first\n");
    let index = git2::Repository::open(&repo.0).unwrap().index().unwrap();
    assert_eq!(
        index
            .get_path(std::path::Path::new("file"), 0)
            .unwrap()
            .mode,
        0o100755
    );
    repo.git(&["add", "file"]);
    repo.git(&["commit", "--quiet", "-m", "script"]);
    fs::remove_file(repo.0.join("file")).unwrap();
    repo.git(&["add", "file"]);
    let raw = repo.diff(DiffSide::Staged);
    let start = row(&raw, "-first");
    repo.apply(raw, DiffSide::Staged, vec![start..=start])
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "first\n");
    let index = git2::Repository::open(&repo.0).unwrap().index().unwrap();
    assert_eq!(
        index
            .get_path(std::path::Path::new("file"), 0)
            .unwrap()
            .mode,
        0o100755
    );
    assert!(!repo.0.join("file").exists());
}

#[cfg(unix)]
#[test]
fn staging_content_keeps_an_unselected_mode_change_out_of_the_index() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repository::new();
    repo.commit("old\n");
    fs::write(repo.0.join("file"), "new\n").unwrap();
    fs::set_permissions(repo.0.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    assert_eq!(
        raw.lines().take(3).collect::<Vec<_>>(),
        [
            "diff --git a/file b/file",
            "old mode 100644",
            "new mode 100755"
        ]
    );
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .next()
        .unwrap()
        .rows();
    repo.apply(raw, DiffSide::Unstaged, vec![rows]).unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "new\n");
    let index = git2::Repository::open(&repo.0).unwrap().index().unwrap();
    assert_eq!(
        index
            .get_path(std::path::Path::new("file"), 0)
            .unwrap()
            .mode,
        0o100644
    );
    assert_eq!(
        fs::metadata(repo.0.join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn unstaging_content_keeps_the_staged_mode_change() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repository::new();
    repo.commit("old\n");
    fs::write(repo.0.join("file"), "new\n").unwrap();
    fs::set_permissions(repo.0.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    repo.git(&["add", "file"]);
    let raw = repo.diff(DiffSide::Staged);
    assert_eq!(
        raw.lines().take(3).collect::<Vec<_>>(),
        [
            "diff --git a/file b/file",
            "old mode 100644",
            "new mode 100755"
        ]
    );
    let rows = RawPatch::parse(&raw)
        .unwrap()
        .hunks()
        .next()
        .unwrap()
        .rows();
    repo.apply(raw, DiffSide::Staged, vec![rows]).unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "old\n");
    let index = git2::Repository::open(&repo.0).unwrap().index().unwrap();
    assert_eq!(
        index
            .get_path(std::path::Path::new("file"), 0)
            .unwrap()
            .mode,
        0o100755
    );
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), "new\n");
}

#[test]
fn blank_context_remains_parseable_when_git_suppresses_empty_line_prefixes() {
    let repo = Repository::new();
    repo.commit("top\n\nold\n\nbottom\n");
    repo.git(&["config", "diff.suppressBlankEmpty", "true"]);
    fs::write(repo.0.join("file"), "top\n\nnew\n\nbottom\n").unwrap();
    let raw = repo.diff(DiffSide::Unstaged);
    let patch = RawPatch::parse(&raw).unwrap();
    let hunks = patch.hunks().collect::<Vec<_>>();
    assert_eq!(hunks.len(), 1);
    assert_eq!(
        hunks[0]
            .lines
            .iter()
            .map(|line| line.raw)
            .collect::<Vec<_>>(),
        [" top\n", " \n", "-old\n", "+new\n", " \n", " bottom\n"]
    );
    let rows = hunks[0].rows();
    repo.apply(raw, DiffSide::Unstaged, vec![rows]).unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "top\n\nnew\n\nbottom\n");
}

#[test]
fn status_failures_identify_the_operation_and_preserve_the_git_error() {
    let repo = Repository::new();
    repo.commit("original\n");
    fs::write(repo.0.join(".git/index"), "invalid index").unwrap();
    let failed = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(&repo.0)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let error = crate::Repository::open(&repo.0)
        .unwrap()
        .status(&Execute)
        .unwrap_err();

    assert_eq!(
        error.chain().map(ToString::to_string).collect::<Vec<_>>(),
        [
            format!("read Git status in {}", repo.0.display()),
            String::from_utf8(failed.stderr).unwrap().trim().to_owned(),
        ]
    );
}
