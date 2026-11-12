use std::{
    io::{self, Write},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{clipboard::CopyToClipboard, execute};
use zess_platform::{ProcessJob, ProcessRunner, ProcessSpec, SystemProcessRunner};
use zess_protocol::MAX_INPUT_BYTES;

#[cfg(windows)]
use crate::CLIPBOARD_HELPER_ARG;

pub trait ClipboardReadJob {
    fn poll(&mut self) -> Option<Result<String>>;
}

pub trait ClipboardWriteJob {
    fn poll(&mut self) -> Option<Result<()>>;
}

pub trait Clipboard {
    type ReadJob: ClipboardReadJob;
    type WriteJob: ClipboardWriteJob;

    fn read(&mut self, primary: bool) -> Result<Self::ReadJob>;
    fn write(
        &mut self,
        primary: bool,
        text: String,
        writer: &mut impl Write,
    ) -> Result<Option<Self::WriteJob>>;
}

/// Writes OSC 52 through the terminal's existing output writer.
#[derive(Default)]
pub struct Osc52Clipboard;

impl Osc52Clipboard {
    pub fn write(&mut self, primary: bool, text: &str, writer: &mut impl Write) -> Result<()> {
        #[cfg(windows)]
        if primary {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows has no PRIMARY selection",
            )
            .into());
        }
        let command = if primary {
            CopyToClipboard::to_primary_from(text)
        } else {
            CopyToClipboard::to_clipboard_from(text)
        };
        execute!(writer, command).context("write clipboard to terminal")
    }
}

pub enum NoClipboardJob {}

impl ClipboardReadJob for NoClipboardJob {
    fn poll(&mut self) -> Option<Result<String>> {
        match *self {}
    }
}

impl ClipboardWriteJob for NoClipboardJob {
    fn poll(&mut self) -> Option<Result<()>> {
        match *self {}
    }
}

impl Clipboard for Osc52Clipboard {
    type ReadJob = NoClipboardJob;
    type WriteJob = NoClipboardJob;

    fn read(&mut self, _primary: bool) -> Result<Self::ReadJob> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OSC 52 clipboard reads are unavailable",
        )
        .into())
    }

    fn write(
        &mut self,
        primary: bool,
        text: String,
        writer: &mut impl Write,
    ) -> Result<Option<Self::WriteJob>> {
        Osc52Clipboard::write(self, primary, &text, writer)?;
        Ok(None)
    }
}

pub struct NativeClipboard {
    runner: Arc<dyn ProcessRunner>,
}

impl Default for NativeClipboard {
    fn default() -> Self {
        Self::new(Arc::new(SystemProcessRunner))
    }
}

impl NativeClipboard {
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

#[derive(Default)]
pub struct TerminalClipboard {
    native: NativeClipboard,
    #[cfg(not(windows))]
    osc52: Osc52Clipboard,
}

impl Clipboard for TerminalClipboard {
    type ReadJob = ClipboardJob;
    type WriteJob = ClipboardWrite;

    fn read(&mut self, primary: bool) -> Result<Self::ReadJob> {
        self.native.read(primary)
    }

    fn write(
        &mut self,
        primary: bool,
        text: String,
        writer: &mut impl Write,
    ) -> Result<Option<Self::WriteJob>> {
        #[cfg(windows)]
        {
            self.native.write(primary, text, writer)
        }
        #[cfg(not(windows))]
        {
            self.osc52.write(primary, &text, writer)?;
            Ok(None)
        }
    }
}

pub struct ClipboardJob {
    process: Option<ProcessJob>,
    pub(crate) deadline: Instant,
}

impl ClipboardJob {
    fn start(
        runner: Arc<dyn ProcessRunner>,
        spec: ProcessSpec,
        input: Option<Vec<u8>>,
    ) -> Result<Self> {
        Self::start_with_limit(runner, spec, input, MAX_INPUT_BYTES)
    }

    fn start_with_limit(
        runner: Arc<dyn ProcessRunner>,
        spec: ProcessSpec,
        input: Option<Vec<u8>>,
        output_limit: usize,
    ) -> Result<Self> {
        Ok(Self {
            process: Some(ProcessJob::start(
                runner,
                spec,
                input,
                Duration::from_secs(3),
                output_limit,
            )?),
            deadline: Instant::now() + Duration::from_secs(3),
        })
    }

    #[cfg(all(test, unix))]
    pub(crate) fn spawn(program: &str, args: &[&str]) -> Result<Self> {
        let mut spec = ProcessSpec::new(program);
        spec.args(args);
        Self::start(Arc::new(SystemProcessRunner), spec, None)
    }

    fn output(&mut self) -> Option<Result<std::process::Output>> {
        if Instant::now() >= self.deadline {
            self.process.take();
            return Some(Err(anyhow::anyhow!("clipboard provider timed out")));
        }
        self.process.as_mut()?.poll().map(|result| {
            let output = result.context("clipboard provider failed")?;
            anyhow::ensure!(
                output.status.success(),
                "clipboard provider failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            Ok(output)
        })
    }
}

impl ClipboardReadJob for ClipboardJob {
    fn poll(&mut self) -> Option<Result<String>> {
        self.output()
            .map(|result| String::from_utf8(result?.stdout).context("clipboard is not UTF-8"))
    }
}

pub struct ClipboardWrite(ClipboardJob);

impl ClipboardWriteJob for ClipboardWrite {
    fn poll(&mut self) -> Option<Result<()>> {
        self.0.output().map(|result| result.map(|_| ()))
    }
}

impl Clipboard for NativeClipboard {
    type ReadJob = ClipboardJob;
    type WriteJob = ClipboardWrite;

    fn read(&mut self, primary: bool) -> Result<Self::ReadJob> {
        #[cfg(windows)]
        {
            if primary {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Windows has no PRIMARY selection",
                )
                .into());
            }
            let mut spec =
                ProcessSpec::new(std::env::current_exe().context("locate clipboard helper")?);
            spec.args([CLIPBOARD_HELPER_ARG, "read"]);
            ClipboardJob::start(self.runner.clone(), spec, None)
        }
        #[cfg(not(windows))]
        {
            let candidates = read_candidates(primary);
            for (program, args) in candidates {
                let mut spec = ProcessSpec::new(program);
                spec.args(args);
                match ClipboardJob::start(self.runner.clone(), spec, None) {
                    Ok(job) => return Ok(job),
                    Err(error)
                        if error
                            .downcast_ref::<io::Error>()
                            .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            anyhow::bail!("no local clipboard provider found (install wl-paste, xclip, or xsel)")
        }
    }

    fn write(
        &mut self,
        primary: bool,
        text: String,
        writer: &mut impl Write,
    ) -> Result<Option<Self::WriteJob>> {
        #[cfg(windows)]
        {
            let _ = writer;
            if primary {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Windows has no PRIMARY selection",
                )
                .into());
            }
            let mut spec =
                ProcessSpec::new(std::env::current_exe().context("locate clipboard helper")?);
            spec.args([CLIPBOARD_HELPER_ARG, "write"]);
            Ok(Some(ClipboardWrite(ClipboardJob::start(
                self.runner.clone(),
                spec,
                Some(text.into_bytes()),
            )?)))
        }
        #[cfg(not(windows))]
        {
            Osc52Clipboard.write(primary, &text, writer)?;
            Ok(None)
        }
    }
}

#[cfg(not(windows))]
fn read_candidates(primary: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut candidates = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        candidates.push((
            "wl-paste",
            if primary {
                vec!["-p", "--no-newline"]
            } else {
                vec!["--no-newline"]
            },
        ));
    }
    if std::env::var_os("DISPLAY").is_some() {
        candidates.push((
            "xclip",
            vec![
                "-o",
                "-selection",
                if primary { "primary" } else { "clipboard" },
            ],
        ));
        candidates.push((
            "xsel",
            if primary {
                vec!["-o"]
            } else {
                vec!["-o", "-b"]
            },
        ));
    }
    if cfg!(target_os = "macos") && !primary {
        candidates.push(("pbpaste", vec![]));
    }
    candidates
}
