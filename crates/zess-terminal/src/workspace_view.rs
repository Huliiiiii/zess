use anyhow::Result;
use zess_protocol::{ClientMessage, workspace_view::WorkspaceView};

use crate::Connection;

#[derive(Default)]
pub(crate) enum WorkspaceViewSync {
    #[default]
    Unsaved,
    Saved(WorkspaceView),
    Saving(WorkspaceView),
}

impl WorkspaceViewSync {
    pub(crate) fn attach(&mut self, saved: Option<WorkspaceView>) {
        *self = saved.map_or(Self::Unsaved, Self::Saved);
    }

    pub(crate) fn is_saved(&self, current: &WorkspaceView) -> bool {
        matches!(self, Self::Saved(saved) if saved == current)
    }

    /// Returns true when the Session has confirmed the current positions.
    pub(crate) fn synchronize(
        &mut self,
        current: WorkspaceView,
        connection: &mut Connection,
    ) -> Result<bool> {
        if self.is_saved(&current) {
            return Ok(true);
        }
        if matches!(self, Self::Saving(_)) {
            return Ok(false);
        }
        connection.send(&ClientMessage::SaveWorkspaceView {
            view: current.clone(),
        })?;
        *self = Self::Saving(current);
        Ok(false)
    }

    pub(crate) fn acknowledge(&mut self) -> Result<()> {
        let Self::Saving(view) = self else {
            anyhow::bail!("unexpected workspace view confirmation");
        };
        *self = Self::Saved(std::mem::take(view));
        Ok(())
    }
}
