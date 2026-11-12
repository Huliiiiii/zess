use std::{
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

use anyhow::{Context, Result};
use zess_platform::{Cancellation, ProcessRunner, ProcessSpec, SystemProcessRunner};

use crate::{Architecture, ArtifactSource, OperatingSystem, Platform, ServerArtifact};

struct NeverCancel;

impl Cancellation for NeverCancel {
    fn check(&self) -> Result<()> {
        Ok(())
    }
}

fn executable(name: &str) -> Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").context("PATH is missing")?)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .with_context(|| format!("this external test requires {name}"))
}

#[test]
#[ignore = "requires POSIX shell tools and shasum"]
fn a_corrupt_upload_keeps_the_cached_server_when_sha256sum_is_unavailable() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let home = fixture.path().join("家 空格 %!& 单'");
    std::fs::create_dir(&home)?;
    let tools = fixture.path().join("tools");
    std::fs::create_dir(&tools)?;
    for name in [
        "sh", "mkdir", "mktemp", "cat", "shasum", "chmod", "mv", "rm",
    ] {
        symlink(executable(name)?, tools.join(name))?;
    }
    let source = fixture.path().join("server");
    std::fs::write(&source, b"abc")?;
    let artifact = ServerArtifact {
        platform: Platform {
            os: OperatingSystem::Linux,
            arch: Architecture::Aarch64,
        },
        version: "fixture".to_owned(),
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_owned(),
        source: ArtifactSource::Local { path: source },
    };
    let mut command = ProcessSpec::new(executable("sh")?);
    command
        .args(["-c", &super::install_command(&artifact)])
        .env("HOME", &home)
        .env("PATH", &tools);
    let valid = SystemProcessRunner.output(&command, Some(b"abc"), &NeverCancel)?;
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert!(valid.stdout.is_empty());
    let installed = home
        .join(".cache/zess/servers")
        .join(&artifact.sha256)
        .join("zess");
    assert_eq!(std::fs::read(&installed)?, b"abc");
    assert_eq!(
        std::fs::metadata(&installed)?.permissions().mode() & 0o777,
        0o700
    );

    let invalid = SystemProcessRunner.output(&command, Some(b"corrupt\0input"), &NeverCancel)?;
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("server SHA-256 mismatch"));
    assert_eq!(std::fs::read(&installed)?, b"abc");
    let entries: Vec<_> =
        std::fs::read_dir(installed.parent().unwrap())?.collect::<std::io::Result<_>>()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].file_name(), "zess");
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Linux PowerShell 7 at ZESS_TEST_PWSH"]
fn powershell_installation_preserves_valid_cache_and_repairs_corrupt_cache() -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let pwsh = std::env::var_os("ZESS_TEST_PWSH").context("set ZESS_TEST_PWSH to pwsh")?;
    let fixture = tempfile::tempdir()?;
    let data = fixture.path().join("家 空格 %!& 单' []");
    std::fs::create_dir(&data)?;
    let artifact = ServerArtifact {
        platform: Platform {
            os: OperatingSystem::Windows,
            arch: Architecture::X86_64,
        },
        version: "fixture".to_owned(),
        sha256: "420e9c8ccb6a038d9ef238d20a40909a0c885716e4b788e6a851b8e84f147c5c".to_owned(),
        source: ArtifactSource::Local {
            path: fixture.path().join("source"),
        },
    };
    let mut command = ProcessSpec::new(pwsh);
    command
        .args(
            super::install_command(&artifact)
                .split_ascii_whitespace()
                .skip(1),
        )
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path().join("config"))
        .env("XDG_CACHE_HOME", fixture.path().join("cache"))
        // On Linux this controls .NET's LocalApplicationData known folder.
        .env("XDG_DATA_HOME", &data);
    let installed = data
        .join("Zess/servers")
        .join(&artifact.sha256)
        .join("zess.exe");
    let payload = b"abc\0\xff\r\n";
    let install = |input: &[u8]| SystemProcessRunner.output(&command, Some(input), &NeverCancel);
    let cache_is_valid = || -> Result<()> {
        assert_eq!(std::fs::read(&installed)?, payload);
        let entries: Vec<_> =
            std::fs::read_dir(installed.parent().unwrap())?.collect::<std::io::Result<_>>()?;
        assert_eq!(entries.len(), 1, "temporary upload was not removed");
        assert_eq!(entries[0].file_name(), "zess.exe");
        Ok(())
    };

    let first = install(payload)?;
    assert!(first.status.success(), "{:?}", first);
    assert!(first.stdout.is_empty());
    assert!(first.stderr.is_empty());
    cache_is_valid()?;
    let original = std::fs::metadata(&installed)?;

    let invalid = install(b"corrupt\0upload")?;
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("server SHA-256 mismatch"));
    cache_is_valid()?;
    assert_eq!(std::fs::metadata(&installed)?.ino(), original.ino());

    let repeated = install(payload)?;
    assert!(repeated.status.success(), "{:?}", repeated);
    cache_is_valid()?;
    let unchanged = std::fs::metadata(&installed)?;
    assert_eq!(unchanged.ino(), original.ino());
    assert_eq!(unchanged.modified()?, original.modified()?);

    std::fs::write(&installed, b"corrupt cache")?;
    let repaired = install(payload)?;
    assert!(repaired.status.success(), "{:?}", repaired);
    cache_is_valid()?;
    Ok(())
}
