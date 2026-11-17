use hegel::{TestCase, generators as gs};

use super::input::{Input, InputEvent};
use super::*;

struct ScratchEditor<'a, 'session> {
    input: &'a mut Input<'session>,
    width: u16,
    height: u16,
}

#[hegel::state_machine]
impl<'a, 'session> ScratchEditor<'a, 'session> {
    #[rule]
    fn key(&mut self, tc: TestCase) {
        let key = tc.draw(gs::sampled_from(vec![
            "i",
            "2",
            "a",
            "Esc",
            "h",
            "j",
            "k",
            "l",
            "w",
            "b",
            "e",
            "v",
            "x",
            "d",
            "u",
            "C-r",
            "r",
            "f",
            "t",
            "s",
            "/",
            "Backspace",
            "Delete",
            "Enter",
            "Left",
            "Right",
            "Up",
            "Down",
            "界",
            "é",
            "\u{301}",
            "🦀",
            "(",
        ]));
        let editing_pattern = self
            .input
            .session
            .active_prompt()
            .is_some_and(|prompt| matches!(prompt.kind, PromptKind::Text(_)));
        let source = editing_pattern.then(|| self.input.text_state().source);
        let event = InputEvent::Key(key.into());
        tc.note(&event.to_string());
        self.input.apply(event);
        if let Some(source) = source {
            assert_eq!(
                self.input.text_state().source,
                source,
                "{}",
                self.input.trace()
            );
        }
    }

    #[rule]
    fn resize(&mut self, tc: TestCase) {
        let width = tc.draw(gs::integers::<u16>().max_value(120));
        let height = tc.draw(gs::integers::<u16>().max_value(40));
        let before = self.input.text_state();
        let count = self.input.session.command_count().value();
        let event = InputEvent::Resize { width, height };
        tc.note(&event.to_string());
        self.input.apply(event);
        self.width = width.max(10);
        self.height = height.max(3);
        self.input.assert_state(before);
        assert_eq!(self.input.session.command_count().value(), count);
    }

    #[invariant(always_run)]
    fn layout_and_selections_stay_within_their_bounds(&mut self, _tc: TestCase) {
        assert_editor_bounds(self.input, self.width, self.height);
    }
}

fn assert_editor_bounds(input: &mut Input<'_>, width: u16, height: u16) {
    assert!(!input.session.exiting());
    let snapshot = input.session.snapshot();
    snapshot.validate().unwrap();
    for area in snapshot.layout {
        assert!(area.x + area.width <= usize::from(width), "{area:?}");
        assert!(area.y + area.height < usize::from(height), "{area:?}");
    }

    let state = input.text_state();
    let source = state.source;
    let lines = source.split('\n').collect::<Vec<_>>();
    let selections = state.cursors;
    assert!(!selections.is_empty());
    for (anchor_row, anchor_column, row, column) in selections {
        for (row, column) in [(row, column), (anchor_row, anchor_column)] {
            assert!(row < lines.len(), "row {row} outside {source:?}");
            assert!(
                column <= lines[row].chars().count(),
                "column {column} outside line {:?}",
                lines[row]
            );
        }
    }
}

#[hegel::test(derandomize = true)]
fn bounded_input_preserves_editor_bounds_and_recoverable_undo(tc: TestCase) {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let mut input = Input::new(&mut session);
    // Every trace includes a newline, a wide character and a combining mark.
    input.keys(&["i", "a", "Enter", "界", "e", "\u{301}", "Esc"]);
    tc.note(&input.trace());
    let editor = ScratchEditor {
        input: &mut input,
        width: 80,
        height: 24,
    };
    hegel::stateful::machine(editor).steps(48).run(tc.clone());
    let (width, height) = (input.session.width, input.session.height);

    input.keys(&["Esc", "Esc", ",", "g", "g", "g", "h"]);
    let before = input.text_state().source;
    let expected = format!("Ω{before}");
    input.keys(&["i", "Ω", "Esc"]);
    tc.note(&input.trace());
    assert_eq!(input.text_state().source, expected, "{}", input.trace());
    input.keys(&["u"]);
    assert_eq!(input.text_state().source, before, "{}", input.trace());
    input.keys(&["C-r"]);
    assert_eq!(input.text_state().source, expected, "{}", input.trace());
    tc.note(&input.trace());
    assert_editor_bounds(&mut input, width, height);
}
