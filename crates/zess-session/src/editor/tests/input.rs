use std::fmt;

use super::*;

pub(super) enum InputEvent {
    Key(String),
    Resize { width: u16, height: u16 },
}

impl fmt::Display for InputEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(key) => write!(f, "key {key:?}"),
            Self::Resize { width, height } => write!(f, "resize {width} {height}"),
        }
    }
}

pub(super) struct TextState {
    pub source: String,
    pub mode: TextMode,
    pub cursors: Vec<(usize, usize, usize, usize)>,
}

pub(super) struct Input<'a> {
    pub session: &'a mut Session,
    events: Vec<InputEvent>,
}

impl<'a> Input<'a> {
    pub fn new(session: &'a mut Session) -> Self {
        Self {
            session,
            events: Vec::new(),
        }
    }

    pub fn apply(&mut self, event: InputEvent) {
        self.events.push(event);
        match self.events.last().unwrap() {
            InputEvent::Key(key) => self.session.input(key),
            InputEvent::Resize { width, height } => self.session.resize(*width, *height),
        }
    }

    pub fn keys(&mut self, keys: &[&str]) {
        for key in keys {
            self.apply(InputEvent::Key((*key).into()));
        }
    }

    pub fn command(&mut self, command: &str) {
        self.keys(&[":"]);
        for ch in command.chars() {
            self.apply(InputEvent::Key(ch.to_string()));
        }
        self.keys(&["Enter"]);
    }

    pub fn trace(&self) -> String {
        self.events
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn text_state(&mut self) -> TextState {
        let view = self.session.focus;
        let buffer = self.session.views[&view].location.buffer;
        let text = self
            .session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap();
        TextState {
            source: text.text(),
            mode: text.view(view).text_mode(),
            cursors: text
                .selections(view)
                .into_iter()
                .map(|cursor| {
                    (
                        cursor.anchor_row,
                        cursor.anchor_column,
                        cursor.row,
                        cursor.column,
                    )
                })
                .collect(),
        }
    }

    pub fn assert_state(&mut self, expected: TextState) {
        let actual = self.text_state();
        assert_eq!(
            (actual.source, actual.mode, actual.cursors),
            (expected.source, expected.mode, expected.cursors),
            "{}",
            self.trace()
        );
    }
}
