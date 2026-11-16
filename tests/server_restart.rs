#![cfg(unix)]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

struct Server {
    root: PathBuf,
    workspace: PathBuf,
    socket: PathBuf,
    child: Child,
}

impl Server {
    fn new() -> Self {
        Self::with_socket(|_| Some(PathBuf::from("zess/run/0123456789abcdef.sock")))
    }

    fn with_socket(socket: impl FnOnce(&Path) -> Option<PathBuf>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zess-restart-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let socket = socket(&workspace);
        if let Some(socket) = &socket {
            fs::create_dir_all(root.join(socket).parent().unwrap()).unwrap();
        }
        fs::write(workspace.join("saved.txt"), "saved\n").unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_zess"));
        command.args(["server", "--workspace"]).arg(&workspace);
        if let Some(socket) = &socket {
            command.arg("--socket").arg(socket);
        }
        let child = command
            .args(["--", "saved.txt"])
            .current_dir(&root)
            .env("XDG_RUNTIME_DIR", &root)
            .env("XDG_CONFIG_HOME", &root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut server = Self {
            socket: root.join(socket.as_deref().unwrap_or(Path::new("pending.sock"))),
            root,
            workspace,
            child,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let socket = loop {
            let socket = if let Some(socket) = &socket {
                Some(server.root.join(socket))
            } else {
                fs::read_dir(server.root.join("zess/run"))
                    .ok()
                    .and_then(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .map(|entry| entry.path())
                            .find(|path| {
                                path.extension()
                                    .is_some_and(|extension| extension == "sock")
                            })
                    })
            };
            if let Some(socket) = socket
                && UnixStream::connect(&socket).is_ok()
            {
                break socket;
            }
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "server exited before listening"
            );
            assert!(Instant::now() < deadline, "server startup timed out");
            thread::sleep(Duration::from_millis(10));
        };
        server.socket = socket;
        server
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zess"));
        command
            .current_dir(&self.root)
            .env("XDG_RUNTIME_DIR", &self.root)
            .env("XDG_CONFIG_HOME", &self.root);
        command
    }

    fn command(&self, arguments: &[&str]) -> Output {
        self.cli().args(arguments).output().unwrap()
    }

    fn status(&self) -> Value {
        let mut stream = connect(&self.socket);
        send(&mut stream, json!({"type": "status"}));
        receive(&mut BufReader::new(stream))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.command(&[
            "session",
            "stop",
            "--socket",
            self.socket.to_str().unwrap(),
            "--force",
        ]);
        // This handle always belongs to the child created by this fixture.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn connect(socket: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(socket).unwrap();
    send(
        &mut stream,
        json!({"type": "hello", "build_id": zess_build::BUILD_ID}),
    );
    let hello = receive(&mut BufReader::new(stream.try_clone().unwrap()));
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["build_id"], zess_build::BUILD_ID);
    stream
}

fn send(stream: &mut UnixStream, message: Value) {
    writeln!(stream, "{message}").unwrap();
}

#[test]
fn named_sessions_share_a_workspace_and_duplicate_names_are_rejected() {
    let server = Server::new();
    let workspace = server.workspace.to_str().unwrap();
    let created = server.command(&[
        "session",
        "new",
        "review",
        "--workspace",
        workspace,
        "--json",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created: Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(created["name"], "review");
    assert_eq!(created["workspace"], workspace);
    assert_ne!(created["pid"], server.child.id());
    let duplicate = server.command(&["session", "new", "review", "--workspace", workspace]);
    assert!(!duplicate.status.success());
    let stopped = server.command(&["session", "stop", "review"]);
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
}

#[test]
fn attaching_to_an_empty_workspace_does_not_create_a_session() {
    let server = Server::new();
    let workspace = server.root.join("empty");
    fs::create_dir(&workspace).unwrap();
    let output = server.command(&[
        "session",
        "attach",
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no session exists"));
    let list = server.command(&["session", "list", "--json"]);
    let sessions: Vec<Value> = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(sessions.len(), 1);
}

fn receive(reader: &mut BufReader<UnixStream>) -> Value {
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn receive_type(reader: &mut BufReader<UnixStream>, kind: &str) -> Value {
    loop {
        let message = receive(reader);
        assert_ne!(message["type"], "rejected", "{message}");
        if message["type"] == kind {
            return message;
        }
    }
}

#[test]
fn unsaved_changes_require_force_before_replacing_the_session() {
    let server = Server::new();
    let mut client = connect(&server.socket);
    send(
        &mut client,
        json!({"type": "attach", "client_id": "restart-test", "width": 80, "height": 24}),
    );
    let mut client = BufReader::new(client);
    let attached = receive(&mut client);
    assert_eq!(attached["type"], "attached");
    let epoch = &attached["context_epoch"];
    for (sequence, key) in [(1, "i"), (2, "x")] {
        send(
            client.get_mut(),
            json!({"type": "input", "sequence": sequence, "key": key, "context_epoch": epoch}),
        );
        loop {
            let message = receive(&mut client);
            if message["type"] == "ack" && message["acknowledged"] == sequence {
                break;
            }
        }
    }
    let before = server.status();
    assert_eq!(before["modified"], true);
    assert_eq!(before["attached"], true);
    let arguments = [
        "session",
        "restart",
        "--socket",
        server.socket.to_str().unwrap(),
    ];
    let rejected = server.command(&arguments);
    assert!(!rejected.status.success());
    assert_eq!(
        String::from_utf8_lossy(&rejected.stderr),
        "Restart creates a fresh session; open buffers, layout and undo history are reset.\nError: unsaved buffers; save them first or use --force to discard them\n"
    );
    assert_eq!(server.status()["pid"], before["pid"]);
    assert_eq!(server.status()["modified"], true);

    let restarted = server.command(&[
        "session",
        "restart",
        "--socket",
        server.socket.to_str().unwrap(),
        "--force",
    ]);
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let after = server.status();
    assert_ne!(after["pid"], before["pid"]);
    assert_eq!(after["modified"], false);
    assert_eq!(after["attached"], false);
    assert_eq!(after["workspace"], before["workspace"]);
    assert_eq!(
        fs::read_to_string(server.workspace.join("saved.txt")).unwrap(),
        "saved\n"
    );
}

#[test]
fn restarting_by_id_uses_the_original_workspace() {
    let server = Server::new();
    let mut pid = server.status()["pid"].clone();
    for _ in 0..2 {
        let output = server.command(&["session", "restart", "0123456789abcdef"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status = server.status();
        assert_ne!(status["pid"], pid);
        assert_eq!(
            status["workspace"].as_str().unwrap(),
            server.workspace.to_str().unwrap()
        );
        assert_eq!(status["attached"], false);
        pid = status["pid"].clone();
    }
}

#[test]
fn default_servers_are_discovered_and_can_be_stopped_by_uppercase_id() {
    let mut server = Server::with_socket(|_| None);
    let id = server.socket.file_stem().unwrap().to_str().unwrap();
    assert_eq!(id.len(), 16);
    assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(
        fs::metadata(server.socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&server.socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let listed = server.command(&["session", "list"]);
    assert!(listed.status.success());
    let listing = String::from_utf8(listed.stdout).unwrap();
    let entries = listing.lines().skip(1).collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "{listing}");
    let fields = entries[0].split('\t').collect::<Vec<_>>();
    assert_eq!(fields[1], id);
    assert_eq!(fields[3], server.child.id().to_string());
    assert_eq!(fields[6], server.workspace.to_str().unwrap());
    let stopped = server.command(&["session", "stop", &id.to_ascii_uppercase()]);
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "stopped server did not exit");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    assert!(!server.socket.exists());
}

#[test]
fn restarting_a_relative_socket_preserves_its_location() {
    let server = Server::with_socket(|_| Some(PathBuf::from("relative.sock")));
    let before = server.status();
    let restarted = server.command(&["session", "restart", "--socket", "relative.sock"]);
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let after = server.status();
    assert_ne!(after["pid"], before["pid"]);
    assert_eq!(after["workspace"], before["workspace"]);
    assert_eq!(server.socket, server.root.join("relative.sock"));
    assert!(!server.workspace.join("relative.sock").exists());
}

#[test]
fn fragmented_management_requests_wait_for_the_delimiter() {
    let server = Server::new();
    let mut stream = connect(&server.socket);
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(b"{\"type\":\"sta").unwrap();
    let expected = server.status();
    stream.write_all(b"tus\"}").unwrap();
    server.status();
    stream.set_nonblocking(true).unwrap();
    assert_eq!(
        std::io::Read::read(&mut stream, &mut [0])
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    stream.set_nonblocking(false).unwrap();
    stream.write_all(b"\n").unwrap();
    let actual = receive(&mut BufReader::new(stream));
    assert_eq!(actual["pid"], expected["pid"]);
    assert_eq!(actual["workspace"], expected["workspace"]);
}

#[test]
fn oversized_requests_only_close_the_offending_connection() {
    use std::io::Read;
    use zess_protocol::MAX_INPUT_BYTES;

    let server = Server::new();
    for size in [MAX_INPUT_BYTES, MAX_INPUT_BYTES + 1] {
        let prefix = b"{\"type\":\"status\",\"padding\":\"";
        let suffix = b"\"}\n";
        let mut message = prefix.to_vec();
        message.resize(size - suffix.len(), b'x');
        message.extend_from_slice(suffix);
        let mut stream = connect(&server.socket);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let written = stream.write_all(&message);
        if size == MAX_INPUT_BYTES {
            written.unwrap();
            assert_eq!(
                receive(&mut BufReader::new(stream))["pid"],
                server.child.id()
            );
        } else {
            if let Err(error) = written {
                assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ));
            }
            let mut response = Vec::new();
            if let Err(error) = stream.read_to_end(&mut response) {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            }
            assert!(response.is_empty());
            assert_eq!(server.status()["pid"], server.child.id());
        }
    }
}

fn attach(
    server: &Server,
    client_id: &str,
    epoch: Option<&Value>,
) -> (BufReader<UnixStream>, Value) {
    let mut stream = connect(&server.socket);
    send(
        &mut stream,
        json!({"type": "attach", "client_id": client_id, "width": 80, "height": 24, "context_epoch": epoch}),
    );
    let mut reader = BufReader::new(stream);
    loop {
        let message = receive(&mut reader);
        if message["type"] == "attached" {
            return (reader, message);
        }
    }
}

fn input(reader: &mut BufReader<UnixStream>, epoch: &Value, sequence: u64, key: &str) {
    send(
        reader.get_mut(),
        json!({"type": "input", "sequence": sequence, "key": key, "context_epoch": epoch}),
    );
    loop {
        let message = receive(reader);
        assert_ne!(message["type"], "rejected", "{message}");
        if message["type"] == "ack" && message["acknowledged"] == sequence {
            return;
        }
    }
}

#[test]
fn reconnecting_to_the_same_server_retains_acknowledgements_and_deduplicates_input() {
    let server = Server::new();
    let (mut client, first) = attach(&server, "reconnect", None);
    for (sequence, key) in [(1, "i"), (2, "x")] {
        input(&mut client, &first["context_epoch"], sequence, key);
    }
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    while server.status()["attached"] == true {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let (mut client, second) = attach(&server, "reconnect", Some(&first["context_epoch"]));
    assert_eq!(second["server_instance"], first["server_instance"]);
    assert_eq!(second["acknowledged"], 2);
    assert_eq!(second["replay_allowed"], true);
    input(&mut client, &second["context_epoch"], 2, "x");
    for (sequence, key) in [(3, "Esc"), (4, ":"), (5, "w"), (6, "Enter")] {
        input(&mut client, &second["context_epoch"], sequence, key);
    }
    assert_eq!(
        fs::read_to_string(server.workspace.join("saved.txt")).unwrap(),
        "xsaved\n"
    );
}

#[test]
fn replacing_a_crashed_server_starts_a_new_input_ledger() {
    let mut server = Server::new();
    let (mut client, first) = attach(&server, "crash-reconnect", None);
    for (sequence, key) in [(1, "i"), (2, "x")] {
        input(&mut client, &first["context_epoch"], sequence, key);
    }
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    drop(client);
    server.child = server
        .cli()
        .args(["server", "--workspace"])
        .arg(&server.workspace)
        .arg("--socket")
        .arg(&server.socket)
        .args(["--", "saved.txt"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if UnixStream::connect(&server.socket).is_ok() {
            break;
        }
        assert!(server.child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let (mut client, second) = attach(&server, "crash-reconnect", Some(&first["context_epoch"]));
    assert_ne!(second["server_instance"], first["server_instance"]);
    assert_eq!(second["acknowledged"], 0);
    assert_eq!(second["replay_allowed"], false);
    for (sequence, key) in [
        (1, "i"),
        (2, "y"),
        (3, "Esc"),
        (4, ":"),
        (5, "w"),
        (6, "Enter"),
    ] {
        input(&mut client, &second["context_epoch"], sequence, key);
    }
    assert_eq!(
        fs::read_to_string(server.workspace.join("saved.txt")).unwrap(),
        "ysaved\n"
    );
}

#[test]
fn an_unanswered_clipboard_request_is_reissued_after_reconnect_and_duplicate_input() {
    fn clipboard_input(reader: &mut BufReader<UnixStream>, epoch: &Value) -> Value {
        send(
            reader.get_mut(),
            json!({"type": "input", "sequence": 3, "key": "p", "context_epoch": epoch}),
        );
        let mut request = None;
        let mut acknowledged = false;
        while request.is_none() || !acknowledged {
            let message = receive(reader);
            assert_ne!(message["type"], "rejected", "{message}");
            if message["type"] == "clipboard_read" {
                request = Some(message);
            } else if message["type"] == "ack" && message["acknowledged"] == 3 {
                acknowledged = true;
            }
        }
        request.unwrap()
    }

    let server = Server::new();
    let (mut client, first) = attach(&server, "clipboard-reconnect", None);
    for (sequence, key) in [(1, "\""), (2, "+")] {
        input(&mut client, &first["context_epoch"], sequence, key);
    }
    let request = clipboard_input(&mut client, &first["context_epoch"]);
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    while server.status()["attached"] == true {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let (mut client, second) = attach(
        &server,
        "clipboard-reconnect",
        Some(&first["context_epoch"]),
    );
    assert_eq!(receive_type(&mut client, "clipboard_read"), request);
    assert_eq!(
        clipboard_input(&mut client, &second["context_epoch"]),
        request
    );
    send(
        client.get_mut(),
        json!({"type": "clipboard_response", "id": request["id"], "text": "external", "error": null}),
    );
    receive_type(&mut client, "delta");
    for (sequence, key) in [(4, ":"), (5, "w"), (6, "Enter")] {
        input(&mut client, &second["context_epoch"], sequence, key);
    }
    assert_eq!(
        fs::read_to_string(server.workspace.join("saved.txt")).unwrap(),
        "sexternalaved\n"
    );
}

#[test]
fn a_rejected_attached_client_does_not_prevent_the_next_attachment() {
    let server = Server::new();
    let (mut client, attached) = attach(&server, "rejected", None);
    send(
        client.get_mut(),
        json!({"type": "input", "sequence": 1, "key": "i", "context_epoch": attached["context_epoch"].as_u64().unwrap().wrapping_add(1)}),
    );
    loop {
        if receive(&mut client)["type"] == "rejected" {
            break;
        }
    }
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(3);
    while server.status()["attached"] == true {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let (mut client, attached) = attach(&server, "replacement", None);
    assert_eq!(attached["acknowledged"], 0);
    input(&mut client, &attached["context_epoch"], 1, "i");
}
