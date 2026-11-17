use zess_plugin::{HostRequest, ItemSelection, PluginOutput};
use zess_protocol::{
    CommandOutcome,
    resource::{PageMetadata, ResourceEvent, ResourceRequest, ResourceUpdate},
};
use zess_ui::{Display, DisplayRow};

use super::*;

fn exit_target(message: &ClientMessage) -> Option<&CommandTarget> {
    match message {
        ClientMessage::Command {
            action:
                CommandAction::Request {
                    target,
                    request: HostRequest::Action { name, .. },
                },
            ..
        } if name == "exit" => Some(target),
        _ => None,
    }
}

#[test]
fn a_page_prompt_completes_while_exit_waits_for_conflict_resolution() {
    let phase = Arc::new(Phase::default());
    let state = view_state(
        BufferKind::Text { path: None },
        ViewContent::Text { lines: Vec::new() },
        &["global", "text"],
    );
    let mut registry = Registry::<()>::default();
    registry.register_command(
        "files",
        "",
        CommandImplementation::Request(HostRequest::OpenBuffer {
            kind: "files".into(),
            argument: serde_json::json!({}),
        }),
    );
    registry.register_command(
        "files.create",
        "",
        CommandImplementation::BufferEvent {
            kind: "files".into(),
            event: "create".into(),
        },
    );
    for command in ["exit", "document.use-workspace"] {
        registry.register_command(command, "", CommandImplementation::Native);
    }
    for (scope, mode, key, command) in [
        ("global", "normal", ":", "command-prompt"),
        ("global", "normal", "o", "files"),
        ("files", "normal", "c", "files.create"),
        ("files", "normal", "j", "data.move-down"),
        ("files", "normal", "Esc", "data.escape"),
        ("global", "prompt", "Enter", "prompt.submit"),
    ] {
        registry.binding(scope, mode, &[key], command);
    }
    for command in [
        "command-prompt",
        "data.move-down",
        "data.escape",
        "prompt.submit",
    ] {
        registry.register_action(command, "", CommandImplementation::Native);
    }

    let mut events = VecDeque::from([
        Some(Event::Paste("local edit".into())),
        None,
        key(KeyCode::Char(':')),
        Some(Event::Paste("exit".into())),
        key(KeyCode::Enter),
        key(KeyCode::Char('o')),
        None,
        key(KeyCode::Char('c')),
        None,
        None,
        Some(Event::Resize(80, 24)),
        Some(Event::Paste("new.txt".into())),
        key(KeyCode::Enter),
        None,
        key(KeyCode::Char('j')),
        key(KeyCode::Esc),
        None,
        key(KeyCode::Char(':')),
        Some(Event::Paste("document.use-workspace".into())),
        key(KeyCode::Enter),
    ]);
    let resolve_conflict_at = events.len();
    events.extend([None, None]);
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                0,
                vec![
                    attached(),
                    snapshot_with_bindings(1, state, registry.binding_catalog()),
                    document_contents(),
                ],
            ),
            (events.len(), vec![ServerMessage::Exited]),
        ],
        None,
    );
    let update = |subscription_id, generation, output, metadata| ServerMessage::Resource {
        update: ResourceUpdate {
            cache: Default::default(),
            kind: Default::default(),
            subscription_id,
            generation,
            document_scoped: false,
            metadata,
            output,
            document: None,
            patch_revision: None,
        },
    };
    let completed = |request_id, server_instance| ServerMessage::CommandResult {
        request_id,
        server_instance,
        result: OperationResult::Completed {
            outcome: CommandOutcome::Applied,
            state_revision: 1,
        },
    };
    let mut pending_edit = None;
    let mut pending_create = None;
    let mut pending_prompt = None;
    let reply_phase = phase.clone();
    stream.reply = Box::new(move |message| match message {
        ClientMessage::Document(DocumentRequest::Edit {
            buffer_id: 7,
            sequence,
            ..
        }) => {
            pending_edit = Some(sequence);
            None
        }
        ClientMessage::Resource {
            request:
                ResourceRequest::Subscribe {
                    subscription_id,
                    input,
                    ..
                },
        } => Some(update(
            subscription_id,
            input.generation,
            PluginOutput {
                display: Some(Display {
                    rows: ["first.txt", "second.txt"]
                        .into_iter()
                        .map(|path| DisplayRow {
                            id: path.into(),
                            text: path.into(),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }),
                selection: Some(ItemSelection {
                    primary: Some("first.txt".into()),
                    selected: vec!["first.txt".into()],
                    anchor: Some("first.txt".into()),
                }),
                ..Default::default()
            },
            Some(PageMetadata {
                view_id: 20,
                buffer_id: 21,
                options: serde_json::json!({}),
            }),
        )),
        ClientMessage::Command {
            request_id,
            server_instance,
            action:
                CommandAction::Invoke {
                    target:
                        CommandTarget::Page {
                            subscription_id,
                            input,
                        },
                    name,
                    ..
                },
        } if name == "files.create" => {
            pending_create = Some(request_id);
            pending_prompt = Some((subscription_id, input.generation));
            Some(completed(request_id, server_instance))
        }
        ClientMessage::ResultAck { request_id, .. } if pending_create == Some(request_id) => {
            let sequence = pending_edit.take()?;
            let ServerMessage::Document(DocumentEvent::Snapshot(mut current)) = document_contents()
            else {
                unreachable!();
            };
            current.version = 2;
            current.text = "workspace change".into();
            Some(ServerMessage::Document(DocumentEvent::Conflict {
                buffer_id: 7,
                sequence,
                current,
            }))
        }
        ClientMessage::Resize { .. } => {
            // Deliver the minibuffer with the conflict still displayed. Resize does not clear it.
            let (subscription_id, generation) = pending_prompt.take()?;
            Some(update(
                subscription_id,
                generation,
                PluginOutput {
                    requests: vec![HostRequest::MiniBuffer {
                        purpose: "create".into(),
                        text: String::new(),
                        key_scope: None,
                        confirmation: None,
                    }],
                    ..Default::default()
                },
                None,
            ))
        }
        ClientMessage::Command {
            request_id,
            server_instance,
            action: CommandAction::PagePrompt { .. },
        } => Some(completed(request_id, server_instance)),
        message if exit_target(&message).is_some() => {
            assert!(
                reply_phase.load(Ordering::SeqCst) >= resolve_conflict_at,
                "exit was sent before the user resolved the document conflict"
            );
            None
        }
        _ => None,
    });
    let output = stream.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events,
        wait_at: None,
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(
        ScriptedFactory(VecDeque::from([stream])),
        &[],
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();

    let rendered = String::from_utf8_lossy(&terminal.output);
    let (before_typing, _) = rendered
        .split_once("create: new.txt")
        .expect("the page's create prompt did not display the entered name");
    let (_, after_conflict) = before_typing
        .split_once("Document 7 changed in the workspace")
        .expect("the document conflict notice was not displayed");
    assert!(
        after_conflict.contains("create: "),
        "the page's create prompt was hidden until typing began"
    );
    let messages = messages(&output);
    let (submitted, request, subscription) = messages
        .iter()
        .enumerate()
        .find_map(|(index, message)| match message {
            ClientMessage::Command {
                request_id,
                action:
                    CommandAction::PagePrompt {
                        subscription_id,
                        input,
                        purpose,
                        text,
                        cancelled: false,
                        ..
                    },
                ..
            } if purpose == "create" && text == "new.txt" => {
                assert_eq!(input.selection.primary.as_deref(), Some("first.txt"));
                Some((index, *request_id, *subscription_id))
            }
            _ => None,
        })
        .expect("the page's create prompt was not submitted");

    let subsequent_actions = messages[submitted + 1..]
        .iter()
        .filter_map(|message| match message {
            ClientMessage::ResultAck { request_id, .. } if *request_id == request => Some("ack"),
            ClientMessage::Resource {
                request:
                    ResourceRequest::Update {
                        subscription_id,
                        event: ResourceEvent::Select { .. },
                        input,
                    },
            } if *subscription_id == subscription => {
                assert_eq!(input.selection.primary.as_deref(), Some("second.txt"));
                Some("select")
            }
            ClientMessage::Resource {
                request: ResourceRequest::Release { subscription_id },
            } if *subscription_id == subscription => Some("release"),
            message if exit_target(message).is_some() => Some("exit"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(subsequent_actions, ["ack", "select", "release", "exit"]);
}

#[test]
fn exit_waits_for_git_staging_to_complete_across_reconnect() {
    let phase = Arc::new(Phase::default());
    let state = view_state(
        BufferKind::Data {
            name: "review".into(),
        },
        ViewContent::Items { rows: Vec::new() },
        &["global", "data", "review"],
    );
    let mut registry = Registry::<()>::default();
    registry.register_action(
        "git",
        "",
        CommandImplementation::Request(HostRequest::OpenBuffer {
            kind: "git".into(),
            argument: serde_json::json!({}),
        }),
    );
    registry.register_action(
        "git.stage",
        "",
        CommandImplementation::BufferEvent {
            kind: "git".into(),
            event: "stage".into(),
        },
    );
    registry.register_command("exit", "", CommandImplementation::Native);
    for (scope, mode, key, command) in [
        ("global", "normal", "g", "git"),
        ("git", "normal", "s", "git.stage"),
        ("global", "normal", ":", "command-prompt"),
        ("global", "prompt", "Enter", "prompt.submit"),
    ] {
        registry.binding(scope, mode, &[key], command);
    }
    for command in ["command-prompt", "prompt.submit"] {
        registry.register_action(command, "", CommandImplementation::Native);
    }
    let bindings = registry.binding_catalog();

    let mut events = VecDeque::from([
        key(KeyCode::Char('g')),
        None,
        key(KeyCode::Char('s')),
        None,
        key(KeyCode::Char(':')),
        Some(Event::Paste("exit".into())),
        key(KeyCode::Enter),
        None,
        None,
    ]);
    let disconnect_at = events.len();
    events.extend([None, None, None, None, None]);
    let mut first = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![
                attached(),
                snapshot_with_bindings(1, state.clone(), bindings.clone()),
            ],
        )],
        Some(disconnect_at),
    );
    let stage_request = Arc::new(Mutex::new(None));
    let first_request = stage_request.clone();
    first.reply = Box::new(move |message| match message {
        ClientMessage::Resource {
            request:
                ResourceRequest::Subscribe {
                    subscription_id,
                    input,
                    source,
                    ..
                },
        } if source.kind == "git" => Some(ServerMessage::Resource {
            update: ResourceUpdate {
                cache: Default::default(),
                kind: Default::default(),
                subscription_id,
                generation: input.generation,
                document_scoped: false,
                output: PluginOutput {
                    display: Some(Display {
                        rows: vec![DisplayRow {
                            id: "file:main.rs".into(),
                            text: "main.rs".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    selection: Some(ItemSelection {
                        primary: Some("file:main.rs".into()),
                        selected: vec!["file:main.rs".into()],
                        anchor: Some("file:main.rs".into()),
                    }),
                    ..Default::default()
                },
                document: None,
                patch_revision: None,
                metadata: Some(PageMetadata {
                    view_id: 20,
                    buffer_id: 21,
                    options: serde_json::json!({}),
                }),
            },
        }),
        ClientMessage::Command {
            request_id,
            server_instance,
            action:
                CommandAction::Invoke {
                    target: CommandTarget::Page { input, .. },
                    name,
                    ..
                },
        } if name == "git.stage" => {
            assert_eq!(input.selection.selected, ["file:main.rs"]);
            assert!(first_request.lock().unwrap().replace(request_id).is_none());
            Some(ServerMessage::CommandResult {
                request_id,
                server_instance,
                result: OperationResult::Pending,
            })
        }
        _ => None,
    });
    let first_output = first.output.clone();

    let mut second = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                disconnect_at,
                vec![attached(), snapshot_with_bindings(2, state, bindings)],
            ),
            (events.len(), vec![ServerMessage::Exited]),
        ],
        None,
    );
    let resumed_request = stage_request.clone();
    second.reply = Box::new(move |message| match message {
        ClientMessage::QueryOperation {
            request_id,
            server_instance,
        } if *resumed_request.lock().unwrap() == Some(request_id) => {
            Some(ServerMessage::CommandResult {
                request_id,
                server_instance,
                result: OperationResult::Completed {
                    outcome: CommandOutcome::Applied,
                    state_revision: 2,
                },
            })
        }
        _ => None,
    });
    let second_output = second.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events,
        wait_at: Some(disconnect_at),
        output: Vec::new(),
        mouse: false,
    };
    run_connection_with_devices(
        ScriptedFactory(VecDeque::from([first, second])),
        &[],
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();

    let request = stage_request
        .lock()
        .unwrap()
        .expect("the Git page did not submit staging");
    assert!(
        messages(&first_output)
            .iter()
            .all(|message| exit_target(message).is_none())
    );
    let resumed = messages(&second_output);
    let actions = resumed
        .iter()
        .filter_map(|message| match message {
            ClientMessage::QueryOperation { request_id, .. } if *request_id == request => {
                Some("query")
            }
            ClientMessage::ResultAck { request_id, .. } if *request_id == request => Some("ack"),
            ClientMessage::Command {
                action: CommandAction::Invoke { name, .. },
                ..
            } if name == "git.stage" => Some("stage"),
            message => exit_target(message).map(|target| {
                assert_eq!(
                    *target,
                    CommandTarget::View {
                        view_id: 3,
                        buffer_id: 7
                    }
                );
                "exit"
            }),
        })
        .collect::<Vec<_>>();
    assert_eq!(actions, ["query", "ack", "exit"]);
}
