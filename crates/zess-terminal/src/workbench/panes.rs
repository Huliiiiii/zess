use zess_ui::MouseKind;

use super::*;

#[cfg(test)]
mod tests;

impl Workbench {
    pub(super) fn active_pane(&self) -> Option<DocumentPane> {
        self.panes.as_ref().map(DocumentPanes::active)
    }

    fn set_page_focus(&mut self, previous: u64) {
        let active = self.active_pane().unwrap();
        if previous != active.view_id {
            let next = self
                .other_pages
                .remove(&active.view_id)
                .unwrap_or_else(|| self.pages.sibling());
            let mut previous_pages = std::mem::replace(&mut self.pages, next);
            previous_pages.set_focused(false);
            self.other_pages.insert(previous, previous_pages);
            self.pages.set_focused(true);
            self.pages.set_source_buffer(Some(active.buffer_id));
            self.page_count.clear();
            self.clipboard = None;
        }
    }

    pub(super) fn focus_pane(&mut self, view_id: u64) {
        let Some(panes) = &mut self.panes else {
            return;
        };
        let previous = panes.active().view_id;
        if panes.focus(view_id) {
            self.set_page_focus(previous);
        }
    }

    pub(super) fn pane_rectangles(
        &self,
        width: u16,
        height: u16,
    ) -> Vec<(DocumentPane, PreviewArea)> {
        let area = document_area(width, height);
        if self.page_document.is_some() {
            return self
                .active_pane()
                .map(|pane| (pane, area))
                .into_iter()
                .collect();
        }
        self.panes
            .as_ref()
            .map(|panes| panes.rectangles(area))
            .unwrap_or_default()
    }

    pub(super) fn focus_pane_by_command(&mut self, name: &str) {
        let panes = self.panes.as_mut().unwrap();
        let previous = panes.active().view_id;
        if name == "focus-next" {
            panes.focus_next();
        } else {
            let direction = match name {
                "focus-left" => Direction::Left,
                "focus-right" => Direction::Right,
                "focus-up" => Direction::Up,
                "focus-down" => Direction::Down,
                _ => unreachable!(),
            };
            panes.focus_direction(
                direction,
                document_area(self.width, self.editor.source_height(self.height)),
            );
        }
        self.set_page_focus(previous);
    }

    pub(super) fn split_pane(&mut self, axis: Axis, state: &SessionSnapshot) -> Result<()> {
        let mut panes = if let Some(panes) = &self.panes {
            panes.clone()
        } else {
            let buffer = state
                .workspace
                .current
                .filter(|buffer| is_document(state, *buffer))
                .context("no document is available beneath the page")?;
            let view = host_view(state).map_or(1, |view| view.view_id);
            DocumentPanes::new(view, buffer)
        };
        let next = if self.pages.subscription().is_some() {
            self.pages.split()?
        } else {
            self.pages.sibling()
        };
        let (source, target) = panes.split(axis);
        if let Some(position) = self.documents.view_state(source.buffer_id, source.view_id) {
            self.documents
                .restore_view(target.buffer_id, target.view_id, &position)?;
        }
        let mut previous = std::mem::replace(&mut self.pages, next);
        previous.set_focused(false);
        self.other_pages.insert(source.view_id, previous);
        self.panes = Some(panes);
        self.pages.set_source_buffer(Some(target.buffer_id));
        self.page_count.clear();
        Ok(())
    }

    pub(super) fn remove_pane_view(&mut self, view_id: u64) {
        self.documents.remove_view(view_id);
        self.positions.retain(|(_, view), _| *view != view_id);
        self.initial_lines.retain(|(_, view), _| *view != view_id);
        if matches!(self.prompt, Some(PromptTarget::Text(TextTarget::Document(pane), _)) if pane.view_id == view_id)
        {
            self.prompt = None;
            self.editor.close_external_prompt();
        }
        if matches!(self.clipboard, Some((TextTarget::Document(pane), _)) if pane.view_id == view_id)
        {
            self.clipboard = None;
        }
        if self.hits.drag.is_some_and(|pane| pane.view_id == view_id) {
            self.hits.drag = None;
        }
        if self.page_drag == Some(view_id) {
            self.page_drag = None;
        }
    }

    pub(super) fn close_pane(&mut self, view_id: u64) -> bool {
        let Some(panes) = &mut self.panes else {
            return false;
        };
        let previous = panes.active().view_id;
        if !panes.close(view_id) {
            return false;
        }
        self.set_page_focus(previous);
        if let Some(pages) = self.other_pages.remove(&view_id) {
            pages.discard();
        }
        self.remove_pane_view(view_id);
        true
    }

    pub(crate) fn resize_pages(&mut self, width: u16, height: u16) {
        let active = self.active_pane();
        if active.is_none() {
            self.pages.resize(width, height);
            return;
        }
        for (pane, area) in self.pane_rectangles(width, height) {
            if let Some(pages) = self.pages_mut(pane.view_id) {
                pages.resize(area.width as u16, area.height as u16 + 1);
            }
        }
    }

    pub(super) fn pages_mut(&mut self, view_id: u64) -> Option<&mut Pages> {
        if self
            .active_pane()
            .is_some_and(|pane| pane.view_id == view_id)
        {
            Some(&mut self.pages)
        } else {
            self.other_pages.get_mut(&view_id)
        }
    }

    pub(super) fn page_mouse(
        &mut self,
        event: &MouseEvent,
        scroll_lines: usize,
    ) -> Option<PageMouseHit> {
        let dragging = matches!(event.kind, MouseKind::Drag | MouseKind::Release);
        if dragging && self.hits.drag.is_some() {
            return None;
        }
        let height = self.editor.source_height(self.height);
        let areas = self.pane_rectangles(self.width, height);
        if areas.is_empty() {
            return self.pages.mouse(event, scroll_lines);
        }
        let x = usize::from(event.column);
        let y = usize::from(event.row);
        let hit = areas.into_iter().find(|(pane, area)| {
            if dragging && self.page_drag.is_some() {
                self.page_drag == Some(pane.view_id)
            } else {
                area.contains(x, y)
            }
        });
        if event.kind == MouseKind::Release {
            self.page_drag = None;
        }
        let (pane, area) = hit?;
        let pressing = matches!(event.kind, MouseKind::Press | MouseKind::DoubleClick);
        if pressing
            && self.active_pane() != Some(pane)
            && (self.prompt.is_some() || self.draft.is_some())
        {
            return Some(PageMouseHit::Handled);
        }
        let local = MouseEvent {
            kind: event.kind,
            column: x.saturating_sub(area.x).min(area.width.saturating_sub(1)) as u16,
            row: y.saturating_sub(area.y).min(area.height.saturating_sub(1)) as u16,
        };
        let hit = self.pages_mut(pane.view_id)?.mouse(&local, scroll_lines)?;
        if pressing {
            self.focus_pane(pane.view_id);
            self.page_drag = Some(pane.view_id);
        }
        Some(hit)
    }
}
