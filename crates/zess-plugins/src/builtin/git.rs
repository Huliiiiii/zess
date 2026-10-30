use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use zess_filter::{Case, Filter};

use anyhow::{Context, Result, bail};
use serde_json::json;
use zess_git::{
    Commit, CommitFile, DiffRenderOptions, DiffSide, DiffSource, DiffTarget, FileStatus, GitEntry,
    LineStats, StagingStatus,
};
use zess_plugin::{
    BufferId, HostRequest, Navigation, PluginInput, PluginOutput, PreviewContent, PreviewLayout,
    PreviewPane, QueryWatch, SnapshotPresentation, ViewId,
};
use zess_services::{QueryRequest, QueryResult};
use zess_ui::{ColumnAlignment, Display, DisplayRow};

use super::{
    Event, Services,
    browser::{self, Browser, PreviewQuery, Task},
    file_actions::{Source, Target, TargetKind, Yanks},
    schedule::QueryState,
};
use zess_plugin::BufferPlugin;

#[cfg(test)]
mod history_preview_tests;
#[cfg(test)]
mod scheduling_tests;
#[cfg(test)]
mod staging_tests;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Page {
    Changes,
    History,
    CommitFiles,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum GitTreeMode {
    #[default]
    Tree,
    Flat,
}

impl GitTreeMode {
    fn parse(options: &serde_json::Value) -> Result<Self> {
        match options.get("git-view") {
            None => Ok(Self::Tree),
            Some(value) => match value.as_str() {
                Some("tree") => Ok(Self::Tree),
                Some("flat") => Ok(Self::Flat),
                _ => bail!("git-view must be \"tree\" or \"flat\""),
            },
        }
    }
}

fn column_alignment(options: &serde_json::Value) -> Result<ColumnAlignment> {
    match options.get("stats-alignment") {
        None => Ok(ColumnAlignment::All),
        Some(value) => match value.as_str() {
            Some("none") => Ok(ColumnAlignment::None),
            Some("consecutive") => Ok(ColumnAlignment::Consecutive),
            Some("all") => Ok(ColumnAlignment::All),
            _ => bail!("stats-alignment must be \"none\", \"consecutive\", or \"all\""),
        },
    }
}

fn line_stats_cells(stats: LineStats) -> Vec<zess_ui::DisplayCell> {
    [
        (format!("+{}", stats.additions), "git.added"),
        (format!("-{}", stats.deletions), "git.deleted"),
    ]
    .into_iter()
    .map(|(text, style)| zess_ui::DisplayCell {
        spans: vec![zess_ui::Span {
            base_style: None,
            text,
            style: style.into(),
        }],
    })
    .collect()
}

enum TreeFile {
    Changed(GitEntry),
    Committed(CommitFile),
}

#[derive(Clone, Copy)]
enum LineCountState {
    Loading,
    Ready,
    Failed,
}

impl TreeFile {
    fn line_stats(&self) -> Option<LineStats> {
        match self {
            Self::Changed(entry) => entry.line_stats,
            Self::Committed(file) => file.line_stats(),
        }
    }

    fn path(&self) -> &str {
        match self {
            Self::Changed(entry) => &entry.path,
            Self::Committed(file) => &file.path,
        }
    }

    fn original_path(&self) -> Option<&str> {
        match self {
            Self::Changed(entry) => entry.original_path.as_deref(),
            Self::Committed(file) => file.original_path.as_deref(),
        }
    }

    fn status_cell(&self) -> zess_ui::DisplayCell {
        let spans = match self {
            Self::Changed(entry) if entry.untracked => Vec::new(),
            Self::Changed(entry) => {
                let conflict = entry.staging_status() == StagingStatus::Conflict;
                [
                    (&entry.index, "git.added"),
                    (&entry.worktree, "git.deleted"),
                ]
                .into_iter()
                .map(|(text, style)| zess_ui::Span {
                    base_style: None,
                    text: text.clone(),
                    style: if conflict { "error" } else { style }.into(),
                })
                .collect()
            }
            Self::Committed(file) => vec![zess_ui::Span {
                base_style: None,
                text: file.status.clone(),
                style: self.style().into(),
            }],
        };
        zess_ui::DisplayCell { spans }
    }

    fn style(&self) -> &str {
        match self {
            Self::Changed(entry) if entry.staging_status() == StagingStatus::Conflict => "error",
            Self::Changed(entry) => match entry.status() {
                FileStatus::Added => "git.added",
                FileStatus::Modified => "modified",
                FileStatus::Deleted => "git.deleted",
            },
            Self::Committed(file) => match file.status.chars().next() {
                Some('A') => "git.added",
                Some('D') => "git.deleted",
                _ => "modified",
            },
        }
    }
}

enum TreeNodeKind {
    Directory { path: String, children: Vec<String> },
    File(TreeFile),
}

struct TreeNode {
    kind: TreeNodeKind,
    parent: Option<String>,
    row: DisplayRow,
}

impl TreeNode {
    fn compare(&self, other: &Self) -> Ordering {
        let file = matches!(self.kind, TreeNodeKind::File(_));
        let other_file = matches!(other.kind, TreeNodeKind::File(_));
        let path = match &self.kind {
            TreeNodeKind::Directory { path, .. } => path,
            TreeNodeKind::File(file) => file.path(),
        };
        let other_path = match &other.kind {
            TreeNodeKind::Directory { path, .. } => path,
            TreeNodeKind::File(file) => file.path(),
        };
        file.cmp(&other_file).then(path.cmp(other_path))
    }
}

#[derive(Default)]
struct GitTree {
    mode: GitTreeMode,
    revision: Option<u64>,
    rows_revision: u64,
    filter: String,
    nodes: BTreeMap<String, TreeNode>,
    roots: Vec<String>,
    collapsed: BTreeSet<String>,
}

impl GitTree {
    fn rebuild(
        &mut self,
        files: Vec<TreeFile>,
        mode: GitTreeMode,
        revision: u64,
        filter: &Filter,
        counts: LineCountState,
    ) {
        self.rows_revision += 1;
        self.mode = mode;
        self.revision = Some(revision);
        self.filter = filter.source().into();
        self.nodes.clear();
        self.roots.clear();
        for file in files {
            let path = file.path();
            let full_path = if let Some(original) = file.original_path() {
                format!("{original} → {path}")
            } else {
                path.to_owned()
            };
            let matches = filter.is_match(path)
                || match &file {
                    TreeFile::Changed(entry) => {
                        filter.is_match(&format!("{}{} {full_path}", entry.index, entry.worktree))
                    }
                    TreeFile::Committed(file) => {
                        filter.is_match(&format!("{} {full_path}", file.status))
                    }
                };
            if !matches {
                continue;
            }
            let parent = if mode == GitTreeMode::Tree {
                path.rsplit_once('/').map(|(path, _)| self.directory(path))
            } else {
                None
            };
            let name = if mode == GitTreeMode::Tree {
                path.rsplit('/').next().unwrap()
            } else {
                path
            };
            let label = match file.original_path() {
                Some(original) => format!("{original} → {name}"),
                None => name.to_owned(),
            };
            let id = format!("file:{path}");
            let depth = if mode == GitTreeMode::Tree {
                path.matches('/').count()
            } else {
                0
            };
            let mut cells = vec![file.status_cell()];
            cells.extend(file.line_stats().map(line_stats_cells).unwrap_or_else(|| {
                match counts {
                    LineCountState::Loading => ["+…", "-…"]
                        .into_iter()
                        .map(|text| zess_ui::DisplayCell {
                            spans: vec![zess_ui::Span {
                                base_style: None,
                                text: text.into(),
                                style: "comment".into(),
                            }],
                        })
                        .collect(),
                    LineCountState::Ready | LineCountState::Failed => {
                        vec![zess_ui::DisplayCell {
                            spans: vec![zess_ui::Span {
                                base_style: None,
                                text: match counts {
                                    LineCountState::Failed => "count failed",
                                    _ => "uncounted",
                                }
                                .into(),
                                style: "comment".into(),
                            }],
                        }]
                    }
                }
            }));
            let row = DisplayRow {
                id: id.clone(),
                text: label,
                cells,
                style: file.style().into(),
                depth,
            };
            self.nodes.insert(
                id.clone(),
                TreeNode {
                    kind: TreeNodeKind::File(file),
                    parent: parent.clone(),
                    row,
                },
            );
            self.append(parent.as_deref(), id);
        }
        if mode == GitTreeMode::Tree {
            let nodes = &self.nodes;
            self.roots
                .sort_by(|left, right| nodes[left].compare(&nodes[right]));
            let directories = self
                .nodes
                .iter()
                .filter_map(|(id, node)| match &node.kind {
                    TreeNodeKind::Directory { children, .. } => {
                        Some((id.clone(), children.clone()))
                    }
                    TreeNodeKind::File(_) => None,
                })
                .collect::<Vec<_>>();
            for (id, mut children) in directories {
                children.sort_by(|left, right| self.nodes[left].compare(&self.nodes[right]));
                let TreeNodeKind::Directory {
                    children: target, ..
                } = &mut self.nodes.get_mut(&id).unwrap().kind
                else {
                    unreachable!()
                };
                *target = children;
            }
        }
    }

    fn directory(&mut self, path: &str) -> String {
        let id = format!("directory:{path}");
        if self.nodes.contains_key(&id) {
            return id;
        }
        let parent = path.rsplit_once('/').map(|(path, _)| self.directory(path));
        self.nodes.insert(
            id.clone(),
            TreeNode {
                kind: TreeNodeKind::Directory {
                    path: path.into(),
                    children: Vec::new(),
                },
                parent: parent.clone(),
                row: DisplayRow {
                    id: id.clone(),
                    text: path.rsplit('/').next().unwrap().into(),
                    style: "directory".into(),
                    depth: path.matches('/').count(),
                    ..DisplayRow::default()
                },
            },
        );
        self.append(parent.as_deref(), id.clone());
        id
    }

    fn append(&mut self, parent: Option<&str>, id: String) {
        if let Some(parent) = parent {
            let TreeNodeKind::Directory { children, .. } =
                &mut self.nodes.get_mut(parent).unwrap().kind
            else {
                unreachable!()
            };
            children.push(id);
        } else {
            self.roots.push(id);
        }
    }

    fn file_targets(&self, id: &str, side: Option<DiffSide>) -> Vec<DiffTarget> {
        fn visit(tree: &GitTree, id: &str, side: Option<DiffSide>, targets: &mut Vec<DiffTarget>) {
            match &tree.nodes[id].kind {
                TreeNodeKind::Directory { children, .. } => {
                    for child in children {
                        visit(tree, child, side, targets);
                    }
                }
                TreeNodeKind::File(file) => {
                    if let (TreeFile::Changed(entry), Some(side)) = (file, side) {
                        let staged = !entry.untracked && entry.index != " ";
                        let visible = match side {
                            DiffSide::Staged => staged,
                            DiffSide::Unstaged => !staged || entry.worktree != " ",
                        };
                        if !visible {
                            return;
                        }
                    }
                    targets.push(DiffTarget {
                        path: file.path().into(),
                        original_path: file.original_path().map(str::to_owned),
                    });
                }
            }
        }
        let mut targets = Vec::new();
        visit(self, id, side, &mut targets);
        targets
    }

    fn rows(&self) -> Vec<DisplayRow> {
        fn visit(tree: &GitTree, ids: &[String], rows: &mut Vec<DisplayRow>) {
            for id in ids {
                let node = &tree.nodes[id];
                let mut row = node.row.clone();
                match &node.kind {
                    TreeNodeKind::Directory { path, children } => {
                        let expanded = !tree.collapsed.contains(path) || !tree.filter.is_empty();
                        row.text = format!("{} {}", if expanded { "▾" } else { "▸" }, row.text);
                        rows.push(row);
                        if expanded {
                            visit(tree, children, rows);
                        }
                    }
                    TreeNodeKind::File(_) => rows.push(row),
                }
            }
        }
        let mut rows = Vec::new();
        visit(self, &self.roots, &mut rows);
        rows
    }

    fn navigate(
        &mut self,
        event: Event,
        primary: Option<&str>,
    ) -> Option<zess_plugin::ItemSelection> {
        let node = self.nodes.get(primary?)?;
        let target = match (&node.kind, event) {
            (TreeNodeKind::Directory { path, .. }, Event::Left)
                if !self.collapsed.contains(path) =>
            {
                self.collapsed.insert(path.clone());
                self.rows_revision += 1;
                return None;
            }
            (_, Event::Left) => node.parent.clone(),
            (TreeNodeKind::Directory { path, children }, Event::Right) => {
                if self.collapsed.remove(path) {
                    self.rows_revision += 1;
                    return None;
                }
                children.first().cloned()
            }
            _ => None,
        };
        target.map(|id| browser::single(Some(id)))
    }
}

#[derive(Default)]
struct HistoryPreview {
    state: QueryState<String>,
    navigation: super::schedule::NavigationDebounce,
    commit: Option<String>,
    info: Vec<DisplayRow>,
    summary: Vec<DisplayRow>,
    totals: ChangeTotals,
    source_loading: bool,
}

impl HistoryPreview {
    fn update(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        commit: Option<&Commit>,
        output: &mut PluginOutput,
    ) -> Result<()> {
        self.advance(services, event, input, commit, output, Instant::now())
    }

    fn advance(
        &mut self,
        services: &Services,
        event: Event,
        input: &PluginInput,
        commit: Option<&Commit>,
        output: &mut PluginOutput,
        now: Instant,
    ) -> Result<()> {
        if event == Event::ViewLeaving {
            self.state = QueryState::default();
            self.navigation = super::schedule::NavigationDebounce::default();
            return Ok(());
        }
        let ready_at = self.navigation.update(event, input, now)?;
        let selected = commit.map(|commit| commit.oid.as_str());
        let loading = output
            .display
            .as_ref()
            .is_some_and(|display| display.loading);
        let mut changed = self.state.query().map(String::as_str) != selected;
        if changed || event == Event::Refresh {
            self.totals.start_loading();
            self.state = if let Some(commit) = selected {
                QueryState::Waiting {
                    query: commit.to_owned(),
                    ready_at: if matches!(
                        event,
                        Event::Preview | Event::ViewEntered | Event::Refresh
                    ) {
                        now
                    } else if let Some(ready_at) = ready_at {
                        ready_at
                    } else {
                        now.checked_add(input.preview_debounce)
                            .context("preview debounce exceeds the supported time range")?
                    },
                }
            } else {
                QueryState::default()
            };
        }
        if selected.is_none() && !loading {
            if self.commit.take().is_some() {
                self.info.clear();
                self.summary.clear();
                changed = true;
            }
            self.totals = ChangeTotals::from_files(std::iter::empty());
        }
        if let Some(ready_at) = ready_at
            && ready_at > now
        {
            self.state.defer(ready_at);
        }
        if let QueryState::Waiting {
            ready_at: deadline, ..
        } = &mut self.state
        {
            if matches!(event, Event::Preview | Event::ViewEntered | Event::Refresh) {
                *deadline = now;
            } else if let Some(ready_at) = ready_at {
                *deadline = ready_at;
            }
        }
        changed |= self.source_loading != loading;
        self.source_loading = loading;
        if let QueryState::Waiting { query, ready_at } = &self.state
            && *ready_at <= now
        {
            self.state = QueryState::Running {
                query: query.clone(),
                task: Task::start(
                    services,
                    QueryRequest::CommitFiles {
                        commit: query.clone(),
                    },
                )?,
            };
            changed = true;
        }
        if let QueryState::Running { query, task } = &mut self.state {
            let completed = match task.poll() {
                Ok(Some(QueryResult::CommitFiles { files })) => {
                    self.totals = ChangeTotals::from_files(
                        files
                            .iter()
                            .map(|file| (file.path.as_str(), file.line_stats())),
                    );
                    let ChangeTotals::Ready(counts) = &self.totals else {
                        unreachable!()
                    };
                    let label = if files.len() == 1 { "file" } else { "files" };
                    let mut cells = vec![zess_ui::DisplayCell { spans: Vec::new() }];
                    cells.extend(line_stats_cells(LineStats {
                        additions: counts.additions,
                        deletions: counts.deletions,
                    }));
                    let mut summary = DisplayRow {
                        text: format!("{} {label}", files.len()),
                        cells,
                        ..DisplayRow::default()
                    };
                    if counts.unknown_files > 0 {
                        let label = if counts.unknown_files == 1 {
                            "file"
                        } else {
                            "files"
                        };
                        summary
                            .text
                            .push_str(&format!(", {} binary {label}", counts.unknown_files));
                    }
                    self.summary = vec![summary, DisplayRow::default()];
                    for file in files {
                        let file = TreeFile::Committed(file);
                        let path = match file.original_path() {
                            Some(original) => format!("{original} → {}", file.path()),
                            None => file.path().to_owned(),
                        };
                        let mut cells = vec![file.status_cell()];
                        cells.extend(match file.line_stats() {
                            Some(stats) => line_stats_cells(stats),
                            None => vec![zess_ui::DisplayCell {
                                spans: vec![zess_ui::Span {
                                    base_style: None,
                                    text: "binary".into(),
                                    style: "comment".into(),
                                }],
                            }],
                        });
                        self.summary.push(DisplayRow {
                            text: path,
                            cells,
                            style: file.style().into(),
                            ..DisplayRow::default()
                        });
                    }
                    true
                }
                Ok(Some(_)) => bail!("commit summary query returned another kind of result"),
                Ok(None) => false,
                Err(error) => {
                    self.summary = vec![DisplayRow {
                        text: format!("{error:#}"),
                        style: "error".into(),
                        ..DisplayRow::default()
                    }];
                    self.totals = ChangeTotals::Failed;
                    true
                }
            };
            if completed {
                self.commit = Some(query.clone());
                self.info = commit
                    .map(|commit| {
                        let mut rows = vec![
                            DisplayRow {
                                text: commit.oid.clone(),
                                style: "constant".into(),
                                ..DisplayRow::default()
                            },
                            DisplayRow {
                                text: format!("Author: {}", commit.author),
                                ..DisplayRow::default()
                            },
                            DisplayRow {
                                text: format!("Date:   {}", commit.date),
                                ..DisplayRow::default()
                            },
                            DisplayRow::default(),
                        ];
                        rows.extend(commit.message.lines().map(|line| DisplayRow {
                            text: line.to_owned(),
                            ..DisplayRow::default()
                        }));
                        rows
                    })
                    .unwrap_or_default();
                self.state = QueryState::Idle {
                    query: Some(query.clone()),
                };
                changed = true;
            }
        }
        if changed
            || matches!(
                event,
                Event::Init | Event::ViewEntered | Event::ViewCloned | Event::Preview
            )
        {
            let path = self.commit.clone().unwrap_or_default();
            output.requests.push(HostRequest::PreviewGroup {
                panes: vec![
                    PreviewPane::View {
                        id: "commit-info".into(),
                        path: path.clone(),
                        header: "Commit".into(),
                        key_scope: Some("git-history-preview".into()),
                        loading,
                        content: PreviewContent::Rows {
                            rows: self.info.clone(),
                            column_alignment: ColumnAlignment::None,
                        },
                    },
                    PreviewPane::View {
                        id: "commit-summary".into(),
                        path,
                        header: "Files changed".into(),
                        key_scope: Some("git-history-preview".into()),
                        loading: !matches!(self.state, QueryState::Idle { .. }) || loading,
                        content: PreviewContent::Rows {
                            rows: self.summary.clone(),
                            column_alignment: column_alignment(&input.options)?,
                        },
                    },
                ],
                layout: PreviewLayout::Rows,
                focus: event == Event::Preview,
            });
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
        Ok(())
    }
}

struct PendingSnapshot {
    commit: String,
    path: String,
    presentation: SnapshotPresentation,
    task: Task,
}

#[derive(Default)]
enum ChangeTotals {
    #[default]
    Loading,
    Refreshing(Counts),
    Ready(Counts),
    Failed,
}

struct Counts {
    additions: u64,
    deletions: u64,
    unknown_files: usize,
}

impl ChangeTotals {
    fn start_loading(&mut self) {
        *self = match std::mem::take(self) {
            Self::Ready(counts) | Self::Refreshing(counts) => Self::Refreshing(counts),
            _ => Self::Loading,
        };
    }

    fn from_files<'a>(files: impl Iterator<Item = (&'a str, Option<LineStats>)>) -> Self {
        let mut paths = BTreeSet::new();
        let mut additions = 0;
        let mut deletions = 0;
        let mut unknown_files = 0;
        for (path, stats) in files {
            if !paths.insert(path) {
                continue;
            }
            if let Some(stats) = stats {
                additions += stats.additions;
                deletions += stats.deletions;
            } else {
                unknown_files += 1;
            }
        }
        Self::Ready(Counts {
            additions,
            deletions,
            unknown_files,
        })
    }

    fn status(&self) -> serde_json::Value {
        match self {
            Self::Loading => json!({"git": {"state": "loading"}}),
            Self::Ready(Counts {
                additions,
                deletions,
                unknown_files,
            })
            | Self::Refreshing(Counts {
                additions,
                deletions,
                unknown_files,
            }) => json!({"git": {
                "state": if matches!(self, Self::Ready(_)) { "ready" } else { "refreshing" },
                "additions": additions,
                "deletions": deletions,
                "unknown_files": unknown_files,
            }}),
            Self::Failed => json!({"git": {"state": "failed"}}),
        }
    }
}

#[derive(Default)]
enum LoadResult {
    #[default]
    Unloaded,
    Partial,
    Ready,
    Failed(String),
}

#[derive(Default)]
struct Model {
    entries: Vec<GitEntry>,
    commits: Vec<Commit>,
    files: Vec<CommitFile>,
    commit: String,
    result: LoadResult,
    revision: u64,
    totals: ChangeTotals,
}

#[derive(Default)]
struct GitView {
    filter: Filter,
    tree: GitTree,
    task: Option<Task>,
    started_at_revision: u64,
    browser: Browser,
    diffs: [Browser; 2],
    history: HistoryPreview,
    snapshot: Option<PendingSnapshot>,
    yanks: Yanks,
}

pub(super) struct Git {
    services: Services,
    page: Page,
    models: BTreeMap<BufferId, Model>,
    views: BTreeMap<(BufferId, ViewId), GitView>,
}

impl Git {
    pub(super) fn new(services: Services, page: Page) -> Self {
        Self {
            services,
            page,
            models: BTreeMap::new(),
            views: BTreeMap::new(),
        }
    }
}

impl BufferPlugin for Git {
    fn update(&mut self, input: &PluginInput) -> Result<PluginOutput> {
        let event = Event::parse(&input.event);
        let mut tree_mode = if self.page == Page::History {
            GitTreeMode::Tree
        } else {
            GitTreeMode::parse(&input.options)?
        };
        let column_alignment = if self.page == Page::History {
            ColumnAlignment::All
        } else {
            column_alignment(&input.options)?
        };
        let key = (input.buffer_id, input.view_id);
        if event == Event::ViewCloned
            && let Some(source) = input.argument["source_view"].as_u64()
            && let Some(old) = self.views.get(&(input.buffer_id, source))
        {
            self.views.insert(
                key,
                GitView {
                    filter: old.filter.clone(),
                    tree: GitTree {
                        collapsed: old.tree.collapsed.clone(),
                        ..GitTree::default()
                    },
                    browser: Browser::cloned(),
                    diffs: [Browser::cloned(), Browser::cloned()],
                    ..GitView::default()
                },
            );
        }
        let model = self.models.entry(input.buffer_id).or_default();
        let view = self.views.entry(key).or_default();
        let mut output = PluginOutput::default();
        if event == Event::ToggleView && self.page != Page::History {
            tree_mode = match tree_mode {
                GitTreeMode::Tree => GitTreeMode::Flat,
                GitTreeMode::Flat => GitTreeMode::Tree,
            };
            let mode = match tree_mode {
                GitTreeMode::Tree => "tree",
                GitTreeMode::Flat => "flat",
            };
            output.requests.push(HostRequest::ConfigureView {
                options: json!({"git-view": mode}),
            });
        }
        let request = match self.page {
            Page::Changes => QueryRequest::Status {},
            Page::History => QueryRequest::Log {},
            Page::CommitFiles => {
                if let Some(commit) = input.argument["commit"].as_str() {
                    model.commit = commit.into();
                }
                QueryRequest::CommitFiles {
                    commit: model.commit.clone(),
                }
            }
        };
        if event == Event::FilterChanged {
            view.filter = Filter::compile(
                input.query.as_ref().context("Filter query is required")?,
                Case::Sensitive,
            )?;
        }
        if event == Event::ViewLeaving {
            view.task = None;
            view.browser.leave();
            view.snapshot = None;
            for browser in &mut view.diffs {
                browser.leave();
            }
        } else {
            if matches!(
                event,
                Event::Refresh | Event::Init | Event::ViewEntered | Event::ViewCloned
            ) || (view.task.is_none()
                && browser::changed(event, input, request.operation(), None))
                || (matches!(model.result, LoadResult::Unloaded) && view.task.is_none())
            {
                match Task::start(&self.services, request.clone()) {
                    Ok(task) => {
                        view.task = Some(task);
                        view.started_at_revision = model.revision;
                        model.totals.start_loading();
                    }
                    Err(error) => {
                        view.task = None;
                        model.result =
                            LoadResult::Failed(format!("{}: {error:#}", request.operation()));
                        model.totals = ChangeTotals::Failed;
                        model.revision += 1;
                        output.changed = true;
                    }
                }
            }
            if let Some(task) = &mut view.task {
                let result = task.poll();
                match result {
                    Ok(Some(result)) => {
                        match (self.page, result) {
                            (Page::Changes, QueryResult::Status { entries }) => {
                                model.entries = entries
                            }
                            (Page::History, QueryResult::Log { commits }) => {
                                model.commits = commits
                            }
                            (Page::CommitFiles, QueryResult::CommitFiles { files }) => {
                                model.files = files
                            }
                            _ => bail!("Git query returned another kind of result"),
                        }
                        model.totals = match self.page {
                            Page::Changes => ChangeTotals::from_files(
                                model
                                    .entries
                                    .iter()
                                    .map(|entry| (entry.path.as_str(), entry.line_stats)),
                            ),
                            Page::CommitFiles => ChangeTotals::from_files(
                                model
                                    .files
                                    .iter()
                                    .map(|file| (file.path.as_str(), file.line_stats())),
                            ),
                            Page::History => ChangeTotals::default(),
                        };
                        view.task = None;
                        model.result = LoadResult::Ready;
                        model.revision += 1;
                        output.changed = true;
                    }
                    Ok(None) => {
                        if self.page == Page::Changes
                            && let Some(entries) = task.status_progress()?
                            && (!matches!(model.result, LoadResult::Ready)
                                || model.revision == view.started_at_revision)
                        {
                            model.entries = entries;
                            model.result = LoadResult::Partial;
                            model.revision += 1;
                            output.changed = true;
                        }
                    }
                    Err(error) => {
                        view.task = None;
                        model.result =
                            LoadResult::Failed(format!("{}: {error:#}", request.operation()));
                        model.totals = ChangeTotals::Failed;
                        model.revision += 1;
                        output.changed = true;
                    }
                }
            }
        }
        let rows = if self.page == Page::History {
            model
                .commits
                .iter()
                .map(|commit| DisplayRow {
                    id: commit.oid.clone(),
                    text: format!(
                        "{} {}",
                        &commit.oid[..8.min(commit.oid.len())],
                        commit.message.lines().next().unwrap_or("")
                    ),
                    ..DisplayRow::default()
                })
                .filter(|row| view.filter.is_match(&row.text))
                .collect()
        } else {
            let mode = tree_mode;
            if view.tree.revision != Some(model.revision)
                || view.tree.mode != mode
                || view.tree.filter != view.filter.source()
            {
                let files = match self.page {
                    Page::Changes => model
                        .entries
                        .iter()
                        .cloned()
                        .map(TreeFile::Changed)
                        .collect(),
                    Page::CommitFiles => model
                        .files
                        .iter()
                        .cloned()
                        .map(TreeFile::Committed)
                        .collect(),
                    Page::History => unreachable!(),
                };
                view.tree.rebuild(
                    files,
                    mode,
                    model.revision,
                    &view.filter,
                    match model.result {
                        LoadResult::Ready => LineCountState::Ready,
                        LoadResult::Failed(_) => LineCountState::Failed,
                        LoadResult::Partial | LoadResult::Unloaded => LineCountState::Loading,
                    },
                );
            }
            output.selection = view
                .tree
                .navigate(event, input.selection.primary.as_deref());
            view.tree.rows()
        };
        let header = match self.page {
            Page::Changes | Page::History => String::new(),
            Page::CommitFiles => format!("Commit › {}", &model.commit[..8.min(model.commit.len())]),
        };
        let header = if view.filter.is_empty() || input.mode == "filter" {
            header
        } else {
            format!("{header}  /{}", view.filter.source())
        };
        let header = if let LoadResult::Failed(error) = &model.result {
            if header.is_empty() {
                error.clone()
            } else {
                format!("{header}  {error}")
            }
        } else {
            header
        };
        let mut status = model.totals.status();
        if let LoadResult::Failed(error) = &model.result {
            status["error"] = error.clone().into();
        }
        output.display = Some(Display {
            title: "Git".into(),
            tabs: [
                ("Changes", "changes", self.page == Page::Changes),
                ("History", "history", self.page != Page::Changes),
            ]
            .into_iter()
            .map(|(label, page, active)| zess_ui::DisplayTab {
                label: label.into(),
                command: format!(
                    "{}.page-{page}",
                    match self.page {
                        Page::Changes => "git",
                        Page::History => "git-history",
                        Page::CommitFiles => "git-commit-files",
                    }
                ),
                active,
            })
            .collect(),
            header,
            rows,
            rows_revision: (self.page != Page::History).then_some(view.tree.rows_revision),
            column_alignment,
            status,
            loading: view.task.is_some() && !matches!(model.result, LoadResult::Partial),
            ..Display::default()
        });
        output.watches = Some(if event == Event::ViewLeaving {
            Vec::new()
        } else {
            vec![QueryWatch {
                operation: request.operation().into(),
                argument: request.argument(),
            }]
        });
        browser::reconcile(event, input, &mut output);
        if self.page != Page::History {
            let rows = &output.display.as_ref().unwrap().rows;
            let missing = input
                .selection
                .primary
                .as_ref()
                .is_none_or(|id| !rows.iter().any(|row| &row.id == id));
            if event == Event::FilterChanged || missing {
                if let Some(file) = rows
                    .iter()
                    .find(|row| matches!(view.tree.nodes[&row.id].kind, TreeNodeKind::File(_)))
                {
                    output.selection = Some(browser::single(Some(file.id.clone())));
                }
            }
        }
        let primary_id = output
            .selection
            .as_ref()
            .and_then(|selection| selection.primary.clone());
        if matches!(
            event,
            Event::YankPath
                | Event::YankAbsolutePath
                | Event::YankName
                | Event::YankContent
                | Event::YankDiff
        ) {
            let targets = input
                .selection
                .selected
                .iter()
                .map(|id| {
                    let node = view
                        .tree
                        .nodes
                        .get(id)
                        .with_context(|| format!("Selected path is unavailable: {id}"))?;
                    let (path, kind) = match &node.kind {
                        TreeNodeKind::Directory { path, .. } => {
                            (path.as_str(), TargetKind::Directory)
                        }
                        TreeNodeKind::File(file) => (
                            file.path(),
                            TargetKind::File {
                                source: match file {
                                    TreeFile::Changed(_) => Source::Worktree,
                                    TreeFile::Committed(_) => Source::Commit(model.commit.clone()),
                                },
                                original_path: file.original_path().map(str::to_owned),
                            },
                        ),
                    };
                    Ok(Target {
                        path: path.into(),
                        name: std::path::Path::new(path)
                            .file_name()
                            .context("Selected path has no file name")?
                            .to_string_lossy()
                            .into_owned(),
                        kind,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            view.yanks.queue(&self.services, event, input, targets)?;
        }
        let primary = primary_id.as_deref();
        let selected = primary
            .and_then(|id| view.tree.nodes.get(id))
            .map(|node| &node.kind);
        let preview_event = if event == Event::Open && selected.is_some() {
            Event::Preview
        } else {
            event
        };
        match (self.page, selected) {
            (Page::History, _) => {
                let commit = model
                    .commits
                    .iter()
                    .find(|commit| Some(commit.oid.as_str()) == primary);
                view.history
                    .update(&self.services, event, input, commit, &mut output)?;
                if !matches!(model.result, LoadResult::Failed(_)) {
                    output.display.as_mut().unwrap().status = view.history.totals.status();
                }
                if matches!(event, Event::Open | Event::Edit)
                    && let Some(oid) = primary
                {
                    output.requests.push(HostRequest::Navigate {
                        navigation: Navigation::Push {
                            kind: "git-commit-files".into(),
                            argument: json!({"commit":oid}),
                        },
                    });
                }
            }
            (Page::Changes, Some(node)) => {
                view.browser
                    .idle(&self.services, event, input, &mut output)?;
                let path = match node {
                    TreeNodeKind::Directory { path, .. } => path.as_str(),
                    TreeNodeKind::File(file) => file.path(),
                };
                let sides = [
                    (DiffSide::Unstaged, "unstaged", "Unstaged"),
                    (DiffSide::Staged, "staged", "Staged"),
                ]
                .map(|(side, id, header)| {
                    (
                        side,
                        id,
                        header,
                        view.tree.file_targets(primary.unwrap(), Some(side)),
                    )
                });
                let layout = PreviewLayout::Responsive { min_pane_width: 60 };
                let count = sides
                    .iter()
                    .filter(|(_, _, _, files)| !files.is_empty())
                    .count();
                let mut areas = input
                    .preview_area
                    .map(|area| layout.rectangles(area, count))
                    .unwrap_or_default()
                    .into_iter();
                let mut panes = Vec::new();
                for (browser, (side, id, header, files)) in view.diffs.iter_mut().zip(sides) {
                    let visible = !files.is_empty();
                    let query = visible.then(|| PreviewQuery {
                        request: QueryRequest::DiffFiles {
                            source: DiffSource::Changes { side },
                            files,
                            render: DiffRenderOptions {
                                width: areas
                                    .next()
                                    .map(|area| area.width)
                                    .filter(|width| *width > 0),
                            },
                        },
                        path: path.into(),
                    });
                    if let Some(pane) = browser.pane(
                        &self.services,
                        preview_event,
                        input,
                        &mut output,
                        query,
                        PreviewPane::View {
                            id: id.into(),
                            path: String::new(),
                            header: header.into(),
                            key_scope: Some("git-diff".into()),
                            loading: false,
                            content: PreviewContent::Diff {
                                text: String::new(),
                            },
                        },
                    )? && visible
                    {
                        panes.push(pane);
                    }
                }
                if !panes.is_empty() && panes.iter().all(|pane| !pane.loading()) {
                    let mut focus = false;
                    for browser in &mut view.diffs {
                        focus |= browser.take_focus_request();
                    }
                    output.requests.push(HostRequest::PreviewGroup {
                        panes,
                        layout,
                        focus,
                    });
                } else {
                    output.requests.push(HostRequest::PreviewPending);
                }
                if matches!(event, Event::Edit | Event::EditLayer)
                    && matches!(node, TreeNodeKind::File(_))
                {
                    output.requests.push(HostRequest::OpenWorktreeSource {
                        path: path.into(),
                        line: 0,
                        presentation: if event == Event::EditLayer {
                            SnapshotPresentation::Fullscreen
                        } else {
                            SnapshotPresentation::Editor
                        },
                    });
                }
            }
            (Page::CommitFiles, Some(node)) => {
                let path = match node {
                    TreeNodeKind::Directory { path, .. } => path.as_str(),
                    TreeNodeKind::File(file) => file.path(),
                };
                let query = Some(PreviewQuery {
                    request: QueryRequest::DiffFiles {
                        source: DiffSource::Commit {
                            commit: model.commit.clone(),
                        },
                        files: view.tree.file_targets(primary.unwrap(), None),
                        render: DiffRenderOptions {
                            width: input
                                .preview_area
                                .map(|area| area.width)
                                .filter(|width| *width > 0),
                        },
                    },
                    path: path.into(),
                });
                view.browser
                    .update(&self.services, preview_event, input, &mut output, query)?;
                if view.browser.pending() {
                    output.requests.push(HostRequest::PreviewPending);
                }
            }
            (Page::Changes | Page::CommitFiles, None) => {
                for browser in &mut view.diffs {
                    browser.idle(&self.services, event, input, &mut output)?;
                }
                if matches!(model.result, LoadResult::Failed(_)) {
                    view.browser
                        .idle(&self.services, event, input, &mut output)?;
                } else {
                    view.browser.show_content(
                        &self.services,
                        event,
                        input,
                        &mut output,
                        "",
                        || PreviewContent::Directory {
                            rows: Vec::new(),
                            column_alignment: Default::default(),
                        },
                    )?;
                }
            }
        }
        if self.page == Page::CommitFiles {
            if event == Event::Back {
                output.requests.push(HostRequest::Navigate {
                    navigation: Navigation::Back,
                });
            }
            let file = match selected {
                Some(TreeNodeKind::File(TreeFile::Committed(file))) => Some(file),
                _ => None,
            };
            let path = file.map(|file| file.path.as_str());
            let stale = view.snapshot.as_ref().is_some_and(|pending| {
                Some(pending.path.as_str()) != path || pending.commit != model.commit
            });
            if stale || event == Event::ViewLeaving {
                view.snapshot = None;
            }
            if matches!(event, Event::Edit | Event::EditLayer)
                && let Some(path) = path
            {
                let task = Task::start(
                    &self.services,
                    QueryRequest::Blob {
                        commit: model.commit.clone(),
                        path: path.into(),
                    },
                )?;
                view.snapshot = Some(PendingSnapshot {
                    commit: model.commit.clone(),
                    path: path.into(),
                    task,
                    presentation: if event == Event::EditLayer {
                        SnapshotPresentation::Fullscreen
                    } else {
                        SnapshotPresentation::Editor
                    },
                });
            }
            if let Some(pending) = &mut view.snapshot {
                match pending.task.poll() {
                    Ok(Some(QueryResult::Snapshot { snapshot })) => {
                        output.requests.push(HostRequest::OpenSnapshot {
                            snapshot,
                            presentation: pending.presentation,
                        });
                        view.snapshot = None;
                    }
                    Ok(Some(QueryResult::Unsupported { message })) => {
                        output.requests.push(HostRequest::Message { text: message });
                        view.snapshot = None;
                    }
                    Ok(Some(_)) => {
                        bail!("historical file query returned another kind of result")
                    }
                    Ok(None) => {}
                    Err(error) => {
                        output.requests.push(HostRequest::Message {
                            text: format!("{error:#}"),
                        });
                        view.snapshot = None;
                    }
                }
            }
            if view.snapshot.is_some() {
                output.display.as_mut().unwrap().loading = true;
                output.requests.push(HostRequest::Schedule { after_ms: 50 });
            }
        }
        let target_page = match event {
            Event::PageChanges => Some(Page::Changes),
            Event::PageHistory => Some(Page::History),
            Event::Page => Some(if self.page == Page::Changes {
                Page::History
            } else {
                Page::Changes
            }),
            _ => None,
        };
        if let Some(target) = target_page {
            let current = if self.page == Page::Changes {
                Page::Changes
            } else {
                Page::History
            };
            if current != target {
                let (current, page, kind) = match target {
                    Page::Changes => ("history", "changes", "git"),
                    Page::History => ("changes", "history", "git-history"),
                    Page::CommitFiles => unreachable!(),
                };
                output.requests.push(HostRequest::Navigate {
                    navigation: Navigation::Page {
                        group: "git".into(),
                        current: current.into(),
                        page: page.into(),
                        kind: kind.into(),
                        argument: json!({}),
                    },
                });
            }
        }
        if self.page == Page::CommitFiles {
            output.requests.push(HostRequest::ConfigurePreview {
                header: String::new(),
                key_scope: Some("git-commit-preview".into()),
            });
        }
        view.yanks.advance(&self.services, event, &mut output);
        if view.task.is_some() {
            output.requests.push(HostRequest::Schedule { after_ms: 50 });
        }
        Ok(output)
    }

    fn remove_buffer_view(&mut self, buffer: BufferId, view: ViewId) {
        self.views.remove(&(buffer, view));
    }

    fn remove_view(&mut self, id: ViewId) {
        self.views.retain(|(_, view), _| *view != id);
    }
    fn remove_buffer(&mut self, id: BufferId) {
        self.models.remove(&id);
        self.views.retain(|(buffer, _), _| *buffer != id);
    }
}
