use std::collections::VecDeque;

use anyhow::{Context, Result, bail, ensure};
use zess_plugin::{HostRequest, PluginInput, PluginOutput, PluginRegistrar};
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
        self.0
            .push_back(Yank::new(services, event, targets, register)?);
        Ok(())
    }

    pub(super) fn advance(&mut self, services: &Services, event: Event, output: &mut PluginOutput) {
        if event == Event::ViewLeaving {
            self.0.clear();
            return;
        }
        while let Some(yank) = self.0.front_mut() {
            match yank.advance(services) {
                Ok(Some(values)) => output.requests.push(HostRequest::Yank {
                    values,
                    register: Some(yank.register),
                }),
                Ok(None) => break,
                Err(error) => output.requests.push(HostRequest::Message {
                    text: format!("{error:#}"),
                }),
            }
            self.0.pop_front();
        }
        if !self.is_empty() {
            output.display.as_mut().unwrap().loading = true;
            output.requests.push(HostRequest::Schedule { after_ms: 50 });
        }
    }
}

struct Yank {
    register: char,
    requests: VecDeque<QueryRequest>,
    task: Option<Task>,
    values: Vec<String>,
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
            requests,
            task: None,
            values,
        })
    }

    fn advance(&mut self, services: &Services) -> Result<Option<Vec<String>>> {
        loop {
            if let Some(task) = &mut self.task {
                match task.poll()? {
                    Some(QueryResult::Text {
                        text,
                        truncated: false,
                        ..
                    }) => {
                        ensure!(!text.contains('\0'), "Cannot yank binary file contents");
                        self.values.push(text);
                    }
                    Some(QueryResult::Snapshot { snapshot }) => self.values.push(snapshot.text),
                    Some(QueryResult::Diff { text, .. }) => self.values.push(text),
                    Some(QueryResult::Unsupported { message }) => bail!("{message}"),
                    Some(_) => bail!("Yank query returned an unsupported result"),
                    None => return Ok(None),
                }
                self.task = None;
            }
            if let Some(request) = self.requests.pop_front() {
                self.task = Some(Task::start(services, request)?);
            } else {
                return Ok(Some(std::mem::take(&mut self.values)));
            }
        }
    }
}
