use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zess_plugin::FileSnapshot;

use crate::DirectoryEntry;
use zess_git::{
    Commit, CommitFile, DiffSide, DiffSource, DiffTarget, GitDecoration, GitDiff, GitEntry,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "argument")]
pub enum QueryRequest {
    #[serde(rename = "text.read-file")]
    ReadFile { path: std::path::PathBuf },
    #[serde(rename = "fs.list")]
    List {
        #[serde(default)]
        path: String,
    },
    #[serde(rename = "fs.preview")]
    Preview {
        path: String,
        #[serde(default)]
        show_ignored: bool,
    },
    #[serde(rename = "git.status")]
    Status {},
    #[serde(rename = "git.decorations")]
    GitDecorations {},
    #[serde(rename = "git.diff")]
    Diff {
        path: String,
        original_path: Option<String>,
        side: Option<DiffSide>,
    },
    #[serde(rename = "git.diff-files")]
    DiffFiles {
        source: DiffSource,
        files: Vec<DiffTarget>,
        #[serde(default)]
        render: zess_git::DiffRenderOptions,
    },
    #[serde(rename = "git.log")]
    Log {},
    #[serde(rename = "git.commit-files")]
    CommitFiles { commit: String },
    #[serde(rename = "git.commit-diff")]
    CommitDiff {
        commit: String,
        path: String,
        original_path: Option<String>,
    },
    #[serde(rename = "git.blob")]
    Blob { commit: String, path: String },
}

impl QueryRequest {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::ReadFile { .. } => "text.read-file",
            Self::List { .. } => "fs.list",
            Self::Preview { .. } => "fs.preview",
            Self::Status {} => "git.status",
            Self::GitDecorations {} => "git.decorations",
            Self::Diff { .. } => "git.diff",
            Self::DiffFiles { .. } => "git.diff-files",
            Self::Log {} => "git.log",
            Self::CommitFiles { .. } => "git.commit-files",
            Self::CommitDiff { .. } => "git.commit-diff",
            Self::Blob { .. } => "git.blob",
        }
    }

    /// Arguments for the dynamic host-call and watch notification boundary.
    pub fn argument(&self) -> Value {
        serde_json::to_value(self).expect("query fields are serializable")["argument"].clone()
    }

    pub(crate) fn from_call(operation: &str, argument: &Value) -> Result<Self> {
        match operation {
            "text.read-file" => anyhow::bail!("file reads require the native editor API"),
            "fs.list" => Ok(Self::List {
                path: argument["path"].as_str().unwrap_or("").into(),
            }),
            "fs.preview" => Ok(Self::Preview {
                path: crate::string(argument, "path")?.into(),
                show_ignored: argument["show_ignored"].as_bool().unwrap_or(false),
            }),
            "git.status" => Ok(Self::Status {}),
            "git.decorations" => Ok(Self::GitDecorations {}),
            "git.diff" => Ok(Self::Diff {
                path: crate::string(argument, "path")?.into(),
                original_path: argument["original_path"].as_str().map(str::to_owned),
                side: serde_json::from_value(argument["side"].clone())?,
            }),
            _ => Ok(serde_json::from_value(
                json!({"operation":operation,"argument":argument}),
            )?),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryResult {
    Directory {
        path: String,
        entries: Vec<DirectoryEntry>,
    },
    Text {
        path: String,
        text: String,
        truncated: bool,
    },
    Diff {
        path: String,
        text: String,
    },
    GitDiff {
        diff: GitDiff,
    },
    Status {
        entries: Vec<GitEntry>,
    },
    GitDecorations {
        entries: Vec<GitDecoration>,
    },
    Log {
        commits: Vec<Commit>,
    },
    CommitFiles {
        files: Vec<CommitFile>,
    },
    Snapshot {
        snapshot: FileSnapshot,
    },
    Unsupported {
        message: String,
    },
}

impl QueryResult {
    pub(crate) fn into_value(self, handle: u64) -> Value {
        let mut value = serde_json::to_value(self).expect("query results are serializable");
        value["handle"] = handle.into();
        value
    }
}

pub enum QueryPoll {
    Pending,
    Ready(Result<QueryResult, String>),
}

impl QueryPoll {
    pub(crate) fn into_value(self, handle: u64) -> Value {
        match self {
            Self::Pending => json!({"status":"pending"}),
            Self::Ready(result) => {
                json!({"status":"ready","result":match result {Ok(result)=>result.into_value(handle),Err(error)=>json!({"error":error})}})
            }
        }
    }
}
