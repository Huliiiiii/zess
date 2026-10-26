use super::*;

impl Session {
    pub(super) fn plugin_text_source(
        &self,
        kind: &str,
        source: BufferId,
    ) -> Result<Option<BufferId>> {
        let fields = self
            .scripts
            .as_ref()
            .map(|runtime| runtime.context_fields(kind, "init"))
            .unwrap_or_default();
        if !fields.iter().any(|field| field.starts_with("text.")) {
            return Ok(None);
        }
        let text = match self.buffers.get(&source) {
            Some(Buffer::Text(text) | Buffer::Snapshot { text, .. }) => text,
            _ => bail!("buffer requires a source text buffer"),
        };
        if fields.iter().any(|field| field == "text.symbols") && !text.has_outline() {
            bail!("source text language does not support an outline");
        }
        Ok(Some(source))
    }

    pub(super) fn context(&mut self, id: ViewId, fields: &[String]) -> PluginContext {
        let text = match &self.buffers[&self.views[&id].location.buffer] {
            Buffer::Data { text_source, .. } => *text_source,
            _ => None,
        }
        .and_then(|source| match self.buffers.get_mut(&source) {
            Some(Buffer::Text(text) | Buffer::Snapshot { text, .. }) => Some(TextContext {
                buffer_id: source,
                path: text.path().map(|path| path.to_string_lossy().into_owned()),
                version: text.version(),
                symbols: if fields.iter().any(|field| field == "text.symbols") {
                    text.outline()
                } else {
                    Vec::new()
                },
            }),
            _ => None,
        });
        PluginContext {
            workspace: self.workspace.to_string_lossy().into_owned(),
            text,
            filesystem_revision: if fields.iter().any(|field| field == "fs.revision") {
                Some(self.services.borrow().fs_revision())
            } else {
                None
            },
        }
    }
}
