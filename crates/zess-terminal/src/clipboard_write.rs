use std::{collections::VecDeque, io::Write};

use zess_protocol::ServerInstanceId;

use crate::{Clipboard, ClipboardWriteJob};

pub(super) struct ClipboardWrites<J> {
    server_instance: Option<ServerInstanceId>,
    queue: VecDeque<(bool, String)>,
    active: Option<J>,
}

impl<J: ClipboardWriteJob> ClipboardWrites<J> {
    pub(super) fn new() -> Self {
        Self {
            server_instance: None,
            queue: VecDeque::new(),
            active: None,
        }
    }

    pub(super) fn attach(&mut self, server_instance: &ServerInstanceId) -> bool {
        let replaced = self
            .server_instance
            .as_ref()
            .is_some_and(|previous| previous != server_instance);
        if replaced {
            self.queue.clear();
            self.active = None;
        }
        self.server_instance = Some(server_instance.clone());
        replaced
    }

    pub(super) fn is_idle(&self) -> bool {
        self.active.is_none() && self.queue.is_empty()
    }

    pub(super) fn copy(&mut self, primary: bool, text: String) {
        self.queue.push_back((primary, text));
    }

    pub(super) fn poll(
        &mut self,
        provider: &mut impl Clipboard<WriteJob = J>,
        writer: &mut impl Write,
    ) -> Option<String> {
        let mut notice = None;
        if let Some(active) = &mut self.active
            && let Some(result) = active.poll()
        {
            self.active = None;
            notice = result.err().map(|error| format!("{error:#}"));
        }
        if self.active.is_none()
            && let Some((primary, text)) = self.queue.pop_front()
        {
            match provider.write(primary, text, writer) {
                Ok(Some(job)) => self.active = Some(job),
                Ok(None) => {}
                Err(error) => notice = Some(format!("{error:#}")),
            }
        }
        notice
    }
}
