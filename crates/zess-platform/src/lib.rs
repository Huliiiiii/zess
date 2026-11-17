#[cfg(windows)]
mod console;
mod filesystem;
mod host;
mod process;
mod watch;

#[cfg(windows)]
pub use console::ConsoleState;
pub use filesystem::{DirectoryEntry, FileSystem, FileType, NativeFileSystem, WalkOptions};
pub use host::{HostInfo, NativePlatform, Platform, UserDirs};
pub use process::{
    Cancellation, CancellationToken, ConsoleMode, InputProgress, OutputMode, OwnedProcess,
    ProcessJob, ProcessRunner, ProcessSpec, SystemProcessRunner,
};
pub use watch::{NativeWatchBackend, WatchBackend, WatchEvent, WatchEventKind, WatchHandle};
