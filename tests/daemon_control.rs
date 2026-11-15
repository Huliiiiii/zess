#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Server {
    root: tempfile::TempDir,
    socket: PathBuf,
    child: Child,
}

impl Server {
    fn new(binary: &Path) -> Self {
        let root = tempfile::Builder::new()
            .prefix("zess-force-daemon-")
            .tempdir()
            .unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let socket = root.path().join("server.sock");
        let log = fs::File::create(root.path().join("server.log")).unwrap();
        let child = Command::new(binary)
            .arg("server")
            .arg("--workspace")
            .arg(workspace)
            .arg("--socket")
            .arg(&socket)
            .env("XDG_CONFIG_HOME", root.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut server = Self {
            root,
            socket,
            child,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while UnixStream::connect(&server.socket).is_err() {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "server startup failed: {}",
                fs::read_to_string(server.root.path().join("server.log")).unwrap()
            );
            assert!(Instant::now() < deadline, "server did not listen");
            thread::sleep(Duration::from_millis(2));
        }
        server
    }

    fn signal(&self, signal: &str) {
        assert!(
            Command::new("kill")
                .arg(signal)
                .arg("--")
                .arg(self.child.id().to_string())
                .status()
                .unwrap()
                .success()
        );
    }

    fn status(&self) -> String {
        fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.signal("-CONT");
            let _ = self.child.kill();
        }
        self.child.wait().unwrap();
    }
}

fn force_stop(server_binary: &Path, manager_binary: &Path) {
    let mut server = Server::new(server_binary);
    server.signal("-STOP");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server
        .status()
        .lines()
        .any(|line| line.starts_with("State:") && line.contains("T (stopped)"))
    {
        assert!(Instant::now() < deadline, "server did not stop");
        thread::sleep(Duration::from_millis(2));
    }
    let mut manager = Command::new(manager_binary)
        .args(["session", "stop", "--socket"])
        .arg(&server.socket)
        .arg("--force")
        .env("XDG_CONFIG_HOME", server.root.path())
        .env("XDG_RUNTIME_DIR", server.root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let pending = server.status().lines().any(|line| {
            line.strip_prefix("SigPnd:")
                .or_else(|| line.strip_prefix("ShdPnd:"))
                .is_some_and(|mask| u64::from_str_radix(mask.trim(), 16).unwrap() & (1 << 14) != 0)
        });
        if pending {
            break;
        }
        if manager.try_wait().unwrap().is_some() {
            let output = manager.wait_with_output().unwrap();
            panic!(
                "force stop exited before signaling the daemon: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(Instant::now() < deadline, "force stop did not send SIGTERM");
        thread::sleep(Duration::from_millis(2));
    }
    server.signal("-CONT");
    let output = manager.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.child.wait().unwrap();
    assert!(!server.socket.exists());
}

#[test]
fn the_cli_force_stops_an_unresponsive_daemon() {
    force_stop(
        Path::new(env!("CARGO_BIN_EXE_zess")),
        Path::new(env!("CARGO_BIN_EXE_zess")),
    );
}

#[test]
fn force_restart_replaces_an_unresponsive_session_when_workspace_is_supplied() {
    let binary = Path::new(env!("CARGO_BIN_EXE_zess"));
    let mut server = Server::new(binary);
    server.signal("-STOP");
    let mut manager = Command::new(binary)
        .args(["session", "restart", "--socket"])
        .arg(&server.socket)
        .arg("--workspace")
        .arg(server.root.path().join("workspace"))
        .arg("--force")
        .env("XDG_CONFIG_HOME", server.root.path())
        .env("XDG_RUNTIME_DIR", server.root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pending = server.status().lines().any(|line| {
            line.strip_prefix("SigPnd:")
                .or_else(|| line.strip_prefix("ShdPnd:"))
                .is_some_and(|mask| u64::from_str_radix(mask.trim(), 16).unwrap() & (1 << 14) != 0)
        });
        if pending {
            break;
        }
        assert!(
            manager.try_wait().unwrap().is_none(),
            "restart exited before termination"
        );
        assert!(
            Instant::now() < deadline,
            "restart did not signal the session"
        );
        thread::sleep(Duration::from_millis(2));
    }
    server.signal("-CONT");
    let output = manager.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.child.wait().unwrap();
    let stop = Command::new(binary)
        .args(["session", "stop", "--socket"])
        .arg(&server.socket)
        .arg("--force")
        .env("XDG_CONFIG_HOME", server.root.path())
        .env("XDG_RUNTIME_DIR", server.root.path())
        .output()
        .unwrap();
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}
