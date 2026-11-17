use std::collections::VecDeque;

use anyhow::{Context, Result, bail, ensure};
use zess_plugin::{
    HostRequest, OperationCompletion, OperationResult, PluginInput, PluginOutput, PluginRegistrar,
};
use zess_services::{QueryRequest, QueryResult};

use super::{Event, Services, browser::Task};

pub(super) struct Target {
    pub path: String,
    pub name: String,
    pub kind: TargetKind,
}

pub(super) enum TargetKind {
    Directory,
    File {
        source: Source,
        original_path: Option<String>,
    },
}

pub(super) enum Source {
    Worktree,
    Commit(String),
}

pub(super) fn register(registry: &mut dyn PluginRegistrar, kind: &str) {
    for mode in ["normal", "select"] {
        registry.binding(kind, mode, &["\""], "data.select-register");
    }
    for (event, description, key) in [
        ("yank-path", "Yank relative paths", "p"),
        ("yank-absolute-path", "Yank absolute paths", "a"),
        ("yank-name", "Yank file names", "n"),
        ("yank-content", "Yank file contents", "f"),
        ("yank-diff", "Yank file diffs", "d"),
    ] {
        super::browser::bind_event(registry, kind, event, description, &["y", key]);
    }
}

#[derive(Default)]
pub(super) struct Yanks(VecDeque<Yank>);

impl Yanks {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn queue(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        targets: Vec<Target>,
        output: &mut PluginOutput,
    ) -> Result<()> {
        let register = match input.register {
            Some(register) => register,
            None => input
                .options
                .get("default-yank-register")
                .map(|register| {
                    register
                        .as_str()
                        .context("default-yank-register must be a single character")?
                        .parse::<char>()
                        .context("default-yank-register must be a single character")
                })
                .transpose()?
                .unwrap_or('+'),
        };
        let mut yank = Yank::new(services, event, targets, register)?;
        yank.operation_id = input.context.operation_id.clone();
        if let Some(operation_id) = &yank.operation_id {
            output.pending_operations.push(operation_id.clone());
        }
        self.0.push_back(yank);
        Ok(())
    }

    pub(super) fn advance(&mut self, services: &Services, event: Event, output: &mut PluginOutput) {
        if event == Event::ViewLeaving {
            output.completed_operations.extend(self.cancel());
            return;
        }
        while self.0.front().is_some_and(|yank| {
            yank.operation_id.is_none()
                || matches!(&yank.state, YankState::Queued { requests, .. } if requests.is_empty())
        }) {
            let Some(mut publication) = self.poll_next(services) else {
                break;
            };
            output.requests.append(&mut publication.requests);
            output
                .completed_operations
                .append(&mut publication.completed_operations);
        }
        if !self.is_empty() {
            if let Some(display) = &mut output.display {
                display.loading = true;
            }
            output.requests.push(HostRequest::Schedule { after_ms: 50 });
        }
    }

    pub(super) fn cancel(&mut self) -> Vec<OperationCompletion> {
        self.0
            .drain(..)
            .filter_map(|yank| {
                yank.operation_id.map(|operation_id| OperationCompletion {
                    operation_id,
                    result: OperationResult::Cancelled,
                })
            })
            .collect()
    }

    pub(super) fn poll(&mut self, services: &Services) -> Vec<PluginOutput> {
        let mut outputs = Vec::new();
        while let Some(output) = self.poll_next(services) {
            outputs.push(output);
        }
        outputs
    }

    fn poll_next(&mut self, services: &Services) -> Option<PluginOutput> {
        let yank = self.0.front_mut()?;
        let result = match yank.advance(services) {
            Ok(Some(values)) => Ok(values),
            Ok(None) => return None,
            Err(error) => Err(format!("{error:#}")),
        };
        let Yank {
            operation_id,
            register,
            ..
        } = self.0.pop_front().unwrap();
        let mut output = PluginOutput::default();
        if let Some(operation_id) = operation_id {
            output.completed_operations.push(OperationCompletion {
                operation_id,
                result: match result {
                    Ok(values) => OperationResult::Yanked { values, register },
                    Err(message) => OperationResult::Failed { message },
                },
            });
        } else {
            output.requests.push(match result {
                Ok(values) => HostRequest::Yank {
                    values,
                    register: Some(register),
                },
                Err(text) => HostRequest::Message { text },
            });
        }
        Some(output)
    }
}

struct Yank {
    register: char,
    operation_id: Option<String>,
    state: YankState,
}

enum YankState {
    Queued {
        requests: VecDeque<QueryRequest>,
        values: Vec<String>,
    },
    Reading {
        remaining: VecDeque<QueryRequest>,
        task: Task,
        values: Vec<String>,
    },
    Ready(Vec<String>),
}

impl Yank {
    fn new(
        services: &Services,
        event: Event,
        targets: Vec<Target>,
        register: char,
    ) -> Result<Self> {
        ensure!(!targets.is_empty(), "Select a path to yank");
        zess_plugin::validate_register_write(register)?;
        if matches!(event, Event::YankContent | Event::YankDiff) {
            ensure!(
                targets
                    .iter()
                    .all(|target| matches!(target.kind, TargetKind::File { .. })),
                "Select files to yank contents or diffs"
            );
        }
        let services = services.borrow();
        let mut requests = VecDeque::new();
        let mut values = Vec::new();
        for Target { path, name, kind } in targets {
            match event {
                Event::YankPath => values.push(path),
                Event::YankAbsolutePath => {
                    values.push(services.root().join(path).to_string_lossy().into_owned());
                }
                Event::YankName => values.push(name),
                Event::YankContent | Event::YankDiff => {
                    let TargetKind::File {
                        source,
                        original_path,
                    } = kind
                    else {
                        unreachable!("content yanks require files");
                    };
                    requests.push_back(match (event, source) {
                        (Event::YankContent, Source::Worktree) => QueryRequest::ReadFile {
                            path: services.root().join(path),
                        },
                        (Event::YankContent, Source::Commit(commit)) => {
                            QueryRequest::Blob { commit, path }
                        }
                        (Event::YankDiff, Source::Worktree) => QueryRequest::Diff {
                            path,
                            original_path,
                            side: None,
                        },
                        (Event::YankDiff, Source::Commit(commit)) => QueryRequest::CommitDiff {
                            commit,
                            path,
                            original_path,
                        },
                        _ => unreachable!("only content or diff yanks create queries"),
                    });
                }
                _ => unreachable!("only yank events create a yank"),
            }
        }
        Ok(Self {
            register,
            operation_id: None,
            state: YankState::Queued { requests, values },
        })
    }

    fn advance(&mut self, services: &Services) -> Result<Option<Vec<String>>> {
        loop {
            match &mut self.state {
                YankState::Ready(values) => return Ok(Some(std::mem::take(values))),
                YankState::Queued { requests, values } => {
                    self.state = if let Some(request) = requests.pop_front() {
                        YankState::Reading {
                            task: Task::start(services, request)?,
                            remaining: std::mem::take(requests),
                            values: std::mem::take(values),
                        }
                    } else {
                        YankState::Ready(std::mem::take(values))
                    };
                }
                YankState::Reading {
                    remaining,
                    task,
                    values,
                } => {
                    let value = match task.poll()? {
                        Some(QueryResult::FileRead { text, .. }) => {
                            ensure!(!text.contains('\0'), "Cannot yank binary file contents");
                            text
                        }
                        Some(QueryResult::Snapshot { snapshot }) => snapshot.text,
                        Some(QueryResult::Diff { text, .. }) => text,
                        Some(QueryResult::Unsupported { message }) => bail!("{message}"),
                        Some(_) => bail!("Yank query returned an unsupported result"),
                        None => return Ok(None),
                    };
                    values.push(value);
                    self.state = YankState::Queued {
                        requests: std::mem::take(remaining),
                        values: std::mem::take(values),
                    };
                }
            }
        }
    }
}
