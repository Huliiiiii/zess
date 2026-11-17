use std::{
    fs::Metadata,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use zess_platform::{
    DirectoryEntry, FileSystem, HostInfo, NativeFileSystem, Platform, UserDirs, WalkOptions,
};

use crate::{HelixSyntaxProvider, TextBuffer, tests::wait_for_highlight_refresh};

struct ResourceHost;

impl Platform for ResourceHost {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(PathBuf::from("injected-host-executable"))
    }
    fn host_info(&self) -> HostInfo {
        unreachable!()
    }
    fn user_dirs(&self) -> anyhow::Result<UserDirs> {
        unreachable!()
    }
    fn current_dir(&self) -> io::Result<PathBuf> {
        unreachable!()
    }
    fn absolute_path(&self, _path: &Path) -> io::Result<PathBuf> {
        unreachable!()
    }
    fn process_id(&self) -> u32 {
        unreachable!()
    }
    fn system_time(&self) -> SystemTime {
        unreachable!()
    }
}

#[derive(Default)]
struct UnavailableResources {
    canonicalized: Mutex<Vec<PathBuf>>,
}

impl FileSystem for UnavailableResources {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.canonicalized.lock().unwrap().push(path.to_owned());
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resource access denied",
        ))
    }

    fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
        unreachable!()
    }
    fn open_read(&self, _path: &Path) -> io::Result<Box<dyn Read + Send>> {
        unreachable!()
    }
    fn create_new(&self, _path: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn write(&self, _path: &Path, _bytes: &[u8]) -> io::Result<()> {
        unreachable!()
    }
    fn save_file(
        &self,
        _path: &Path,
        _previous: Option<(&Path, &[u8])>,
        _bytes: &[u8],
    ) -> io::Result<PathBuf> {
        unreachable!()
    }
    fn append(&self, _path: &Path, _bytes: &[u8]) -> io::Result<()> {
        unreachable!()
    }
    fn metadata(&self, _path: &Path) -> io::Result<Metadata> {
        unreachable!()
    }
    fn symlink_metadata(&self, _path: &Path) -> io::Result<Metadata> {
        unreachable!()
    }
    fn read_dir(&self, _path: &Path) -> io::Result<Vec<DirectoryEntry>> {
        unreachable!()
    }
    fn walk_directory(&self, _path: &Path, _options: WalkOptions) -> anyhow::Result<Vec<PathBuf>> {
        unreachable!()
    }
    fn create_dir(&self, _path: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn create_dir_all(&self, _path: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn remove_file(&self, _path: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn remove_dir(&self, _path: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn remove_dir_all(&self, _path: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn copy(&self, _from: &Path, _to: &Path) -> io::Result<u64> {
        unreachable!()
    }
    fn read_link(&self, _path: &Path) -> io::Result<PathBuf> {
        unreachable!()
    }
}

#[test]
fn syntax_loading_uses_the_supplied_host_and_reports_its_resource_failure() {
    if std::env::var_os("ZESS_TEST_SYNTAX_RESOURCE_CONTEXT").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "language::tests::resources::syntax_loading_uses_the_supplied_host_and_reports_its_resource_failure"])
            .env("ZESS_TEST_SYNTAX_RESOURCE_CONTEXT", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let filesystem = Arc::new(UnavailableResources::default());
    let provider = Arc::new(HelixSyntaxProvider::with_platform(
        Arc::new(ResourceHost),
        filesystem.clone(),
    ));
    let mut buffer = TextBuffer::new_with_path_and_capabilities(
        "fn main() {}",
        Path::new("source.rs"),
        Arc::new(NativeFileSystem),
        provider,
    );
    assert_eq!(buffer.language_id(), Some("rust"));
    assert!(filesystem.canonicalized.lock().unwrap().is_empty());
    wait_for_highlight_refresh(&mut buffer);
    assert_eq!(buffer.syntax_error(), Some("resolve Zess executable"));
    assert!(!buffer.has_outline());
    assert_eq!(buffer.syntax_error(), Some("resolve Zess executable"));
    assert!(!filesystem.canonicalized.lock().unwrap().is_empty());
    assert!(
        filesystem
            .canonicalized
            .lock()
            .unwrap()
            .iter()
            .all(|path| path == Path::new("injected-host-executable"))
    );
    assert_eq!(buffer.text(), "fn main() {}");
}
