use serde::{Deserialize, Serialize};
use serde_json::Value;
use zess_ui::DisplayRow;

use crate::BufferId;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostRequest {
    PreviewGroup {
        panes: Vec<PreviewPane>,
        layout: PreviewLayout,
        #[serde(default)]
        focus: bool,
    },
    PreviewPending,
    ConfigurePreview {
        header: String,
        key_scope: Option<String>,
    },
    Navigate {
        navigation: Navigation,
    },
    OpenSnapshot {
        snapshot: FileSnapshot,
        presentation: SnapshotPresentation,
    },
    Action {
        name: String,
        #[serde(default)]
        argument: Value,
    },
    BufferEvent {
        kind: String,
        event: String,
        #[serde(default)]
        argument: Value,
    },
    Schedule {
        after_ms: u64,
    },
    FocusText {
        buffer_id: BufferId,
        #[serde(default)]
        line: usize,
    },
    OpenFile {
        path: String,
        #[serde(default)]
        line: usize,
    },
    OpenWorktreeSource {
        path: String,
        #[serde(default)]
        line: usize,
        presentation: SnapshotPresentation,
    },
    FileLayer {
        path: String,
        #[serde(default)]
        line: usize,
    },
    OpenBuffer {
        kind: String,
        #[serde(default)]
        argument: Value,
    },
    Layer {
        purpose: String,
        text: String,
        #[serde(default)]
        inline: bool,
    },
    MiniBuffer {
        purpose: String,
        text: String,
        #[serde(default)]
        key_scope: Option<String>,
    },
    Preview {
        path: String,
        content: PreviewContent,
        #[serde(default)]
        focus: bool,
    },
    ConfigureView {
        options: Value,
    },
    SetMode {
        mode: String,
    },
    Message {
        text: String,
    },
    Yank {
        values: Vec<String>,
        #[serde(default)]
        register: Option<char>,
    },
    Close,
}

pub fn validate_register_write(register: char) -> anyhow::Result<()> {
    anyhow::ensure!(
        !matches!(register, '#' | '.' | '%'),
        "Register {register} does not support writing"
    );
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "presentation", rename_all = "snake_case")]
pub enum PreviewPane {
    Surface {
        id: String,
        path: String,
        #[serde(default)]
        header: String,
        #[serde(default)]
        loading: bool,
        content: PreviewContent,
    },
    View {
        id: String,
        path: String,
        #[serde(default)]
        header: String,
        #[serde(default)]
        loading: bool,
        content: PreviewContent,
        #[serde(default)]
        key_scope: Option<String>,
    },
}

impl PreviewPane {
    pub fn id(&self) -> &str {
        match self {
            Self::Surface { id, .. } | Self::View { id, .. } => id,
        }
    }

    pub fn loading(&self) -> bool {
        match self {
            Self::Surface { loading, .. } | Self::View { loading, .. } => *loading,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct PreviewArea {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl PreviewArea {
    pub fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub fn split_preview(self) -> (Self, Self) {
        let available = self.width.saturating_sub(1);
        let main_width = available / 2;
        (
            Self {
                width: main_width,
                ..self
            },
            Self {
                x: self.x + (main_width + 1).min(self.width),
                width: available - main_width,
                ..self
            },
        )
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreviewLayout {
    Rows,
    Columns,
    Responsive { min_pane_width: usize },
}

impl PreviewLayout {
    pub fn rectangles(self, rect: PreviewArea, count: usize) -> Vec<PreviewArea> {
        let columns = match self {
            Self::Columns => true,
            Self::Rows => false,
            Self::Responsive { min_pane_width } => {
                rect.width >= min_pane_width.saturating_mul(count) + count.saturating_sub(1)
            }
        };
        let gap = usize::from(columns);
        let extent = if columns { rect.width } else { rect.height };
        let mut available = extent.saturating_sub(gap * count.saturating_sub(1));
        let mut offset = 0;
        (0..count)
            .map(|index| {
                let size = available / (count - index);
                let area = if columns {
                    PreviewArea {
                        x: rect.x + offset.min(extent),
                        width: size,
                        ..rect
                    }
                } else {
                    PreviewArea {
                        y: rect.y + offset.min(extent),
                        height: size,
                        ..rect
                    }
                };
                available -= size;
                offset += size + gap;
                area
            })
            .collect()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreviewContent {
    Text {
        text: String,
    },
    Rows {
        rows: Vec<DisplayRow>,
        #[serde(default)]
        column_alignment: zess_ui::ColumnAlignment,
    },
    Directory {
        rows: Vec<DisplayRow>,
        #[serde(default)]
        column_alignment: zess_ui::ColumnAlignment,
    },
    Diff {
        text: String,
    },
    GitDiff {
        diff: zess_git::GitDiff,
    },
    Unsupported {
        message: String,
    },
    Snapshot {
        snapshot: FileSnapshot,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Navigation {
    Push {
        kind: String,
        argument: Value,
    },
    Back,
    Page {
        group: String,
        current: String,
        page: String,
        kind: String,
        argument: Value,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotPresentation {
    Editor,
    Fullscreen,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotId {
    pub repository: String,
    pub commit: String,
    pub path: String,
    pub blob: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub id: SnapshotId,
    pub text: String,
}
