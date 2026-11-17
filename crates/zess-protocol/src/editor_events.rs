use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EditorHooks {
    pub input: bool,
    pub selection_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub row: usize,
    pub column: usize,
    pub anchor_row: usize,
    pub anchor_column: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffSelection {
    pub row: Option<usize>,
    pub start_row: Option<usize>,
    pub end_row: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EditorSelection {
    Text {
        cursors: Vec<Cursor>,
    },
    Items {
        selection: zess_plugin::ItemSelection,
    },
    Diff {
        diff_selection: DiffSelection,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventSource {
    View {
        view_id: u64,
        buffer_id: u64,
    },
    Page {
        subscription_id: u64,
        slot_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EditorEvent {
    Input { key: String },
    SelectionChanged { selection: EditorSelection },
}
