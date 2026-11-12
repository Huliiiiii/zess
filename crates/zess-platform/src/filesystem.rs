use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

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

pub struct WalkOptions {
    pub max_depth: Option<usize>,
    pub hidden: bool,
    pub ignore: bool,
    pub require_git: bool,
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
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn metadata(&self, path: &Path) -> io::Result<Metadata>;
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
}

pub struct NativeFileSystem;

impl FileSystem for NativeFileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(fs::File::open(path)?))
    }

    fn create_new(&self, path: &Path) -> io::Result<()> {
        fs::File::create_new(path)?;
        Ok(())
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        fs::write(path, bytes)
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
        for entry in ignore::WalkBuilder::new(path)
            .max_depth(options.max_depth)
            .hidden(options.hidden)
            .ignore(options.ignore)
            .require_git(options.require_git)
            .build()
        {
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
