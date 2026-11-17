use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use zess_filter::{Case, Filter};
use zess_git::SelectedPath;
use zess_git::ignore::{IgnoreDestination, IgnoreRequest};
use zess_git::mutation::{FileMutation, FileMutationRequest};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use zess_git::{
    Commit, CommitFile, CommitFileList, DiffRenderOptions, DiffSide, DiffSource, DiffTarget,
    FileStatus, GitEntry, LineStats, PagerCommand, StagingStatus,
};
use zess_plugin::{
    BufferId, HostRequest, Navigation, OperationCompletion, OperationResult, PageNavigationSource,
    PluginInput, PluginOutput, PreviewContent, PreviewLayout, PreviewPane, QueryWatch,
    SnapshotPresentation, ViewId,
};
use zess_services::{QueryRequest, QueryResult};
use zess_ui::{ColumnAlignment, Display, DisplayRow};

use super::{
    Event, Services, ViewOptions,
    browser::{self, Browser, PreviewQuery, Task},
    file_actions::{Source, Target, TargetKind, Yanks},
    schedule::QueryState,
};
use zess_plugin::BufferPlugin;

#[cfg(test)]
mod history_preview_tests;
#[cfg(test)]
mod mutation_tests;
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

impl Page {
    pub(super) fn kind(self) -> &'static str {
        match self {
            Self::Changes => "git",
            Self::History => "git-history",
            Self::CommitFiles => "git-commit-files",
        }
    }

    fn tab(self) -> Self {
        match self {
            Self::Changes => Self::Changes,
            Self::History | Self::CommitFiles => Self::History,
        }
    }

    fn tab_name(self) -> &'static str {
        match self.tab() {
            Self::Changes => "changes",
            _ => "history",
        }
    }

    pub(super) fn tab_command(self, target: Self) -> String {
        format!("{}.page-{}", self.kind(), target.tab_name())
    }

    pub(super) fn other_tab(self) -> Self {
        if self == Self::Changes {
            Self::History
        } else {
            Self::Changes
        }
    }

    pub(super) fn tab_request(self, target: Self) -> HostRequest {
        HostRequest::Navigate {
            navigation: Navigation::Page {
                group: "git".into(),
                current: self.tab_name().into(),
                page: target.tab_name().into(),
                kind: target.kind().into(),
                argument: json!({}),
                source: Some(PageNavigationSource {
                    kind: self.kind().into(),
                    tab_kind: self.tab().kind().into(),
                }),
            },
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
enum GitTreeMode {
    #[default]
    Tree,
    Flat,
}

impl GitTreeMode {
    fn parse(options: &Value) -> Result<Self> {
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

struct GitOptions {
    diff_pager: Option<PagerCommand>,
    diff_file_header: bool,
    tree_mode: GitTreeMode,
    column_alignment: ColumnAlignment,
}

impl GitOptions {
    fn parse(page: Page, options: &Value) -> Result<Self> {
        let diff_pager = options
            .get("diff-pager")
            .map(|value| {
                let pager: PagerCommand = serde_json::from_value(value.clone())
                    .context("diff-pager must be an object with a program string and an args array of strings")?;
                anyhow::ensure!(
                    !pager.program.trim().is_empty(),
                    "diff-pager program must not be empty"
                );
                anyhow::ensure!(
                    !pager.program.contains('\0')
                        && pager.args.iter().all(|arg| !arg.contains('\0')),
                    "diff-pager program and arguments must not contain NUL"
                );
                Ok(pager)
            })
            .transpose()?;
        let diff_file_header = options
            .get("diff-file-header")
            .map(|value| {
                value
                    .as_bool()
                    .context("diff-file-header must be a boolean")
            })
            .transpose()?
            .unwrap_or(false);
        let tree_mode = if page == Page::History {
            GitTreeMode::Tree
        } else {
            GitTreeMode::parse(options)?
        };
        let column_alignment = match options.get("stats-alignment") {
            None => ColumnAlignment::All,
            Some(value) => match value.as_str() {
                Some("none") => ColumnAlignment::None,
                Some("consecutive") => ColumnAlignment::Consecutive,
                Some("all") => ColumnAlignment::All,
                _ => bail!("stats-alignment must be \"none\", \"consecutive\", or \"all\""),
            },
        };
        Ok(Self {
            diff_pager,
            diff_file_header,
            tree_mode,
            column_alignment,
        })
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

const FILE_ID_PREFIX: &str = "file:";
const DIRECTORY_ID_PREFIX: &str = "directory:";

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
            let id = format!("{FILE_ID_PREFIX}{path}");
            let depth = if mode == GitTreeMode::Tree {
                path.matches('/').count()
            } else {
                0
            };
            let mut cells = vec![file.status_cell()];
            cells.extend(
                file.line_stats()
                    .map(line_stats_cells)
                    .unwrap_or_else(|| match counts {
                        LineCountState::Loading => vec![
                            zess_ui::DisplayCell {
                                spans: vec![zess_ui::Span {
                                    base_style: None,
                                    text: "  ".into(),
                                    style: "comment".into(),
                                }],
                            };
                            2
                        ],
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
                    }),
            );
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
        let id = format!("{DIRECTORY_ID_PREFIX}{path}");
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
    column_alignment: ColumnAlignment,
    published_alignment: ColumnAlignment,
    state: QueryState<String>,
    navigation: super::schedule::NavigationDebounce,
    published: Option<HistorySummary>,
    totals: ChangeTotals,
    source_loading: bool,
}

struct HistorySummary {
    commit: String,
    info: Vec<DisplayRow>,
    summary: Vec<DisplayRow>,
    files: Option<CommitFileList>,
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
            if self.published.take().is_some() {
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
            let summary = match task.poll() {
                Ok(Some(QueryResult::CommitFiles(list))) => {
                    self.totals = ChangeTotals::from_commit(&list);
                    let files = &list.files;
                    let label = if files.len() == 1 { "file" } else { "files" };
                    let mut summary = DisplayRow {
                        text: format!("{} {label}", files.len()),
                        ..DisplayRow::default()
                    };
                    if let ChangeTotals::Ready(counts) = &self.totals {
                        summary
                            .cells
                            .push(zess_ui::DisplayCell { spans: Vec::new() });
                        summary.cells.extend(line_stats_cells(LineStats {
                            additions: counts.additions,
                            deletions: counts.deletions,
                        }));
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
                    }
                    let mut summary_rows = vec![summary];
                    if let Some(warning) = &list.path_warning {
                        summary_rows.push(DisplayRow {
                            text: warning.clone(),
                            style: "warning".into(),
                            ..DisplayRow::default()
                        });
                    }
                    if let Some(error) = &list.line_count_error {
                        summary_rows.push(DisplayRow {
                            text: error.clone(),
                            style: "error".into(),
                            ..DisplayRow::default()
                        });
                    }
                    summary_rows.push(DisplayRow::default());
                    for file in files {
                        let file = TreeFile::Committed(file.clone());
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
                                    text: if list.line_count_error.is_some() {
                                        "count failed"
                                    } else {
                                        "binary"
                                    }
                                    .into(),
                                    style: "comment".into(),
                                }],
                            }],
                        });
                        summary_rows.push(DisplayRow {
                            id: file.path().to_owned(),
                            text: path,
                            cells,
                            style: file.style().into(),
                            ..DisplayRow::default()
                        });
                    }
                    Some((summary_rows, Some(list)))
                }
                Ok(Some(_)) => bail!("commit summary query returned another kind of result"),
                Ok(None) => None,
                Err(error) => {
                    let summary = vec![DisplayRow {
                        text: format!("{error:#}"),
                        style: "error".into(),
                        ..DisplayRow::default()
                    }];
                    self.totals = ChangeTotals::Unavailable;
                    Some((summary, None))
                }
            };
            if let Some((summary, files)) = summary {
                let info = commit
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
                self.published = Some(HistorySummary {
                    commit: query.clone(),
                    info,
                    summary,
                    files,
                });
                self.state = QueryState::Idle {
                    query: Some(query.clone()),
                };
                changed = true;
            }
        }
        if changed
            || self.column_alignment != self.published_alignment
            || matches!(
                event,
                Event::Init | Event::ViewEntered | Event::ViewCloned | Event::Preview
            )
        {
            self.published_alignment = self.column_alignment;
            let path = self
                .published
                .as_ref()
                .map(|summary| summary.commit.clone())
                .unwrap_or_default();
            output.requests.push(HostRequest::PreviewGroup {
                panes: vec![
                    PreviewPane::View {
                        id: "commit-info".into(),
                        path: path.clone(),
                        header: "Commit".into(),
                        key_scope: Some("git-history-preview".into()),
                        loading: false,
                        content: PreviewContent::Rows {
                            rows: self
                                .published
                                .as_ref()
                                .map(|summary| summary.info.clone())
                                .unwrap_or_default(),
                            column_alignment: ColumnAlignment::None,
                        },
                    },
                    PreviewPane::View {
                        id: "commit-summary".into(),
                        path,
                        header: "Files changed".into(),
                        key_scope: Some("git-history-preview".into()),
                        loading: !matches!(self.state, QueryState::Idle { .. }),
                        content: PreviewContent::Items {
                            rows: self
                                .published
                                .as_ref()
                                .map(|summary| summary.summary.clone())
                                .unwrap_or_default(),
                            column_alignment: self.column_alignment,
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
            output.requests.push(HostRequest::Schedule { after_ms });
        }
        Ok(())
    }
}

struct PendingSnapshot {
    operation_id: Option<String>,
    commit: String,
    view_primary: Option<String>,
    presentation: SnapshotPresentation,
    task: Task,
}

#[derive(Default)]
enum ChangeTotals {
    #[default]
    Unknown,
    Ready(Counts),
    Failed(String),
    Unavailable,
}

struct Counts {
    additions: u64,
    deletions: u64,
    unknown_files: usize,
}

impl ChangeTotals {
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

    fn from_commit(list: &CommitFileList) -> Self {
        match &list.line_count_error {
            Some(error) => Self::Failed(format!("git.commit-files: {error}")),
            None => Self::from_files(
                list.files
                    .iter()
                    .map(|file| (file.path.as_str(), file.line_stats())),
            ),
        }
    }

    fn status(&self, loading: bool) -> serde_json::Value {
        match self {
            Self::Unknown if loading => json!({"git": {"state": "loading"}}),
            Self::Unknown => Value::Null,
            Self::Ready(Counts {
                additions,
                deletions,
                unknown_files,
            }) => json!({"git": {
                "state": if loading { "refreshing" } else { "ready" },
                "additions": additions,
                "deletions": deletions,
                "unknown_files": unknown_files,
            }}),
            Self::Failed(_) | Self::Unavailable => json!({"git": {"state": "failed"}}),
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

fn complete_operation(
    operation_id: Option<String>,
    result: OperationResult,
    output: &mut PluginOutput,
) {
    if let Some(operation_id) = operation_id {
        output.completed_operations.push(OperationCompletion {
            operation_id,
            result,
        });
    }
}

fn operation_failed(operation_id: Option<String>, message: String, output: &mut PluginOutput) {
    complete_operation(
        operation_id,
        OperationResult::Failed {
            message: message.clone(),
        },
        output,
    );
    output.requests.push(HostRequest::Message { text: message });
}

enum MutationOperation {
    Files,
    Ignore(IgnoreDestination),
}

struct RunningMutation {
    task: zess_services::FileMutationTask,
    operation: MutationOperation,
    operation_id: Option<String>,
}

impl RunningMutation {
    fn poll(&mut self, services: &Services) -> Option<PluginOutput> {
        let result = self.task.poll()?;
        let operation_id = self.operation_id.take();
        services.borrow_mut().invalidate_fs();
        let mut output = PluginOutput {
            changed: true,
            ..PluginOutput::default()
        };
        match result {
            Ok(()) => {
                complete_operation(operation_id, OperationResult::Completed, &mut output);
                if let MutationOperation::Ignore(destination) = &self.operation {
                    let text = match destination {
                        IgnoreDestination::Repository => "Ignore rules added to .gitignore",
                        IgnoreDestination::Local => {
                            "Ignore rules added to repository-local info/exclude"
                        }
                    };
                    output
                        .requests
                        .push(HostRequest::Message { text: text.into() });
                }
            }
            Err(message) => operation_failed(operation_id, message.clone(), &mut output),
        }
        Some(output)
    }
}

enum MutationState {
    Idle {
        epoch: u64,
    },
    Writing {
        epoch: u64,
        mutation: RunningMutation,
    },
    RefreshingStatus {
        epoch: u64,
    },
}

impl Default for MutationState {
    fn default() -> Self {
        Self::Idle { epoch: 0 }
    }
}

impl MutationState {
    fn epoch(&self) -> u64 {
        match self {
            Self::Idle { epoch }
            | Self::Writing { epoch, .. }
            | Self::RefreshingStatus { epoch } => *epoch,
        }
    }

    fn start(
        &mut self,
        task: Result<zess_services::FileMutationTask>,
        operation: MutationOperation,
        operation_id: Option<String>,
        output: &mut PluginOutput,
    ) {
        let Self::Idle { epoch } = self else {
            unreachable!("a mutation starts only from a settled status snapshot");
        };
        match task {
            Ok(task) => {
                *self = Self::Writing {
                    epoch: *epoch,
                    mutation: RunningMutation {
                        task,
                        operation,
                        operation_id,
                    },
                }
            }
            Err(error) => operation_failed(operation_id, format!("{error:#}"), output),
        }
    }

    fn poll(&mut self, services: &Services) -> Option<PluginOutput> {
        let Self::Writing { epoch, mutation } = self else {
            return None;
        };
        let output = mutation.poll(services)?;
        *self = Self::RefreshingStatus { epoch: *epoch + 1 };
        Some(output)
    }

    fn status_settled(&mut self) {
        if let Self::RefreshingStatus { epoch } = self {
            *self = Self::Idle { epoch: *epoch };
        }
    }
}

#[derive(Default)]
struct Model {
    entries: Vec<GitEntry>,
    commits: Vec<Commit>,
    files: Vec<CommitFile>,
    commit: String,
    path_warning: Option<String>,
    result: LoadResult,
    revision: u64,
    load_generation: u64,
    published_generation: u64,
    totals: ChangeTotals,
    mutation: MutationState,
}

#[derive(Default)]
struct GitView {
    options: ViewOptions<GitOptions>,
    filter: Filter,
    tree: GitTree,
    load: Option<GitLoad>,
    browser: Browser,
    diffs: [Browser; 2],
    history: HistoryPreview,
    snapshot: Option<PendingSnapshot>,
    yanks: Yanks,
    mutation: MutationIntent,
    diff_refresh: DiffRefresh,
}

impl GitView {
    fn cancel_operations(&mut self) -> Vec<OperationCompletion> {
        let mut completions = self.yanks.cancel();
        completions.extend(std::mem::take(&mut self.mutation).cancel());
        if let Some(pending) = self.snapshot.take()
            && let Some(operation_id) = pending.operation_id
        {
            completions.push(OperationCompletion {
                operation_id,
                result: OperationResult::Cancelled,
            });
        }
        completions
    }
}

struct GitLoad {
    task: Task,
    epoch: u64,
    generation: u64,
    published_revision: u64,
}

#[derive(Default)]
enum MutationIntent {
    #[default]
    Idle,
    WaitingForStatus {
        pending: PendingMutation,
        operation_id: Option<String>,
    },
    ConfirmingDiscard(DiscardConfirmation),
}

impl MutationIntent {
    fn cancel(self) -> Option<OperationCompletion> {
        let operation_id = match self {
            Self::Idle => None,
            Self::WaitingForStatus { operation_id, .. } => operation_id,
            Self::ConfirmingDiscard(pending) => pending.operation_id,
        }?;
        Some(OperationCompletion {
            operation_id,
            result: OperationResult::Cancelled,
        })
    }
}

enum DiffRefresh {
    Current { epoch: u64 },
    WaitingForStatus { epoch: u64, focus: Option<String> },
    LoadingDiffs { epoch: u64, focus: Option<String> },
}

impl Default for DiffRefresh {
    fn default() -> Self {
        Self::Current { epoch: 0 }
    }
}

impl DiffRefresh {
    fn epoch(&self) -> u64 {
        match self {
            Self::Current { epoch }
            | Self::WaitingForStatus { epoch, .. }
            | Self::LoadingDiffs { epoch, .. } => *epoch,
        }
    }
}

enum PendingMutation {
    Files {
        operation: FileMutation,
        paths: Vec<SelectedPath>,
    },
    Ignore(IgnoreRequest),
}

fn selected_paths(selection: &zess_plugin::ItemSelection) -> Result<Vec<SelectedPath>> {
    let ids = if selection.selected.is_empty() {
        selection.primary.as_slice()
    } else {
        selection.selected.as_slice()
    };
    ids.iter()
        .map(|id| {
            let (path, directory) = if let Some(path) = id.strip_prefix(FILE_ID_PREFIX) {
                (path, false)
            } else if let Some(path) = id.strip_prefix(DIRECTORY_ID_PREFIX) {
                (path, true)
            } else {
                bail!("Selected path is unavailable: {id}");
            };
            Ok(SelectedPath {
                path: path.into(),
                directory,
            })
        })
        .collect()
}

fn entry_matches_path(entry: &GitEntry, path: &SelectedPath) -> bool {
    if path.directory {
        entry
            .path
            .strip_prefix(&path.path)
            .is_some_and(|suffix| suffix.starts_with('/'))
    } else {
        entry.path == path.path
    }
}

struct DiscardConfirmation {
    operation_id: Option<String>,
    purpose: String,
    epoch: u64,
    request: FileMutationRequest,
}

fn cancelled_operation(operation_id: Option<String>) -> Option<OperationCompletion> {
    operation_id.map(|operation_id| OperationCompletion {
        operation_id,
        result: OperationResult::Cancelled,
    })
}

pub(super) struct Git {
    services: Services,
    page: Page,
    models: BTreeMap<BufferId, Model>,
    views: BTreeMap<(BufferId, ViewId), GitView>,
    detached_mutations: Vec<RunningMutation>,
    completed_mutations: Vec<PluginOutput>,
    operation_completions: Vec<OperationCompletion>,
}

impl Git {
    pub(super) fn new(services: Services, page: Page) -> Self {
        Self {
            services,
            page,
            models: BTreeMap::new(),
            views: BTreeMap::new(),
            detached_mutations: Vec::new(),
            completed_mutations: Vec::new(),
            operation_completions: Vec::new(),
        }
    }
}

impl Git {
    pub(super) fn poll_operations(&mut self) -> Vec<PluginOutput> {
        let mut outputs = std::mem::take(&mut self.completed_mutations);
        outputs.extend(
            self.models
                .values_mut()
                .filter_map(|model| model.mutation.poll(&self.services)),
        );
        self.detached_mutations.retain_mut(|mutation| {
            if let Some(output) = mutation.poll(&self.services) {
                outputs.push(output);
                false
            } else {
                true
            }
        });
        outputs.extend(
            self.operation_completions
                .drain(..)
                .map(|completion| PluginOutput {
                    completed_operations: vec![completion],
                    ..PluginOutput::default()
                }),
        );
        for view in self.views.values_mut() {
            outputs.extend(view.yanks.poll(&self.services));
        }
        outputs
    }
}

impl BufferPlugin for Git {
    fn validate_view_options(&self, options: &Value) -> Result<()> {
        GitOptions::parse(self.page, options).map(|_| ())
    }

    fn update(&mut self, input: &PluginInput) -> Result<PluginOutput> {
        let event = Event::parse(&input.event);
        let pending_mutation = if self.page == Page::Changes {
            match event {
                Event::Ignore | Event::Exclude => Some(PendingMutation::Ignore(IgnoreRequest {
                    destination: if event == Event::Ignore {
                        IgnoreDestination::Repository
                    } else {
                        IgnoreDestination::Local
                    },
                    paths: selected_paths(&input.selection)?,
                })),
                Event::Stage | Event::Unstage | Event::Discard => Some(PendingMutation::Files {
                    operation: match event {
                        Event::Stage => FileMutation::Stage,
                        Event::Unstage => FileMutation::Unstage,
                        Event::Discard => FileMutation::Discard,
                        _ => unreachable!(),
                    },
                    paths: selected_paths(&input.selection)?,
                }),
                _ => None,
            }
        } else {
            None
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
        let view = self.views.entry(key).or_default();
        let options = view.options.resolve(&input.options, |options| {
            GitOptions::parse(self.page, options)
        })?;
        let mut tree_mode = options.tree_mode;
        let model = self.models.entry(input.buffer_id).or_default();
        let mut output = PluginOutput::default();
        if let Some(completion) = model.mutation.poll(&self.services) {
            // View requests can fail independently of the completed write.
            self.completed_mutations.push(completion);
            output.changed = true;
        }
        let epoch = model.mutation.epoch();
        if view.diff_refresh.epoch() != epoch {
            view.diff_refresh = DiffRefresh::WaitingForStatus { epoch, focus: None };
        }
        let confirmation = input.argument["confirmation"].as_str();
        if event == Event::LayerResult
            && self.page == Page::Changes
            && (confirmation.is_some()
                || matches!(view.mutation, MutationIntent::ConfirmingDiscard(_)))
        {
            anyhow::ensure!(
                matches!(&view.mutation, MutationIntent::ConfirmingDiscard(pending)
                    if input.argument["purpose"] == pending.purpose
                        && pending.operation_id.as_deref() == confirmation),
                "Git discard confirmation expired; run discard again"
            );
            let MutationIntent::ConfirmingDiscard(pending) = std::mem::take(&mut view.mutation)
            else {
                unreachable!();
            };
            if input.argument["cancelled"] != true
                && input.argument["text"]
                    .as_str()
                    .is_some_and(|text| text.trim() == "yes")
            {
                if matches!(model.mutation, MutationState::Idle { .. }) && pending.epoch == epoch {
                    model.mutation.start(
                        self.services.borrow().start_file_mutation(pending.request),
                        MutationOperation::Files,
                        pending.operation_id,
                        &mut output,
                    );
                } else {
                    operation_failed(
                        pending.operation_id,
                        "Git status changed; retry the file operation".into(),
                        &mut output,
                    );
                }
            } else {
                self.operation_completions
                    .extend(cancelled_operation(pending.operation_id));
            }
        }
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
        let mut received_files = false;
        let request = match self.page {
            Page::Changes => QueryRequest::Status {},
            Page::History => QueryRequest::Log {},
            Page::CommitFiles => {
                if let Some(commit) = input.argument["commit"].as_str() {
                    model.commit = commit.into();
                    if matches!(event, Event::Init | Event::ViewEntered)
                        && input
                            .argument
                            .get("files")
                            .is_some_and(|files| !files.is_null())
                    {
                        let list = serde_json::from_value(input.argument.clone())?;
                        model.totals = ChangeTotals::from_commit(&list);
                        model.files = list.files;
                        model.path_warning = list.path_warning;
                        received_files = true;
                        model.result = LoadResult::Ready;
                        model.revision += 1;
                    }
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
            self.operation_completions.extend(view.cancel_operations());
            view.diff_refresh = match std::mem::take(&mut view.diff_refresh) {
                DiffRefresh::Current { epoch } => DiffRefresh::Current { epoch },
                DiffRefresh::WaitingForStatus { epoch, .. }
                | DiffRefresh::LoadingDiffs { epoch, .. } => {
                    DiffRefresh::WaitingForStatus { epoch, focus: None }
                }
            };
            view.load = None;
            view.browser.leave();
            for browser in &mut view.diffs {
                browser.leave();
            }
        } else {
            if view.load.as_ref().is_some_and(|load| {
                load.epoch != epoch
                    || load.generation < model.published_generation
                    || (load.generation == model.published_generation
                        && load.published_revision != model.revision)
            }) {
                view.load = None;
            }
            if matches!(
                event,
                Event::Refresh | Event::Init | Event::ViewEntered | Event::ViewCloned
            ) || (view.load.is_none()
                && browser::changed(event, input, request.operation(), None))
                || (matches!(model.result, LoadResult::Unloaded) && view.load.is_none())
                || (matches!(model.mutation, MutationState::RefreshingStatus { .. })
                    && view.load.is_none())
            {
                model.load_generation += 1;
                match Task::start(&self.services, request.clone()) {
                    Ok(task) => {
                        view.load = Some(GitLoad {
                            task,
                            epoch,
                            generation: model.load_generation,
                            published_revision: model.revision,
                        });
                    }
                    Err(error) => {
                        view.load = None;
                        model.published_generation = model.load_generation;
                        model.result =
                            LoadResult::Failed(format!("{}: {error:#}", request.operation()));
                        model.totals = ChangeTotals::Unavailable;
                        model.mutation.status_settled();
                        model.revision += 1;
                        output.changed = true;
                    }
                }
            }
            if let Some(load) = &mut view.load {
                let result = load.task.poll();
                match result {
                    Ok(Some(result)) => {
                        model.totals = match (self.page, result) {
                            (
                                Page::Changes,
                                QueryResult::Status {
                                    entries,
                                    path_warning,
                                    line_count_error,
                                },
                            ) => {
                                model.entries = entries;
                                model.path_warning = path_warning;
                                match line_count_error {
                                    Some(error) => ChangeTotals::Failed(format!(
                                        "{}: {error}",
                                        request.operation()
                                    )),
                                    None => ChangeTotals::from_files(
                                        model
                                            .entries
                                            .iter()
                                            .map(|entry| (entry.path.as_str(), entry.line_stats)),
                                    ),
                                }
                            }
                            (Page::History, QueryResult::Log { commits }) => {
                                model.commits = commits;
                                ChangeTotals::default()
                            }
                            (Page::CommitFiles, QueryResult::CommitFiles(list)) => {
                                let totals = ChangeTotals::from_commit(&list);
                                model.files = list.files;
                                model.path_warning = list.path_warning;
                                totals
                            }
                            _ => bail!("Git query returned another kind of result"),
                        };
                        model.published_generation = load.generation;
                        view.load = None;
                        model.result = LoadResult::Ready;
                        model.mutation.status_settled();
                        model.revision += 1;
                        output.changed = true;
                    }
                    Ok(None) => {
                        if self.page == Page::Changes
                            && let Some(progress) = load.task.status_progress()?
                        {
                            model.entries = progress.entries;
                            model.path_warning = progress.path_warning;
                            model.published_generation = load.generation;
                            model.result = LoadResult::Partial;
                            model.revision += 1;
                            load.published_revision = model.revision;
                            output.changed = true;
                        }
                    }
                    Err(error) => {
                        model.published_generation = load.generation;
                        view.load = None;
                        model.result =
                            LoadResult::Failed(format!("{}: {error:#}", request.operation()));
                        model.totals = ChangeTotals::Unavailable;
                        model.mutation.status_settled();
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
                    match (&model.result, &model.totals) {
                        (_, ChangeTotals::Failed(_)) | (LoadResult::Failed(_), _) => {
                            LineCountState::Failed
                        }
                        (LoadResult::Ready, _) => LineCountState::Ready,
                        (LoadResult::Partial | LoadResult::Unloaded, _) => LineCountState::Loading,
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
        let mut header = if view.filter.is_empty() || input.mode == "filter" {
            header
        } else {
            format!("{header}  /{}", view.filter.source())
        };
        let error = match (&model.result, &model.totals) {
            (LoadResult::Failed(error), _) | (_, ChangeTotals::Failed(error)) => Some(error),
            _ => None,
        };
        for diagnostic in [error, model.path_warning.as_ref()].into_iter().flatten() {
            if header.is_empty() {
                header = diagnostic.clone();
            } else {
                header.push_str("  ");
                header.push_str(diagnostic);
            }
        }
        let loading = view.load.is_some();
        let mut status = model.totals.status(loading && !received_files);
        if let Some(error) = error {
            status["error"] = error.clone().into();
        }
        output.display = Some(Display {
            title: "Git".into(),
            tabs: [("Changes", Page::Changes), ("History", Page::History)]
                .into_iter()
                .map(|(label, page)| zess_ui::DisplayTab {
                    label: label.into(),
                    command: self.page.tab_command(page),
                    active: self.page.tab() == page,
                })
                .collect(),
            header,
            rows,
            rows_revision: (self.page != Page::History).then_some(view.tree.rows_revision),
            column_alignment: if self.page == Page::History {
                ColumnAlignment::All
            } else {
                options.column_alignment
            },
            status,
            loading,
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
                .view_selection()
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
        if let Some(pending) = pending_mutation {
            match (&model.mutation, &view.mutation) {
                (MutationState::Writing { .. }, _)
                | (
                    _,
                    MutationIntent::WaitingForStatus { .. } | MutationIntent::ConfirmingDiscard(_),
                ) => {
                    let text = match &pending {
                        PendingMutation::Ignore(_) => {
                            "Wait for the current Git file operation to finish"
                        }
                        PendingMutation::Files { .. } => "Git file operation is running",
                    };
                    operation_failed(input.context.operation_id.clone(), text.into(), &mut output);
                }
                _ => {
                    if let Some(operation_id) = &input.context.operation_id {
                        output.pending_operations.push(operation_id.clone());
                    }
                    view.mutation = MutationIntent::WaitingForStatus {
                        pending,
                        operation_id: input.context.operation_id.clone(),
                    };
                }
            }
        }
        if view.load.is_none()
            && matches!(model.mutation, MutationState::Idle { .. })
            && matches!(view.mutation, MutationIntent::WaitingForStatus { .. })
        {
            let MutationIntent::WaitingForStatus {
                pending,
                operation_id,
            } = std::mem::take(&mut view.mutation)
            else {
                unreachable!();
            };
            let paths = match &pending {
                PendingMutation::Files { paths, .. }
                | PendingMutation::Ignore(IgnoreRequest { paths, .. }) => paths,
            };
            let missing = paths
                .iter()
                .find(|path| {
                    !model
                        .entries
                        .iter()
                        .any(|entry| entry_matches_path(entry, path))
                })
                .map(|path| format!("Selected path is unavailable: {}", path.path));
            match (&model.result, missing, pending) {
                (LoadResult::Ready, Some(message), _) => {
                    operation_failed(operation_id, message, &mut output)
                }
                (LoadResult::Ready, None, PendingMutation::Ignore(request)) => {
                    let operation = MutationOperation::Ignore(request.destination);
                    model.mutation.start(
                        self.services.borrow().start_ignore(request),
                        operation,
                        operation_id,
                        &mut output,
                    );
                }
                (LoadResult::Ready, None, PendingMutation::Files { operation, paths }) => {
                    let entries = model
                        .entries
                        .iter()
                        .filter(|entry| paths.iter().any(|path| entry_matches_path(entry, path)))
                        .collect::<Vec<_>>();
                    if entries
                        .iter()
                        .any(|entry| entry.staging_status() == StagingStatus::Conflict)
                    {
                        operation_failed(
                            operation_id,
                            "Cannot change a selection containing unresolved conflicts".into(),
                            &mut output,
                        );
                    } else {
                        let entries = entries
                            .into_iter()
                            .filter(|entry| match operation {
                                FileMutation::Stage | FileMutation::Discard => {
                                    entry.untracked || entry.worktree != " "
                                }
                                FileMutation::Unstage => !entry.untracked && entry.index != " ",
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        if entries.is_empty() {
                            operation_failed(
                                operation_id,
                                "No changes on the selected side".into(),
                                &mut output,
                            );
                        } else {
                            let request = FileMutationRequest { operation, entries };
                            if operation == FileMutation::Discard {
                                let tracked = request
                                    .entries
                                    .iter()
                                    .filter(|entry| !entry.untracked)
                                    .map(|entry| format!("{:?}", entry.path))
                                    .collect::<Vec<_>>();
                                let untracked = request
                                    .entries
                                    .iter()
                                    .filter(|entry| entry.untracked)
                                    .map(|entry| format!("{:?}", entry.path))
                                    .collect::<Vec<_>>();
                                let mut actions = Vec::new();
                                if !tracked.is_empty() {
                                    actions.push(format!(
                                        "Restore working tree from index: {}",
                                        tracked.join(", ")
                                    ));
                                }
                                if !untracked.is_empty() {
                                    actions.push(format!(
                                        "Delete untracked files: {}",
                                        untracked.join(", ")
                                    ));
                                }
                                let purpose =
                                    format!("{}. Type yes to confirm", actions.join("; "));
                                output.requests.push(HostRequest::MiniBuffer {
                                    purpose: purpose.clone(),
                                    text: String::new(),
                                    key_scope: None,
                                    confirmation: operation_id.clone(),
                                });
                                view.mutation =
                                    MutationIntent::ConfirmingDiscard(DiscardConfirmation {
                                        operation_id,
                                        purpose,
                                        epoch,
                                        request,
                                    });
                            } else {
                                model.mutation.start(
                                    self.services.borrow().start_file_mutation(request),
                                    MutationOperation::Files,
                                    operation_id,
                                    &mut output,
                                );
                            }
                        }
                    }
                }
                _ => operation_failed(
                    operation_id,
                    "Cannot change files while Git status is unavailable".into(),
                    &mut output,
                ),
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
            view.yanks
                .queue(&self.services, event, input, targets, &mut output)?;
        }
        let view_primary = primary_id.as_deref();
        let mut command_primary = input.selection.primary.as_deref();
        if matches!(
            event,
            Event::Open | Event::Preview | Event::Edit | Event::EditLayer
        ) && let Some(id) = command_primary
        {
            let available = match self.page {
                Page::History => model.commits.iter().any(|commit| commit.oid == id),
                Page::Changes | Page::CommitFiles => view.tree.nodes.contains_key(id),
            };
            if !available {
                command_primary = None;
                operation_failed(
                    input.context.operation_id.clone(),
                    format!("Selected item is unavailable: {id}"),
                    &mut output,
                );
            }
        }
        let primary = match event {
            Event::Open | Event::Preview => command_primary.or(view_primary),
            _ => view_primary,
        };
        let preview_event = match event {
            Event::Open | Event::Preview if command_primary.is_none() => Event::Unknown,
            Event::Open if self.page != Page::History => Event::Preview,
            _ => event,
        };
        let selected = primary
            .and_then(|id| view.tree.nodes.get(id))
            .map(|node| &node.kind);
        match &mut view.diff_refresh {
            DiffRefresh::WaitingForStatus { focus, .. }
            | DiffRefresh::LoadingDiffs { focus, .. } => {
                if preview_event == Event::Preview {
                    *focus = primary.map(str::to_owned);
                }
                if focus.as_deref() != primary || event == Event::ViewLeaving {
                    *focus = None;
                }
            }
            DiffRefresh::Current { .. } => {}
        }
        let refresh_diffs = matches!(view.diff_refresh, DiffRefresh::WaitingForStatus { .. })
            && event != Event::ViewLeaving
            && view.load.is_none()
            && matches!(model.mutation, MutationState::Idle { .. })
            && matches!(model.result, LoadResult::Ready);
        if refresh_diffs {
            let DiffRefresh::WaitingForStatus { epoch, focus } =
                std::mem::take(&mut view.diff_refresh)
            else {
                unreachable!();
            };
            view.diff_refresh = DiffRefresh::LoadingDiffs { epoch, focus };
        }
        match (self.page, selected) {
            (Page::History, _) => {
                let commit = model
                    .commits
                    .iter()
                    .find(|commit| Some(commit.oid.as_str()) == primary);
                view.history.column_alignment = options.column_alignment;
                view.history
                    .update(&self.services, preview_event, input, commit, &mut output)?;
                if !matches!(model.result, LoadResult::Failed(_)) {
                    let display = output.display.as_mut().unwrap();
                    display.status = view.history.totals.status(display.loading);
                }
                if matches!(event, Event::Open | Event::Edit)
                    && let Some(oid) = command_primary
                {
                    let files = view
                        .history
                        .published
                        .as_ref()
                        .filter(|summary| summary.commit == oid)
                        .and_then(|summary| summary.files.as_ref());
                    output.requests.push(HostRequest::Navigate {
                        navigation: Navigation::Push {
                            kind: "git-commit-files".into(),
                            argument: json!({
                                "commit": oid,
                                "files": files.map(|list| &list.files),
                                "path_warning": files.and_then(|list| list.path_warning.as_ref()),
                                "line_count_error": files.and_then(|list| list.line_count_error.as_ref()),
                            }),
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
                    if matches!(view.diff_refresh, DiffRefresh::WaitingForStatus { .. }) {
                        browser.defer(preview_event, input)?;
                        continue;
                    }
                    let visible = !files.is_empty();
                    let query = visible.then(|| PreviewQuery {
                        request: QueryRequest::DiffFiles {
                            source: DiffSource::Changes { side },
                            files,
                            render: DiffRenderOptions {
                                file_header: options.diff_file_header,
                                width: areas
                                    .next()
                                    .map(|area| area.width)
                                    .filter(|width| *width > 0),
                                pager: options.diff_pager.clone(),
                            },
                        },
                        path: path.into(),
                    });
                    if refresh_diffs {
                        browser.invalidate(query.clone());
                    }
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
                    if let DiffRefresh::LoadingDiffs {
                        epoch,
                        focus: requested_focus,
                    } = &mut view.diff_refresh
                    {
                        focus |= requested_focus.take().is_some();
                        view.diff_refresh = DiffRefresh::Current { epoch: *epoch };
                    }
                    output.requests.push(HostRequest::PreviewGroup {
                        panes,
                        layout,
                        focus,
                    });
                } else {
                    output.requests.push(HostRequest::PreviewPending);
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
                            file_header: options.diff_file_header,
                            width: input
                                .preview_area
                                .map(|area| area.width)
                                .filter(|width| *width > 0),
                            pager: options.diff_pager.clone(),
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
            (Page::Changes, None)
                if matches!(view.diff_refresh, DiffRefresh::WaitingForStatus { .. }) =>
            {
                for browser in &mut view.diffs {
                    browser.defer(preview_event, input)?;
                }
                output.requests.push(HostRequest::PreviewPending);
            }
            (Page::Changes | Page::CommitFiles, None) => {
                if matches!(view.diff_refresh, DiffRefresh::LoadingDiffs { .. }) {
                    view.diff_refresh = DiffRefresh::Current { epoch };
                }
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
        let edit = matches!(event, Event::Edit | Event::EditLayer);
        let target = command_primary
            .and_then(|id| view.tree.nodes.get(id))
            .and_then(|node| match &node.kind {
                TreeNodeKind::File(file) => Some(file.path()),
                TreeNodeKind::Directory { .. } => None,
            });
        if edit
            && self.page == Page::Changes
            && let Some(path) = target
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
        if self.page == Page::CommitFiles {
            if event == Event::Back {
                output.requests.push(HostRequest::Navigate {
                    navigation: Navigation::Back,
                });
            }
            let stale = view.snapshot.as_ref().is_some_and(|pending| {
                pending.view_primary.as_deref() != view_primary || pending.commit != model.commit
            });
            if (stale || event == Event::ViewLeaving)
                && let Some(pending) = view.snapshot.take()
            {
                self.operation_completions
                    .extend(cancelled_operation(pending.operation_id));
            }
            if edit && let Some(path) = target {
                if let Some(pending) = view.snapshot.take() {
                    self.operation_completions
                        .extend(cancelled_operation(pending.operation_id));
                }
                let task = Task::start(
                    &self.services,
                    QueryRequest::Blob {
                        commit: model.commit.clone(),
                        path: path.into(),
                    },
                );
                let task = match task {
                    Ok(task) => Some(task),
                    Err(error) => {
                        operation_failed(
                            input.context.operation_id.clone(),
                            format!("{error:#}"),
                            &mut output,
                        );
                        None
                    }
                };
                if let Some(task) = task {
                    if let Some(operation_id) = &input.context.operation_id {
                        output.pending_operations.push(operation_id.clone());
                    }
                    view.snapshot = Some(PendingSnapshot {
                        operation_id: input.context.operation_id.clone(),
                        commit: model.commit.clone(),
                        view_primary: view_primary.map(str::to_owned),
                        task,
                        presentation: if event == Event::EditLayer {
                            SnapshotPresentation::Fullscreen
                        } else {
                            SnapshotPresentation::Editor
                        },
                    });
                }
            }
            if let Some(pending) = &mut view.snapshot {
                match pending.task.poll() {
                    Ok(Some(QueryResult::Snapshot { snapshot })) => {
                        complete_operation(
                            pending.operation_id.take(),
                            OperationResult::DocumentOpened,
                            &mut output,
                        );
                        output.requests.push(HostRequest::OpenSnapshot {
                            snapshot,
                            presentation: pending.presentation,
                        });
                        view.snapshot = None;
                    }
                    Ok(Some(QueryResult::Unsupported { message })) => {
                        operation_failed(pending.operation_id.take(), message, &mut output);
                        view.snapshot = None;
                    }
                    Ok(Some(_)) => {
                        operation_failed(
                            pending.operation_id.take(),
                            "historical file query returned another kind of result".into(),
                            &mut output,
                        );
                        view.snapshot = None;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        operation_failed(
                            pending.operation_id.take(),
                            format!("{error:#}"),
                            &mut output,
                        );
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
            Event::Page => Some(self.page.other_tab()),
            _ => None,
        };
        if let Some(target) = target_page
            && self.page.tab() != target
        {
            output.requests.push(self.page.tab_request(target));
        }
        if self.page == Page::CommitFiles {
            output.requests.push(HostRequest::ConfigurePreview {
                header: String::new(),
                key_scope: Some("git-commit-preview".into()),
            });
        }
        view.yanks.advance(&self.services, event, &mut output);
        let display = output.display.as_mut().unwrap();
        if let Some(load) = &view.load {
            display.status["git"]["query_id"] = load.generation.into();
        }
        if let MutationState::Writing { epoch, .. } = &model.mutation {
            display.loading = true;
            display.status["git"]["write_epoch"] = (*epoch).into();
        }
        if view.load.is_some() || matches!(model.mutation, MutationState::Writing { .. }) {
            output.requests.push(HostRequest::Schedule { after_ms: 50 });
        }
        Ok(output)
    }

    fn poll_operations(&mut self) -> Vec<PluginOutput> {
        Git::poll_operations(self)
    }

    fn remove_buffer_view(&mut self, buffer: BufferId, view: ViewId) {
        if let Some(mut view) = self.views.remove(&(buffer, view)) {
            self.operation_completions.extend(view.cancel_operations());
        }
    }

    fn remove_view(&mut self, id: ViewId) {
        self.views.retain(|(_, view_id), view| {
            if *view_id == id {
                self.operation_completions.extend(view.cancel_operations());
                false
            } else {
                true
            }
        });
    }
    fn remove_buffer(&mut self, id: BufferId) {
        if let Some(model) = self.models.remove(&id)
            && let MutationState::Writing { mutation, .. } = model.mutation
        {
            self.detached_mutations.push(mutation);
        }
        self.views.retain(|(buffer, _), view| {
            if *buffer == id {
                self.operation_completions.extend(view.cancel_operations());
                false
            } else {
                true
            }
        });
    }
}
