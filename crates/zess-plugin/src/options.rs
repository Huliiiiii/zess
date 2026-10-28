use std::time::Duration;

pub const DEFAULT_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorOptions {
    pub mouse: bool,
    pub scroll_lines: usize,
    pub preview_debounce: Duration,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            mouse: true,
            scroll_lines: 3,
            preview_debounce: DEFAULT_PREVIEW_DEBOUNCE,
        }
    }
}
