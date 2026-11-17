use zess_protocol::TextRange;
use zess_protocol::document::{
    DocumentChange, DocumentHistory, DocumentOrigin, DocumentSelection, HistoryRevision,
};

use super::*;

fn snapshot(version: u64, text: &str) -> DocumentSnapshot {
    DocumentSnapshot {
        buffer_id: 7,
        version,
        text: text.into(),
        modified: false,
        language: None,
        path: None,
        readonly: false,
        history: DocumentHistory {
            saved: Some(0),
            ..Default::default()
        },
    }
}

fn open() -> Documents {
    let mut docs = Documents::default();
    docs.open(7);
    assert_eq!(docs.requests(), [DocumentRequest::Read { buffer_id: 7 }]);
    docs.receive(DocumentEvent::Snapshot(snapshot(18, "猫")));
    docs
}

#[test]
fn a_lost_edit_receipt_can_be_replayed_before_saving_its_version() {
    let mut docs = open();
    docs.execute(7, 12, TextCommand::Append).unwrap();
    docs.input(7, 12, "犬").unwrap();
    let edit = docs.requests();
    assert_eq!(
        edit,
        [DocumentRequest::Edit {
            buffer_id: 7,
            sequence: 1,
            base_version: 18,
            origin: Some(DocumentOrigin {
                view_id: 12,
                selection: DocumentSelection {
                    ranges: vec![TextRange { anchor: 0, head: 1 }],
                    primary: 0
                }
            }),
            changes: vec![DocumentChange {
                start: 1,
                end: 1,
                insert: "犬".into()
            }],
        }]
    );
    docs.save(7, None, None).unwrap();
    docs.input(7, 12, "!").unwrap();
    assert_eq!(docs.text(7).unwrap().text(), "猫犬!");
    assert_eq!(docs.view_state(7, 12).unwrap().ranges[0].head, 3);
    assert!(docs.requests().is_empty());

    docs.reconnect();
    let replay = docs.requests();
    assert_eq!(replay[1..], edit);
    docs.receive(DocumentEvent::Snapshot(snapshot(19, "猫犬")));
    assert_eq!(docs.text(7).unwrap().text(), "猫犬!");
    docs.receive(DocumentEvent::Applied {
        buffer_id: 7,
        sequence: 1,
        version: 19,
        modified: true,
    });
    assert!(matches!(
        docs.requests().as_slice(),
        [DocumentRequest::History {
            sequence: 2,
            action: DocumentHistoryAction::Commit { .. },
            ..
        }]
    ));
    docs.receive(DocumentEvent::Applied {
        buffer_id: 7,
        sequence: 2,
        version: 20,
        modified: true,
    });
    assert_eq!(
        docs.requests(),
        [DocumentRequest::Save {
            buffer_id: 7,
            sequence: 3,
            version: 20,
            path: None
        }]
    );
    docs.receive(DocumentEvent::Saved {
        buffer_id: 7,
        sequence: 3,
        version: 21,
    });
    assert_eq!(docs.modified(7), Some(true));
    assert_eq!(
        docs.requests(),
        [DocumentRequest::Edit {
            buffer_id: 7,
            sequence: 4,
            base_version: 21,
            origin: Some(DocumentOrigin {
                view_id: 12,
                selection: DocumentSelection {
                    ranges: vec![TextRange { anchor: 2, head: 2 }],
                    primary: 0
                }
            }),
            changes: vec![DocumentChange {
                start: 2,
                end: 2,
                insert: "!".into()
            }],
        }]
    );
}

#[test]
fn conflicting_edits_are_retained_until_the_user_chooses_a_version() {
    for keep_local in [false, true] {
        let mut docs = open();
        docs.execute(7, 12, TextCommand::Append).unwrap();
        docs.input(7, 12, "犬").unwrap();
        docs.requests();
        let notice = docs.receive(DocumentEvent::Conflict {
            buffer_id: 7,
            sequence: 1,
            current: snapshot(19, "虎"),
        });
        assert!(notice.notice.is_some());
        assert!(docs.requests().is_empty());
        assert_eq!(docs.text(7).unwrap().text(), "猫犬");
        assert!(docs.save(7, None, None).is_err());
        let position = docs.view_state(7, 12);
        docs.resolve(7, keep_local).unwrap();
        if keep_local {
            assert_eq!(docs.view_state(7, 12), position);
            assert_eq!(
                docs.requests(),
                [DocumentRequest::Edit {
                    buffer_id: 7,
                    sequence: 2,
                    base_version: 19,
                    origin: None,
                    changes: vec![DocumentChange {
                        start: 0,
                        end: 1,
                        insert: "猫犬".into()
                    }],
                }]
            );
        } else {
            assert_eq!(docs.text(7).unwrap().text(), "虎");
            assert!(docs.requests().is_empty());
        }
    }
}

#[test]
fn reload_after_an_unacknowledged_save_preserves_later_external_edits() {
    let mut docs = open();
    docs.save(7, None, None).unwrap();
    docs.requests();
    docs.reconnect();
    docs.requests();
    docs.receive(DocumentEvent::Snapshot(snapshot(21, "豹")));
    docs.receive(DocumentEvent::Saved {
        buffer_id: 7,
        sequence: 1,
        version: 18,
    });
    assert_eq!(docs.text(7).unwrap().text(), "豹");
    assert_eq!(docs.modified(7), Some(false));
}

#[test]
fn only_the_successful_receipt_for_a_sent_save_can_close_its_view() {
    let mut docs = open();
    docs.save(7, None, Some(12)).unwrap();
    let saved = DocumentEvent::Saved {
        buffer_id: 7,
        sequence: 1,
        version: 18,
    };
    assert!(docs.receive(saved.clone()).close.is_none());
    assert_eq!(
        docs.requests(),
        [DocumentRequest::Save {
            buffer_id: 7,
            sequence: 1,
            version: 18,
            path: None,
        }]
    );
    assert!(
        docs.receive(DocumentEvent::Applied {
            buffer_id: 7,
            sequence: 1,
            version: 19,
            modified: true,
        })
        .close
        .is_none()
    );
    assert_eq!(
        docs.receive(saved.clone()).close,
        Some(DocumentPane {
            buffer_id: 7,
            view_id: 12,
        })
    );
    assert!(docs.receive(saved).close.is_none());
}

#[test]
fn edits_accumulated_while_waiting_for_a_receipt_share_one_transaction() {
    let mut docs = open();
    docs.execute(7, 12, TextCommand::Append).unwrap();
    docs.input(7, 12, "a").unwrap();
    docs.requests();
    docs.input(7, 12, "b").unwrap();
    docs.input(7, 12, "c").unwrap();
    docs.execute(7, 12, TextCommand::DeleteBackward).unwrap();
    docs.input(7, 12, "d").unwrap();
    docs.receive(DocumentEvent::Applied {
        buffer_id: 7,
        sequence: 1,
        version: 19,
        modified: true,
    });
    assert_eq!(
        docs.requests(),
        [DocumentRequest::Edit {
            buffer_id: 7,
            sequence: 2,
            base_version: 19,
            origin: None,
            changes: vec![DocumentChange {
                start: 2,
                end: 2,
                insert: "bd".into()
            }],
        }]
    );
}

fn saved_edit() -> DocumentSnapshot {
    let cursor = DocumentSelection {
        ranges: vec![TextRange { anchor: 0, head: 1 }],
        primary: 0,
    };
    let mut remote = snapshot(21, "猫犬");
    remote.history = DocumentHistory {
        root: Some("猫".into()),
        revisions: vec![HistoryRevision {
            changes: vec![DocumentChange {
                start: 1,
                end: 1,
                insert: "犬".into(),
            }],
            before: cursor.clone(),
            after: cursor,
        }],
        current: 1,
        saved: Some(1),
        pending: None,
        pending_view: None,
    };
    remote
}

#[test]
fn pending_edits_are_recoverable_when_another_client_saves_during_reconnect() {
    let mut docs = open();
    docs.execute(7, 12, TextCommand::Append).unwrap();
    docs.input(7, 12, "犬").unwrap();
    let sent = docs.requests();
    docs.input(7, 12, "!").unwrap();

    docs.reconnect();
    assert_eq!(docs.requests()[1..], sent);
    let remote = saved_edit();
    assert!(
        docs.receive(DocumentEvent::Snapshot(remote.clone()))
            .notice
            .is_none()
    );
    let update = docs.receive(DocumentEvent::Applied {
        buffer_id: 7,
        sequence: 1,
        version: 19,
        modified: true,
    });
    assert_eq!(update.notice.as_deref(), Some(conflict_notice(7).as_str()));
    assert_eq!(docs.text(7).unwrap().text(), "猫犬!");
    assert_eq!(docs.modified(7), Some(true));
    assert!(docs.requests().is_empty());

    docs.resolve(7, true).unwrap();
    assert_eq!(docs.text(7).unwrap().text(), "猫犬!");
    assert!(
        matches!(docs.requests().as_slice(), [DocumentRequest::Edit { base_version, changes, .. }]
        if *base_version == remote.version && changes == &[DocumentChange { start: 2, end: 2, insert: "!".into() }])
    );
}

#[test]
fn a_new_terminal_can_undo_an_edit_saved_in_the_session_history() {
    let mut docs = Documents::default();
    docs.open(7);
    docs.requests();
    assert!(
        docs.receive(DocumentEvent::Snapshot(saved_edit()))
            .notice
            .is_none()
    );
    docs.execute(7, 12, TextCommand::Undo).unwrap();
    assert_eq!(docs.text(7).unwrap().text(), "猫");
    assert_eq!(
        docs.requests(),
        [DocumentRequest::History {
            buffer_id: 7,
            sequence: 1,
            base_version: 21,
            action: DocumentHistoryAction::Undo
        }]
    );
}

#[test]
fn a_workspace_language_missing_on_the_client_leaves_the_document_editable() {
    let mut doc = snapshot(3, "猫");
    doc.language = Some("only-in-the-workspace".into());
    let mut docs = Documents::default();
    docs.open(7);
    assert!(docs.receive(DocumentEvent::Snapshot(doc)).notice.is_some());
    docs.execute(7, 12, TextCommand::Append).unwrap();
    docs.input(7, 12, "犬").unwrap();
    assert_eq!(docs.text(7).unwrap().text(), "猫犬");
}
