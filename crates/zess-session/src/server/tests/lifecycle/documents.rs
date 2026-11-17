use zess_protocol::TextRange;
use zess_protocol::document::{
    DocumentChange, DocumentEvent, DocumentHistoryAction, DocumentRequest, DocumentSelection,
};
use zess_text::{TextBuffer, TextCommand};

use super::*;

fn attach(
    server: &Server,
    client: &str,
    files: Vec<String>,
) -> (UnixStream, BufReader<UnixStream>, u64) {
    let (mut stream, mut reader) = server.connect();
    hello(&mut stream, &mut reader);
    framing::write(
        &mut stream,
        &ClientMessage::Attach {
            expected_session: None,
            client_id: client.to_owned(),
            width: 80,
            height: 24,
            files,
        },
    )
    .unwrap();
    assert!(matches!(
        receive(&mut reader),
        ServerMessage::Attached { .. }
    ));
    let ServerMessage::Snapshot { state, .. } = receive(&mut reader) else {
        panic!("expected initial session snapshot")
    };
    let buffer = state
        .views
        .iter()
        .find(|v| Some(v.view_id) == state.focused_view)
        .unwrap()
        .buffer_id;
    (stream, reader, buffer)
}

fn send(stream: &mut UnixStream, request: DocumentRequest) {
    for part in request.into_wire_requests() {
        framing::write(stream, &ClientMessage::Document(part)).unwrap();
    }
}

fn event(reader: &mut BufReader<UnixStream>) -> DocumentEvent {
    let mut decoder = zess_protocol::document::DocumentStream::default();
    loop {
        if let ServerMessage::Document(part) = receive(reader)
            && let Some(event) = decoder.receive(part)
        {
            return event;
        }
    }
}

fn detach(mut stream: UnixStream, mut reader: BufReader<UnixStream>) {
    framing::write(&mut stream, &ClientMessage::Detach).unwrap();
    let mut trailing = String::new();
    reader.read_to_string(&mut trailing).unwrap();
}

#[test]
fn reconnecting_preserves_user_undo_groups_and_saves_only_the_requested_revision() {
    let server = Server::start("keep");
    let path = server.root.join("notes");
    fs::write(&path, "猫").unwrap();
    let (mut stream, mut reader, buffer_id) =
        attach(&server, "editor", vec![path.to_string_lossy().into_owned()]);
    send(&mut stream, DocumentRequest::Read { buffer_id });
    let DocumentEvent::Snapshot(initial) = event(&mut reader) else {
        panic!("expected document")
    };
    assert_eq!(initial.text, "猫");
    let insert = |sequence, base_version, start, value: &str| DocumentRequest::Edit {
        buffer_id,
        sequence,
        base_version,
        origin: None,
        changes: vec![DocumentChange {
            start,
            end: start,
            insert: value.to_owned(),
        }],
    };
    let first = insert(1, initial.version, 1, "犬");
    send(&mut stream, first.clone());
    // Discard the first receipt. A retry on the new connection must recover it.
    detach(stream, reader);

    let (mut stream, mut reader, reattached) = attach(&server, "editor", vec![]);
    assert_eq!(reattached, buffer_id);
    send(&mut stream, first);
    let DocumentEvent::Applied {
        version: v1,
        sequence: 1,
        ..
    } = event(&mut reader)
    else {
        panic!("missing edit receipt")
    };
    let second = insert(2, v1, 2, "🐱");
    send(&mut stream, second);
    let DocumentEvent::Applied { version: v2, .. } = event(&mut reader) else {
        panic!("missing edit receipt")
    };
    let point = |at| DocumentSelection {
        ranges: vec![TextRange {
            anchor: at,
            head: at,
        }],
        primary: 0,
    };
    send(
        &mut stream,
        DocumentRequest::History {
            buffer_id,
            sequence: 3,
            base_version: v2,
            action: DocumentHistoryAction::Commit {
                before: point(1),
                after: point(3),
            },
        },
    );
    let DocumentEvent::Applied {
        version: committed, ..
    } = event(&mut reader)
    else {
        panic!("missing commit receipt")
    };
    assert!(committed > v2);
    send(
        &mut stream,
        DocumentRequest::Save {
            buffer_id,
            sequence: 4,
            version: committed,
            path: None,
        },
    );
    let DocumentEvent::Saved { version: saved, .. } = event(&mut reader) else {
        panic!("missing save receipt")
    };
    assert!(saved > committed);
    send(&mut stream, insert(5, saved, 3, "!"));
    let later = loop {
        if let DocumentEvent::Applied {
            version,
            sequence: 5,
            ..
        } = event(&mut reader)
        {
            break version;
        }
    };
    send(
        &mut stream,
        DocumentRequest::Save {
            buffer_id,
            sequence: 6,
            version: saved,
            path: None,
        },
    );
    assert!(
        matches!(event(&mut reader), DocumentEvent::Conflict{current,..} if current.version==later && current.text=="猫犬🐱!")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "猫犬🐱");
    detach(stream, reader);

    let (mut stream, mut reader, id) = attach(&server, "new terminal", vec![]);
    assert_eq!(id, buffer_id);
    send(&mut stream, DocumentRequest::Read { buffer_id });
    let DocumentEvent::Snapshot(snapshot) = event(&mut reader) else {
        panic!("expected document")
    };
    let mut replica = TextBuffer::new(&snapshot.text);
    replica.restore_history(&snapshot.history).unwrap();
    replica.execute(1, TextCommand::Undo).unwrap();
    assert_eq!(replica.text(), "猫犬🐱");
    assert!(!replica.modified());
    replica.execute(1, TextCommand::Undo).unwrap();
    assert_eq!(replica.text(), "猫");
    replica.execute(1, TextCommand::Redo).unwrap();
    assert_eq!(replica.text(), "猫犬🐱");
}

#[test]
fn replaying_an_edit_preserves_its_receipt_after_another_client_saves() {
    let server = Server::start("keep");
    let path = server.root.join("notes");
    fs::write(&path, "a").unwrap();
    let (mut stream, mut reader, buffer_id) =
        attach(&server, "editor", vec![path.to_string_lossy().into_owned()]);
    send(&mut stream, DocumentRequest::Read { buffer_id });
    let DocumentEvent::Snapshot(initial) = event(&mut reader) else {
        panic!("expected document")
    };
    let edit = DocumentRequest::Edit {
        buffer_id,
        sequence: 1,
        base_version: initial.version,
        changes: vec![DocumentChange {
            start: 1,
            end: 1,
            insert: "b".into(),
        }],
        origin: None,
    };
    send(&mut stream, edit.clone());
    let DocumentEvent::Applied {
        version,
        modified: true,
        ..
    } = event(&mut reader)
    else {
        panic!("expected an unsaved edit")
    };
    detach(stream, reader);

    let (mut stream, mut reader, _) = attach(&server, "another editor", vec![]);
    send(
        &mut stream,
        DocumentRequest::Save {
            buffer_id,
            sequence: 1,
            version,
            path: None,
        },
    );
    let DocumentEvent::Saved { version: saved, .. } = event(&mut reader) else {
        panic!("missing save receipt")
    };
    assert!(saved > version);
    detach(stream, reader);

    let (mut stream, mut reader, _) = attach(&server, "editor", vec![]);
    send(&mut stream, edit);
    assert_eq!(
        event(&mut reader),
        DocumentEvent::Applied {
            buffer_id,
            sequence: 1,
            version,
            modified: true,
        },
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "ab");
}

#[test]
fn subscribed_documents_receive_external_changes_before_a_stale_edit_can_replace_them() {
    let server = Server::start("keep");
    let path = server.root.join("notes");
    fs::write(&path, "before").unwrap();
    let (mut stream, mut reader, buffer_id) = attach(
        &server,
        "subscriber",
        vec![path.to_string_lossy().into_owned()],
    );
    send(&mut stream, DocumentRequest::Read { buffer_id });
    let DocumentEvent::Snapshot(before) = event(&mut reader) else {
        panic!("expected document")
    };
    fs::write(&path, "after").unwrap();
    let DocumentEvent::Snapshot(current) = event(&mut reader) else {
        panic!("expected file notification")
    };
    assert_eq!(current.text, "after");
    send(
        &mut stream,
        DocumentRequest::Edit {
            buffer_id,
            sequence: 1,
            base_version: before.version,
            origin: None,
            changes: vec![DocumentChange {
                start: 0,
                end: 6,
                insert: "local".into(),
            }],
        },
    );
    assert!(
        matches!(event(&mut reader),DocumentEvent::Conflict{current:accepted,..} if accepted==current)
    );
}

#[test]
fn saving_a_scratch_document_as_a_workspace_file_updates_its_path() {
    let server = Server::start("keep");
    let (mut stream, mut reader, buffer_id) = attach(&server, "writer", Vec::new());
    send(&mut stream, DocumentRequest::Read { buffer_id });
    let DocumentEvent::Snapshot(before) = event(&mut reader) else {
        panic!("missing initial document")
    };
    assert_eq!(before.path, None);
    send(
        &mut stream,
        DocumentRequest::Edit {
            buffer_id,
            sequence: 1,
            base_version: before.version,
            origin: None,
            changes: vec![DocumentChange {
                start: 0,
                end: 0,
                insert: "written in the workspace".into(),
            }],
        },
    );
    let DocumentEvent::Applied { version, .. } = event(&mut reader) else {
        panic!("missing edit receipt")
    };
    send(
        &mut stream,
        DocumentRequest::Save {
            buffer_id,
            version,
            sequence: 2,
            path: Some("saved.txt".into()),
        },
    );
    let DocumentEvent::Saved {
        version: saved_version,
        ..
    } = event(&mut reader)
    else {
        panic!("missing save receipt")
    };
    assert!(saved_version > version);
    let DocumentEvent::Snapshot(saved) = event(&mut reader) else {
        panic!("missing renamed document")
    };
    let path = server.root.join("saved.txt");
    assert_eq!(saved.version, saved_version);
    assert_eq!(saved.path.as_deref(), path.to_str());
    assert!(!saved.modified);
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "written in the workspace"
    );

    send(
        &mut stream,
        DocumentRequest::Save {
            buffer_id,
            version: saved_version,
            sequence: 3,
            path: Some("renamed.txt".into()),
        },
    );
    assert_eq!(
        event(&mut reader),
        DocumentEvent::Saved {
            buffer_id,
            sequence: 3,
            version: saved_version
        },
    );
    let DocumentEvent::Snapshot(renamed) = event(&mut reader) else {
        panic!("missing renamed document")
    };
    let path = server.root.join("renamed.txt");
    assert_eq!(renamed.path.as_deref(), path.to_str());
    assert_eq!(renamed.version, saved_version);
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "written in the workspace"
    );
}
