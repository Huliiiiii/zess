use super::*;

#[derive(Default)]
pub(super) struct History {
    pub(super) root_kind: Option<String>,
    stack: Vec<ViewLocation>,
    pages: BTreeMap<(String, String), Vec<ViewLocation>>,
    pub(super) plugin: Option<PluginNavigation>,
}

impl History {
    pub(super) fn replace_buffer(&mut self, buffer: BufferId, replacement: BufferId) {
        self.stack.retain(|location| location.buffer != buffer);
        for locations in self.pages.values_mut() {
            locations.retain(|location| location.buffer != buffer);
        }
        if let Some(plugin) = &mut self.plugin {
            if plugin.origin.buffer == buffer {
                plugin.origin = ViewLocation::new(replacement);
            }
            plugin.buffers.remove(&buffer);
        }
    }

    pub(super) fn references_buffer(&self, buffer: BufferId) -> bool {
        self.stack.iter().any(|view| view.buffer == buffer)
            || self
                .pages
                .values()
                .flatten()
                .any(|view| view.buffer == buffer)
            || self.plugin.as_ref().is_some_and(|plugin| {
                plugin.origin.buffer == buffer || plugin.buffers.contains(&buffer)
            })
    }
}

pub(super) struct PluginNavigation {
    pub origin: ViewLocation,
    origin_root_kind: Option<String>,
    pub buffers: HashSet<BufferId>,
}

impl Session {
    pub(super) fn open_plugin_navigation(
        &mut self,
        id: ViewId,
        kind: &str,
        argument: Value,
        text_source: Option<BufferId>,
    ) -> Result<()> {
        self.ensure_focus_allowed(id)?;
        if !matches!(self.views.layer_content(id), Some(LayerContent::File)) {
            self.ensure_buffer_switch_allowed(id)?;
        }
        self.require_buffer_kind(kind)?;
        self.close_plugin_navigation(self.views.page_id(id), false)?;
        let origin_root_kind = self.views.page(id).navigation.root_kind.clone();
        self.leave_page(self.views.page_id(id))?;
        let buffer = self.add_buffer(Buffer::Data {
            kind: kind.into(),
            modified: false,
            text_source,
        });
        let view = self.views.get_mut(&id).unwrap();
        let origin = view.replace_location(ViewLocation::new(buffer));
        *self.views.navigation_mut(id) = History {
            root_kind: Some(kind.into()),
            plugin: Some(PluginNavigation {
                origin,
                origin_root_kind,
                buffers: HashSet::from([buffer]),
            }),
            ..History::default()
        };
        self.dispatch(id, "init", argument)
    }

    pub(super) fn close_plugin_navigation(&mut self, page: PageId, force: bool) -> Result<bool> {
        let id = self.views.main(page);
        let Some(plugin) = self.views.page(id).navigation.plugin.as_ref() else {
            return Ok(false);
        };
        if !force
            && plugin.buffers.iter().any(|buffer| {
                matches!(
                    self.buffers.get(buffer),
                    Some(Buffer::Data { modified: true, .. })
                )
            })
        {
            bail!("unsaved browser buffers; :w or :q!");
        }
        self.leave_page(self.views.page_id(id))?;
        let history = std::mem::take(self.views.navigation_mut(id));
        let plugin = history.plugin.unwrap();
        self.views.navigation_mut(id).root_kind = plugin.origin_root_kind;
        self.views
            .get_mut(&id)
            .unwrap()
            .replace_location(plugin.origin);
        for buffer in plugin.buffers {
            if let Some(runtime) = &mut self.scripts {
                runtime.remove_buffer_view(buffer, id);
            }
            if self.views.references_buffer(buffer) {
                continue;
            }
            self.remove_file_watch(buffer);
            self.buffers.remove(&buffer);
            if let Some(runtime) = &mut self.scripts {
                runtime.remove_buffer(buffer);
            }
        }
        self.remember_text();
        self.dispatch(id, "view_entered", Value::Null)?;
        Ok(true)
    }

    pub(super) fn navigate(&mut self, id: ViewId, navigation: Navigation) -> Result<()> {
        self.ensure_focus_allowed(id)?;
        if !matches!(self.views.layer_content(id), Some(LayerContent::File)) {
            self.ensure_buffer_switch_allowed(id)?;
        }
        let source = self.views[&id].location.buffer;
        let source = match &self.buffers[&source] {
            Buffer::Data {
                text_source: Some(source),
                ..
            } => *source,
            _ => source,
        };
        let text_source = match &navigation {
            Navigation::Push { kind, .. } | Navigation::Page { kind, .. } => {
                self.require_buffer_kind(kind)?;
                self.plugin_text_source(kind, source)?
            }
            Navigation::Back => {
                if self.views.page(id).navigation.stack.is_empty() {
                    return Ok(());
                }
                None
            }
        };
        self.cancel_input_context()?;
        self.dispatch(id, "view_leaving", Value::Null)?;
        let push = matches!(&navigation, Navigation::Push { .. });
        let mut history = std::mem::take(self.views.navigation_mut(id));
        let (replacement, argument) = match navigation {
            Navigation::Push { kind, argument } => {
                let buffer = self.add_buffer(Buffer::Data {
                    kind,
                    modified: false,
                    text_source,
                });
                if let Some(plugin) = &mut history.plugin {
                    plugin.buffers.insert(buffer);
                }
                (Some(ViewLocation::new(buffer)), argument)
            }
            Navigation::Back => (history.stack.pop(), Value::Null),
            Navigation::Page {
                group,
                current,
                page,
                kind,
                argument,
            } => {
                let mut incoming = history
                    .pages
                    .remove(&(group.clone(), page))
                    .unwrap_or_default();
                let next = if let Some(next) = incoming.pop() {
                    next
                } else {
                    let buffer = self.add_buffer(Buffer::Data {
                        kind,
                        modified: false,
                        text_source,
                    });
                    if let Some(plugin) = &mut history.plugin {
                        plugin.buffers.insert(buffer);
                    }
                    ViewLocation::new(buffer)
                };
                let old = self.views.get_mut(&id).unwrap().replace_location(next);
                history.stack.push(old);
                let outgoing = std::mem::replace(&mut history.stack, incoming);
                history.pages.insert((group, current), outgoing);
                *self.views.navigation_mut(id) = history;
                self.dispatch(id, "view_entered", argument)?;
                return Ok(());
            }
        };
        if let Some(next) = replacement {
            let old = self.views.get_mut(&id).unwrap().replace_location(next);
            // Push creates a new location. Back has already removed its destination.
            if push {
                history.stack.push(old);
            }
            *self.views.navigation_mut(id) = history;
            self.dispatch(id, "view_entered", argument)?;
        } else {
            *self.views.navigation_mut(id) = history;
        }
        Ok(())
    }

    fn require_buffer_kind(&self, kind: &str) -> Result<()> {
        if !self
            .scripts
            .as_ref()
            .is_some_and(|runtime| runtime.has_buffer_kind(kind))
        {
            bail!("unknown buffer kind: {kind}");
        }
        Ok(())
    }

    pub(super) fn open_snapshot(
        &mut self,
        source: ViewId,
        snapshot: FileSnapshot,
        presentation: SnapshotPresentation,
    ) -> Result<()> {
        if matches!(presentation, SnapshotPresentation::Fullscreen)
            && self.views.child(source).is_some()
        {
            bail!("view already has a layer");
        }
        self.focus_view(source)?;
        if matches!(presentation, SnapshotPresentation::Editor) {
            self.ensure_buffer_switch_allowed(source)?;
        }
        self.cancel_input_context()?;
        let source = self.focus;
        if matches!(presentation, SnapshotPresentation::Fullscreen) {
            anyhow::ensure!(
                self.views.child(source).is_none(),
                "view already has a layer"
            );
        } else {
            self.ensure_buffer_switch_allowed(source)?;
        }
        let buffer = if matches!(presentation, SnapshotPresentation::Editor) {
            self.buffers.iter().find_map(|(&id, buffer)| match buffer {
                Buffer::Snapshot { id: saved, .. } if saved == &snapshot.id => Some(id),
                _ => None,
            })
        } else {
            None
        };
        let buffer = buffer.unwrap_or_else(|| {
            let mut text = TextBuffer::new_with_path_and_capabilities(
                &snapshot.text,
                &self.absolute(Path::new(&snapshot.id.path)),
                self.services.borrow().file_system(),
                self.syntax_provider.clone(),
            );
            text.set_readonly(true);
            self.add_buffer(Buffer::Snapshot {
                id: snapshot.id,
                text,
            })
        });
        match presentation {
            SnapshotPresentation::Editor => self.switch_buffer(buffer)?,
            SnapshotPresentation::Fullscreen => {
                let child = self.create_page(buffer);
                self.views.attach_layer(
                    child,
                    super::LayerAttachment {
                        parent: source,
                        content: LayerContent::File,
                        presentation: LayerPresentation::Fullscreen,
                    },
                );
                self.focus_view(child)?;
            }
        }
        Ok(())
    }
}
