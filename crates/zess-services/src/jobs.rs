use std::process::Output;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::query::{QueryRequest, QueryResult, StatusProgress};
use anyhow::{Context, Result, bail};
use zess_platform::{ProcessRunner, ProcessSpec, SystemProcessRunner};

use crate::filesystem::Snapshot;
use crate::workspace::Workspace;

const WORKERS: usize = 4;
const QUEUED_JOBS: usize = 32;

#[cfg(all(test, unix))]
mod tests;

#[derive(Clone)]
pub(crate) struct Cancellation {
    cancelled: Arc<AtomicBool>,
    processes: Arc<dyn ProcessRunner>,
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new(Arc::new(SystemProcessRunner))
    }
}

impl Cancellation {
    pub(crate) fn new(processes: Arc<dyn ProcessRunner>) -> Self {
        Self {
            cancelled: Arc::default(),
            processes,
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            bail!("background job was cancelled");
        }
        Ok(())
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn output(&self, spec: &ProcessSpec) -> Result<Output> {
        self.processes.output(spec, None, self)
    }

    fn output_with_input(&self, spec: &ProcessSpec, input: &[u8]) -> Result<Output> {
        self.processes.output(spec, Some(input), self)
    }
}

impl zess_platform::Cancellation for Cancellation {
    fn check(&self) -> Result<()> {
        Cancellation::check(self)
    }
}

pub(crate) struct Job<T> {
    cancellation: Cancellation,
    pub(crate) result: Receiver<T>,
}

impl<T> Job<T> {
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }
}

struct Task {
    cancellation: Cancellation,
    kind: TaskKind,
}

enum TaskKind {
    Watches {
        request: crate::watcher::WatchRequest,
        result: mpsc::Sender<Result<Vec<crate::watcher::WatchLease>, String>>,
    },
    Background {
        request: QueryRequest,
        result: mpsc::Sender<Result<QueryResult, String>>,
        status_progress: Option<mpsc::Sender<StatusProgress>>,
    },
    Filesystem {
        path: String,
        result: mpsc::Sender<Result<Arc<Snapshot>, String>>,
    },
}

pub(crate) struct JobScheduler {
    tasks: SyncSender<Task>,
    processes: Arc<dyn ProcessRunner>,
}

impl JobScheduler {
    pub(crate) fn new(workspace: Workspace) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(QUEUED_JOBS);
        let receiver = Arc::new(Mutex::new(receiver));

        for index in 0..WORKERS {
            let receiver = receiver.clone();
            let workspace = workspace.clone();

            thread::Builder::new()
                .name(format!("zess-service-{index}"))
                .spawn(move || {
                    loop {
                        let task = {
                            let Ok(receiver) = receiver.lock() else {
                                return;
                            };

                            receiver.recv()
                        };
                        let Ok(task) = task else {
                            return;
                        };

                        if task.cancellation.check().is_err() {
                            continue;
                        }

                        match task.kind {
                            TaskKind::Watches { request, result } => {
                                let value = request
                                    .execute(&workspace, &task.cancellation)
                                    .map_err(|error| format!("{error:#}"));
                                let _ = result.send(value);
                            }
                            TaskKind::Background {
                                request,
                                result,
                                status_progress,
                            } => {
                                let value = workspace
                                    .query_with_progress(
                                        &request,
                                        &task.cancellation,
                                        status_progress.as_ref(),
                                    )
                                    .map_err(|error| format!("{error:#}"));
                                let _ = result.send(value);
                            }
                            TaskKind::Filesystem { path, result } => {
                                let value = workspace
                                    .read_fs(&path, &task.cancellation)
                                    .map(Arc::new)
                                    .map_err(|error| format!("{error:#}"));
                                let _ = result.send(value);
                            }
                        }
                    }
                })
                .context("cannot start background worker")?;
        }

        Ok(Self {
            tasks: sender,
            processes: workspace.processes.clone(),
        })
    }

    pub(crate) fn start_watch(
        &self,
        request: crate::watcher::WatchRequest,
    ) -> Result<Option<Job<Result<Vec<crate::watcher::WatchLease>, String>>>> {
        self.enqueue(|result| TaskKind::Watches { request, result })
    }

    pub(crate) fn try_start(
        &self,
        request: QueryRequest,
        status_progress: Option<mpsc::Sender<StatusProgress>>,
    ) -> Result<Option<Job<Result<QueryResult, String>>>> {
        self.enqueue(|result| TaskKind::Background {
            request,
            result,
            status_progress,
        })
    }

    pub(crate) fn start_fs(
        &self,
        path: String,
    ) -> Result<Option<Job<Result<Arc<Snapshot>, String>>>> {
        self.enqueue(|result| TaskKind::Filesystem { path, result })
    }

    fn enqueue<T>(&self, kind: impl FnOnce(mpsc::Sender<T>) -> TaskKind) -> Result<Option<Job<T>>> {
        let cancellation = Cancellation::new(self.processes.clone());
        let (sender, receiver) = mpsc::channel();
        let task = Task {
            cancellation: cancellation.clone(),
            kind: kind(sender),
        };
        match self.tasks.try_send(task) {
            Ok(()) => Ok(Some(Job {
                cancellation,
                result: receiver,
            })),
            Err(TrySendError::Full(_)) => Ok(None),
            Err(TrySendError::Disconnected(_)) => bail!("background workers are unavailable"),
        }
    }
}

impl zess_git::Execution for Cancellation {
    fn environment(&self, name: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
        self.processes.environment(name)
    }

    fn check(&self) -> Result<()> {
        Cancellation::check(self)
    }
    fn output(&self, spec: &ProcessSpec) -> Result<Output> {
        Cancellation::output(self, spec)
    }
    fn output_with_input(&self, spec: &ProcessSpec, input: &[u8]) -> Result<Output> {
        Cancellation::output_with_input(self, spec, input)
    }
}
