use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;

const BOMBADIL_VERSION: &str = "0.7.10";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    server: ChildGuard,
    root: tempfile::TempDir,
    workspace: PathBuf,
    socket: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("zess-bombadil-")
            .tempdir()?;
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace)?;
        fs::write(workspace.join("origin.txt"), "abcdef\n")?;
        fs::write(
            workspace.join("diff.txt"),
            (0..40)
                .map(|line| format!("BASELINE_ALPHA_{line:02}\n"))
                .collect::<String>(),
        )?;
        for arguments in [
            &["init", "--quiet"][..],
            &["config", "user.name", "Terminal fixture"],
            &["config", "user.email", "terminal@example.invalid"],
            &["config", "commit.gpgsign", "false"],
            &["config", "core.hooksPath", "/dev/null"],
            &["config", "diff.external", ""],
            &["add", "."],
            &["commit", "--quiet", "-m", "Terminal fixture baseline"],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(&workspace)
                .args(arguments)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
                .output()?;
            ensure!(
                output.status.success(),
                "git {arguments:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::write(
            workspace.join("diff.txt"),
            (0..40)
                .map(|line| format!("WORKTREE_BETA_{line:02}\n"))
                .collect::<String>(),
        )?;
        let config = root.path().join("init.w");
        fs::write(&config, "")?;
        let socket = root.path().join("session.sock");
        let server = Command::new(env!("CARGO_BIN_EXE_zess"))
            .arg("server")
            .arg("--workspace")
            .arg(&workspace)
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(config)
            .arg("origin.txt")
            .current_dir(root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_PAGER", "cat")
            .stdout(Stdio::null())
            .stderr(File::create(root.path().join("server.log"))?)
            .spawn()
            .map(ChildGuard)?;
        let mut fixture = Self {
            root,
            workspace,
            socket,
            server,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&fixture.socket).is_err() {
            ensure!(
                fixture.server.0.try_wait()?.is_none(),
                "server exited during startup: {}",
                fs::read_to_string(fixture.root.path().join("server.log"))?
            );
            ensure!(Instant::now() < deadline, "server startup timed out");
            thread::sleep(Duration::from_millis(10));
        }
        Ok(fixture)
    }
}

#[derive(Deserialize)]
struct TraceEntry {
    #[serde(default)]
    action: Option<Value>,
    #[serde(default)]
    snapshots: Vec<Snapshot>,
}

#[derive(Deserialize)]
struct Snapshot {
    name: Option<String>,
    value: Value,
}

#[derive(Deserialize)]
struct Journey {
    step: String,
    completed: bool,
    checkpoints: Vec<String>,
    exited: bool,
    #[serde(rename = "exitCode")]
    exit_code: Option<i32>,
}

fn read_trace(directory: &Path) -> Result<(Journey, Vec<Value>)> {
    let mut journey = None;
    let mut actions = Vec::new();
    for line in BufReader::new(File::open(directory.join("trace.jsonl"))?).lines() {
        let entry: TraceEntry = serde_json::from_str(&line?)?;
        actions.extend(entry.action);
        for snapshot in entry.snapshots {
            if snapshot.name.as_deref() == Some("journey") {
                journey = Some(serde_json::from_value(snapshot.value)?);
            }
        }
    }
    Ok((
        journey.context("trace contains no journey snapshots")?,
        actions,
    ))
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        ensure!(Instant::now() < deadline, "process exceeded {timeout:?}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn explore(case: &str, checkpoints: &[&str], expected_files: &[(&str, &[u8])]) -> Result<()> {
    let bombadil =
        PathBuf::from(std::env::var_os("BOMBADIL_BIN").unwrap_or_else(|| "bombadil".into()));
    let bombadil = if bombadil.is_relative() && bombadil.components().count() > 1 {
        std::env::current_dir()?.join(bombadil)
    } else {
        bombadil
    };
    let version = Command::new(&bombadil)
        .arg("--version")
        .output()
        .context("run Bombadil 0.7.10; put it on PATH or set BOMBADIL_BIN")?;
    ensure!(version.status.success(), "Bombadil --version failed");
    ensure!(
        String::from_utf8(version.stdout)?.trim() == format!("bombadil {BOMBADIL_VERSION}"),
        "these terminal specifications require Bombadil {BOMBADIL_VERSION}"
    );
    let output_root = std::env::var_os("BOMBADIL_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bombadil"));
    fs::create_dir_all(&output_root)?;
    let output_root = output_root.canonicalize()?;
    let output = tempfile::Builder::new()
        .prefix(&format!("{case}-"))
        .tempdir_in(&output_root)?
        .keep();
    eprintln!("Bombadil {case}: {}", output.display());
    let mut fixture = Fixture::new()?;
    let result = (|| {
        let mut command = Command::new(&bombadil);
        command
            .args(["terminal", "test", "--specification"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/bombadil/{case}.ts")))
            .args([
                "--time-limit",
                "40s",
                "--quiescence-timeout-ms",
                "50",
                "--exit-on-violation",
                "--output-path",
            ])
            .arg(&output);
        let reproduce = std::env::var_os("BOMBADIL_REPRODUCE")
            .map(|directory| PathBuf::from(directory).canonicalize())
            .transpose()
            .context("open the Bombadil trace to reproduce")?;
        if let Some(directory) = &reproduce {
            command.arg("--reproduce").arg(directory);
        }
        let mut child = command
            .arg("--")
            .arg(env!("CARGO_BIN_EXE_zess"))
            .arg("attach")
            .arg("--workspace")
            .arg(&fixture.workspace)
            .arg("--socket")
            .arg(&fixture.socket)
            .current_dir(&fixture.workspace)
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .env("XDG_CONFIG_HOME", fixture.root.path())
            .env("XDG_RUNTIME_DIR", fixture.root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_PAGER", "cat")
            .stdout(File::create(output.join("bombadil.stdout"))?)
            .stderr(File::create(output.join("bombadil.stderr"))?)
            .spawn()
            .map(ChildGuard)?;
        let status =
            wait_for_exit(&mut child.0, Duration::from_secs(50)).context("wait for Bombadil")?;
        ensure!(
            status.success(),
            "Bombadil failed: {status}: {}",
            fs::read_to_string(output.join("bombadil.stderr"))?
        );
        let (journey, actions) = read_trace(&output)?;
        ensure!(journey.completed, "incomplete journey at {}", journey.step);
        ensure!(
            journey.exited && journey.exit_code == Some(0),
            "terminal did not exit normally"
        );
        ensure!(!actions.is_empty(), "trace contains no actions");
        for checkpoint in checkpoints {
            ensure!(
                journey
                    .checkpoints
                    .iter()
                    .any(|observed| observed == checkpoint),
                "missing checkpoint {checkpoint}"
            );
        }
        ensure!(
            wait_for_exit(&mut fixture.server.0, Duration::from_secs(2))?.success(),
            "server did not exit successfully after closing its final view"
        );
        for (name, expected) in expected_files {
            let actual = fs::read(fixture.workspace.join(name))?;
            ensure!(actual == *expected, "saved {name} differs: {actual:?}");
        }
        if let Some(directory) = reproduce {
            let (original, expected_actions) = read_trace(&directory)?;
            ensure!(
                actions == expected_actions,
                "replayed action sequence differs"
            );
            ensure!(
                journey.checkpoints == original.checkpoints,
                "replayed checkpoints differ"
            );
        }
        Ok(())
    })();
    for (name, expected) in expected_files {
        fs::write(output.join(format!("{name}.expected")), expected)?;
        match fs::read(fixture.workspace.join(name)) {
            Ok(actual) => fs::write(output.join(name), actual)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    fs::copy(
        fixture.root.path().join("server.log"),
        output.join("server.log"),
    )?;
    result.with_context(|| format!("artifacts: {}", output.display()))
}

#[test]
#[ignore = "requires Bombadil 0.7.10; set BOMBADIL_BIN or install it on PATH"]
fn browser_pages_preserve_unsaved_text_and_the_return_selection() -> Result<()> {
    explore(
        "pages",
        &[
            "unsaved-origin-selected",
            "files-opened",
            "files-help-opened",
            "files-help-resized",
            "changes-preview-focused",
            "diff-help-opened",
            "first-return-preserves-selection",
            "first-return-edited",
            "history-opened",
            "commit-files-opened",
            "commit-preview-focused",
            "files-from-commit-preview",
            "second-return-preserves-selection",
            "second-return-edited",
            "origin-saved",
            "closed-normally",
        ],
        &[("origin.txt", b"UN#AVED_abcdef\n")],
    )
}

#[test]
#[ignore = "requires Bombadil 0.7.10; set BOMBADIL_BIN or install it on PATH"]
fn cancelling_temporary_input_preserves_text_and_named_registers() -> Result<()> {
    explore(
        "input",
        &[
            "goto-prefix-visible",
            "goto-prefix-cancelled",
            "window-prefix-with-count",
            "window-prefix-cancelled",
            "count-does-not-leak",
            "first-candidate-and-documentation",
            "tab-completes-command",
            "completed-command-cancelled",
            "enter-executes-selected-candidate",
            "register-selected-for-command",
            "register-command-cancelled",
            "named-register-preserved",
            "register-output-saved",
            "closed-normally",
        ],
        &[
            ("origin.txt", b"ZXbcdef\n"),
            ("register-output.txt", b"ZAFTER"),
        ],
    )
}
