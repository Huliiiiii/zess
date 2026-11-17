use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StatusConfig {
    pub layouts: BTreeMap<String, StatusLayout>,
    pub mode_labels: BTreeMap<String, String>,
    pub status_formats: BTreeMap<String, String>,
    pub builtin_statuslines: BTreeSet<String>,
    pub builtin_components: BTreeSet<String>,
    pub view_options: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StatusLayout {
    pub left: Option<Vec<StatusElement>>,
    pub center: Option<Vec<StatusElement>>,
    pub right: Option<Vec<StatusElement>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StatusElement {
    Literal(String),
    Component(String),
    Join {
        separator: String,
        elements: Vec<StatusElement>,
    },
}
