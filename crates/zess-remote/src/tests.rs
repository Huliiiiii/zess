use std::{
    collections::VecDeque,
    io::{BufReader, Cursor},
    sync::Mutex,
};

use anyhow::bail;
use zess_platform::{Cancellation, OwnedProcess, ProcessRunner, ProcessSpec};

use crate::{
    Architecture, ArtifactCatalog, ArtifactSource, Bootstrap, OperatingSystem, Platform,
    ProxyOperation, ProxyRequest, RemoteInfo, ServerArtifact, SshBootstrap, SshTarget,
    artifact::digest, read_request, write_request,
};

struct Step {
    success: bool,
    stdout: Vec<u8>,
    input: Option<Vec<u8>>,
}

struct ScriptedRunner {
    steps: Mutex<VecDeque<Step>>,
    commands: Mutex<Vec<String>>,
}

impl ProcessRunner for ScriptedRunner {
    fn output(
        &self,
        spec: &ProcessSpec,
        input: Option<&[u8]>,
        _: &dyn Cancellation,
    ) -> anyhow::Result<std::process::Output> {
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected process execution");
        assert_eq!(input, step.input.as_deref());
        self.commands
            .lock()
            .unwrap()
            .push(spec.args.last().unwrap().to_string_lossy().into_owned());
        if step.success {
            #[cfg(unix)]
            let status = {
                use std::os::unix::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(0)
            };
            #[cfg(windows)]
            let status = {
                use std::os::windows::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(0)
            };
            Ok(std::process::Output {
                status,
                stdout: step.stdout,
                stderr: vec![],
            })
        } else {
            bail!("probe failed")
        }
    }

    fn spawn(&self, _: &ProcessSpec) -> anyhow::Result<OwnedProcess> {
        bail!("unexpected stream spawn")
    }
}

fn step(success: bool, stdout: &[u8]) -> Step {
    Step {
        success,
        stdout: stdout.to_vec(),
        input: None,
    }
}

#[test]
fn windows_server_upload_is_verified_before_connecting() {
    use base64::{Engine, prelude::BASE64_STANDARD};
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("server.exe");
    let bytes = b"windows server fixture\0\xff";
    std::fs::write(&path, bytes).unwrap();
    let platform = Platform {
        os: OperatingSystem::Windows,
        arch: Architecture::Aarch64,
    };
    let info = RemoteInfo {
        version: "test".to_owned(),
        protocol_version: crate::PROXY_PROTOCOL_VERSION,
        platform,
        binary_sha256: digest(bytes),
    };
    let runner = ScriptedRunner {
        steps: Mutex::new(VecDeque::from([
            step(false, b""),
            step(true, b"windows\r\nARM64\r\n"),
            step(false, b""),
            step(false, b""),
            Step {
                success: true,
                stdout: vec![],
                input: Some(bytes.to_vec()),
            },
            step(true, &serde_json::to_vec(&info).unwrap()),
        ])),
        commands: Mutex::new(vec![]),
    };
    let bootstrap = SshBootstrap {
        runner,
        version: "test".to_owned(),
        artifacts: ArtifactCatalog::default(),
        server_binary: Some(path),
        file_system: std::sync::Arc::new(zess_platform::NativeFileSystem),
        platform: std::sync::Arc::new(zess_platform::NativePlatform),
    };
    let server = bootstrap
        .prepare(&SshTarget::new("fixture".to_owned()))
        .unwrap();
    assert_eq!(server.platform, platform);
    let commands = bootstrap.runner.commands.lock().unwrap();
    let encoded = commands[4].split_whitespace().last().unwrap();
    let bytes = BASE64_STANDARD.decode(encoded).unwrap();
    let utf16: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes(*bytes))
        .collect();
    let script = String::from_utf16(&utf16).unwrap();
    assert!(script.contains("OpenStandardInput().CopyTo"));
    assert!(script.contains("Get-FileHash -Algorithm SHA256"));
    assert!(script.contains("[IO.File]::Move"));
    assert!(script.contains(&digest(b"windows server fixture\0\xff")));
    assert!(bootstrap.runner.steps.lock().unwrap().is_empty());
}

#[test]
fn incompatible_cached_server_is_replaced_by_the_selected_artifact() {
    let platform = Platform {
        os: OperatingSystem::Linux,
        arch: Architecture::X86_64,
    };
    let mut artifacts = ArtifactCatalog::default();
    artifacts.insert(ServerArtifact {
        platform,
        version: "test".to_owned(),
        sha256: "b".repeat(64),
        source: ArtifactSource::Download {
            url: "https://example.invalid/server".to_owned(),
        },
    });
    let old = RemoteInfo {
        platform,
        version: "test".to_owned(),
        protocol_version: crate::PROXY_PROTOCOL_VERSION,
        binary_sha256: "a".repeat(64),
    };
    let old = serde_json::to_vec(&old).unwrap();
    let new = RemoteInfo {
        platform,
        version: "test".to_owned(),
        protocol_version: crate::PROXY_PROTOCOL_VERSION,
        binary_sha256: "b".repeat(64),
    };
    let runner = ScriptedRunner {
        steps: Mutex::new(VecDeque::from([
            step(true, b"Linux\nx86_64\n"),
            step(true, &old),
            step(true, &old),
            step(true, b""),
            step(true, &serde_json::to_vec(&new).unwrap()),
        ])),
        commands: Mutex::new(vec![]),
    };
    let bootstrap = SshBootstrap {
        runner,
        version: "test".to_owned(),
        artifacts,
        server_binary: None,
        file_system: std::sync::Arc::new(zess_platform::NativeFileSystem),
        platform: std::sync::Arc::new(zess_platform::NativePlatform),
    };
    let server = bootstrap
        .prepare(&SshTarget::new("fixture".to_owned()))
        .unwrap();
    assert!(server.command().contains(&"b".repeat(64)));
    assert!(bootstrap.runner.commands.lock().unwrap()[3].contains("curl --fail"));
    assert!(bootstrap.runner.steps.lock().unwrap().is_empty());
}

#[test]
fn remote_workspace_names_are_preserved_as_data() {
    let workspace = "C:\\work tree\\quote'\";$(touch hacked)\\项目";
    let request = ProxyRequest::new(ProxyOperation::Attach {
        workspace: workspace.to_owned(),
        files: vec!["../file".to_owned()],
        config: None,
    });
    let mut bytes = vec![];
    write_request(&mut bytes, &request).unwrap();
    bytes.extend_from_slice(b"protocol bytes\n");
    let mut reader = BufReader::new(Cursor::new(bytes));
    let decoded: ProxyRequest = read_request(&mut reader).unwrap();
    decoded.validate().unwrap();
    let ProxyOperation::Attach {
        workspace: decoded, ..
    } = decoded.operation
    else {
        panic!("wrong request")
    };
    assert_eq!(decoded, workspace);
    let mut remainder = String::new();
    std::io::Read::read_to_string(&mut reader, &mut remainder).unwrap();
    assert_eq!(remainder, "protocol bytes\n");
}

#[test]
fn ssh_uses_system_configuration_without_tty_or_forwarding() {
    let mut target = SshTarget::new("my-server".to_owned());
    target.config_file = Some("config with spaces".into());
    target.identity_file = Some("identity with spaces".into());
    target.jump_host = Some("jump".to_owned());
    let spec = target.command("zess remote-proxy").unwrap();
    assert!(spec.env.is_empty());
    assert!(matches!(
        spec.console_mode,
        zess_platform::ConsoleMode::Inherit
    ));
    let args: Vec<_> = spec.args.iter().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(
        args,
        [
            "-T",
            "-o",
            "ClearAllForwardings=yes",
            "-i",
            "identity with spaces",
            "-F",
            "config with spaces",
            "-J",
            "jump",
            "--",
            "my-server",
            "zess remote-proxy"
        ]
    );
    assert!(
        SshTarget::new("-oProxyCommand=bad".to_owned())
            .command("ignored")
            .is_err()
    );
}
