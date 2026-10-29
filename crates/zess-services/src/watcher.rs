use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use zess_git::{DiffSource, DiffTarget};

use crate::QueryRequest;
use crate::fs_cache;
use crate::jobs::{Cancellation, Job, JobScheduler};
use crate::workspace::Workspace;

const INVALIDATION_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) enum QueryKey {
    OpenFile(PathBuf),
    Filesystem(PathBuf),
    GitStatus,
    GitLog,
    GitCommitFiles {
        commit: String,
        path: Option<String>,
    },
    GitBlob {
        commit: String,
        path: Option<String>,
    },
    GitCommitDiff {
        commit: String,
        path: String,
        original_path: Option<String>,
    },
    GitDiff {
        path: PathBuf,
        original: Option<PathBuf>,
    },
    GitDiffFiles {
        source: DiffSource,
        files: Vec<DiffTarget>,
    },
}

impl QueryKey {
    pub(crate) fn from_request(request: &QueryRequest) -> Result<Self> {
        match request {
            QueryRequest::ReadFile { path } => {
                anyhow::ensure!(
                    path.is_absolute(),
                    "file observation requires an absolute path"
                );
                Ok(Self::OpenFile(path.clone()))
            }
            QueryRequest::List { path } | QueryRequest::Preview { path, .. } => {
                Self::filesystem(path)
            }
            QueryRequest::Status {} | QueryRequest::GitDecorations {} => Ok(Self::GitStatus),
            QueryRequest::Log {} => Ok(Self::GitLog),
            QueryRequest::CommitFiles { commit } => Ok(Self::GitCommitFiles {
                commit: commit.clone(),
                path: None,
            }),
            QueryRequest::Blob { commit, path } => Ok(Self::GitBlob {
                commit: commit.clone(),
                path: Some(path.clone()),
            }),
            QueryRequest::CommitDiff {
                commit,
                path,
                original_path,
            } => Ok(Self::GitCommitDiff {
                commit: commit.clone(),
                path: path.clone(),
                original_path: original_path.clone(),
            }),
            QueryRequest::Diff {
                path,
                original_path,
                ..
            } => Self::diff(path, original_path.as_deref()),
            QueryRequest::DiffFiles { source, files, .. } => {
                for target in files {
                    fs_cache::key(&target.path)?;
                    if let Some(original) = &target.original_path {
                        fs_cache::key(original)?;
                    }
                }
                Ok(Self::GitDiffFiles {
                    source: source.clone(),
                    files: files.clone(),
                })
            }
        }
    }

    pub(crate) fn from_call(operation: &str, args: &Value) -> Result<Self> {
        match operation {
            "fs.list" | "fs.preview" => {
                if operation == "fs.preview" {
                    crate::string(args, "path")?;
                }
                let relative = args["path"].as_str().unwrap_or("");
                Self::filesystem(relative)
            }
            "git.status" | "git.decorations" => Ok(Self::GitStatus),
            "git.log" => Ok(Self::GitLog),
            "git.commit-files" => Ok(Self::GitCommitFiles {
                commit: crate::string(args, "commit")?.into(),
                path: args["path"].as_str().map(str::to_owned),
            }),
            "git.blob" => Ok(Self::GitBlob {
                commit: crate::string(args, "commit")?.into(),
                path: args["path"].as_str().map(str::to_owned),
            }),
            "git.commit-diff" => Ok(Self::GitCommitDiff {
                commit: crate::string(args, "commit")?.into(),
                path: crate::string(args, "path")?.into(),
                original_path: args["original_path"].as_str().map(str::to_owned),
            }),
            "git.diff" => {
                let path = crate::string(args, "path")?;
                let original = args["original_path"].as_str();
                Self::diff(path, original)
            }
            "git.diff-files" => Self::from_request(&QueryRequest::from_call(operation, args)?),
            _ => bail!("operation cannot be observed: {operation}"),
        }
    }

    fn filesystem(relative: &str) -> Result<Self> {
        let path = fs_cache::key(relative)?;
        Ok(Self::Filesystem(path))
    }

    fn diff(path: &str, original: Option<&str>) -> Result<Self> {
        Ok(Self::GitDiff {
            path: fs_cache::key(path)?,
            original: original.map(fs_cache::key).transpose()?,
        })
    }
}

#[derive(Default)]
struct Signal {
    dirty: AtomicBool,
    revision: AtomicU64,
}

pub(crate) struct Query {
    pub(crate) key: QueryKey,
    signal: Arc<Signal>,
    binding: Mutex<Binding>,
}

impl Query {
    pub(crate) fn revision(&self) -> u64 {
        self.signal.revision.load(Ordering::Relaxed)
    }

    pub(crate) fn registration(&self) -> Arc<Registration> {
        self.binding.lock().unwrap().registration.clone()
    }

    pub(crate) fn cache_is_current(&self, registration: &Arc<Registration>) -> bool {
        let binding = self.binding.lock().unwrap();
        Arc::ptr_eq(&binding.registration, registration)
            && registration.ready() == Ok(true)
            && !self.signal.dirty.load(Ordering::Relaxed)
            && binding
                .watches
                .iter()
                .all(|watch| !watch.owner.stale.load(Ordering::Relaxed))
    }
}

pub(crate) struct Registration(Mutex<RegistrationState>);

enum RegistrationState {
    Pending,
    Ready,
    Failed(String),
}

impl Registration {
    fn new() -> Self {
        Self(Mutex::new(RegistrationState::Pending))
    }

    pub(crate) fn ready(&self) -> Result<bool, String> {
        match &*self.0.lock().unwrap() {
            RegistrationState::Pending => Ok(false),
            RegistrationState::Ready => Ok(true),
            RegistrationState::Failed(error) => Err(error.clone()),
        }
    }
}

/// Keeps a query's watches alive and tracks this consumer's acknowledged changes.
pub struct Observation {
    query: Arc<Query>,
    seen: u64,
    pending: u64,
}

impl Observation {
    pub(crate) fn new(query: Arc<Query>) -> Self {
        let revision = query.revision();
        Self {
            query,
            seen: revision,
            pending: revision,
        }
    }

    pub fn changed(&mut self) -> bool {
        self.pending = self.query.revision();
        self.pending != self.seen
    }

    /// Acknowledges the revision captured by the last `changed` call.
    pub fn acknowledge(&mut self) {
        self.seen = self.pending;
    }
}

struct Binding {
    watches: Vec<WatchLease>,
    registration: Arc<Registration>,
    last_invalidation: Option<Instant>,
}

struct Preparation {
    query: Weak<Query>,
    registration: Arc<Registration>,
    job: Job<Result<Vec<WatchLease>, String>>,
}

pub(crate) struct WatchRequest {
    registry: Arc<Mutex<WatchRegistry>>,
    query: Weak<Query>,
    signal: Arc<Signal>,
    key: QueryKey,
}

impl WatchRequest {
    pub(crate) fn execute(
        &self,
        workspace: &Workspace,
        cancellation: &Cancellation,
    ) -> Result<Vec<WatchLease>> {
        let check = || {
            cancellation.check()?;
            anyhow::ensure!(
                self.query.strong_count() > 0,
                "watch subscription was released"
            );
            Ok::<_, anyhow::Error>(())
        };
        check()?;
        let plan = Plan::new(workspace, &self.key)?;
        let metadata = Arc::<[PathBuf]>::from(plan.metadata);
        let mut registry = self.registry.lock().unwrap();
        let mut watches = Vec::new();
        for dependency in plan.dependencies {
            check()?;
            watches.push(registry.subscribe(dependency, &self.signal, metadata.clone())?);
        }
        check()?;
        Ok(watches)
    }
}

pub(crate) struct Queries {
    entries: HashMap<QueryKey, Weak<Query>>,
    registry: Arc<Mutex<WatchRegistry>>,
    pending: VecDeque<Weak<Query>>,
    preparation: Option<Preparation>,
    errors: VecDeque<String>,
    backend_error: Arc<Mutex<Option<String>>>,
    watched_paths: Arc<AtomicUsize>,
}

impl Default for Queries {
    fn default() -> Self {
        let backend_error = Arc::new(Mutex::new(None));
        let watched_paths = Arc::new(AtomicUsize::new(0));
        Self {
            entries: HashMap::new(),
            registry: Arc::new(Mutex::new(WatchRegistry {
                error: backend_error.clone(),
                watched_paths: watched_paths.clone(),
                ..WatchRegistry::default()
            })),
            pending: VecDeque::new(),
            preparation: None,
            errors: VecDeque::new(),
            backend_error,
            watched_paths,
        }
    }
}

impl Queries {
    pub(crate) fn acquire(
        &mut self,
        key: QueryKey,
        scheduler: &JobScheduler,
    ) -> Result<(Arc<Query>, bool)> {
        self.entries.retain(|_, query| query.strong_count() > 0);
        if let Some(query) = self.entries.get(&key).and_then(Weak::upgrade) {
            if query.registration().ready().is_err() {
                query.binding.lock().unwrap().registration = Arc::new(Registration::new());
                self.pending.push_back(Arc::downgrade(&query));
            }
            self.advance(scheduler);
            return Ok((query, false));
        }
        let query = Arc::new(Query {
            key: key.clone(),
            signal: Arc::default(),
            binding: Mutex::new(Binding {
                watches: Vec::new(),
                registration: Arc::new(Registration::new()),
                last_invalidation: None,
            }),
        });
        self.entries.insert(key, Arc::downgrade(&query));
        self.pending.push_back(Arc::downgrade(&query));
        self.advance(scheduler);
        Ok((query, true))
    }

    pub(crate) fn advance(&mut self, scheduler: &JobScheduler) {
        if let Some(preparation) = &self.preparation {
            if preparation.query.strong_count() == 0 {
                preparation.job.cancel();
            }
            let result = match preparation.job.result.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("watch preparation stopped without a result".into()))
                }
            };
            if let Some(result) = result {
                let preparation = self.preparation.take().unwrap();
                if let Some(query) = preparation.query.upgrade() {
                    match result {
                        Ok(watches) => {
                            let mut binding = query.binding.lock().unwrap();
                            binding.watches = watches;
                            *preparation.registration.0.lock().unwrap() = RegistrationState::Ready;
                            // Queries take their first snapshot after registration. Editor files
                            // are read before registration, so they also need an initial refresh.
                            if binding.last_invalidation.is_none()
                                && matches!(query.key, QueryKey::OpenFile(_))
                            {
                                query.signal.revision.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(error) => {
                            *preparation.registration.0.lock().unwrap() =
                                RegistrationState::Failed(error.clone());
                            self.errors.push_back(error);
                        }
                    }
                }
            }
        }
        if self.preparation.is_some() {
            return;
        }
        while let Some(pending) = self.pending.pop_front() {
            let Some(query) = pending.upgrade() else {
                continue;
            };
            let registration = query.registration();
            let request = WatchRequest {
                registry: self.registry.clone(),
                query: pending.clone(),
                signal: query.signal.clone(),
                key: query.key.clone(),
            };
            match scheduler.start_watch(request) {
                Ok(Some(job)) => {
                    self.preparation = Some(Preparation {
                        query: pending,
                        registration,
                        job,
                    })
                }
                Ok(None) => self.pending.push_front(pending),
                Err(error) => {
                    let error = format!("{error:#}");
                    *registration.0.lock().unwrap() = RegistrationState::Failed(error.clone());
                    self.errors.push_back(error);
                }
            }
            break;
        }
    }

    pub(crate) fn poll(&mut self, scheduler: &JobScheduler) -> (Vec<QueryKey>, Result<()>) {
        self.advance(scheduler);
        self.entries.retain(|_, query| query.strong_count() > 0);
        let live = self
            .entries
            .values()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        let mut changed = Vec::new();
        for query in live {
            let mut binding = query.binding.lock().unwrap();
            if binding.registration.ready() != Ok(true)
                || binding
                    .last_invalidation
                    .is_some_and(|last| last.elapsed() < INVALIDATION_INTERVAL)
                || !query.signal.dirty.swap(false, Ordering::Relaxed)
            {
                continue;
            }
            binding.registration = Arc::new(Registration::new());
            binding.last_invalidation = Some(Instant::now());
            self.pending.push_back(Arc::downgrade(&query));
            query.signal.revision.fetch_add(1, Ordering::Relaxed);
            changed.push(query.key.clone());
        }
        self.advance(scheduler);
        let error = self
            .errors
            .pop_front()
            .or_else(|| self.backend_error.lock().unwrap().take());
        (
            changed,
            error.map_or(Ok(()), |error| Err(anyhow::anyhow!(error))),
        )
    }

    pub(crate) fn len(&self) -> usize {
        self.entries
            .values()
            .filter(|query| query.strong_count() > 0)
            .count()
    }

    pub(crate) fn watched_paths(&self) -> usize {
        self.watched_paths.load(Ordering::Relaxed)
    }
}

impl Drop for Queries {
    fn drop(&mut self) {
        if let Some(preparation) = &self.preparation {
            preparation.job.cancel();
        }
    }
}

#[derive(Eq, Hash, PartialEq)]
struct WatchSpec {
    path: PathBuf,
    recursive: bool,
}

#[derive(PartialEq)]
enum Filter {
    Exact(PathBuf),
    Directory(PathBuf),
    Tree(PathBuf),
}

impl Filter {
    fn matches(&self, path: &Path) -> bool {
        match self {
            Self::Exact(target) => target.starts_with(path),
            Self::Directory(directory) => path == directory || path.parent() == Some(directory),
            Self::Tree(directory) => path.starts_with(directory),
        }
    }
}

#[derive(PartialEq)]
struct Dependency {
    spec: WatchSpec,
    filter: Filter,
}

struct Listener {
    signal: Weak<Signal>,
    filter: Filter,
    metadata: Arc<[PathBuf]>,
}

impl Listener {
    fn accepts(&self, event: &Event) -> bool {
        if event.need_rescan() || event.paths.is_empty() {
            return true;
        }
        event.paths.iter().any(|path| {
            let git_metadata = self
                .metadata
                .iter()
                .any(|directory| path.starts_with(directory));
            let lock_file = path
                .file_name()
                .is_some_and(|name| name.as_encoded_bytes().ends_with(b".lock"));
            if git_metadata && lock_file {
                return false;
            }
            self.filter.matches(path)
        })
    }
}

struct PhysicalWatch {
    watched_paths: Arc<AtomicUsize>,
    watcher: Mutex<RecommendedWatcher>,
    stale: Arc<AtomicBool>,
    listeners: Arc<Mutex<HashMap<u64, Listener>>>,
}

pub(crate) struct WatchLease {
    owner: Arc<PhysicalWatch>,
    listener: u64,
}

impl Drop for PhysicalWatch {
    fn drop(&mut self) {
        self.watched_paths.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Drop for WatchLease {
    fn drop(&mut self) {
        self.owner.listeners.lock().unwrap().remove(&self.listener);
    }
}

#[derive(Default)]
struct WatchRegistry {
    entries: HashMap<WatchSpec, Weak<PhysicalWatch>>,
    error: Arc<Mutex<Option<String>>>,
    next_listener: u64,
    watched_paths: Arc<AtomicUsize>,
}

impl WatchRegistry {
    fn subscribe(
        &mut self,
        dependency: Dependency,
        signal: &Arc<Signal>,
        metadata: Arc<[PathBuf]>,
    ) -> Result<WatchLease> {
        self.entries.retain(|_, watch| watch.strong_count() > 0);
        let spec = dependency.spec;
        let owner = if let Some(owner) = self.entries.get(&spec).and_then(Weak::upgrade) {
            if owner.stale.swap(false, Ordering::Relaxed)
                && let Err(error) = owner.watcher.lock().unwrap().watch(&spec.path, mode(&spec))
            {
                owner.stale.store(true, Ordering::Relaxed);
                return Err(error).with_context(|| {
                    format!("cannot refresh filesystem watch in {}", spec.path.display())
                });
            }
            owner
        } else {
            let listeners = Arc::new(Mutex::new(HashMap::<u64, Listener>::new()));
            let incoming = listeners.clone();
            let error = self.error.clone();
            let path = spec.path.clone();
            let stale = Arc::new(AtomicBool::new(false));
            let changed_inode = stale.clone();
            let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
                let listeners = incoming.lock().unwrap();
                match event {
                    Ok(event) => {
                        if matches!(event.kind, EventKind::Access(_)) {
                            return;
                        }
                        if matches!(
                            event.kind,
                            EventKind::Remove(_)
                                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                        ) && event.paths.contains(&path)
                        {
                            changed_inode.store(true, Ordering::Relaxed);
                        }
                        for listener in listeners.values() {
                            if !listener.accepts(&event) {
                                continue;
                            }
                            let Some(signal) = listener.signal.upgrade() else {
                                continue;
                            };
                            signal.dirty.store(true, Ordering::Relaxed);
                        }
                    }
                    Err(cause) => {
                        changed_inode.store(true, Ordering::Relaxed);
                        *error.lock().unwrap() = Some(format!(
                            "filesystem watcher failed for {}: {cause}",
                            path.display()
                        ));
                        for listener in listeners.values() {
                            if let Some(signal) = listener.signal.upgrade() {
                                signal.dirty.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
            })
            .with_context(|| {
                format!(
                    "cannot create filesystem watcher for {}",
                    spec.path.display()
                )
            })?;
            watcher.watch(&spec.path, mode(&spec)).with_context(|| {
                format!("cannot watch filesystem changes in {}", spec.path.display())
            })?;
            self.watched_paths.fetch_add(1, Ordering::Relaxed);
            let owner = Arc::new(PhysicalWatch {
                watched_paths: self.watched_paths.clone(),
                watcher: Mutex::new(watcher),
                stale,
                listeners,
            });
            self.entries.insert(spec, Arc::downgrade(&owner));
            owner
        };
        self.next_listener += 1;
        let listener = self.next_listener;
        owner.listeners.lock().unwrap().insert(
            listener,
            Listener {
                signal: Arc::downgrade(signal),
                filter: dependency.filter,
                metadata,
            },
        );
        Ok(WatchLease { owner, listener })
    }
}

fn mode(spec: &WatchSpec) -> RecursiveMode {
    if spec.recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    }
}

struct Plan {
    dependencies: Vec<Dependency>,
    metadata: Vec<PathBuf>,
}

impl Plan {
    fn new(workspace: &Workspace, key: &QueryKey) -> Result<Self> {
        let mut plan = Self {
            dependencies: Vec::new(),
            metadata: Vec::new(),
        };
        match key {
            QueryKey::OpenFile(path) => {
                plan.exact(path)?;
                if let Ok(target) = path.canonicalize()
                    && target != *path
                {
                    plan.exact(&target)?;
                }
            }
            QueryKey::Filesystem(relative) => {
                let path = if workspace.root.try_exists()? {
                    workspace.path(relative.to_str().context("filesystem path is not UTF-8")?)?
                } else {
                    workspace.root.join(relative)
                };
                plan.exact(&path)?;
                if path.is_dir() {
                    plan.directory(&path, false)?;
                    let directory = path
                        .canonicalize()
                        .with_context(|| format!("cannot resolve {}", path.display()))?;
                    // require_git(false) also applies rules above the repository.
                    plan.ignore_rules(&directory, None)?;
                } else if let Ok(target) = path.canonicalize()
                    && target != path
                    && target.starts_with(&workspace.root)
                {
                    plan.exact(&target)?;
                }
            }
            QueryKey::GitCommitFiles { .. }
            | QueryKey::GitBlob { .. }
            | QueryKey::GitCommitDiff { .. }
            | QueryKey::GitDiffFiles {
                source: DiffSource::Commit { .. },
                ..
            } => {}
            QueryKey::GitStatus
            | QueryKey::GitLog
            | QueryKey::GitDiff { .. }
            | QueryKey::GitDiffFiles {
                source: DiffSource::Changes { .. },
                ..
            } => {
                plan.metadata = zess_git::metadata_directories(&workspace.root)?;
                for directory in plan.metadata.clone() {
                    plan.exact(&directory)?;
                    if directory.is_dir() {
                        plan.directory(&directory, true)?;
                    }
                }
                let mut repository = None;
                for ancestor in workspace.root.ancestors() {
                    let marker = ancestor.join(".git");
                    plan.exact(&marker)?;
                    if marker.exists() {
                        repository = Some(ancestor);
                        break;
                    }
                }
                match key {
                    QueryKey::GitStatus => {
                        plan.exact(&workspace.root)?;
                        if workspace.root.is_dir() {
                            plan.directory(&workspace.root, true)?;
                        }
                        if let Some(repository) = repository {
                            plan.ignore_rules(&workspace.root, Some(repository))?;
                        }
                    }
                    QueryKey::GitDiff { path, original } => {
                        let path =
                            workspace.path(path.to_str().context("Git path is not UTF-8")?)?;
                        plan.exact(&path)?;
                        if path.is_dir() {
                            plan.directory(&path, true)?;
                        }
                        if let Some(original) = original {
                            let original = workspace
                                .path(original.to_str().context("Git path is not UTF-8")?)?;
                            plan.exact(&original)?;
                            if original.is_dir() {
                                plan.directory(&original, true)?;
                            }
                        }
                    }
                    QueryKey::GitDiffFiles { files, .. } => {
                        for target in files {
                            let path = workspace.path(&target.path)?;
                            plan.exact(&path)?;
                            for directory in path
                                .parent()
                                .context("Git target has no parent")?
                                .ancestors()
                            {
                                plan.exact(&directory.join(".gitattributes"))?;
                                if directory == repository.unwrap_or(&workspace.root) {
                                    break;
                                }
                            }
                            if let Some(original) = &target.original_path {
                                plan.exact(&workspace.path(original)?)?;
                            }
                        }
                    }
                    QueryKey::GitLog => {}
                    QueryKey::OpenFile(_)
                    | QueryKey::Filesystem(_)
                    | QueryKey::GitCommitFiles { .. }
                    | QueryKey::GitBlob { .. }
                    | QueryKey::GitCommitDiff { .. } => unreachable!(),
                }
            }
        }
        Ok(plan)
    }

    fn directory(&mut self, path: &Path, recursive: bool) -> Result<()> {
        let path = path
            .canonicalize()
            .with_context(|| format!("cannot resolve watch directory {}", path.display()))?;
        let spec = WatchSpec {
            path: path.to_owned(),
            recursive,
        };
        let filter = if recursive {
            Filter::Tree(path.to_owned())
        } else {
            Filter::Directory(path.to_owned())
        };
        self.push(Dependency { spec, filter });
        Ok(())
    }

    fn exact(&mut self, path: &Path) -> Result<()> {
        let mut pending = vec![path.to_owned()];
        let mut followed = HashSet::new();
        while let Some(path) = pending.pop() {
            self.exact_entry(&path)?;
            for ancestor in path.ancestors() {
                let Ok(target) = fs::read_link(ancestor) else {
                    continue;
                };
                let parent = ancestor
                    .parent()
                    .context("symbolic link has no parent")?
                    .canonicalize()
                    .with_context(|| {
                        format!("cannot resolve symbolic link parent {}", ancestor.display())
                    })?;
                let entry = parent.join(ancestor.file_name().context("symbolic link has no name")?);
                if followed.insert(entry.clone()) {
                    self.exact_entry(&entry)?;
                    pending.push(parent.join(target));
                }
            }
        }
        Ok(())
    }

    fn exact_entry(&mut self, path: &Path) -> Result<()> {
        if path.parent().is_none() {
            return self.directory(path, false);
        }
        if path.file_name().is_none() {
            let target = path
                .canonicalize()
                .with_context(|| format!("cannot resolve watch path {}", path.display()))?;
            return self.exact_entry(&target);
        }
        let mut target = path;
        let mut directory = path
            .parent()
            .context("cannot watch a path without a parent")?;
        loop {
            match fs::metadata(directory) {
                Ok(metadata) if metadata.is_dir() => break,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("cannot inspect watch directory {}", directory.display())
                    });
                }
            }
            target = directory;
            directory = directory
                .parent()
                .context("cannot find an existing watch directory")?;
        }
        let target = target.strip_prefix(directory)?;
        let directory = directory
            .canonicalize()
            .with_context(|| format!("cannot resolve watch directory {}", directory.display()))?;
        self.push(Dependency {
            spec: WatchSpec {
                path: directory.clone(),
                recursive: false,
            },
            filter: Filter::Exact(directory.join(target)),
        });
        Ok(())
    }

    fn ignore_rules(&mut self, directory: &Path, boundary: Option<&Path>) -> Result<()> {
        for ancestor in directory.ancestors() {
            self.exact(&ancestor.join(".gitignore"))?;
            self.exact(&ancestor.join(".git/info/exclude"))?;
            if Some(ancestor) == boundary {
                break;
            }
        }
        Ok(())
    }

    fn push(&mut self, dependency: Dependency) {
        if !self.dependencies.contains(&dependency) {
            self.dependencies.push(dependency);
        }
    }
}
