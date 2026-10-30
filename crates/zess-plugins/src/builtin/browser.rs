use anyhow::{Context, Result, bail};
use std::time::Instant;
use zess_plugin::{
    HostRequest, ItemSelection, PluginInput, PluginOutput, PreviewContent, PreviewLayout,
    PreviewPane, QueryWatch,
};
use zess_ui::DisplayRow;

use super::{
    Event, Services,
    schedule::{NavigationDebounce, QueryState},
};
use zess_plugin::PluginRegistrar;
use zess_services::{DirectoryEntryKind, QueryPoll, QueryRequest, QueryResult};

#[cfg(test)]
mod tests;

pub(super) struct Task {
    services: Services,
    handle: Option<u64>,
}

impl Task {
    pub(super) fn start(services: &Services, request: QueryRequest) -> Result<Self> {
        let handle = services.borrow_mut().start_query(request)?;
        Ok(Self {
            services: services.clone(),
            handle: Some(handle),
        })
    }

    pub(super) fn poll(&mut self) -> Result<Option<QueryResult>> {
        let handle = self.handle.context("job was already consumed")?;
        match self.services.borrow_mut().poll_query(handle)? {
            QueryPoll::Pending => Ok(None),
            QueryPoll::Ready(result) => {
                self.handle = None;
                result.map(Some).map_err(anyhow::Error::msg)
            }
        }
    }

    pub(super) fn status_progress(&mut self) -> Result<Option<Vec<zess_git::GitEntry>>> {
        let Some(handle) = self.handle else {
            return Ok(None);
        };
        self.services.borrow_mut().status_progress(handle)
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if let Some(handle) = self.handle {
            self.services.borrow_mut().cancel_query(handle);
        }
    }
}

pub(super) fn changed(
    event: Event,
    input: &PluginInput,
    operation: &str,
    path: Option<&str>,
) -> bool {
    if event != Event::FsChanged {
        return false;
    }
    let Some(queries) = input.argument["queries"].as_array() else {
        return true;
    };
    queries.iter().any(|query| {
        query["operation"] == operation && path.is_none_or(|path| query["argument"]["path"] == path)
    })
}

pub(super) fn single(id: Option<String>) -> ItemSelection {
    ItemSelection {
        primary: id.clone(),
        selected: id.iter().cloned().collect(),
        anchor: id,
    }
}

pub(super) fn reconcile(event: Event, input: &PluginInput, output: &mut PluginOutput) {
    let rows = &output.display.as_ref().unwrap().rows;
    let mut selection = output
        .selection
        .take()
        .unwrap_or_else(|| input.selection.clone());
    if event == Event::FilterChanged
        || !rows
            .iter()
            .any(|row| Some(&row.id) == selection.primary.as_ref())
    {
        selection = single(rows.first().map(|row| row.id.clone()));
    } else {
        selection
            .selected
            .retain(|id| rows.iter().any(|row| &row.id == id));
        if !rows
            .iter()
            .any(|row| Some(&row.id) == selection.anchor.as_ref())
        {
            selection.anchor = selection.primary.clone();
        }
    }
    output.selection = Some(selection);
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PreviewQuery {
    pub request: QueryRequest,
    pub path: String,
}

struct PreviewUpdate {
    path: String,
    content: PreviewContent,
    focus: bool,
}

#[derive(Default)]
pub(super) struct Browser {
    state: QueryState<PreviewQuery>,
    navigation: NavigationDebounce,
    static_preview: Option<String>,
    content: Option<PreviewContent>,
    content_query: Option<PreviewQuery>,
    initialized: bool,
    focus_on_ready: bool,
}

impl Browser {
    pub(super) fn cloned() -> Self {
        Self {
            initialized: true,
            ..Self::default()
        }
    }

    pub(super) fn leave(&mut self) {
        self.state = QueryState::default();
        self.navigation = NavigationDebounce::default();
        self.static_preview = None;
        self.focus_on_ready = false;
    }

    pub(super) fn pending(&self) -> bool {
        !matches!(self.state, QueryState::Idle { .. })
    }

    pub(super) fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.focus_on_ready)
    }

    pub(super) fn idle(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        output: &mut PluginOutput,
    ) -> Result<()> {
        self.static_preview = None;
        self.advance(services, event, input, output, None, Instant::now())?;
        self.focus_on_ready = false;
        Ok(())
    }

    pub(super) fn show_content(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        output: &mut PluginOutput,
        path: &str,
        content: impl FnOnce() -> PreviewContent,
    ) -> Result<()> {
        self.advance(services, event, input, output, None, Instant::now())?;
        self.focus_on_ready = false;
        if event == Event::ViewLeaving {
            return Ok(());
        }
        if self.static_preview.as_deref() == Some(path)
            && !matches!(
                event,
                Event::Preview
                    | Event::Refresh
                    | Event::FsChanged
                    | Event::ViewEntered
                    | Event::ViewCloned
            )
            && input.event != "render"
            && !output.changed
        {
            return Ok(());
        }
        self.static_preview = Some(path.into());
        output.requests.push(HostRequest::Preview {
            path: path.into(),
            content: content(),
            focus: event == Event::Preview,
        });
        Ok(())
    }

    pub(super) fn update(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        output: &mut PluginOutput,
        query: Option<PreviewQuery>,
    ) -> Result<()> {
        self.static_preview = None;
        if let Some(preview) =
            self.advance(services, event, input, output, query, Instant::now())?
        {
            output.requests.push(HostRequest::Preview {
                path: preview.path,
                content: preview.content,
                focus: preview.focus,
            });
            self.focus_on_ready = false;
        }
        Ok(())
    }

    pub(super) fn update_surface(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        output: &mut PluginOutput,
        query: Option<PreviewQuery>,
    ) -> Result<()> {
        self.static_preview = None;
        if let Some(preview) =
            self.advance(services, event, input, output, query, Instant::now())?
        {
            output.requests.push(HostRequest::PreviewGroup {
                panes: vec![PreviewPane::Surface {
                    id: "main".into(),
                    path: preview.path,
                    content: preview.content,
                    header: String::new(),
                    loading: false,
                }],
                layout: PreviewLayout::Rows,
                focus: false,
            });
            self.focus_on_ready = false;
        }
        Ok(())
    }

    pub(super) fn pane(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        output: &mut PluginOutput,
        query: Option<PreviewQuery>,
        mut pane: PreviewPane,
    ) -> Result<Option<PreviewPane>> {
        self.static_preview = None;
        self.advance(services, event, input, output, query, Instant::now())?;
        if event == Event::ViewLeaving {
            return Ok(None);
        }
        let PreviewPane::View {
            content,
            path,
            loading,
            ..
        } = &mut pane
        else {
            anyhow::bail!("interactive preview requires a view pane");
        };
        if let Some(value) = &self.content {
            *content = value.clone();
        }
        *path = self
            .content_query
            .as_ref()
            .map(|query| query.path.clone())
            .unwrap_or_default();
        *loading = !matches!(self.state, QueryState::Idle { .. })
            || (self.state.query().is_none()
                && output
                    .display
                    .as_ref()
                    .is_some_and(|display| display.loading));
        Ok(Some(pane))
    }

    fn advance(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        output: &mut PluginOutput,
        query: Option<PreviewQuery>,
        now: Instant,
    ) -> Result<Option<PreviewUpdate>> {
        let mut preview = None;
        if event == Event::ViewLeaving {
            self.leave();
            output.watches = Some(Vec::new());
            return Ok(None);
        }
        let ready_at = self.navigation.update(event, input, now)?;
        let navigation = ready_at.is_some() && event != Event::Preview;
        let immediate = ready_at == Some(now);
        if navigation {
            self.focus_on_ready = false;
        }
        if !self.initialized {
            preview = Some(PreviewUpdate {
                path: String::new(),
                content: PreviewContent::Unsupported {
                    message: String::new(),
                },
                focus: false,
            });
            self.initialized = true;
        }
        let invalidated = event == Event::Refresh
            || (matches!(self.state, QueryState::Idle { .. })
                && query.as_ref().is_some_and(|query| {
                    if matches!(query.request, QueryRequest::DiffFiles { .. }) {
                        event == Event::FsChanged
                            && input.argument["queries"].as_array().is_none_or(|queries| {
                                queries.iter().any(|changed| {
                                    changed["operation"] == query.request.operation()
                                        && changed["argument"] == query.request.argument()
                                })
                            })
                    } else {
                        changed(event, input, query.request.operation(), Some(&query.path))
                    }
                }));
        if query.as_ref() != self.state.query() || invalidated {
            let changed = query.as_ref() != self.state.query();
            if changed {
                self.focus_on_ready = false;
            }
            self.state = if let Some(query) = query {
                QueryState::Waiting {
                    query,
                    ready_at: if event == Event::Refresh || immediate {
                        now
                    } else if let Some(ready_at) = ready_at {
                        ready_at
                    } else if invalidated {
                        self.navigation.quiet_until(now)
                    } else {
                        now.checked_add(input.preview_debounce)
                            .context("preview debounce exceeds the supported time range")?
                    },
                }
            } else {
                if !output
                    .display
                    .as_ref()
                    .is_some_and(|display| display.loading)
                {
                    self.content = None;
                    self.content_query = None;
                    preview = Some(PreviewUpdate {
                        path: String::new(),
                        content: PreviewContent::Directory {
                            rows: Vec::new(),
                            column_alignment: Default::default(),
                        },
                        focus: false,
                    });
                }
                QueryState::default()
            };
        }
        if let Some(ready_at) = ready_at
            && ready_at > now
        {
            self.state.defer(ready_at);
        }
        if let QueryState::Waiting {
            ready_at: deadline, ..
        } = &mut self.state
            && let Some(ready_at) = ready_at
        {
            *deadline = ready_at;
        }
        let cached = if let QueryState::Waiting { query, .. } = &self.state {
            services.borrow().cached_query(&query.request)?
        } else {
            None
        };
        if cached.is_none()
            && let QueryState::Waiting { query, ready_at } = &self.state
            && *ready_at <= now
        {
            self.state = QueryState::Running {
                query: query.clone(),
                task: Task::start(services, query.request.clone())?,
            };
        }
        let result = if let Some(value) = cached {
            Some(Ok(value))
        } else if let QueryState::Running { task, .. } = &mut self.state {
            task.poll().transpose()
        } else {
            None
        };
        if let Some(result) = result {
            let query = self.state.query().unwrap().clone();
            self.state = QueryState::Idle {
                query: Some(query.clone()),
            };
            self.content = Some(match result {
                Ok(value) => decode_preview(value)?,
                Err(error) => PreviewContent::Unsupported {
                    message: format!("{error:#}"),
                },
            });
            self.content_query = Some(query.clone());
            preview = Some(PreviewUpdate {
                path: query.path,
                content: self.content.clone().unwrap(),
                focus: self.focus_on_ready,
            });
        }
        if event == Event::Preview {
            self.focus_on_ready = true;
            if matches!(self.state, QueryState::Idle { .. })
                && let Some(content) = &self.content
                && self.content_query.as_ref() == self.state.query()
            {
                preview = Some(PreviewUpdate {
                    path: self
                        .state
                        .query()
                        .map(|query| query.path.clone())
                        .unwrap_or_default(),
                    content: content.clone(),
                    focus: true,
                });
            }
        }
        let after_ms = match &self.state {
            QueryState::Waiting { ready_at, .. } => {
                Some(ready_at.saturating_duration_since(now).as_millis() as u64)
            }
            QueryState::Running { .. } => Some(50),
            QueryState::Idle { .. } => None,
        };
        if let Some(after_ms) = after_ms {
            output.display.as_mut().unwrap().loading = true;
            output.requests.push(HostRequest::Schedule { after_ms });
        }
        if let Some(query) = if matches!(self.state, QueryState::Waiting { .. }) {
            self.content_query.as_ref()
        } else {
            self.state.query()
        } {
            output.watches.get_or_insert_default().push(QueryWatch {
                operation: query.request.operation().into(),
                argument: query.request.argument(),
            });
        }
        Ok(preview)
    }
}

fn decode_preview(value: QueryResult) -> Result<PreviewContent> {
    Ok(match value {
        QueryResult::Directory { entries, .. } => PreviewContent::Directory {
            column_alignment: Default::default(),
            rows: entries
                .into_iter()
                .map(|entry| DisplayRow {
                    id: entry.path,
                    text: entry.name,
                    style: if entry.kind == DirectoryEntryKind::Directory {
                        "directory".into()
                    } else {
                        String::new()
                    },
                    depth: 0,
                    ..DisplayRow::default()
                })
                .collect(),
        },
        QueryResult::Snapshot { snapshot } => PreviewContent::Snapshot { snapshot },
        QueryResult::Unsupported { message } => PreviewContent::Unsupported { message },
        QueryResult::Text {
            mut text,
            truncated,
            ..
        } => {
            if truncated {
                text.push_str("\n… Preview truncated\n");
            }
            PreviewContent::Text { text }
        }
        QueryResult::Diff { text, .. } => PreviewContent::Diff { text },
        QueryResult::GitDiff { diff } => PreviewContent::GitDiff { diff },
        _ => bail!("query did not return preview content"),
    })
}

pub(super) fn bind_event(
    registry: &mut dyn PluginRegistrar,
    kind: &str,
    event: &str,
    description: &str,
    keys: &[&str],
) {
    let command = format!("{kind}.{event}");
    registry.buffer_command(&command, description, kind, event);
    for mode in ["normal", "select"] {
        registry.binding(kind, mode, keys, &command);
    }
}

pub(super) fn register_file_actions(
    registry: &mut dyn PluginRegistrar,
    kind: &str,
    open_description: &str,
    layer_description: &str,
) {
    super::file_actions::register(registry, kind);
    bind_event(registry, kind, "edit", open_description, &["g", "f"]);
    bind_event(registry, kind, "edit-layer", layer_description, &["e"]);
}

pub(super) fn register(registry: &mut dyn PluginRegistrar, kind: &str) {
    bind_event(registry, kind, "refresh", "Refresh browser", &["R"]);
    registry.buffer_command(&format!("{kind}.preview"), "Read preview", kind, "preview");
    for (key, command) in [
        ("H", "preview.move-left"),
        ("J", "preview.move-down"),
        ("K", "preview.move-up"),
        ("L", "preview.move-right"),
    ] {
        for mode in ["normal", "select"] {
            registry.binding(kind, mode, &[key], command);
        }
    }
}
