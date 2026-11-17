#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use zess_platform::{Cancellation, OwnedProcess, ProcessRunner, ProcessSpec, SystemProcessRunner};
use zess_remote::{
    Connector, ProxyOperation, ProxyRequest, SshConnection, SshConnector, SshTarget,
};

struct NeverCancel;

impl Cancellation for NeverCancel {
    fn check(&self) -> Result<()> {
        Ok(())
    }
}

struct SshFixture {
    directory: tempfile::TempDir,
    daemon: OwnedProcess,
    target: SshTarget,
}

impl SshFixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        for name in ["host", "client"] {
            run(
                "ssh-keygen",
                &[
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-f",
                    root.join(name).to_str().unwrap(),
                ],
            )?;
        }
        let username = String::from_utf8(run("id", &["-un"])?)?.trim().to_owned();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let shell = String::from_utf8(run("sh", &["-c", "command -v sh"])?)?
            .trim()
            .to_owned();
        let path = format!("{}:{}", root.display(), std::env::var("PATH")?);
        let wrapper = root.join("wrapper.sh");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport HOME='{}'\nexport PATH='{path}'\nexec '{shell}' -c \"$SSH_ORIGINAL_COMMAND\"\n",
                root.display()
            ),
        )?;
        let sshd_config = root.join("sshd_config");
        std::fs::write(
            &sshd_config,
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {}/host\nAuthorizedKeysFile {}/client.pub\nPidFile {}/sshd.pid\nStrictModes no\nSetEnv \"PATH={path}\" \"XDG_CONFIG_HOME={}/empty-config\"\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nAllowUsers {username}\nForceCommand {shell} {}\nLogLevel ERROR\n",
                root.display(),
                root.display(),
                root.display(),
                root.display(),
                wrapper.display()
            ),
        )?;
        let sshd = String::from_utf8(run("sh", &["-c", "command -v sshd"])?)?
            .trim()
            .to_owned();
        run(&sshd, &["-t", "-f", sshd_config.to_str().unwrap()])?;
        let mut spec = ProcessSpec::new(sshd);
        spec.args(["-D", "-e", "-f"]).arg(&sshd_config);
        let mut daemon = SystemProcessRunner.spawn(&spec)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if daemon.try_wait()?.is_some() || Instant::now() > deadline {
                let _ = daemon.kill();
                let mut stderr = String::new();
                if let Some(mut pipe) = daemon.take_stderr() {
                    pipe.read_to_string(&mut stderr)?;
                }
                anyhow::bail!("fixture sshd did not start: {stderr}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let public_key = std::fs::read_to_string(root.join("host.pub"))?;
        std::fs::write(
            root.join("known_hosts"),
            format!("[127.0.0.1]:{port} {public_key}"),
        )?;
        let config = root.join("ssh_config");
        std::fs::write(
            &config,
            format!(
                "Host fixture\n HostName 127.0.0.1\n Port {port}\n User {username}\n IdentityFile {}/client\n IdentitiesOnly yes\n IdentityAgent none\n BatchMode yes\n StrictHostKeyChecking yes\n UserKnownHostsFile {}/known_hosts\n GlobalKnownHostsFile /dev/null\n ControlMaster no\n",
                root.display(),
                root.display()
            ),
        )?;
        let binary = root.join("zess");
        let hello = serde_json::to_string(&zess_protocol::ServerMessage::Hello {
            build_id: zess_build::BUILD_ID.into(),
        })?;
        let script = format!(
            "#!/bin/sh\nset -eu\n[ \"$1\" = remote-proxy ]\nIFS= read -r hello\nprintf '%s\\n' '{hello}'\nIFS= read -r request\nprintf '%s\\n' '{{\"status\":\"ready\",\"session_id\":null}}'\nprintf '%s\\n' \"$request\"\nexec cat\n"
        );
        std::fs::write(&binary, script)?;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
        let mut target = SshTarget::new("fixture".to_owned());
        target.config_file = Some(config);
        Ok(Self {
            directory,
            daemon,
            target,
        })
    }
}

impl Drop for SshFixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn run(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let mut spec = ProcessSpec::new(program);
    spec.args(args);
    let output = SystemProcessRunner
        .output(&spec, None, &NeverCancel)
        .with_context(|| format!("test fixture requires {program}"))?;
    anyhow::ensure!(
        output.status.success(),
        "fixture command {program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

#[test]
fn ssh_uses_preinstalled_server_and_preserves_workspace_data() -> Result<()> {
    let fixture = SshFixture::new()?;
    let workspace = "work space/'\";$()/项目";
    let request = ProxyRequest::new(ProxyOperation::Attach {
        workspace: workspace.to_owned(),
        files: vec!["a b".to_owned()],
        config: None,
    });
    let encoded = serde_json::to_string(&request)?;
    let mut connector = SshConnector {
        bootstrap: std::sync::Arc::new(Preinstalled),
        target: fixture.target.clone(),
        runner: SystemProcessRunner,
        request,
    };
    let connection = connector.connect()?;
    let mut connection = BufReader::new(connection);
    let mut actual = String::new();
    connection.read_line(&mut actual)?;
    assert_eq!(actual.trim_end(), encoded);
    connection.get_mut().write_all(b"protocol payload\n")?;
    connection.get_mut().flush()?;
    actual.clear();
    connection.read_line(&mut actual)?;
    assert_eq!(actual, "protocol payload\n");
    drop(connection);
    Ok(())
}

#[test]
fn ssh_rejects_a_host_key_absent_from_known_hosts() -> Result<()> {
    let fixture = SshFixture::new()?;
    std::fs::write(fixture.directory.path().join("known_hosts"), "")?;
    let mut connector = SshConnector {
        bootstrap: std::sync::Arc::new(Preinstalled),
        target: fixture.target.clone(),
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    assert!(connector.connect().is_err());
    Ok(())
}

#[test]
fn a_stalled_peer_does_not_block_client_reads_or_writes() -> Result<()> {
    let mut spec = ProcessSpec::new("sh");
    spec.args(["-c", "sleep 30"]);
    let mut connection = SshConnection::new(SystemProcessRunner.spawn(&spec)?)?;
    connection.set_nonblocking(true)?;
    assert_eq!(
        connection.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let payload = vec![0; 16384];
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut written = 0;
    loop {
        match connection.write(&payload) {
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(
            Instant::now() < deadline && written < 1024 * 1024,
            "outbound queue did not apply backpressure"
        );
    }
    let start = Instant::now();
    drop(connection);
    assert!(start.elapsed() < Duration::from_secs(2));
    Ok(())
}

#[test]
fn cancelling_a_connection_interrupts_the_remote_ready_wait() -> Result<()> {
    let fixture = SshFixture::new()?;
    let marker = fixture.directory.path().join("waiting-for-ready");
    let hello = serde_json::to_string(&zess_protocol::ServerMessage::Hello {
        build_id: zess_build::BUILD_ID.into(),
    })?;
    let script = format!(
        "#!/bin/sh\nIFS= read -r hello\nprintf '%s\\n' '{hello}'\nIFS= read -r request\ntouch '{}'\nexec cat >/dev/null\n",
        marker.display()
    );
    std::fs::write(fixture.directory.path().join("zess"), script)?;
    let token = zess_platform::CancellationToken::new();
    let mut target = fixture.target.clone();
    target.cancellation = Some(token.clone());
    let mut connector = SshConnector {
        bootstrap: std::sync::Arc::new(Preinstalled),
        target,
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    thread::scope(|scope| -> Result<()> {
        let cancellation = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !marker.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let reached = marker.exists();
            token.cancel();
            reached
        });
        let error = match connector.connect() {
            Ok(_) => anyhow::bail!("unready proxy unexpectedly connected"),
            Err(error) => format!("{error:#}"),
        };
        assert!(cancellation.join().unwrap());
        assert!(error.contains("cancelled"), "{error}");
        Ok(())
    })
}

#[test]
fn cancelling_after_ready_interrupts_a_blocking_response_read() -> Result<()> {
    let fixture = SshFixture::new()?;
    let hello = serde_json::to_string(&zess_protocol::ServerMessage::Hello {
        build_id: zess_build::BUILD_ID.into(),
    })?;
    let script = format!(
        "#!/bin/sh\nIFS= read -r hello\nprintf '%s\\n' '{hello}'\nIFS= read -r request\nprintf '%s\\n' '{{\"status\":\"ready\",\"session_id\":null}}'\nexec cat >/dev/null\n"
    );
    std::fs::write(fixture.directory.path().join("zess"), script)?;
    let token = zess_platform::CancellationToken::new();
    let mut target = fixture.target.clone();
    target.cancellation = Some(token.clone());
    let mut connector = SshConnector {
        bootstrap: std::sync::Arc::new(Preinstalled),
        target,
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    let mut connection = connector.connect()?;
    thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(50));
            token.cancel();
        });
        let error = connection.read(&mut [0]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    });
    Ok(())
}

#[test]
fn waiting_for_ssh_exit_reports_failure_status() -> Result<()> {
    let mut spec = ProcessSpec::new("sh");
    spec.args(["-c", "exit 7"]);
    let mut connection = SshConnection::new(SystemProcessRunner.spawn(&spec)?)?;
    let error = connection
        .wait_for_exit(Duration::from_secs(2))
        .unwrap_err();
    assert!(error.to_string().contains("exit status: 7"), "{error}");
    Ok(())
}

#[test]
fn cancelling_interrupts_wait_for_ssh_exit() -> Result<()> {
    let fixture = SshFixture::new()?;
    let hello = serde_json::to_string(&zess_protocol::ServerMessage::Hello {
        build_id: zess_build::BUILD_ID.into(),
    })?;
    let script = format!(
        "#!/bin/sh\nIFS= read -r hello\nprintf '%s\\n' '{hello}'\nIFS= read -r request\nprintf '%s\\n' '{{\"status\":\"ready\",\"session_id\":null}}'\nexec cat >/dev/null\n"
    );
    std::fs::write(fixture.directory.path().join("zess"), script)?;
    let token = zess_platform::CancellationToken::new();
    let mut target = fixture.target.clone();
    target.cancellation = Some(token.clone());
    let mut connector = SshConnector {
        bootstrap: std::sync::Arc::new(Preinstalled),
        target,
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    let mut connection = connector.connect()?;
    thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(50));
            token.cancel();
        });
        let error = connection
            .wait_for_exit(Duration::from_secs(2))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    });
    Ok(())
}

struct Preinstalled;

impl zess_remote::ServerBootstrap for Preinstalled {
    fn prepare(
        &self,
        _: &SshTarget,
        _: &dyn zess_platform::ProcessRunner,
    ) -> anyhow::Result<String> {
        Ok("zess remote-proxy".into())
    }
}

struct FixtureArtifacts {
    path: std::path::PathBuf,
    hash: String,
    requests: std::sync::atomic::AtomicUsize,
}

struct HeldOpenRunner;

impl ProcessRunner for HeldOpenRunner {
    fn output(
        &self,
        spec: &ProcessSpec,
        input: Option<&[u8]>,
        cancellation: &dyn Cancellation,
    ) -> Result<std::process::Output> {
        SystemProcessRunner.output(spec, input, cancellation)
    }

    fn output_with_progress(
        &self,
        spec: &ProcessSpec,
        input: Option<&[u8]>,
        cancellation: &dyn Cancellation,
        progress: &dyn zess_platform::InputProgress,
    ) -> Result<std::process::Output> {
        let input = input.context("upload input missing")?;
        let mut process = SystemProcessRunner.spawn(spec)?;
        let mut stdin = process.take_stdin().context("SSH stdin missing")?;
        let mut stdout = process.take_stdout().context("SSH stdout missing")?;
        let mut stderr = process.take_stderr().context("SSH stderr missing")?;
        stdin.write_all(input)?;
        stdin.flush()?;
        progress.written(input.len() as u64, input.len() as u64);
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            cancellation.check()?;
            if let Some(status) = process.try_wait()? {
                break status;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "SSH installation waited for stdin EOF"
            );
            thread::sleep(Duration::from_millis(10));
        };
        let mut output = Vec::new();
        let mut errors = Vec::new();
        stdout.read_to_end(&mut output)?;
        stderr.read_to_end(&mut errors)?;
        drop(stdin);
        Ok(std::process::Output {
            status,
            stdout: output,
            stderr: errors,
        })
    }

    fn spawn(&self, spec: &ProcessSpec) -> Result<OwnedProcess> {
        SystemProcessRunner.spawn(spec)
    }
}

impl zess_remote::ServerArtifacts for FixtureArtifacts {
    fn get(
        &self,
        target: zess_remote::ServerTarget,
        cancellation: &dyn Cancellation,
        _: Option<&dyn zess_remote::ProgressReporter>,
    ) -> Result<zess_remote::ServerArtifact> {
        cancellation.check()?;
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(zess_remote::ServerArtifact {
            target,
            path: self.path.clone(),
            sha256: self.hash.clone(),
        })
    }
}

fn standard_tools(directory: &std::path::Path) -> Result<PathBuf> {
    let tools = directory.join("standard-tools");
    std::fs::create_dir(&tools)?;
    for name in [
        "sh",
        "uname",
        "sha256sum",
        "head",
        "wc",
        "cat",
        "mktemp",
        "mkdir",
        "tar",
        "gzip",
        "rm",
        "chmod",
        "ln",
        "mv",
    ] {
        let command = format!("command -v {name}");
        let path = String::from_utf8(run("sh", &["-c", &command])?)?;
        std::os::unix::fs::symlink(path.trim(), tools.join(name))?;
    }
    Ok(tools)
}

#[test]
fn ssh_deploys_and_reuses_a_verified_server() -> Result<()> {
    use sha2::{Digest, Sha256};
    use zess_remote::{ProgressReporter, RemoteProgress, RemoteStage, ServerBootstrap};
    struct Reporter(std::sync::Mutex<Vec<RemoteProgress>>);
    impl ProgressReporter for Reporter {
        fn report(&self, event: RemoteProgress) {
            self.0.lock().unwrap().push(event);
        }
    }
    let fixture = SshFixture::new()?;
    let reporter = std::sync::Arc::new(Reporter(std::sync::Mutex::new(Vec::new())));
    let mut target = fixture.target.clone();
    target.progress = Some(reporter.clone());
    let wrapper = fixture.directory.path().join("wrapper.sh");
    let tools = standard_tools(fixture.directory.path())?;
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport HOME='{}'\nexport PATH='{}'\nexec /bin/sh -c \"$SSH_ORIGINAL_COMMAND\"\n",
            fixture.directory.path().display(),
            tools.display()
        ),
    )?;
    let package = fixture.directory.path().join("package");
    std::fs::create_dir_all(package.join("lib"))?;
    std::fs::copy(fixture.directory.path().join("zess"), package.join("zess"))?;
    std::fs::write(package.join("patchelf"), "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(
        package.join("patchelf"),
        std::fs::Permissions::from_mode(0o700),
    )?;
    std::fs::write(package.join("lib/ld-linux-x86-64.so.2"), "loader")?;
    let path = fixture.directory.path().join("server.tar.gz");
    run(
        "tar",
        &[
            "-czf",
            path.to_str().unwrap(),
            "-C",
            package.to_str().unwrap(),
            ".",
        ],
    )?;
    let bytes = std::fs::read(&path)?;
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let artifacts = std::sync::Arc::new(FixtureArtifacts {
        path,
        hash,
        requests: 0.into(),
    });
    let bootstrap = zess_remote::SshBootstrap {
        artifacts: artifacts.clone(),
        filesystem: std::sync::Arc::new(zess_platform::NativeFileSystem),
    };
    let command = bootstrap.prepare(&target, &HeldOpenRunner)?;
    assert!(
        reporter
            .0
            .lock()
            .unwrap()
            .contains(&RemoteProgress::Transfer {
                stage: RemoteStage::Uploading,
                completed: bytes.len() as u64,
                total: Some(bytes.len() as u64),
            })
    );
    assert!(command.contains(zess_build::BUILD_ID));
    assert_eq!(bootstrap.prepare(&target, &SystemProcessRunner)?, command);
    assert_eq!(
        artifacts.requests.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let installed = fixture
        .directory
        .path()
        .join(".cache/zess/servers")
        .join(zess_build::BUILD_ID)
        .join("current");
    std::fs::write(installed.join("lib/ld-linux-x86-64.so.2"), "corrupt")?;
    assert_eq!(bootstrap.prepare(&target, &SystemProcessRunner)?, command);
    assert_eq!(
        artifacts.requests.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    let mut connector = SshConnector {
        target,
        bootstrap: std::sync::Arc::new(bootstrap),
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    let connection = connector.connect()?;
    let mut reader = BufReader::new(connection);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    assert!(line.contains("status"));
    Ok(())
}

#[test]
fn ssh_rejects_an_artifact_with_the_wrong_hash() -> Result<()> {
    use zess_remote::ServerBootstrap;
    let fixture = SshFixture::new()?;
    let artifacts = std::sync::Arc::new(FixtureArtifacts {
        path: fixture.directory.path().join("zess"),
        hash: "0".repeat(64),
        requests: 0.into(),
    });
    let bootstrap = zess_remote::SshBootstrap {
        artifacts,
        filesystem: std::sync::Arc::new(zess_platform::NativeFileSystem),
    };
    assert!(
        format!(
            "{:#}",
            bootstrap
                .prepare(&fixture.target, &SystemProcessRunner)
                .unwrap_err()
        )
        .contains("SHA-256 mismatch")
    );
    assert!(
        !fixture
            .directory
            .path()
            .join(".cache/zess/servers")
            .exists()
    );
    Ok(())
}

#[test]
#[ignore = "requires ZESS_TEST_SERVER_ARCHIVE containing a GNU Zess runtime package"]
fn installed_server_starts_a_session_with_its_own_executable() -> Result<()> {
    use sha2::{Digest, Sha256};
    use zess_remote::ServerBootstrap;

    let fixture = SshFixture::new()?;
    let runtime = fixture.directory.path().join("runtime");
    std::fs::create_dir(&runtime)?;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    let tools = standard_tools(fixture.directory.path())?;
    std::fs::write(
        fixture.directory.path().join("wrapper.sh"),
        format!(
            "#!/bin/sh\nexport HOME='{}' XDG_RUNTIME_DIR='{}' XDG_CONFIG_HOME='{}' PATH='{}'\nexec /bin/sh -c \"$SSH_ORIGINAL_COMMAND\"\n",
            fixture.directory.path().display(),
            runtime.display(),
            fixture.directory.path().join("config").display(),
            tools.display()
        ),
    )?;
    let path = PathBuf::from(
        std::env::var_os("ZESS_TEST_SERVER_ARCHIVE")
            .context("ZESS_TEST_SERVER_ARCHIVE is required")?,
    );
    let bytes = std::fs::read(&path)?;
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let bootstrap = zess_remote::SshBootstrap {
        artifacts: std::sync::Arc::new(FixtureArtifacts {
            path,
            hash,
            requests: 0.into(),
        }),
        filesystem: std::sync::Arc::new(zess_platform::NativeFileSystem),
    };
    bootstrap.prepare(&fixture.target, &SystemProcessRunner)?;
    let executable = fixture
        .directory
        .path()
        .join(".cache/zess/servers")
        .join(zess_build::BUILD_ID)
        .join("current/zess");
    let workspace = fixture.directory.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let invoke = |args: &str| -> Result<Vec<u8>> {
        let command = format!("'{}' {args}", executable.display());
        let output =
            SystemProcessRunner.output(&fixture.target.command(&command)?, None, &NeverCancel)?;
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.stdout)
    };
    let result = invoke(&format!(
        "session new --workspace '{}' --json",
        workspace.display()
    ))?;
    let session: serde_json::Value = serde_json::from_slice(&result)?;
    let id = session["id"].as_str().context("missing session ID")?;
    invoke(&format!("session stop '{id}' --force"))?;
    Ok(())
}

#[test]
fn ssh_diagnostics_are_reported_to_the_observer() -> Result<()> {
    struct Reporter(std::sync::Mutex<Vec<zess_remote::RemoteProgress>>);
    impl zess_remote::ProgressReporter for Reporter {
        fn report(&self, event: zess_remote::RemoteProgress) {
            self.0.lock().unwrap().push(event);
        }
    }
    let fixture = SshFixture::new()?;
    let hello = serde_json::to_string(&zess_protocol::ServerMessage::Hello {
        build_id: zess_build::BUILD_ID.into(),
    })?;
    std::fs::write(
        fixture.directory.path().join("zess"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' 'ssh diagnostic' >&2\nIFS= read -r hello\nprintf '%s\\n' '{hello}'\nIFS= read -r request\nprintf '%s\\n' '{{\"status\":\"ready\",\"session_id\":null}}'\nexec cat >/dev/null\n"
        ),
    )?;
    let reporter = std::sync::Arc::new(Reporter(std::sync::Mutex::new(Vec::new())));
    let mut target = fixture.target.clone();
    target.progress = Some(reporter.clone());
    let mut connector = SshConnector {
        target,
        bootstrap: std::sync::Arc::new(Preinstalled),
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    let connection = connector.connect()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let events = reporter.0.lock().unwrap();
        if events.iter().any(|event| matches!(event, zess_remote::RemoteProgress::Diagnostic(message) if message.contains("ssh diagnostic"))) {
            break;
        }
        drop(events);
        anyhow::ensure!(Instant::now() < deadline, "diagnostic was not reported");
        thread::sleep(Duration::from_millis(10));
    }
    drop(connection);
    Ok(())
}

#[test]
fn cancelling_an_ssh_upload_removes_only_the_new_staging_directory() -> Result<()> {
    use sha2::{Digest, Sha256};
    use zess_remote::ServerBootstrap;

    struct InterruptedUpload {
        base: PathBuf,
        cancellation: zess_platform::CancellationToken,
    }
    impl ProcessRunner for InterruptedUpload {
        fn output(
            &self,
            spec: &ProcessSpec,
            input: Option<&[u8]>,
            cancellation: &dyn Cancellation,
        ) -> Result<std::process::Output> {
            SystemProcessRunner.output(spec, input, cancellation)
        }
        fn output_with_progress(
            &self,
            spec: &ProcessSpec,
            input: Option<&[u8]>,
            _: &dyn Cancellation,
            _: &dyn zess_platform::InputProgress,
        ) -> Result<std::process::Output> {
            let bytes = input.context("missing upload")?;
            let mut process = SystemProcessRunner.spawn(spec)?;
            let mut stdin = process.take_stdin().context("missing SSH stdin")?;
            stdin.write_all(&bytes[..bytes.len() / 2])?;
            stdin.flush()?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while std::fs::read_dir(&self.base)?.count() < 2 {
                anyhow::ensure!(Instant::now() < deadline, "remote upload did not start");
                thread::sleep(Duration::from_millis(10));
            }
            self.cancellation.cancel();
            drop(process);
            drop(stdin);
            self.cancellation.check()?;
            unreachable!()
        }
        fn spawn(&self, spec: &ProcessSpec) -> Result<OwnedProcess> {
            SystemProcessRunner.spawn(spec)
        }
    }

    let fixture = SshFixture::new()?;
    let package = fixture.directory.path().join("package");
    std::fs::create_dir_all(package.join("lib"))?;
    std::fs::copy(fixture.directory.path().join("zess"), package.join("zess"))?;
    std::fs::write(package.join("patchelf"), "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(
        package.join("patchelf"),
        std::fs::Permissions::from_mode(0o700),
    )?;
    std::fs::write(package.join("lib/ld-linux-x86-64.so.2"), "loader")?;
    let path = fixture.directory.path().join("server.tar.gz");
    run(
        "tar",
        &[
            "-czf",
            path.to_str().unwrap(),
            "-C",
            package.to_str().unwrap(),
            ".",
        ],
    )?;
    let bytes = std::fs::read(&path)?;
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let base = fixture
        .directory
        .path()
        .join(".cache/zess/servers")
        .join(zess_build::BUILD_ID);
    let existing = base.join("existing");
    std::fs::create_dir_all(&existing)?;
    std::fs::write(existing.join("keep"), "active installation")?;
    let token = zess_platform::CancellationToken::new();
    let mut target = fixture.target.clone();
    target.cancellation = Some(token.clone());
    let bootstrap = zess_remote::SshBootstrap {
        artifacts: std::sync::Arc::new(FixtureArtifacts {
            path,
            hash,
            requests: 0.into(),
        }),
        filesystem: std::sync::Arc::new(zess_platform::NativeFileSystem),
    };
    let error = bootstrap
        .prepare(
            &target,
            &InterruptedUpload {
                base: base.clone(),
                cancellation: token,
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("cancelled"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(&base)?.count() != 1 {
        anyhow::ensure!(
            Instant::now() < deadline,
            "remote staging directory was not removed"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(existing.join("keep"))?,
        "active installation"
    );
    Ok(())
}
