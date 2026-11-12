use zess_protocol::{ServerInstanceId, SessionDelta, SessionId, SessionSnapshot};

/// Cached server state. All text and selections originate in snapshots or deltas.
#[derive(Clone)]
pub struct RemoteStateCache {
    pub session_id: SessionId,
    pub server_instance: ServerInstanceId,
    pub revision: u64,
    pub state: SessionSnapshot,
}

impl RemoteStateCache {
    pub fn apply_delta(
        &mut self,
        session_id: &SessionId,
        server_instance: &ServerInstanceId,
        base_revision: u64,
        revision: u64,
        delta: SessionDelta,
    ) -> bool {
        if session_id != &self.session_id
            || server_instance != &self.server_instance
            || base_revision != self.revision
            || revision <= base_revision
        {
            false
        } else {
            delta.apply(&mut self.state);
            self.revision = revision;
            true
        }
    }
}
