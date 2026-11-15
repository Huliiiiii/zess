mod client;

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};

use zess_ipc::Listener;

use super::*;

fn listener() -> (SocketPath, Listener) {
    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
    let socket = SocketPath::from(std::env::temp_dir().join(format!(
        "zess-terminal-ipc-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    )));
    let listener = Listener::bind(&socket).unwrap();
    (socket, listener)
}

#[test]
fn a_frame_is_presented_after_all_rows_are_written() {
    let frame = Frame {
        width: 20,
        height: 3,
        rows: vec![FrameRow {
            spans: vec![zess_ui::Span {
                base_style: None,
                text: "commit history".into(),
                style: String::new(),
            }],
        }],
        ..Default::default()
    };
    let mut output = Vec::new();
    render(&mut output, &frame, 20, 3, None).unwrap();
    let output = String::from_utf8(output).unwrap();
    let begin = output.find("\u{1b}[?2026h").unwrap();
    let content = output.find("commit history").unwrap();
    let end = output.find("\u{1b}[?2026l").unwrap();
    assert!(begin < content && content < end);
    assert_eq!(output.matches("\u{1b}[?2026h").count(), 1);
    assert_eq!(output.matches("\u{1b}[?2026l").count(), 1);
}

#[test]
fn narrow_rows_keep_joined_emoji_and_following_cells_visible() {
    crossterm::style::force_color_output(true);
    let row = FrameRow {
        spans: vec![zess_ui::Span {
            base_style: None,
            text: "a👩‍👩‍👧‍👦bcdef".into(),
            style: String::new(),
        }],
    };
    for (width, expected) in [
        (3, "\x1b[0m\x1b[39m\x1b[49ma👩‍👩‍👧‍👦\x1b[0m\x1b[0m"),
        (7, "\x1b[0m\x1b[39m\x1b[49ma👩‍👩‍👧‍👦bcde\x1b[0m\x1b[0m"),
    ] {
        let mut output = Vec::new();
        render_row(&mut output, &row, width, &Default::default()).unwrap();
        assert_eq!(output, expected.as_bytes());
    }
}

#[test]
fn reconnect_replays_keys_and_discards_mouse_coordinates_in_sequence() {
    let (socket, listener) = listener();
    let writer = Stream::connect(&socket).unwrap();
    let reader = listener.accept().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    writer.set_nonblocking(true).unwrap();
    let mut connection = Connection {
        stream: Box::new(LocalConnection::new(
            writer.try_clone().unwrap(),
            NativeDaemonControl.watch(&writer).unwrap(),
        )),
        input: framing::Decoder::default(),
        output: VecDeque::new(),
        written: 0,
        pending_attach: None,
    };
    let pending = VecDeque::from([
        (1, PendingInput::Key("i".into())),
        (2, PendingInput::Mouse),
        (3, PendingInput::Key("x".into())),
    ]);
    send_pending(
        &mut connection,
        &pending,
        42,
        &ServerInstanceId("same".into()),
    )
    .unwrap();
    connection.poll().unwrap();
    let messages = BufReader::new(reader)
        .lines()
        .take(3)
        .map(|line| serde_json::from_str::<ClientMessage>(&line.unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert!(
        matches!(&messages[0], ClientMessage::Input { sequence: 1, key, context_epoch: 42 } if key == "i")
    );
    assert!(matches!(
        &messages[1],
        ClientMessage::Skip {
            through: 2,
            context_epoch: 42
        }
    ));
    assert!(
        matches!(&messages[2], ClientMessage::Input { sequence: 3, key, context_epoch: 42 } if key == "x")
    );
}

#[test]
fn dropping_a_connection_closes_the_socket_without_waiting_for_the_peer() {
    let (socket, listener) = listener();
    let connection = Connection::connect(
        &mut LocalConnectionFactory(socket.clone()),
        "close",
        &[],
        None,
        None,
        80,
        24,
    )
    .unwrap();
    let mut peer = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    drop(connection);
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn reconnecting_to_a_replacement_server_preserves_pending_input_and_rejects_attachment() {
    let mut ledger = InputLedger::new();
    ledger.attach(ServerInstanceId("old".into()), 0).unwrap();
    ledger.push(PendingInput::Key("i".into()));
    ledger.push(PendingInput::Key("x".into()));
    ledger.acknowledge(1);
    assert!(ledger.attach(ServerInstanceId("new".into()), 0).is_err());
    assert_eq!(ledger.server_instance, Some(ServerInstanceId("old".into())));
    assert_eq!(ledger.pending.len(), 1);
    assert_eq!(ledger.pending[0].0, 2);
    assert!(matches!(&ledger.pending[0].1, PendingInput::Key(key) if key == "x"));
}

#[test]
fn reconnecting_to_the_same_server_preserves_the_deduplication_sequence() {
    let mut ledger = InputLedger::new();
    ledger.attach(ServerInstanceId("same".into()), 0).unwrap();
    ledger.push(PendingInput::Key("i".into()));
    ledger.push(PendingInput::Key("x".into()));
    ledger.acknowledge(1);
    ledger.attach(ServerInstanceId("same".into()), 2).unwrap();
    assert!(ledger.pending.is_empty());
    assert_eq!(ledger.push(PendingInput::Key("y".into())), 3);
}

#[test]
fn a_peer_that_stops_reading_does_not_block_terminal_progress() {
    let (socket, listener) = listener();
    let mut connection = Connection::connect(
        &mut LocalConnectionFactory(socket.clone()),
        "stalled",
        &[],
        None,
        None,
        80,
        24,
    )
    .unwrap();
    let _peer = listener.accept().unwrap();
    for sequence in 1..16 {
        connection
            .send(&ClientMessage::Input {
                sequence,
                key: "x".repeat(512 * 1024),
                context_epoch: 1,
            })
            .unwrap();
    }
    let started = Instant::now();
    for _ in 0..32 {
        connection.poll().unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        !connection.output.is_empty(),
        "the peer must exert backpressure"
    );
}

#[test]
fn idle_motion_and_other_buttons_do_not_generate_editor_input() {
    assert_eq!(mouse_kind(MouseEventKind::Moved), None);
    assert_eq!(mouse_kind(MouseEventKind::Down(MouseButton::Right)), None);
    assert_eq!(mouse_kind(MouseEventKind::Down(MouseButton::Middle)), None);
    assert_eq!(
        mouse_kind(MouseEventKind::Drag(MouseButton::Left)),
        Some(MouseKind::Drag)
    );
}

#[test]
fn two_clicks_in_the_same_cell_generate_one_double_click() {
    let mut clicks = ClickTracker::default();
    let now = Instant::now();
    assert_eq!(
        clicks.at(MouseEventKind::Down(MouseButton::Left), 4, 2, now),
        Some(MouseKind::Press)
    );
    assert_eq!(
        clicks.at(
            MouseEventKind::Up(MouseButton::Left),
            4,
            2,
            now + Duration::from_millis(40),
        ),
        Some(MouseKind::Release)
    );
    assert_eq!(
        clicks.at(
            MouseEventKind::Down(MouseButton::Left),
            4,
            2,
            now + Duration::from_millis(150),
        ),
        Some(MouseKind::DoubleClick)
    );
    clicks.at(
        MouseEventKind::Up(MouseButton::Left),
        4,
        2,
        now + Duration::from_millis(190),
    );
    assert_eq!(
        clicks.at(
            MouseEventKind::Down(MouseButton::Left),
            4,
            2,
            now + Duration::from_millis(250),
        ),
        Some(MouseKind::Press)
    );
}

#[test]
fn slow_clicks_and_clicks_in_another_cell_remain_single_clicks() {
    for (column, row, delay) in [
        (4, 2, DOUBLE_CLICK_INTERVAL),
        (5, 2, Duration::from_millis(100)),
        (4, 3, Duration::from_millis(100)),
    ] {
        let mut clicks = ClickTracker::default();
        let now = Instant::now();
        clicks.at(MouseEventKind::Down(MouseButton::Left), 4, 2, now);
        clicks.at(MouseEventKind::Up(MouseButton::Left), 4, 2, now);
        assert_eq!(
            clicks.at(
                MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                now + delay,
            ),
            Some(MouseKind::Press)
        );
    }
}

#[test]
fn dragging_scrolling_and_other_buttons_interrupt_double_clicks() {
    for interruption in [
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::ScrollDown,
        MouseEventKind::Down(MouseButton::Right),
        MouseEventKind::Down(MouseButton::Middle),
    ] {
        let mut clicks = ClickTracker::default();
        let now = Instant::now();
        clicks.at(MouseEventKind::Down(MouseButton::Left), 4, 2, now);
        clicks.at(interruption, 4, 2, now);
        clicks.at(MouseEventKind::Up(MouseButton::Left), 4, 2, now);
        assert_eq!(
            clicks.at(
                MouseEventKind::Down(MouseButton::Left),
                4,
                2,
                now + Duration::from_millis(100),
            ),
            Some(MouseKind::Press)
        );
    }
}

#[cfg(unix)]
#[test]
fn a_waiting_clipboard_provider_does_not_block_terminal_progress() {
    let mut job = ClipboardJob::spawn("sleep", &["10"]).unwrap();
    let started = Instant::now();
    assert!(job.poll().is_none());
    assert!(started.elapsed() < Duration::from_millis(100));
    job.deadline = Instant::now();
    assert_eq!(
        job.poll().unwrap().unwrap_err().to_string(),
        "clipboard provider timed out"
    );
}

#[cfg(unix)]
#[test]
fn clipboard_output_larger_than_a_pipe_is_read_completely() {
    let mut job =
        ClipboardJob::spawn("sh", &["-c", "head -c 200000 /dev/zero | tr '\\000' x"]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let text = loop {
        if let Some(result) = job.poll() {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(text, "x".repeat(200000));
}

#[cfg(unix)]
#[test]
fn a_clipboard_response_completed_while_disconnected_is_reused_after_reconnect() {
    let job = ClipboardJob::spawn("printf", &["external"]).unwrap();
    let instance = ServerInstanceId("same".into());
    let mut clipboard = ClipboardRead::Reading {
        server_instance: instance.clone(),
        id: 7,
        job,
    };
    let mut disconnected = ClientConnection::Disconnected {
        retry_at: Instant::now(),
        attempts: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while !matches!(clipboard, ClipboardRead::Ready { .. }) {
        clipboard.poll(&mut disconnected).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let (socket, listener) = listener();
    let mut connection = Connection::connect(
        &mut LocalConnectionFactory(socket.clone()),
        "clipboard-response",
        &[],
        None,
        None,
        80,
        24,
    )
    .unwrap();
    let peer = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    clipboard
        .request(
            &instance,
            7,
            false,
            &mut connection,
            &mut NativeClipboard::default(),
        )
        .unwrap();
    connection.poll().unwrap();
    let messages = BufReader::new(peer)
        .lines()
        .take(2)
        .map(|line| serde_json::from_str::<ClientMessage>(&line.unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert!(
        matches!(&messages[1], ClientMessage::ClipboardResponse { id: 7, text: Some(text), error: None } if text == "external")
    );
}

#[test]
fn reconnecting_to_another_server_discards_the_previous_clipboard_response() {
    let first = ServerInstanceId("first".into());
    let second = ServerInstanceId("second".into());
    let mut clipboard = ClipboardRead::<ClipboardJob>::Ready {
        server_instance: first.clone(),
        id: 7,
        response: clipboard_response(7, Ok("previous server".into())).unwrap(),
    };
    clipboard.attach(&first);
    assert!(
        matches!(&clipboard, ClipboardRead::Ready { id: 7, response: ClientMessage::ClipboardResponse { text: Some(text), .. }, .. } if text == "previous server")
    );
    clipboard.attach(&second);
    assert!(matches!(clipboard, ClipboardRead::Idle));
}

#[test]
fn a_limit_sized_message_and_the_next_message_can_share_a_socket_read() {
    let (socket, listener) = listener();
    let mut connection = Connection::connect(
        &mut LocalConnectionFactory(socket.clone()),
        "frame-limit",
        &[],
        None,
        None,
        80,
        24,
    )
    .unwrap();
    let mut peer = listener.accept().unwrap();
    peer.write_all(
        &framing::encode(&ServerMessage::Hello {
            build_id: zess_build::BUILD_ID.into(),
        })
        .unwrap(),
    )
    .unwrap();
    connection.poll().unwrap();
    let envelope = framing::encode(&ServerMessage::ClipboardWrite {
        text: String::new(),
    })
    .unwrap()
    .len();
    let text = "x".repeat(MAX_MESSAGE_BYTES - envelope);
    let message = framing::encode(&ServerMessage::ClipboardWrite { text: text.clone() }).unwrap();
    assert_eq!(message.len(), MAX_MESSAGE_BYTES);
    // The retained prefix models earlier fragmented socket reads. Both final
    // delimiters arrive together, so the byte limit must apply to each frame.
    let split = message.len() - 100;
    connection
        .input
        .push(&message[..split], MAX_MESSAGE_BYTES)
        .unwrap();
    let mut tail = message[split..].to_vec();
    tail.extend(framing::encode(&ServerMessage::Exited).unwrap());
    peer.write_all(&tail).unwrap();
    let messages = connection.poll().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(
        matches!(&messages[0], ServerMessage::ClipboardWrite { text: actual } if actual == &text)
    );
    assert!(matches!(messages[1], ServerMessage::Exited));
}

#[cfg(windows)]
#[test]
fn primary_selection_is_unavailable_in_both_native_and_terminal_clipboards() {
    let mut native = NativeClipboard::default();
    let read_error = native.read(true).err().unwrap();
    assert_eq!(
        read_error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::Unsupported
    );
    let mut output = Vec::new();
    let write_error = native
        .write(true, "text".into(), &mut output)
        .err()
        .unwrap();
    assert_eq!(
        write_error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::Unsupported
    );
    let error = TerminalClipboard::default()
        .write(true, "text".into(), &mut output)
        .err()
        .unwrap();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(output.is_empty());
}
