use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use crate::query::{QueryRequest, QueryResult};
use anyhow::{Context, Result, bail};
use zess_platform::WalkOptions;

use crate::jobs::Cancellation;
use crate::workspace::Workspace;

const PREVIEW_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub kind: DirectoryEntryKind,
    pub size: u64,
    pub ignored: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectoryEntryKind {
    Directory,
    Symlink,
    File,
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileOperation {
    Create {
        path: String,
        #[serde(default)]
        directory: bool,
    },
    Rename {
        path: String,
        to: String,
    },
    Delete {
        path: String,
    },
}

impl FileOperation {
    pub fn path(&self) -> &str {
        match self {
            Self::Create { path, .. } | Self::Rename { path, .. } | Self::Delete { path } => path,
        }
    }
}

#[derive(serde::Serialize)]
pub struct AppliedOperations {
    pub applied: Vec<FileOperation>,
    pub remaining: Vec<FileOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(crate) enum Request {
    List,
    Preview { show_ignored: bool },
}

impl Request {
    pub(crate) fn from_query(query: &QueryRequest) -> Result<(&str, Self)> {
        match query {
            QueryRequest::List { path } => Ok((path, Self::List)),
            QueryRequest::Preview { path, show_ignored } => Ok((
                path,
                Self::Preview {
                    show_ignored: *show_ignored,
                },
            )),
            _ => bail!("not a filesystem query"),
        }
    }
}

pub(crate) struct Entry {
    name: String,
    kind: DirectoryEntryKind,
    size: u64,
    ignored: bool,
}

pub(crate) enum Snapshot {
    Directory(Vec<Entry>),
    Text { text: String, truncated: bool },
    Unsupported(&'static str),
}

impl Snapshot {
    pub(crate) fn response(&self, relative: &str, request: Request) -> Result<QueryResult> {
        let directory_path = relative.ends_with(['/', std::path::MAIN_SEPARATOR])
            || relative
                .strip_suffix('.')
                .is_some_and(|parent| parent.ends_with(['/', std::path::MAIN_SEPARATOR]));
        if directory_path && !matches!(self, Self::Directory(_)) {
            bail!("not a directory: {relative}");
        }
        if matches!(request, Request::List) && !matches!(self, Self::Directory(_)) {
            bail!("cannot list a file: {relative}");
        }

        Ok(match self {
            Self::Directory(entries) => {
                let entries = entries
                    .iter()
                    .filter(|entry| match request {
                        Request::List => true,
                        Request::Preview { show_ignored } => show_ignored || !entry.ignored,
                    })
                    .map(|entry| DirectoryEntry {
                        path: Path::new(relative)
                            .join(&entry.name)
                            .to_string_lossy()
                            .into_owned(),
                        name: entry.name.clone(),
                        kind: entry.kind,
                        size: entry.size,
                        ignored: entry.ignored,
                    })
                    .collect();
                QueryResult::Directory {
                    path: relative.into(),
                    entries,
                }
            }
            Self::Text { text, truncated } => QueryResult::Text {
                path: relative.into(),
                text: text.clone(),
                truncated: *truncated,
            },
            Self::Unsupported(message) => QueryResult::Unsupported {
                message: (*message).into(),
            },
        })
    }
}

impl Workspace {
    pub(crate) fn read_fs(&self, relative: &str, cancellation: &Cancellation) -> Result<Snapshot> {
        let snapshot = self.read_fs_inner(relative, cancellation)?;
        cancellation.check()?;
        #[cfg(test)]
        if let Some(hook) = &self.read_hook {
            hook(relative);
        }
        Ok(snapshot)
    }

    fn read_fs_inner(&self, relative: &str, cancellation: &Cancellation) -> Result<Snapshot> {
        let path = self.path(relative)?;
        let metadata = self
            .filesystem
            .metadata(&path)
            .context("cannot inspect preview path")?;

        if metadata.is_dir() {
            return self.read_directory(relative, cancellation);
        }

        let mut file = self
            .filesystem
            .open_read(&path)
            .context("cannot open preview file")?;
        let mut bytes = Vec::with_capacity(PREVIEW_LIMIT.min(metadata.len() as usize));
        let mut chunk = [0; 16 * 1024];

        while bytes.len() < PREVIEW_LIMIT {
            cancellation.check()?;

            let limit = chunk.len().min(PREVIEW_LIMIT - bytes.len());
            let read = file
                .read(&mut chunk[..limit])
                .context("cannot read preview file")?;

            if read == 0 {
                break;
            }

            bytes.extend_from_slice(&chunk[..read]);
        }

        cancellation.check()?;

        let truncated = metadata.len() > bytes.len() as u64;

        if is_image(relative, &bytes) {
            // TODO: render image previews in the native service.
            return Ok(Snapshot::Unsupported("Image preview is not implemented"));
        }

        if bytes.contains(&0) {
            // TODO: render binary previews in the native service.
            return Ok(Snapshot::Unsupported("Binary preview is not implemented"));
        }

        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(error) if truncated && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])?
            }
            Err(_) => {
                // TODO: render binary previews in the native service.
                return Ok(Snapshot::Unsupported("Binary preview is not implemented"));
            }
        };

        Ok(Snapshot::Text {
            text: text.to_owned(),
            truncated,
        })
    }

    fn read_directory(&self, relative: &str, cancellation: &Cancellation) -> Result<Snapshot> {
        let path = self.path(relative)?;
        let mut visible = HashSet::new();

        cancellation.check()?;

        for path in self
            .filesystem
            .walk_directory(
                &path,
                WalkOptions {
                    max_depth: Some(1),
                    hidden: false,
                    ignore: false,
                    require_git: false,
                },
            )
            .context("cannot read directory ignore rules")?
        {
            cancellation.check()?;
            visible.insert(path);
        }

        let mut entries = Vec::new();

        cancellation.check()?;

        for entry in self
            .filesystem
            .read_dir(&path)
            .context("cannot list directory")?
        {
            cancellation.check()?;

            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("directory contains a filename that is not UTF-8"))?;
            let metadata = self.filesystem.symlink_metadata(&entry.path())?;
            let kind = if metadata.is_dir() {
                DirectoryEntryKind::Directory
            } else if metadata.is_symlink() {
                DirectoryEntryKind::Symlink
            } else {
                DirectoryEntryKind::File
            };
            entries.push(Entry {
                name,
                kind,
                size: metadata.len(),
                ignored: !visible.contains(&entry.path()),
            });
        }

        cancellation.check()?;

        entries.sort_by(|a, b| {
            (b.kind == DirectoryEntryKind::Directory)
                .cmp(&(a.kind == DirectoryEntryKind::Directory))
                .then_with(|| a.name.cmp(&b.name))
        });

        cancellation.check()?;

        Ok(Snapshot::Directory(entries))
    }
}

pub(crate) fn is_image(relative: &str, bytes: &[u8]) -> bool {
    let extension = Path::new(relative)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let known_extension = matches!(
        extension.as_str(),
        "avif"
            | "bmp"
            | "gif"
            | "heic"
            | "heif"
            | "ico"
            | "jpeg"
            | "jpg"
            | "png"
            | "svg"
            | "tif"
            | "tiff"
            | "webp"
    );
    let known_magic = bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"BM")
        || bytes.starts_with(b"II*\0")
        || bytes.starts_with(b"MM\0*")
        || bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
        || bytes.get(4..12) == Some(b"ftypavif")
        || bytes.get(4..12) == Some(b"ftypavis");

    known_extension || known_magic
}
