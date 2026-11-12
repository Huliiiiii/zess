use std::{collections::VecDeque, io::Write};

use anyhow::Result;
use zess_protocol::ServerInstanceId;

use crate::{
    ClientConnection, Clipboard, ClipboardRead, ClipboardReadJob, ClipboardWriteJob,
    clipboard_write::ClipboardWrites,
};

enum Action {
    Copy {
        primary: bool,
        text: String,
    },
    Read {
        instance: ServerInstanceId,
        id: u64,
        primary: bool,
    },
}

pub(super) struct ClipboardActions<R, W> {
    read: ClipboardRead<R>,
    writes: ClipboardWrites<W>,
    queue: VecDeque<Action>,
}

impl<R: ClipboardReadJob, W: ClipboardWriteJob> ClipboardActions<R, W> {
    pub(super) fn new() -> Self {
        Self {
            read: ClipboardRead::Idle,
            writes: ClipboardWrites::new(),
            queue: VecDeque::new(),
        }
    }

    pub(super) fn attach(&mut self, instance: &ServerInstanceId) {
        self.read.attach(instance);
        if self.writes.attach(instance) {
            self.queue.clear();
        }
    }

    pub(super) fn copy(&mut self, primary: bool, text: String) {
        self.queue.push_back(Action::Copy { primary, text });
    }

    pub(super) fn read(&mut self, instance: ServerInstanceId, id: u64, primary: bool) {
        self.queue.push_back(Action::Read {
            instance,
            id,
            primary,
        });
    }

    pub(super) fn poll(
        &mut self,
        provider: &mut impl Clipboard<ReadJob = R, WriteJob = W>,
        writer: &mut impl Write,
        connection: &mut ClientConnection,
    ) -> Result<Option<String>> {
        let mut notice = None;
        loop {
            self.read.poll(connection)?;
            if let Some(message) = self.writes.poll(provider, writer) {
                notice = Some(format!("Clipboard: {message}"));
            }
            if matches!(self.read, ClipboardRead::Reading { .. }) || !self.writes.is_idle() {
                return Ok(notice);
            }
            let Some(action) = self.queue.pop_front() else {
                return Ok(notice);
            };
            match action {
                Action::Copy { primary, text } => self.writes.copy(primary, text),
                Action::Read {
                    instance,
                    id,
                    primary,
                } => {
                    if !connection.attached() {
                        self.queue.push_front(Action::Read {
                            instance,
                            id,
                            primary,
                        });
                        return Ok(notice);
                    }
                    self.read.request(
                        &instance,
                        id,
                        primary,
                        connection.active_mut().unwrap(),
                        provider,
                    )?;
                }
            }
        }
    }
}
