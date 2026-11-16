use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zess_platform::{Cancellation, FileSystem, Platform};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServerTarget {
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    LinuxX86_64,
    #[serde(rename = "x86_64-pc-windows-msvc")]
    WindowsX86_64,
}

impl ServerTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::WindowsX86_64 => "x86_64-pc-windows-msvc",
        }
    }

    pub fn asset_name(self) -> String {
        let suffix = match self {
            Self::LinuxX86_64 => ".tar.gz",
            Self::WindowsX86_64 => ".exe",
        };
        format!("zess-server-{}{suffix}", self.as_str())
    }
}

pub struct ServerArtifact {
    pub target: ServerTarget,
    pub path: PathBuf,
    pub sha256: String,
}

pub trait ServerArtifacts: Send + Sync {
    fn get(&self, target: ServerTarget, cancellation: &dyn Cancellation) -> Result<ServerArtifact>;
}

pub struct GithubReleaseArtifacts {
    repository: String,
    file_system: Arc<dyn FileSystem>,
    platform: Arc<dyn Platform>,
    agent: ureq::Agent,
}

#[derive(Deserialize)]
struct Manifest {
    build_id: String,
    artifacts: Vec<ManifestArtifact>,
}

#[derive(Deserialize)]
struct ManifestArtifact {
    target: ServerTarget,
    file: String,
    sha256: String,
}

impl GithubReleaseArtifacts {
    pub fn new(
        repository: impl Into<String>,
        file_system: Arc<dyn FileSystem>,
        platform: Arc<dyn Platform>,
    ) -> Self {
        Self {
            repository: repository.into(),
            file_system,
            platform,
            agent: ureq::Agent::config_builder()
                .timeout_resolve(Some(Duration::from_secs(30)))
                .timeout_connect(Some(Duration::from_secs(30)))
                .timeout_send_request(Some(Duration::from_secs(30)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_per_read(Some(Duration::from_secs(30)))
                .https_only(true)
                .build()
                .into(),
        }
    }

    fn download(&self, name: &str, cancellation: &dyn Cancellation) -> Result<Vec<u8>> {
        cancellation.check()?;
        let url = format!(
            "https://github.com/{}/releases/download/build-{}/{name}",
            self.repository,
            zess_build::BUILD_ID
        );
        let response = self.agent.get(&url).call();
        cancellation.check()?;
        let response =
            response.with_context(|| format!("could not download release asset {name}"))?;
        let mut reader = response.into_body().into_reader();
        let mut bytes = Vec::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            cancellation.check()?;
            let count = reader.read(&mut buffer);
            cancellation.check()?;
            let count = count.with_context(|| format!("could not read release asset {name}"))?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        Ok(bytes)
    }

    fn publish(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let temp = path.with_extension(format!(
            "tmp-{}-{}",
            self.platform.process_id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        self.file_system.create_new(&temp)?;
        let result = self
            .file_system
            .write(&temp, bytes)
            .and_then(|()| self.file_system.rename(&temp, path));
        if result.is_err() {
            let _ = self.file_system.remove_file(&temp);
        }
        result?;
        Ok(())
    }
}

fn artifact_hash(manifest: &[u8], target: ServerTarget) -> Result<String> {
    let manifest: Manifest =
        serde_json::from_slice(manifest).context("invalid server release manifest")?;
    ensure!(
        manifest.build_id == zess_build::BUILD_ID,
        "server release build ID does not match this client"
    );
    let mut matches = manifest
        .artifacts
        .iter()
        .filter(|entry| entry.target == target);
    let entry = matches
        .next()
        .ok_or_else(|| anyhow!("server release has no artifact for {}", target.as_str()))?;
    ensure!(
        matches.next().is_none(),
        "server release has duplicate artifacts for {}",
        target.as_str()
    );
    ensure!(
        entry.file == target.asset_name(),
        "server release has an unexpected artifact filename"
    );
    ensure!(
        entry.sha256.len() == 64 && entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "server release has an invalid SHA-256"
    );
    Ok(entry.sha256.to_ascii_lowercase())
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl ServerArtifacts for GithubReleaseArtifacts {
    fn get(&self, target: ServerTarget, cancellation: &dyn Cancellation) -> Result<ServerArtifact> {
        cancellation.check()?;
        let directory = self
            .platform
            .user_dirs()?
            .cache
            .join("zess")
            .join("servers")
            .join(zess_build::BUILD_ID);
        let path = directory.join(target.asset_name());
        let manifest_path = directory.join("manifest.json");
        if let Ok(manifest) = self.file_system.read(&manifest_path)
            && let Ok(sha256) = artifact_hash(&manifest, target)
            && let Ok(bytes) = self.file_system.read(&path)
            && digest(&bytes) == sha256
        {
            return Ok(ServerArtifact {
                target,
                path,
                sha256,
            });
        }

        let manifest = self.download("manifest.json", cancellation)?;
        let sha256 = artifact_hash(&manifest, target)?;
        let bytes = self.download(&target.asset_name(), cancellation)?;
        ensure!(
            digest(&bytes) == sha256,
            "downloaded server SHA-256 does not match the release manifest"
        );
        cancellation.check()?;
        self.file_system.create_dir_all(&directory)?;
        self.publish(&path, &bytes)?;
        self.publish(&manifest_path, &manifest)?;
        Ok(ServerArtifact {
            target,
            path,
            sha256,
        })
    }
}
