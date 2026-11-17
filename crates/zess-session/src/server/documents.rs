use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use anyhow::Result;
use zess_protocol::{
    ServerMessage,
    document::{DocumentEvent, DocumentRequest},
};
use zess_text::TextBuffer;

use super::{Connection, Session};

#[derive(PartialEq, Eq)]
pub(super) struct DocumentState {
    version: u64,
    language: Option<String>,
    path: Option<std::path::PathBuf>,
}

impl DocumentState {
    fn from_buffer(buffer: &TextBuffer) -> Self {
        Self {
            version: buffer.document_version(),
            language: buffer.language_id().map(str::to_owned),
            path: buffer.path().map(ToOwned::to_owned),
        }
    }

    fn matches(&self, buffer: &TextBuffer) -> bool {
        self.version == buffer.document_version()
            && self.language.as_deref() == buffer.language_id()
            && self.path.as_deref() == buffer.path()
    }
}

impl Connection {
    fn send_document(&mut self, event: DocumentEvent) -> Result<()> {
        for part in event.into_wire_events() {
            self.send(&ServerMessage::Document(part))?;
        }
        Ok(())
    }

    pub(super) fn publish_documents(&mut self, session: &mut Session) -> Result<()> {
        let mut changed = Vec::new();
        self.subscribed_documents.retain(|&id, known| {
            let Some(buffer) = session.document(id) else {
                return false;
            };
            if !known.matches(buffer) {
                *known = DocumentState::from_buffer(buffer);
                changed.push(id);
            }
            true
        });
        for buffer in changed {
            self.send_document(DocumentEvent::Snapshot(
                session.document_snapshot(buffer).unwrap(),
            ))?;
        }
        Ok(())
    }
}

enum Receipt {
    Applied { version: u64, modified: bool },
    Saved { version: u64 },
    Conflict,
    Failed(String),
}

impl Receipt {
    fn event(&self, buffer_id: u64, sequence: u64, session: &mut Session) -> DocumentEvent {
        match self {
            Self::Applied { version, modified } => DocumentEvent::Applied {
                buffer_id,
                sequence,
                version: *version,
                modified: *modified,
            },
            Self::Saved { version } => DocumentEvent::Saved {
                buffer_id,
                sequence,
                version: *version,
            },
            Self::Conflict => match session.document_snapshot(buffer_id) {
                Some(current) => DocumentEvent::Conflict {
                    buffer_id,
                    sequence,
                    current,
                },
                None => DocumentEvent::Failed {
                    buffer_id,
                    sequence,
                    message: format!("document {buffer_id} is no longer open"),
                },
            },
            Self::Failed(message) => DocumentEvent::Failed {
                buffer_id,
                sequence,
                message: message.clone(),
            },
        }
    }
}

struct DocumentReceipt {
    sequence: u64,
    request_hash: u64,
    outcome: Receipt,
}

// The client sends the next edit/save for a document only after receiving the
// preceding receipt. Keep its watermark and outcome across connection loss.
#[derive(Default)]
pub(super) struct DocumentOperations {
    clients: HashMap<String, HashMap<u64, DocumentReceipt>>,
}

impl DocumentOperations {
    pub(super) fn request(
        &mut self,
        session: &mut Session,
        connection: &mut Connection,
        request: DocumentRequest,
    ) -> Result<()> {
        let (buffer_id, sequence, version) = match &request {
            DocumentRequest::Read { buffer_id } => {
                self.clients.retain(|_, receipts| {
                    receipts.retain(|&id, _| session.document(id).is_some());
                    !receipts.is_empty()
                });
                let event = if let Some(buffer) = session.document(*buffer_id) {
                    connection
                        .subscribed_documents
                        .insert(*buffer_id, DocumentState::from_buffer(buffer));
                    DocumentEvent::Snapshot(session.document_snapshot(*buffer_id).unwrap())
                } else {
                    DocumentEvent::Failed {
                        buffer_id: *buffer_id,
                        sequence: 0,
                        message: format!("document {buffer_id} is no longer open"),
                    }
                };
                return connection.send_document(event);
            }
            DocumentRequest::Edit {
                buffer_id,
                sequence,
                base_version,
                ..
            }
            | DocumentRequest::History {
                buffer_id,
                sequence,
                base_version,
                ..
            } => (*buffer_id, *sequence, *base_version),
            DocumentRequest::Save {
                buffer_id,
                sequence,
                version,
                ..
            } => (*buffer_id, *sequence, *version),
            DocumentRequest::Chunk { .. } => {
                unreachable!("chunks are assembled by the connection decoder")
            }
        };
        let client = connection.role.client().unwrap();
        let documents = self.clients.entry(client.clone()).or_default();
        let mut hasher = DefaultHasher::new();
        request.hash(&mut hasher);
        let request_hash = hasher.finish();
        if let Some(previous) = documents.get(&buffer_id)
            && sequence == previous.sequence
        {
            let event = if previous.request_hash == request_hash {
                previous.outcome.event(buffer_id, sequence, session)
            } else {
                DocumentEvent::Failed {
                    buffer_id,
                    sequence,
                    message: "document sequence already identifies a different operation".into(),
                }
            };
            return connection.send_document(event);
        }
        if sequence == 0
            || documents
                .get(&buffer_id)
                .is_some_and(|previous| sequence < previous.sequence)
        {
            return connection.send_document(DocumentEvent::Failed {
                buffer_id,
                sequence,
                message: "document operation has already been superseded".into(),
            });
        }
        let result = match session.document(buffer_id) {
            None => Receipt::Failed(format!("document {buffer_id} is no longer open")),
            Some(buffer) if buffer.document_version() != version => Receipt::Conflict,
            Some(_) => {
                let outcome = match request {
                    DocumentRequest::Edit {
                        changes, origin, ..
                    } => session
                        .edit_document(buffer_id, &changes, origin.as_ref())
                        .map(|(version, modified)| Receipt::Applied { version, modified }),
                    DocumentRequest::History { action, .. } => session
                        .change_document_history(buffer_id, &action)
                        .map(|(version, modified)| Receipt::Applied { version, modified }),
                    DocumentRequest::Save { path, .. } => session
                        .save_document(buffer_id, version, path.as_deref())
                        .map(|version| Receipt::Saved { version }),
                    _ => unreachable!(),
                };
                outcome.unwrap_or_else(|error| Receipt::Failed(format!("{error:#}")))
            }
        };
        if let Receipt::Saved { version } = &result
            && let Some(known) = connection.subscribed_documents.get_mut(&buffer_id)
        {
            known.version = *version;
        }
        if matches!(result, Receipt::Applied { .. } | Receipt::Conflict)
            && let Some(buffer) = session.document(buffer_id)
        {
            connection
                .subscribed_documents
                .insert(buffer_id, DocumentState::from_buffer(buffer));
        }
        let event = result.event(buffer_id, sequence, session);
        documents.insert(
            buffer_id,
            DocumentReceipt {
                sequence,
                request_hash,
                outcome: result,
            },
        );
        connection.send_document(event)
    }
}
