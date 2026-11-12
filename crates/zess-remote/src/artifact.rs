use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zess_platform::{FileSystem, NativeFileSystem};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperatingSystem {
    Linux,
    Windows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Platform {
    pub os: OperatingSystem,
    pub arch: Architecture,
}

impl Platform {
    pub fn current() -> anyhow::Result<Self> {
        Self::parse(std::env::consts::OS, std::env::consts::ARCH)
    }

    pub fn parse(os: &str, arch: &str) -> anyhow::Result<Self> {
        let os = match os.trim().to_ascii_lowercase().as_str() {
            "linux" => OperatingSystem::Linux,
            "windows" | "windows_nt" => OperatingSystem::Windows,
            other => bail!("unsupported remote operating system: {other}"),
        };
        let arch = match arch.trim().to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" | "x64" => Architecture::X86_64,
            "aarch64" | "arm64" => Architecture::Aarch64,
            other => bail!("unsupported remote architecture: {other}"),
        };
        Ok(Self { os, arch })
    }

    pub fn artifact_name(self) -> String {
        let os = match self.os {
            OperatingSystem::Linux => "linux",
            OperatingSystem::Windows => "windows",
        };
        let arch = match self.arch {
            Architecture::X86_64 => "x86_64",
            Architecture::Aarch64 => "aarch64",
        };
        let extension = if self.os == OperatingSystem::Windows {
            ".exe"
        } else {
            ""
        };
        format!("zess-{os}-{arch}{extension}")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ArtifactSource {
    Local { path: PathBuf },
    Download { url: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerArtifact {
    pub platform: Platform,
    pub version: String,
    pub sha256: String,
    pub source: ArtifactSource,
}

impl ServerArtifact {
    pub fn local(platform: Platform, version: String, path: PathBuf) -> anyhow::Result<Self> {
        Self::local_with_file_system(platform, version, path, &NativeFileSystem)
    }

    pub fn local_with_file_system(
        platform: Platform,
        version: String,
        path: PathBuf,
        file_system: &dyn FileSystem,
    ) -> anyhow::Result<Self> {
        let bytes = file_system
            .read(&path)
            .with_context(|| format!("read server binary {}", path.display()))?;
        Ok(Self {
            platform,
            version,
            sha256: digest(&bytes),
            source: ArtifactSource::Local { path },
        })
    }

    pub(crate) fn validate(&self, platform: Platform, version: &str) -> anyhow::Result<()> {
        if self.platform != platform || self.version != version {
            bail!("server artifact does not match the remote platform and requested version");
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("server artifact requires a 64-digit SHA-256 checksum");
        }
        if let ArtifactSource::Download { url } = &self.source
            && (!url.starts_with("https://") || url.contains(['\r', '\n', '\0']))
        {
            bail!("server artifact download requires an HTTPS URL");
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct ArtifactCatalog {
    artifacts: BTreeMap<Platform, ServerArtifact>,
}

impl ArtifactCatalog {
    pub fn insert(&mut self, artifact: ServerArtifact) {
        self.artifacts.insert(artifact.platform, artifact);
    }

    pub fn read(path: &std::path::Path) -> anyhow::Result<Self> {
        Self::read_with_file_system(path, &NativeFileSystem)
    }

    pub fn read_with_file_system(
        path: &std::path::Path,
        file_system: &dyn FileSystem,
    ) -> anyhow::Result<Self> {
        let artifacts: Vec<ServerArtifact> = serde_json::from_slice(&file_system.read(path)?)?;
        let mut catalog = Self::default();
        for mut artifact in artifacts {
            if let ArtifactSource::Local { path: binary } = &mut artifact.source
                && binary.is_relative()
            {
                *binary = path
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join(&binary);
            }
            if catalog.artifacts.contains_key(&artifact.platform) {
                bail!("duplicate platform in server artifact catalog");
            }
            catalog.insert(artifact);
        }
        Ok(catalog)
    }

    pub(crate) fn get(&self, platform: Platform) -> Option<&ServerArtifact> {
        self.artifacts.get(&platform)
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 15) as usize] as char);
    }
    output
}
