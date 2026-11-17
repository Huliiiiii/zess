use std::{collections::VecDeque, io::Write};

use anyhow::Result;
use zess_protocol::ServerInstanceId;

use crate::{
    ClientConnection, Clipboard, ClipboardRead, ClipboardReadJob, ClipboardWriteJob,
    clipboard_write::ClipboardWrites,
};

enum Action {
    ReadForEditor {
        id: u64,
        primary: bool,
    },
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
    local_read: Option<(u64, R)>,
    local_result: Option<(u64, Result<String>)>,
    writes: ClipboardWrites<W>,
    queue: VecDeque<Action>,
}

impl<R: ClipboardReadJob, W: ClipboardWriteJob> ClipboardActions<R, W> {
    pub(super) fn new() -> Self {
        Self {
            read: ClipboardRead::Idle,
            local_read: None,
            local_result: None,
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

    pub(super) fn read_for_editor(&mut self, id: u64, primary: bool) {
        self.queue.push_back(Action::ReadForEditor { id, primary });
    }

    pub(super) fn take_local_result(&mut self) -> Option<(u64, Result<String>)> {
        self.local_result.take()
    }

    pub(super) fn is_idle(&self) -> bool {
        self.queue.is_empty()
            && self.writes.is_idle()
            && self.local_read.is_none()
            && !matches!(self.read, ClipboardRead::Reading { .. })
    }

    pub(super) fn poll(
        &mut self,
        provider: &mut impl Clipboard<ReadJob = R, WriteJob = W>,
        writer: &mut impl Write,
        connection: &mut ClientConnection,
    ) -> Result<Option<String>> {
        let mut notice = None;
        loop {
            if let Some((id, job)) = &mut self.local_read {
                if let Some(result) = job.poll() {
                    self.local_result = Some((*id, result));
                    self.local_read = None;
                }
                return Ok(notice);
            }
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
                Action::ReadForEditor { id, primary } => match provider.read(primary) {
                    Ok(job) => self.local_read = Some((id, job)),
                    Err(error) => {
                        self.local_result = Some((id, Err(error)));
                        return Ok(notice);
                    }
                },
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
