use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zess_ui::Display;

use crate::{BufferId, FileTree, HostRequest, ViewId};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemSelection {
    #[serde(default)]
    pub primary: Option<String>,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub anchor: Option<String>,
}

impl ItemSelection {
    pub fn reconcile(&mut self, rows: &[zess_ui::DisplayRow]) {
        let visible = rows
            .iter()
            .map(|row| row.id.as_str())
            .filter(|id| !id.is_empty())
            .collect::<std::collections::HashSet<_>>();
        let mut selected = std::collections::HashSet::new();
        self.selected
            .retain(|id| visible.contains(id.as_str()) && selected.insert(id.clone()));
        if !self
            .primary
            .as_deref()
            .is_some_and(|id| visible.contains(id))
        {
            self.primary = rows
                .iter()
                .find(|row| !row.id.is_empty())
                .map(|row| row.id.clone());
        }
        if !self
            .anchor
            .as_deref()
            .is_some_and(|id| visible.contains(id))
        {
            self.anchor.clone_from(&self.primary);
        }
        if let Some(primary) = &self.primary
            && !self.selected.contains(primary)
        {
            self.selected.push(primary.clone());
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemNavigation {
    Step,
    Jump,
}

#[derive(Serialize, Deserialize)]
pub struct SelectionChange {
    pub navigation: Option<ItemNavigation>,
}

#[derive(Clone)]
pub struct PluginInput {
    pub buffer_id: BufferId,
    pub view_id: ViewId,
    pub generation: u64,
    pub preview_debounce: Duration,
    pub preview_area: Option<crate::PreviewArea>,
    pub event: String,
    pub argument: Value,
    pub query: Option<zess_filter::Query>,
    pub options: Value,
    pub mode: String,
    pub selection: ItemSelection,
    pub view_selection: Option<ItemSelection>,
    pub register: Option<char>,
    pub context: PluginContext,
}

impl PluginInput {
    /// The current view selection when an event acts on an earlier selection.
    pub fn view_selection(&self) -> &ItemSelection {
        self.view_selection.as_ref().unwrap_or(&self.selection)
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PluginContext {
    pub workspace: String,
    pub text: Option<TextContext>,
    pub filesystem_revision: Option<u64>,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TextContext {
    pub buffer_id: BufferId,
    pub path: Option<String>,
    pub version: u64,
    pub symbols: Vec<OutlineSymbol>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OutlineSymbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub column: usize,
    pub depth: usize,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PluginOutput {
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub watches: Option<Vec<QueryWatch>>,
    #[serde(default)]
    pub selection: Option<ItemSelection>,
    #[serde(default)]
    pub modified: Option<bool>,
    #[serde(default)]
    pub display: Option<Display>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_tree: Option<FileTree>,
    #[serde(default)]
    pub requests: Vec<HostRequest>,
    #[serde(default)]
    pub pending_operations: Vec<String>,
    #[serde(default)]
    pub completed_operations: Vec<OperationCompletion>,
    /// Error from synchronous dispatch. The host publishes the update before reporting it.
    #[serde(skip)]
    pub command_error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OperationCompletion {
    pub operation_id: String,
    pub result: OperationResult,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OperationResult {
    Completed,
    /// The operation opened the document returned by this plugin output.
    DocumentOpened,
    Yanked {
        values: Vec<String>,
        register: char,
    },
    Failed {
        message: String,
    },
    Cancelled,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryWatch {
    pub operation: String,
    #[serde(default)]
    pub argument: Value,
}
