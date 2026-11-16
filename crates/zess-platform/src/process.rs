use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{
    Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[cfg(test)]
mod tests;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub enum ConsoleMode {
    Hidden,
    Inherit,
}

pub enum OutputMode {
    Capture,
    Discard,
}

pub struct ProcessSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub console_mode: ConsoleMode,
    pub stdout: OutputMode,
    pub stderr: OutputMode,
    /// Maximum bytes captured from each output stream by `ProcessRunner::output`.
    pub output_limit: Option<usize>,
}

impl ProcessSpec {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            console_mode: ConsoleMode::Hidden,
            stdout: OutputMode::Capture,
            stderr: OutputMode::Capture,
            output_limit: None,
        }
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args(&mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> &mut Self {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    pub fn current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.cwd = Some(path.as_ref().to_owned());
        self
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }
}

pub trait Cancellation {
    fn check(&self) -> Result<()>;
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn check(&self) -> Result<()> {
        anyhow::ensure!(!self.0.load(Ordering::Acquire), "operation was cancelled");
        Ok(())
    }
}

impl Cancellation for CancellationToken {
    fn check(&self) -> Result<()> {
        self.check()
    }
}

pub trait ProcessRunner: Send + Sync {
    /// Returns an environment value inherited by this runner's commands.
    fn environment(&self, _name: &OsStr) -> Option<OsString> {
        None
    }

    fn output(
        &self,
        spec: &ProcessSpec,
        input: Option<&[u8]>,
        cancellation: &dyn Cancellation,
    ) -> Result<Output>;
    fn spawn(&self, spec: &ProcessSpec) -> Result<OwnedProcess>;
}

pub struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn environment(&self, name: &OsStr) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn output(
        &self,
        spec: &ProcessSpec,
        input: Option<&[u8]>,
        cancellation: &dyn Cancellation,
    ) -> Result<Output> {
        cancellation.check()?;
        let child = OwnedProcess::spawn(spec, input.is_some())?;
        collect_output(child, input, cancellation, spec.output_limit)
    }

    fn spawn(&self, spec: &ProcessSpec) -> Result<OwnedProcess> {
        OwnedProcess::spawn(spec, true)
    }
}

fn collect_output(
    child: OwnedProcess,
    input: Option<&[u8]>,
    cancellation: &dyn Cancellation,
    output_limit: Option<usize>,
) -> Result<Output> {
    thread::scope(|scope| {
        // The process guard must drop before the scope waits for pipe workers.
        let mut child = child;
        let mut stdin = if let Some(input) = input {
            let mut stdin = child.take_stdin().context("missing process stdin")?;
            Some(
                thread::Builder::new()
                    .name("zess-process-stdin".into())
                    .spawn_scoped(scope, move || stdin.write_all(input))?,
            )
        } else {
            drop(child.take_stdin());
            None
        };
        let mut stdout = child
            .take_stdout()
            .map(|stdout| {
                thread::Builder::new()
                    .name("zess-process-stdout".into())
                    .spawn_scoped(scope, move || read_pipe(stdout, output_limit))
            })
            .transpose()?;
        let mut stderr = child
            .take_stderr()
            .map(|stderr| {
                thread::Builder::new()
                    .name("zess-process-stderr".into())
                    .spawn_scoped(scope, move || read_pipe(stderr, output_limit))
            })
            .transpose()?;
        let mut stdout_bytes = stdout.is_none().then(Vec::new);
        let mut stderr_bytes = stderr.is_none().then(Vec::new);
        let mut exited = None;
        let status = loop {
            cancellation.check()?;
            if stdout.as_ref().is_some_and(|reader| reader.is_finished()) {
                stdout_bytes = Some(
                    stdout
                        .take()
                        .unwrap()
                        .join()
                        .map_err(|_| anyhow::anyhow!("process stdout reader stopped"))??,
                );
            }
            if stderr.as_ref().is_some_and(|reader| reader.is_finished()) {
                stderr_bytes = Some(
                    stderr
                        .take()
                        .unwrap()
                        .join()
                        .map_err(|_| anyhow::anyhow!("process stderr reader stopped"))??,
                );
            }
            if stdin.as_ref().is_some_and(|writer| writer.is_finished())
                && let Err(error) = stdin
                    .take()
                    .unwrap()
                    .join()
                    .map_err(|_| anyhow::anyhow!("process stdin writer stopped"))?
                && error.kind() != io::ErrorKind::BrokenPipe
            {
                return Err(error).context("write process input");
            }
            if exited.is_none()
                && let Some(status) = child.try_wait()?
            {
                // Descendants can keep the pipes open after the command exits.
                child.finish()?;
                exited = Some(status);
            }
            if let Some(status) = exited
                && stdout_bytes.is_some()
                && stderr_bytes.is_some()
                && stdin.is_none()
            {
                break status;
            }
            thread::sleep(Duration::from_millis(10));
        };
        cancellation.check()?;
        Ok(Output {
            status,
            stdout: stdout_bytes.unwrap(),
            stderr: stderr_bytes.unwrap(),
        })
    })
}

fn read_pipe(mut pipe: impl Read, limit: Option<usize>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 16 * 1024];
    loop {
        let count = pipe.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        anyhow::ensure!(
            limit.is_none_or(|limit| count <= limit.saturating_sub(bytes.len())),
            "process output exceeded its byte limit"
        );
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes)
}

struct JobCancellation {
    cancelled: CancellationToken,
    deadline: Instant,
}

impl Cancellation for JobCancellation {
    fn check(&self) -> Result<()> {
        self.cancelled.check()?;
        anyhow::ensure!(Instant::now() < self.deadline, "process job timed out");
        Ok(())
    }
}

pub struct ProcessJob {
    cancelled: CancellationToken,
    result: mpsc::Receiver<Result<Output>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ProcessJob {
    pub fn start(
        runner: Arc<dyn ProcessRunner>,
        spec: ProcessSpec,
        input: Option<Vec<u8>>,
        timeout: Duration,
        output_limit: usize,
    ) -> Result<Self> {
        let cancelled = CancellationToken::new();
        let cancellation = JobCancellation {
            cancelled: cancelled.clone(),
            deadline: Instant::now() + timeout,
        };
        cancellation.check()?;
        let child = runner.spawn(&spec)?;
        let (sender, result) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("zess-process-job".into())
            .spawn(move || {
                let output =
                    collect_output(child, input.as_deref(), &cancellation, Some(output_limit));
                let _ = sender.send(output);
            })?;
        Ok(Self {
            cancelled,
            result,
            worker: Some(worker),
        })
    }

    pub fn poll(&mut self) -> Option<Result<Output>> {
        match self.result.try_recv() {
            Ok(output) => {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
                Some(output)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) if self.worker.is_some() => {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
                Some(Err(anyhow::anyhow!(
                    "process worker stopped without a result"
                )))
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }
}

impl Drop for ProcessJob {
    fn drop(&mut self) {
        self.cancelled.cancel();
        // The worker owns the process guard and reaps it after observing cancellation.
        // UI teardown must not wait for pipe workers or OS process cleanup.
    }
}

enum ProcessStage {
    Running,
    Draining(ExitStatus),
    Finished,
}

/// Owns a process tree. Dropping it terminates remaining children and reaps its process.
pub struct OwnedProcess {
    child: Child,
    stage: ProcessStage,
    #[cfg(unix)]
    console: Option<unix::ConsoleGuard>,
    #[cfg(windows)]
    console: Option<crate::ConsoleState>,
    #[cfg(windows)]
    tree: windows::ProcessTree,
}

impl OwnedProcess {
    fn spawn(spec: &ProcessSpec, pipe_input: bool) -> Result<Self> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .envs(spec.env.iter().map(|(key, value)| (key, value)));
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(if pipe_input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(match spec.stdout {
                OutputMode::Capture => Stdio::piped(),
                OutputMode::Discard => Stdio::null(),
            })
            .stderr(match spec.stderr {
                OutputMode::Capture => Stdio::piped(),
                OutputMode::Discard => Stdio::null(),
            });
        #[cfg(unix)]
        let console = {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
            if matches!(spec.console_mode, ConsoleMode::Inherit) {
                let console = unix::ConsoleGuard::prepare()?;
                if let Some(console) = &console {
                    console.configure(&mut command);
                }
                console
            } else {
                None
            }
        };
        #[cfg(windows)]
        let console = if matches!(spec.console_mode, ConsoleMode::Inherit) {
            crate::ConsoleState::capture()?
        } else {
            None
        };
        #[cfg(windows)]
        let tree = {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
            let console_flags = match spec.console_mode {
                ConsoleMode::Hidden => CREATE_NO_WINDOW,
                ConsoleMode::Inherit => 0,
            };
            command.creation_flags(CREATE_SUSPENDED | console_flags);
            windows::ProcessTree::new()?
        };
        let child = command
            .spawn()
            .with_context(|| format!("cannot start {}", spec.program.to_string_lossy()))?;
        let process = Self {
            child,
            stage: ProcessStage::Running,
            console,
            #[cfg(windows)]
            tree,
        };
        #[cfg(windows)]
        process.tree.attach_and_resume(&process.child)?;
        Ok(process)
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Returns the controlling terminal to the caller after interactive authentication.
    pub fn release_console(&mut self) -> io::Result<()> {
        if let Some(console) = &mut self.console {
            #[cfg(unix)]
            console.release()?;
            #[cfg(windows)]
            console.restore()?;
            self.console = None;
        }
        Ok(())
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.stage {
            ProcessStage::Running => {
                let status = self.child.try_wait()?;
                if let Some(status) = status {
                    self.stage = ProcessStage::Draining(status);
                }
                Ok(status)
            }
            ProcessStage::Draining(status) => Ok(Some(status)),
            ProcessStage::Finished => self.child.try_wait(),
        }
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.stage = ProcessStage::Draining(status);
        Ok(status)
    }

    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            // A reaped parent may still have descendants holding its output pipes.
            if unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        #[cfg(windows)]
        self.tree.terminate()?;
        if matches!(self.stage, ProcessStage::Running) {
            self.child.kill()?;
        }
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        self.release_console()?;
        self.kill()?;
        self.stage = ProcessStage::Finished;
        Ok(())
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if !matches!(self.stage, ProcessStage::Finished) {
            let _ = self.kill();
            if matches!(self.stage, ProcessStage::Running) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        let _ = self.release_console();
    }
}
