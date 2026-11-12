use super::*;
use crate::tests::{Execute, Repository as Fixture};
use std::{fs, process::Command};

fn request(destination: IgnoreDestination, paths: &[(&str, bool)]) -> IgnoreRequest {
    IgnoreRequest {
        destination,
        paths: paths
            .iter()
            .map(|(path, directory)| SelectedPath {
                path: (*path).into(),
                directory: *directory,
            })
            .collect(),
    }
}

fn ignored(root: &Path, path: &str) -> bool {
    Command::new("git")
        .args(["check-ignore", "--quiet", "--", path])
        .current_dir(root)
        .status()
        .unwrap()
        .success()
}

#[test]
#[cfg(unix)]
fn literal_patterns_match_only_the_selected_paths() {
    let repo = Fixture::new();
    let name = "#hello ![x]*?\\ end ";
    fs::write(repo.0.join(name), "").unwrap();
    fs::create_dir(repo.0.join("dir [x]")).unwrap();
    fs::write(repo.0.join("dir [x]/child"), "").unwrap();
    fs::write(repo.0.join("#hello !xZZ end "), "").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    fs::write(repo.0.join(".gitignore"), b"old\r\nwithout-newline").unwrap();
    for _ in 0..2 {
        repository
            .ignore_paths(
                request(
                    IgnoreDestination::Repository,
                    &[(name, false), ("dir [x]", true)],
                ),
                &Execute,
            )
            .unwrap();
    }
    assert!(ignored(&repo.0, name));
    assert!(ignored(&repo.0, "dir [x]/child"));
    assert!(!ignored(&repo.0, "#hello !xZZ end "));
    let content = fs::read_to_string(repo.0.join(".gitignore")).unwrap();
    assert_eq!(
        content,
        "old\r\nwithout-newline\r\n/\\#hello\\ \\!\\[x\\]\\*\\?\\\\\\ end\\ \r\n/dir\\ \\[x\\]/\r\n"
    );
}

#[test]
fn tracked_paths_reject_the_whole_request_before_writing() {
    let repo = Fixture::new();
    fs::create_dir(repo.0.join("tracked")).unwrap();
    fs::write(repo.0.join("tracked/file"), "").unwrap();
    repo.git(&["add", "tracked/file"]);
    let repository = Repository::open(&repo.0).unwrap();
    for path in ["tracked", "tracked/file", "bad\nname"] {
        let error = repository
            .ignore_paths(
                request(
                    IgnoreDestination::Repository,
                    &[("new", false), (path, path == "tracked")],
                ),
                &Execute,
            )
            .err()
            .unwrap();
        let reason = if path == "bad\nname" {
            "path contains a character that cannot be represented in an ignore file"
        } else {
            "path contains tracked files; remove them from the index first"
        };
        assert_eq!(
            format!("{error:#}"),
            format!(
                "update ignore file {}: cannot ignore path {path:?}: {reason}",
                repo.0.join(".gitignore").display()
            )
        );
        assert!(!repo.0.join(".gitignore").exists());
    }
}

#[test]
fn local_rules_use_the_common_git_directory_in_linked_worktrees() {
    let repo = Fixture::new();
    repo.commit("head\n");
    let linked = repo.0.join("linked");
    repo.git(&[
        "worktree",
        "add",
        "--quiet",
        "-b",
        "linked",
        linked.to_str().unwrap(),
    ]);
    let exclude = repo.0.join(".git/info/exclude");
    let mut expected = fs::read(&exclude).unwrap();
    expected.extend_from_slice(b"/local\\ file\n");
    let repository = Repository::open(&linked).unwrap();
    repository
        .ignore_paths(
            request(IgnoreDestination::Local, &[("local file", false)]),
            &Execute,
        )
        .unwrap();
    assert!(ignored(&linked, "local file"));
    assert!(!linked.join(".gitignore").exists());
    assert_eq!(fs::read(exclude).unwrap(), expected);
}

#[test]
fn workspace_paths_are_anchored_to_the_repository_root() {
    let repo = Fixture::new();
    fs::create_dir(repo.0.join("nested")).unwrap();
    let repository = Repository::open(repo.0.join("nested")).unwrap();
    repository
        .ignore_paths(
            request(IgnoreDestination::Repository, &[("file", false)]),
            &Execute,
        )
        .unwrap();
    assert!(ignored(&repo.0, "nested/file"));
    assert!(!ignored(&repo.0, "file"));
    assert_eq!(
        fs::read(repo.0.join(".gitignore")).unwrap(),
        b"/nested/file\n"
    );
}

#[test]
fn a_later_negation_is_overridden_by_a_new_rule() {
    let repo = Fixture::new();
    fs::write(repo.0.join(".gitignore"), b"/file\n!/file\n").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    repository
        .ignore_paths(
            request(IgnoreDestination::Repository, &[("file", false)]),
            &Execute,
        )
        .unwrap();
    assert!(ignored(&repo.0, "file"));
    assert_eq!(
        fs::read(repo.0.join(".gitignore")).unwrap(),
        b"/file\n!/file\n/file\n"
    );
}

#[test]
fn a_nested_negation_reports_that_the_path_remains_unignored() {
    let repo = Fixture::new();
    fs::create_dir(repo.0.join("nested")).unwrap();
    fs::write(repo.0.join("nested/.gitignore"), b"!file\n").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    for (destination, target) in [
        (IgnoreDestination::Repository, repo.0.join(".gitignore")),
        (IgnoreDestination::Local, repo.0.join(".git/info/exclude")),
    ] {
        let error = repository
            .ignore_paths(request(destination, &[("nested/file", false)]), &Execute)
            .err()
            .unwrap();
        assert_eq!(
            format!("{error:#}"),
            format!(
                "update ignore file {}: Git still does not ignore path \"nested/file\"; a higher-precedence rule may override it (the ignore file may have been updated)",
                target.display()
            )
        );
        assert!(!ignored(&repo.0, "nested/file"));
    }
}

#[test]
fn local_rules_create_a_missing_info_directory() {
    let repo = Fixture::new();
    fs::remove_dir_all(repo.0.join(".git/info")).unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    repository
        .ignore_paths(
            request(IgnoreDestination::Local, &[("file", false)]),
            &Execute,
        )
        .unwrap();
    assert_eq!(
        fs::read(repo.0.join(".git/info/exclude")).unwrap(),
        b"/file\n"
    );
    assert!(ignored(&repo.0, "file"));
}
