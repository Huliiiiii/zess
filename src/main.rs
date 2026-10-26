use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use zess_ipc::{SocketPath, Stream};
use zess_session::Session;

mod server;

fn main() -> Result<()> {
    #[derive(PartialEq)]
    enum GrammarAction {
        Fetch,
        Build,
    }

    #[derive(PartialEq)]
    enum Mode {
        Edit,
        Attach,
        Server,
        List,
        Stop,
        Restart,
        Grammar(GrammarAction),
    }

    let mut mode = Mode::Edit;
    let mut workspace = std::env::current_dir()?;
    let mut workspace_given = false;
    let mut socket = None;
    let mut config = None;
    let mut files = Vec::new();
    let mut force = false;
    let mut arguments = std::env::args().skip(1).peekable();
    if arguments
        .peek()
        .is_some_and(|arg| matches!(arg.as_str(), "server" | "attach"))
    {
        mode = match arguments.next().as_deref() {
            Some("server") => Mode::Server,
            Some("attach") => Mode::Attach,
            _ => unreachable!(),
        };
    }
    if mode == Mode::Server
        && arguments
            .peek()
            .is_some_and(|arg| matches!(arg.as_str(), "start" | "list" | "stop" | "restart"))
    {
        let action = arguments.next().unwrap();
        mode = match action.as_str() {
            "start" => Mode::Server,
            "list" => Mode::List,
            "stop" => Mode::Stop,
            "restart" => Mode::Restart,
            _ => unreachable!(),
        };
    }
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!(
                    "Zess — terminal editor with Wisp plugins\n\nUsage: zess [attach] [OPTIONS] [FILES...]\n       zess server [start] [OPTIONS] [FILES...]\n       zess server list [--socket PATH]\n       zess server stop [ID] [--workspace DIR | --socket PATH] [--force]\n       zess server restart [ID | --socket PATH] [--workspace DIR] [--config FILE] [--force]\n       zess --grammar fetch [GRAMMARS...]\n       zess --grammar build [GRAMMARS...]\n\nOptions: --workspace DIR, --socket PATH, --config FILE\n\nBy default, start a server if needed and attach the terminal.\nCtrl-z detaches; :q closes a window and stops the server after the last window.\n:exit terminates the server.\n:files, :outline, :git open plugin buffers; :config-reload reloads Wisp.\n\nGrammar fetch downloads pinned sources; grammar build compiles cached sources without networking.\nSet ZESS_RUNTIME to select the grammar and query directory.\n\nServer IDs are workspace path hashes shown by `server list`.\n`server list` discovers sockets in the default runtime directory; use --socket for custom paths.\n`server stop` defaults to the current workspace and refuses unsaved buffers unless --force is given.\n`server restart` starts a fresh detached session using this executable.\nSaved files remain on disk; open buffers, layout and undo history are reset.\nRestart disconnects an attached terminal; --force discards unsaved changes.\nFor older servers without management support, --force is required; with ID/--socket, also specify --workspace.\nRestart uses --config or the current default configuration."
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!("zess {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--workspace" => {
                workspace_given = true;
                workspace =
                    PathBuf::from(arguments.next().context("--workspace needs a directory")?)
            }
            "--socket" => {
                socket = Some(SocketPath::from(PathBuf::from(
                    arguments.next().context("--socket needs a path")?,
                )))
            }
            "--config" => {
                config = Some(PathBuf::from(
                    arguments.next().context("--config needs a file")?,
                ))
            }
            "--grammar" => {
                if mode != Mode::Edit {
                    bail!("--grammar cannot be combined with another mode");
                }
                let action = match arguments.next().as_deref() {
                    Some("fetch") => GrammarAction::Fetch,
                    Some("build") => GrammarAction::Build,
                    _ => bail!("usage: zess --grammar fetch|build [GRAMMARS...]"),
                };
                mode = Mode::Grammar(action);
            }
            "--force" if matches!(mode, Mode::Stop | Mode::Restart) => force = true,
            "--" => {
                files.extend(arguments.map(PathBuf::from));
                break;
            }
            _ if argument.starts_with('-') => bail!("unknown option {argument}"),
            _ => files.push(PathBuf::from(argument)),
        }
    }
    if let Mode::Grammar(action) = mode {
        if workspace_given || socket.is_some() || config.is_some() {
            bail!("usage: zess --grammar fetch|build [GRAMMARS...]");
        }
        let names = files
            .iter()
            .map(|name| {
                name.to_str()
                    .context("grammar name must be UTF-8")
                    .map(str::to_owned)
            })
            .collect::<Result<Vec<_>>>()?;
        let directory = match action {
            GrammarAction::Fetch => zess_text::runtime::fetch_grammars(None, &names)?,
            GrammarAction::Build => zess_text::runtime::build_grammars(None, &names)?,
        };
        println!("Syntax runtime: {}", directory.display());
        return Ok(());
    }
    if mode == Mode::List {
        if !files.is_empty() || config.is_some() || workspace_given {
            bail!("usage: zess server list [--socket PATH]");
        }
        return server::list(socket.as_ref());
    }
    if mode == Mode::Stop {
        if files.len() > 1
            || config.is_some()
            || (!files.is_empty() && (socket.is_some() || workspace_given))
            || (socket.is_some() && workspace_given)
        {
            bail!("usage: zess server stop [ID] [--workspace DIR | --socket PATH] [--force]");
        }
        let socket = if let Some(id) = files.first() {
            server::socket_by_id(id.to_str().context("server ID must be hexadecimal")?)?
        } else if let Some(socket) = socket {
            socket
        } else {
            server::default_socket(
                &workspace
                    .canonicalize()
                    .context("open workspace directory")?,
            )?
        };

        return server::stop(&socket, force);
    }
    if mode == Mode::Restart {
        if files.len() > 1 || (!files.is_empty() && socket.is_some()) {
            bail!(
                "usage: zess server restart [ID | --socket PATH] [--workspace DIR] [--config FILE] [--force]"
            );
        }
        let explicit_target = !files.is_empty() || socket.is_some();
        let workspace = if workspace_given || !explicit_target {
            Some(
                workspace
                    .canonicalize()
                    .context("open workspace directory")?,
            )
        } else {
            None
        };
        let socket = if let Some(id) = files.first() {
            server::socket_by_id(id.to_str().context("server ID must be hexadecimal")?)?
        } else if let Some(socket) = socket {
            socket
        } else {
            server::default_socket(workspace.as_deref().unwrap())?
        };
        return server::restart(
            &socket,
            workspace.as_deref(),
            config.or_else(default_config).as_deref(),
            force,
        );
    }
    if !workspace_given && files.first().is_some_and(|path| path.is_dir()) {
        for path in &mut files {
            if path.is_relative() {
                *path = workspace.join(&*path);
            }
        }
        workspace = files[0]
            .canonicalize()
            .context("open workspace directory")?;
        files[0] = workspace.clone();
    }
    let workspace = workspace
        .canonicalize()
        .context("open workspace directory")?;
    let socket = match socket {
        Some(path) => path,
        None => server::default_socket(&workspace)?,
    };
    let config = config.or_else(default_config);
    if mode == Mode::Server {
        socket.prepare_parent()?;
        return zess_session::serve(&socket, Session::new(workspace, &files, config)?);
    }
    if mode == Mode::Edit && Stream::connect(&socket).is_err() {
        server::start(&workspace, &socket, config.as_deref(), &files)?;
    }
    let files = files
        .iter()
        .map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                workspace.join(path)
            }
        })
        .collect::<Vec<_>>();
    zess_terminal::run_files(&socket, &files)
}

fn default_config() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    ["init.w", "init.wisp"]
        .into_iter()
        .map(|name| base.join("zess").join(name))
        .find(|path| path.is_file())
}
