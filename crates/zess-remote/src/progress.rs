#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteStage {
    DetectingPlatform,
    CheckingInstalledServer,
    CheckingCache,
    Downloading,
    Uploading,
    Installing,
    Connecting,
    Ready,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteProgress {
    Stage(RemoteStage),
    Transfer {
        stage: RemoteStage,
        completed: u64,
        total: Option<u64>,
    },
    Diagnostic(String),
}

/// Receives connection events from I/O threads; implementations must return promptly.
pub trait ProgressReporter: Send + Sync {
    fn report(&self, event: RemoteProgress);
}
