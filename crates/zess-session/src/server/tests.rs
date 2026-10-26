use std::fs;
use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};

use zess_ui::{CursorShape, Frame};

use super::*;

fn stream_pair() -> (Stream, Stream) {
    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
    let socket = SocketPath::from(std::env::temp_dir().join(format!(
        "zess-session-ipc-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    )));
    let listener = Listener::bind(&socket).unwrap();
    let client = Stream::connect(&socket).unwrap();
    let server = listener.accept().unwrap();
    (server, client)
}

fn update(status: &str) -> ServerMessage {
    ServerMessage::Update {
        acknowledged: 0,
        frame: Frame {
            styles: Default::default(),
            mouse_context: zess_ui::MouseContext::default(),
            mouse_enabled: true,
            width: 80,
            height: 24,
            rows: Vec::new(),
            cursor: None,
            cursor_shape: CursorShape::Block,
            status: status.into(),
            prompt: String::new(),
        },
    }
}

#[test]
fn coalescing_frames_preserves_clipboard_writes_in_order() {
    let (server, client) = stream_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut connection = Connection::new(server).unwrap();
    for message in [
        update("initial"),
        update("before copy"),
        ServerMessage::ClipboardWrite {
            text: "first".into(),
        },
        update("intermediate"),
        update("after copy"),
        ServerMessage::ClipboardWrite {
            text: "second".into(),
        },
        update("obsolete"),
        update("latest"),
    ] {
        connection.send(&message).unwrap();
    }
    connection.flush().unwrap();
    connection.stream.shutdown(Shutdown::Write).unwrap();
    let mut copies = Vec::new();
    let mut last_frame = String::new();
    for line in BufReader::new(client).lines() {
        match serde_json::from_str::<ServerMessage>(&line.unwrap()).unwrap() {
            ServerMessage::ClipboardWrite { text } => copies.push(text),
            ServerMessage::Update { frame, .. } => {
                assert_ne!(frame.status, "intermediate");
                assert_ne!(frame.status, "obsolete");
                last_frame = frame.status;
            }
            message => panic!("unexpected message: {message:?}"),
        }
    }
    assert_eq!(copies, ["first", "second"]);
    assert_eq!(last_frame, "latest");
}

#[test]
fn oversized_clipboard_writes_leave_the_connection_usable() {
    let (server, client) = stream_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut connection = Connection::new(server).unwrap();
    // Each NUL is six bytes in JSON, so the envelope crosses the limit
    // even though the selected text itself is smaller than the limit.
    let error = connection
        .send(&ServerMessage::ClipboardWrite {
            text: "\0".repeat(MAX_MESSAGE_BYTES / 6),
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("selection exceeds the clipboard message limit ({MAX_MESSAGE_BYTES} bytes)")
    );
    connection.send(&update("still connected")).unwrap();
    connection.flush().unwrap();
    connection.stream.shutdown(Shutdown::Write).unwrap();
    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    assert!(
        matches!(serde_json::from_str::<ServerMessage>(&line).unwrap(), ServerMessage::Update { frame, .. } if frame.status == "still connected")
    );
}

#[test]
fn coalescing_preserves_a_partially_written_message() {
    let (server, client) = stream_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut connection = Connection::new(server).unwrap();
    let original = "x".repeat(8 * 1024 * 1024);
    connection.send(&update(&original)).unwrap();
    connection.flush().unwrap();
    assert!(connection.written > 0, "socket must block before reading");
    connection
        .send(&ServerMessage::ClipboardWrite {
            text: "copy".into(),
        })
        .unwrap();
    connection.send(&update("obsolete")).unwrap();
    connection.send(&update("latest")).unwrap();
    let reader = thread::spawn(move || {
        BufReader::new(client)
            .lines()
            .map(|line| serde_json::from_str::<ServerMessage>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !connection.output.is_empty() {
        connection.flush().unwrap();
        assert!(Instant::now() < deadline, "client did not drain the output");
        thread::yield_now();
    }
    connection.stream.shutdown(Shutdown::Write).unwrap();
    let messages = reader.join().unwrap();
    assert_eq!(messages.len(), 3);
    assert!(
        matches!(&messages[0], ServerMessage::Update { frame, .. } if frame.status == original)
    );
    assert!(matches!(&messages[1], ServerMessage::ClipboardWrite { text } if text == "copy"));
    assert!(
        matches!(&messages[2], ServerMessage::Update { frame, .. } if frame.status == "latest")
    );
}

#[test]
fn mouse_input_is_applied_once_only_in_the_attached_context() {
    use zess_ui::{MouseEvent, MouseKind};

    struct RunningServer {
        root: std::path::PathBuf,
        socket: std::path::PathBuf,
        thread: Option<thread::JoinHandle<Result<()>>>,
    }
    impl Drop for RunningServer {
        fn drop(&mut self) {
            if let Ok(mut stream) = UnixStream::connect(&self.socket) {
                serde_json::to_writer(&mut stream, &ClientMessage::Stop { force: true }).unwrap();
                stream.write_all(b"\n").unwrap();
            }
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap().unwrap();
            }
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn send(stream: &mut UnixStream, message: &ClientMessage) {
        serde_json::to_writer(&mut *stream, message).unwrap();
        stream.write_all(b"\n").unwrap();
    }
    fn receive(reader: &mut BufReader<UnixStream>) -> ServerMessage {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }
    fn acknowledged(reader: &mut BufReader<UnixStream>, sequence: u64) {
        loop {
            match receive(reader) {
                ServerMessage::Update { acknowledged, .. } if acknowledged >= sequence => break,
                ServerMessage::Update { .. } => {}
                message => panic!("unexpected response: {message:?}"),
            }
        }
    }
    let root = std::env::temp_dir().join(format!(
        "zess-mouse-server-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("note.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let socket = root.join("server.sock");
    let (workspace, server_socket, file) = (root.clone(), socket.clone(), path.clone());
    let handle = thread::spawn(move || {
        serve(
            &SocketPath::from(server_socket),
            Session::new(workspace, &[file], None)?,
        )
    });
    let server = RunningServer {
        root,
        socket,
        thread: Some(handle),
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stream = loop {
        if let Ok(stream) = UnixStream::connect(&server.socket) {
            break stream;
        }
        assert!(Instant::now() < deadline);
        assert!(!server.thread.as_ref().unwrap().is_finished());
        thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    send(
        &mut stream,
        &ClientMessage::Attach {
            client_id: "mouse-test".into(),
            width: 80,
            height: 24,
            files: Vec::new(),
            context_epoch: None,
        },
    );
    let ServerMessage::Attached {
        context_epoch,
        frame,
        ..
    } = receive(&mut reader)
    else {
        panic!("server did not attach")
    };
    for column in [5, 7] {
        send(
            &mut stream,
            &ClientMessage::Mouse {
                sequence: 1,
                context_epoch,
                mouse_context: frame.mouse_context,
                event: MouseEvent {
                    kind: MouseKind::Press,
                    column,
                    row: 0,
                },
            },
        );
        acknowledged(&mut reader, 1);
    }
    for (index, key) in ["r", "X", ":", "w", "Enter"].into_iter().enumerate() {
        let sequence = index as u64 + 2;
        send(
            &mut stream,
            &ClientMessage::Input {
                sequence,
                key: key.into(),
                context_epoch,
            },
        );
        acknowledged(&mut reader, sequence);
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), "abXdef\n");
    send(
        &mut stream,
        &ClientMessage::Mouse {
            sequence: 7,
            context_epoch: context_epoch.wrapping_add(1),
            mouse_context: frame.mouse_context,
            event: MouseEvent {
                kind: MouseKind::Press,
                column: 6,
                row: 0,
            },
        },
    );
    loop {
        match receive(&mut reader) {
            ServerMessage::Rejected { reason } => {
                assert_eq!(
                    reason,
                    "input context changed; reconnect before sending mouse input"
                );
                break;
            }
            ServerMessage::Update { .. } => {}
            message => panic!("unexpected response: {message:?}"),
        }
    }
}
