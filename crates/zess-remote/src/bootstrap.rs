use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, bail};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use zess_platform::{
    FileSystem, NativeFileSystem, NativePlatform, Platform as HostPlatform, ProcessRunner,
};

use crate::{
    ArtifactCatalog, ArtifactSource, OperatingSystem, PROXY_PROTOCOL_VERSION, Platform,
    ServerArtifact, SshTarget, artifact::digest, ssh::execute,
};

#[cfg(all(test, unix))]
mod tests;

#[derive(Debug, Serialize, Deserialize)]
pub struct RemoteInfo {
    pub version: String,
    pub protocol_version: u32,
    pub platform: Platform,
    pub binary_sha256: String,
}

impl RemoteInfo {
    pub fn current(version: &str) -> anyhow::Result<Self> {
        Self::current_with_file_system(version, &NativeFileSystem, &NativePlatform.current_exe()?)
    }

    pub fn current_with_file_system(
        version: &str,
        file_system: &dyn FileSystem,
        executable: &Path,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            version: version.to_owned(),
            protocol_version: PROXY_PROTOCOL_VERSION,
            platform: Platform::current()?,
            binary_sha256: digest(&file_system.read(executable)?),
        })
    }
}

pub struct RemoteServer {
    pub platform: Platform,
    command: String,
}

impl RemoteServer {
    pub fn new(platform: Platform, command: String) -> Self {
        Self { platform, command }
    }

    pub fn command(&self) -> &str {
        &self.command
    }
}

pub trait Bootstrap {
    fn prepare(&self, target: &SshTarget) -> anyhow::Result<RemoteServer>;
}

pub struct SshBootstrap<R> {
    pub runner: R,
    pub version: String,
    pub artifacts: ArtifactCatalog,
    pub server_binary: Option<PathBuf>,
    pub file_system: Arc<dyn FileSystem>,
    pub platform: Arc<dyn HostPlatform>,
}

impl<R: ProcessRunner> Bootstrap for SshBootstrap<R> {
    fn prepare(&self, target: &SshTarget) -> anyhow::Result<RemoteServer> {
        target.check_cancellation()?;
        let platform = detect_platform(&self.runner, target)?;
        let local = self.platform.host_info();
        let local = Platform::parse(local.os, local.arch)?;
        let artifact = if let Some(path) = &self.server_binary {
            Some(ServerArtifact::local_with_file_system(
                platform,
                self.version.clone(),
                path.clone(),
                self.file_system.as_ref(),
            )?)
        } else if let Some(artifact) = self.artifacts.get(platform) {
            Some(artifact.clone())
        } else if local == platform {
            Some(ServerArtifact::local_with_file_system(
                platform,
                self.version.clone(),
                self.platform.current_exe()?,
                self.file_system.as_ref(),
            )?)
        } else {
            None
        };
        target.check_cancellation()?;
        if let Some(artifact) = &artifact {
            artifact.validate(platform, &self.version)?;
        }
        let installed = remote_command(
            platform,
            "zess remote-info",
            &windows_exec(
                "(Get-Command zess -CommandType Application -ErrorAction Stop).Source",
                "remote-info",
            ),
        );
        if let Ok(output) = execute(&self.runner, target, &installed, None)
            && let Ok(info) = parse_info(&output)
            && self.matches(&info, platform, artifact.as_ref())
        {
            return Ok(RemoteServer {
                platform,
                command: remote_command(
                    platform,
                    "exec zess remote-proxy",
                    &windows_exec(
                        "(Get-Command zess -CommandType Application -ErrorAction Stop).Source",
                        "remote-proxy",
                    ),
                ),
            });
        }
        target.check_cancellation()?;
        let Some(artifact) = artifact else {
            bail!(
                "no compatible preinstalled Zess server for {}; supply --server-binary or a server artifact catalog for this remote platform",
                platform.artifact_name()
            );
        };
        let cache = cache_info_command(&artifact);
        if let Ok(output) = execute(&self.runner, target, &cache, None)
            && let Ok(info) = parse_info(&output)
            && self.matches(&info, platform, Some(&artifact))
        {
            return Ok(RemoteServer {
                platform,
                command: proxy_command(&artifact),
            });
        }
        target.check_cancellation()?;
        let input = match &artifact.source {
            ArtifactSource::Local { path } => {
                let bytes = self
                    .file_system
                    .read(path)
                    .with_context(|| format!("read server binary {}", path.display()))?;
                anyhow::ensure!(
                    digest(&bytes).eq_ignore_ascii_case(&artifact.sha256),
                    "local server binary checksum changed since artifact selection"
                );
                Some(bytes)
            }
            ArtifactSource::Download { .. } => None,
        };
        execute(
            &self.runner,
            target,
            &install_command(&artifact),
            input.as_deref(),
        )
        .context("install remote server")?;
        let output = execute(&self.runner, target, &cache, None)
            .context("run installed remote server; confirm that the binary targets the remote OS, architecture and C runtime")?;
        let info = parse_info(&output)?;
        anyhow::ensure!(
            self.matches(&info, platform, Some(&artifact)),
            "installed server identity or protocol does not match the requested artifact"
        );
        Ok(RemoteServer {
            platform,
            command: proxy_command(&artifact),
        })
    }
}

impl<R> SshBootstrap<R> {
    fn matches(
        &self,
        info: &RemoteInfo,
        platform: Platform,
        artifact: Option<&ServerArtifact>,
    ) -> bool {
        info.version == self.version
            && info.protocol_version == PROXY_PROTOCOL_VERSION
            && info.platform == platform
            && artifact
                .is_none_or(|artifact| artifact.sha256.eq_ignore_ascii_case(&info.binary_sha256))
    }
}

fn parse_info(output: &[u8]) -> anyhow::Result<RemoteInfo> {
    serde_json::from_slice(output.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(output)).context(
        "decode remote-info response; remote startup scripts must keep stdout free of banners",
    )
}

fn detect_platform(runner: &impl ProcessRunner, target: &SshTarget) -> anyhow::Result<Platform> {
    let posix = match execute(runner, target, "sh -c 'uname -s; uname -m'", None) {
        Err(error)
            if error
                .downcast_ref::<crate::ssh::SshCommandFailure>()
                .is_some_and(|failure| failure.status.code() == Some(255)) =>
        {
            return Err(error);
        }
        result => result,
    };
    target.check_cancellation()?;
    if let Ok(output) = &posix {
        let output = String::from_utf8_lossy(output);
        let mut lines = output.lines();
        if let (Some(os), Some(arch)) = (lines.next(), lines.next())
            && let Ok(platform) = Platform::parse(os, arch)
        {
            return Ok(platform);
        }
    }
    let command = powershell(
        "[Console]::WriteLine('windows'); if ($env:PROCESSOR_ARCHITEW6432) {[Console]::WriteLine($env:PROCESSOR_ARCHITEW6432)} else {[Console]::WriteLine($env:PROCESSOR_ARCHITECTURE)}",
    );
    let windows = execute(runner, target, &command, None).with_context(|| {
        format!(
            "detect remote platform (POSIX probe: {})",
            posix.err().map_or_else(
                || "unrecognized output".to_owned(),
                |error| error.to_string()
            )
        )
    })?;
    let output = String::from_utf8_lossy(&windows);
    let mut lines = output.trim_start_matches('\u{feff}').lines();
    Platform::parse(
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
    )
}

fn remote_command(platform: Platform, posix: &str, windows: &str) -> String {
    if platform.os == OperatingSystem::Windows {
        powershell(windows)
    } else {
        format!("sh -c {}", quote(posix))
    }
}

fn posix_directory(artifact: &ServerArtifact) -> String {
    format!(
        "\"$HOME/.cache/zess/servers/{}\"",
        artifact.sha256.to_ascii_lowercase()
    )
}

fn windows_directory(artifact: &ServerArtifact) -> String {
    format!(
        "$d=Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Zess/servers/{}';",
        artifact.sha256.to_ascii_lowercase()
    )
}

fn cache_info_command(artifact: &ServerArtifact) -> String {
    remote_command(
        artifact.platform,
        &format!("exec {}/zess remote-info", posix_directory(artifact)),
        &format!(
            "{} {}",
            windows_directory(artifact),
            windows_exec("(Join-Path $d 'zess.exe')", "remote-info")
        ),
    )
}

fn proxy_command(artifact: &ServerArtifact) -> String {
    remote_command(
        artifact.platform,
        &format!("exec {}/zess remote-proxy", posix_directory(artifact)),
        &format!(
            "{} {}",
            windows_directory(artifact),
            windows_exec("(Join-Path $d 'zess.exe')", "remote-proxy")
        ),
    )
}

fn install_command(artifact: &ServerArtifact) -> String {
    let hash = artifact.sha256.to_ascii_lowercase();
    let posix_source = match &artifact.source {
        ArtifactSource::Local { .. } => "cat > \"$t\"".to_owned(),
        ArtifactSource::Download { url } => format!(
            "curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output \"$t\" -- {}",
            quote(url)
        ),
    };
    let windows_source = match &artifact.source {
        ArtifactSource::Local { .. } => "$f=[IO.File]::Create($t);try{[Console]::OpenStandardInput().CopyTo($f)}finally{$f.Dispose()}".to_owned(),
        ArtifactSource::Download { url } => format!("& curl.exe --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output $t -- {};if($LASTEXITCODE -ne 0){{throw 'server download failed'}}", quote_powershell(url)),
    };
    let posix = format!(
        "set -eu; umask 077; d={}; mkdir -p \"$d\"; t=$(mktemp \"$d/.upload.XXXXXXXX\"); trap 'rm -f \"$t\"' EXIT HUP INT TERM; {posix_source}; if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum \"$t\"); else h=$(shasum -a 256 \"$t\"); fi; h=${{h%% *}}; [ \"$h\" = '{hash}' ] || {{ echo 'server SHA-256 mismatch' >&2; exit 1; }}; chmod 700 \"$t\"; mv -f \"$t\" \"$d/zess\"",
        posix_directory(artifact)
    );
    let windows = format!(
        "$ErrorActionPreference='Stop';{} [IO.Directory]::CreateDirectory($d)>$null;$t=Join-Path $d ([Guid]::NewGuid().ToString()+'.tmp');try{{{windows_source};if((Get-FileHash -Algorithm SHA256 -LiteralPath $t).Hash.ToLowerInvariant() -ne '{hash}'){{throw 'server SHA-256 mismatch'}};$dest=Join-Path $d 'zess.exe';if(Test-Path -LiteralPath $dest){{if((Get-FileHash -Algorithm SHA256 -LiteralPath $dest).Hash.ToLowerInvariant() -ne '{hash}'){{[IO.File]::Replace($t,$dest,[NullString]::Value)}}}}else{{[IO.File]::Move($t,$dest)}}}}catch{{[Console]::Error.WriteLine($_);exit 1}}finally{{if(Test-Path -LiteralPath $t){{Remove-Item -LiteralPath $t -Force}}}}",
        windows_directory(artifact)
    );
    remote_command(artifact.platform, &posix, &windows)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn quote_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn windows_exec(program: &str, argument: &str) -> String {
    // Leave all three handles inherited so PowerShell never decodes the protocol.
    format!(
        "$s=[Diagnostics.ProcessStartInfo]::new();$s.FileName={program};$s.Arguments='{argument}';$s.UseShellExecute=$false;$p=[Diagnostics.Process]::Start($s);$p.WaitForExit();exit $p.ExitCode"
    )
}

fn powershell(script: &str) -> String {
    let script = format!("[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);{script}");
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        BASE64_STANDARD.encode(bytes)
    )
}
