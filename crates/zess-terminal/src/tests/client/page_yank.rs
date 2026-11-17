use super::*;

#[test]
fn page_yank_uses_the_register_only_for_the_next_command() {
    use zess_plugin::{HostRequest, ItemSelection, PluginOutput};
    use zess_protocol::resource::ResourceUpdate;

    let state = view_state(
        BufferKind::Text { path: None },
        ViewContent::Text { lines: Vec::new() },
        &["global", "text"],
    );
    let mut registry = Registry::<()>::default();
    registry.buffer_command("files.yank-content", "", "files", "yank-content");
    registry.binding("files", "normal", &["y"], "files.yank-content");
    for (key, command) in [
        ("\"", "data.select-register"),
        ("j", "data.move-down"),
        ("2", "data.count-2"),
    ] {
        registry.register_action(command, "", CommandImplementation::Native);
        registry.binding("files", "normal", &[key], command);
    }
    let mut workbench = workbench::Workbench::default();
    workbench.editor.set_catalog(registry.binding_catalog());
    workbench
        .apply_request(
            HostRequest::OpenBuffer {
                kind: "files".into(),
                argument: serde_json::Value::Null,
            },
            &state,
        )
        .unwrap();
    workbench
        .apply_resource(
            ResourceUpdate {
                cache: Default::default(),
                kind: Default::default(),
                document_scoped: false,
                subscription_id: workbench.pages.subscription().unwrap(),
                generation: 0,
                output: PluginOutput {
                    display: Some(zess_ui::Display {
                        rows: ["note.txt", "other.txt"]
                            .into_iter()
                            .map(|path| zess_ui::DisplayRow {
                                id: path.into(),
                                text: path.into(),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    }),
                    selection: Some(ItemSelection {
                        primary: Some("note.txt".into()),
                        selected: vec!["note.txt".into()],
                        anchor: Some("note.txt".into()),
                    }),
                    ..Default::default()
                },
                document: None,
                patch_revision: None,
                metadata: None,
            },
            &state,
        )
        .unwrap();

    for (prefix, expected_path, expected_register) in [
        (&["\"", "a", "j"][..], "other.txt", None),
        (&["\"", "a"][..], "other.txt", Some('a')),
        (&["\"", "a", "2"][..], "other.txt", Some('a')),
        (&[][..], "other.txt", None),
    ] {
        for key in prefix {
            workbench.key(key, &state, 80).unwrap();
        }
        let effects = workbench.key("y", &state, 80).unwrap();
        let [
            workbench::Effect::Command(CommandAction::Invoke {
                target: CommandTarget::Page { input, .. },
                name,
                ..
            }),
        ] = effects.as_slice()
        else {
            panic!("yank did not invoke the page command");
        };
        assert_eq!(name, "files.yank-content");
        assert_eq!(input.selection.selected, [expected_path]);
        assert_eq!(input.register, expected_register, "{prefix:?}");
    }
}

#[test]
fn a_completed_page_yank_reaches_the_register_after_focus_changes_and_applies_once() {
    use zess_plugin::{HostRequest, ItemSelection, PluginOutput};
    use zess_protocol::{
        CommandOutcome,
        resource::{PageMetadata, ResourceRequest, ResourceUpdate},
    };

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
        "files.yank-content",
        "",
        CommandImplementation::BufferEvent {
            kind: "files".into(),
            event: "yank-content".into(),
        },
    );
    registry.register_command(
        "replace-register",
        "",
        CommandImplementation::Request(HostRequest::Yank {
            values: vec!["newer".into()],
            register: Some('a'),
        }),
    );
    for (scope, key, command) in [
        ("global", "o", "files"),
        ("files", "y", "files.yank-content"),
        ("files", "Esc", "data.escape"),
        ("files", "\"", "data.select-register"),
        ("text", "\"", "text.select-register"),
        ("text", "p", "text.paste-after"),
        ("text", "x", "replace-register"),
    ] {
        registry.binding(scope, "normal", &[key], command);
    }
    for command in [
        "data.escape",
        "data.select-register",
        "text.select-register",
        "text.paste-after",
    ] {
        registry.register_action(command, "", CommandImplementation::Native);
    }
    let receipt = |request_id, server_instance| ServerMessage::CommandResult {
        request_id,
        server_instance,
        result: OperationResult::Completed {
            outcome: CommandOutcome::Yanked {
                values: vec!["copied".into()],
                register: 'a',
            },
            state_revision: 1,
        },
    };
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![
                attached(),
                snapshot_with_bindings(1, state, registry.binding_catalog()),
                document_contents(),
            ],
        )],
        None,
    );
    let mut pending_yank = None;
    let mut pasted = false;
    stream.reply = Box::new(move |message| match message {
        ClientMessage::Resource {
            request:
                ResourceRequest::Subscribe {
                    subscription_id,
                    input,
                    ..
                },
        } => Some(ServerMessage::Resource {
            update: ResourceUpdate {
                cache: Default::default(),
                kind: Default::default(),
                document_scoped: false,
                subscription_id,
                generation: input.generation,
                output: PluginOutput {
                    display: Some(zess_ui::Display {
                        rows: vec![zess_ui::DisplayRow {
                            id: "note.txt".into(),
                            text: "note.txt".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    selection: Some(ItemSelection {
                        primary: Some("note.txt".into()),
                        selected: vec!["note.txt".into()],
                        anchor: Some("note.txt".into()),
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
                    target:
                        CommandTarget::Page {
                            subscription_id,
                            input,
                        },
                    name,
                    ..
                },
        } if name == "files.yank-content" => {
            assert_eq!(input.selection.selected, ["note.txt"]);
            assert_eq!(input.selection.primary.as_deref(), Some("note.txt"));
            assert_eq!(input.register, Some('a'));
            pending_yank = Some((subscription_id, request_id, server_instance.clone()));
            Some(ServerMessage::CommandResult {
                request_id,
                server_instance,
                result: OperationResult::Pending,
            })
        }
        ClientMessage::Resource {
            request: ResourceRequest::Release { subscription_id },
        } => {
            let (owner, request_id, server_instance) = pending_yank.as_ref()?;
            assert_eq!(subscription_id, *owner);
            Some(receipt(*request_id, server_instance.clone()))
        }
        ClientMessage::Resize { .. } => {
            let (_, request_id, server_instance) = pending_yank.as_ref()?;
            Some(receipt(*request_id, server_instance.clone()))
        }
        ClientMessage::Document(DocumentRequest::Edit {
            buffer_id,
            sequence,
            base_version,
            ..
        }) => {
            if pasted {
                return Some(ServerMessage::Exited);
            }
            pasted = true;
            Some(ServerMessage::Document(DocumentEvent::Applied {
                buffer_id,
                sequence,
                version: base_version + 1,
                modified: true,
            }))
        }
        ClientMessage::Document(DocumentRequest::History {
            buffer_id,
            sequence,
            base_version,
            ..
        }) => Some(ServerMessage::Document(DocumentEvent::Applied {
            buffer_id,
            sequence,
            version: base_version,
            modified: true,
        })),
        _ => None,
    });
    let output = stream.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('o')),
            key(KeyCode::Char('"')),
            key(KeyCode::Char('a')),
            key(KeyCode::Char('y')),
            key(KeyCode::Esc),
            None,
            key(KeyCode::Char('"')),
            key(KeyCode::Char('a')),
            key(KeyCode::Char('p')),
            key(KeyCode::Char('x')),
            Some(Event::Resize(81, 24)),
            key(KeyCode::Char('"')),
            key(KeyCode::Char('a')),
            key(KeyCode::Char('p')),
            None,
        ]),
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
    .unwrap_or_else(|error| {
        panic!(
            "{error:#}\nClient sent:\n{}",
            String::from_utf8_lossy(&output.lock().unwrap())
        )
    });

    let messages = messages(&output);
    let (index, subscription) = messages
        .iter()
        .enumerate()
        .find_map(|(index, message)| match message {
            ClientMessage::Command {
                request_id: RequestId(1),
                action:
                    CommandAction::Invoke {
                        target:
                            CommandTarget::Page {
                                subscription_id, ..
                            },
                        name,
                        ..
                    },
                ..
            } if name == "files.yank-content" => Some((index, *subscription_id)),
            _ => None,
        })
        .unwrap();
    assert!(messages[index + 1..].iter().any(|message| matches!(
        message,
        ClientMessage::Resource {
            request: ResourceRequest::Release { subscription_id },
        } if *subscription_id == subscription
    )));
    let pasted = messages
        .iter()
        .filter_map(|message| match message {
            ClientMessage::Document(DocumentRequest::Edit {
                buffer_id: 7,
                changes,
                ..
            }) => {
                let [change] = changes.as_slice() else {
                    panic!("paste did not produce one change");
                };
                Some(change.insert.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(pasted, ["copied", "newer"]);
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(
                message,
                ClientMessage::ResultAck {
                    request_id: RequestId(1),
                    ..
                }
            ))
            .count(),
        2
    );
}
