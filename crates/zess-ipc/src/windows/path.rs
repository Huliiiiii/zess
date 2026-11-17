use std::{
    ffi::OsStr,
    fmt, fs,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use xxhash_rust::xxh3::xxh3_64;

use crate::{ServerId, windows::security};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SocketPath(PathBuf, Namespace);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Namespace {
    Session,
    Private,
}

impl From<PathBuf> for SocketPath {
    fn from(path: PathBuf) -> Self {
        Self(path, Namespace::Session)
    }
}

impl SocketPath {
    pub fn private(path: PathBuf) -> Self {
        Self(path, Namespace::Private)
    }

    pub fn server_id(&self) -> Option<ServerId> {
        self.0.file_stem()?.to_str()?.parse().ok()
    }

    pub fn is_present(&self) -> std::io::Result<bool> {
        security::is_present(self)
    }

    pub fn remove_stale(&self) -> Result<()> {
        Ok(())
    }

    /// A logical endpoint name, suitable for passing to `--socket`.
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn display(&self) -> std::path::Display<'_> {
        self.0.display()
    }

    pub fn log_path(&self) -> PathBuf {
        directory().join(format!("{}.log", self.id()))
    }

    pub fn prepare_parent(&self) -> Result<()> {
        fs::create_dir_all(directory()).context("create server log directory")
    }

    pub fn for_workspace(workspace: &Path) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .context("open workspace directory")?;
        Ok(Self::by_id(ServerId(hash(workspace.as_os_str()))))
    }

    pub fn by_id(id: ServerId) -> Self {
        Self(PathBuf::from(format!("{id}.pipe")), Namespace::Session)
    }

    pub fn discover() -> Result<Vec<Self>> {
        security::discover()
    }

    pub(crate) fn id(&self) -> ServerId {
        self.0
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.strip_suffix(".pipe"))
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| ServerId(hash(self.0.as_os_str())))
    }

    pub(crate) fn native_name(&self) -> Result<Vec<u16>> {
        let namespace = match self.1 {
            Namespace::Session => "zess",
            Namespace::Private => "zess-private",
        };
        Ok(format!(
            r"\\.\pipe\{}-{}-{}",
            namespace,
            security::current_user_sid()?,
            self.id()
        )
        .encode_utf16()
        .chain(Some(0))
        .collect())
    }
}

impl fmt::Debug for SocketPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

fn hash(value: &OsStr) -> u64 {
    let bytes = value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    xxh3_64(&bytes)
}

fn directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("zess/run")
}
