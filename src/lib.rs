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

use crate::server::SessionTarget;

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
    session_id: zess_protocol::SessionId,
}

impl zess_terminal::ConnectionFactory for SessionConnectionFactory {
    fn connect(&mut self) -> Result<Box<dyn zess_terminal::ConnectionStream>> {
        let stream = self.manager.connect(&self.endpoint)?;
        let process = self.manager.watch(&stream)?;
        Ok(Box::new(
            zess_terminal::LocalConnection::new(stream, process)
                .with_session_id(self.session_id.clone()),
        ))
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
    let mut session_name = None;
    let mut connect_by_id = false;
    let mut ssh_host = None;
    let mut ssh_port = None;
    let mut ssh_user = None;
    let mut ssh_identity = None;
    let mut ssh_config = None;
    let mut ssh_jump = None;
    let mut remote_workspace = None;
    let mut ssh_address = None;
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
                Some("new") => Mode::New,
                Some("attach") => {
                    connect_by_id = true;
                    Mode::Attach
                }
                Some("stop") => Mode::Stop,
                Some("restart") => Mode::Restart,
                _ => bail!("usage: zess session list|new|attach|stop|restart [OPTIONS]"),
            },
            _ => unreachable!(),
        };
    }
    if mode == Mode::Server
        && arguments
            .peek()
            .is_some_and(|arg| matches!(arg.as_str(), "start" | "list" | "stop" | "restart"))
    {
        bail!("use zess session new|list|stop|restart");
    }
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!(
                    "Zess — terminal editor with Wisp plugins\n\nUsage: zess [OPTIONS] [FILES...]\n       zess ssh://[USER@]HOST[:PORT]/WORKSPACE\n       zess session new [NAME] [OPTIONS]\n       zess session list [OPTIONS]\n       zess session attach [NAME|ID] [OPTIONS]\n       zess session stop [NAME|ID] [OPTIONS]\n       zess session restart [NAME|ID] [OPTIONS]\n\nOptions: --workspace DIR, --config FILE, --ssh HOST, --json\nCtrl-z detaches; :q exits when only a clean scratch remains.\nRestart creates a fresh session and resets open buffers, layout and undo history.\nA matching server is deployed automatically over SSH."
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!(
                    "zess {} (build {})",
                    env!("CARGO_PKG_VERSION"),
                    zess_build::BUILD_ID
                );
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
            "--session-name" if mode == Mode::Server => {
                session_name = Some(arguments.next().context("--session-name needs a name")?)
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
            _ if argument.starts_with("ssh://") => {
                if ssh_address.replace(argument).is_some() {
                    bail!("only one SSH address may be specified");
                }
            }
            _ if argument.starts_with('-') => bail!("unknown option {argument}"),
            _ => files.push(PathBuf::from(argument)),
        }
    }
    if let Some(address) = ssh_address.as_deref() {
        let address = remote::SshAddress::parse(address)?;
        if ssh_host.as_deref().is_some_and(|host| host != address.host)
            || ssh_user
                .as_deref()
                .zip(address.user)
                .is_some_and(|(explicit, parsed)| explicit != parsed)
            || ssh_port
                .zip(address.port)
                .is_some_and(|(explicit, parsed)| explicit != parsed)
        {
            bail!("SSH address conflicts with explicit SSH options");
        }
        if remote_workspace
            .as_deref()
            .is_some_and(|path| path != address.workspace)
            || (workspace_given && workspace.to_str() != Some(address.workspace))
        {
            bail!("SSH address conflicts with the specified workspace");
        }
        ssh_host = Some(address.host.to_owned());
        ssh_user = ssh_user.or_else(|| address.user.map(str::to_owned));
        ssh_port = ssh_port.or(address.port);
        remote_workspace = Some(address.workspace.to_owned());
    }
    if connect_by_id && session_id.is_none() && !files.is_empty() {
        session_id = Some(remote::path_string(&files.remove(0))?);
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
        let artifacts = Arc::new(zess_remote::GithubReleaseArtifacts::new(
            env!("CARGO_PKG_REPOSITORY")
                .strip_prefix("https://github.com/")
                .context("repository must be on GitHub")?
                .trim_end_matches(".git"),
            filesystem.clone(),
            host.clone(),
        ));
        let remote = remote::RemoteOptions {
            target,
            bootstrap: Arc::new(zess_remote::SshBootstrap {
                artifacts,
                filesystem: filesystem.clone(),
            }),
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
                if paths.len() > 1 {
                    bail!("usage: zess session new [NAME]");
                }
                let name = paths.into_iter().next();
                let session = remote.create(workspace, config, Vec::new(), name)?;
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
                        .find(|entry| {
                            entry.id == id
                                || entry.session_id.0 == id
                                || entry.name.as_deref() == Some(id.as_str())
                        })
                        .with_context(|| format!("remote session {id} was not found"))?;
                    zess_protocol::check_build_id(&session.build_id)?;
                    let mut factory = remote.session_factory(session.session_id.0)?;
                    zess_terminal::run_connection_with_platform(&mut factory, &files, host.as_ref())
                } else {
                    let sessions = remote.workspace_sessions(workspace.clone())?;
                    if mode == Mode::Attach || sessions.len() > 1 {
                        let selected = match sessions.as_slice() {
                            [] => bail!("no session exists for the remote workspace"),
                            [session] => session,
                            _ => select_session(&sessions)?,
                        };
                        zess_protocol::check_build_id(&selected.build_id)?;
                        let mut factory = remote.session_factory(selected.session_id.0.clone())?;
                        return zess_terminal::run_connection_with_platform(
                            &mut factory,
                            &files,
                            host.as_ref(),
                        );
                    }
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
    {
        bail!("SSH connection options require --ssh HOST");
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
            bail!("usage: zess session list [--socket PATH]");
        }
        let entries = if let Some(socket) = &socket {
            vec![manager.inspect(socket)?]
        } else {
            manager.entries()?
        };
        if json {
            println!("{}", serde_json::to_string_pretty(&entries)?);
        } else {
            print_sessions(&entries);
        }
        return Ok(());
    }
    if mode == Mode::Stop {
        if files.len() > 1
            || config.is_some()
            || (!files.is_empty() && (socket.is_some() || workspace_given))
            || (socket.is_some() && workspace_given)
        {
            bail!("usage: zess session stop [ID] [--workspace DIR | --socket PATH] [--force]");
        }
        let target = if let Some(id) = files.first() {
            manager.resolve(id.to_str().context("server ID must be hexadecimal")?)?
        } else if let Some(socket) = socket {
            SessionTarget::Endpoint(socket)
        } else {
            SessionTarget::Endpoint(
                manager.endpoint_for_workspace(
                    &filesystem
                        .canonicalize(&workspace)
                        .context("open workspace directory")?,
                )?,
            )
        };

        manager.stop_target(&target, force)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&remote::ProxyResponse::Stopped)?
            );
            return Ok(());
        }
        println!("Stopped server at {}", target.endpoint().display());
        return Ok(());
    }
    if mode == Mode::Restart {
        if files.len() > 1 || (!files.is_empty() && socket.is_some()) {
            bail!(
                "usage: zess session restart [ID | --socket PATH] [--workspace DIR] [--config FILE] [--force]"
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
        let target = if let Some(id) = files.first() {
            manager.resolve(id.to_str().context("server ID must be hexadecimal")?)?
        } else if let Some(socket) = socket {
            SessionTarget::Endpoint(socket)
        } else {
            SessionTarget::Endpoint(manager.endpoint_for_workspace(workspace.as_deref().unwrap())?)
        };
        eprintln!(
            "Restart creates a fresh session; open buffers, layout and undo history are reset{}.",
            if force {
                "; unsaved changes will be discarded"
            } else {
                ""
            }
        );
        let session = manager.restart_target(
            &target,
            workspace.as_deref(),
            config
                .or_else(|| default_config(filesystem.as_ref(), host.as_ref()))
                .as_deref(),
            force,
            files.first().and_then(|id| id.to_str()),
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&session)?);
        } else {
            print_sessions(&[session]);
        }
        return Ok(());
    }
    if mode == Mode::Edit
        && !workspace_given
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
    let socket_given = socket.is_some();
    let mut selected_session = None;
    let socket = match (session_id, socket) {
        (Some(id), None) => {
            let target = manager.resolve(&id)?;
            let info = manager.inspect_target(&target)?;
            workspace = PathBuf::from(&info.workspace);
            selected_session = Some(info.session_id);
            target.endpoint().clone()
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
        let mut session = Session::with_capabilities(
            workspace,
            &files,
            config,
            services,
            syntax_provider,
            script_compiler,
        )?;
        if let Some(name) = session_name {
            session.set_name(name);
        }
        return zess_session::serve_with_platform(ipc.as_ref(), &socket, session, host.as_ref());
    }
    let (socket, session_id) = if matches!(mode, Mode::Edit | Mode::New) {
        if mode == Mode::New {
            if files.len() > 1 {
                bail!("usage: zess session new [NAME]");
            }
            let name = files
                .first()
                .map(|path| path.to_str().context("session name must be UTF-8"))
                .transpose()?;
            let session = manager.create(&workspace, config.as_deref(), &[], name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                print_sessions(&[session]);
            }
            return Ok(());
        }
        let sessions = manager.workspace_sessions(&workspace)?;
        let session = if sessions.len() > 1 {
            select_session(&sessions)?.clone()
        } else {
            manager.ensure(&workspace, &socket, config.as_deref(), &files)?
        };
        (
            SocketPath::from(PathBuf::from(session.endpoint)),
            session.session_id,
        )
    } else {
        if selected_session.is_none() && !socket_given {
            let sessions = manager.workspace_sessions(&workspace)?;
            let session = match sessions.as_slice() {
                [] => bail!("no session exists for {}", workspace.display()),
                [session] => session,
                _ => select_session(&sessions)?,
            };
            (
                SocketPath::from(PathBuf::from(&session.endpoint)),
                session.session_id.clone(),
            )
        } else {
            let session_id = match selected_session {
                Some(id) => id,
                None => manager.inspect(&socket)?.session_id,
            };
            (socket, session_id)
        }
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
            session_id,
        },
        &files,
        host.as_ref(),
    )
}

fn select_session(sessions: &[server::SessionInfo]) -> Result<&server::SessionInfo> {
    use std::io::Write;
    eprintln!("Multiple sessions are available:");
    for (index, session) in sessions.iter().enumerate() {
        eprintln!(
            "  {}: {} ({})",
            index + 1,
            session.name.as_deref().unwrap_or("unnamed"),
            session.id
        );
    }
    eprint!("Select a session: ");
    std::io::stderr().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let index: usize = input.trim().parse().context("enter a session number")?;
    sessions
        .get(index.wrapping_sub(1))
        .context("invalid session number")
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
    println!("NAME\tID\tSESSION\tBUILD\tPID\tCLIENT\tMODIFIED\tWORKSPACE");
    for session in sessions {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            session.name.as_deref().unwrap_or("-"),
            session.id,
            session.session_id.0,
            session.build_id,
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
