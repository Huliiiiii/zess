use std::{
    collections::{HashMap, VecDeque},
    io::{ErrorKind, Read, Write},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use zess_ipc::{Listener, SocketPath, Stream, framing};
use zess_protocol::{ClientMessage, MAX_INPUT_BYTES, MAX_MESSAGE_BYTES, ServerMessage};

use crate::Session;

#[cfg(test)]
mod tests;

struct PendingMessage {
    bytes: Vec<u8>,
    update: bool,
}

struct Connection {
    stream: Stream,
    input: framing::Decoder,
    output: VecDeque<PendingMessage>,
    written: usize,
    client: Option<String>,
    accepted: Instant,
    closing: bool,
    eof: bool,
}

impl Connection {
    fn new(stream: Stream) -> Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            input: framing::Decoder::default(),
            output: VecDeque::new(),
            written: 0,
            client: None,
            accepted: Instant::now(),
            closing: false,
            eof: false,
        })
    }

    fn send(&mut self, message: &ServerMessage) -> Result<()> {
        let bytes = framing::encode(message)?;
        if matches!(
            message,
            ServerMessage::ClipboardWrite { .. } | ServerMessage::ClipboardWritePrimary { .. }
        ) && bytes.len() > MAX_MESSAGE_BYTES
        {
            bail!(
                "selection exceeds the clipboard message limit ({} bytes)",
                MAX_MESSAGE_BYTES
            );
        }
        let update = matches!(message, ServerMessage::Update { .. });
        // Preserve the first message in case it has been partially written, and
        // coalesce only adjacent frames so one-time effects keep their order.
        if update && self.output.len() > 1 && self.output.back().unwrap().update {
            self.output.pop_back();
        }
        self.output.push_back(PendingMessage { bytes, update });
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        while let Some(message) = self.output.front() {
            match self.stream.write(&message.bytes[self.written..]) {
                Ok(0) => bail!("client closed"),
                Ok(count) => {
                    self.written += count;
                    if self.written == message.bytes.len() {
                        self.output.pop_front();
                        self.written = 0;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Vec<ClientMessage>> {
        let mut bytes = [0; 8192];
        loop {
            match self.stream.read(&mut bytes) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(count) => {
                    self.input.push(&bytes[..count], MAX_INPUT_BYTES)?;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let mut messages = Vec::new();
        while let Some(message) = self.input.decode_next()? {
            messages.push(message);
        }
        Ok(messages)
    }
}

/// Serve a single active terminal while retaining all session state on detach.
pub fn serve(socket: &SocketPath, mut session: Session) -> Result<()> {
    let listener = Listener::bind(socket)?;
    listener.set_nonblocking(true)?;
    let mut connections = Vec::<Connection>::new();
    let mut active = None::<String>;
    let mut acknowledged = HashMap::<String, u64>::new();
    let mut previous_client = None::<String>;
    let mut stopping = false;
    let mut context_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos() as u64;

    loop {
        loop {
            match listener.accept() {
                Ok(stream) => connections.push(Connection::new(stream)?),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let mut index = 0;
        while index < connections.len() {
            let connection = &mut connections[index];
            let messages = if connection.closing {
                Ok(Vec::new())
            } else {
                connection.receive()
            };
            let mut disconnected = messages.is_err();
            if let Ok(messages) = messages {
                for message in messages {
                    match message {
                        ClientMessage::Status if connection.client.is_none() => {
                            connection.send(&ServerMessage::Status {
                                pid: std::process::id(),
                                workspace: session.workspace().to_string_lossy().into_owned(),
                                attached: active.is_some(),
                                modified: session.modified(),
                            })?;
                            connection.closing = true;
                            break;
                        }
                        ClientMessage::Stop { force } if connection.client.is_none() => {
                            if !force && session.modified() {
                                connection.send(&ServerMessage::Rejected {
                                    reason: "unsaved buffers; save them first or use --force to discard them".into(),
                                })?;
                                connection.closing = true;
                            } else {
                                stopping = true;
                            }
                            break;
                        }
                        ClientMessage::Attach {
                            client_id,
                            width,
                            height,
                            files,
                            context_epoch: previous_epoch,
                        } if connection.client.is_none() => {
                            if active.is_some() {
                                connection.send(&ServerMessage::Rejected {
                                    reason:
                                        "another terminal is attached; detach it with Ctrl-z first"
                                            .into(),
                                })?;
                                connection.closing = true;
                            } else if client_id.is_empty() || client_id.len() > 128 {
                                disconnected = true;
                            } else {
                                if previous_client
                                    .as_ref()
                                    .is_some_and(|previous| previous != &client_id)
                                {
                                    context_epoch += 1;
                                }
                                previous_client = Some(client_id.clone());
                                active = Some(client_id.clone());
                                connection.client = Some(client_id.clone());
                                session.resize(width, height);
                                if !acknowledged.contains_key(&client_id) {
                                    session.open_paths(&files);
                                }
                                connection.send(&ServerMessage::Attached {
                                    acknowledged: *acknowledged.entry(client_id).or_default(),
                                    context_epoch,
                                    replay_allowed: previous_epoch
                                        .is_none_or(|epoch| epoch == context_epoch),
                                    frame: session.frame(),
                                })?;
                            }
                        }
                        ClientMessage::Input {
                            sequence,
                            key,
                            context_epoch: input_epoch,
                        } if connection.client.is_some() => {
                            if input_epoch != context_epoch {
                                connection.send(&ServerMessage::Rejected {
                                    reason: "input context changed; reconnect before replaying queued input".into(),
                                })?;
                                connection.closing = true;
                                break;
                            }
                            let client = connection.client.as_ref().unwrap();
                            let watermark = acknowledged.get_mut(client).unwrap();
                            if sequence == *watermark + 1 {
                                session.input(&key);
                                *watermark = sequence;
                                session.flush_clipboard_writes(|text| {
                                    connection.send(&ServerMessage::ClipboardWrite { text })
                                });
                                session.flush_primary_clipboard_writes(|text| {
                                    connection.send(&ServerMessage::ClipboardWritePrimary { text })
                                });
                                if let Some((id, primary)) = session.clipboard_request() {
                                    connection
                                        .send(&ServerMessage::ClipboardRead { id, primary })?;
                                }
                            } else if sequence > *watermark + 1 {
                                connection.send(&ServerMessage::Rejected {
                                    reason: "input sequence gap; reconnect to retry unacknowledged input".into(),
                                })?;
                                connection.closing = true;
                                break;
                            }
                            connection.send(&ServerMessage::Update {
                                acknowledged: *watermark,
                                frame: session.frame(),
                            })?;
                        }
                        ClientMessage::Mouse {
                            sequence,
                            context_epoch: input_epoch,
                            mouse_context,
                            event,
                        } if connection.client.is_some() => {
                            if input_epoch != context_epoch {
                                connection.send(&ServerMessage::Rejected { reason: "input context changed; reconnect before sending mouse input".into() })?;
                                connection.closing = true;
                                break;
                            }
                            let watermark = acknowledged
                                .get_mut(connection.client.as_ref().unwrap())
                                .unwrap();
                            if sequence == *watermark + 1 {
                                session.mouse(mouse_context, event);
                                *watermark = sequence;
                            } else if sequence > *watermark + 1 {
                                connection.send(&ServerMessage::Rejected { reason: "input sequence gap; reconnect to retry unacknowledged input".into() })?;
                                connection.closing = true;
                                break;
                            }
                            connection.send(&ServerMessage::Update {
                                acknowledged: *watermark,
                                frame: session.frame(),
                            })?;
                        }
                        ClientMessage::Skip {
                            through,
                            context_epoch: input_epoch,
                        } if connection.client.is_some() && input_epoch == context_epoch => {
                            let watermark = acknowledged
                                .get_mut(connection.client.as_ref().unwrap())
                                .unwrap();
                            *watermark = (*watermark).max(through);
                            connection.send(&ServerMessage::Update {
                                acknowledged: *watermark,
                                frame: session.frame(),
                            })?;
                        }
                        ClientMessage::Resize { width, height } if connection.client.is_some() => {
                            session.resize(width, height);
                            let watermark = acknowledged[connection.client.as_ref().unwrap()];
                            connection.send(&ServerMessage::Update {
                                acknowledged: watermark,
                                frame: session.frame(),
                            })?;
                        }
                        ClientMessage::ClipboardResponse { id, text, error }
                            if connection.client.is_some() =>
                        {
                            session.complete_clipboard_read(id, text, error);
                            let watermark = acknowledged[connection.client.as_ref().unwrap()];
                            connection.send(&ServerMessage::Update {
                                acknowledged: watermark,
                                frame: session.frame(),
                            })?;
                        }
                        ClientMessage::Detach => {
                            disconnected = true;
                            break;
                        }
                        _ => {
                            disconnected = true;
                            break;
                        }
                    }
                    if session.exiting() || stopping {
                        break;
                    }
                }
            }
            if connection.flush().is_err() {
                disconnected = true;
            }
            if connection.eof {
                disconnected = true;
            }
            if connection.closing && connection.output.is_empty() {
                disconnected = true;
            }
            if connection.client.is_none() && connection.accepted.elapsed() > Duration::from_secs(5)
            {
                disconnected = true;
            }
            if disconnected {
                let connection = connections.remove(index);
                if connection.client.is_some() {
                    session.invalidate_mouse();
                    active = None;
                }
            } else {
                index += 1;
            }
            if stopping {
                break;
            }
        }
        if session.exiting() || stopping {
            for connection in &mut connections {
                let _ = connection.send(&ServerMessage::Exited);
                let _ = connection.flush();
            }
            break;
        }
        let changed = session.tick();
        let copied = session.flush_clipboard_writes(|text| {
            if let Some(connection) = connections
                .iter_mut()
                .find(|connection| connection.client.is_some())
            {
                connection.send(&ServerMessage::ClipboardWrite { text })?;
            }
            Ok(())
        });
        let primary_copied = session.flush_primary_clipboard_writes(|text| {
            if let Some(connection) = connections
                .iter_mut()
                .find(|connection| connection.client.is_some())
            {
                connection.send(&ServerMessage::ClipboardWritePrimary { text })?;
            }
            Ok(())
        });
        if changed || copied || primary_copied {
            let mut index = 0;
            while index < connections.len() {
                let connection = &mut connections[index];
                if let Some(client) = &connection.client {
                    let watermark = acknowledged[client];
                    connection.send(&ServerMessage::Update {
                        acknowledged: watermark,
                        frame: session.frame(),
                    })?;
                }
                if connection.flush().is_err() {
                    let connection = connections.remove(index);
                    if connection.client.is_some() {
                        session.invalidate_mouse();
                        active = None;
                    }
                } else {
                    index += 1;
                }
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}
