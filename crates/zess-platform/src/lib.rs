mod filesystem;
mod host;
mod process;
mod watch;

pub use filesystem::{DirectoryEntry, FileSystem, FileType, NativeFileSystem, WalkOptions};
pub use host::{HostInfo, NativePlatform, Platform, UserDirs};
pub use process::{
    Cancellation, CancellationToken, ConsoleMode, OutputMode, OwnedProcess, ProcessJob,
    ProcessRunner, ProcessSpec, SystemProcessRunner,
};
pub use watch::{NativeWatchBackend, WatchBackend, WatchEvent, WatchEventKind, WatchHandle};

/// Encode a native UTF-8 path with slash separators, preserving its components.
pub fn slash_separated_path(path: &str) -> String {
    path.replace(std::path::MAIN_SEPARATOR, "/")
}
