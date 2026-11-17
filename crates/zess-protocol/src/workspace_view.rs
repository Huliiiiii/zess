use serde::{Deserialize, Serialize};

use crate::{TextRange, resource::PageId};

pub mod layout;
pub mod pages;

pub use layout::{Axis, Direction, DocumentPane, DocumentPanes};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentViewState {
    pub ranges: Vec<TextRange>,
    pub primary: usize,
    pub mode: String,
    pub scroll: usize,
    pub horizontal_scroll: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentView {
    pub buffer_id: u64,
    pub view_id: u64,
    pub state: DocumentViewState,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentLayer {
    pub buffer_id: u64,
    pub previous: DocumentPane,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceDraft {
    pub purpose: String,
    pub text: String,
    pub inline: bool,
    pub position: DocumentViewState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanePages {
    pub view_id: u64,
    pub pages: pages::PageWorkspace,
}

/// Client positions retained by the running Session for the next attachment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceView {
    pub documents: Vec<DocumentView>,
    pub text_panes: Option<DocumentPanes>,
    pub pages: Vec<PanePages>,
    pub page_watermarks: Vec<PageId>,
    pub page_document: Option<DocumentLayer>,
    pub draft: Option<WorkspaceDraft>,
}
