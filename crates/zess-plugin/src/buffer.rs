use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zess_ui::Display;

use crate::{BufferId, HostRequest, ViewId};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ItemSelection {
    #[serde(default)]
    pub primary: Option<String>,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub anchor: Option<String>,
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
    pub register: Option<char>,
    pub context: PluginContext,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PluginContext {
    pub workspace: String,
    pub text: Option<TextContext>,
    pub filesystem_revision: Option<u64>,
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
    #[serde(default)]
    pub requests: Vec<HostRequest>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryWatch {
    pub operation: String,
    #[serde(default)]
    pub argument: Value,
}
