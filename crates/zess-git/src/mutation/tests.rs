use super::*;
use crate::tests::{Execute, Repository as Fixture};
use std::fs;

fn mutate(repo: &Fixture, operation: FileMutation) {
    let repository = Repository::open(&repo.0).unwrap();
    let entries = repository.status(&Execute).unwrap();
    repository
        .mutate_files(FileMutationRequest { operation, entries }, &Execute)
        .unwrap();
}

#[test]
fn staging_and_unstaging_preserve_the_other_side_of_modified_files() {
    let repo = Fixture::new();
    repo.commit("head\n");
    fs::write(repo.0.join("file"), "index\n").unwrap();
    repo.git(&["add", "file"]);
    fs::write(repo.0.join("file"), "worktree\n").unwrap();
    mutate(&repo, FileMutation::Unstage);
    assert_eq!(repo.git(&["show", ":file"]), "head\n");
    assert_eq!(
        fs::read_to_string(repo.0.join("file")).unwrap(),
        "worktree\n"
    );
    mutate(&repo, FileMutation::Stage);
    assert_eq!(repo.git(&["show", ":file"]), "worktree\n");
}

#[test]
fn discarding_restores_the_index_without_changing_staged_content() {
    let repo = Fixture::new();
    repo.commit("head\n");
    fs::write(repo.0.join("file"), "index\n").unwrap();
    repo.git(&["add", "file"]);
    fs::write(repo.0.join("file"), "worktree\n").unwrap();
    mutate(&repo, FileMutation::Discard);
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), "index\n");
    assert_eq!(repo.git(&["show", ":file"]), "index\n");
}

#[test]
fn literal_paths_can_be_staged_unstaged_and_deleted_before_the_first_commit() {
    let repo = Fixture::new();
    let name = ":(glob)*\t\n.txt";
    fs::write(repo.0.join(name), "new\n").unwrap();
    fs::write(repo.0.join("unselected"), "keep\n").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    for operation in [
        FileMutation::Stage,
        FileMutation::Unstage,
        FileMutation::Discard,
    ] {
        let entries = repository
            .status(&Execute)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.path == name)
            .collect();
        repository
            .mutate_files(FileMutationRequest { operation, entries }, &Execute)
            .unwrap();
    }
    assert!(!repo.0.join(name).exists());
    assert_eq!(
        fs::read_to_string(repo.0.join("unselected")).unwrap(),
        "keep\n"
    );
}

#[test]
fn unstaging_a_rename_restores_both_index_paths_in_a_linked_worktree() {
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
    let repository = Repository::open(&linked).unwrap();
    fs::rename(linked.join("file"), linked.join(":(glob)*new\n")).unwrap();
    let status = repository.status(&Execute).unwrap();
    repository
        .mutate_files(
            FileMutationRequest {
                operation: FileMutation::Stage,
                entries: status,
            },
            &Execute,
        )
        .unwrap();
    let entries = repository.status(&Execute).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].original_path.as_deref(), Some("file"));
    repository
        .mutate_files(
            FileMutationRequest {
                operation: FileMutation::Unstage,
                entries,
            },
            &Execute,
        )
        .unwrap();
    let output = std::process::Command::new("git")
        .args(["show", ":file"])
        .current_dir(&linked)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"head\n");
    assert_eq!(
        fs::read_to_string(linked.join(":(glob)*new\n")).unwrap(),
        "head\n"
    );
    assert_eq!(fs::read_to_string(repo.0.join("file")).unwrap(), "head\n");
}

#[test]
fn conflicts_reject_the_entire_selection_before_changing_the_index() {
    let repo = Fixture::new();
    repo.commit("head\n");
    let head = repo.git(&["rev-parse", "HEAD:file"]);
    let mut command = zess_platform::ProcessSpec::new("git");
    command
        .args(["update-index", "--index-info"])
        .current_dir(&repo.0);
    let index = format!(
        "0 {}\tfile\n100644 {} 1\tfile\n100644 {} 2\tfile\n100644 {} 3\tfile\n",
        "0".repeat(head.trim().len()),
        head.trim(),
        head.trim(),
        head.trim()
    );
    assert!(
        Execute
            .output_with_input(&command, index.as_bytes())
            .unwrap()
            .status
            .success()
    );
    fs::write(repo.0.join("new"), "new\n").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    let before = fs::read(repo.0.join(".git/index")).unwrap();
    for operation in [
        FileMutation::Stage,
        FileMutation::Unstage,
        FileMutation::Discard,
    ] {
        let entries = repository.status(&Execute).unwrap();
        assert_eq!(
            repository
                .mutate_files(FileMutationRequest { operation, entries }, &Execute)
                .unwrap_err()
                .to_string(),
            "Cannot change a selection containing unresolved conflicts"
        );
        assert_eq!(fs::read(repo.0.join(".git/index")).unwrap(), before);
        assert_eq!(fs::read_to_string(repo.0.join("new")).unwrap(), "new\n");
    }
}

#[test]
fn unstaging_before_the_first_commit_keeps_edits_made_after_staging() {
    let repo = Fixture::new();
    fs::write(repo.0.join("file"), "index\n").unwrap();
    repo.git(&["add", "file"]);
    fs::write(repo.0.join("file"), "worktree\n").unwrap();
    mutate(&repo, FileMutation::Unstage);
    assert_eq!(repo.git(&["ls-files"]), "");
    assert_eq!(
        fs::read_to_string(repo.0.join("file")).unwrap(),
        "worktree\n"
    );
}

#[test]
fn staging_edits_to_a_staged_rename_keeps_its_original_path_removed() {
    let repo = Fixture::new();
    repo.commit("head\n");
    repo.git(&["mv", "file", "renamed"]);
    fs::write(repo.0.join("renamed"), "worktree\n").unwrap();
    mutate(&repo, FileMutation::Stage);
    assert_eq!(repo.git(&["show", ":renamed"]), "worktree\n");
    assert_eq!(repo.git(&["ls-files"]), "renamed\n");
}

#[test]
fn staging_uses_the_configured_clean_filter() {
    let repo = Fixture::new();
    fs::write(repo.0.join(".gitattributes"), "file filter=example\n").unwrap();
    repo.git(&["config", "filter.example.clean", "sed s/worktree/stored/g"]);
    repo.git(&["config", "filter.example.smudge", "sed s/stored/worktree/g"]);
    repo.git(&["config", "filter.example.required", "true"]);
    fs::write(repo.0.join("file"), "worktree contents\n").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    let entries = repository
        .status(&Execute)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.path == "file")
        .collect();
    repository
        .mutate_files(
            FileMutationRequest {
                operation: FileMutation::Stage,
                entries,
            },
            &Execute,
        )
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "stored contents\n");
    assert_eq!(
        fs::read_to_string(repo.0.join("file")).unwrap(),
        "worktree contents\n"
    );
}

#[test]
fn discarding_uses_the_configured_smudge_filter_without_changing_the_index() {
    let repo = Fixture::new();
    fs::write(repo.0.join(".gitattributes"), "file filter=example\n").unwrap();
    repo.git(&["config", "filter.example.clean", "sed s/worktree/stored/g"]);
    repo.git(&["config", "filter.example.smudge", "sed s/stored/worktree/g"]);
    repo.git(&["config", "filter.example.required", "true"]);
    repo.commit("worktree original\n");
    fs::write(repo.0.join("file"), "local edits\n").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    let entries = repository
        .status(&Execute)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.path == "file")
        .collect();
    repository
        .mutate_files(
            FileMutationRequest {
                operation: FileMutation::Discard,
                entries,
            },
            &Execute,
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(repo.0.join("file")).unwrap(),
        "worktree original\n"
    );
    assert_eq!(repo.git(&["show", ":file"]), "stored original\n");
}

#[test]
fn staging_a_recreated_untracked_file_replaces_its_staged_deletion() {
    let repo = Fixture::new();
    repo.commit("head\n");
    repo.git(&["rm", "--cached", "file"]);
    fs::write(repo.0.join("file"), "recreated\n").unwrap();
    let repository = Repository::open(&repo.0).unwrap();
    let entries = repository.status(&Execute).unwrap();
    assert_eq!(
        entries.iter().filter(|entry| entry.path == "file").count(),
        2
    );
    let entries = entries
        .into_iter()
        .filter(|entry| entry.untracked)
        .collect();
    repository
        .mutate_files(
            FileMutationRequest {
                operation: FileMutation::Stage,
                entries,
            },
            &Execute,
        )
        .unwrap();
    assert_eq!(repo.git(&["show", ":file"]), "recreated\n");
    assert_eq!(
        fs::read_to_string(repo.0.join("file")).unwrap(),
        "recreated\n"
    );
}

#[test]
fn deleting_a_recreated_untracked_file_keeps_its_staged_deletion() {
    let repo = Fixture::new();
    repo.commit("head\n");
    repo.git(&["rm", "--cached", "file"]);
    let repository = Repository::open(&repo.0).unwrap();
    let entries = repository
        .status(&Execute)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.untracked)
        .collect();
    repository
        .mutate_files(
            FileMutationRequest {
                operation: FileMutation::Discard,
                entries,
            },
            &Execute,
        )
        .unwrap();
    assert!(!repo.0.join("file").exists());
    assert_eq!(repo.git(&["ls-files"]), "");
    assert_eq!(repo.git(&["show", "HEAD:file"]), "head\n");
}

#[cfg(unix)]
#[test]
fn discarding_untracked_symlinks_unlinks_them_without_following_their_targets() {
    use std::os::unix::fs::symlink;
    let repo = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("target"), "outside\n").unwrap();
    symlink(outside.0.join("target"), repo.0.join("outside-link")).unwrap();
    symlink("missing", repo.0.join("dangling-link")).unwrap();
    mutate(&repo, FileMutation::Discard);
    assert!(repo.0.join("outside-link").symlink_metadata().is_err());
    assert!(repo.0.join("dangling-link").symlink_metadata().is_err());
    assert_eq!(
        fs::read_to_string(outside.0.join("target")).unwrap(),
        "outside\n"
    );
}

#[cfg(unix)]
#[test]
fn discarding_a_changed_tracked_symlink_restores_the_index_link() {
    use std::os::unix::fs::symlink;
    let repo = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("target"), "outside\n").unwrap();
    symlink("original-target", repo.0.join("link")).unwrap();
    repo.git(&["add", "link"]);
    repo.git(&["commit", "--quiet", "-m", "link"]);
    fs::remove_file(repo.0.join("link")).unwrap();
    symlink(outside.0.join("target"), repo.0.join("link")).unwrap();
    mutate(&repo, FileMutation::Discard);
    assert_eq!(
        fs::read_link(repo.0.join("link")).unwrap(),
        std::path::Path::new("original-target")
    );
    assert_eq!(repo.git(&["show", ":link"]), "original-target");
    assert_eq!(
        fs::read_to_string(outside.0.join("target")).unwrap(),
        "outside\n"
    );
}
