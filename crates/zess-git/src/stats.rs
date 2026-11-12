use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use git2::{
    Delta, Diff, DiffFindOptions, DiffFlags, DiffOptions, ErrorCode, Patch,
    Repository as GitRepository,
};

use super::LineStats;
use crate::{Execution, NativeGitQueries};
use zess_platform::FileSystem;

const UNTRACKED_SIZE_LIMIT: u64 = 1024 * 1024;

impl NativeGitQueries {
    pub(crate) fn git_worktree_counts(
        &self,
        root: &Path,
        cancellation: &dyn Execution,
    ) -> Result<HashMap<String, Option<LineStats>>> {
        cancellation.check()?;
        let repository = self
            .libgit2_repository(root)
            .with_context(|| format!("open Git repository at {}", root.display()))?
            .context("Git repository not found")?;
        let tree = match repository.head() {
            Ok(head) => Some(head.peel_to_tree().context("read HEAD tree")?),
            Err(error) if matches!(error.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => {
                None
            }
            Err(error) => return Err(error).context("resolve HEAD"),
        };
        let prefix = self.git_workspace_prefix(root, &repository)?;
        let mut options = diff_options(&prefix);
        options
            .show_untracked_content(true)
            .recurse_untracked_dirs(true);
        // This compares HEAD directly with the final worktree, including staged
        // additions, rather than adding the two sides of the index together.
        let mut diff = repository
            .diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut options))
            .context("compare HEAD with index and working tree")?;
        cancellation.check()?;
        diff.find_similar(Some(DiffFindOptions::new().renames(true)))
            .context("detect renamed files")?;
        diff_counts(
            &diff,
            &prefix,
            root,
            self.file_system.as_ref(),
            cancellation,
        )
    }

    pub(super) fn git_commit_counts(
        &self,
        root: &Path,
        oid: &str,
        cancellation: &dyn Execution,
    ) -> Result<HashMap<String, Option<LineStats>>> {
        cancellation.check()?;
        let repository = self
            .libgit2_repository(root)
            .with_context(|| format!("open Git repository at {}", root.display()))?
            .context("Git repository not found")?;
        let commit = repository
            .revparse_single(oid)?
            .peel_to_commit()
            .context("read commit for line counts")?;
        let tree = commit.tree()?;
        let parent = if commit.parent_count() == 0 {
            None
        } else {
            Some(commit.parent(0)?.tree()?)
        };
        let prefix = self.git_workspace_prefix(root, &repository)?;
        let mut options = diff_options(&prefix);
        let mut diff = repository
            .diff_tree_to_tree(parent.as_ref(), Some(&tree), Some(&mut options))
            .context("compare commit with its first parent")?;
        cancellation.check()?;
        diff.find_similar(Some(DiffFindOptions::new().renames(true)))
            .context("detect renamed files")?;
        // Commit name-status paths are repository-relative.
        diff_counts(
            &diff,
            Path::new(""),
            root,
            self.file_system.as_ref(),
            cancellation,
        )
    }

    pub(super) fn git_workspace_prefix(
        &self,
        workspace: &Path,
        repository: &GitRepository,
    ) -> Result<std::path::PathBuf> {
        let root = self.file_system.canonicalize(
            repository
                .workdir()
                .context("Git repository has no working directory")?,
        )?;
        Ok(workspace.strip_prefix(root)?.to_owned())
    }
}

fn diff_options(prefix: &Path) -> DiffOptions {
    let mut options = DiffOptions::new();
    options.include_typechange(true).context_lines(0);
    if !prefix.as_os_str().is_empty() {
        options.disable_pathspec_match(true).pathspec(prefix);
    }
    options
}

fn diff_counts(
    diff: &Diff<'_>,
    prefix: &Path,
    workspace: &Path,
    file_system: &dyn FileSystem,
    cancellation: &dyn Execution,
) -> Result<HashMap<String, Option<LineStats>>> {
    let mut counts = HashMap::new();
    // git2 exposes no cancellation callback for diff generation, rename
    // detection or an individual patch calculation. Keep those calls on the
    // service worker and observe cancellation between files and C calls.
    for (index, delta) in diff.deltas().enumerate() {
        cancellation.check()?;
        let file = delta.new_file();
        let path = file.path().context("Git delta has no path")?;
        let Ok(path) = path.strip_prefix(prefix) else {
            continue;
        };
        let path = path.to_str().context("Git path is not UTF-8")?;
        if delta.status() == Delta::Conflicted {
            counts.insert(path.into(), None);
            continue;
        }
        if delta.status() == Delta::Untracked {
            let metadata = match file_system.symlink_metadata(&workspace.join(path)) {
                Ok(metadata) => Some(metadata),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            if metadata.is_none_or(|metadata| {
                metadata.len() > UNTRACKED_SIZE_LIMIT
                    || !(metadata.is_file() || metadata.is_symlink())
            }) {
                counts.insert(path.into(), None);
                continue;
            }
        }
        let patch = Patch::from_diff(diff, index)
            .with_context(|| format!("read line changes in {path}"))?;
        let stats = if delta.flags().contains(DiffFlags::BINARY) {
            None
        } else {
            let (_, additions, deletions) = patch
                .map(|patch| patch.line_stats())
                .transpose()
                .with_context(|| format!("count changed lines in {path}"))?
                .unwrap_or((0, 0, 0));
            Some(LineStats {
                additions: additions as u64,
                deletions: deletions as u64,
            })
        };
        cancellation.check()?;
        counts.insert(path.into(), stats);
    }
    cancellation.check()?;
    Ok(counts)
}
