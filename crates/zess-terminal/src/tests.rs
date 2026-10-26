use std::io::BufRead;
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
    let (_, messages) = mpsc::channel();
    let mut connection = Connection {
        writer,
        messages,
        reader: None,
        attached: true,
    };
    let pending = VecDeque::from([
        (1, PendingInput::Key("i".into())),
        (2, PendingInput::Mouse),
        (3, PendingInput::Key("x".into())),
    ]);
    send_pending(&mut connection, &pending, 42).unwrap();
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
fn closing_a_connection_stops_the_reader_while_the_server_remains_open() {
    let (socket, listener) = listener();
    let mut connection = Connection::connect(&socket, "reader-close", &[], None, 80, 24).unwrap();
    let peer = listener.accept().unwrap();
    let (closed, result) = mpsc::channel();
    let closer = thread::spawn(move || {
        connection.close();
        while connection.messages.try_recv().is_ok() {}
        closed.send(connection.messages.try_recv()).unwrap();
    });
    let outcome = result.recv_timeout(Duration::from_secs(2));
    drop(peer);
    closer.join().unwrap();
    assert!(
        matches!(outcome, Ok(Err(mpsc::TryRecvError::Disconnected))),
        "closing the connection left its reader blocked"
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
