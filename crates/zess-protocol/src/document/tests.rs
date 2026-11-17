use super::*;
use crate::{ClientMessage, ServerMessage};

#[test]
fn large_edits_apply_only_after_the_complete_request_arrives() {
    let text = "😺\0".repeat(MAX_INPUT_BYTES / 8);
    let request = DocumentRequest::Edit {
        buffer_id: 1,
        sequence: 1,
        base_version: 0,
        origin: None,
        changes: vec![DocumentChange {
            start: 0,
            end: 0,
            insert: text,
        }],
    };
    let frames = request.clone().into_wire_requests();
    assert!(frames.len() > 1);
    let mut stream = DocumentStream::default();
    let count = frames.len();
    for (i, frame) in frames.into_iter().enumerate() {
        let json = serde_json::to_vec(&ClientMessage::Document(frame)).unwrap();
        assert!(json.len() < MAX_INPUT_BYTES);
        let ClientMessage::Document(frame) = serde_json::from_slice(&json).unwrap() else {
            panic!("expected document")
        };
        let decoded = stream.request(frame).unwrap();
        if i + 1 == count {
            assert!(decoded.as_ref() == Some(&request))
        } else {
            assert!(decoded.is_none())
        }
    }
}

#[test]
fn snapshots_with_large_undo_histories_fit_the_transport_frames() {
    let text = "\0😸".repeat(MAX_MESSAGE_BYTES / 16);
    let snapshot = DocumentEvent::Snapshot(DocumentSnapshot {
        buffer_id: 3,
        version: 4,
        modified: true,
        readonly: false,
        language: None,
        path: None,
        text: text.clone(),
        history: DocumentHistory {
            root: Some(text),
            ..Default::default()
        },
    });
    let frames = snapshot.clone().into_wire_events();
    assert!(frames.len() > 1);
    let count = frames.len();
    let mut stream = DocumentStream::default();
    for (i, frame) in frames.into_iter().enumerate() {
        let json = serde_json::to_vec(&ServerMessage::Document(frame)).unwrap();
        assert!(json.len() < MAX_MESSAGE_BYTES);
        let ServerMessage::Document(frame) = serde_json::from_slice(&json).unwrap() else {
            panic!("expected document")
        };
        let decoded = stream.receive(frame);
        if i + 1 == count {
            assert!(decoded.as_ref() == Some(&snapshot))
        } else {
            assert!(decoded.is_none())
        }
    }
}
