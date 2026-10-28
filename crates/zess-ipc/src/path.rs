use std::{
    collections::hash_map::DefaultHasher,
    fmt, fs,
    hash::{Hash, Hasher},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use fnv::FnvHasher;
use xxhash_rust::xxh3::xxh3_64;

use crate::Stream;

pub struct ServerId(u64);

impl fmt::Display for ServerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl FromStr for ServerId {
    type Err = anyhow::Error;

    fn from_str(id: &str) -> Result<Self> {
        if id.len() != 16 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("server ID must be the 16-digit hexadecimal ID from `zess server list`");
        }
        Ok(Self(u64::from_str_radix(id, 16)?))
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SocketPath(PathBuf);

impl From<PathBuf> for SocketPath {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}

impl SocketPath {
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
        let bytes = workspace.as_os_str().as_bytes();
        let socket = Self::in_directory(ServerId(xxh3_64(bytes)), &directory);
        if matches!(fs::symlink_metadata(socket.as_path()), Err(error) if error.kind() == io::ErrorKind::NotFound)
        {
            let mut fnv = FnvHasher::default();
            fnv.write(bytes);
            let mut legacy = DefaultHasher::new();
            workspace.hash(&mut legacy);
            for id in [ServerId(fnv.finish()), ServerId(legacy.finish())] {
                let legacy = Self::in_directory(id, &directory);
                if fs::symlink_metadata(legacy.as_path())
                    .is_ok_and(|metadata| metadata.file_type().is_socket())
                    && Stream::connect(&legacy).is_ok()
                {
                    return Ok(legacy);
                }
            }
        }
        Ok(socket)
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
