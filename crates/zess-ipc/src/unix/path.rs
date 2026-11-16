use std::{
    fs, io,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use xxhash_rust::xxh3::xxh3_64;

use crate::{ServerId, Stream};

#[derive(PartialEq, Eq)]
pub(crate) struct SocketIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SocketPath(PathBuf);

impl From<PathBuf> for SocketPath {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}

impl SocketPath {
    pub fn server_id(&self) -> Option<ServerId> {
        self.0.file_stem()?.to_str()?.parse().ok()
    }

    pub fn is_present(&self) -> io::Result<bool> {
        match fs::symlink_metadata(&self.0) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn identity(&self) -> io::Result<SocketIdentity> {
        let metadata = fs::symlink_metadata(&self.0)?;
        Ok(SocketIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(crate) fn remove_if_same(&self, identity: &SocketIdentity) -> io::Result<()> {
        match self.identity() {
            Ok(current) if current == *identity => fs::remove_file(&self.0),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn remove_stale(&self) -> Result<()> {
        let metadata = match fs::symlink_metadata(&self.0) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_socket() {
            bail!("socket path is occupied: {}", self.display());
        }
        match Stream::connect(self) {
            Ok(_) => bail!("a server is already listening at {}", self.display()),
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                fs::remove_file(&self.0).context("remove stale server socket")
            }
            Err(error) => Err(error).context("check existing server socket"),
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn display(&self) -> std::path::Display<'_> {
        self.0.display()
    }

    pub fn log_path(&self) -> PathBuf {
        self.0.with_extension("log")
    }

    pub fn prepare_parent(&self) -> Result<()> {
        if let Some(parent) = self.0.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    pub fn for_workspace(workspace: &Path) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .context("open workspace directory")?;
        let directory = directory();
        ensure_private_directory(directory.parent().expect("runtime directory has a parent"))?;
        ensure_private_directory(&directory)?;
        let id = ServerId(xxh3_64(workspace.as_os_str().as_bytes()));
        Ok(Self::in_directory(id, &directory))
    }

    pub fn by_id(id: ServerId) -> Self {
        Self::in_directory(id, &directory())
    }

    pub fn discover() -> Result<Vec<Self>> {
        let entries = match fs::read_dir(directory()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("read server runtime directory"),
        };
        let mut sockets = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_socket()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "sock")
            {
                sockets.push(Self(entry.path()));
            }
        }
        sockets.sort();
        Ok(sockets)
    }

    fn in_directory(id: ServerId, directory: &Path) -> Self {
        Self(directory.join(format!("{id}.sock")))
    }
}

fn directory() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        runtime.join("zess/run")
    } else {
        // SAFETY: geteuid takes no arguments and has no memory safety preconditions.
        let uid = unsafe { libc::geteuid() };
        std::env::temp_dir().join(format!("zess-{uid}/run"))
    }
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    const DIRECTORY_MODE: u32 = 0o700;

    match fs::DirBuilder::new().mode(DIRECTORY_MODE).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    }
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid takes no arguments and has no memory safety preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != uid {
        bail!(
            "runtime directory must be a directory owned by the current user: {}",
            path.display()
        );
    }
    if metadata.mode() & 0o777 != DIRECTORY_MODE {
        fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))?;
    }
    Ok(())
}
