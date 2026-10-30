use anyhow::{Context, Result};
use std::time::Instant;
use zess_plugin::{ItemNavigation, PluginInput, SelectionChange};

use super::{Event, browser::Task};

#[derive(Default)]
pub(super) struct NavigationDebounce {
    quiet_until: Option<Instant>,
}

impl NavigationDebounce {
    pub(super) fn update(
        &mut self,
        event: Event,
        input: &PluginInput,
        now: Instant,
    ) -> Result<Option<Instant>> {
        let navigation = match event {
            Event::Select if input.argument.is_null() => Some(ItemNavigation::Step),
            Event::Select => {
                serde_json::from_value::<SelectionChange>(input.argument.clone())?.navigation
            }
            Event::Preview => Some(ItemNavigation::Jump),
            Event::Left | Event::Right | Event::FilterChanged | Event::MiniBufferChanged => {
                let deadline = now
                    .checked_add(input.preview_debounce)
                    .context("preview debounce exceeds the supported time range")?;
                self.quiet_until = Some(deadline);
                return Ok(Some(deadline));
            }
            _ => None,
        };
        match navigation {
            Some(ItemNavigation::Jump) => {
                self.quiet_until = None;
                Ok(Some(now))
            }
            Some(ItemNavigation::Step) => {
                let continuing = self.quiet_until.is_some_and(|deadline| deadline > now);
                let deadline = now
                    .checked_add(input.preview_debounce)
                    .context("preview debounce exceeds the supported time range")?;
                self.quiet_until = Some(deadline);
                Ok(Some(if continuing { deadline } else { now }))
            }
            None => Ok(None),
        }
    }

    pub(super) fn quiet_until(&self, now: Instant) -> Instant {
        self.quiet_until.unwrap_or(now).max(now)
    }
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
    pub(super) fn defer(&mut self, ready_at: Instant) {
        *self = match std::mem::take(self) {
            Self::Waiting { query, .. } | Self::Running { query, .. } => {
                Self::Waiting { query, ready_at }
            }
            idle @ Self::Idle { .. } => idle,
        };
    }

    pub(super) fn query(&self) -> Option<&Q> {
        match self {
            Self::Idle { query } => query.as_ref(),
            Self::Waiting { query, .. } | Self::Running { query, .. } => Some(query),
        }
    }
}
