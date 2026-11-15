#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
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
