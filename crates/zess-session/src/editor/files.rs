use super::*;
use zess_services::{QueryPoll, QueryResult};

pub(super) struct FileWatch {
    path: PathBuf,
    observation: Observation,
    reload: Reload,
}

enum Reload {
    Idle,
    Pending,
    Reading { handle: u64, version: u64 },
}

impl FileWatch {
    fn new(path: &Path, services: &mut NativeServices) -> Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            observation: services.observe_file(path)?,
            reload: Reload::Idle,
        })
    }

    fn cancel(self, services: &mut NativeServices) {
        if let Reload::Reading { handle, .. } = self.reload {
            services.cancel_query(handle);
        }
    }

    fn poll(&mut self, text: &mut TextBuffer, services: &mut NativeServices) -> Result<bool> {
        if self.observation.changed() {
            if let Reload::Reading { handle, .. } = self.reload {
                services.cancel_query(handle);
            }
            self.observation.acknowledge();
            self.reload = Reload::Pending;
        }

        match self.reload {
            Reload::Idle => Ok(false),
            Reload::Pending => {
                if !text.modified() {
                    self.reload = Reload::Reading {
                        handle: services.start_file_read(&self.path)?,
                        version: text.version(),
                    };
                }
                Ok(false)
            }
            Reload::Reading { handle, version } => {
                if text.modified() || text.version() != version {
                    services.cancel_query(handle);
                    self.reload = Reload::Pending;
                    return Ok(false);
                }
                let result = match services.poll_query(handle) {
                    Ok(result) => result,
                    Err(error) => {
                        self.reload = Reload::Idle;
                        return Err(error);
                    }
                };
                match result {
                    QueryPoll::Pending => Ok(false),
                    QueryPoll::Ready(result) => {
                        self.reload = Reload::Idle;
                        match result {
                            Ok(QueryResult::FileRead {
                                target,
                                text: contents,
                            }) => {
                                let before = text.version();
                                text.reload_contents(target, &contents);
                                Ok(before != text.version())
                            }
                            Ok(_) => bail!("file read returned an unexpected result"),
                            Err(error) => bail!(error),
                        }
                    }
                }
            }
        }
    }
}

impl Session {
    /// Bind before reading, so changes during the initial read remain observable.
    pub(super) fn open_file_buffer(&mut self, path: &Path) -> Result<BufferId> {
        let watch = FileWatch::new(path, &mut self.services.borrow_mut())?;
        let text = TextBuffer::open_with_capabilities(
            path,
            self.services.borrow().file_system(),
            self.syntax_provider.clone(),
        )?;
        let buffer = self.add_buffer(Buffer::Text(text));
        self.file_watches.insert(buffer, watch);
        Ok(buffer)
    }

    pub(super) fn rebind_file_watch(&mut self, buffer: BufferId) -> Result<()> {
        let Some(Buffer::Text(text)) = self.buffers.get(&buffer) else {
            return Ok(());
        };
        let Some(path) = text.path() else {
            return Ok(());
        };
        let watch = FileWatch::new(path, &mut self.services.borrow_mut());
        self.remove_file_watch(buffer);
        let mut watch = watch?;
        // Re-read after binding to cover writes between saving and registration.
        watch.reload = Reload::Pending;
        self.file_watches.insert(buffer, watch);
        Ok(())
    }

    pub(super) fn remove_file_watch(&mut self, buffer: BufferId) {
        if let Some(watch) = self.file_watches.remove(&buffer) {
            watch.cancel(&mut self.services.borrow_mut());
        }
    }

    pub(super) fn poll_file_changes(&mut self) -> bool {
        let mut changed = false;
        let mut services = self.services.borrow_mut();
        for (&id, watch) in &mut self.file_watches {
            let Some(Buffer::Text(text)) = self.buffers.get_mut(&id) else {
                continue;
            };
            match watch.poll(text, &mut services) {
                Ok(updated) => changed |= updated,
                Err(error) => {
                    self.message = format!("file reload: {error:#}");
                    changed = true;
                }
            }
        }
        changed
    }
}
