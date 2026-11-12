use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};

pub enum FileType {
    File,
    Directory,
    Symlink,
    Other,
}

impl FileType {
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File)
    }
    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Directory)
    }
    pub fn is_symlink(&self) -> bool {
        matches!(self, Self::Symlink)
    }
}

pub struct DirectoryEntry {
    pub path: PathBuf,
    pub file_name: OsString,
    pub file_type: FileType,
}

pub struct WalkExcludes {
    pub root: Option<PathBuf>,
    pub global: Option<PathBuf>,
    pub local: Option<PathBuf>,
}

pub struct WalkOptions {
    pub max_depth: Option<usize>,
    pub hidden: bool,
    pub ignore: bool,
    pub require_git: bool,
    pub excludes: Option<WalkExcludes>,
}

impl DirectoryEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn file_name(&self) -> OsString {
        self.file_name.clone()
    }
    pub fn file_type(&self) -> io::Result<&FileType> {
        Ok(&self.file_type)
    }
}

pub trait FileSystem: Send + Sync {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>>;
    fn create_new(&self, path: &Path) -> io::Result<()>;
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
    fn read_text_file(&self, path: &Path) -> io::Result<(PathBuf, String)> {
        let target = self.canonicalize(path)?;
        let text = self.read_to_string(&target)?;
        Ok((target, text))
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Publishes complete contents if the target and bytes still match `previous`.
    /// With no previous file, the destination must be absent. Returns the saved target.
    fn save_file(
        &self,
        path: &Path,
        previous: Option<(&Path, &[u8])>,
        bytes: &[u8],
    ) -> io::Result<PathBuf>;
    fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn metadata(&self, path: &Path) -> io::Result<Metadata>;
    /// Returns the filesystem device for discovery on platforms that expose it.
    fn device_id(&self, path: &Path) -> io::Result<Option<u64>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Ok(Some(self.metadata(path)?.dev()))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(None)
        }
    }
    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirectoryEntry>>;
    fn walk_directory(&self, path: &Path, options: WalkOptions) -> anyhow::Result<Vec<PathBuf>>;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn remove_dir(&self, path: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn copy(&self, from: &Path, to: &Path) -> io::Result<u64>;
    fn read_link(&self, path: &Path) -> io::Result<PathBuf>;
    fn try_exists(&self, path: &Path) -> io::Result<bool> {
        match self.metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Joins a relative path to the canonical workspace root. Resolves the
    /// nearest existing ancestor to check containment and returns the joined path.
    fn workspace_path(&self, root: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            bail!("path must remain relative to the workspace");
        }

        let path = root.join(relative);
        let mut ancestor = path.as_path();
        while !self.try_exists(ancestor)? {
            if self.symlink_metadata(ancestor).is_ok() {
                bail!("broken symbolic link in workspace path");
            }

            ancestor = ancestor.parent().context("invalid workspace path")?;
        }

        if !self.canonicalize(ancestor)?.starts_with(root) {
            bail!("symbolic link leaves the workspace");
        }

        Ok(path)
    }
}

pub struct NativeFileSystem;

impl FileSystem for NativeFileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        open_regular_file(path, fs::OpenOptions::new().read(true))?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(open_regular_file(
            path,
            fs::OpenOptions::new().read(true),
        )?))
    }

    fn create_new(&self, path: &Path) -> io::Result<()> {
        fs::File::create_new(path)?;
        Ok(())
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        fs::write(path, bytes)
    }

    fn save_file(
        &self,
        path: &Path,
        previous: Option<(&Path, &[u8])>,
        bytes: &[u8],
    ) -> io::Result<PathBuf> {
        let target = if let Some((previous_target, _)) = previous {
            let target = existing_save_target(path)?;
            if target != previous_target {
                return Err(save_conflict());
            }
            target
        } else {
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
            })?;
            fs::canonicalize(parent_directory(path))?.join(name)
        };
        let mut builder = tempfile::Builder::new();
        builder.prefix(".zess-save-");
        #[cfg(unix)]
        if previous.is_none() {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o666));
        }
        let mut temporary = builder.tempfile_in(parent_directory(&target))?;
        temporary.write_all(bytes)?;

        if let Some((_, previous)) = previous {
            let permissions = open_regular_file(&target, fs::OpenOptions::new().write(true))
                .map_err(save_conflict_if_missing)?
                .metadata()?
                .permissions();
            #[cfg(unix)]
            let permissions = {
                use std::os::unix::fs::PermissionsExt;
                fs::Permissions::from_mode(permissions.mode() & !0o6000)
            };
            temporary.as_file().set_permissions(permissions)?;
            temporary.as_file().sync_all()?;

            let current = open_regular_file(&target, fs::OpenOptions::new().read(true).write(true))
                .map_err(save_conflict_if_missing)?;
            if !file_has_contents(current, previous)? || existing_save_target(path)? != target {
                return Err(save_conflict());
            }
            temporary.persist(&target)?;
        } else {
            temporary.as_file().sync_all()?;
            if fs::canonicalize(parent_directory(path))? != parent_directory(&target) {
                return Err(save_conflict());
            }
            temporary.persist_noclobber(path)?;
        }
        Ok(target)
    }

    fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(bytes)
    }

    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        fs::metadata(path)
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata> {
        fs::symlink_metadata(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirectoryEntry>> {
        fs::read_dir(path)?
            .map(|entry| {
                let entry = entry?;
                let kind = entry.file_type()?;
                let file_type = if kind.is_symlink() {
                    FileType::Symlink
                } else if kind.is_dir() {
                    FileType::Directory
                } else if kind.is_file() {
                    FileType::File
                } else {
                    FileType::Other
                };
                Ok(DirectoryEntry {
                    path: entry.path(),
                    file_name: entry.file_name(),
                    file_type,
                })
            })
            .collect()
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }

    fn walk_directory(&self, path: &Path, options: WalkOptions) -> anyhow::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let mut walker = ignore::WalkBuilder::new(path);
        walker
            .max_depth(options.max_depth)
            .hidden(options.hidden)
            .ignore(options.ignore)
            .require_git(options.require_git);
        if let Some(excludes) = options.excludes {
            walker.git_global(false).git_exclude(false);
            if let Some(root) = excludes.root {
                walker.current_dir(root);
            }
            // The builder gives later files precedence and keeps valid patterns
            // from a global ignore file with parse errors.
            if let Some(file) = excludes.global
                && file.is_file()
            {
                let _ = walker.add_ignore(file);
            }
            if let Some(file) = excludes.local
                && file.is_file()
                && let Some(error) = walker.add_ignore(file)
            {
                return Err(error.into());
            }
        }
        for entry in walker.build() {
            let entry = entry?;
            if let Some(error) = entry.error() {
                return Err(error.clone().into());
            }
            if entry.depth() > 0 {
                paths.push(entry.into_path());
            }
        }
        Ok(paths)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        fs::copy(from, to)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        fs::read_link(path)
    }
}

fn open_regular_file(path: &Path, options: &mut fs::OpenOptions) -> io::Result<fs::File> {
    let not_regular = || io::Error::new(io::ErrorKind::InvalidInput, "not a regular file");
    if !fs::metadata(path)?.is_file() {
        return Err(not_regular());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO substituted between metadata and open must not block the caller.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(not_regular())
    }
}

fn existing_save_target(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map_err(save_conflict_if_missing)
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn save_conflict_if_missing(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::NotFound {
        save_conflict()
    } else {
        error
    }
}

fn save_conflict() -> io::Error {
    io::Error::other("file changed on disk")
}

fn file_has_contents(file: fs::File, mut expected: &[u8]) -> io::Result<bool> {
    let mut file = BufReader::new(file);
    loop {
        let bytes = file.fill_buf()?;
        if bytes.is_empty() {
            return Ok(expected.is_empty());
        }
        let Some(rest) = expected.strip_prefix(bytes) else {
            return Ok(false);
        };
        expected = rest;
        let len = bytes.len();
        file.consume(len);
    }
}
