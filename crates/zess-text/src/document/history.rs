use anyhow::{Result, ensure};
use helix_core::{
    Rope,
    history::{History, State},
};
use zess_protocol::document::{DocumentHistory, HistoryRevision};

use super::{
    EditGroup, EditOwner, EditTransaction, TextBuffer, replacements, selection_from_state,
    selection_state, transaction,
};

impl TextBuffer {
    pub fn history_revision(&self) -> usize {
        self.history.current_revision()
    }

    /// Marks the revision sent for saving, which can precede later local edits.
    pub fn mark_history_saved(&mut self, revision: usize) {
        if self.saved_revision != revision {
            self.saved_revision = revision;
            self.document_version += 1;
        }
    }

    /// The branch reachable with undo and redo, plus the uncommitted insert group.
    pub fn replica_history(&mut self) -> DocumentHistory {
        let pending = match &self.group {
            EditTransaction::Active(group) if !group.transaction.changes().is_empty() => {
                let after = group
                    .owner
                    .local()
                    .and_then(|id| self.views.get(&id))
                    .map(|view| view.selection.clone())
                    .unwrap_or_else(|| {
                        group
                            .original
                            .selection
                            .clone()
                            .map(group.transaction.changes())
                    });
                Some(HistoryRevision {
                    changes: replacements(&group.transaction),
                    before: selection_state(&group.original.selection),
                    after: selection_state(&after),
                })
            }
            _ => None,
        };
        let mut root = match &self.group {
            EditTransaction::Idle => self.rope.clone(),
            EditTransaction::Active(group) => group.original.doc.clone(),
        };
        let original = self.history.current_revision();
        let mut current = 0;
        while let Some(inverse) = self.history.undo() {
            inverse.apply(&mut root);
            current += 1;
        }

        let mut revisions = Vec::new();
        let mut saved = (self.saved_revision == 0).then_some(0);
        while let Some(forward) = self.history.redo() {
            let changes = replacements(forward);
            let after = selection_state(forward.selection().expect("committed revision selection"));
            let revision = self.history.current_revision();
            let inverse = self.history.undo().unwrap();
            let before = selection_state(inverse.selection().expect("undo selection"));
            self.history.redo();
            revisions.push(HistoryRevision {
                changes,
                before,
                after,
            });
            if revision == self.saved_revision {
                saved = Some(revisions.len());
            }
        }
        while self.history.current_revision() != original {
            self.history
                .undo()
                .expect("original revision is on the active undo branch");
        }
        DocumentHistory {
            root: (!revisions.is_empty() || pending.is_some()).then(|| root.to_string()),
            revisions,
            current,
            saved,
            pending_view: match &self.group {
                EditTransaction::Active(group) if pending.is_some() => group.owner.view(),
                _ => None,
            },
            pending,
        }
    }

    pub fn restore_history(&mut self, state: &DocumentHistory) -> Result<()> {
        ensure!(
            state.current <= state.revisions.len(),
            "invalid current undo revision"
        );
        let mut rope = state
            .root
            .as_deref()
            .map_or_else(|| self.rope.clone(), Rope::from_str);
        let mut history = History::default();
        for revision in &state.revisions {
            let forward = transaction(&rope, &revision.changes)?;
            let before = State {
                doc: rope.clone(),
                selection: selection_from_state(&revision.before, rope.len_chars())?,
            };
            ensure!(forward.apply(&mut rope), "undo revision does not apply");
            let after = selection_from_state(&revision.after, rope.len_chars())?;
            history.commit_revision(&forward.with_selection(after), &before);
        }
        while history.current_revision() > state.current {
            history.undo().unwrap().apply(&mut rope);
        }
        let group = if let Some(revision) = &state.pending {
            let change = transaction(&rope, &revision.changes)?;
            let original = State {
                doc: rope.clone(),
                selection: selection_from_state(&revision.before, rope.len_chars())?,
            };
            ensure!(change.apply(&mut rope), "open undo group does not apply");
            EditTransaction::Active(EditGroup {
                original,
                transaction: change,
                owner: EditOwner::Replica(state.pending_view),
            })
        } else {
            EditTransaction::Idle
        };
        ensure!(
            rope == self.rope,
            "undo history does not match the document contents"
        );
        self.history = history;
        self.group = group;
        self.saved_revision = state.saved.unwrap_or(usize::MAX);
        Ok(())
    }
}
