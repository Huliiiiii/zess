use zess_ui::{MouseContext, MouseEvent, MouseKind};

use super::*;

#[derive(PartialEq)]
pub(super) enum Content {
    Text {
        version: u64,
        positions: Vec<Vec<usize>>,
    },
    Items {
        ids: Vec<String>,
    },
    Diff {
        scroll: usize,
    },
    Rows,
}

#[derive(PartialEq)]
pub(super) struct Region {
    pub target: RenderTarget,
    pub buffer: BufferId,
    pub pane: Rect,
    pub body: Rect,
    pub content: Content,
}

impl Session {
    pub fn invalidate_mouse(&mut self) {
        self.mouse_drag = None;
        self.mouse_regions.clear();
        self.mouse_context.content_revision = self.mouse_context.content_revision.wrapping_add(1);
        self.mouse_context.layout_revision = self.mouse_context.layout_revision.wrapping_add(1);
    }

    pub(super) fn mouse_enabled(&self) -> bool {
        self.scripts
            .as_ref()
            .map(|runtime| runtime.editor_options())
            .unwrap_or_default()
            .mouse
    }
    pub(super) fn record_command_mouse(&mut self) {
        self.command_mouse = self
            .active_prompt()
            .filter(|prompt| matches!(prompt.kind, PromptKind::Command))
            .map(|prompt| {
                self.command_grid(
                    prompt,
                    usize::from(self.width),
                    usize::from(self.height.saturating_sub(1)),
                )
            });
    }
}
