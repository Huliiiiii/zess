use std::{
    fs::{self, OpenOptions},
    io::BufReader,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use zess_ipc::{SocketPath, Stream, framing, platform};
use zess_protocol::{ClientMessage, ServerMessage};

pub fn default_socket(workspace: &Path) -> Result<SocketPath> {
    SocketPath::for_workspace(workspace)
}

pub fn socket_by_id(id: &str) -> Result<SocketPath> {
    Ok(SocketPath::by_id(id.parse()?))
}

fn request(socket: &SocketPath, message: ClientMessage) -> Result<ServerMessage> {
    let mut stream = Stream::connect(socket)
        .with_context(|| format!("connect to server at {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    framing::write(&mut stream, &message)?;
    let response = framing::read(&mut BufReader::new(stream), 1024 * 1024)?;
    let Some(response) = response.filter(|response| response.ends_with(b"\n")) else {
        bail!(
            "server at {} did not return a management response; close it with :exit, or on Linux use server stop with --force to discard the session",
            socket.display()
        );
    };
    framing::decode(&response).context("decode server management response")
}

pub fn list(socket: Option<&SocketPath>) -> Result<()> {
    let explicit = socket.is_some();
    let sockets = if let Some(socket) = socket {
        vec![socket.clone()]
    } else {
        SocketPath::discover()?
    };

    if sockets.is_empty() {
        println!("No servers found.");
        return Ok(());
    }

    println!("ID\tPID\tCLIENT\tMODIFIED\tWORKSPACE\tSOCKET");
    for socket in sockets {
        let id = if explicit {
            "-".into()
        } else {
            socket
                .as_path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
        };
        match request(&socket, ClientMessage::Status) {
            Ok(ServerMessage::Status {
                pid,
                workspace,
                attached,
                modified,
            }) => println!(
                "{id}\t{pid}\t{}\t{}\t{workspace}\t{}",
                if attached { "attached" } else { "detached" },
                if modified { "yes" } else { "no" },
                socket.display()
            ),
            Ok(ServerMessage::Rejected { reason }) => bail!("{reason}"),
            Ok(_) => bail!("unexpected management response from {}", socket.display()),
            Err(error) if explicit => return Err(error),
            Err(error) => {
                println!("{id}\t-\tunavailable\t-\t-\t{}", socket.display());
                eprintln!("{}: {error:#}", socket.display());
            }
        }
    }

    Ok(())
}

pub fn stop(socket: &SocketPath, force: bool) -> Result<()> {
    match request(socket, ClientMessage::Stop { force }) {
        Ok(ServerMessage::Exited) => {}
        Ok(ServerMessage::Rejected { reason }) => bail!("{reason}"),
        Ok(_) => bail!("unexpected management response from {}", socket.display()),
        #[cfg(target_os = "linux")]
        Err(error) if force => platform::stop(socket)
            .with_context(|| format!("management request failed ({error:#}); stop socket peer"))?,
        Err(error) => return Err(error),
    }

    println!("Stopped server at {}", socket.display());

    Ok(())
}

pub fn start(
    workspace: &Path,
    socket: &SocketPath,
    config: Option<&Path>,
    files: &[PathBuf],
) -> Result<()> {
    socket.prepare_parent()?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(socket.log_path())?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("server")
        .arg("--workspace")
        .arg(workspace)
        .arg("--socket")
        .arg(socket.as_path());
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    command
        .arg("--")
        .args(files)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    platform::detach(&mut command);
    let mut server = command.spawn().context("start editor server")?;
    let start = Instant::now();
    loop {
        if Stream::connect(socket).is_ok() {
            break;
        }
        if let Some(status) = server.try_wait()? {
            bail!(
                "server exited ({status}); see {}",
                socket.log_path().display()
            );
        }
        if start.elapsed() > Duration::from_secs(20) {
            bail!(
                "server startup timed out; see {}",
                socket.log_path().display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn restart(
    socket: &SocketPath,
    workspace: Option<&Path>,
    config: Option<&Path>,
    force: bool,
) -> Result<()> {
    // Check the workspace before stopping so a missing directory leaves the session intact.
    let workspace = match request(socket, ClientMessage::Status) {
        Ok(ServerMessage::Status {
            workspace: current,
            ..
        }) => {
            let current = PathBuf::from(current)
                .canonicalize()
                .context("open server workspace")?;
            if let Some(workspace) = workspace
                && workspace != current
            {
                bail!(
                    "server workspace is {}; refusing to restart it in {}",
                    current.display(),
                    workspace.display()
                );
            }
            current
        }
        Ok(ServerMessage::Rejected { reason }) => bail!("{reason}"),
        Ok(_) => bail!("unexpected management response from {}", socket.display()),
        Err(_) if force => workspace
            .context("cannot determine the older server's workspace; supply --workspace DIR with --force")?
            .canonicalize()
            .context("open server workspace")?,
        Err(error) => {
            return Err(error).context(
                "cannot inspect server; older servers require --force to discard their session",
            );
        }
    };
    eprintln!(
        "Restart creates a fresh session; open buffers, layout and undo history are reset{}.",
        if force {
            "; unsaved changes will be discarded"
        } else {
            ""
        }
    );
    stop(socket, force)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::symlink_metadata(socket.as_path()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error).context("wait for stopped server socket removal"),
            Ok(_) if Instant::now() >= deadline => bail!(
                "stopped server did not remove {}; replacement was not started",
                socket.display()
            ),
            Ok(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    start(&workspace, socket, config, &[])
        .context("old session stopped; start replacement server")?;
    println!(
        "Restarted server at {} using {}",
        socket.display(),
        std::env::current_exe()?.display()
    );
    Ok(())
}
