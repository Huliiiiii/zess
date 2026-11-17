use super::*;
use zess_protocol::workspace_view::WorkspaceDraft;
use zess_text::TextBuffer;
use zess_ui::{CursorShape, Frame, MouseKind};

pub(super) const VIEW: u64 = 1;

pub(super) struct Draft {
    purpose: String,
    pub(super) text: TextBuffer,
    inline: bool,
    origin_y: usize,
    positions: Vec<Vec<usize>>,
    dragging: bool,
}

impl Draft {
    pub(super) fn snapshot(&self) -> WorkspaceDraft {
        WorkspaceDraft {
            purpose: self.purpose.clone(),
            text: self.text.text(),
            inline: self.inline,
            position: self.text.view(VIEW).workspace_state(),
        }
    }

    pub(super) fn mode(&self) -> &str {
        self.text.view(VIEW).mode()
    }
}

impl Workbench {
    pub(super) fn open_layer(
        &mut self,
        purpose: String,
        value: String,
        inline: bool,
    ) -> Result<()> {
        ensure!(self.draft.is_none(), "view already has a layer");
        let mut text = self.documents.draft(&value);
        text.execute(VIEW, TextCommand::Insert)?;
        self.pages.focus_changed();
        self.editor.reset_input();
        self.draft = Some(Draft {
            purpose,
            text,
            inline,
            origin_y: 0,
            positions: Vec::new(),
            dragging: false,
        });
        Ok(())
    }

    pub(super) fn restore_draft(&mut self, saved: &WorkspaceDraft) -> Result<()> {
        self.open_layer(saved.purpose.clone(), saved.text.clone(), saved.inline)?;
        let position = &saved.position;
        self.draft.as_mut().unwrap().text.restore_view(
            VIEW,
            &position.ranges,
            position.primary,
            &position.mode,
            position.scroll,
            position.horizontal_scroll,
        )
    }

    pub(super) fn layer_command(&mut self, name: &str, effects: &mut Vec<Effect>) -> Result<bool> {
        let Some(draft) = &mut self.draft else {
            return Ok(false);
        };
        if let Some(command) = TextCommand::parse(name) {
            let effect = draft.text.execute(VIEW, command)?;
            self.text_effect(TextTarget::Draft, effect, effects);
            return Ok(true);
        }
        if matches!(name, "quit" | "quit!" | "write" | "write-quit") {
            if let Some(action) =
                self.pages
                    .submit_input(&draft.purpose, None, &draft.text.text(), name == "quit!")
                && let CommandAction::PagePrompt {
                    subscription_id,
                    purpose,
                    ..
                } = &action
            {
                self.submitted_prompt = Some((*subscription_id, purpose.clone()));
                effects.push(Effect::Command(action));
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn layer_pending(&mut self, key: &str, effects: &mut Vec<Effect>) -> Result<bool> {
        let Some(draft) = self
            .draft
            .as_mut()
            .filter(|draft| draft.text.has_pending_input(VIEW))
        else {
            return Ok(false);
        };
        let effect = draft.text.input_text(VIEW, key)?;
        self.text_effect(TextTarget::Draft, effect, effects);
        Ok(true)
    }

    pub(super) fn layer_input(&mut self, key: &str, effects: &mut Vec<Effect>) -> Result<bool> {
        let Some(draft) = self.draft.as_mut() else {
            return Ok(false);
        };
        if draft.mode() == "insert" {
            let effect = draft.text.input_text(VIEW, key)?;
            self.text_effect(TextTarget::Draft, effect, effects);
        }
        Ok(true)
    }

    pub(super) fn layer_paste(&mut self, text: &str, effects: &mut Vec<Effect>) -> Result<bool> {
        let Some(draft) = self.draft.as_mut() else {
            return Ok(false);
        };
        let effect = draft.text.paste_text(VIEW, text)?;
        self.text_effect(TextTarget::Draft, effect, effects);
        Ok(true)
    }

    pub(super) fn layer_mouse(&mut self, event: &MouseEvent, scroll_lines: usize) -> bool {
        let Some(draft) = self.draft.as_mut() else {
            return false;
        };
        let line = usize::from(event.row).checked_sub(draft.origin_y);
        match event.kind {
            MouseKind::Release => draft.dragging = false,
            MouseKind::Drag if !draft.dragging => {}
            MouseKind::ScrollUp => draft.text.scroll_view(VIEW, 0, -(scroll_lines as isize)),
            MouseKind::ScrollDown => draft.text.scroll_view(VIEW, 0, scroll_lines as isize),
            _ => {
                if let Some(row) = line.and_then(|row| draft.positions.get(row))
                    && let Some(pos) = row.get(usize::from(event.column)).or(row.last())
                {
                    draft
                        .text
                        .select_at(VIEW, *pos, event.kind == MouseKind::Drag);
                    draft.dragging = true;
                }
            }
        }
        true
    }

    pub(super) fn render_layer(&mut self, frame: &mut Frame) {
        let Some(draft) = &mut self.draft else { return };
        let height = usize::from(frame.height.saturating_sub(1));
        let rows = if draft.inline {
            draft.text.line_count().min(height)
        } else {
            height
        };
        let rendered = draft
            .text
            .render(VIEW, usize::from(frame.width), rows, true);
        draft.positions = rendered.hit_positions;
        draft.origin_y = height.saturating_sub(rows);
        frame.rows.resize_with(height, Default::default);
        frame.rows.truncate(draft.origin_y);
        frame.rows.extend(rendered.rows);
        frame.cursor = rendered.cursor.map(|(x, y)| (x, y + draft.origin_y as u16));
        frame.cursor_shape = CursorShape::for_mode(draft.mode());
        frame.status = draft.purpose.clone();
    }

    pub(crate) fn page_prompt_result(
        &mut self,
        id: u64,
        purpose: &str,
        outcome: &zess_protocol::CommandOutcome,
    ) {
        if !self
            .submitted_prompt
            .as_ref()
            .is_some_and(|(sub, origin)| *sub == id && origin == purpose)
        {
            return;
        }
        self.submitted_prompt = None;
        if let zess_protocol::CommandOutcome::Failed { message } = outcome {
            self.editor.set_prompt_error(message.clone());
            return;
        }
        self.draft = None;
        self.prompt = None;
        self.editor.reset_input();
    }
}
