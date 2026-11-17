use super::*;
use zess_protocol::editor_events::{EditorEvent, EditorSelection, EventSource};

#[cfg(test)]
mod tests;

impl Workbench {
    fn event_source(&self, state: &SessionSnapshot) -> Option<EventSource> {
        if let Some(subscription_id) = self.pages.subscription() {
            return Some(EventSource::Page {
                subscription_id,
                slot_id: self.pages.event_slot().map(str::to_owned),
            });
        }
        if let Some(pane) = self.active_document(state) {
            return Some(EventSource::View {
                buffer_id: pane.buffer_id,
                view_id: pane.view_id,
            });
        }
        let view = host_view(state)?;
        Some(EventSource::View {
            buffer_id: view.buffer_id,
            view_id: view.view_id,
        })
    }

    fn event_selection(&self, state: &SessionSnapshot) -> Option<(EventSource, EditorSelection)> {
        let source = self.event_source(state)?;
        let selection = if let Some(draft) = &self.draft {
            EditorSelection::Text {
                cursors: draft.text.selections(layer::VIEW),
            }
        } else {
            match &source {
                EventSource::Page { .. } => self.pages.event_selection()?,
                EventSource::View { buffer_id, view_id } => {
                    if let Some(text) = self.documents.text(*buffer_id) {
                        EditorSelection::Text {
                            cursors: text.selections(*view_id),
                        }
                    } else if let ViewSelection::Items { ids, primary } =
                        &host_view(state)?.selection
                    {
                        EditorSelection::Items {
                            selection: zess_plugin::ItemSelection {
                                primary: primary.clone(),
                                selected: ids.clone(),
                                anchor: primary.clone(),
                            },
                        }
                    } else {
                        return None;
                    }
                }
            }
        };
        Some((source, selection))
    }

    fn with_editor_events(
        &mut self,
        state: &SessionSnapshot,
        key: Option<&str>,
        apply: impl FnOnce(&mut Self) -> Result<Vec<Effect>>,
    ) -> Result<Vec<Effect>> {
        let input = key.filter(|_| state.editor_hooks.input);
        if input.is_none() && !state.editor_hooks.selection_changed {
            return apply(self);
        }
        let before = state
            .editor_hooks
            .selection_changed
            .then(|| self.event_selection(state))
            .flatten();
        let mut effects = match apply(self) {
            Ok(effects) => effects,
            Err(error) => vec![Effect::Notice(format!("{error:#}"))],
        };
        if let Some(key) = input
            && let Some(source) = self.event_source(state)
            && !matches!(source, EventSource::View { buffer_id, .. } if self.documents.is_reloading(buffer_id))
        {
            effects.push(Effect::Command(CommandAction::Observe {
                source,
                event: EditorEvent::Input { key: key.into() },
            }));
        }
        if state.editor_hooks.selection_changed
            && let Some(after) = self.event_selection(state)
            && before.as_ref() != Some(&after)
        {
            let (source, selection) = after;
            effects.push(Effect::Command(CommandAction::Observe {
                source,
                event: EditorEvent::SelectionChanged { selection },
            }));
        }
        Ok(effects)
    }

    pub(crate) fn key(
        &mut self,
        key: &str,
        state: &SessionSnapshot,
        width: u16,
    ) -> Result<Vec<Effect>> {
        self.with_editor_events(state, Some(key), |this| this.apply_key(key, state, width))
    }

    pub(crate) fn mouse(
        &mut self,
        event: &MouseEvent,
        state: &SessionSnapshot,
        width: u16,
        height: u16,
    ) -> Result<Vec<Effect>> {
        self.with_editor_events(state, None, |this| {
            this.apply_mouse(event, state, width, height)
        })
    }

    pub(crate) fn paste(&mut self, text: &str, state: &SessionSnapshot) -> Result<Vec<Effect>> {
        self.with_editor_events(state, None, |this| this.apply_paste(text, state))
    }

    pub(crate) fn paste_clipboard(
        &mut self,
        id: u64,
        result: Result<String>,
        state: &SessionSnapshot,
    ) -> Result<Vec<Effect>> {
        self.with_editor_events(state, None, |this| {
            this.apply_paste_clipboard(id, result, state)
        })
    }
}
