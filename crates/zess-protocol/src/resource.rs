use serde::{Deserialize, Serialize};
use serde_json::Value;
use zess_plugin::{
    FileTreeDirectory, FileTreeKey, ItemNavigation, ItemSelection, PluginOutput, PreviewLayout,
    PreviewPane,
};
use zess_ui::DisplayRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PageId {
    pub context_epoch: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageCacheKey {
    Preview {
        primary: String,
        width: usize,
        height: usize,
    },
    FileTreeDirectory(FileTreeKey),
    Rows,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PageCacheEntry {
    pub page_id: PageId,
    pub key: PageCacheKey,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCacheInvalidation {
    Changed,
    Evicted,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PreviewProjection {
    pub panes: Vec<PreviewPane>,
    pub layout: PreviewLayout,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CachedPreview {
    pub entry: PageCacheEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<PreviewProjection>,
    pub focus: bool,
    /// Position of the preview among the page's ordered effects.
    pub request_index: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CachedDirectory {
    pub entry: PageCacheEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<FileTreeDirectory>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CachedRows {
    pub entry: PageCacheEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<DisplayRow>>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ResourceCacheUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<CachedPreview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<CachedDirectory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<CachedRows>,
}

impl ResourceCacheUpdate {
    fn is_empty(&self) -> bool {
        self.preview.is_none() && self.directories.is_empty() && self.rows.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageSplit {
    pub source: PageId,
    pub model: PageId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageSource {
    pub kind: String,
    pub argument: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    Plain,
    Glob,
    Regex,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageFilter {
    pub kind: FilterKind,
    pub text: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PageInput {
    pub generation: u64,
    pub selection: ItemSelection,
    pub register: Option<char>,
    pub mode: String,
    pub filter: Option<PageFilter>,
    pub source_buffer: Option<u64>,
    pub width: usize,
    pub active: bool,
    pub preview_width: usize,
    pub preview_height: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PageCommandInput {
    pub generation: u64,
    pub selection: ItemSelection,
    pub register: Option<char>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceEvent {
    Select {
        navigation: ItemNavigation,
    },
    FilterChanged,
    Register,
    OpenSource {
        path: String,
        line: usize,
        fullscreen: bool,
    },
    CommitSource {
        commit: String,
        path: String,
        line: usize,
        fullscreen: bool,
    },
    MiniBufferChanged {
        purpose: String,
        text: String,
        query_kind: FilterKind,
    },
    ModeChanged,
    Resize,
    Refresh,
}

impl ResourceEvent {
    pub fn supersedes_source_navigation(&self) -> bool {
        matches!(
            self,
            Self::Select { .. }
                | Self::FilterChanged
                | Self::MiniBufferChanged { .. }
                | Self::OpenSource { .. }
                | Self::CommitSource { .. }
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceRequest {
    /// The inventory of this connection's retained page projections.
    CacheInterest {
        entries: Vec<PageCacheEntry>,
    },
    Subscribe {
        subscription_id: u64,
        page_id: PageId,
        clone_from: Option<PageSplit>,
        source: PageSource,
        input: PageInput,
    },
    Update {
        subscription_id: u64,
        input: PageInput,
        event: ResourceEvent,
    },
    Discard {
        page_id: PageId,
    },
    Release {
        subscription_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PagePatch {
    pub revision: u64,
    pub side: zess_git::DiffSide,
    pub target: zess_git::DiffTarget,
    pub rows: Vec<std::ops::RangeInclusive<zess_git::patch::PatchRow>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageDocument {
    pub buffer_id: u64,
    pub line: usize,
    pub fullscreen: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageMetadata {
    pub view_id: u64,
    pub buffer_id: u64,
    pub options: serde_json::Value,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceUpdateKind {
    #[default]
    Page,
    SourceNavigation,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResourceUpdate {
    #[serde(default)]
    pub kind: ResourceUpdateKind,
    pub document_scoped: bool,
    pub subscription_id: u64,
    /// The generation of the page projection or the initiating source navigation.
    pub generation: u64,
    pub output: PluginOutput,
    #[serde(default, skip_serializing_if = "ResourceCacheUpdate::is_empty")]
    pub cache: ResourceCacheUpdate,
    pub document: Option<PageDocument>,
    pub patch_revision: Option<u64>,
    pub metadata: Option<PageMetadata>,
}

impl std::fmt::Debug for ResourceUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceUpdate")
            .field("subscription_id", &self.subscription_id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
