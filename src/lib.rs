use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use zess_ipc::platform::NativeDaemonControl;
use zess_ipc::{LocalIpc, NativeLocalIpc, SocketPath};
use zess_platform::{
    CancellationToken, FileSystem, NativeFileSystem, NativePlatform, NativeWatchBackend, Platform,
    ProcessRunner, SystemProcessRunner, WatchBackend,
};
use zess_remote::SshTarget;
use zess_session::Session;
use zess_text::runtime::{GrammarToolchain, NativeGrammarToolchain};

mod remote;
pub mod server;

struct RuntimeDependencies {
    filesystem: Arc<dyn FileSystem>,
    host: Arc<dyn Platform>,
    ipc: Arc<dyn LocalIpc>,
    manager: server::SessionManager,
    processes: Arc<dyn ProcessRunner>,
    watches: Arc<dyn WatchBackend>,
    git_queries: Arc<dyn zess_git::GitQueries>,
    grammars: Box<dyn GrammarToolchain>,
    script_compiler: Arc<dyn zess_script::ScriptCompiler>,
}

struct SessionConnectionFactory {
    manager: server::SessionManager,
    endpoint: SocketPath,
}

impl zess_terminal::ConnectionFactory for SessionConnectionFactory {
    fn connect(&mut self) -> Result<Box<dyn zess_terminal::ConnectionStream>> {
        Ok(Box::new(self.manager.connect(&self.endpoint)?))
    }

    fn connect_with_cancellation(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn zess_terminal::ConnectionStream>> {
        self.manager = self.manager.clone().with_cancellation(cancellation.clone());
        self.connect()
    }
}

pub fn run() -> Result<()> {
    let initial = std::env::args().skip(1).collect::<Vec<_>>();
    let filesystem: Arc<dyn FileSystem> = Arc::new(NativeFileSystem);
    let host: Arc<dyn Platform> = Arc::new(NativePlatform);
    let ipc: Arc<dyn LocalIpc> = Arc::new(NativeLocalIpc);
    let manager = server::SessionManager::new(
        filesystem.clone(),
        ipc.clone(),
        Arc::new(NativeDaemonControl),
        host.clone(),
    );
    let dependencies = RuntimeDependencies {
        grammars: Box::new(NativeGrammarToolchain::new(
            host.clone(),
            filesystem.clone(),
        )),
        git_queries: Arc::new(zess_git::NativeGitQueries::new(filesystem.clone())),
        filesystem,
        host,
        ipc,
        manager,
        processes: Arc::new(SystemProcessRunner),
        watches: Arc::new(NativeWatchBackend),
        script_compiler: Arc::new(zess_script::NativeSteelCompiler),
    };
    run_with(initial, dependencies)
}

fn run_with(initial: Vec<String>, dependencies: RuntimeDependencies) -> Result<()> {
    let RuntimeDependencies {
        filesystem,
        host,
        ipc,
        manager,
        processes,
        watches,
        git_queries,
        grammars,
        script_compiler,
    } = dependencies;
    match initial.first().map(String::as_str) {
        Some(zess_terminal::CLIPBOARD_HELPER_ARG) => {
            return zess_terminal::run_clipboard_helper(
                initial.get(1).context("clipboard operation missing")?,
            );
        }
        Some("remote-info") => {
            let info = zess_remote::RemoteInfo::current_with_file_system(
                env!("CARGO_PKG_VERSION"),
                filesystem.as_ref(),
                &host.current_exe()?,
            )?;
            println!("{}", serde_json::to_string(&info)?);
            return Ok(());
        }
        Some("remote-proxy") => return remote::serve_proxy(manager, filesystem, host),
        _ => {}
    }
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
        New,
        Grammar(GrammarAction),
    }

    let mut mode = Mode::Edit;
    let mut workspace = host.current_dir()?;
    let mut workspace_given = false;
    let mut socket = None;
    let mut config = None;
    let mut files = Vec::new();
    let mut force = false;
    let mut json = false;
    let mut session_id = None;
    let mut connect_by_id = false;
    let mut ssh_host = None;
    let mut ssh_port = None;
    let mut ssh_user = None;
    let mut ssh_identity = None;
    let mut ssh_config = None;
    let mut ssh_jump = None;
    let mut remote_workspace = None;
    let mut server_binary = None;
    let mut server_artifacts = None;
    let mut arguments = initial.into_iter().peekable();
    if arguments
        .peek()
        .is_some_and(|arg| matches!(arg.as_str(), "server" | "attach" | "session"))
    {
        mode = match arguments.next().as_deref() {
            Some("server") => Mode::Server,
            Some("attach") => Mode::Attach,
            Some("session") => match arguments.next().as_deref() {
                Some("list") => Mode::List,
                Some("new" | "create") => Mode::New,
                Some("connect" | "attach") => {
                    connect_by_id = true;
                    Mode::Attach
                }
                Some("stop") => Mode::Stop,
                Some("restart") => Mode::Restart,
                _ => bail!("usage: zess session list|new|connect|stop|restart [OPTIONS]"),
            },
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
                    "Zess — terminal editor with Wisp plugins\n\nUsage: zess [attach] [OPTIONS] [FILES...]\n       zess server [start] [OPTIONS] [FILES...]\n       zess server list [--socket PATH]\n       zess server stop [ID] [--workspace DIR | --socket PATH] [--force]\n       zess server restart [ID | --socket PATH] [--workspace DIR] [--config FILE] [--force]\n       zess --grammar fetch [GRAMMARS...]\n       zess --grammar build [GRAMMARS...]\n\nOptions: --workspace DIR, --socket PATH, --config FILE\n\nBy default, start a server if needed and attach the terminal.\nCtrl-z detaches; :q closes a buffer or exits when only a clean scratch remains.\nThe server keeps the session by default after the client exits.\n:files, :outline, :git open plugin buffers; :config-reload reloads Wisp.\n\nGrammar fetch downloads pinned sources; grammar build compiles cached sources without networking.\nSet ZESS_RUNTIME to select the grammar and query directory.\n\nServer IDs are workspace path hashes shown by `server list`.\n`server list` discovers sockets in the default runtime directory; use --socket for custom paths.\n`server stop` defaults to the current workspace and refuses unsaved buffers unless --force is given.\n`server restart` starts a fresh detached session using this executable.\nSaved files remain on disk; open buffers, layout and undo history are reset.\nRestart disconnects an attached terminal; --force discards unsaved changes.\nFor older servers without management support, --force is required; with ID/--socket, also specify --workspace.\nRestart uses --config or the current default configuration."
                );
                println!(
                    "\nSession manager: zess session list|new|connect|stop|restart [OPTIONS]\n  --session ID selects a running session; --json prints manager responses.\nSSH: --ssh HOST [--remote-workspace PATH]\n  --ssh-config FILE, --ssh-identity FILE, --ssh-user USER, --ssh-port PORT, --ssh-jump HOST\n  --server-binary FILE selects a binary built for the remote platform.\n  --server-artifacts FILE selects a JSON artifact catalog for uploads or verified HTTPS downloads.\nRemote paths are interpreted on the remote machine.\nThe default on-client-exit=stop-if-unused stops an unused, unmodified session after normal client exit."
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!("zess {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--json" => json = true,
            "--session" => session_id = Some(arguments.next().context("--session needs an ID")?),
            "--ssh" => {
                ssh_host = Some(
                    arguments
                        .next()
                        .context("--ssh needs a host or SSH alias")?,
                )
            }
            "--ssh-port" => {
                ssh_port = Some(
                    arguments
                        .next()
                        .context("--ssh-port needs a port")?
                        .parse()
                        .context("invalid SSH port")?,
                )
            }
            "--ssh-user" => ssh_user = Some(arguments.next().context("--ssh-user needs a user")?),
            "--ssh-identity" => {
                ssh_identity = Some(PathBuf::from(
                    arguments.next().context("--ssh-identity needs a path")?,
                ))
            }
            "--ssh-config" => {
                ssh_config = Some(PathBuf::from(
                    arguments.next().context("--ssh-config needs a path")?,
                ))
            }
            "--ssh-jump" => ssh_jump = Some(arguments.next().context("--ssh-jump needs a host")?),
            "--remote-workspace" => {
                remote_workspace = Some(
                    arguments
                        .next()
                        .context("--remote-workspace needs a path")?,
                )
            }
            "--server-binary" => {
                server_binary = Some(PathBuf::from(
                    arguments.next().context("--server-binary needs a path")?,
                ))
            }
            "--server-artifacts" => {
                server_artifacts = Some(PathBuf::from(
                    arguments
                        .next()
                        .context("--server-artifacts needs a JSON catalog")?,
                ))
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
    if connect_by_id && session_id.is_none() && !files.is_empty() {
        session_id = Some(remote::path_string(&files.remove(0))?);
    }
    if connect_by_id && session_id.is_none() {
        bail!("usage: zess session connect ID [OPTIONS] [FILES...]");
    }
    if mode == Mode::Edit && session_id.is_some() {
        mode = Mode::Attach;
    }
    if let Some(ssh_host) = ssh_host {
        if socket.is_some() || matches!(mode, Mode::Grammar(_)) {
            bail!("SSH sessions select their remote endpoint by workspace or --session");
        }
        let mut target = SshTarget::new(ssh_host);
        target.port = ssh_port;
        target.user = ssh_user;
        target.identity_file = ssh_identity;
        target.config_file = ssh_config;
        target.jump_host = ssh_jump;
        let remote = remote::RemoteOptions {
            target,
            server_binary,
            server_artifacts,
            file_system: filesystem.clone(),
            platform: host.clone(),
        };
        let remote_workspace_given = remote_workspace.is_some() || workspace_given;
        let workspace = remote_workspace
            .or_else(|| workspace_given.then(|| workspace.to_string_lossy().into_owned()))
            .unwrap_or_else(|| ".".to_owned());
        let config = config.as_deref().map(remote::path_string).transpose()?;
        let paths = files
            .iter()
            .map(|path| remote::path_string(path))
            .collect::<Result<Vec<_>>>()?;
        return match mode {
            Mode::List => {
                let sessions = remote.sessions()?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&sessions)?);
                } else {
                    print_sessions(&sessions);
                }
                Ok(())
            }
            Mode::Stop => {
                let id = session_id.or_else(|| paths.first().cloned());
                remote.stop(id, Some(workspace), force)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&remote::ProxyResponse::Stopped)?
                    );
                } else {
                    println!("Stopped remote session.");
                }
                Ok(())
            }
            Mode::New | Mode::Server => {
                let session = remote.create(workspace, config, paths)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&session)?);
                } else {
                    print_sessions(&[session]);
                }
                Ok(())
            }
            Mode::Restart => {
                let id = session_id.or_else(|| paths.first().cloned());
                let workspace = if id.is_some() && !remote_workspace_given {
                    None
                } else {
                    Some(workspace)
                };
                let session = remote.restart(id, workspace, config, force)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&session)?);
                } else {
                    print_sessions(&[session]);
                }
                Ok(())
            }
            Mode::Edit | Mode::Attach => {
                if let Some(id) = session_id {
                    let session = remote
                        .sessions()?
                        .into_iter()
                        .find(|entry| entry.id == id || entry.session_id.0 == id)
                        .with_context(|| format!("remote session {id} was not found"))?;
                    let mut factory = remote.session_factory(session.session_id.0)?;
                    zess_terminal::run_connection_with_platform(&mut factory, &files, host.as_ref())
                } else {
                    let mut factory = remote.factory(workspace, Vec::new(), config)?;
                    zess_terminal::run_connection_with_platform(&mut factory, &files, host.as_ref())
                }
            }
            Mode::Grammar(_) => unreachable!(),
        };
    }
    if ssh_port.is_some()
        || ssh_user.is_some()
        || ssh_identity.is_some()
        || ssh_config.is_some()
        || ssh_jump.is_some()
        || remote_workspace.is_some()
        || server_binary.is_some()
        || server_artifacts.is_some()
    {
        bail!("SSH connection and server artifact options require --ssh HOST");
    }
    if matches!(mode, Mode::Stop | Mode::Restart)
        && let Some(id) = session_id.take()
    {
        if !files.is_empty() {
            bail!("select a session once, by ID or --session");
        }
        files.push(PathBuf::from(id));
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
        let progress = |action: &str, name: &str| println!("{action}: {name}");
        let directory = match action {
            GrammarAction::Fetch => grammars.fetch(None, &names, &progress)?,
            GrammarAction::Build => grammars.build(None, &names, &progress)?,
        };
        println!("Syntax runtime: {}", directory.display());
        return Ok(());
    }
    if mode == Mode::List {
        if !files.is_empty() || config.is_some() || workspace_given {
            bail!("usage: zess server list [--socket PATH]");
        }
        if json {
            let entries = if let Some(socket) = &socket {
                vec![manager.inspect(socket)?]
            } else {
                manager.entries()?
            };
            println!("{}", serde_json::to_string_pretty(&entries)?);
            return Ok(());
        }
        return server::list_with_manager(&manager, socket.as_ref());
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
            manager.resolve(id.to_str().context("server ID must be hexadecimal")?)?
        } else if let Some(socket) = socket {
            socket
        } else {
            manager.endpoint_for_workspace(
                &filesystem
                    .canonicalize(&workspace)
                    .context("open workspace directory")?,
            )?
        };

        if json {
            manager.stop(&socket, force)?;
            println!(
                "{}",
                serde_json::to_string(&remote::ProxyResponse::Stopped)?
            );
            return Ok(());
        }
        manager.stop(&socket, force)?;
        println!("Stopped server at {}", socket.display());
        return Ok(());
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
                filesystem
                    .canonicalize(&workspace)
                    .context("open workspace directory")?,
            )
        } else {
            None
        };
        let socket = if let Some(id) = files.first() {
            manager.resolve(id.to_str().context("server ID must be hexadecimal")?)?
        } else if let Some(socket) = socket {
            socket
        } else {
            manager.endpoint_for_workspace(workspace.as_deref().unwrap())?
        };
        return server::restart_with_manager(
            &manager,
            &socket,
            workspace.as_deref(),
            config
                .or_else(|| default_config(filesystem.as_ref(), host.as_ref()))
                .as_deref(),
            force,
        );
    }
    if !workspace_given
        && files.first().is_some_and(|path| {
            filesystem
                .metadata(path)
                .is_ok_and(|metadata| metadata.is_dir())
        })
    {
        for path in &mut files {
            if path.is_relative() {
                *path = workspace.join(&*path);
            }
        }
        workspace = filesystem
            .canonicalize(&files[0])
            .context("open workspace directory")?;
        files[0] = workspace.clone();
    }
    let mut workspace = filesystem
        .canonicalize(&workspace)
        .context("open workspace directory")?;
    let socket = match (session_id, socket) {
        (Some(id), None) => {
            let endpoint = manager.resolve(&id)?;
            workspace = PathBuf::from(manager.inspect(&endpoint)?.workspace);
            endpoint
        }
        (Some(_), Some(_)) => bail!("--session cannot be combined with --socket"),
        (None, Some(path)) => path,
        (None, None) => manager.endpoint_for_workspace(&workspace)?,
    };
    let config = config.or_else(|| default_config(filesystem.as_ref(), host.as_ref()));
    if mode == Mode::Server {
        ipc.prepare(&socket)?;
        let services = zess_services::NativeServices::with_git_queries(
            workspace.clone(),
            filesystem.clone(),
            processes,
            watches,
            git_queries,
        )?;
        let syntax_provider = Arc::new(zess_text::HelixSyntaxProvider::with_platform(
            host.clone(),
            filesystem.clone(),
        ));
        return zess_session::serve_with_platform(
            ipc.as_ref(),
            &socket,
            Session::with_capabilities(
                workspace,
                &files,
                config,
                services,
                syntax_provider,
                script_compiler,
            )?,
            host.as_ref(),
        );
    }
    let socket = if matches!(mode, Mode::Edit | Mode::New) {
        let session = manager.ensure(&workspace, &socket, config.as_deref(), &files)?;
        if mode == Mode::New {
            if json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                print_sessions(&[session]);
            }
            return Ok(());
        }
        SocketPath::from(PathBuf::from(session.endpoint))
    } else {
        socket
    };
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
    zess_terminal::run_connection_with_platform(
        &mut SessionConnectionFactory {
            manager,
            endpoint: socket,
        },
        &files,
        host.as_ref(),
    )
}

fn default_config(filesystem: &dyn FileSystem, host: &dyn Platform) -> Option<PathBuf> {
    let base = host.user_dirs().ok()?.config;
    ["init.w", "init.wisp"]
        .into_iter()
        .map(|name| base.join("zess").join(name))
        .find(|path| {
            filesystem
                .metadata(path)
                .is_ok_and(|metadata| metadata.is_file())
        })
}

fn print_sessions(sessions: &[server::SessionInfo]) {
    println!("ID\tSESSION\tPID\tCLIENT\tMODIFIED\tWORKSPACE");
    for session in sessions {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            session.id,
            session.session_id.0,
            session.pid,
            if session.attached {
                "attached"
            } else {
                "detached"
            },
            if session.modified { "yes" } else { "no" },
            session.workspace
        );
    }
}
