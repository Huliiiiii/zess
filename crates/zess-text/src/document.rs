use anyhow::{Context, Result, ensure};
use helix_core::{
    ChangeSet, Range, Rope, Selection, Transaction, history::State,
    line_ending::auto_detect_line_ending,
};
use zess_protocol::{
    TextRange,
    document::{DocumentChange, DocumentHistoryAction, DocumentOrigin, DocumentSelection},
};
use zess_ui::ScrollMode;

use super::{EditGroup, EditOwner, EditTransaction, TextBuffer, TextMode};

mod history;
#[cfg(test)]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub enum DocumentStep {
    Edit {
        changes: Vec<DocumentChange>,
        origin: Option<DocumentOrigin>,
    },
    History(DocumentHistoryAction),
}

pub(super) enum RecordedStep {
    Edit(ChangeSet, Option<DocumentOrigin>),
    History(DocumentHistoryAction),
}

pub(super) fn record_edit(
    steps: &mut Vec<RecordedStep>,
    transaction: &Transaction,
    origin: Option<DocumentOrigin>,
) {
    if transaction.changes().is_empty() {
        return;
    }
    if let Some(RecordedStep::Edit(previous, _)) = steps.last_mut() {
        *previous = std::mem::take(previous).compose(transaction.changes().clone());
    } else {
        steps.push(RecordedStep::Edit(transaction.changes().clone(), origin));
    }
}

pub(super) fn replacements(transaction: &Transaction) -> Vec<DocumentChange> {
    transaction
        .changes_iter()
        .map(|(start, end, insert)| DocumentChange {
            start,
            end,
            insert: insert.map_or_else(String::new, |s| s.to_string()),
        })
        .collect()
}

pub(super) fn selection_state(selection: &Selection) -> DocumentSelection {
    DocumentSelection {
        ranges: selection
            .iter()
            .map(|r| TextRange {
                anchor: r.anchor,
                head: r.head,
            })
            .collect(),
        primary: selection.primary_index(),
    }
}

fn selection_from_state(state: &DocumentSelection, length: usize) -> Result<Selection> {
    ensure!(
        !state.ranges.is_empty() && state.primary < state.ranges.len(),
        "invalid document selection"
    );
    ensure!(
        state
            .ranges
            .iter()
            .all(|r| r.anchor <= length && r.head <= length),
        "selection exceeds the document"
    );
    Ok(Selection::new(
        state
            .ranges
            .iter()
            .map(|r| Range::new(r.anchor, r.head))
            .collect(),
        state.primary,
    ))
}

fn transaction(rope: &Rope, changes: &[DocumentChange]) -> Result<Transaction> {
    let length = rope.len_chars();
    let mut previous = 0;
    for change in changes {
        ensure!(
            previous <= change.start && change.start <= change.end && change.end <= length,
            "document change is outside its base version or overlaps a preceding change"
        );
        previous = change.end;
    }
    let transaction = Transaction::change(
        rope,
        changes.iter().map(|c| {
            (
                c.start,
                c.end,
                (!c.insert.is_empty()).then(|| c.insert.as_str().into()),
            )
        }),
    );
    ensure!(
        !transaction.changes().is_empty(),
        "document edit contains no changes"
    );
    Ok(transaction)
}

impl super::TextView {
    pub fn workspace_state(&self) -> zess_protocol::workspace_view::DocumentViewState {
        let DocumentSelection { ranges, primary } = selection_state(self.selection());
        zess_protocol::workspace_view::DocumentViewState {
            ranges,
            primary,
            mode: self.mode().to_owned(),
            scroll: self.scroll(),
            horizontal_scroll: self.horizontal_scroll(),
        }
    }
}

impl TextBuffer {
    /// Identifies the contents, undo history, and saved revision of a document.
    pub fn document_version(&self) -> u64 {
        self.document_version
    }

    pub(super) fn has_pending_history(&self) -> bool {
        matches!(&self.group, EditTransaction::Active(group) if !group.transaction.changes().is_empty())
    }

    pub fn track_changes(&mut self) {
        self.recorded_changes = Some(Vec::new());
    }

    pub fn has_document_steps(&self) -> bool {
        self.recorded_changes
            .as_ref()
            .is_some_and(|steps| !steps.is_empty())
    }

    pub fn take_document_steps(&mut self) -> Vec<DocumentStep> {
        self.recorded_changes
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|step| match step {
                RecordedStep::Edit(changes, _) if changes.is_empty() => None,
                RecordedStep::Edit(changes, origin) => Some(DocumentStep::Edit {
                    changes: replacements(&Transaction::from(changes)),
                    origin,
                }),
                RecordedStep::History(action) => Some(DocumentStep::History(action)),
            })
            .collect()
    }

    pub fn finish_document_edit(&mut self) {
        self.commit_group();
    }

    pub fn set_replica_path(&mut self, path: Option<&str>) {
        self.path = path.map(std::path::PathBuf::from);
    }

    pub fn readonly(&self) -> bool {
        self.readonly
    }

    fn document_selection(&self) -> Selection {
        self.views
            .iter()
            .min_by_key(|(id, _)| *id)
            .map_or_else(|| Selection::point(0), |(_, view)| view.selection.clone())
    }

    fn apply_document_transaction(&mut self, transaction: Transaction, selection: Selection) {
        self.commit_group();
        let original = State {
            doc: self.rope.clone(),
            selection,
        };
        let mapped = original.selection.clone().map(transaction.changes());
        self.apply_without_history(None, &transaction, None);
        if let Some(steps) = &mut self.recorded_changes {
            record_edit(steps, &transaction, None);
            steps.push(RecordedStep::History(DocumentHistoryAction::Commit {
                before: selection_state(&original.selection),
                after: selection_state(&mapped),
            }));
        }
        self.history
            .commit_revision(&transaction.with_selection(mapped), &original);
    }

    /// Applies a single remote revision and maps every attached view through it.
    pub fn apply_document_changes(
        &mut self,
        changes: &[DocumentChange],
        origin: Option<&DocumentOrigin>,
    ) -> Result<()> {
        ensure!(!self.readonly, "buffer is read-only");
        let transaction = transaction(&self.rope, changes)?;
        if !matches!(self.group, EditTransaction::Active(ref group) if group.owner.local().is_none())
        {
            let selection = if let Some(origin) = origin {
                selection_from_state(&origin.selection, self.rope.len_chars())?
            } else {
                self.document_selection()
            };
            self.commit_group();
            self.group = EditTransaction::Active(EditGroup {
                original: State {
                    doc: self.rope.clone(),
                    selection,
                },
                transaction: Transaction::new(&self.rope),
                owner: EditOwner::Replica(origin.map(|o| o.view_id)),
            });
        }
        self.apply_without_history(None, &transaction, None);
        let EditTransaction::Active(group) = &mut self.group else {
            unreachable!()
        };
        group.transaction = std::mem::take(&mut group.transaction).compose(transaction);
        Ok(())
    }

    pub fn apply_document_history(&mut self, action: &DocumentHistoryAction) -> Result<()> {
        ensure!(!self.readonly, "buffer is read-only");
        match action {
            DocumentHistoryAction::Commit { before, after } => {
                let EditTransaction::Active(group) = &self.group else {
                    return Ok(());
                };
                if group.transaction.changes().is_empty() {
                    self.group = EditTransaction::Idle;
                    return Ok(());
                }
                let original_selection =
                    selection_from_state(before, group.original.doc.len_chars())?;
                let selection = selection_from_state(after, self.rope.len_chars())?;
                let EditTransaction::Active(mut group) = std::mem::take(&mut self.group) else {
                    unreachable!()
                };
                group.original.selection = original_selection;
                self.history.commit_revision(
                    &group.transaction.with_selection(selection),
                    &group.original,
                );
                self.document_version += 1;
            }
            DocumentHistoryAction::Undo => {
                ensure!(
                    !self.history.at_root() || self.has_pending_history(),
                    "document history has no matching revision"
                );
                self.commit_group();
                let transaction = self.history.undo().unwrap().clone();
                self.apply_without_history(None, &transaction, None);
            }
            DocumentHistoryAction::Redo => {
                ensure!(
                    !self.has_pending_history(),
                    "document history has no matching revision"
                );
                let transaction = self
                    .history
                    .redo()
                    .cloned()
                    .context("document history has no matching revision")?;
                self.commit_group();
                self.apply_without_history(None, &transaction, None);
            }
        }
        Ok(())
    }

    pub(super) fn replace_contents(&mut self, contents: &str, selection: Selection) {
        self.commit_group();
        if self.rope != contents {
            let transaction =
                helix_core::diff::compare_ropes(&self.rope, &Rope::from_str(contents));
            self.apply_document_transaction(transaction, selection);
        }
        if let Some(line_ending) = auto_detect_line_ending(&self.rope) {
            self.line_ending = line_ending;
        }
    }

    pub fn replace_from_replica(&mut self, contents: &str) {
        if self.readonly {
            self.replace_readonly(contents).expect("read-only replica");
        } else {
            self.replace_contents(contents, self.document_selection());
        }
    }

    pub fn restore_view(
        &mut self,
        id: u64,
        ranges: &[TextRange],
        primary: usize,
        mode: &str,
        scroll: usize,
        horizontal_scroll: usize,
    ) -> Result<()> {
        let length = self.rope.len_chars();
        ensure!(
            !ranges.is_empty() && primary < ranges.len(),
            "invalid primary selection"
        );
        ensure!(
            ranges
                .iter()
                .all(|range| range.head <= length && range.anchor <= length),
            "document selection exceeds its content"
        );
        if mode == "insert"
            && let EditTransaction::Active(group) = &mut self.group
            && group.owner == EditOwner::Replica(Some(id))
        {
            group.owner = EditOwner::View(id);
        }
        self.ensure_view(id);
        let mode = TextMode::parse(mode);
        ensure!(
            !self.readonly || mode != TextMode::Insert,
            "buffer is read-only"
        );
        let view = self.views.get_mut(&id).unwrap();
        if view.mode != mode {
            view.cancel_pending_input();
            view.mode = mode;
        }

        view.selection = Selection::new(
            ranges
                .iter()
                .map(|range| Range::new(range.anchor, range.head))
                .collect(),
            primary,
        );
        view.scroll = scroll.min(self.rope.len_lines().saturating_sub(1));
        view.horizontal_scroll = horizontal_scroll;
        view.scroll_mode = ScrollMode::UserControlled;
        self.refresh_cursors();
        Ok(())
    }
}
