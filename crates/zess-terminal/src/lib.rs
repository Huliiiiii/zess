use std::{
    collections::VecDeque,
    io::{self, BufReader, Write},
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    SynchronizedUpdate,
    clipboard::CopyToClipboard,
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    event::{
        self, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{Attribute, Print, ResetColor, SetAttribute},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_ipc::{SocketPath, Stream, framing};
use zess_protocol::{ClientMessage, MAX_INPUT_BYTES, MAX_MESSAGE_BYTES, ServerMessage};
use zess_ui::{CursorShape, Frame, FrameRow, MouseEvent, MouseKind};

mod style;

#[cfg(test)]
mod tests;

const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_CONNECT_ATTEMPTS: u8 = 5;
const ATTACH_TIMEOUT: Duration = Duration::from_secs(3);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(16);
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(400);

fn read_clipboard(primary: bool) -> Result<String> {
    let mut candidates: Vec<(&str, Vec<&str>)> = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        candidates.push((
            "wl-paste",
            if primary {
                vec!["-p", "--no-newline"]
            } else {
                vec!["--no-newline"]
            },
        ));
    }
    if std::env::var_os("DISPLAY").is_some() {
        candidates.push((
            "xclip",
            vec![
                "-o",
                "-selection",
                if primary { "primary" } else { "clipboard" },
            ],
        ));
        candidates.push((
            "xsel",
            if primary {
                vec!["-o"]
            } else {
                vec!["-o", "-b"]
            },
        ));
    }
    if cfg!(target_os = "macos") && !primary {
        candidates.push(("pbpaste", vec![]));
    }
    for (program, args) in candidates {
        match Command::new(program).args(args).output() {
            Ok(output) if output.status.success() => {
                return String::from_utf8(output.stdout).context("clipboard is not UTF-8");
            }
            Ok(output) => anyhow::bail!(
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("run {program}")),
        }
    }
    anyhow::bail!("no local clipboard provider found (install wl-paste, xclip, or xsel)")
}

/// Run the terminal client connected to a local zess server socket.
pub fn run(socket: &SocketPath) -> Result<()> {
    run_files(socket, &[])
}

/// Run the terminal client and open the requested files on the first attach.
pub fn run_files(socket: &SocketPath, files: &[PathBuf]) -> Result<()> {
    let files = files
        .iter()
        .map(|path| {
            path.to_str()
                .with_context(|| format!("file path is not valid UTF-8: {}", path.display()))
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>>>()?;
    let client_id = format!("{}-{}", std::process::id(), unique_id());
    run_with_id(socket, client_id, files)
}

fn unique_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}

struct TerminalGuard {
    alternate_screen: bool,
    raw_mode: bool,
    mouse_capture: bool,
}

impl TerminalGuard {
    fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        if self.mouse_capture == enabled {
            return Ok(());
        }
        let mut stdout = io::stdout();
        if enabled {
            self.mouse_capture = true;
            // Button-motion tracking includes dragging, without idle-motion reports.
            stdout.write_all(b"\x1b[?1002h\x1b[?1006h")?;
            stdout.flush()?;
        } else {
            execute!(stdout, DisableMouseCapture)?;
            self.mouse_capture = false;
        }
        Ok(())
    }

    fn enter() -> Result<Self> {
        let mut guard = Self {
            alternate_screen: false,
            raw_mode: false,
            mouse_capture: false,
        };

        terminal::enable_raw_mode().context("enable terminal raw mode")?;
        guard.raw_mode = true;

        guard.alternate_screen = true;
        execute!(io::stdout(), EnterAlternateScreen).context("enter terminal alternate screen")?;
        execute!(io::stdout(), Hide, Clear(ClearType::All))
            .context("initialize terminal screen")?;

        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();

        if self.mouse_capture {
            let _ = execute!(stdout, DisableMouseCapture);
        }

        if self.alternate_screen {
            let _ = execute!(
                stdout,
                ResetColor,
                SetAttribute(Attribute::Reset),
                SetCursorStyle::DefaultUserShape,
                Show,
                LeaveAlternateScreen
            );
        }

        if self.raw_mode {
            let _ = terminal::disable_raw_mode();
        }
    }
}

struct Connection {
    writer: Stream,
    messages: Receiver<io::Result<ServerMessage>>,
    reader: Option<JoinHandle<()>>,
    attached: bool,
}

impl Connection {
    fn connect(
        socket: &SocketPath,
        client_id: &str,
        files: &[String],
        context_epoch: Option<u64>,
        width: u16,
        height: u16,
    ) -> Result<Self> {
        let writer = Stream::connect(socket)
            .with_context(|| format!("connect to zess socket {}", socket.display()))?;
        let reader_stream = writer.try_clone().context("clone zess socket")?;
        let (sender, messages) = mpsc::channel();

        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(reader_stream);
            loop {
                match framing::read(&mut reader, MAX_MESSAGE_BYTES) {
                    Ok(None) => {
                        let _ = sender.send(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "zess server closed the connection",
                        )));
                        break;
                    }
                    Ok(Some(line)) => match framing::decode(&line) {
                        Ok(message) => {
                            if sender.send(Ok(message)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ =
                                sender.send(Err(io::Error::new(io::ErrorKind::InvalidData, error)));
                            break;
                        }
                    },
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });

        let mut connection = Self {
            writer,
            messages,
            reader: Some(reader),
            attached: false,
        };

        connection.send(&ClientMessage::Attach {
            client_id: client_id.to_owned(),
            files: files.to_vec(),
            context_epoch,
            width,
            height,
        })?;

        Ok(connection)
    }

    fn send(&mut self, message: &ClientMessage) -> Result<()> {
        framing::write(&mut self.writer, message).context("write zess client message")?;
        self.writer.flush().context("flush zess client message")?;
        Ok(())
    }

    fn close(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);

        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

enum Recovery {
    ConfirmReplay { epoch: u64 },
    AwaitSkip { epoch: u64, through: u64 },
}

enum PendingInput {
    Key(String),
    Mouse,
}

fn run_with_id(socket: &SocketPath, client_id: String, files: Vec<String>) -> Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let (mut width, mut height) = terminal::size().unwrap_or((80, 24));

    let mut connection = None;
    let mut frame = Frame {
        width,
        height,
        ..Frame::default()
    };
    let mut pending = VecDeque::<(u64, PendingInput)>::new();
    let mut next_sequence = 1_u64;
    let mut acknowledged = 0_u64;
    let mut context_epoch = None;
    let mut recovery = None;
    let mut disconnected_since = Instant::now() - RECONNECT_INTERVAL;
    let mut connect_attempts = 0;
    let mut connected_since = Instant::now();
    let mut display_notice = Some("Connecting to zess server...".to_owned());
    let mut dirty = true;
    let mut clicks = ClickTracker::default();

    loop {
        if connection.is_none() && connect_attempts >= MAX_CONNECT_ATTEMPTS {
            anyhow::bail!(
                "could not attach to zess after {MAX_CONNECT_ATTEMPTS} attempts: {}",
                display_notice.as_deref().unwrap_or("connection lost")
            );
        }

        if connection.is_none() && disconnected_since.elapsed() >= RECONNECT_INTERVAL {
            connect_attempts += 1;
            match Connection::connect(socket, &client_id, &files, context_epoch, width, height) {
                Ok(new_connection) => {
                    connection = Some(new_connection);
                    connected_since = Instant::now();
                    display_notice = Some("Connected; attaching...".to_owned());
                    dirty = true;
                }
                Err(error) => {
                    disconnected_since = Instant::now();
                    let notice = format!("Connection failed: {error:#}");
                    if display_notice.as_deref() != Some(&notice) {
                        display_notice = Some(notice);
                        dirty = true;
                    }
                }
            }
        }

        if connection.is_some() {
            let mut disconnect = false;

            {
                let active = connection.as_mut().expect("connection checked above");

                loop {
                    match active.messages.try_recv() {
                        Ok(Ok(ServerMessage::Attached {
                            acknowledged: server_ack,
                            context_epoch: server_epoch,
                            replay_allowed,
                            frame: new_frame,
                        })) => {
                            acknowledged = acknowledged.max(server_ack);
                            discard_acknowledged(&mut pending, acknowledged);
                            next_sequence = next_sequence.max(acknowledged.saturating_add(1));
                            context_epoch = Some(server_epoch);
                            frame = new_frame;
                            active.attached = true;
                            connect_attempts = 0;
                            dirty = true;

                            match recovery {
                                Some(Recovery::AwaitSkip { through, .. })
                                    if acknowledged < through =>
                                {
                                    recovery = Some(Recovery::AwaitSkip {
                                        epoch: server_epoch,
                                        through,
                                    });
                                    display_notice = Some(
                                        "Discarding queued input; waiting for server confirmation"
                                            .to_owned(),
                                    );
                                    if let Err(error) = active.send(&ClientMessage::Skip {
                                        through,
                                        context_epoch: server_epoch,
                                    }) {
                                        display_notice = Some(format!(
                                            "Disconnected; retaining queued input: {error}"
                                        ));
                                        disconnect = true;
                                    }
                                }
                                Some(Recovery::AwaitSkip { .. }) => {
                                    recovery = None;
                                    display_notice = None;
                                }
                                Some(Recovery::ConfirmReplay { .. }) if !pending.is_empty() => {
                                    recovery = Some(Recovery::ConfirmReplay {
                                        epoch: server_epoch,
                                    });
                                    display_notice = Some(recovery_notice());
                                }
                                _ if pending.is_empty() => {
                                    recovery = None;
                                    display_notice = None;
                                }
                                _ if replay_allowed => {
                                    recovery = None;
                                    display_notice = None;
                                    if let Err(error) = send_pending(active, &pending, server_epoch)
                                    {
                                        display_notice = Some(format!(
                                            "Disconnected; retaining queued input: {error}"
                                        ));
                                        disconnect = true;
                                    }
                                }
                                _ => {
                                    recovery = Some(Recovery::ConfirmReplay {
                                        epoch: server_epoch,
                                    });
                                    display_notice = Some(recovery_notice());
                                }
                            }
                        }
                        Ok(Ok(ServerMessage::Update {
                            acknowledged: server_ack,
                            frame: new_frame,
                        })) => {
                            acknowledged = acknowledged.max(server_ack);
                            discard_acknowledged(&mut pending, acknowledged);
                            next_sequence = next_sequence.max(acknowledged.saturating_add(1));
                            frame = new_frame;
                            match recovery {
                                Some(Recovery::AwaitSkip { through, .. })
                                    if acknowledged >= through =>
                                {
                                    recovery = None;
                                    display_notice = None;
                                }
                                Some(Recovery::ConfirmReplay { .. }) if pending.is_empty() => {
                                    recovery = None;
                                    display_notice = None;
                                }
                                None => display_notice = None,
                                _ => {}
                            }
                            dirty = true;
                        }
                        Ok(Ok(ServerMessage::ClipboardWrite { text })) => {
                            execute!(io::stdout(), CopyToClipboard::to_clipboard_from(&text))
                                .context("write clipboard to terminal")?;
                        }
                        Ok(Ok(ServerMessage::ClipboardWritePrimary { text })) => {
                            execute!(io::stdout(), CopyToClipboard::to_primary_from(&text))
                                .context("write primary clipboard to terminal")?;
                        }
                        Ok(Ok(ServerMessage::ClipboardRead { id, primary })) => {
                            let result = read_clipboard(primary);
                            let (text, error) = match result {
                                Ok(text) => (Some(text), None),
                                Err(error) => (None, Some(format!("{error:#}"))),
                            };
                            let mut response = ClientMessage::ClipboardResponse { id, text, error };
                            if framing::encode(&response)?.len() > MAX_INPUT_BYTES {
                                response = ClientMessage::ClipboardResponse {
                                    id,
                                    text: None,
                                    error: Some("clipboard contents exceed the input limit".into()),
                                };
                            }
                            active.send(&response)?;
                        }
                        Ok(Ok(ServerMessage::Rejected { reason })) => {
                            if pending.is_empty() {
                                return Err(anyhow::anyhow!(
                                    "zess server rejected the client: {}",
                                    sanitize(&reason)
                                ));
                            }

                            display_notice = Some(format!(
                                "Server rejected reconnect; retaining queued input: {}",
                                sanitize(&reason)
                            ));
                            disconnect = true;
                            dirty = true;
                            break;
                        }
                        Ok(Ok(ServerMessage::Exited)) => return Ok(()),
                        Ok(Ok(ServerMessage::Status { .. })) => {
                            return Err(anyhow::anyhow!("unexpected server status response"));
                        }
                        Ok(Err(error)) => {
                            display_notice = Some(format!("Disconnected; reconnecting: {error}"));
                            disconnect = true;
                            dirty = true;
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            display_notice = Some("Disconnected; reconnecting...".to_owned());
                            disconnect = true;
                            dirty = true;
                            break;
                        }
                    }
                }

                if !active.attached && connected_since.elapsed() >= ATTACH_TIMEOUT {
                    display_notice = Some("Timed out waiting for server attachment".to_owned());
                    disconnect = true;
                }
            }

            if disconnect {
                if let Some(mut old) = connection.take() {
                    old.close();
                }
                disconnected_since = Instant::now();
                dirty = true;
            }
        }

        if dirty {
            terminal.set_mouse_capture(
                frame.mouse_enabled
                    && connection.as_ref().is_some_and(|active| active.attached)
                    && recovery.is_none(),
            )?;
            if !terminal.mouse_capture {
                clicks = ClickTracker::default();
            }
            let notice = display_notice.as_ref().map(|notice| {
                if connection.as_ref().is_none_or(|active| !active.attached) {
                    format!("Esc/Ctrl-c exit | {notice}")
                } else if recovery.is_some() {
                    format!("Ctrl-c exit | {notice}")
                } else {
                    notice.clone()
                }
            });
            render(&mut io::stdout(), &frame, width, height, notice.as_deref())?;
            dirty = false;
        }

        if !event::poll(EVENT_POLL_INTERVAL).context("poll terminal input")? {
            continue;
        }

        let event = event::read().context("read terminal input")?;
        if !matches!(event, Event::Mouse(_)) {
            clicks = ClickTracker::default();
        }
        match event {
            Event::Resize(new_width, new_height) => {
                width = new_width;
                height = new_height;
                dirty = true;

                let mut disconnect = false;
                if let Some(active) = connection.as_mut()
                    && active.attached
                    && let Err(error) = active.send(&ClientMessage::Resize { width, height })
                {
                    display_notice = Some(format!("Disconnected; reconnecting: {error}"));
                    disconnect = true;
                }

                if disconnect {
                    if let Some(mut old) = connection.take() {
                        old.close();
                    }
                    disconnected_since = Instant::now();
                }
            }
            Event::Mouse(mouse)
                if terminal.mouse_capture && frame.width == width && frame.height == height =>
            {
                let Some(kind) = clicks.at(mouse.kind, mouse.column, mouse.row, Instant::now())
                else {
                    continue;
                };
                let sequence = next_sequence;
                next_sequence = next_sequence.saturating_add(1);
                // Coordinates are valid only in this connection's displayed layout.
                // A reconnect resolves the sequence with Skip, never replays a gesture.
                pending.push_back((sequence, PendingInput::Mouse));
                if let Some(active) = connection.as_mut()
                    && let Err(error) = active.send(&ClientMessage::Mouse {
                        sequence,
                        context_epoch: context_epoch
                            .expect("attached clients have a context epoch"),
                        mouse_context: frame.mouse_context,
                        event: MouseEvent {
                            kind,
                            column: mouse.column,
                            row: mouse.row,
                        },
                    })
                {
                    display_notice = Some(format!("Disconnected; reconnecting: {error}"));
                    if let Some(mut old) = connection.take() {
                        old.close();
                    }
                    disconnected_since = Instant::now();
                    dirty = true;
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let Some(active) = connection.as_mut() {
                        let _ = active.send(&ClientMessage::Detach);
                    }
                    return Ok(());
                }

                let is_disconnected = connection.as_ref().is_none_or(|active| !active.attached);
                let cancel =
                    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                if (is_disconnected && key.code == KeyCode::Esc)
                    || ((is_disconnected || recovery.is_some()) && cancel)
                {
                    return Ok(());
                }
                if is_disconnected {
                    continue;
                }

                match recovery {
                    Some(Recovery::ConfirmReplay { epoch }) => {
                        if key.code == KeyCode::Char('r')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            recovery = None;
                            context_epoch = Some(epoch);
                            display_notice = None;

                            let mut disconnect = false;
                            if let Some(active) = connection.as_mut()
                                && let Err(error) = send_pending(active, &pending, epoch)
                            {
                                display_notice =
                                    Some(format!("Disconnected; retaining queued input: {error}"));
                                disconnect = true;
                            }
                            if disconnect {
                                if let Some(mut old) = connection.take() {
                                    old.close();
                                }
                                disconnected_since = Instant::now();
                            }
                        } else if key.code == KeyCode::Esc {
                            let through = pending.back().map(|(sequence, _)| *sequence);
                            if let Some(through) = through {
                                recovery = Some(Recovery::AwaitSkip { epoch, through });
                                display_notice = Some(
                                    "Discarding queued input; waiting for server confirmation"
                                        .to_owned(),
                                );

                                let mut disconnect = false;
                                if let Some(active) = connection.as_mut()
                                    && let Err(error) = active.send(&ClientMessage::Skip {
                                        through,
                                        context_epoch: epoch,
                                    })
                                {
                                    display_notice = Some(format!(
                                        "Disconnected; retaining queued input: {error}"
                                    ));
                                    disconnect = true;
                                }
                                if disconnect {
                                    if let Some(mut old) = connection.take() {
                                        old.close();
                                    }
                                    disconnected_since = Instant::now();
                                }
                            }
                        }
                        dirty = true;
                        continue;
                    }
                    Some(Recovery::AwaitSkip { epoch, through }) => {
                        if key.code == KeyCode::Esc {
                            let mut disconnect = false;
                            if let Some(active) = connection.as_mut()
                                && let Err(error) = active.send(&ClientMessage::Skip {
                                    through,
                                    context_epoch: epoch,
                                })
                            {
                                display_notice =
                                    Some(format!("Disconnected; retaining queued input: {error}"));
                                disconnect = true;
                            }
                            if disconnect {
                                if let Some(mut old) = connection.take() {
                                    old.close();
                                }
                                disconnected_since = Instant::now();
                            }
                            dirty = true;
                        }
                        continue;
                    }
                    None => {}
                }

                let Some(encoded) = encode_key(key) else {
                    continue;
                };

                let sequence = next_sequence;
                next_sequence = next_sequence.saturating_add(1);
                pending.push_back((sequence, PendingInput::Key(encoded.clone())));
                let input_epoch = context_epoch.expect("attached clients have a context epoch");

                let mut disconnect = false;
                if let Some(active) = connection.as_mut()
                    && let Err(error) = active.send(&ClientMessage::Input {
                        sequence,
                        key: encoded,
                        context_epoch: input_epoch,
                    })
                {
                    display_notice = Some(format!("Disconnected; reconnecting: {error}"));
                    disconnect = true;
                    // Preserve this sequence; the server may have processed it before
                    // the write failed, and its acknowledgement resolves the ambiguity.
                }

                if disconnect {
                    if let Some(mut old) = connection.take() {
                        old.close();
                    }
                    disconnected_since = Instant::now();
                    dirty = true;
                }
            }
            _ => {}
        }
    }
}

fn discard_acknowledged(pending: &mut VecDeque<(u64, PendingInput)>, acknowledged: u64) {
    while pending
        .front()
        .is_some_and(|(sequence, _)| *sequence <= acknowledged)
    {
        pending.pop_front();
    }
}

fn send_pending(
    connection: &mut Connection,
    pending: &VecDeque<(u64, PendingInput)>,
    context_epoch: u64,
) -> Result<()> {
    for (sequence, input) in pending {
        let message = match input {
            PendingInput::Key(key) => ClientMessage::Input {
                sequence: *sequence,
                key: key.clone(),
                context_epoch,
            },
            PendingInput::Mouse => ClientMessage::Skip {
                through: *sequence,
                context_epoch,
            },
        };
        connection.send(&message)?;
    }

    Ok(())
}

fn recovery_notice() -> String {
    "Session changed: Ctrl-r replay queued input here; Esc discard queued input".to_owned()
}

struct Click {
    at: Instant,
    column: u16,
    row: u16,
}

#[derive(Default)]
enum ClickTracker {
    #[default]
    Idle,
    Pressed(Click),
    Released(Click),
}

impl ClickTracker {
    fn at(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        now: Instant,
    ) -> Option<MouseKind> {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Self::Released(click) = self
                    && click.column == column
                    && click.row == row
                    && now.duration_since(click.at) < DOUBLE_CLICK_INTERVAL
                {
                    *self = Self::Idle;
                    return Some(MouseKind::DoubleClick);
                }
                *self = Self::Pressed(Click {
                    at: now,
                    column,
                    row,
                });
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Self::Pressed(click) = std::mem::take(self)
                    && click.column == column
                    && click.row == row
                {
                    *self = Self::Released(click);
                }
            }
            MouseEventKind::Moved => {
                if let Self::Pressed(click) | Self::Released(click) = self
                    && (click.column != column || click.row != row)
                {
                    *self = Self::Idle;
                }
            }
            _ => *self = Self::Idle,
        }
        mouse_kind(kind)
    }
}

fn mouse_kind(kind: MouseEventKind) -> Option<MouseKind> {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => Some(MouseKind::Press),
        MouseEventKind::Drag(MouseButton::Left) => Some(MouseKind::Drag),
        MouseEventKind::Up(MouseButton::Left) => Some(MouseKind::Release),
        MouseEventKind::ScrollUp => Some(MouseKind::ScrollUp),
        MouseEventKind::ScrollDown => Some(MouseKind::ScrollDown),
        _ => None,
    }
}

fn encode_key(event: KeyEvent) -> Option<String> {
    let modifiers = event.modifiers;
    let key = match event.code {
        KeyCode::Esc => "Esc".to_owned(),
        KeyCode::Enter => "Enter".to_owned(),
        KeyCode::Backspace => "Backspace".to_owned(),
        KeyCode::Delete => "Delete".to_owned(),
        KeyCode::Tab => "Tab".to_owned(),
        KeyCode::BackTab => "S-Tab".to_owned(),
        KeyCode::Left => "Left".to_owned(),
        KeyCode::Right => "Right".to_owned(),
        KeyCode::Up => "Up".to_owned(),
        KeyCode::Down => "Down".to_owned(),
        KeyCode::Home => "Home".to_owned(),
        KeyCode::End => "End".to_owned(),
        KeyCode::PageUp => "PageUp".to_owned(),
        KeyCode::PageDown => "PageDown".to_owned(),
        KeyCode::Char(character) => {
            let character = if modifiers.contains(KeyModifiers::CONTROL) {
                character.to_ascii_lowercase()
            } else {
                character
            };
            let mut prefix = String::new();
            if modifiers.contains(KeyModifiers::CONTROL) {
                prefix.push_str("C-");
            }
            if modifiers.contains(KeyModifiers::ALT) {
                prefix.push_str("A-");
            }
            format!("{prefix}{character}")
        }
        _ => return None,
    };

    Some(key)
}

fn render(
    stdout: &mut impl Write,
    frame: &Frame,
    width: u16,
    height: u16,
    notice: Option<&str>,
) -> Result<()> {
    stdout
        .sync_update(|mut stdout| {
            let body_rows = height.saturating_sub(1) as usize;

            for row in 0..body_rows {
                queue!(
                    stdout,
                    MoveTo(0, row as u16),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                    Clear(ClearType::CurrentLine)
                )?;

                if let Some(frame_row) = frame.rows.get(row) {
                    render_row(&mut stdout, frame_row, width, &frame.styles)?;
                }
            }

            let on_prompt = frame
                .cursor
                .is_some_and(|(_, y)| y == frame.height.saturating_sub(1));
            let prompt = sanitize(&frame.prompt);
            let visible_prompt = clip_to_width(&prompt, width.saturating_sub(1) as usize).0;
            if height > 0 {
                queue!(
                    stdout,
                    MoveTo(0, height - 1),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                    Clear(ClearType::CurrentLine)
                )?;
                if on_prompt && notice.is_none() {
                    queue!(stdout, Print(&visible_prompt))?;
                } else {
                    let pending = clip_to_width(&prompt, width as usize).0;
                    let pending_width = UnicodeWidthStr::width(pending.as_str());
                    let message_width = if pending_width > 0 && notice.is_none() {
                        (width as usize).saturating_sub(pending_width + 1)
                    } else {
                        width as usize
                    };
                    queue!(
                        stdout,
                        Print(
                            clip_to_width(
                                &sanitize(notice.unwrap_or(&frame.status)),
                                message_width
                            )
                            .0
                        )
                    )?;
                    if pending_width > 0 && notice.is_none() {
                        queue!(
                            stdout,
                            MoveTo(width - pending_width as u16, height - 1),
                            Print(pending)
                        )?;
                    }
                }
            }

            if let Some((x, y)) = frame.cursor.filter(|_| notice.is_none()) {
                if width > 0 && height > 0 {
                    let x = x.min(width - 1);
                    let y = if on_prompt {
                        height - 1
                    } else {
                        y.min(height.saturating_sub(2))
                    };
                    let style = match frame.cursor_shape {
                        CursorShape::Block => SetCursorStyle::SteadyBlock,
                        CursorShape::Bar => SetCursorStyle::SteadyBar,
                        CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
                    };
                    queue!(stdout, style, MoveTo(x, y), Show)?;
                } else {
                    queue!(stdout, Hide)?;
                }
            } else {
                queue!(stdout, Hide)?;
            }

            queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
            Ok(())
        })
        .context("flush terminal frame")?
}

fn render_row(
    stdout: &mut impl Write,
    row: &FrameRow,
    width: u16,
    styles: &std::collections::HashMap<String, zess_ui::TextStyle>,
) -> Result<()> {
    let mut remaining = width as usize;

    for span in &row.spans {
        if remaining == 0 {
            break;
        }

        let resolve = |role: &str| style::resolve_in(role, styles);
        let base = span.base_style.as_deref().map(resolve).unwrap_or_default();
        style::apply(stdout, &base.patch(&resolve(&span.style)))?;
        let (text, cells, truncated) = clip_to_width(&sanitize(&span.text), remaining);
        queue!(stdout, Print(text))?;
        remaining -= cells;
        if truncated {
            break;
        }
    }

    queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
    Ok(())
}

fn clip_to_width(text: &str, maximum: usize) -> (String, usize, bool) {
    let mut output = String::new();
    let mut cells = 0;

    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        if cells + width > maximum {
            return (output, cells, true);
        }

        output.push_str(grapheme);
        cells += width;
    }

    (output, cells, false)
}

fn sanitize(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{1b}' => escaped.push_str("\\x1b"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{{{:x}}}", character as u32));
            }
            character => escaped.push(character),
        }
    }

    escaped
}
