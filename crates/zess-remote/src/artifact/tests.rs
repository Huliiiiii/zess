use std::{path::PathBuf, sync::Mutex, time::SystemTime};

use serde_json::json;
use tempfile::TempDir;
use zess_platform::{CancellationToken, HostInfo, NativeFileSystem, NativePlatform, UserDirs};

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

struct ReleaseFixture {
    manifest: Vec<u8>,
    binary: Vec<u8>,
    calls: Mutex<Vec<String>>,
}

struct FixtureMiddleware(Arc<ReleaseFixture>);

impl ureq::middleware::Middleware for FixtureMiddleware {
    fn handle(
        &self,
        request: ureq::http::Request<ureq::SendBody>,
        _: ureq::middleware::MiddlewareNext,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let url = request.uri().to_string();
        let body = if url.ends_with("/manifest.json") {
            self.0.manifest.clone()
        } else {
            self.0.binary.clone()
        };
        self.0.calls.lock().unwrap().push(url);
        Ok(ureq::http::Response::new(ureq::Body::builder().data(body)))
    }
}

fn fixture(
    build_id: &str,
    expected_hash: &str,
) -> (TempDir, Arc<ReleaseFixture>, GithubReleaseArtifacts) {
    let directory = tempfile::tempdir().unwrap();
    let release = Arc::new(ReleaseFixture {
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
    let mut provider = GithubReleaseArtifacts::new(
        "owner/repo",
        Arc::new(NativeFileSystem),
        Arc::new(TestPlatform(directory.path().to_owned())),
    );
    provider.agent = ureq::Agent::config_builder()
        .middleware(FixtureMiddleware(release.clone()))
        .build()
        .into();
    (directory, release, provider)
}

#[test]
fn matching_release_is_cached_for_the_selected_target() {
    let expected = SERVER_SHA256;
    let (_directory, release, provider) = fixture(zess_build::BUILD_ID, expected);
    let cancellation = CancellationToken::new();
    let artifact = provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    assert_eq!(artifact.sha256, expected);
    assert_eq!(std::fs::read(&artifact.path).unwrap(), b"server executable");
    let calls = release.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        format!(
            "https://github.com/owner/repo/releases/download/build-{}/manifest.json",
            zess_build::BUILD_ID
        )
    );
    assert!(calls[1].ends_with("/zess-server-x86_64-pc-windows-msvc.exe"));
    drop(calls);
    provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    assert_eq!(release.calls.lock().unwrap().len(), 2);
}

#[test]
fn linux_release_archive_is_selected_and_verified() {
    let (_directory, release, provider) = fixture(zess_build::BUILD_ID, SERVER_SHA256);
    let artifact = provider
        .get(ServerTarget::LinuxX86_64, &CancellationToken::new())
        .unwrap();
    assert_eq!(artifact.sha256, SERVER_SHA256);
    assert_eq!(std::fs::read(artifact.path).unwrap(), b"server executable");
    assert!(
        release.calls.lock().unwrap()[1].ends_with("/zess-server-x86_64-unknown-linux-gnu.tar.gz")
    );
}

#[test]
fn release_with_another_build_id_is_rejected_before_download() {
    let (_directory, release, provider) = fixture("another-build", SERVER_SHA256);
    assert!(
        provider
            .get(ServerTarget::WindowsX86_64, &CancellationToken::new())
            .is_err()
    );
    assert_eq!(release.calls.lock().unwrap().len(), 1);
}

#[test]
fn corrupted_cached_binary_is_downloaded_again() {
    let (_directory, release, provider) = fixture(zess_build::BUILD_ID, SERVER_SHA256);
    let cancellation = CancellationToken::new();
    let artifact = provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    std::fs::write(&artifact.path, b"corrupt").unwrap();
    let artifact = provider
        .get(ServerTarget::WindowsX86_64, &cancellation)
        .unwrap();
    assert_eq!(std::fs::read(artifact.path).unwrap(), b"server executable");
    assert_eq!(release.calls.lock().unwrap().len(), 4);
}

#[test]
fn cancellation_during_download_stops_before_publishing() {
    struct CancelReader {
        cancellation: CancellationToken,
        manifest: std::io::Cursor<Vec<u8>>,
    }
    impl std::io::Read for CancelReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = self.manifest.read(buffer)?;
            self.cancellation.cancel();
            Ok(count)
        }
    }

    struct CancelMiddleware {
        cancellation: CancellationToken,
        manifest: Vec<u8>,
    }
    impl ureq::middleware::Middleware for CancelMiddleware {
        fn handle(
            &self,
            _: ureq::http::Request<ureq::SendBody>,
            _: ureq::middleware::MiddlewareNext,
        ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
            Ok(ureq::http::Response::new(ureq::Body::builder().reader(
                CancelReader {
                    cancellation: self.cancellation.clone(),
                    manifest: std::io::Cursor::new(self.manifest.clone()),
                },
            )))
        }
    }

    let (directory, release, mut provider) = fixture(zess_build::BUILD_ID, SERVER_SHA256);
    let cancellation = CancellationToken::new();
    provider.agent = ureq::Agent::config_builder()
        .middleware(CancelMiddleware {
            cancellation: cancellation.clone(),
            manifest: release.manifest.clone(),
        })
        .build()
        .into();
    let error = match provider.get(ServerTarget::WindowsX86_64, &cancellation) {
        Ok(_) => panic!("cancelled download succeeded"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "operation was cancelled");
    assert!(!directory.path().join("zess/servers").exists());
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
