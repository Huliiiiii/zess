use std::path::Path;

use anyhow::Result;

use crate::{NativeServices, Observation, QueryRequest, watcher::QueryKey};

impl NativeServices {
    /// Observes an editor file, including paths outside the workspace.
    pub fn observe_file(&mut self, path: &Path) -> Result<Observation> {
        anyhow::ensure!(
            path.is_absolute(),
            "file observation requires an absolute path"
        );
        Ok(Observation::new(
            self.acquire_key(QueryKey::OpenFile(path.to_owned()))?,
        ))
    }

    pub fn start_file_read(&mut self, path: &Path) -> Result<u64> {
        self.start_query(QueryRequest::ReadFile {
            path: path.to_owned(),
        })
    }
}
