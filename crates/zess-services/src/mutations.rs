use anyhow::Result;
use zess_git::mutation::FileMutationRequest;

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
    pub fn start_file_mutation(&self, request: FileMutationRequest) -> Result<FileMutationTask> {
        let workspace = self.workspace.clone();
        let (sender, result) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("zess-git-files".into())
            .spawn(move || {
                let outcome = zess_git::Repository::open(&workspace.root)
                    .and_then(|repository| {
                        repository.mutate_files(request, &crate::jobs::Cancellation::default())
                    })
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(outcome);
            })?;
        Ok(FileMutationTask {
            state: MutationState::Pending(result),
        })
    }
}
