#![cfg(target_os = "linux")]

use std::{
    fs, io,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use zess::server;
use zess_ipc::{
    Listener, LocalIpc, NativeLocalIpc, SocketPath, Stream, platform::NativeDaemonControl,
};
use zess_platform::{NativeFileSystem, NativePlatform};

struct ReplaceEndpointOnConnect {
    replacement: PathBuf,
    replaced: AtomicBool,
}

impl LocalIpc for ReplaceEndpointOnConnect {
    fn prepare(&self, endpoint: &SocketPath) -> anyhow::Result<()> {
        NativeLocalIpc.prepare(endpoint)
    }

    fn connect(&self, endpoint: &SocketPath) -> io::Result<Stream> {
        let stream = NativeLocalIpc.connect(endpoint)?;
        if !self.replaced.swap(true, Ordering::Relaxed) {
            fs::rename(&self.replacement, endpoint.as_path())?;
        }
        Ok(stream)
    }

    fn bind(&self, endpoint: &SocketPath) -> anyhow::Result<Listener> {
        NativeLocalIpc.bind(endpoint)
    }
}

fn manager_with_replacement(replacement: &Path) -> server::SessionManager {
    server::SessionManager::new(
        Arc::new(NativeFileSystem),
        Arc::new(ReplaceEndpointOnConnect {
            replacement: replacement.to_owned(),
            replaced: AtomicBool::new(false),
        }),
        Arc::new(NativeDaemonControl),
        Arc::new(NativePlatform),
    )
}

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
fn forced_stop_terminates_the_original_daemon_after_endpoint_replacement() {
    let binary = Path::new(env!("CARGO_BIN_EXE_zess"));
    let mut original = Server::new(binary);
    let replacement = Server::new(binary);
    original.signal("-STOP");

    let manager = manager_with_replacement(&replacement.socket);
    let endpoint = SocketPath::from(original.socket.clone());
    let stop = thread::spawn({
        let manager = manager.clone();
        let endpoint = endpoint.clone();
        move || manager.stop(&endpoint, true)
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let pending = original.status().lines().any(|line| {
            line.strip_prefix("SigPnd:")
                .or_else(|| line.strip_prefix("ShdPnd:"))
                .is_some_and(|mask| u64::from_str_radix(mask.trim(), 16).unwrap() & (1 << 14) != 0)
        });
        if pending {
            break;
        }
        assert!(
            !stop.is_finished(),
            "force stop did not signal the original daemon"
        );
        assert!(
            Instant::now() < deadline,
            "force stop did not signal the original daemon"
        );
        thread::sleep(Duration::from_millis(2));
    }
    original.signal("-CONT");
    stop.join().unwrap().unwrap();
    original.child.wait().unwrap();
    assert_eq!(
        manager.inspect(&endpoint).unwrap().pid,
        replacement.child.id()
    );
}

#[test]
fn restart_leaves_both_daemons_running_when_the_endpoint_changes() {
    let binary = Path::new(env!("CARGO_BIN_EXE_zess"));
    let mut original = Server::new(binary);
    let replacement = Server::new(binary);
    let manager = manager_with_replacement(&replacement.socket);
    let endpoint = SocketPath::from(original.socket.clone());
    let error = manager.restart(&endpoint, None, None, true).unwrap_err();

    assert!(error.to_string().contains("session changed during restart"));
    assert!(original.child.try_wait().unwrap().is_none());
    assert_eq!(
        manager.inspect(&endpoint).unwrap().pid,
        replacement.child.id()
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
