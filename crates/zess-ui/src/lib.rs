use serde::{Deserialize, Serialize};
use serde_json::Value;

mod style;
pub use style::{StyleAttribute, StyleColor, TextStyle};

mod ansi;
pub use ansi::{AnsiText, DiffLineKind, DiffLineMetadata};

mod item_jump;
pub use item_jump::{ItemJump, JumpInput};

mod rows;
pub use rows::{align_row, clip_spans, fit, fitting_prefix, item_label, skip_columns, spans_width};

mod columns;
pub use columns::{ColumnAlignment, RowColumns, RowLayout};

mod viewport;
pub use viewport::{
    HalfPageScroll, ScrollDirection, ScrollMode, scroll_limit_with_margin, scroll_with_margin,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DisplayRow {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub cells: Vec<DisplayCell>,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayCell {
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayTab {
    pub label: String,
    pub command: String,
    pub active: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Display {
    #[serde(default)]
    pub tabs: Vec<DisplayTab>,
    #[serde(default)]
    pub loading: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub rows: Vec<DisplayRow>,
    /// Equal revisions identify unchanged rows within the same content source.
    #[serde(default)]
    pub rows_revision: Option<u64>,
    #[serde(default)]
    pub column_alignment: ColumnAlignment,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub status: Value,
    #[serde(default)]
    pub status_data: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Block,
    Bar,
    Underline,
}

impl CursorShape {
    pub fn for_mode(mode: &str) -> Self {
        match mode {
            "insert" => Self::Bar,
            "select" => Self::Underline,
            _ => Self::Block,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseKind {
    Press,
    DoubleClick,
    Drag,
    Release,
    ScrollUp,
    ScrollDown,
}

#[derive(Serialize, Deserialize)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    #[serde(default)]
    pub styles: std::collections::HashMap<String, TextStyle>,
    #[serde(default)]
    pub mouse_enabled: bool,
    pub width: u16,
    pub height: u16,
    pub rows: Vec<FrameRow>,
    pub cursor: Option<(u16, u16)>,
    #[serde(default)]
    pub cursor_shape: CursorShape,
    pub status: String,
    pub prompt: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameRow {
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Span {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_style: Option<String>,
    pub text: String,
    #[serde(default)]
    pub style: String,
}
