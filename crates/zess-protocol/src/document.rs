use serde::{Deserialize, Serialize};

use crate::{MAX_INPUT_BYTES, MAX_MESSAGE_BYTES, TextRange};

#[cfg(test)]
mod tests;

/// A replacement in Unicode scalar offsets of the base document.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocumentChange {
    pub start: usize,
    pub end: usize,
    pub insert: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocumentSelection {
    pub ranges: Vec<TextRange>,
    pub primary: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocumentOrigin {
    pub view_id: u64,
    pub selection: DocumentSelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRevision {
    pub changes: Vec<DocumentChange>,
    pub before: DocumentSelection,
    pub after: DocumentSelection,
}

/// The current undo branch in commit order. The root text is omitted for an untouched buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentHistory {
    pub root: Option<String>,
    pub revisions: Vec<HistoryRevision>,
    pub current: usize,
    pub saved: Option<usize>,
    pub pending: Option<HistoryRevision>,
    pub pending_view: Option<u64>,
}

impl Default for DocumentHistory {
    fn default() -> Self {
        Self {
            root: None,
            revisions: Vec::new(),
            current: 0,
            saved: Some(0),
            pending: None,
            pending_view: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSnapshot {
    pub buffer_id: u64,
    /// Identifies the contents, undo history, and saved revision.
    pub version: u64,
    pub modified: bool,
    pub readonly: bool,
    pub language: Option<String>,
    pub path: Option<String>,
    pub text: String,
    pub history: DocumentHistory,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DocumentHistoryAction {
    Commit {
        before: DocumentSelection,
        after: DocumentSelection,
    },
    Undo,
    Redo,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocumentRequest {
    Read {
        buffer_id: u64,
    },
    Edit {
        buffer_id: u64,
        sequence: u64,
        base_version: u64,
        changes: Vec<DocumentChange>,
        origin: Option<DocumentOrigin>,
    },
    History {
        buffer_id: u64,
        sequence: u64,
        base_version: u64,
        action: DocumentHistoryAction,
    },
    Save {
        buffer_id: u64,
        sequence: u64,
        version: u64,
        path: Option<String>,
    },
    Chunk {
        json: String,
        last: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocumentEvent {
    Chunk {
        json: String,
        last: bool,
    },
    Snapshot(DocumentSnapshot),
    Applied {
        buffer_id: u64,
        sequence: u64,
        version: u64,
        modified: bool,
    },
    Saved {
        buffer_id: u64,
        sequence: u64,
        /// The version after the save.
        version: u64,
    },
    Conflict {
        buffer_id: u64,
        sequence: u64,
        current: DocumentSnapshot,
    },
    Failed {
        buffer_id: u64,
        sequence: u64,
        message: String,
    },
}

// JSON can escape each byte as six bytes. Allow space for both message envelopes.
fn split_frames<T: Serialize>(
    value: T,
    maximum: usize,
    chunk: impl Fn(String, bool) -> T,
) -> Vec<T> {
    let json = serde_json::to_string(&value).expect("document messages are serializable");
    if json.len() + 256 <= maximum {
        return vec![value];
    }
    crate::chunks::json_parts(&json, maximum)
        .map(|(part, last)| chunk(part.to_owned(), last))
        .collect()
}

impl DocumentRequest {
    pub fn into_wire_requests(self) -> Vec<Self> {
        if matches!(&self, Self::Edit { changes, origin, .. } if changes.len() < 8 && changes.iter().all(|change| change.insert.len() < 1024) && origin.as_ref().is_none_or(|o| o.selection.ranges.len() < 32))
            || matches!(&self, Self::Read { .. })
        {
            return vec![self];
        }
        split_frames(self, MAX_INPUT_BYTES, |json, last| Self::Chunk {
            json,
            last,
        })
    }
}

impl DocumentEvent {
    pub fn into_wire_events(self) -> Vec<Self> {
        if matches!(&self, Self::Applied { .. } | Self::Saved { .. }) {
            return vec![self];
        }
        split_frames(self, MAX_MESSAGE_BYTES, |json, last| Self::Chunk {
            json,
            last,
        })
    }
}

/// Incomplete document messages belong to one transport connection.
#[derive(Default)]
pub struct DocumentStream(crate::chunks::JsonParts);

impl DocumentStream {
    pub fn request(
        &mut self,
        request: DocumentRequest,
    ) -> serde_json::Result<Option<DocumentRequest>> {
        if let DocumentRequest::Chunk { json, last } = request {
            return self.0.assemble(json, last);
        }
        self.0.clear();
        Ok(Some(request))
    }

    pub fn receive(&mut self, event: DocumentEvent) -> Option<DocumentEvent> {
        if let DocumentEvent::Chunk { json, last } = event {
            return self.0.assemble(json, last).unwrap_or_else(|error| {
                Some(DocumentEvent::Failed {
                    buffer_id: 0,
                    sequence: 0,
                    message: format!("could not decode document: {error}"),
                })
            });
        }
        self.0.clear();
        Some(event)
    }
}
