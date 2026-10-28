use anyhow::Result;
use zess_git::PatchRequest;

/// A single index mutation runs away from the session's input/render thread.
/// Once submitted it completes even if the preview is subsequently closed.
pub struct PatchTask {
    result: std::sync::mpsc::Receiver<Result<(), String>>,
}

impl PatchTask {
    pub fn poll(&self) -> Option<Result<(), String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("Git patch worker stopped".into()))
            }
        }
    }
}

impl crate::NativeServices {
    pub fn start_patch(&self, request: PatchRequest) -> Result<PatchTask> {
        let workspace = self.workspace.clone();
        let (sender, result) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("zess-git-patch".into())
            .spawn(move || {
                let outcome = zess_git::Repository::open(&workspace.root)
                    .and_then(|repository| {
                        repository.apply_patch(request, &crate::jobs::Cancellation::default())
                    })
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(outcome);
            })?;
        Ok(PatchTask { result })
    }
}
