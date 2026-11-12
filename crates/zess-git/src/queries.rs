use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result};
use zess_platform::FileSystem;

use crate::{Blob, BlobRead, Execution, LineStats};

pub struct RepositoryLocation {
    pub worktree: PathBuf,
    pub common_dir: PathBuf,
}

/// Queries that read Git metadata or objects independently of the process runner.
pub trait GitQueries: Send + Sync {
    fn discover(&self, root: &Path) -> Result<Option<RepositoryLocation>>;
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

/// Uses native libgit2/gix repository IO; `FileSystem` supplies path and size checks.
pub struct NativeGitQueries {
    pub(crate) file_system: Arc<dyn FileSystem>,
}

impl NativeGitQueries {
    pub fn new(file_system: Arc<dyn FileSystem>) -> Self {
        Self { file_system }
    }
}

// Immutable Git blobs can be reused across workspaces and commits. The bound
// keeps browsing history from retaining every file read during a session.
type BlobCache = HashMap<(String, String), Arc<Vec<u8>>>;
static BLOBS: OnceLock<Mutex<BlobCache>> = OnceLock::new();
const BLOB_LIMIT: usize = 1024 * 1024;
const CACHE_LIMIT: usize = 8 * BLOB_LIMIT;

impl GitQueries for NativeGitQueries {
    fn discover(&self, root: &Path) -> Result<Option<RepositoryLocation>> {
        let path = match git2::Repository::discover_path(root, std::iter::empty::<&Path>()) {
            Ok(path) => path,
            Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("locate Git working tree in {}", root.display()));
            }
        };
        let repository = git2::Repository::open(&path)
            .with_context(|| format!("open Git repository at {}", path.display()))?;
        Ok(Some(RepositoryLocation {
            worktree: repository
                .workdir()
                .context("Git repository has no working directory")?
                .to_owned(),
            common_dir: repository.commondir().to_owned(),
        }))
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
        let repository = gix::discover(root)?;
        let repository_root = self.file_system.canonicalize(
            repository
                .workdir()
                .context("Git repository has no working directory")?,
        )?;
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
        let repository = gix::discover(root)?;
        let commit = repository
            .find_object(gix::ObjectId::from_hex(oid.as_bytes())?)?
            .peel_to_commit()?;
        let parent = commit.parent_ids().next().map(|oid| oid.to_string());
        cancellation.check()?;
        Ok(parent)
    }
}
