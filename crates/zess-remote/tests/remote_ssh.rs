#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use zess_platform::{
    Cancellation, CancellationToken, OwnedProcess, ProcessRunner, ProcessSpec, SystemProcessRunner,
};
use zess_remote::{
    ArtifactCatalog, Bootstrap, Connector, Platform, ProxyOperation, ProxyRequest, RemoteServer,
    SshBootstrap, SshConnection, SshConnector, SshTarget,
};

struct NeverCancel;

struct CommandBootstrap(String);

impl Bootstrap for CommandBootstrap {
    fn prepare(&self, _: &SshTarget) -> Result<RemoteServer> {
        Ok(RemoteServer::new(Platform::current()?, self.0.clone()))
    }
}

impl Cancellation for NeverCancel {
    fn check(&self) -> Result<()> {
        Ok(())
    }
}

struct SshFixture {
    directory: tempfile::TempDir,
    daemon: OwnedProcess,
    target: SshTarget,
    binary: PathBuf,
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
        let path = std::env::var("PATH")?;
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
        let binary = root.join("server with spaces '");
        let platform = Platform::current()?;
        let platform = serde_json::to_string(&platform)?;
        let protocol_version = zess_remote::PROXY_PROTOCOL_VERSION;
        let script = format!(
            "#!/bin/sh\nset -eu\ncase \"$1\" in\nremote-info)\nif command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum \"$0\"); else h=$(shasum -a 256 \"$0\"); fi\nh=${{h%% *}}\nprintf '%s\\n' '{{\"version\":\"fixture\",\"protocol_version\":{protocol_version},\"platform\":{platform},\"binary_sha256\":\"'\"$h\"'\"}}'\n;;\nremote-proxy)\nIFS= read -r request\nprintf '%s\\n' '{{\"status\":\"ready\",\"protocol_version\":{protocol_version}}}'\nprintf '%s\\n' \"$request\"\nexec cat\n;;\nesac\n"
        );
        std::fs::write(&binary, script)?;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
        let mut target = SshTarget::new("fixture".to_owned());
        target.config_file = Some(config);
        Ok(Self {
            directory,
            daemon,
            target,
            binary,
        })
    }

    fn bootstrap(&self) -> SshBootstrap<SystemProcessRunner> {
        SshBootstrap {
            runner: SystemProcessRunner,
            version: "fixture".to_owned(),
            artifacts: ArtifactCatalog::default(),
            server_binary: Some(self.binary.clone()),
            file_system: std::sync::Arc::new(zess_platform::NativeFileSystem),
            platform: std::sync::Arc::new(zess_platform::NativePlatform),
        }
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
fn ssh_uploads_then_reuses_the_server_and_preserves_workspace_data() -> Result<()> {
    let fixture = SshFixture::new()?;
    let workspace = "work space/'\";$()/项目";
    let request = ProxyRequest::new(ProxyOperation::Attach {
        workspace: workspace.to_owned(),
        files: vec!["a b".to_owned()],
        config: None,
    });
    let encoded = serde_json::to_string(&request)?;
    let mut connector = SshConnector {
        target: fixture.target.clone(),
        bootstrap: fixture.bootstrap(),
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
    let cache = fixture.directory.path().join(".cache/zess/servers");
    let entries: Vec<_> = std::fs::read_dir(cache)?.collect::<std::io::Result<_>>()?;
    assert_eq!(entries.len(), 1);
    let installed = entries[0].path().join("zess");
    assert_eq!(std::fs::read(&installed)?, std::fs::read(&fixture.binary)?);
    let modified = std::fs::metadata(&installed)?.modified()?;
    fixture.bootstrap().prepare(&fixture.target)?;
    assert_eq!(std::fs::metadata(&installed)?.modified()?, modified);
    Ok(())
}

#[test]
fn ssh_rejects_a_host_key_absent_from_known_hosts() -> Result<()> {
    let fixture = SshFixture::new()?;
    std::fs::write(fixture.directory.path().join("known_hosts"), "")?;
    let error = match fixture.bootstrap().prepare(&fixture.target) {
        Ok(_) => anyhow::bail!("unexpectedly accepted an unknown host key"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("Host key verification failed"), "{error}");
    assert!(!Path::new(&fixture.directory.path().join(".cache/zess")).exists());
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
    let token = CancellationToken::new();
    let mut target = fixture.target.clone();
    target.cancellation = Some(token.clone());
    let mut connector = SshConnector {
        target,
        bootstrap: CommandBootstrap(format!(
            "sh -c 'IFS= read -r request; touch {}; cat >/dev/null'",
            marker.display()
        )),
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    let started = Instant::now();
    thread::scope(|scope| -> Result<()> {
        let cancellation = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !marker.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let ready_wait_reached = marker.exists();
            token.cancel();
            ready_wait_reached
        });
        let result = connector.connect();
        assert!(
            cancellation.join().unwrap(),
            "proxy did not receive its initial request"
        );
        let error = match result {
            Ok(_) => anyhow::bail!("unready proxy unexpectedly connected"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains("cancelled"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        Ok(())
    })
}

#[test]
fn cancelling_after_ready_interrupts_a_blocking_response_read() -> Result<()> {
    let fixture = SshFixture::new()?;
    let token = CancellationToken::new();
    let mut target = fixture.target.clone();
    target.cancellation = Some(token.clone());
    let ready = serde_json::to_string(&zess_remote::ProxyHandshake::ready())?;
    let script = format!("IFS= read -r request; printf '%s\\n' '{ready}'; cat >/dev/null");
    let mut connector = SshConnector {
        target,
        bootstrap: CommandBootstrap(format!("sh -c '{}'", script.replace('\'', "'\\''"))),
        runner: SystemProcessRunner,
        request: ProxyRequest::new(ProxyOperation::Status),
    };
    let mut connection = connector.connect()?;
    let started = Instant::now();
    thread::scope(|scope| -> Result<()> {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(50));
            token.cancel();
        });
        let error = connection.read(&mut [0]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
        assert!(error.to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
        Ok(())
    })
}
