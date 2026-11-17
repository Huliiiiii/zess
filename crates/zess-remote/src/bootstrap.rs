use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, prelude::BASE64_STANDARD};
use sha2::{Digest, Sha256};
use zess_platform::{Cancellation, FileSystem, InputProgress, ProcessRunner};

use crate::{
    ProgressReporter, RemoteProgress, RemoteStage, ServerArtifacts, ServerTarget, SshTarget,
};

pub trait ServerBootstrap: Send + Sync {
    /// Returns a complete SSH command that starts the prepared proxy.
    fn prepare(&self, target: &SshTarget, runner: &dyn ProcessRunner) -> Result<String>;
}

pub struct SshBootstrap {
    pub artifacts: Arc<dyn ServerArtifacts>,
    pub filesystem: Arc<dyn FileSystem>,
}

impl ServerBootstrap for SshBootstrap {
    fn prepare(&self, target: &SshTarget, runner: &dyn ProcessRunner) -> Result<String> {
        let cancellation = super::ssh::Deadline::new(target);
        target.report(RemoteProgress::Stage(RemoteStage::DetectingPlatform));
        let posix = execute(
            runner,
            target,
            "sh -c 'uname -s; uname -m'",
            None,
            &cancellation,
        );
        let platform = match &posix {
            Ok(bytes) => {
                let output = String::from_utf8_lossy(bytes);
                let mut lines = output.lines();
                match (lines.next(), lines.next()) {
                    (Some("Linux"), Some("x86_64")) => ServerTarget::LinuxX86_64,
                    (Some(os), Some(arch)) => bail!("unsupported remote platform {os}/{arch}"),
                    _ => bail!("invalid remote platform probe response"),
                }
            }
            Err(_) => {
                cancellation.check()?;
                let output = execute(runner, target, &powershell("[Console]::WriteLine('Windows');if($env:PROCESSOR_ARCHITEW6432){[Console]::WriteLine($env:PROCESSOR_ARCHITEW6432)}else{[Console]::WriteLine($env:PROCESSOR_ARCHITECTURE)}"), None, &cancellation).context("detect remote platform")?;
                let output = String::from_utf8_lossy(&output);
                let mut lines = output.trim_start_matches('\u{feff}').lines();
                match (lines.next(), lines.next()) {
                    (Some("Windows"), Some("AMD64")) => ServerTarget::WindowsX86_64,
                    (os, arch) => bail!(
                        "unsupported remote platform {}/{}",
                        os.unwrap_or("unknown"),
                        arch.unwrap_or("unknown")
                    ),
                }
            }
        };
        let remote = RemotePlatform::new(platform);
        target.report(RemoteProgress::Stage(RemoteStage::CheckingCache));
        if execute(runner, target, &remote.check(), None, &cancellation).is_ok() {
            return Ok(remote.proxy());
        }
        cancellation.check()?;
        let artifact = self
            .artifacts
            .get(platform, &cancellation, target.progress.as_deref())?;
        ensure!(
            artifact.target == platform,
            "server artifact targets the wrong platform"
        );
        let bytes = self
            .filesystem
            .read(&artifact.path)
            .context("read server artifact")?;
        let hash: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        ensure!(
            hash == artifact.sha256.to_ascii_lowercase(),
            "local server SHA-256 mismatch"
        );
        target.report(RemoteProgress::Stage(RemoteStage::Uploading));
        let upload = UploadProgress {
            reporter: target.progress.as_deref(),
            complete: AtomicBool::new(false),
        };
        execute_upload(
            runner,
            target,
            &remote.install(&hash),
            &bytes,
            &cancellation,
            &upload,
        )
        .context("install remote server")?;
        if !upload.complete.load(Ordering::Acquire) {
            target.report(RemoteProgress::Stage(RemoteStage::Installing));
        }
        execute(runner, target, &remote.check(), None, &cancellation)
            .context("verify remote server cache")?;
        Ok(remote.proxy())
    }
}

fn execute(
    runner: &dyn ProcessRunner,
    target: &SshTarget,
    command: &str,
    input: Option<&[u8]>,
    cancellation: &dyn Cancellation,
) -> Result<Vec<u8>> {
    let output = runner.output(&target.command(command)?, input, cancellation)?;
    ensure!(
        output.status.success(),
        "SSH command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

struct UploadProgress<'a> {
    reporter: Option<&'a dyn ProgressReporter>,
    complete: AtomicBool,
}

impl InputProgress for UploadProgress<'_> {
    fn written(&self, completed: u64, total: u64) {
        if let Some(reporter) = self.reporter {
            reporter.report(RemoteProgress::Transfer {
                stage: RemoteStage::Uploading,
                completed,
                total: Some(total),
            });
            if completed == total {
                self.complete.store(true, Ordering::Release);
                reporter.report(RemoteProgress::Stage(RemoteStage::Installing));
            }
        }
    }
}

fn execute_upload(
    runner: &dyn ProcessRunner,
    target: &SshTarget,
    command: &str,
    input: &[u8],
    cancellation: &dyn Cancellation,
    progress: &UploadProgress<'_>,
) -> Result<()> {
    let output = runner.output_with_progress(
        &target.command(command)?,
        Some(input),
        cancellation,
        progress,
    )?;
    ensure!(
        output.status.success(),
        "SSH command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

struct RemotePlatform(ServerTarget);

impl RemotePlatform {
    fn new(target: ServerTarget) -> Self {
        Self(target)
    }

    fn command(&self, posix: &str, windows: &str) -> String {
        match self.0 {
            ServerTarget::LinuxX86_64 => format!("sh -c {}", quote(posix)),
            ServerTarget::WindowsX86_64 => powershell(windows),
        }
    }

    fn check(&self) -> String {
        let posix = format!(
            "set -eu; d=\"$HOME/.cache/zess/servers/{}/current\"; test -x \"$d/zess\"; test -f \"$d/source.sha256\"; test -f \"$d/installed.sha256\"; cd \"$d\"; sha256sum -c --status installed.sha256",
            zess_build::BUILD_ID
        );
        let windows = format!(
            "$ErrorActionPreference='Stop';{} $p=Join-Path $d 'zess.exe';$h=Join-Path $d 'sha256';if(!(Test-Path -LiteralPath $p) -or !(Test-Path -LiteralPath $h) -or (Get-FileHash -Algorithm SHA256 -LiteralPath $p).Hash.ToLowerInvariant() -ne ([IO.File]::ReadAllText($h)).Trim()){{exit 1}}",
            windows_dir()
        );
        self.command(&posix, &windows)
    }

    fn install(&self, hash: &str) -> String {
        let posix = format!(
            "set -eu; umask 077; base=\"$HOME/.cache/zess/servers/{}\"; d=\"$base/current\"; mkdir -p \"$base\"; t=$(mktemp -d \"$base/install.XXXXXXXX\"); link=\"$base/.current.$$\"; trap 'rm -f \"$link\"; if [ -n \"$t\" ]; then rm -rf \"$t\"; fi' EXIT HUP INT TERM; cat > \"$t/archive.tar.gz\"; h=$(sha256sum \"$t/archive.tar.gz\"); h=${{h%% *}}; [ \"$h\" = '{hash}' ] || {{ echo 'server SHA-256 mismatch' >&2; exit 1; }}; tar -xzf \"$t/archive.tar.gz\" -C \"$t\"; rm \"$t/archive.tar.gz\"; test -x \"$t/patchelf\"; test -f \"$t/lib/ld-linux-x86-64.so.2\"; \"$t/patchelf\" --set-interpreter \"$d/lib/ld-linux-x86-64.so.2\" --set-rpath '$ORIGIN/lib' \"$t/zess\"; chmod 700 \"$t/zess\"; printf '%s' '{hash}' > \"$t/source.sha256\"; (cd \"$t\"; sha256sum zess lib/* > installed.sha256); ln -s \"$t\" \"$link\"; mv -Tf \"$link\" \"$d\"; t=''",
            zess_build::BUILD_ID
        );
        let windows = format!(
            "$ErrorActionPreference='Stop';{} [IO.Directory]::CreateDirectory($d)>$null;$t=Join-Path $d ([Guid]::NewGuid().ToString()+'.tmp');try{{$f=[IO.File]::Create($t);try{{[Console]::OpenStandardInput().CopyTo($f)}}finally{{$f.Dispose()}};if((Get-FileHash -Algorithm SHA256 -LiteralPath $t).Hash.ToLowerInvariant() -ne '{hash}'){{throw 'server SHA-256 mismatch'}};$dest=Join-Path $d 'zess.exe';if(Test-Path -LiteralPath $dest){{[IO.File]::Replace($t,$dest,[NullString]::Value)}}else{{[IO.File]::Move($t,$dest)}};[IO.File]::WriteAllText((Join-Path $d 'sha256'),'{hash}')}}catch{{[Console]::Error.WriteLine($_);exit 1}}finally{{if(Test-Path -LiteralPath $t){{Remove-Item -LiteralPath $t -Force}}}}",
            windows_dir()
        );
        self.command(&posix, &windows)
    }

    fn proxy(&self) -> String {
        let posix = format!(
            "exec \"$HOME/.cache/zess/servers/{}/current/zess\" remote-proxy",
            zess_build::BUILD_ID
        );
        let windows = format!(
            "{} $s=[Diagnostics.ProcessStartInfo]::new();$s.FileName=Join-Path $d 'zess.exe';$s.Arguments='remote-proxy';$s.UseShellExecute=$false;$p=[Diagnostics.Process]::Start($s);$p.WaitForExit();exit $p.ExitCode",
            windows_dir()
        );
        self.command(&posix, &windows)
    }
}

fn windows_dir() -> String {
    format!(
        "$d=Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Zess/servers/{}';",
        zess_build::BUILD_ID
    )
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn powershell(script: &str) -> String {
    let script = format!("[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);{script}");
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        BASE64_STANDARD.encode(bytes)
    )
}
