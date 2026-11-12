use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
};
use zess_filter::{Case, Filter};

use anyhow::{Context, Result, bail};
use serde_json::json;
use zess_plugin::{
    BufferId, HostRequest, ItemSelection, OperationCompletion, PluginInput, PluginOutput,
    QueryWatch, ViewId,
};
use zess_ui::{Display, DisplayRow};

use super::{
    Event, Services,
    browser::{self, Browser, PreviewQuery, Task},
    file_actions::{Source, Target, TargetKind, Yanks},
};
use zess_plugin::BufferPlugin;

use zess_services::{DirectoryEntry as Entry, DirectoryEntryKind, QueryRequest, QueryResult};

use zess_services::FileOperation as Operation;

mod git_status;

#[cfg(test)]
mod file_yank;

#[derive(Default)]
struct Model {
    pending: Vec<Operation>,
    directories: BTreeMap<String, Vec<Entry>>,
}

#[derive(Default)]
struct FileView {
    expanded: BTreeSet<String>,
    filter: Filter,
    show_ignored: bool,
    target: Option<String>,
    directories: BTreeMap<String, DirectoryRequest>,
    rename: Option<RenameSession>,
    browser: Browser,
    find: Option<FindSession>,
    yanks: Yanks,
    git_status: git_status::GitStatus,
}

#[derive(Default)]
enum DirectoryRequest {
    #[default]
    NotRequested,
    Running(Task),
    Settled,
}

enum RenameSession {
    Collecting {
        targets: VecDeque<String>,
        destinations: Vec<(String, String)>,
    },
    Ready {
        destinations: Vec<(String, String)>,
    },
}

impl RenameSession {
    fn collect(&self, text: &str) -> Self {
        let Self::Collecting {
            targets,
            destinations,
        } = self
        else {
            unreachable!("only collecting sessions accept a destination");
        };
        let mut targets = targets.clone();
        let mut destinations = destinations.clone();
        if let Some(source) = targets.pop_front() {
            destinations.push((source, text.into()));
        }
        if targets.is_empty() {
            Self::Ready { destinations }
        } else {
            Self::Collecting {
                targets,
                destinations,
            }
        }
    }
}

struct FindSession {
    query: Filter,
    original_selection: ItemSelection,
    traversal: FindTraversal,
}

struct FindTraversal {
    pending: VecDeque<String>,
    tasks: BTreeMap<String, Task>,
    visited: BTreeSet<String>,
    entries: Vec<Entry>,
}

impl FindSession {
    fn new(original_selection: ItemSelection) -> Self {
        Self {
            query: Filter::default(),
            original_selection,
            traversal: FindTraversal {
                pending: VecDeque::from([String::new()]),
                tasks: BTreeMap::new(),
                visited: BTreeSet::new(),
                entries: Vec::new(),
            },
        }
    }

    fn restart(self) -> Self {
        Self {
            query: self.query,
            ..Self::new(self.original_selection)
        }
    }

    fn advance(
        &mut self,
        services: &Services,
        model: &mut Model,
        show_ignored: bool,
        output: &mut PluginOutput,
    ) -> Result<()> {
        let mut complete = Vec::new();
        for (path, task) in &mut self.traversal.tasks {
            match task.poll() {
                Ok(Some(QueryResult::Directory { entries, .. })) => {
                    for entry in &entries {
                        if entry.ignored && !show_ignored {
                            continue;
                        }
                        if entry.kind == DirectoryEntryKind::Directory {
                            self.traversal.pending.push_back(entry.path.clone());
                        } else {
                            self.traversal.entries.push(entry.clone());
                        }
                    }
                    model.directories.insert(path.clone(), entries);
                    complete.push(path.clone());
                }
                Ok(Some(_)) => bail!("file search returned another kind of result"),
                Ok(None) => {}
                Err(error) => {
                    output.requests.push(message(&format!("{error:#}")));
                    complete.push(path.clone());
                }
            }
        }
        for path in complete {
            self.traversal.tasks.remove(&path);
        }
        while self.traversal.tasks.len() < 4 {
            let Some(path) = self.traversal.pending.pop_front() else {
                break;
            };
            let task = Task::start(services, QueryRequest::List { path: path.clone() })?;
            self.traversal.visited.insert(path.clone());
            self.traversal.tasks.insert(path, task);
        }
        self.traversal.entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(())
    }

    fn loading(&self) -> bool {
        !self.traversal.pending.is_empty() || !self.traversal.tasks.is_empty()
    }
}

pub(super) struct Files {
    services: Services,
    models: BTreeMap<BufferId, Model>,
    views: BTreeMap<(BufferId, ViewId), FileView>,
    operation_completions: Vec<OperationCompletion>,
}

impl Files {
    pub(super) fn new(services: Services) -> Self {
        Self {
            services,
            models: BTreeMap::new(),
            views: BTreeMap::new(),
            operation_completions: Vec::new(),
        }
    }
}

impl BufferPlugin for Files {
    fn update(&mut self, input: &PluginInput) -> Result<PluginOutput> {
        let event = Event::parse(&input.event);
        let git_status = git_status::enabled(&input.options)?;
        if input
            .options
            .get("preview-wrap")
            .is_some_and(|value| !value.is_boolean())
        {
            bail!("preview-wrap must be a boolean");
        }
        let key = (input.buffer_id, input.view_id);
        if event == Event::ViewCloned
            && let Some(source) = input.argument["source_view"].as_u64()
        {
            if let Some(old) = self.views.get(&(input.buffer_id, source)) {
                self.views.insert(
                    key,
                    FileView {
                        expanded: old.expanded.clone(),
                        filter: old.filter.clone(),
                        show_ignored: old.show_ignored,
                        browser: Browser::cloned(),
                        ..FileView::default()
                    },
                );
            }
        }
        let model = self.models.entry(input.buffer_id).or_default();
        let view = self.views.entry(key).or_default();
        let old_pending = model.pending.clone();
        let mut output = PluginOutput::default();
        view.git_status
            .update(&self.services, event, input, git_status, &mut output)?;
        let entries = tree(model, view);
        let primary = input.selection.primary.as_deref();
        let current = entries
            .iter()
            .find(|(entry, _)| Some(entry.path.as_str()) == primary);
        let selected = &input.selection.selected;
        match event {
            Event::Init | Event::Navigate => {
                if let Some(path) = input.argument["path"].as_str() {
                    let mut directory = parent(path);
                    while !directory.is_empty() {
                        view.expanded.insert(directory.clone());
                        directory = parent(&directory);
                    }
                    view.target = Some(path.into());
                }
            }
            Event::Left => {
                if let Some((entry, _)) = current {
                    if !view.expanded.remove(&entry.path) {
                        let parent = parent(&entry.path);
                        if !parent.is_empty() {
                            output.selection = Some(browser::single(Some(parent)));
                        }
                    } else {
                        view.expanded
                            .retain(|path| !path.starts_with(&format!("{}/", entry.path)));
                    }
                }
            }
            Event::Right => {
                if let Some((entry, _)) = current
                    && entry.kind == DirectoryEntryKind::Directory
                {
                    view.expanded.insert(entry.path.clone());
                }
            }
            Event::Toggle => {
                if let Some((entry, _)) = current
                    && entry.kind == DirectoryEntryKind::Directory
                    && !view.expanded.remove(&entry.path)
                {
                    view.expanded.insert(entry.path.clone());
                }
            }
            Event::YankPath
            | Event::YankAbsolutePath
            | Event::YankName
            | Event::YankContent
            | Event::YankDiff => {
                let selected = selected
                    .iter()
                    .map(|path| {
                        entries
                            .iter()
                            .map(|(entry, _)| entry.as_ref())
                            .chain(view.find.iter().flat_map(|find| &find.traversal.entries))
                            .find(|entry| &entry.path == path)
                            .ok_or_else(|| anyhow::anyhow!("Selected path is unavailable: {path}"))
                            .map(|entry| Target {
                                path: entry.path.clone(),
                                name: entry.name.clone(),
                                kind: if entry.kind == DirectoryEntryKind::Directory {
                                    TargetKind::Directory
                                } else {
                                    TargetKind::File {
                                        source: Source::Worktree,
                                        original_path: None,
                                    }
                                },
                            })
                    })
                    .collect::<Result<Vec<_>>>()?;
                view.yanks
                    .queue(&self.services, event, input, selected, &mut output)?;
            }
            Event::Find => {
                view.target = None;
                view.find = Some(FindSession::new(input.selection.clone()));
                output.requests.push(HostRequest::SetMode {
                    mode: "find".into(),
                });
                output.requests.push(HostRequest::MiniBuffer {
                    purpose: "find".into(),
                    text: String::new(),
                    key_scope: Some("files.find".into()),
                });
            }
            Event::MiniBufferChanged if input.argument["purpose"] == "find" => {
                if let Some(find) = &mut view.find {
                    find.query = Filter::compile(
                        input.query.as_ref().context("Find query is required")?,
                        Case::Sensitive,
                    )?;
                    output.selection = Some(ItemSelection::default());
                }
            }
            Event::LayerResult if input.argument["purpose"] == "find" => {
                if input.argument["cancelled"] == true {
                    if let Some(find) = view.find.take() {
                        output.selection = Some(find.original_selection);
                    }
                    output.requests.push(HostRequest::SetMode {
                        mode: "normal".into(),
                    });
                } else if let Some(find) = &view.find {
                    let path = primary.filter(|path| {
                        find.traversal
                            .entries
                            .iter()
                            .any(|entry| entry.path == *path && find.query.is_match(&entry.path))
                    });
                    let Some(path) = path else {
                        bail!("No file selected");
                    };
                    open(&mut output, path, false);
                }
            }
            Event::ToggleIgnored => {
                view.show_ignored = !view.show_ignored;
                if let Some(find) = view.find.take() {
                    view.find = Some(find.restart());
                }
            }
            Event::FilterChanged => {
                view.filter = Filter::compile(
                    input.query.as_ref().context("Filter query is required")?,
                    Case::Sensitive,
                )?;
                view.target = None;
            }
            Event::Select => view.target = None,
            Event::Open => {
                if let Some((entry, _)) = current
                    && entry.kind != DirectoryEntryKind::Directory
                {
                    open(&mut output, &entry.path, false);
                }
            }
            Event::Edit | Event::EditLayer => {
                if let Some((entry, _)) = current
                    && entry.kind != DirectoryEntryKind::Directory
                {
                    open(&mut output, &entry.path, event == Event::EditLayer);
                }
            }
            Event::Create | Event::Rename => {
                if event == Event::Rename && selected.is_empty() {
                    output.requests.push(message("Select a path to rename"));
                } else {
                    view.rename = (event == Event::Rename).then(|| RenameSession::Collecting {
                        targets: selected.iter().cloned().collect(),
                        destinations: Vec::new(),
                    });
                    let text = if let Some(RenameSession::Collecting { targets, .. }) = &view.rename
                    {
                        destination(model, targets.front().unwrap())
                    } else {
                        let directory = current
                            .map(|(entry, _)| {
                                if entry.kind == DirectoryEntryKind::Directory {
                                    entry.path.clone()
                                } else {
                                    parent(&entry.path)
                                }
                            })
                            .unwrap_or_default();
                        if directory.is_empty() {
                            String::new()
                        } else {
                            format!("{directory}/")
                        }
                    };
                    output.requests.push(HostRequest::MiniBuffer {
                        purpose: input.event.clone(),
                        text,
                        key_scope: None,
                    });
                }
            }
            Event::Delete => {
                let mut candidate = model.pending.clone();
                let mut operations = Vec::new();
                for path in selected {
                    let created = candidate
                        .iter()
                        .any(|op| matches!(op,Operation::Create{path:created,..} if created==path));
                    candidate.retain(|op| op.path() != path);
                    if !created {
                        operations.push(Operation::Delete { path: path.clone() });
                    }
                }
                candidate.extend(operations);
                validate(&self.services, &candidate)?;
                model.pending = candidate;
            }
            Event::UndoPending => model
                .pending
                .retain(|op| !selected.iter().any(|path| path == op.path())),
            Event::LayerResult => {
                let text = input.argument["text"].as_str().unwrap_or("");
                if input.argument["cancelled"] == true || text.is_empty() {
                    view.rename = None;
                } else if input.argument["purpose"] == "rename" {
                    if let Some(session) = &view.rename {
                        let next = session.collect(text);
                        if let RenameSession::Collecting { targets, .. } = &next {
                            output.requests.push(HostRequest::MiniBuffer {
                                purpose: "rename".into(),
                                text: destination(model, targets.front().unwrap()),
                                key_scope: None,
                            });
                            view.rename = Some(next);
                        } else if let RenameSession::Ready { destinations } = next {
                            let mut candidate = model.pending.clone();
                            for (path, to) in &destinations {
                                let created = candidate.iter().find_map(|op| match op {
                                    Operation::Create {
                                        path: created,
                                        directory,
                                    } if created == path => Some(*directory),
                                    _ => None,
                                });
                                candidate.retain(|op| op.path() != path);
                                candidate.push(if let Some(directory) = created {
                                    Operation::Create {
                                        path: to.trim_end_matches('/').into(),
                                        directory,
                                    }
                                } else {
                                    Operation::Rename {
                                        path: path.clone(),
                                        to: to.clone(),
                                    }
                                });
                            }
                            validate(&self.services, &candidate)?;
                            model.pending = candidate;
                            view.rename = None;
                        }
                    }
                } else {
                    let path = text.trim_end_matches('/').to_owned();
                    let mut candidate = model.pending.clone();
                    candidate.retain(|op| op.path() != path);
                    candidate.push(Operation::Create {
                        path,
                        directory: text.ends_with('/'),
                    });
                    validate(&self.services, &candidate)?;
                    model.pending = candidate;
                }
            }
            Event::Save => {
                let result = self
                    .services
                    .borrow_mut()
                    .apply_file_operations(&model.pending)?;
                model.pending = result.remaining;
                output.requests.push(message(
                    result.error.as_deref().unwrap_or("Saved file operations"),
                ));
            }
            _ => {}
        }
        if matches!(event, Event::FsChanged | Event::Refresh)
            && let Some(find) = view.find.take()
        {
            view.find = Some(find.restart());
        }
        if event == Event::Refresh {
            self.services.borrow_mut().invalidate_fs();
            view.directories.clear();
        }
        if event == Event::ViewLeaving {
            view.find = None;
            view.rename = None;
            view.directories.clear();
        } else {
            view.directories
                .retain(|path, _| !browser::changed(event, input, "fs.list", Some(path)));
            for (path, job) in &mut view.directories {
                let DirectoryRequest::Running(task) = job else {
                    continue;
                };
                match task.poll() {
                    Ok(Some(QueryResult::Directory { entries, .. })) => {
                        model.directories.insert(path.clone(), entries);
                        *job = DirectoryRequest::Settled;
                        output.changed = true;
                    }
                    Ok(Some(_)) => bail!("directory query returned another kind of result"),
                    Ok(None) => {}
                    Err(error) => {
                        *job = DirectoryRequest::Settled;
                        output.requests.push(message(&format!("{error:#}")));
                    }
                }
            }
            let entries = tree(model, view);
            let mut paths = BTreeSet::from([String::new()]);
            for (entry, _) in &entries {
                if entry.kind == DirectoryEntryKind::Directory
                    && view.expanded.contains(&entry.path)
                {
                    paths.insert(entry.path.clone());
                }
            }
            view.directories.retain(|path, _| paths.contains(path));
            for path in paths {
                if view
                    .directories
                    .values()
                    .filter(|job| matches!(job, DirectoryRequest::Running(_)))
                    .count()
                    >= 4
                {
                    break;
                }
                let request = view.directories.entry(path.clone()).or_default();
                if matches!(request, DirectoryRequest::NotRequested) {
                    *request = DirectoryRequest::Running(Task::start(
                        &self.services,
                        QueryRequest::List { path },
                    )?);
                }
            }
        }
        let mut entries = tree(model, view);
        for operation in &model.pending {
            if let Operation::Create { path, directory } = operation {
                entries.push((
                    Cow::Owned(Entry {
                        path: path.clone(),
                        name: path.clone(),
                        kind: if *directory {
                            DirectoryEntryKind::Directory
                        } else {
                            DirectoryEntryKind::File
                        },
                        ignored: false,
                        size: 0,
                    }),
                    0,
                ));
            }
        }
        let rows = if let Some(find) = &mut view.find {
            find.advance(&self.services, model, view.show_ignored, &mut output)?;
            find.traversal
                .entries
                .iter()
                .filter(|entry| find.query.is_match(&entry.path))
                .map(|entry| {
                    let mut row = DisplayRow {
                        id: entry.path.clone(),
                        text: entry.path.clone(),
                        ..DisplayRow::default()
                    };
                    view.git_status.decorate(&mut row, false);
                    row
                })
                .collect()
        } else {
            entries
                .into_iter()
                .filter(|(entry, _)| view.filter.is_match(&entry.path))
                .map(|(entry, depth)| row(entry, depth, model, view))
                .collect::<Vec<_>>()
        };
        if let Some(target) = &view.target
            && rows.iter().any(|row| &row.id == target)
        {
            output.selection = Some(browser::single(view.target.take()));
        }
        let loading = view
            .directories
            .values()
            .any(|request| matches!(request, DirectoryRequest::Running(_)))
            || view.find.as_ref().is_some_and(FindSession::loading)
            || view.git_status.loading();
        output.changed |= old_pending != model.pending;
        output.modified = Some(!model.pending.is_empty());
        output.display = Some(Display {
            column_alignment: Default::default(),
            rows_revision: None,
            tabs: Vec::new(),
            title: "Files".into(),
            loading,
            header: if view.filter.is_empty() || input.mode == "filter" || view.find.is_some() {
                String::new()
            } else {
                format!("/{}", view.filter.source())
            },
            rows,
            status: if model.pending.is_empty() {
                String::new()
            } else {
                zess_plugin::default_status_text(
                    "pending",
                    &[("count", model.pending.len().to_string())],
                )?
            }
            .into(),
            status_data: json!({"files": {
                "pending_count": model.pending.len(),
            }}),
        });
        let paths = view
            .directories
            .keys()
            .chain(view.find.iter().flat_map(|find| &find.traversal.visited))
            .collect::<BTreeSet<_>>();
        let watches = output.watches.get_or_insert_default();
        watches.extend(paths.into_iter().map(|path| QueryWatch {
            operation: "fs.list".into(),
            argument: json!({"path":path}),
        }));
        browser::reconcile(event, input, &mut output);
        let query = output
            .selection
            .as_ref()
            .and_then(|s| s.primary.as_ref())
            .map(|path| PreviewQuery {
                request: QueryRequest::Preview {
                    path: path.clone(),
                    show_ignored: view.show_ignored,
                },
                path: path.clone(),
            });
        view.browser
            .update_surface(&self.services, event, input, &mut output, query)?;
        view.yanks.advance(&self.services, event, &mut output);
        if loading {
            output.requests.push(HostRequest::Schedule { after_ms: 50 });
        }
        Ok(output)
    }

    fn poll_operations(&mut self) -> Vec<PluginOutput> {
        let mut outputs = self
            .operation_completions
            .drain(..)
            .map(|completion| PluginOutput {
                completed_operations: vec![completion],
                ..PluginOutput::default()
            })
            .collect::<Vec<_>>();
        for view in self.views.values_mut() {
            outputs.extend(view.yanks.poll(&self.services));
        }
        outputs
    }

    fn remove_buffer_view(&mut self, buffer: BufferId, view: ViewId) {
        if let Some(mut view) = self.views.remove(&(buffer, view)) {
            self.operation_completions.extend(view.yanks.cancel());
        }
    }

    fn remove_view(&mut self, id: ViewId) {
        self.views.retain(|(_, view_id), view| {
            if *view_id == id {
                self.operation_completions.extend(view.yanks.cancel());
                false
            } else {
                true
            }
        });
    }
    fn remove_buffer(&mut self, id: BufferId) {
        self.models.remove(&id);
        self.views.retain(|(buffer, _), view| {
            if *buffer == id {
                self.operation_completions.extend(view.yanks.cancel());
                false
            } else {
                true
            }
        });
    }
}

fn tree<'a>(model: &'a Model, view: &FileView) -> Vec<(Cow<'a, Entry>, usize)> {
    fn visit<'a>(
        model: &'a Model,
        view: &FileView,
        path: &str,
        depth: usize,
        out: &mut Vec<(Cow<'a, Entry>, usize)>,
    ) {
        if let Some(entries) = model.directories.get(path) {
            for entry in entries {
                if entry.ignored && !view.show_ignored {
                    continue;
                }
                out.push((Cow::Borrowed(entry), depth));
                if entry.kind == DirectoryEntryKind::Directory
                    && view.expanded.contains(&entry.path)
                {
                    visit(model, view, &entry.path, depth + 1, out);
                }
            }
        }
    }
    let mut entries = Vec::new();
    visit(model, view, "", 0, &mut entries);
    entries
}

fn row(entry: Cow<'_, Entry>, depth: usize, model: &Model, view: &FileView) -> DisplayRow {
    let entry = entry.into_owned();
    let directory = entry.kind == DirectoryEntryKind::Directory;
    let operation = model.pending.iter().find(|op| op.path() == entry.path);
    let (label, style) = match operation {
        Some(Operation::Delete { .. }) => (format!("− {} [delete]", entry.path), "error"),
        Some(Operation::Rename { to, .. }) => (format!("{} → {to}", entry.path), "warning"),
        Some(Operation::Create { .. }) => (format!("+ {} [new, unsaved]", entry.path), "warning"),
        None => (entry.name, if directory { "directory" } else { "" }),
    };
    let prefix = if directory {
        if view.expanded.contains(&entry.path) {
            "▾ "
        } else {
            "▸ "
        }
    } else {
        "  "
    };
    let mut row = DisplayRow {
        id: entry.path,
        text: format!("{prefix}{label}"),
        style: style.into(),
        depth,
        ..DisplayRow::default()
    };
    if operation.is_none() {
        view.git_status.decorate(&mut row, directory);
    }
    row
}

fn parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
        .into()
}
fn message(text: &str) -> HostRequest {
    HostRequest::Message { text: text.into() }
}
fn destination(model: &Model, path: &str) -> String {
    model
        .pending
        .iter()
        .find_map(|op| match op {
            Operation::Rename { path: from, to } if from == path => Some(to.clone()),
            _ => None,
        })
        .unwrap_or_else(|| path.into())
}
fn validate(services: &Services, operations: &[Operation]) -> Result<()> {
    services.borrow().validate_file_operations(operations)
}

fn open(output: &mut PluginOutput, path: &str, layer: bool) {
    output.requests.push(if layer {
        HostRequest::FileLayer {
            path: path.into(),
            line: 0,
        }
    } else {
        HostRequest::Action {
            name: "browser.open-source".into(),
            argument: json!({"path":path,"line":0}),
        }
    });
}
