use anyhow::{Context, Result, ensure};
use zess_git::{DiffSide, DiffSource, patch::RawPatch};

use super::{Content, DiffPreview, Pane, Previews, Selection};

pub(in crate::pages) enum PreviewSource {
    Directory {
        path: String,
    },
    Worktree {
        path: String,
        line: usize,
    },
    Commit {
        commit: String,
        path: String,
        line: usize,
    },
}

pub(super) struct Search {
    forward: bool,
    selection: Selection,
    scroll: usize,
    horizontal_scroll: usize,
    follow_selection: bool,
}

impl DiffPreview {
    pub(super) fn cancel_search(&mut self, viewport: &mut zess_text::PreviewViewport) {
        if let Some(search) = self.search.take() {
            self.selection = search.selection;
            self.follow_selection = search.follow_selection;
            viewport.scroll = search.scroll;
            viewport.horizontal_scroll = search.horizontal_scroll;
        }
    }
}

impl Previews {
    fn current(&self) -> Result<&Pane> {
        let pane = self.panes.get(self.active).context("no active preview")?;
        ensure!(
            !self.pending && !pane.loading,
            "wait for the current preview to finish loading"
        );
        Ok(pane)
    }

    pub(in crate::pages) fn selected_patch(
        &self,
        revision: u64,
    ) -> Result<zess_protocol::resource::PagePatch> {
        let pane = self.current()?;
        let Content::Diff(preview) = &pane.content else {
            anyhow::bail!("staging requires a Git diff")
        };
        let Some(DiffSource::Changes { side }) = preview.diff.source else {
            anyhow::bail!("this diff cannot be staged")
        };
        let (file, rows) = preview
            .diff
            .selected_patch_rows(preview.presentation, &preview.selection)?;
        RawPatch::parse(&file.raw)?.selection(side, &rows)?;
        Ok(zess_protocol::resource::PagePatch {
            revision,
            target: file.target.clone(),
            side,
            rows,
        })
    }

    pub(in crate::pages) fn source(&self) -> Result<PreviewSource> {
        let pane = self.current()?;
        let (path, line) = match &pane.content {
            Content::Text {
                buffer,
                snapshot: Some(id),
            } => {
                return Ok(PreviewSource::Commit {
                    commit: id.commit.clone(),
                    path: id.path.clone(),
                    line: buffer.view(super::PREVIEW_VIEW).cursor().0,
                });
            }
            Content::Text {
                buffer,
                snapshot: None,
            } => (
                pane.path.clone(),
                buffer.view(super::PREVIEW_VIEW).cursor().0,
            ),
            Content::Rows {
                scope: "directory", ..
            } => {
                return Ok(PreviewSource::Directory {
                    path: pane.path.clone(),
                });
            }
            Content::Rows { .. } => anyhow::bail!("preview has no source"),
            Content::Diff(preview) => {
                let row = preview
                    .selection
                    .row()
                    .and_then(|row| preview.diff.raw_row(preview.presentation, row))
                    .context("this diff has no selected source line")?;
                let line = preview.diff.index.rows[row]
                    .source_line
                    .context("this diff has no selected source line")?;
                let Some(source) = &preview.diff.source else {
                    ensure!(!pane.path.is_empty(), "preview has no source path");
                    return Ok(PreviewSource::Worktree {
                        path: pane.path.clone(),
                        line,
                    });
                };
                let file = preview
                    .diff
                    .file_at(row)
                    .context("this diff has no Git source")?;
                match source {
                    DiffSource::Commit { commit } => {
                        return Ok(PreviewSource::Commit {
                            commit: commit.clone(),
                            path: file.target.path.clone(),
                            line,
                        });
                    }
                    DiffSource::Changes {
                        side: DiffSide::Staged,
                    } => {
                        ensure!(
                            !self.panes.iter().any(|p| p.interactive && p.loading),
                            "wait for Git previews to finish loading"
                        );
                        let mut line = line;
                        for other in &self.panes {
                            if let Content::Diff(preview) = &other.content
                                && preview.diff.source
                                    == Some(DiffSource::Changes {
                                        side: DiffSide::Unstaged,
                                    })
                            {
                                line = preview.diff.map_old_line(Some(&file.target), line);
                            }
                        }
                        (file.target.path.clone(), line)
                    }
                    DiffSource::Changes { .. } => (file.target.path.clone(), line),
                }
            }
        };
        ensure!(!path.is_empty(), "preview has no source path");
        Ok(PreviewSource::Worktree { path, line })
    }

    fn diff_view(&mut self) -> Option<(&mut DiffPreview, &mut zess_text::PreviewViewport)> {
        let pane = self.panes.get_mut(self.active)?;
        if let Content::Diff(diff) = &mut pane.content {
            Some((diff, &mut pane.viewport))
        } else {
            None
        }
    }

    pub(in crate::pages) fn begin_search(&mut self, forward: bool) -> bool {
        let Some((diff, viewport)) = self.diff_view() else {
            return false;
        };
        diff.search = Some(Search {
            forward,
            selection: diff.selection.clone(),
            scroll: viewport.scroll,
            horizontal_scroll: viewport.horizontal_scroll,
            follow_selection: diff.follow_selection,
        });
        true
    }

    pub(in crate::pages) fn search(&mut self, pattern: &str) -> Result<bool> {
        let Some((diff, viewport)) = self.diff_view() else {
            return Ok(false);
        };
        let Some(search) = &diff.search else {
            return Ok(false);
        };
        diff.selection.clone_from(&search.selection);
        viewport.scroll = search.scroll;
        viewport.horizontal_scroll = search.horizontal_scroll;
        diff.follow_selection = search.follow_selection;
        if pattern.is_empty() {
            return Ok(false);
        }
        let found = diff.diff.search(
            diff.presentation,
            pattern,
            diff.selection.row(),
            search.forward,
        )?;
        if let Some(found) = found {
            diff.selection.set_head(Some(found));
            diff.follow_selection = true;
        }
        Ok(found.is_some())
    }

    pub(in crate::pages) fn cancel_search(&mut self) {
        if let Some((diff, viewport)) = self.diff_view() {
            diff.cancel_search(viewport);
        }
    }

    pub(in crate::pages) fn accept_search(&mut self) {
        if let Some((diff, _)) = self.diff_view() {
            diff.search = None;
        }
    }

    pub(in crate::pages) fn repeat_search(&mut self, pattern: &str, forward: bool) -> Result<bool> {
        let Some((diff, _)) = self.diff_view() else {
            return Ok(false);
        };
        if pattern.is_empty() {
            return Ok(false);
        }
        let found = diff
            .diff
            .search(diff.presentation, pattern, diff.selection.row(), forward)?;
        if let Some(found) = found {
            diff.selection.set_head(Some(found));
            diff.follow_selection = true;
        }
        Ok(found.is_some())
    }
}
