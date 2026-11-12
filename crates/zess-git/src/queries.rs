use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result};
use zess_platform::FileSystem;

use crate::ignore::IgnoreFiles;
use crate::{Blob, BlobRead, Execution, LineStats, WorktreeDirectories};

pub struct RepositoryLocation {
    pub worktree: PathBuf,
    pub common_dir: PathBuf,
}

/// Queries that read Git metadata or objects independently of the process runner.
pub trait GitQueries: Send + Sync {
    fn discover(&self, root: &Path) -> Result<Option<RepositoryLocation>>;
    fn ignore_files(&self, _directory: &Path) -> IgnoreFiles {
        IgnoreFiles::default()
    }
    fn worktree_counts(
        &self,
        root: &Path,
        execution: &dyn Execution,
    ) -> Result<HashMap<String, Option<LineStats>>>;
    fn commit_counts(
        &self,
        root: &Path,
        commit: &str,
        execution: &dyn Execution,
    ) -> Result<HashMap<String, Option<LineStats>>>;
    fn blob(
        &self,
        root: &Path,
        commit: &str,
        path: &str,
        execution: &dyn Execution,
    ) -> Result<BlobRead>;
    fn commit_parent(
        &self,
        root: &Path,
        commit: &str,
        execution: &dyn Execution,
    ) -> Result<Option<String>>;
}

impl WorktreeDirectories {
    fn open_gix(&self, filesystem: &dyn FileSystem) -> Result<(gix::Repository, Option<PathBuf>)> {
        use gix::sec::trust::DefaultForLevel;

        let trust = gix::discover::repository::trust(
            &self.git,
            Some(&self.root),
            &std::env::current_dir()?,
            (&self.root.join(".git"), None),
        )?;
        let repository = gix::open_opts(
            &self.git,
            gix::open::Options::default_for_level(trust)
                .with(trust)
                .open_path_as_is(true),
        )?;
        self.verify_common_directory(filesystem, repository.common_dir())?;
        let worktree = if repository.is_bare() {
            None
        } else {
            // Opening a metadata directory directly loses the gitfile's worktree hint.
            let configured = repository
                .config_snapshot()
                .string("core.worktree")
                .is_some();
            let worktree = repository
                .workdir()
                .filter(|_| configured)
                .unwrap_or(&self.root);
            Some(worktree.to_owned())
        };
        Ok((repository, worktree))
    }
}

fn find_gitfile(root: &Path, filesystem: &dyn FileSystem) -> Result<Option<WorktreeDirectories>> {
    let device = filesystem.device_id(root)?;
    for ancestor in root.ancestors() {
        if ancestor != root && device.is_some() && filesystem.device_id(ancestor)? != device {
            return Ok(None);
        }
        let marker = ancestor.join(".git");
        match filesystem.metadata(&marker) {
            Ok(metadata) if !metadata.is_dir() => {
                return crate::resolve_worktree_directories(ancestor, marker, true, filesystem)
                    .map(Some);
            }
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", marker.display()));
            }
        }
        if gix::discover::is_git(ancestor).is_ok() {
            return Ok(None);
        }
    }
    Ok(None)
}

/// Uses native libgit2/gix repository IO; `FileSystem` supplies path and size checks.
pub struct NativeGitQueries {
    pub(crate) file_system: Arc<dyn FileSystem>,
}

impl NativeGitQueries {
    pub fn new(file_system: Arc<dyn FileSystem>) -> Self {
        Self { file_system }
    }

    pub(super) fn libgit2_repository(&self, root: &Path) -> Result<Option<git2::Repository>> {
        let filesystem = self.file_system.as_ref();
        let Some(directories) = find_gitfile(root, filesystem)? else {
            return match git2::Repository::discover(root) {
                Ok(repository) => Ok(Some(repository)),
                Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            };
        };
        let repository = git2::Repository::open_ext(
            &directories.git,
            git2::RepositoryOpenFlags::NO_SEARCH | git2::RepositoryOpenFlags::NO_DOTGIT,
            std::iter::empty::<&Path>(),
        )?;
        directories.verify_common_directory(filesystem, repository.commondir())?;
        if !repository.is_bare() {
            let worktree = match repository.config()?.get_string("core.worktree") {
                Ok(path) => directories.git.join(path),
                Err(error) if error.code() == git2::ErrorCode::NotFound => directories.root,
                Err(error) => return Err(error.into()),
            };
            repository.set_workdir(&worktree, false)?;
        }
        Ok(Some(repository))
    }
}

pub(super) fn gix_repository(
    root: &Path,
    filesystem: &dyn FileSystem,
) -> Result<(gix::Repository, Option<PathBuf>)> {
    let Some(directories) = find_gitfile(root, filesystem)? else {
        let repository = gix::discover(root)?;
        let worktree = repository.workdir().map(Path::to_owned);
        return Ok((repository, worktree));
    };
    directories.open_gix(filesystem)
}

// Immutable Git blobs can be reused across workspaces and commits. The bound
// keeps browsing history from retaining every file read during a session.
type BlobCache = HashMap<(String, String), Arc<Vec<u8>>>;
static BLOBS: OnceLock<Mutex<BlobCache>> = OnceLock::new();
const BLOB_LIMIT: usize = 1024 * 1024;
const CACHE_LIMIT: usize = 8 * BLOB_LIMIT;

impl GitQueries for NativeGitQueries {
    fn discover(&self, root: &Path) -> Result<Option<RepositoryLocation>> {
        let Some(repository) = self
            .libgit2_repository(root)
            .with_context(|| format!("locate Git working tree in {}", root.display()))?
        else {
            return Ok(None);
        };
        Ok(Some(RepositoryLocation {
            worktree: repository
                .workdir()
                .context("Git repository has no working directory")?
                .to_owned(),
            common_dir: repository.commondir().to_owned(),
        }))
    }

    fn ignore_files(&self, directory: &Path) -> IgnoreFiles {
        crate::ignore::ignore_files(directory, self.file_system.as_ref())
    }

    fn worktree_counts(
        &self,
        root: &Path,
        execution: &dyn Execution,
    ) -> Result<HashMap<String, Option<LineStats>>> {
        self.git_worktree_counts(root, execution)
    }

    fn commit_counts(
        &self,
        root: &Path,
        commit: &str,
        execution: &dyn Execution,
    ) -> Result<HashMap<String, Option<LineStats>>> {
        self.git_commit_counts(root, commit, execution)
    }

    fn blob(
        &self,
        root: &Path,
        oid: &str,
        path: &str,
        cancellation: &dyn Execution,
    ) -> Result<BlobRead> {
        let (repository, worktree) = gix_repository(root, self.file_system.as_ref())?;
        let repository_root = self
            .file_system
            .canonicalize(&worktree.context("Git repository has no working directory")?)?;
        let prefix = root.strip_prefix(&repository_root)?;
        let tree = repository
            .find_object(gix::ObjectId::from_hex(oid.as_bytes())?)?
            .peel_to_tree()?;
        let entry = match tree.lookup_entry_by_path(prefix.join(path))? {
            Some(entry) => entry,
            None => {
                return Ok(BlobRead::Missing);
            }
        };
        if !matches!(
            entry.mode().kind(),
            gix::objs::tree::EntryKind::Blob | gix::objs::tree::EntryKind::BlobExecutable
        ) {
            return Ok(BlobRead::UnsupportedObject);
        }
        let blob = entry.object_id();
        let repository_root = repository_root
            .to_str()
            .context("Git repository path is not UTF-8")?
            .to_owned();
        let key = (repository_root.clone(), blob.to_string());
        let cache = BLOBS.get_or_init(Mutex::default);
        let cached = cache.lock().unwrap().get(&key).cloned();
        let bytes = if let Some(bytes) = cached {
            bytes
        } else {
            cancellation.check()?;
            if repository.find_header(blob)?.size() > BLOB_LIMIT as u64 {
                return Ok(BlobRead::TooLarge);
            }
            let object = repository.find_blob(blob)?;
            let bytes = Arc::new(object.detach().data);
            cancellation.check()?;
            let mut cache = cache.lock().unwrap();
            if cache.values().map(|bytes| bytes.len()).sum::<usize>() + bytes.len() > CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(key, bytes.clone());
            bytes
        };
        cancellation.check()?;
        Ok(BlobRead::File(Blob {
            repository: repository_root,
            commit: oid.into(),
            path: path.into(),
            oid: blob.to_string(),
            bytes,
        }))
    }

    fn commit_parent(
        &self,
        root: &Path,
        oid: &str,
        cancellation: &dyn Execution,
    ) -> Result<Option<String>> {
        cancellation.check()?;
        let (repository, _) = gix_repository(root, self.file_system.as_ref())?;
        let commit = repository
            .find_object(gix::ObjectId::from_hex(oid.as_bytes())?)?
            .peel_to_commit()?;
        let parent = commit.parent_ids().next().map(|oid| oid.to_string());
        cancellation.check()?;
        Ok(parent)
    }
}
