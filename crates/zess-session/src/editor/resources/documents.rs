use super::*;

pub(super) enum PageEffect {
    Request(HostRequest),
    Document(PageDocument),
}

impl Session {
    fn resource_document(
        &mut self,
        buffer_id: BufferId,
        line: usize,
        fullscreen: bool,
    ) -> PageDocument {
        self.resources.workspace_changed = true;
        if fullscreen {
            self.resources.fullscreen_documents.insert(buffer_id);
        } else if !self
            .projected_client_workspace()
            .buffers
            .contains(&buffer_id)
        {
            let id = self.id();
            self.client_actions.push(zess_protocol::ClientAction {
                id,
                kind: zess_protocol::ClientActionKind::Open { buffer_id },
            });
        }
        PageDocument {
            buffer_id,
            line,
            fullscreen,
        }
    }

    pub(super) fn open_page_worktree_source(
        &mut self,
        path: &str,
        line: usize,
        presentation: SnapshotPresentation,
    ) -> Result<PageDocument> {
        let path = self.plugin_file_path(path)?;
        anyhow::ensure!(
            self.services.borrow().file_system().try_exists(&path)?,
            "Git source file no longer exists"
        );
        let (buffer, fullscreen) = match presentation {
            SnapshotPresentation::Fullscreen => (self.open_file_buffer(&path)?, true),
            SnapshotPresentation::Editor => (self.load_file(&path)?, false),
        };
        Ok(self.resource_document(buffer, line, fullscreen))
    }

    pub(super) fn page_document_request(&mut self, request: HostRequest) -> Result<PageEffect> {
        let (buffer, line, fullscreen) = match request {
            HostRequest::FocusText { buffer_id, line } => {
                anyhow::ensure!(
                    matches!(
                        self.buffers.get(&buffer_id),
                        Some(Buffer::Text(_) | Buffer::Snapshot { .. })
                    ),
                    "source text buffer no longer exists"
                );
                (
                    buffer_id,
                    line,
                    self.resources.fullscreen_documents.contains(&buffer_id),
                )
            }
            HostRequest::OpenFile { path, line } => (self.load_plugin_file(&path)?, line, false),
            HostRequest::FileLayer { path, line } => {
                let path = self.plugin_file_path(&path)?;
                (self.open_file_buffer(&path)?, line, true)
            }
            HostRequest::OpenWorktreeSource {
                path,
                line,
                presentation,
            } => {
                return self
                    .open_page_worktree_source(&path, line, presentation)
                    .map(PageEffect::Document);
            }
            HostRequest::OpenSnapshot {
                snapshot,
                presentation,
            } => (
                self.snapshot_buffer(snapshot, presentation),
                0,
                matches!(presentation, SnapshotPresentation::Fullscreen),
            ),
            HostRequest::Action { name, argument } if name == "browser.open-source" => {
                let path = argument["path"]
                    .as_str()
                    .context("browser.open-source requires a path")?;
                let line = argument["line"].as_u64().unwrap_or(0) as usize;
                (self.load_plugin_file(path)?, line, false)
            }
            request => return Ok(PageEffect::Request(request)),
        };
        Ok(PageEffect::Document(
            self.resource_document(buffer, line, fullscreen),
        ))
    }

    pub(crate) fn close_page_document(&mut self, id: BufferId, force: bool) -> Result<()> {
        anyhow::ensure!(
            self.resources.fullscreen_documents.contains(&id),
            "buffer is not a fullscreen document"
        );
        anyhow::ensure!(
            force || !matches!(self.buffers.get(&id), Some(Buffer::Text(text)) if text.modified()),
            "unsaved buffer; :w or :q!"
        );
        self.resources.workspace_changed = true;
        self.resources.fullscreen_documents.remove(&id);
        self.remove_file_watch(id);
        self.buffers.remove(&id);
        Ok(())
    }

    pub(super) fn publish_source_navigation(
        &mut self,
        id: u64,
        generation: u64,
        result: Result<PageDocument>,
    ) {
        let mut output = PluginOutput::default();
        let document = match result {
            Ok(document) => Some(document),
            Err(error) => {
                output.requests.push(HostRequest::Message {
                    text: format!("{error:#}"),
                });
                None
            }
        };
        self.resources.updates.push(ResourceUpdate {
            cache: Default::default(),
            kind: ResourceUpdateKind::SourceNavigation,
            subscription_id: id,
            generation,
            document_scoped: self.resources.active[&id].text_source.is_some(),
            document,
            patch_revision: None,
            metadata: None,
            output,
        });
    }

    pub(super) fn poll_page_sources(&mut self) {
        let mut completed = Vec::new();
        {
            let mut services = self.services.borrow_mut();
            for (&id, page) in &mut self.resources.active {
                let Some(pending) = &page.commit_source else {
                    continue;
                };
                let result = match services.poll_query(pending.handle) {
                    Ok(zess_services::QueryPoll::Pending) => continue,
                    Ok(zess_services::QueryPoll::Ready(result)) => {
                        result.map_err(anyhow::Error::msg)
                    }
                    Err(error) => Err(error),
                };
                completed.push((id, page.commit_source.take().unwrap(), result));
            }
        }
        for (id, pending, result) in completed {
            let snapshot = result.and_then(|result| match result {
                zess_services::QueryResult::Snapshot { snapshot } => Ok(snapshot),
                zess_services::QueryResult::Unsupported { message } => {
                    Err(anyhow::Error::msg(message))
                }
                _ => bail!("historical file query returned another kind of result"),
            });
            let document = snapshot.map(|snapshot| {
                let presentation = if pending.fullscreen {
                    SnapshotPresentation::Fullscreen
                } else {
                    SnapshotPresentation::Editor
                };
                let buffer = self.snapshot_buffer(snapshot, presentation);
                self.resource_document(buffer, pending.line, pending.fullscreen)
            });
            self.publish_source_navigation(id, pending.generation, document);
        }
    }
}
