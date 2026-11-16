#![cfg(target_os = "linux")]

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use zess_platform::SystemProcessRunner;
use zess_remote::{
    Connector, ProxyOperation, ProxyRequest, SshConnection, SshConnector, SshTarget,
};

#[test]
fn ssh_addresses_select_the_remote_workspace_and_connection() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("ssh");
    std::fs::write(
        &executable,
        r##"#!/bin/sh
printf '%s\n' "$@" > "$ZESS_CAPTURE.args"
IFS= read -r hello
printf '%s\n' "$hello"
IFS= read -r request
printf '%s\n' "$request" > "$ZESS_CAPTURE.request"
printf '%s\n' '{"status":"failed","message":"fixture stop"}'
"##,
    )?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let capture = directory.path().join("capture");
    for (uri, host, user, port, workspace) in [
        ("ssh://pca:/path", "pca", None, None, "/path"),
        (
            "ssh://pca/目录 with space",
            "pca",
            None,
            None,
            "/目录 with space",
        ),
        (
            "ssh://alice@pca:2222/work",
            "pca",
            Some("alice"),
            Some("2222"),
            "/work",
        ),
        ("ssh://[::1]:2200/work", "::1", None, Some("2200"), "/work"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_zess"))
            .arg(uri)
            .env("PATH", directory.path())
            .env("ZESS_CAPTURE", &capture)
            .output()?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("fixture stop"),
            "{uri}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let args = std::fs::read_to_string(capture.with_extension("args"))?;
        let args: Vec<_> = args.lines().collect();
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--" && pair[1] == host)
        );
        if let Some(user) = user {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "-l" && pair[1] == user)
            );
        }
        if let Some(port) = port {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "-p" && pair[1] == port)
            );
        }
        let request: Value =
            serde_json::from_slice(&std::fs::read(capture.with_extension("request"))?)?;
        assert_eq!(request["operation"]["workspace"], workspace);
    }
    Ok(())
}

#[test]
fn conflicting_ssh_addresses_are_rejected_before_connecting() -> Result<()> {
    for args in [
        vec!["ssh://pca:/path", "--ssh", "other"],
        vec!["ssh://pca:/path", "--remote-workspace", "/other"],
        vec!["ssh://alice@pca:22/path", "--ssh-user", "bob"],
        vec!["ssh://pca:22/path", "--ssh-port", "23"],
        vec!["ssh://pca:/path", "ssh://other:/path"],
        vec!["ssh://user:password@pca/path"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_zess"))
            .args(&args)
            .output()?;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("conflict")
                || stderr.contains("only one")
                || stderr.contains("password"),
            "{stderr}"
        );
    }
    Ok(())
}

#[test]
fn an_ssh_address_after_double_dash_remains_a_positional_argument() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_zess"))
        .args(["session", "attach", "--", "ssh://pca:/path"])
        .env("XDG_RUNTIME_DIR", directory.path())
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown session: ssh://pca:/path"));
    Ok(())
}

struct RemoteSession {
    directory: tempfile::TempDir,
    binary: PathBuf,
    host: String,
    ssh_config: PathBuf,
    sessions: Vec<String>,
    tmux_socket: String,
    pane_started: bool,
}

impl RemoteSession {
    fn new() -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("zess-ssh-sessions-")
            .tempdir()?;
        let tmux_socket = directory
            .path()
            .join("tmux.sock")
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            directory,
            binary: std::env::var_os("ZESS_TEST_SSH_BINARY").map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_zess"))),
            host: std::env::var("ZESS_TEST_SSH_HOST").unwrap_or_else(|_| "zess-fixture".to_owned()),
            ssh_config: std::env::var_os("ZESS_TEST_SSH_CONFIG").map(PathBuf::from)
                .context("set ZESS_TEST_SSH_CONFIG to an isolated localhost fixture with its own keys, known_hosts and remote runtime directories")?,
            sessions: Vec::new(),
            tmux_socket,
            pane_started: false,
        })
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command.arg("session").args(arguments);
        self.with_ssh(command)
    }

    fn with_ssh(&self, mut command: Command) -> Command {
        command
            .args(["--ssh", &self.host, "--ssh-config"])
            .arg(&self.ssh_config);
        command
    }

    fn run(&self, arguments: &[&str]) -> Result<Output> {
        Ok(self.command(arguments).output()?)
    }

    fn json(&self, arguments: &[&str]) -> Result<Value> {
        let output = self.run(arguments)?;
        ensure!(
            output.status.success(),
            "CLI {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "CLI {arguments:?} did not return JSON: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn entries(&self) -> Result<Vec<Value>> {
        let entries = self.json(&["list", "--json"])?;
        Ok(entries
            .as_array()
            .context("session list must return an array")?
            .clone())
    }

    fn connect_operation(&self, operation: ProxyOperation) -> Result<SshConnection> {
        let mut target = SshTarget::new(self.host.clone());
        target.config_file = Some(self.ssh_config.clone());
        let mut connector = SshConnector {
            target,
            runner: SystemProcessRunner,
            request: ProxyRequest::new(operation),
        };
        connector.connect()
    }

    fn protocol(&self, session: &str) -> Result<RemoteWire> {
        let mut connection = self.connect_operation(ProxyOperation::AttachSession {
            session_id: session.to_owned(),
        })?;
        zess_remote::write_request(
            &mut connection,
            &zess_protocol::ClientMessage::Hello {
                build_id: zess_build::BUILD_ID.into(),
            },
        )?;
        let hello: zess_protocol::ServerMessage =
            zess_remote::read_request(&mut io::BufReader::new(&mut connection))?;
        let zess_protocol::ServerMessage::Hello { build_id } = hello else {
            anyhow::bail!("missing server hello")
        };
        zess_protocol::check_build_id(&build_id)?;
        connection.set_nonblocking(true)?;
        Ok(RemoteWire {
            connection,
            pending: Vec::new(),
        })
    }

    fn tmux(&self, arguments: &[&str]) -> Result<String> {
        let output = Command::new("tmux")
            .args(["-S", &self.tmux_socket, "-f", "/dev/null"])
            .args(arguments)
            .output()
            .context("this external test requires tmux")?;
        ensure!(
            output.status.success(),
            "tmux {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    fn connect(&mut self, session: &str, file: Option<&Path>) -> Result<()> {
        let mut command = self.command(&["attach", session]);
        if let Some(file) = file {
            command.arg(file);
        }
        self.connect_terminal(command)
    }

    fn connect_workspace(&mut self, workspace: &Path, file: &Path) -> Result<()> {
        let mut command = Command::new(&self.binary);
        command
            .arg("attach")
            .arg("--remote-workspace")
            .arg(workspace)
            .arg(file);
        self.connect_terminal(self.with_ssh(command))
    }

    fn connect_terminal(&mut self, command: Command) -> Result<()> {
        let args: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        let mut tmux = Command::new("tmux");
        tmux.args(["-S", &self.tmux_socket, "-f", "/dev/null"]);
        if self.pane_started {
            tmux.args(["respawn-pane", "-k", "-t", "ssh:0.0"]);
        } else {
            tmux.args(["new-session", "-d", "-s", "ssh", "-x", "110", "-y", "30"]);
        }
        let output = tmux.arg(&self.binary).args(args).output()?;
        ensure!(
            output.status.success(),
            "start terminal: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.pane_started = true;
        self.tmux(&["set-option", "-t", "ssh", "remain-on-exit", "on"])?;
        Ok(())
    }

    fn screen(&self) -> Result<String> {
        self.tmux(&["capture-pane", "-p", "-t", "ssh:0.0"])
    }

    fn wait_screen(&self, predicate: impl Fn(&str) -> bool) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let screen = self.screen()?;
            if predicate(&screen) {
                return Ok(screen);
            }
            ensure!(
                Instant::now() < deadline,
                "terminal did not reach the expected state:\n{screen}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn keys(&self, keys: &[&str]) -> Result<()> {
        let mut arguments = vec!["send-keys", "-t", "ssh:0.0"];
        arguments.extend(keys);
        self.tmux(&arguments)?;
        Ok(())
    }

    fn literal(&self, text: &str) -> Result<()> {
        self.tmux(&["send-keys", "-t", "ssh:0.0", "-l", text])?;
        Ok(())
    }

    fn normal(&self) -> Result<()> {
        self.keys(&["Escape"])?;
        self.wait_screen(|screen| screen.contains(" NOR "))?;
        Ok(())
    }

    fn save(&self) -> Result<()> {
        self.literal(":w")?;
        self.wait_screen(|screen| screen.lines().any(|line| line.starts_with(":w")))?;
        self.keys(&["Enter"])?;
        self.wait_screen(|screen| !screen.contains("[+]"))?;
        Ok(())
    }

    fn detach(&self) -> Result<()> {
        self.keys(&["C-z"])?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while self
            .tmux(&["display-message", "-p", "-t", "ssh:0.0", "#{pane_dead}"])?
            .trim()
            != "1"
        {
            ensure!(
                Instant::now() < deadline,
                "terminal did not detach:\n{}",
                self.screen()?
            );
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    fn wait_file(&self, file: &Path, expected: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let actual = std::fs::read_to_string(file)?;
            if actual == expected {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "saved file contained {actual:?}, expected {expected:?}:\n{}",
                self.screen()?
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

struct RemoteWire {
    connection: SshConnection,
    pending: Vec<u8>,
}

impl RemoteWire {
    fn send(&mut self, message: Value) -> Result<()> {
        self.connection.set_nonblocking(false)?;
        zess_remote::write_request(&mut self.connection, &message)?;
        self.connection.set_nonblocking(true)?;
        Ok(())
    }

    fn receive(&mut self, kind: &str) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                let message: Value =
                    serde_json::from_slice(&self.pending.drain(..=end).collect::<Vec<_>>())?;
                ensure!(
                    message["type"] != "rejected",
                    "server rejected the protocol request: {message}"
                );
                if message["type"] == kind {
                    return Ok(message);
                }
            }
            let mut bytes = [0; 8192];
            match self.connection.read(&mut bytes) {
                Ok(0) => anyhow::bail!("SSH stream closed while waiting for {kind}"),
                Ok(size) => self.pending.extend_from_slice(&bytes[..size]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error.into()),
            }
            ensure!(
                self.pending.len() <= zess_protocol::MAX_MESSAGE_BYTES,
                "oversized server message"
            );
            ensure!(
                Instant::now() < deadline,
                "no {kind} response from remote session"
            );
        }
    }

    fn input(&mut self, epoch: &Value, sequence: u64, key: &str) -> Result<()> {
        self.send(json!({"type":"input", "sequence":sequence, "key":key, "context_epoch":epoch}))?;
        let ack = self.receive("ack")?;
        assert_eq!(ack["acknowledged"], sequence);
        Ok(())
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        for session in &self.sessions {
            let _ = self.run(&["stop", "--session", session, "--force"]);
        }
        if self.pane_started {
            let _ = self.tmux(&["kill-server"]);
        }
    }
}

/// The SSH server must share this machine's temporary filesystem, isolate its runtime,
/// and have the same build of zess preinstalled on PATH.
#[test]
#[ignore = "requires an isolated localhost SSH fixture through ZESS_TEST_SSH_CONFIG and tmux"]
fn remote_sessions_keep_edits_and_undo_across_terminal_detachment() -> Result<()> {
    let mut fixture = RemoteSession::new()?;
    let missing = fixture.directory.path().join("workspace-does-not-exist");
    let started = Instant::now();
    let failed = fixture.connect_operation(ProxyOperation::Attach {
        workspace: missing.to_string_lossy().into_owned(),
        files: Vec::new(),
        config: None,
    });
    let error = match failed {
        Ok(_) => anyhow::bail!("nonexistent workspace unexpectedly attached"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("workspace-does-not-exist"), "{error}");
    assert!(error.contains("remote proxy:"), "{error}");
    ensure!(
        started.elapsed() < Duration::from_secs(30),
        "failed workspace handshake did not return promptly"
    );
    let workspace = fixture.directory.path().join("项目 空格");
    std::fs::create_dir(&workspace)?;
    let file = workspace.join("笔记 空格.txt");
    let config = workspace.join("init.w");
    std::fs::write(&file, "base\n")?;
    std::fs::write(
        &config,
        "set-session-options! : obj 'on-client-exit 'keep\n",
    )?;
    let info = fixture.json(&[
        "new",
        "--remote-workspace",
        workspace.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
        "--json",
    ])?;
    let original = info["session_id"]
        .as_str()
        .context("session ID missing")?
        .to_owned();
    fixture.sessions.push(original.clone());
    assert_eq!(info["workspace"], workspace.to_str().unwrap());
    fixture.connect_workspace(&workspace, &file)?;
    fixture.wait_screen(|screen| screen.contains("base") && screen.contains("笔记 空格.txt"))?;
    fixture.keys(&["i"])?;
    fixture.wait_screen(|screen| screen.contains(" INS "))?;
    fixture.literal("REMOTE_EDIT")?;
    fixture.wait_screen(|screen| screen.contains("REMOTE_EDITbase"))?;
    fixture.normal()?;
    fixture.save()?;
    fixture.wait_file(&file, "REMOTE_EDITbase\n")?;
    fixture.keys(&["i"])?;
    fixture.wait_screen(|screen| screen.contains(" INS "))?;
    fixture.literal("UNSAVED")?;
    fixture.wait_screen(|screen| screen.contains("UNSAVED"))?;
    fixture.normal()?;
    fixture.detach()?;
    let entries = fixture.entries()?;
    let retained = entries
        .iter()
        .find(|entry| entry["session_id"] == original)
        .context("detached session disappeared")?;
    assert_eq!(retained["server_instance"], info["server_instance"]);
    assert_eq!(retained["attached"], false);
    assert_eq!(retained["modified"], true);
    assert_eq!(std::fs::read_to_string(&file)?, "REMOTE_EDITbase\n");
    let refused = fixture.run(&[
        "restart",
        "--session",
        &original,
        "--config",
        config.to_str().unwrap(),
        "--json",
    ])?;
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("unsaved"));
    fixture.connect(&original, None)?;
    let resumed =
        fixture.wait_screen(|screen| screen.contains("UNSAVED") && screen.contains(" NOR "))?;
    assert_eq!(resumed.matches("UNSAVED").count(), 1);
    fixture.keys(&["u"])?;
    fixture
        .wait_screen(|screen| screen.contains("REMOTE_EDITbase") && !screen.contains("UNSAVED"))?;
    fixture.keys(&["u"])?;
    fixture.wait_screen(|screen| screen.contains("base") && !screen.contains("REMOTE_EDIT"))?;
    fixture.save()?;
    fixture.wait_file(&file, "base\n")?;
    fixture.detach()?;
    let restarted = fixture.json(&[
        "restart",
        "--session",
        &original,
        "--config",
        config.to_str().unwrap(),
        "--json",
    ])?;
    let replacement = restarted["session_id"]
        .as_str()
        .context("replacement session ID missing")?
        .to_owned();
    fixture.sessions.retain(|session| session != &original);
    fixture.sessions.push(replacement.clone());
    assert_ne!(replacement, original);
    assert_ne!(restarted["server_instance"], info["server_instance"]);
    assert_eq!(restarted["workspace"], info["workspace"]);
    let stale = fixture.run(&["attach", &original])?;
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("was not found"));
    let entries = fixture.entries()?;
    let matching: Vec<_> = entries
        .iter()
        .filter(|entry| entry["workspace"] == info["workspace"])
        .collect();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0]["session_id"], replacement);
    let stopped = fixture.json(&["stop", "--session", &replacement, "--json"])?;
    assert_eq!(stopped["status"], "stopped");
    fixture.sessions.retain(|session| session != &replacement);
    assert!(
        !fixture
            .entries()?
            .iter()
            .any(|entry| entry["session_id"] == replacement)
    );
    println!(
        "SSH terminal: saved Unicode path, retained {original}, restored undo, restarted as {replacement}, rejected stale ID, stopped cleanly"
    );
    Ok(())
}

#[test]
#[ignore = "requires an isolated localhost SSH fixture through ZESS_TEST_SSH_CONFIG"]
fn replaying_acknowledged_input_after_an_ssh_disconnect_does_not_repeat_the_edit() -> Result<()> {
    let mut fixture = RemoteSession::new()?;
    let workspace = fixture.directory.path().join("replay workspace");
    std::fs::create_dir(&workspace)?;
    let file = workspace.join("saved.txt");
    std::fs::write(&file, "base\n")?;
    let info = fixture.json(&[
        "new",
        "--remote-workspace",
        workspace.to_str().unwrap(),
        "--json",
    ])?;
    let session = info["session_id"]
        .as_str()
        .context("session ID missing")?
        .to_owned();
    fixture.sessions.push(session.clone());
    let mut first = fixture.protocol(&session)?;
    first.send(json!({"type":"attach", "client_id":"ssh-replay", "expected_session":session, "files":[file.to_str().unwrap()], "width":80, "height":24}))?;
    let attached = first.receive("attached")?;
    let epoch = &attached["context_epoch"];
    first.input(epoch, 1, "i")?;
    first.input(epoch, 2, "X")?;
    drop(first);
    let mut second = fixture.protocol(&session)?;
    second.send(json!({"type":"attach", "client_id":"ssh-replay", "expected_session":session, "context_epoch":epoch, "width":80, "height":24}))?;
    let resumed = second.receive("attached")?;
    assert_eq!(resumed["server_instance"], attached["server_instance"]);
    assert_eq!(resumed["acknowledged"], 2);
    assert_eq!(resumed["replay_allowed"], true);
    second.input(epoch, 2, "X")?;
    for (sequence, key) in [(3, "Esc"), (4, ":"), (5, "w"), (6, "Enter")] {
        second.input(epoch, sequence, key)?;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let actual = std::fs::read_to_string(&file)?;
        if actual == "Xbase\n" {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "acknowledged input was replayed incorrectly: {actual:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    drop(second);
    let stopped = fixture.json(&["stop", "--session", &session, "--json"])?;
    assert_eq!(stopped["status"], "stopped");
    fixture.sessions.clear();
    Ok(())
}
