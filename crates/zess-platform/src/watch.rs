use std::path::{Path, PathBuf};

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub enum WatchEventKind {
    Access,
    Create,
    Modify,
    Rename,
    Remove,
    Other,
}

pub struct WatchEvent {
    pub paths: Vec<PathBuf>,
    pub kind: WatchEventKind,
    pub rescan: bool,
}

impl From<Event> for WatchEvent {
    fn from(event: Event) -> Self {
        let rescan = event.need_rescan();
        let kind = match event.kind {
            EventKind::Access(_) => WatchEventKind::Access,
            EventKind::Create(_) => WatchEventKind::Create,
            EventKind::Modify(notify::event::ModifyKind::Name(_)) => WatchEventKind::Rename,
            EventKind::Modify(_) => WatchEventKind::Modify,
            EventKind::Remove(_) => WatchEventKind::Remove,
            EventKind::Any | EventKind::Other => WatchEventKind::Other,
        };
        Self {
            paths: event.paths,
            kind,
            rescan,
        }
    }
}

/// Dropping the handle releases the underlying watch.
pub trait WatchHandle: Send {
    fn refresh(&mut self) -> Result<()>;
}

pub trait WatchBackend: Send + Sync {
    fn watch(
        &self,
        path: &Path,
        recursive: bool,
        callback: Box<dyn Fn(Result<WatchEvent>) + Send>,
    ) -> Result<Box<dyn WatchHandle>>;
}

pub struct NativeWatchBackend;

impl WatchBackend for NativeWatchBackend {
    fn watch(
        &self,
        path: &Path,
        recursive: bool,
        callback: Box<dyn Fn(Result<WatchEvent>) + Send>,
    ) -> Result<Box<dyn WatchHandle>> {
        let mut handle = NativeWatchHandle {
            watcher: notify::recommended_watcher(move |event: notify::Result<Event>| {
                callback(event.map(WatchEvent::from).map_err(Into::into));
            })?,
            path: path.to_owned(),
            mode: if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            },
        };
        handle.refresh()?;
        Ok(Box::new(handle))
    }
}

struct NativeWatchHandle {
    watcher: RecommendedWatcher,
    path: PathBuf,
    mode: RecursiveMode,
}

impl WatchHandle for NativeWatchHandle {
    fn refresh(&mut self) -> Result<()> {
        self.watcher.watch(&self.path, self.mode)?;
        Ok(())
    }
}
