use anyhow::Result;
use zess_git::{ignore::IgnoreRequest, mutation::FileMutationRequest};

/// Submitted mutations finish once, independently of view and query lifetimes.
pub struct FileMutationTask {
    state: MutationState,
}

enum MutationState {
    Pending(std::sync::mpsc::Receiver<Result<(), String>>),
    Ready(Result<(), String>),
}

impl FileMutationTask {
    pub fn poll(&mut self) -> Option<&Result<(), String>> {
        if let MutationState::Pending(receiver) = &self.state {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("Git file worker stopped".into())
                }
            };
            self.state = MutationState::Ready(result);
        }
        match &self.state {
            MutationState::Ready(result) => Some(result),
            MutationState::Pending(_) => unreachable!(),
        }
    }
}

impl crate::NativeServices {
    pub fn start_ignore(&self, request: IgnoreRequest) -> Result<FileMutationTask> {
        let workspace = self.workspace.clone();
        let (sender, result) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("zess-git-ignore".into())
            .spawn(move || {
                let outcome = workspace
                    .repository()
                    .and_then(|repository| {
                        repository.ignore_paths(
                            request,
                            &crate::jobs::Cancellation::new(workspace.processes.clone()),
                        )
                    })
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(outcome);
            })?;
        Ok(FileMutationTask {
            state: MutationState::Pending(result),
        })
    }

    pub fn start_file_mutation(&self, request: FileMutationRequest) -> Result<FileMutationTask> {
        let workspace = self.workspace.clone();
        let (sender, result) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("zess-git-files".into())
            .spawn(move || {
                let outcome = workspace
                    .repository()
                    .and_then(|repository| {
                        repository.mutate_files(
                            request,
                            &crate::jobs::Cancellation::new(workspace.processes.clone()),
                        )
                    })
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(outcome);
            })?;
        Ok(FileMutationTask {
            state: MutationState::Pending(result),
        })
    }
}
