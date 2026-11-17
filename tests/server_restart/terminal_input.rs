use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    net::Shutdown,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use serde_json::json;
use zess_protocol::{ServerMessage, document::DocumentEvent};

use super::{Server, attach, focused_document, receive_type, send};

struct Terminal {
    socket: PathBuf,
}

impl Terminal {
    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command
            .arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null"]);
        command
    }

    fn start(server: &Server, arguments: &[&str]) -> Self {
        let terminal = Self {
            socket: server.root.join("tmux.sock"),
        };
        let output = terminal
            .command()
            .args(["new-session", "-d", "-s", "editor", "-x", "100", "-y", "26"])
            .arg(env!("CARGO_BIN_EXE_zess"))
            .args(arguments)
            .env("XDG_CONFIG_HOME", &server.root)
            .env("XDG_RUNTIME_DIR", &server.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        terminal
    }

    fn attach(server: &Server) -> Self {
        Self::start(
            server,
            &["attach", "--socket", server.socket.to_str().unwrap()],
        )
    }

    fn keys(&self, keys: &[&str]) {
        let status = self
            .command()
            .args(["send-keys", "-t", "editor"])
            .args(keys)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn literal_input(&self, input: &str) {
        let status = self
            .command()
            .args(["send-keys", "-t", "editor", "-l", input])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn wait_screen(&self, predicate: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let capture = self
                .command()
                .args(["capture-pane", "-p", "-t", "editor"])
                .output()
                .unwrap();
            assert!(
                capture.status.success(),
                "{}",
                String::from_utf8_lossy(&capture.stderr)
            );
            let screen = String::from_utf8(capture.stdout).unwrap();
            if predicate(&screen) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "unexpected terminal screen:\n{screen}"
            );
            thread::sleep(Duration::from_millis(15));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
    }
}

struct RelayRoute {
    endpoint: PathBuf,
    client: Option<UnixStream>,
    paused: bool,
}

impl Drop for RelayRoute {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            let _ = client.shutdown(Shutdown::Both);
        }
    }
}

struct SocketRelay {
    endpoint: PathBuf,
    route: Arc<(Mutex<Option<RelayRoute>>, Condvar)>,
    applied: mpsc::Receiver<u64>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SocketRelay {
    fn start(server: &Server) -> Self {
        let endpoint = server.root.join("custom.sock");
        let listener = UnixListener::bind(&endpoint).unwrap();
        let route = Arc::new((
            Mutex::new(Some(RelayRoute {
                endpoint: server.socket.clone(),
                client: None,
                paused: false,
            })),
            Condvar::new(),
        ));
        let current = route.clone();
        let (receipt, applied) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok((mut client, _)) = listener.accept() {
                let mut peer = {
                    let (state, resumed) = &*current;
                    let mut route = resumed
                        .wait_while(state.lock().unwrap(), |route| {
                            route.as_ref().is_some_and(|route| route.paused)
                        })
                        .unwrap();
                    let Some(route) = route.as_mut() else {
                        break;
                    };
                    route.client = Some(client.try_clone().unwrap());
                    UnixStream::connect(&route.endpoint).unwrap()
                };
                let mut response = BufReader::new(peer.try_clone().unwrap());
                let mut reply = client.try_clone().unwrap();
                thread::scope(|scope| {
                    scope.spawn(|| {
                        let mut frame = Vec::new();
                        while response.read_until(b'\n', &mut frame).is_ok_and(|n| n > 0) {
                            if reply.write_all(&frame).is_err() {
                                break;
                            }
                            if let Ok(ServerMessage::Document(DocumentEvent::Applied {
                                sequence,
                                ..
                            })) = serde_json::from_slice(&frame)
                            {
                                let _ = receipt.send(sequence);
                            }
                            frame.clear();
                        }
                        let _ = reply.shutdown(Shutdown::Write);
                    });
                    let _ = io::copy(&mut client, &mut peer);
                    let _ = peer.shutdown(Shutdown::Write);
                });
            }
        });
        Self {
            endpoint,
            route,
            applied,
            worker: Some(worker),
        }
    }

    fn redirect(&self, server: &Server) {
        *self.route.0.lock().unwrap() = Some(RelayRoute {
            endpoint: server.socket.clone(),
            client: None,
            paused: false,
        });
        self.route.1.notify_one();
    }

    fn pause(&self) {
        let mut route = self.route.0.lock().unwrap();
        let route = route.as_mut().unwrap();
        route.paused = true;
        route
            .client
            .take()
            .unwrap()
            .shutdown(Shutdown::Both)
            .unwrap();
    }

    fn wait_applied(&self, sequence: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .applied
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("missing document receipt {sequence}: {error}"))
            != sequence
        {}
    }
}

impl Drop for SocketRelay {
    fn drop(&mut self) {
        self.route.0.lock().unwrap().take();
        self.route.1.notify_one();
        let _ = UnixStream::connect(&self.endpoint);
        let _ = self.worker.take().unwrap().join();
    }
}

struct PausedServer(u32);

impl PausedServer {
    fn new(server: &Server) -> Self {
        let pid = server.child.id();
        assert!(
            Command::new("kill")
                .arg("-STOP")
                .arg(pid.to_string())
                .status()
                .unwrap()
                .success()
        );
        Self(pid)
    }
}

impl Drop for PausedServer {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .arg("-CONT")
            .arg(self.0.to_string())
            .status();
    }
}

fn wait_file(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let contents = fs::read_to_string(path).unwrap();
        if contents == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected:?}, found {contents:?}"
        );
        thread::sleep(Duration::from_millis(15));
    }
}

#[test]
#[ignore = "requires tmux"]
fn reconnecting_to_a_custom_socket_keeps_the_original_session() {
    let original = Server::with_socket(|_| Some(PathBuf::from("original.sock")));
    let replacement = Server::with_socket(|_| Some(PathBuf::from("replacement.sock")));
    let relay = SocketRelay::start(&original);
    let terminal = Terminal::start(
        &original,
        &[
            "--workspace",
            original.workspace.to_str().unwrap(),
            "--socket",
            relay.endpoint.to_str().unwrap(),
        ],
    );
    let editing = |screen: &str| {
        !screen.contains("Connecting to zess")
            && screen.contains(" NOR ")
            && screen.contains("saved.txt")
    };
    terminal.wait_screen(editing);
    let initial = original.status();
    assert_eq!(initial["attached"], true);

    relay.redirect(&original);
    terminal.wait_screen(|screen| screen.contains("Disconnected; reconnecting"));
    terminal.wait_screen(|screen| editing(screen) && original.status()["attached"] == true);
    let reconnected = original.status();
    assert_eq!(reconnected["attached"], true);
    assert_eq!(reconnected["session_id"], initial["session_id"]);
    assert_eq!(reconnected["server_instance"], initial["server_instance"]);
    let path = original.workspace.join("saved.txt");
    terminal.keys(&["i", "R", "Escape"]);
    terminal.literal_input(":write\r");
    wait_file(&path, "Rsaved\n");

    relay.redirect(&replacement);
    terminal.wait_screen(|screen| screen.contains("session identity changed"));
    let replacement = replacement.status();
    assert_ne!(replacement["session_id"], initial["session_id"]);
    assert_ne!(replacement["server_instance"], initial["server_instance"]);
    assert_eq!(replacement["attached"], false);
}

#[test]
#[ignore = "requires tmux"]
fn undo_after_reconnecting_returns_to_the_edit_saved_by_another_client() {
    let server = Server::new();
    let path = server.workspace.join("saved.txt");
    let relay = SocketRelay::start(&server);
    let terminal = Terminal::start(
        &server,
        &["attach", "--socket", relay.endpoint.to_str().unwrap()],
    );
    terminal.wait_screen(|screen| {
        screen.contains(" NOR ") && screen.lines().any(|row| row.trim() == "1 saved")
    });
    terminal.keys(&["i", "X"]);
    terminal.wait_screen(|screen| screen.contains(" INS ") && screen.contains("Xsaved"));
    relay.wait_applied(1);
    relay.pause();
    terminal.wait_screen(|screen| {
        screen.contains("Disconnected; reconnecting") && server.status()["attached"] == false
    });

    let (mut writer, _) = attach(&server, "other editor");
    let document = focused_document(&mut writer);
    assert_eq!(document["text"], "Xsaved\n");
    send(
        writer.get_mut(),
        json!({
            "type": "document",
            "kind": "save",
            "buffer_id": document["buffer_id"],
            "sequence": 1,
            "version": document["version"],
        }),
    );
    assert_eq!(receive_type(&mut writer, "document")["kind"], "saved");
    let saved = fs::read_to_string(&path).unwrap();
    assert_eq!(saved, "Xsaved\n");
    send(writer.get_mut(), json!({"type": "detach"}));
    receive_type(&mut writer, "detached");
    terminal.wait_screen(|_| server.status()["attached"] == false);

    relay.redirect(&server);
    terminal.wait_screen(|screen| {
        !screen.contains("Disconnected; reconnecting")
            && screen.contains(" INS ")
            && screen.contains("Xsaved")
            && !screen.contains("[+]")
            && server.status()["attached"] == true
    });
    terminal.keys(&["Y", "Escape"]);
    terminal.wait_screen(|screen| screen.contains("XYsaved"));
    terminal.keys(&["u"]);
    terminal.wait_screen(|screen| {
        screen
            .lines()
            .next()
            .and_then(|row| row.split_whitespace().last())
            == Some(saved.trim_end())
            && screen.contains(" NOR ")
            && !screen.contains("[+]")
    });
    assert_eq!(fs::read_to_string(path).unwrap(), saved);
}

#[test]
#[ignore = "requires tmux"]
fn terminal_mouse_drag_selects_whole_graphemes() {
    let server = Server::new();
    let path = server.workspace.join("unicode.txt");
    let line = "x界e\u{301}yz";
    fs::write(&path, format!("{line}\n")).unwrap();
    let terminal = Terminal::attach(&server);
    terminal.wait_screen(|screen| screen.contains(" NOR "));

    terminal.literal_input(":open unicode.txt\r");
    let screen = terminal.wait_screen(|screen| {
        screen.contains(" NOR ") && screen.lines().next().is_some_and(|row| row.ends_with(line))
    });
    let gutter = screen.lines().next().unwrap().strip_suffix(line).unwrap();
    // SGR uses one-based cell coordinates. Press the second cell of 界 and drag through é.
    let press = gutter.len() + 3;
    let drag = gutter.len() + 4;
    terminal.literal_input(&format!(
        "\x1b[<0;{press};1M\x1b[<32;{drag};1M\x1b[<0;{drag};1m"
    ));
    terminal.keys(&["M-d"]);
    terminal.literal_input(":write\r");
    wait_file(&path, "xyz\n");
}

#[test]
#[ignore = "requires tmux"]
fn terminal_double_click_toggles_a_directory_and_keeps_files_in_the_browser() {
    let server = Server::new();
    let folder = server.workspace.join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("child.txt"), "child contents\n").unwrap();
    fs::write(server.workspace.join("note.txt"), "note preview\n").unwrap();
    let terminal = Terminal::attach(&server);
    terminal.wait_screen(|screen| screen.contains(" NOR "));
    terminal.literal_input(":files\r");

    let browser_row = |screen: &str, label: &str| {
        screen.lines().position(|line| {
            line.split_once('│')
                .is_some_and(|(browser, _)| browser.trim() == label)
        })
    };
    let preview_shows = |screen: &str, text: &str| {
        screen.lines().any(|line| {
            line.split_once('│')
                .is_some_and(|(_, preview)| preview.contains(text))
        })
    };
    let click = |row: usize, count: usize| {
        let row = row + 1;
        terminal.literal_input(&format!("\x1b[<0;1;{row}M\x1b[<0;1;{row}m").repeat(count));
    };

    let screen = terminal.wait_screen(|screen| {
        browser_row(screen, "▸ folder").is_some() && browser_row(screen, "note.txt").is_some()
    });
    click(browser_row(&screen, "note.txt").unwrap(), 2);
    let screen = terminal.wait_screen(|screen| preview_shows(screen, "note preview"));
    assert!(browser_row(&screen, "note.txt").is_some());

    click(browser_row(&screen, "▸ folder").unwrap(), 1);
    let screen = terminal.wait_screen(|screen| preview_shows(screen, "child.txt"));
    assert!(browser_row(&screen, "▸ folder").is_some());
    assert!(browser_row(&screen, "child.txt").is_none());

    terminal.keys(&["j"]);
    let screen = terminal.wait_screen(|screen| preview_shows(screen, "note preview"));
    click(browser_row(&screen, "▸ folder").unwrap(), 2);
    let screen = terminal.wait_screen(|screen| browser_row(screen, "child.txt").is_some());
    click(browser_row(&screen, "▾ folder").unwrap(), 2);
    let screen = terminal.wait_screen(|screen| browser_row(screen, "▸ folder").is_some());
    assert!(browser_row(&screen, "child.txt").is_none());
}

#[test]
#[ignore = "requires tmux and Unix process signals"]
fn terminal_keeps_later_edits_and_focus_when_the_server_resumes() {
    let server = Server::new();
    let path = server.workspace.join("saved.txt");
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(&server.workspace)
            .status()
            .unwrap()
            .success()
    );
    let terminal = Terminal::attach(&server);
    terminal.wait_screen(|screen| {
        screen.contains(" NOR ") && screen.lines().any(|line| line.trim() == "1 saved")
    });

    let paused = PausedServer::new(&server);
    terminal.keys(&["Space", "g"]);
    terminal.wait_screen(|screen| screen.contains("Git") && !screen.contains("saved.txt"));
    terminal.keys(&["Escape"]);
    terminal.wait_screen(|screen| screen.contains(" NOR ") && screen.contains("saved.txt"));

    terminal.keys(&["i", "A"]);
    terminal.wait_screen(|screen| screen.contains("Asaved") && screen.contains(" INS "));
    terminal.keys(&["Escape"]);
    terminal.wait_screen(|screen| screen.contains(" NOR "));
    terminal.keys(&[":", "w"]);
    terminal
        .wait_screen(|screen| screen.contains("Asaved") && screen.lines().any(|line| line == ":w"));
    terminal.keys(&["Enter", "i", "B"]);
    terminal.wait_screen(|screen| screen.contains("BAsaved") && screen.contains(" INS "));
    assert_eq!(fs::read_to_string(&path).unwrap(), "saved\n");

    drop(paused);
    wait_file(&path, "Asaved\n");
    terminal.wait_screen(|screen| {
        screen.contains("BAsaved") && screen.contains(" INS ") && screen.contains("[+]")
    });
    terminal.keys(&["Escape"]);
    terminal.wait_screen(|screen| screen.contains(" NOR "));
    terminal.keys(&[":", "w", "Enter"]);
    wait_file(&path, "BAsaved\n");
    terminal.wait_screen(|screen| {
        screen.contains("BAsaved") && screen.contains(" NOR ") && !screen.contains("[+]")
    });
}
