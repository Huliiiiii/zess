use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::query::{QueryRequest, QueryResult};
use anyhow::{Context, Result, bail};

use crate::filesystem::Snapshot;
use crate::workspace::Workspace;

const WORKERS: usize = 4;
const QUEUED_JOBS: usize = 32;

#[derive(Clone, Default)]
pub(crate) struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub(crate) fn check(&self) -> Result<()> {
        if self.0.load(Ordering::Acquire) {
            bail!("background job was cancelled");
        }

        Ok(())
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn output(&self, command: &mut Command) -> Result<Output> {
        self.check()?;

        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        thread::scope(|scope| {
            // This guard is dropped before the scope waits for pipe readers, including
            // when a reader cannot be spawned or cancellation interrupts the process.
            let mut child = RunningChild {
                child,
                reaped: false,
            };
            let mut stdout = child.child.stdout.take().context("missing git stdout")?;
            let mut stderr = child.child.stderr.take().context("missing git stderr")?;
            let stdout = thread::Builder::new()
                .name("zess-git-stdout".into())
                .spawn_scoped(scope, move || {
                    let mut bytes = Vec::new();
                    stdout.read_to_end(&mut bytes)?;

                    Ok::<_, std::io::Error>(bytes)
                })?;
            let stderr = thread::Builder::new()
                .name("zess-git-stderr".into())
                .spawn_scoped(scope, move || {
                    let mut bytes = Vec::new();
                    stderr.read_to_end(&mut bytes)?;

                    Ok::<_, std::io::Error>(bytes)
                })?;
            let status = loop {
                self.check()?;

                if let Some(status) = child.child.try_wait()? {
                    child.reaped = true;
                    break status;
                }

                thread::sleep(Duration::from_millis(10));
            };
            let stdout = stdout
                .join()
                .map_err(|_| anyhow::anyhow!("git stdout reader stopped"))??;
            let stderr = stderr
                .join()
                .map_err(|_| anyhow::anyhow!("git stderr reader stopped"))??;

            self.check()?;

            Ok(Output {
                status,
                stdout,
                stderr,
            })
        })
    }
}

struct RunningChild {
    child: Child,
    reaped: bool,
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
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
        status_progress: Option<mpsc::Sender<Vec<zess_git::GitEntry>>>,
    },
    Filesystem {
        path: String,
        result: mpsc::Sender<Result<Arc<Snapshot>, String>>,
    },
}

pub(crate) struct JobScheduler {
    tasks: SyncSender<Task>,
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

        Ok(Self { tasks: sender })
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
        status_progress: Option<mpsc::Sender<Vec<zess_git::GitEntry>>>,
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
        let cancellation = Cancellation::default();
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
    fn check(&self) -> Result<()> {
        Cancellation::check(self)
    }
    fn output(&self, command: &mut Command) -> Result<Output> {
        Cancellation::output(self, command)
    }
}
