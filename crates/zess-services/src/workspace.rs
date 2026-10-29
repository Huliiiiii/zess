use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::filesystem::{FileOperation, Request};
use crate::jobs::Cancellation;
use crate::query::{QueryRequest, QueryResult};

#[derive(Clone)]
pub(crate) struct Workspace {
    pub(crate) root: PathBuf,
    pub(crate) git_colors: std::sync::Arc<std::sync::Mutex<crate::git_colors::GitColors>>,
    #[cfg(test)]
    pub(crate) read_hook: Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>,
}

impl Workspace {
    pub(crate) fn query(
        &self,
        request: &QueryRequest,
        cancellation: &Cancellation,
    ) -> Result<QueryResult> {
        self.query_with_progress(request, cancellation, None)
    }

    pub(crate) fn query_with_progress(
        &self,
        request: &QueryRequest,
        cancellation: &Cancellation,
        status_progress: Option<&std::sync::mpsc::Sender<Vec<zess_git::GitEntry>>>,
    ) -> Result<QueryResult> {
        cancellation.check()?;
        if !matches!(
            request,
            QueryRequest::ReadFile { .. }
                | QueryRequest::List { .. }
                | QueryRequest::Preview { .. }
                | QueryRequest::GitDecorations {}
        ) {
            self.refresh_git_colors(cancellation)?;
        }
        match request {
            QueryRequest::ReadFile { path } => {
                anyhow::ensure!(path.is_absolute(), "file read requires an absolute path");
                let text =
                    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
                cancellation.check()?;
                Ok(QueryResult::Text {
                    path: path.to_string_lossy().into_owned(),
                    text,
                    truncated: false,
                })
            }
            QueryRequest::List { .. } | QueryRequest::Preview { .. } => {
                let (path, request) = Request::from_query(request)?;
                self.read_fs(path, cancellation)?.response(path, request)
            }
            QueryRequest::Status {} => {
                let entries = zess_git::Repository::open(&self.root)?.status_with_progress(
                    cancellation,
                    |entries| {
                        if let Some(progress) = status_progress {
                            let _ = progress.send(entries.to_owned());
                        }
                        #[cfg(test)]
                        if let Some(hook) = &self.read_hook {
                            hook("git.status");
                        }
                    },
                )?;
                Ok(QueryResult::Status { entries })
            }
            QueryRequest::GitDecorations {} => {
                let entries = zess_git::Repository::open(&self.root)?.decorations(cancellation)?;
                #[cfg(test)]
                if let Some(hook) = &self.read_hook {
                    hook("git.decorations");
                }
                Ok(QueryResult::GitDecorations { entries })
            }
            QueryRequest::Diff {
                path,
                original_path,
                side,
            } => Ok(QueryResult::Diff {
                path: path.clone(),
                text: zess_git::Repository::open(&self.root)?.diff(
                    path,
                    original_path.as_deref(),
                    *side,
                    cancellation,
                )?,
            }),
            QueryRequest::DiffFiles {
                source,
                files,
                render,
            } => {
                let repository = zess_git::Repository::open(&self.root)?;
                let renderer = repository.diff_renderer(render, cancellation)?;
                let mut diffs = Vec::with_capacity(files.len());
                for target in files {
                    cancellation.check()?;
                    let raw = match source {
                        zess_git::DiffSource::Changes { side } => repository.diff(
                            &target.path,
                            target.original_path.as_deref(),
                            Some(*side),
                            cancellation,
                        )?,
                        zess_git::DiffSource::Commit { commit } => repository.commit_diff(
                            commit,
                            &target.path,
                            target.original_path.as_deref(),
                            cancellation,
                        )?,
                    };
                    diffs.push(zess_git::FileDiff {
                        target: target.clone(),
                        rendered: renderer.render(
                            &repository,
                            source,
                            target,
                            &raw,
                            cancellation,
                        )?,
                        raw,
                    });
                }
                cancellation.check()?;
                Ok(QueryResult::GitDiff {
                    diff: zess_git::GitDiff {
                        source: source.clone(),
                        files: diffs,
                    },
                })
            }
            QueryRequest::Log {} => Ok(QueryResult::Log {
                commits: zess_git::Repository::open(&self.root)?.log(cancellation)?,
            }),
            QueryRequest::CommitFiles { commit } => Ok(QueryResult::CommitFiles {
                files: zess_git::Repository::open(&self.root)?
                    .commit_files(commit, cancellation)?,
            }),
            QueryRequest::CommitDiff {
                commit,
                path,
                original_path,
            } => Ok(QueryResult::Diff {
                path: path.clone(),
                text: zess_git::Repository::open(&self.root)?.commit_diff(
                    commit,
                    path,
                    original_path.as_deref(),
                    cancellation,
                )?,
            }),
            QueryRequest::Blob { commit, path } => {
                let blob = match zess_git::Repository::open(&self.root)?.blob(
                    commit,
                    path,
                    cancellation,
                )? {
                    zess_git::BlobRead::File(blob) => blob,
                    zess_git::BlobRead::Missing => {
                        return Ok(QueryResult::Unsupported {
                            message: "This file does not exist in this commit".into(),
                        });
                    }
                    zess_git::BlobRead::UnsupportedObject => {
                        return Ok(QueryResult::Unsupported {
                            message: "Preview unavailable for this Git object".into(),
                        });
                    }
                    zess_git::BlobRead::TooLarge => {
                        return Ok(QueryResult::Unsupported {
                            message: "Historical file exceeds the preview size limit".into(),
                        });
                    }
                };
                if crate::filesystem::is_image(path, &blob.bytes) {
                    // TODO: render historical image previews.
                    return Ok(QueryResult::Unsupported {
                        message: "Image preview is not implemented".into(),
                    });
                }
                if blob.bytes.contains(&0) || std::str::from_utf8(&blob.bytes).is_err() {
                    // TODO: render historical binary previews.
                    return Ok(QueryResult::Unsupported {
                        message: "Binary preview is not implemented".into(),
                    });
                }
                let snapshot = zess_plugin::FileSnapshot {
                    id: zess_plugin::SnapshotId {
                        repository: blob.repository,
                        commit: blob.commit,
                        path: blob.path,
                        blob: blob.oid,
                    },
                    text: std::str::from_utf8(&blob.bytes)
                        .context("historical file is not UTF-8")?
                        .to_owned(),
                };
                Ok(QueryResult::Snapshot { snapshot })
            }
        }
    }

    pub(crate) fn path(&self, relative: &str) -> Result<PathBuf> {
        let relative = Path::new(relative);

        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            bail!("path must remain relative to the workspace");
        }

        let path = self.root.join(relative);
        let mut ancestor = path.as_path();

        while !ancestor.try_exists()? {
            if ancestor.symlink_metadata().is_ok() {
                bail!("broken symbolic link in workspace path");
            }

            ancestor = ancestor.parent().context("invalid workspace path")?;
        }

        if !ancestor.canonicalize()?.starts_with(&self.root) {
            bail!("symbolic link leaves the workspace");
        }

        Ok(path)
    }

    pub(crate) fn validate_operations(&self, operations: &[FileOperation]) -> Result<()> {
        let mut prior: Vec<&FileOperation> = Vec::new();

        for operation in operations {
            let relative = operation.path();
            let path = self.path(relative)?;

            if path == self.root {
                bail!("the workspace root cannot be changed");
            }

            let exists = self.virtual_kind(relative, &prior)?;

            match operation {
                FileOperation::Create { .. } => {
                    if exists.is_some() {
                        bail!("path already exists: {relative}");
                    }

                    self.validate_parent(relative, &prior)?;
                }
                FileOperation::Rename { to, .. } => {
                    if exists.is_none() {
                        bail!("path does not exist: {relative}");
                    }

                    let destination = self.path(to)?;

                    if destination == self.root || destination.starts_with(&path) {
                        bail!("cannot move a path into itself: {relative}");
                    }

                    if self.virtual_kind(to, &prior)?.is_some() {
                        bail!("destination already exists: {to}");
                    }

                    self.validate_parent(to, &prior)?;
                }
                FileOperation::Delete { .. } => {
                    if exists.is_none() {
                        bail!("path does not exist: {relative}");
                    }
                }
            }

            prior.push(operation);
        }

        Ok(())
    }

    fn validate_parent(&self, relative: &str, prior: &[&FileOperation]) -> Result<()> {
        let parent = Path::new(relative).parent().unwrap_or(Path::new(""));
        let parent = parent.to_str().context("path is not UTF-8")?;

        if self.virtual_kind(parent, prior)? != Some(true) {
            bail!("parent directory does not exist: {parent}");
        }

        Ok(())
    }

    fn virtual_kind(&self, relative: &str, prior: &[&FileOperation]) -> Result<Option<bool>> {
        let mut path = PathBuf::from(relative);

        for operation in prior.iter().rev() {
            let source = Path::new(operation.path());

            match operation {
                FileOperation::Create { directory, .. } if path == source => {
                    return Ok(Some(*directory));
                }
                FileOperation::Delete { .. } if path.starts_with(source) => return Ok(None),
                FileOperation::Rename { to, .. } => {
                    let destination = Path::new(to);

                    if let Ok(suffix) = path.strip_prefix(destination) {
                        path = source.join(suffix);
                    } else if path.starts_with(source) {
                        return Ok(None);
                    }
                }
                _ => {}
            }
        }

        match self
            .path(path.to_str().context("path is not UTF-8")?)?
            .symlink_metadata()
        {
            Ok(metadata) => Ok(Some(metadata.is_dir())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn apply_operation(&self, operation: &FileOperation) -> Result<()> {
        let path = self.path(operation.path())?;

        match operation {
            FileOperation::Create { directory, .. } => {
                if *directory {
                    fs::create_dir(path)?;
                } else {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)?;
                }
            }
            FileOperation::Rename { to, .. } => {
                let to = self.path(to)?;

                if to.try_exists()? || to.symlink_metadata().is_ok() {
                    bail!("destination already exists");
                }

                fs::rename(path, to)?;
            }
            FileOperation::Delete { .. } => {
                if path.symlink_metadata()?.is_dir() {
                    fs::remove_dir_all(path)?;
                } else {
                    fs::remove_file(path)?;
                }
            }
        }

        Ok(())
    }
}
