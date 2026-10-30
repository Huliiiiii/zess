use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

mod file_watch;
mod filesystem;
mod fs_cache;
mod git_colors;
mod mutations;
pub use mutations::FileMutationTask;
mod patches;
pub use patches::PatchTask;
mod query;
pub use query::{QueryPoll, QueryRequest, QueryResult};
mod jobs;
mod watcher;
mod workspace;

pub use filesystem::{AppliedOperations, DirectoryEntry, DirectoryEntryKind, FileOperation};
use fs_cache::FsCache;
use jobs::{Cancellation, Job, JobScheduler};
pub use watcher::Observation;
use watcher::{Queries, Query, QueryKey, Registration};
use workspace::Workspace;

#[cfg(test)]
mod tests;

pub struct NativeServices {
    workspace: Workspace,
    filesystem: FsCache,
    queries: Queries,
    next_handle: u64,
    jobs: HashMap<u64, BackgroundJob>,
    scheduler: Option<JobScheduler>,
}

impl NativeServices {
    pub fn new(root: PathBuf) -> Result<Self> {
        let root = root.canonicalize().context("cannot open workspace")?;

        if !root.is_dir() {
            bail!("workspace must be a directory");
        }

        Ok(Self {
            workspace: Workspace {
                root,
                git_colors: Default::default(),
                #[cfg(test)]
                read_hook: None,
            },
            filesystem: FsCache::default(),
            queries: Queries::default(),
            next_handle: 1,
            jobs: HashMap::new(),
            scheduler: None,
        })
    }

    pub fn git_styles(&self) -> HashMap<String, zess_ui::TextStyle> {
        self.workspace.git_colors.lock().unwrap().palette()
    }

    pub fn root(&self) -> &std::path::Path {
        &self.workspace.root
    }

    pub fn fs_revision(&self) -> u64 {
        self.filesystem.revision()
    }

    pub fn invalidate_fs(&mut self) {
        self.filesystem.invalidate();
    }

    /// Returns a current List/Preview snapshot without I/O or scheduling.
    /// Other requests and snapshots without live, current watches return None.
    /// Result conversion copies text and directory entries.
    pub fn cached_query(&self, request: &QueryRequest) -> Result<Option<QueryResult>> {
        self.filesystem.cached_query(request)
    }

    pub fn poll_changes(&mut self) -> Result<()> {
        let Some(scheduler) = &self.scheduler else {
            return Ok(());
        };
        let (changed, result) = self.queries.poll(scheduler);
        for key in changed {
            if let QueryKey::Filesystem(path) = key {
                self.filesystem.invalidate_path(&path);
            }
        }
        result
    }

    pub fn observe(&mut self, operation: &str, args: &Value) -> Result<Observation> {
        let key = QueryKey::from_call(operation, args)?;
        Ok(Observation::new(self.acquire_key(key)?))
    }

    fn acquire_query(&mut self, request: &QueryRequest) -> Result<Arc<Query>> {
        let key = QueryKey::from_request(request)?;
        self.acquire_key(key)
    }

    fn acquire_key(&mut self, key: QueryKey) -> Result<Arc<Query>> {
        if self.scheduler.is_none() {
            self.scheduler = Some(JobScheduler::new(self.workspace.clone())?);
        }
        let (query, fresh) = self
            .queries
            .acquire(key, self.scheduler.as_ref().unwrap())?;
        if fresh && let QueryKey::Filesystem(path) = &query.key {
            self.filesystem.invalidate_path(path);
        }
        Ok(query)
    }

    fn wait_for_registration(&mut self, query: &Query) -> Result<()> {
        let registration = query.registration();
        loop {
            self.queries.advance(self.scheduler.as_ref().unwrap());
            match registration.ready() {
                Ok(true) => return Ok(()),
                Ok(false) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(error) => return Err(anyhow::Error::msg(error)),
            }
        }
    }

    pub fn cancel_jobs(&mut self) {
        self.filesystem.cancel_all();
        for (_, job) in self.jobs.drain() {
            job.cancel();
        }
    }

    pub fn validate_file_operations(&self, operations: &[FileOperation]) -> Result<()> {
        self.workspace.validate_operations(operations)
    }

    pub fn apply_file_operations(
        &mut self,
        operations: &[FileOperation],
    ) -> Result<AppliedOperations> {
        self.validate_file_operations(operations)?;
        let mut applied = Vec::new();
        for (index, operation) in operations.iter().enumerate() {
            if let Err(error) = self.workspace.apply_operation(operation) {
                self.invalidate_fs();
                return Ok(AppliedOperations {
                    applied,
                    remaining: operations[index..].to_vec(),
                    error: Some(format!("{error:#}")),
                });
            }
            applied.push(operation.clone());
        }
        self.invalidate_fs();
        Ok(AppliedOperations {
            applied,
            remaining: Vec::new(),
            error: None,
        })
    }

    pub fn start_query(&mut self, request: QueryRequest) -> Result<u64> {
        let handle = self.next_handle;
        self.next_handle += 1;
        self.start_query_with_handle(request, handle)?;
        Ok(handle)
    }

    fn start_query_with_handle(&mut self, request: QueryRequest, handle: u64) -> Result<()> {
        if self.scheduler.is_none() {
            self.scheduler = Some(JobScheduler::new(self.workspace.clone())?);
        }
        let query = self.acquire_query(&request)?;
        let scheduler = self.scheduler.as_ref().unwrap();
        if matches!(
            request,
            QueryRequest::List { .. } | QueryRequest::Preview { .. }
        ) {
            self.filesystem.start(&request, handle, scheduler, query)?;
        } else {
            let revision = query.revision();
            let registration = query.registration();
            self.jobs.insert(
                handle,
                BackgroundJob {
                    job: None,
                    registration,
                    request,
                    query,
                    revision,
                    status_progress: None,
                },
            );
        }
        Ok(())
    }

    pub fn poll_query(&mut self, handle: u64) -> Result<QueryPoll> {
        if let Some(scheduler) = &self.scheduler {
            self.queries.advance(scheduler);
        }
        if let Some(scheduler) = &self.scheduler
            && let Some(result) = self.filesystem.poll(handle, scheduler)
        {
            return Ok(result);
        }
        let state = self
            .jobs
            .get_mut(&handle)
            .context("unknown or completed job handle")?;
        let scheduler = self
            .scheduler
            .as_ref()
            .context("background scheduler is unavailable")?;
        if let Err(error) = state.registration.ready() {
            self.jobs.remove(&handle);
            return Ok(QueryPoll::Ready(Err(error)));
        }
        let registration = state.query.registration();
        if !Arc::ptr_eq(&registration, &state.registration) {
            state.registration = registration;
            state.status_progress = None;
            if let Some(previous) = state.job.take() {
                previous.cancel();
            }
        }
        match state.registration.ready() {
            Ok(true) => {}
            Ok(false) => return Ok(QueryPoll::Pending),
            Err(error) => {
                self.jobs.remove(&handle);
                return Ok(QueryPoll::Ready(Err(error)));
            }
        }
        if state.revision != state.query.revision() {
            state.status_progress = None;
            if let Some(previous) = state.job.take() {
                previous.cancel();
            }
            state.revision = state.query.revision();
        }
        if state.job.is_none() {
            let progress = if matches!(state.request, QueryRequest::Status {}) {
                let (sender, receiver) = std::sync::mpsc::channel();
                state.status_progress = Some(receiver);
                Some(sender)
            } else {
                None
            };
            state.job = scheduler.try_start(state.request.clone(), progress)?;
        }
        let Some(job) = &state.job else {
            return Ok(QueryPoll::Pending);
        };
        match job.result.try_recv() {
            Ok(result) => {
                self.jobs.remove(&handle);
                Ok(QueryPoll::Ready(result))
            }
            Err(TryRecvError::Empty) => Ok(QueryPoll::Pending),
            Err(TryRecvError::Disconnected) => {
                self.jobs.remove(&handle);
                bail!("background service stopped without a result");
            }
        }
    }

    pub fn status_progress(&mut self, handle: u64) -> Result<Option<Vec<zess_git::GitEntry>>> {
        let state = self
            .jobs
            .get_mut(&handle)
            .context("unknown or completed job handle")?;
        if state.revision != state.query.revision()
            || !state.query.cache_is_current(&state.registration)
        {
            return Ok(None);
        }
        let Some(progress) = &state.status_progress else {
            return Ok(None);
        };
        match progress.try_recv() {
            Ok(entries) => Ok(Some(entries)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => Ok(None),
        }
    }

    pub fn cancel_query(&mut self, handle: u64) {
        self.filesystem.cancel(handle);
        if let Some(job) = self.jobs.remove(&handle) {
            job.cancel();
        }
    }

    pub fn call(&mut self, operation: &str, args: Value) -> Value {
        match self.dispatch(operation, &args) {
            Ok(value) => value,
            Err(error) => json!({"error": format!("{error:#}")}),
        }
    }

    fn dispatch(&mut self, operation: &str, args: &Value) -> Result<Value> {
        let handle = self.next_handle;
        self.next_handle += 1;

        match operation {
            "job.start" => {
                let operation = string(args, "operation")?;
                let argument = args.get("argument").cloned().unwrap_or_else(|| json!({}));
                let request = QueryRequest::from_call(operation, &argument)?;
                if matches!(
                    request,
                    QueryRequest::List { .. } | QueryRequest::Preview { .. }
                ) && argument["refresh"] == true
                {
                    self.invalidate_fs();
                }
                self.start_query_with_handle(request, handle)?;
                Ok(json!({"handle":handle}))
            }
            "job.poll" => {
                let handle = args["handle"]
                    .as_u64()
                    .context("handle must be an integer")?;
                Ok(self.poll_query(handle)?.into_value(handle))
            }
            "job.cancel" => {
                let handle = args["handle"]
                    .as_u64()
                    .context("handle must be an integer")?;
                self.cancel_query(handle);
                Ok(json!({"status":"cancelled"}))
            }
            "service.stats" => Ok(json!({
                "calls": handle,
                "next_handle": self.next_handle,
                "cached_paths": self.filesystem.len(),
                "jobs": self.jobs.len() + self.filesystem.jobs(),
                "observed_queries": self.queries.len(),
                "watched_paths": self.queries.watched_paths()
            })),
            "fs.list" | "fs.preview" => {
                let request = QueryRequest::from_call(operation, args)?;
                let query = self.acquire_query(&request)?;
                self.wait_for_registration(&query)?;
                self.filesystem
                    .read(&self.workspace, &request, args["refresh"] == true, &query)
                    .map(|result| result.into_value(handle))
            }
            "fs.invalidate" => {
                self.invalidate_fs();
                Ok(json!({"revision":self.fs_revision()}))
            }
            "fs.validate" | "fs.apply" => {
                let operations: Vec<FileOperation> =
                    serde_json::from_value(args["operations"].clone())?;
                if operation == "fs.validate" {
                    self.validate_file_operations(&operations)?;
                    Ok(json!({"valid":true}))
                } else {
                    Ok(serde_json::to_value(
                        self.apply_file_operations(&operations)?,
                    )?)
                }
            }
            "text.read" => {
                let relative = string(args, "path")?;
                let text = fs::read_to_string(self.workspace.path(relative)?)
                    .context("cannot read UTF-8 file")?;

                Ok(json!({"path": relative, "text": text}))
            }
            "git.status" | "git.decorations" | "git.diff" | "git.diff-files" | "git.log"
            | "git.commit-files" | "git.commit-diff" | "git.blob" => {
                let request = QueryRequest::from_call(operation, args)?;
                let query = self.acquire_query(&request)?;
                self.wait_for_registration(&query)?;
                self.workspace
                    .query(&request, &Cancellation::default())
                    .map(|result| result.into_value(handle))
            }
            _ => bail!("unknown host operation: {operation}"),
        }
    }
}

struct BackgroundJob {
    registration: Arc<Registration>,
    job: Option<Job<Result<QueryResult, String>>>,
    request: QueryRequest,
    query: Arc<Query>,
    revision: u64,
    status_progress: Option<std::sync::mpsc::Receiver<Vec<zess_git::GitEntry>>>,
}

impl BackgroundJob {
    fn cancel(self) {
        if let Some(job) = self.job {
            job.cancel();
        }
    }
}

impl Drop for NativeServices {
    fn drop(&mut self) {
        self.cancel_jobs();
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("{key} must be a string"))
}
