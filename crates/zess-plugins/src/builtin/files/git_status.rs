use std::collections::BTreeMap;

use anyhow::{Result, bail};
use zess_git::{FileStatus, GitDecoration, StagingStatus};
use zess_plugin::{PluginInput, PluginOutput, QueryWatch};
use zess_services::{QueryRequest, QueryResult};
use zess_ui::{DisplayCell, DisplayRow, Span};

use super::{Event, Services, Task, browser, message};

#[cfg(test)]
mod tests;

#[derive(Default)]
enum State {
    #[default]
    Unloaded,
    Loading(Task),
    Ready,
}

#[derive(Default)]
pub(super) struct GitStatus {
    state: State,
    entries: BTreeMap<String, GitDecoration>,
    directories: BTreeMap<String, DirectoryStatus>,
}

enum DirectoryStatus {
    Modified,
    Conflict,
}

impl GitStatus {
    pub(super) fn update(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        enabled: bool,
        output: &mut PluginOutput,
    ) -> Result<()> {
        if !enabled || event == Event::ViewLeaving {
            output.changed |= !self.entries.is_empty() || !self.directories.is_empty();
            *self = Self::default();
            return Ok(());
        }
        let request = QueryRequest::GitDecorations {};
        output.watches.get_or_insert_default().push(QueryWatch {
            operation: request.operation().into(),
            argument: request.argument(),
        });
        if matches!(self.state, State::Unloaded)
            || matches!(event, Event::Refresh | Event::Init | Event::ViewEntered)
            || browser::changed(event, input, request.operation(), None)
        {
            self.state = match Task::start(services, request) {
                Ok(task) => State::Loading(task),
                Err(error) => {
                    output
                        .requests
                        .push(message(&format!("Git status: {error:#}")));
                    State::Ready
                }
            };
        }
        if let State::Loading(task) = &mut self.state {
            match task.poll() {
                Ok(Some(QueryResult::GitDecorations {
                    entries,
                    path_warning,
                })) => {
                    if let Some(warning) = path_warning {
                        output
                            .requests
                            .push(message(&format!("Git status: {warning}")));
                    }
                    self.entries.clear();
                    self.directories.clear();
                    for entry in entries {
                        self.ancestors(&entry.path, entry.staging_status);
                        if let Some(original) = &entry.original_path {
                            self.ancestors(original, entry.staging_status);
                        }
                        self.entries.insert(entry.path.clone(), entry);
                    }
                    self.state = State::Ready;
                    output.changed = true;
                }
                Ok(Some(_)) => bail!("Git decorations query returned another kind of result"),
                Ok(None) => {}
                Err(error) => {
                    self.state = State::Ready;
                    output
                        .requests
                        .push(message(&format!("Git status: {error:#}")));
                }
            }
        }
        Ok(())
    }

    fn ancestors(&mut self, path: &str, staging_status: StagingStatus) {
        let mut path = path;
        while let Some((parent, _)) = path.rsplit_once('/') {
            let entry = self
                .directories
                .entry(parent.into())
                .or_insert(DirectoryStatus::Modified);
            if staging_status == StagingStatus::Conflict {
                *entry = DirectoryStatus::Conflict;
            }
            path = parent;
        }
    }

    pub(super) fn loading(&self) -> bool {
        matches!(self.state, State::Loading(_))
    }

    pub(super) fn decorate(&self, row: &mut DisplayRow, directory: bool) {
        let (text, style) = if directory {
            match self.directories.get(&row.id) {
                Some(DirectoryStatus::Modified) => ("M", "modified"),
                Some(DirectoryStatus::Conflict) => ("!", "error"),
                None => return,
            }
        } else {
            let Some(entry) = self.entries.get(&row.id) else {
                return;
            };
            match entry.staging_status {
                StagingStatus::Conflict => ("!", "error"),
                StagingStatus::Untracked => {
                    row.style = "git.added".into();
                    return;
                }
                StagingStatus::Unmodified | StagingStatus::Ignored => return,
                StagingStatus::Staged | StagingStatus::Unstaged | StagingStatus::Both => {
                    match entry.status {
                        FileStatus::Added => ("A", "git.added"),
                        FileStatus::Modified => ("M", "modified"),
                        FileStatus::Deleted => ("D", "git.deleted"),
                    }
                }
            }
        };
        row.style = style.into();
        row.cells.push(DisplayCell {
            spans: vec![Span {
                base_style: None,
                text: text.into(),
                style: style.into(),
            }],
        });
    }
}
