use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
#[path = "daemon/windows.rs"]
mod windows;

use zess_ipc::platform::{DaemonControl, DaemonSpec, NativeDaemonControl};

#[test]
fn dropping_the_process_handle_keeps_the_daemon_running() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "zess-daemon-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let marker = root.join("daemon-alive");
    let mut daemon = NativeDaemonControl
        .spawn(&DaemonSpec {
            program: std::env::current_exe().unwrap(),
            args: ["--exact", "daemon_child", "--ignored", "--nocapture"]
                .map(Into::into)
                .into(),
            cwd: Some(root.clone()),
            log_path: root.join("daemon.log"),
        })
        .unwrap();
    assert!(daemon.try_wait().unwrap().is_none());
    drop(daemon);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "daemon did not survive handle release"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read(&marker).unwrap(), b"alive");
    // The child closes its log at exit. Windows keeps that file open until then.
    loop {
        match fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("remove daemon fixture: {error}"),
        }
    }
}

#[test]
#[ignore = "child process entry point used by the daemon lifetime test"]
fn daemon_child() {
    assert!(
        std::env::current_dir()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("zess-daemon-test-")
    );
    thread::sleep(Duration::from_millis(50));
    fs::write("daemon-alive", b"alive").unwrap();
}

#[cfg(windows)]
#[test]
fn forced_stop_terminates_the_server_at_the_endpoint() {
    use zess_ipc::SocketPath;

    let root = std::env::temp_dir().join(format!("zess-dstop-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let program = root.join(format!("zess{}", std::env::consts::EXE_SUFFIX));
    fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
    let mut daemon = NativeDaemonControl
        .spawn(&DaemonSpec {
            program,
            args: ["server", "--ignored", "--nocapture"]
                .map(Into::into)
                .into(),
            cwd: Some(root.clone()),
            log_path: root.join("daemon.log"),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("daemon-ready").exists() {
        assert!(
            daemon.try_wait().unwrap().is_none(),
            "server exited during startup"
        );
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let socket = SocketPath::from(root.join("server.pipe"));
    let stopped = NativeDaemonControl.stop(&socket);
    let deadline = Instant::now() + Duration::from_secs(6);
    let status = loop {
        if let Some(status) = daemon.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    };
    fs::remove_dir_all(&root).unwrap();
    stopped.unwrap();
    assert!(!status.success());
    assert!(!socket.is_present().unwrap());
}

#[cfg(windows)]
#[test]
#[ignore = "child process entry point used by the forced stop test"]
fn pipe_server_child() {
    use zess_ipc::{Listener, SocketPath};

    let root = std::env::current_dir().unwrap();
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("zess-dstop-")
    );
    let socket = SocketPath::from(root.join("server.pipe"));
    let _listener = Listener::bind(&socket).unwrap();
    fs::write(root.join("daemon-ready"), b"ready").unwrap();
    thread::sleep(Duration::from_secs(5));
}

#[cfg(windows)]
fn assert_peer_rejected(name: &str, arguments: &[&str]) {
    use std::process::{Command, Stdio};
    use zess_ipc::SocketPath;

    let root = std::env::temp_dir().join(format!("zess-dstop-no-{}-{name}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let program = root.join(name);
    fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("daemon-ready").exists() {
        assert!(child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let stopped = NativeDaemonControl.stop(&SocketPath::from(root.join("server.pipe")));
    let running = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    child.wait().unwrap();
    fs::remove_dir_all(&root).unwrap();
    assert!(
        running,
        "force stop terminated a peer without the Zess server identity"
    );
    assert!(
        stopped
            .unwrap_err()
            .to_string()
            .contains("not a Zess server")
    );
}

#[cfg(windows)]
#[test]
fn a_zess_process_running_in_another_mode_is_not_terminated() {
    assert_peer_rejected(
        &format!("zess{}", std::env::consts::EXE_SUFFIX),
        &["--exact", "pipe_server_child", "--ignored", "--nocapture"],
    );
}

#[cfg(windows)]
#[test]
fn another_application_running_in_server_mode_is_not_terminated() {
    assert_peer_rejected(
        &format!("another-app{}", std::env::consts::EXE_SUFFIX),
        &["server", "--ignored", "--nocapture"],
    );
}
