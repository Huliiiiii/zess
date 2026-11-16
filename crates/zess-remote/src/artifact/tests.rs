use std::{ffi::OsStr, path::PathBuf, process::Output, sync::Mutex, time::SystemTime};

use serde_json::json;
use tempfile::TempDir;
use zess_platform::{
    CancellationToken, HostInfo, NativeFileSystem, NativePlatform, OwnedProcess, UserDirs,
};

use super::*;

const SERVER_SHA256: &str = "49ef64922d20ae181f0ace1d4e9d738cc5c000611b56004a8ed9d83a77c11173";

struct TestPlatform(PathBuf);

impl Platform for TestPlatform {
    fn host_info(&self) -> HostInfo {
        NativePlatform.host_info()
    }
    fn user_dirs(&self) -> Result<UserDirs> {
        let mut dirs = NativePlatform.user_dirs()?;
        dirs.cache = self.0.clone();
        Ok(dirs)
    }
    fn current_dir(&self) -> std::io::Result<PathBuf> {
        NativePlatform.current_dir()
    }
    fn current_exe(&self) -> std::io::Result<PathBuf> {
        NativePlatform.current_exe()
    }
    fn absolute_path(&self, path: &Path) -> std::io::Result<PathBuf> {
        NativePlatform.absolute_path(path)
    }
    fn process_id(&self) -> u32 {
        NativePlatform.process_id()
    }
    fn system_time(&self) -> SystemTime {
        NativePlatform.system_time()
    }
}

struct Gh {
    manifest: Vec<u8>,
    binary: Vec<u8>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl ProcessRunner for Gh {
    fn output(
        &self,
        spec: &ProcessSpec,
        _: Option<&[u8]>,
        cancellation: &dyn Cancellation,
    ) -> Result<Output> {
        cancellation.check()?;
        assert_eq!(spec.program, OsStr::new("gh"));
        let args: Vec<_> = spec
            .args
            .iter()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect();
        let name = args[args.iter().position(|arg| arg == "--pattern").unwrap() + 1].as_str();
        let stdout = if name == "manifest.json" {
            self.manifest.clone()
        } else {
            self.binary.clone()
        };
        self.calls.lock().unwrap().push(args);
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout,
            stderr: Vec::new(),
        })
    }

    fn spawn(&self, _: &ProcessSpec) -> Result<OwnedProcess> {
        unreachable!()
    }
}

fn fixture(build_id: &str, expected_hash: &str) -> (TempDir, Arc<Gh>, GithubReleaseArtifacts) {
    let directory = tempfile::tempdir().unwrap();
    let gh = Arc::new(Gh {
        manifest: serde_json::to_vec(&json!({
            "build_id": build_id,
            "artifacts": [
                { "target": "x86_64-unknown-linux-gnu", "file": "zess-server-x86_64-unknown-linux-gnu.tar.gz", "sha256": SERVER_SHA256 },
                { "target": "x86_64-pc-windows-msvc", "file": "zess-server-x86_64-pc-windows-msvc.exe", "sha256": expected_hash }
            ]
        })).unwrap(),
        binary: b"server executable".to_vec(),
        calls: Mutex::new(Vec::new()),
    });
    let provider = GithubReleaseArtifacts::new(
        "owner/repo",
        Arc::new(NativeFileSystem),
        Arc::new(TestPlatform(directory.path().to_owned())),
        gh.clone(),
    );
    (directory, gh, provider)
}

#[test]
fn matching_release_is_cached_for_the_selected_target() {
    let expected = SERVER_SHA256;
    let (_directory, gh, provider) = fixture(zess_build::BUILD_ID, expected);
    let cancellation = CancellationToken::new();
    let artifact = provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    assert_eq!(artifact.sha256, expected);
    assert_eq!(std::fs::read(&artifact.path).unwrap(), b"server executable");
    let calls = gh.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].contains(&format!("build-{}", zess_build::BUILD_ID)));
    assert!(calls[1].contains(&"zess-server-x86_64-pc-windows-msvc.exe".to_owned()));
    drop(calls);
    provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    assert_eq!(gh.calls.lock().unwrap().len(), 2);
}

#[test]
fn linux_release_archive_is_selected_and_verified() {
    let (_directory, gh, provider) = fixture(zess_build::BUILD_ID, SERVER_SHA256);
    let artifact = provider
        .get(ServerTarget::LinuxX86_64, &CancellationToken::new())
        .unwrap();
    assert_eq!(artifact.sha256, SERVER_SHA256);
    assert_eq!(std::fs::read(artifact.path).unwrap(), b"server executable");
    assert!(
        gh.calls.lock().unwrap()[1]
            .contains(&"zess-server-x86_64-unknown-linux-gnu.tar.gz".to_owned())
    );
}

#[test]
fn release_with_another_build_id_is_rejected_before_download() {
    let (_directory, gh, provider) = fixture("another-build", SERVER_SHA256);
    assert!(
        provider
            .get(ServerTarget::WindowsX86_64, &CancellationToken::new())
            .is_err()
    );
    assert_eq!(gh.calls.lock().unwrap().len(), 1);
}

#[test]
fn corrupted_cached_binary_is_downloaded_again() {
    let (_directory, gh, provider) = fixture(zess_build::BUILD_ID, SERVER_SHA256);
    let cancellation = CancellationToken::new();
    let artifact = provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    std::fs::write(&artifact.path, b"corrupt").unwrap();
    let artifact = provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    assert_eq!(std::fs::read(artifact.path).unwrap(), b"server executable");
    assert_eq!(gh.calls.lock().unwrap().len(), 4);
}

#[test]
fn wrong_hash_is_not_published_to_the_cache() {
    let (directory, _, provider) = fixture(zess_build::BUILD_ID, &"0".repeat(64));
    assert!(
        provider
            .get(ServerTarget::WindowsX86_64, &CancellationToken::new())
            .is_err()
    );
    assert!(!directory.path().join("zess/servers").exists());
}
