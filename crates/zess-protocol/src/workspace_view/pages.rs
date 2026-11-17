use serde::{Deserialize, Serialize};
use zess_plugin::ItemSelection;

use crate::resource::{PageFilter, PageId, PageSource, PageSplit};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviewPosition {
    pub slot: String,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PagePosition {
    pub page_id: PageId,
    pub clone_from: Option<PageSplit>,
    pub source: PageSource,
    pub source_buffer: Option<u64>,
    pub selection: ItemSelection,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub mode: String,
    pub filter: Option<PageFilter>,
    pub preview: Option<PreviewPosition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageTabPosition {
    pub parent: Option<PageId>,
    pub group: String,
    pub tab: String,
    pub stack: Vec<PagePosition>,
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageWorkspace {
    pub stack: Vec<PagePosition>,
    pub cached: Vec<PagePosition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inactive_tabs: Vec<PageTabPosition>,
    pub suspended: bool,
}

impl PageWorkspace {
    pub fn positions(&self) -> impl Iterator<Item = &PagePosition> {
        self.stack
            .iter()
            .chain(&self.cached)
            .chain(self.inactive_tabs.iter().flat_map(|tab| &tab.stack))
    }
}
