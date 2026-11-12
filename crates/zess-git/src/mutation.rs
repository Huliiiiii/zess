use std::collections::BTreeSet;

use zess_platform::ProcessSpec;

use anyhow::{Context, Result, bail, ensure};

use crate::{Execution, GitEntry, Repository, StagingStatus, validate_relative};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq)]
pub enum FileMutation {
    Stage,
    Unstage,
    Discard,
}

pub struct FileMutationRequest {
    pub operation: FileMutation,
    pub entries: Vec<GitEntry>,
}

impl Repository {
    pub fn mutate_files(
        &self,
        request: FileMutationRequest,
        execution: &impl Execution,
    ) -> Result<()> {
        ensure!(!request.entries.is_empty(), "No Git changes selected");
        let current = self.status(execution)?;
        let mut paths = BTreeSet::new();
        for selected in &request.entries {
            validate_relative(&selected.path)?;
            ensure!(!selected.path.is_empty(), "Git path must not be empty");
            let entry = current
                .iter()
                .find(|entry| {
                    entry.path == selected.path
                        && entry.original_path == selected.original_path
                        && entry.index == selected.index
                        && entry.worktree == selected.worktree
                        && entry.untracked == selected.untracked
                })
                .context("Git changes changed; refresh and select the files again")?;
            ensure!(
                entry.staging_status() != StagingStatus::Conflict,
                "Cannot change a selection containing unresolved conflicts"
            );
            paths.insert(entry.path.clone());
            if (request.operation == FileMutation::Unstage
                || (request.operation == FileMutation::Stage && entry.worktree == "R"))
                && let Some(original) = &entry.original_path
            {
                validate_relative(original)?;
                paths.insert(original.clone());
            }
        }
        match request.operation {
            FileMutation::Stage => self.mutate_paths(&["add", "-A"], &paths, execution),
            FileMutation::Unstage => {
                let head = self.git(&["rev-parse", "--verify", "HEAD"], execution)?;
                if head.status.success() {
                    self.mutate_paths(&["reset", "--quiet", "HEAD"], &paths, execution)
                } else {
                    self.mutate_paths(
                        &[
                            "rm",
                            "--cached",
                            "--quiet",
                            "--ignore-unmatch",
                            "--force",
                            "-r",
                        ],
                        &paths,
                        execution,
                    )
                }
            }
            FileMutation::Discard => {
                let mut restore = BTreeSet::new();
                let mut delete = Vec::new();
                for entry in &request.entries {
                    let path = self.file_mutation_path(&entry.path)?;
                    let indexed = self.git_checked(
                        &[
                            "--literal-pathspecs",
                            "ls-files",
                            "--stage",
                            "-z",
                            "--",
                            &entry.path,
                        ],
                        execution,
                    )?;
                    ensure!(
                        !indexed.starts_with(b"160000 "),
                        "Discard submodule changes in the submodule repository"
                    );
                    if entry.worktree == "R"
                        && let Some(original) = &entry.original_path
                    {
                        validate_relative(original)?;
                        restore.insert(original.clone());
                    }
                    if entry.untracked || indexed.is_empty() {
                        if let Ok(metadata) = self.file_system.symlink_metadata(&path) {
                            ensure!(!metadata.is_dir(), "Cannot delete an untracked directory");
                            delete.push(path);
                        }
                    } else {
                        restore.insert(entry.path.clone());
                    }
                }
                if !restore.is_empty() {
                    self.mutate_paths(&["restore", "--worktree"], &restore, execution)?;
                }
                for path in delete {
                    self.file_system
                        .remove_file(&path)
                        .with_context(|| format!("delete untracked file {}", path.display()))?;
                }
                Ok(())
            }
        }
    }

    fn file_mutation_path(&self, relative: &str) -> Result<std::path::PathBuf> {
        validate_relative(relative)?;
        let path = std::path::Path::new(relative);
        let parent = path.parent().context("Git file must have a parent path")?;
        self.path(parent.to_str().context("Git path is not UTF-8")?)?;
        Ok(self.root.join(path))
    }

    // Mutation commands must preserve configured clean/smudge/process filters;
    // Repository::git_command deliberately disables them for read-only diffs.
    fn file_mutation_command(&self, args: &[&str]) -> ProcessSpec {
        let mut command = ProcessSpec::new("git");
        command
            .args([
                "--no-pager",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
            ])
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
        command
    }

    fn mutate_paths(
        &self,
        operation: &[&str],
        paths: &BTreeSet<String>,
        execution: &impl Execution,
    ) -> Result<()> {
        let mut args = vec!["--literal-pathspecs"];
        args.extend_from_slice(operation);
        args.extend(["--pathspec-from-file=-", "--pathspec-file-nul"]);
        let command = self.file_mutation_command(&args);
        let input = paths
            .iter()
            .flat_map(|path| path.bytes().chain([0]))
            .collect::<Vec<_>>();
        let output = execution.output_with_input(&command, &input)?;
        if !output.status.success() {
            bail!(
                "git {}: {}",
                operation.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}
