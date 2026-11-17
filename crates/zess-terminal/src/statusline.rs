use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use zess_plugin::{StatusConfig, StatusContext, StatusLine};
use zess_protocol::ClientMessage;
use zess_ui::Span;

const LOADING_DELAY: Duration = Duration::from_millis(200);

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    Document(u64),
    Page(u64),
    Preview { subscription: u64, slot: String },
}

#[derive(Clone, PartialEq)]
struct Format {
    name: String,
    context: StatusContext,
}

struct Entry {
    wanted: Format,
    in_flight: Option<(u64, Format)>,
    completed: Option<Format>,
    last_line: Option<StatusLine>,
    loading_line: Option<StatusLine>,
    visible: bool,
    preview_id: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadingScope {
    View(u64),
    GitQuery { generation: u64, query: u64 },
    GitWrite { generation: u64, epoch: u64 },
}

impl Entry {
    fn line(&self) -> Option<&StatusLine> {
        self.loading_line.as_ref().or(self.last_line.as_ref())
    }
}

struct Loading {
    scope: LoadingScope,
    deadline: Instant,
    visible: bool,
}

#[derive(Default)]
pub(crate) struct Statuslines {
    next_id: u64,
    next_preview: u64,
    entries: HashMap<Target, Entry>,
    loading: HashMap<Target, Loading>,
    loading_deadline: Option<Instant>,
    frame_time: Option<Instant>,
}

impl Statuslines {
    pub(crate) fn invalidate(&mut self) {
        for entry in self.entries.values_mut() {
            entry.completed = None;
            entry.in_flight = None;
        }
    }

    pub(crate) fn begin_frame(&mut self, now: Instant) {
        for entry in self.entries.values_mut() {
            entry.visible = false;
        }
        for loading in self.loading.values_mut() {
            loading.visible = false;
        }
        self.loading_deadline = None;
        self.frame_time = Some(now);
    }

    pub(crate) fn show_loading(&mut self, target: &Target, pending: Option<LoadingScope>) -> bool {
        let Some(scope) = pending else {
            self.loading.remove(target);
            return false;
        };
        let now = self.frame_time.unwrap_or_else(Instant::now);
        let new = || Loading {
            scope,
            deadline: now + LOADING_DELAY,
            visible: true,
        };
        let loading = self.loading.entry(target.clone()).or_insert_with(new);
        if loading.scope != scope {
            *loading = new();
        }
        loading.visible = true;
        if now >= loading.deadline {
            return true;
        }
        self.loading_deadline = Some(
            self.loading_deadline
                .map_or(loading.deadline, |deadline| deadline.min(loading.deadline)),
        );
        false
    }

    pub(crate) fn loading_due(&self, now: Instant) -> bool {
        self.loading_deadline
            .is_some_and(|deadline| now >= deadline)
    }

    pub(crate) fn finish_frame(&mut self) {
        self.entries.retain(|_, entry| entry.visible);
        self.loading.retain(|_, loading| loading.visible);
    }

    pub(crate) fn render(
        &mut self,
        target: Target,
        config: Option<&StatusConfig>,
        name: &str,
        mut context: StatusContext,
        previous: Option<&StatusLine>,
    ) -> Vec<Span> {
        let (width, inset, active) = (context.width, context.right_inset, context.active);
        if let Some(line) = config.and_then(|config| config.format_local(name, &context)) {
            return line.render(width, inset, active);
        }
        if config.is_none() {
            return previous.map_or_else(Vec::new, |line| line.render(width, inset, active));
        }
        let preview = matches!(target, Target::Preview { .. });
        let entry = self.entries.entry(target).or_insert_with(|| {
            let preview_id = preview.then(|| {
                // Client-owned previews have no server Buffer or View. Allocate one identity
                // for both for the lifetime of this visible preview slot.
                self.next_preview += 1;
                u64::MAX - self.next_preview
            });
            Entry {
                wanted: Format {
                    name: name.into(),
                    context: context.clone(),
                },
                in_flight: None,
                completed: None,
                last_line: previous.cloned(),
                loading_line: None,
                visible: true,
                preview_id,
            }
        });
        if let Some(id) = entry.preview_id {
            context.view_id = id;
            context.buffer_id = id;
        }
        entry.visible = true;
        if !context.loading {
            entry.loading_line = None;
        }
        entry.wanted = Format {
            name: name.into(),
            context,
        };
        entry
            .line()
            .or(previous)
            .map_or_else(Vec::new, |line| line.render(width, inset, active))
    }

    pub(crate) fn requests(&mut self) -> Vec<ClientMessage> {
        let mut requests = Vec::new();
        for entry in self.entries.values_mut() {
            if entry.in_flight.is_some() || entry.completed.as_ref() == Some(&entry.wanted) {
                continue;
            }
            self.next_id += 1;
            let id = self.next_id;
            entry.in_flight = Some((id, entry.wanted.clone()));
            requests.push(ClientMessage::FormatStatusline {
                id,
                name: entry.wanted.name.clone(),
                context: entry.wanted.context.clone(),
            });
        }
        requests
    }

    pub(crate) fn receive(
        &mut self,
        id: u64,
        result: Result<StatusLine, String>,
    ) -> Result<bool, String> {
        let Some(entry) = self.entries.values_mut().find(|entry| {
            entry
                .in_flight
                .as_ref()
                .is_some_and(|(sent, _)| *sent == id)
        }) else {
            return Ok(false);
        };
        let (_, sent) = entry.in_flight.take().unwrap();
        if sent != entry.wanted {
            return Ok(false);
        }
        let loading = sent.context.loading;
        entry.completed = Some(sent);
        let line = result?;
        let changed = entry.line() != Some(&line);
        if loading {
            entry.loading_line = Some(line);
        } else {
            entry.last_line = Some(line);
        }
        Ok(changed)
    }
}
