use zess_protocol::document::DocumentHistory;

use super::*;
use crate::TextCommand;

fn synchronize(replica: &mut TextBuffer, workspace: &mut TextBuffer) {
    for step in replica.take_document_steps() {
        match step {
            DocumentStep::Edit { changes, origin } => workspace
                .apply_document_changes(&changes, origin.as_ref())
                .unwrap(),
            DocumentStep::History(action) => workspace.apply_document_history(&action).unwrap(),
        }
    }
}

#[test]
fn the_restored_insert_view_continues_its_open_undo_group() {
    let mut first = TextBuffer::new("one two");
    let mut workspace = TextBuffer::new("one two");
    first.execute(7, TextCommand::GotoLineEnd).unwrap();
    first.track_changes();
    first.execute(7, TextCommand::Append).unwrap();
    first.input_text(7, "猫").unwrap();
    synchronize(&mut first, &mut workspace);
    let mut second = TextBuffer::new(&workspace.text());
    second
        .restore_history(&workspace.replica_history())
        .unwrap();
    // Restoring the other pane must preserve the active edit group's owner.
    second
        .restore_view(9, &[TextRange { anchor: 0, head: 0 }], 0, "insert", 0, 0)
        .unwrap();
    let cursor = selection_state(first.view(7).selection());
    second
        .restore_view(7, &cursor.ranges, cursor.primary, "insert", 0, 0)
        .unwrap();
    second.track_changes();
    second.input_text(7, "犬").unwrap();
    second.execute(7, TextCommand::Escape).unwrap();
    synchronize(&mut second, &mut workspace);
    assert_eq!(workspace.text(), "one two猫犬");
    second.track_changes();
    second.execute(7, TextCommand::Undo).unwrap();
    synchronize(&mut second, &mut workspace);
    assert_eq!(workspace.text(), "one two");
    assert_eq!(second.text(), "one two");

    let mut third = TextBuffer::new(&workspace.text());
    third.restore_history(&workspace.replica_history()).unwrap();
    third.execute(8, TextCommand::Redo).unwrap();
    assert_eq!(third.text(), "one two猫犬");
}

#[test]
fn a_failed_redo_leaves_the_open_undo_group_intact() {
    let mut workspace = TextBuffer::new("a");
    workspace
        .apply_document_changes(
            &[DocumentChange {
                start: 1,
                end: 1,
                insert: "b".into(),
            }],
            None,
        )
        .unwrap();
    let version = workspace.document_version();
    let history = workspace.replica_history();
    assert!(
        workspace
            .apply_document_history(&DocumentHistoryAction::Redo)
            .is_err()
    );
    assert_eq!(workspace.document_version(), version);
    assert_eq!(workspace.replica_history(), history);
}

#[test]
fn committing_cancelled_edits_creates_no_undo_revision() {
    let mut workspace = TextBuffer::new("a");
    for change in [
        DocumentChange {
            start: 1,
            end: 1,
            insert: "b".into(),
        },
        DocumentChange {
            start: 1,
            end: 2,
            insert: String::new(),
        },
    ] {
        workspace.apply_document_changes(&[change], None).unwrap();
    }
    let version = workspace.document_version();
    let cursor = selection_state(&Selection::point(0));
    workspace
        .apply_document_history(&DocumentHistoryAction::Commit {
            before: cursor.clone(),
            after: cursor,
        })
        .unwrap();
    assert_eq!(workspace.document_version(), version);
    assert_eq!(workspace.text(), "a");
    assert_eq!(workspace.replica_history(), DocumentHistory::default());
}
