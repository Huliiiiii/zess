use serde::{Deserialize, Serialize};

pub mod patch;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Staged,
    Unstaged,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct DiffTarget {
    pub path: String,
    pub original_path: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSource {
    Changes { side: DiffSide },
    Commit { commit: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileDiff {
    pub target: DiffTarget,
    pub raw: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitDiff {
    pub source: DiffSource,
    pub files: Vec<FileDiff>,
}
