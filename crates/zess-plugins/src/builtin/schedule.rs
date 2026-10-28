use std::time::Instant;

use super::{Event, browser::Task};

pub(super) fn navigation(event: Event) -> bool {
    matches!(
        event,
        Event::Select
            | Event::Left
            | Event::Right
            | Event::FilterChanged
            | Event::MiniBufferChanged
    )
}

pub(super) enum QueryState<Q> {
    Idle { query: Option<Q> },
    Waiting { query: Q, ready_at: Instant },
    Running { query: Q, task: Task },
}

impl<Q> Default for QueryState<Q> {
    fn default() -> Self {
        Self::Idle { query: None }
    }
}

impl<Q> QueryState<Q> {
    pub(super) fn query(&self) -> Option<&Q> {
        match self {
            Self::Idle { query } => query.as_ref(),
            Self::Waiting { query, .. } | Self::Running { query, .. } => Some(query),
        }
    }
}
