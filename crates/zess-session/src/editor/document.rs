use anyhow::{Context, Result, bail, ensure};
use zess_plugin::BufferId;
use zess_protocol::document::{
    DocumentChange, DocumentHistoryAction, DocumentOrigin, DocumentSnapshot,
};
use zess_text::TextBuffer;

use super::{Buffer, Session};

impl Session {
    pub(crate) fn document(&self, buffer: BufferId) -> Option<&TextBuffer> {
        match self.buffers.get(&buffer)? {
            Buffer::Text(text) | Buffer::Snapshot { text, .. } => Some(text),
            _ => None,
        }
    }

    fn editable_document(&mut self, buffer: BufferId) -> Result<&mut TextBuffer> {
        let Some(Buffer::Text(text)) = self.buffers.get_mut(&buffer) else {
            bail!("buffer {buffer} is not an editable text buffer");
        };
        Ok(text)
    }

    pub(crate) fn document_snapshot(&mut self, buffer_id: BufferId) -> Option<DocumentSnapshot> {
        let text = match self.buffers.get_mut(&buffer_id)? {
            Buffer::Text(text) | Buffer::Snapshot { text, .. } => text,
            _ => return None,
        };
        Some(DocumentSnapshot {
            buffer_id,
            version: text.document_version(),
            modified: text.modified(),
            language: text.language_id().map(str::to_owned),
            text: text.text(),
            history: text.replica_history(),
            readonly: text.readonly(),
            path: text.path().map(|p| p.to_string_lossy().into_owned()),
        })
    }

    pub(crate) fn edit_document(
        &mut self,
        buffer: BufferId,
        changes: &[DocumentChange],
        origin: Option<&DocumentOrigin>,
    ) -> Result<(u64, bool)> {
        let document = self.editable_document(buffer)?;
        document.apply_document_changes(changes, origin)?;
        Ok((document.document_version(), document.modified()))
    }

    pub(crate) fn change_document_history(
        &mut self,
        buffer: BufferId,
        action: &DocumentHistoryAction,
    ) -> Result<(u64, bool)> {
        let text = self.editable_document(buffer)?;
        text.apply_document_history(action)?;
        Ok((text.document_version(), text.modified()))
    }

    pub(super) fn set_document_language(&mut self, buffer: BufferId, argument: &str) -> Result<()> {
        let text = self
            .buffers
            .get_mut(&buffer)
            .and_then(Buffer::text)
            .context("command requires a text buffer")?;
        if argument.is_empty() {
            self.message = text.language_id().unwrap_or("text").to_owned();
        } else {
            text.set_language((argument != "text").then_some(argument))?;
        }
        Ok(())
    }

    pub(super) fn reload_document(&mut self, buffer: BufferId) -> Result<()> {
        let filesystem = self.services.borrow().file_system();
        let text = self.editable_document(buffer)?;
        let path = text.path().context("buffer has no file name")?;
        let (target, contents) = filesystem
            .read_text_file(path)
            .with_context(|| format!("read {}", path.display()))?;
        text.reload_contents(target, &contents);
        self.rebind_file_watch(buffer)?;
        self.services.borrow_mut().invalidate_fs();
        self.message = "File reloaded".into();
        self.refresh_context_plugins()
    }

    pub(crate) fn save_document(
        &mut self,
        buffer: BufferId,
        version: u64,
        path: Option<&str>,
    ) -> Result<u64> {
        let path = path.map(|p| self.workspace.join(p));
        let text = self.editable_document(buffer)?;
        ensure!(
            text.document_version() == version,
            "buffer version changed; refresh before saving"
        );
        text.save(path.as_deref())?;
        let saved = text.document_version();
        self.services.borrow_mut().invalidate_fs();
        if let Err(error) = self.rebind_file_watch(buffer) {
            self.message = format!("buffer saved; file watch: {error:#}");
        }
        Ok(saved)
    }
}
