use zess_protocol::CommandOutcome;

use super::*;

fn file_contents(buffer_id: u64, version: u64, text: &str) -> ServerMessage {
    ServerMessage::Document(DocumentEvent::Snapshot(DocumentSnapshot {
        buffer_id,
        version,
        modified: false,
        readonly: false,
        language: None,
        path: Some(format!("{buffer_id}.txt")),
        history: Default::default(),
        text: text.into(),
    }))
}

fn file_state() -> SessionSnapshot {
    view_state(
        BufferKind::Text {
            path: Some("7.txt".into()),
        },
        ViewContent::Text { lines: Vec::new() },
        &["global", "text"],
    )
}

fn two_file_state() -> SessionSnapshot {
    let mut state = file_state();
    state.buffers.push(BufferSummary {
        id: 8,
        kind: BufferKind::Text {
            path: Some("8.txt".into()),
        },
        modified: false,
    });
    state.workspace.buffers.push(8);
    state
}

fn reload_bindings() -> Registry<()> {
    let mut registry = Registry::default();
    for (scope, mode, key, command) in [
        ("text", "normal", "i", "text.insert"),
        ("text", "insert", "Esc", "text.escape"),
        ("text", "normal", ":", "command-prompt"),
        ("text", "normal", "r", "reload"),
        ("global", "prompt", "Enter", "prompt.submit"),
        ("text", "normal", "]", "buffer-next"),
        ("text", "normal", "[", "buffer-previous"),
    ] {
        registry.register_action(command, "", CommandImplementation::Native);
        registry.binding(scope, mode, &[key], command);
    }
    registry.register_command_line("reload", "reload");
    registry
}

fn applied(buffer_id: u64, sequence: u64, version: u64) -> ServerMessage {
    ServerMessage::Document(DocumentEvent::Applied {
        buffer_id,
        sequence,
        version,
        modified: true,
    })
}

#[test]
fn reload_waits_for_earlier_edits_and_pauses_input_until_the_reloaded_document_arrives() {
    let phase = Arc::new(Phase::default());
    let state = two_file_state();
    let mut registry = reload_bindings();
    registry.register_command("fixture.other", "", CommandImplementation::External(()));
    registry.binding("text", "normal", &["x"], "fixture.other");
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                0,
                vec![
                    attached(),
                    snapshot_with_bindings(1, state, registry.binding_catalog()),
                    file_contents(7, 1, "original"),
                ],
            ),
            (14, vec![applied(7, 1, 2)]),
            (
                19,
                vec![ServerMessage::CommandResult {
                    request_id: RequestId(2),
                    server_instance: ServerInstanceId("server".into()),
                    result: OperationResult::Completed {
                        outcome: CommandOutcome::Applied,
                        state_revision: 1,
                    },
                }],
            ),
            (23, vec![file_contents(7, 3, "disk")]),
            (28, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let server_phase = phase.clone();
    stream.reply = Box::new(move |message| match message {
        ClientMessage::Document(DocumentRequest::Read { buffer_id: 8 }) => {
            Some(file_contents(8, 1, "other buffer"))
        }
        ClientMessage::Document(DocumentRequest::History {
            buffer_id,
            sequence,
            base_version,
            ..
        }) => Some(applied(buffer_id, sequence, base_version)),
        ClientMessage::Command {
            action: CommandAction::Request { .. },
            ..
        } => {
            assert!(server_phase.load(Ordering::SeqCst) >= 14);
            Some(file_contents(7, 2, "Aoriginal"))
        }
        ClientMessage::Command {
            action: CommandAction::Invoke { .. },
            ..
        } => {
            assert!(server_phase.load(Ordering::SeqCst) < 14);
            None
        }
        _ => None,
    });
    let output = stream.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('i')),
            key(KeyCode::Char('A')),
            key(KeyCode::Esc),
            key(KeyCode::Char(':')),
            Some(Event::Paste("reload".into())),
            key(KeyCode::Enter),
            key(KeyCode::Char('i')),
            key(KeyCode::Char('B')),
            key(KeyCode::Esc),
            key(KeyCode::Char(']')),
            key(KeyCode::Char('x')),
            key(KeyCode::Char('[')),
            None,
            None, // The first edit is acknowledged.
            None,
            key(KeyCode::Char('i')),
            key(KeyCode::Char('B')),
            key(KeyCode::Esc),
            None, // The reload result arrives before its snapshot.
            key(KeyCode::Char('i')),
            key(KeyCode::Char('B')),
            key(KeyCode::Esc),
            None, // The refreshed document arrives.
            key(KeyCode::Char('i')),
            key(KeyCode::Char('C')),
            key(KeyCode::Esc),
            None,
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
    .unwrap();

    let display = String::from_utf8(terminal.output).unwrap();
    assert!(display.contains("Reload pending. Editing and saving are paused."));

    let messages = messages(&output);
    let ordered: Vec<_> = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                ClientMessage::Command { .. }
                    | ClientMessage::Document(DocumentRequest::Edit { .. })
                    | ClientMessage::Document(DocumentRequest::History { .. })
            )
        })
        .collect();
    assert!(
        matches!(
            ordered.as_slice(),
            [
                ClientMessage::Document(DocumentRequest::Edit { buffer_id: 7, base_version: 1, changes: before, .. }),
                ClientMessage::Command { action: CommandAction::Invoke { target: CommandTarget::Document { buffer_id: 8 }, name: other, .. }, .. },
                ClientMessage::Document(DocumentRequest::History { buffer_id: 7, base_version: 2, .. }),
                ClientMessage::Command { action: CommandAction::Request { target: CommandTarget::Document { buffer_id: 7 }, request: HostRequest::Action { name: reload, .. } }, .. },
                ClientMessage::Document(DocumentRequest::Edit { buffer_id: 7, base_version: 3, changes: after, .. }),
            ] if before.len() == 1 && before[0].insert == "A" && other == "fixture.other"
                && reload == "reload" && after.len() == 1 && after[0].insert == "C"
        ),
        "{}",
        serde_json::to_string_pretty(&ordered).unwrap()
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(
                message,
                ClientMessage::Document(DocumentRequest::Read { buffer_id: 7 })
            ))
            .count(),
        2,
    );
}

#[test]
fn an_interrupted_reload_resumes_editing_after_its_failure_and_a_fresh_read() {
    let phase = Arc::new(Phase::default());
    let bindings = reload_bindings().binding_catalog();
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![
                attached(),
                snapshot_with_bindings(1, file_state(), bindings.clone()),
                file_contents(7, 1, "original"),
            ],
        )],
        Some(2),
    );
    let first_output = first.output.clone();
    let second = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                3,
                vec![
                    attached(),
                    snapshot_with_bindings(1, file_state(), bindings),
                    file_contents(7, 1, "original"),
                ],
            ),
            (
                7,
                vec![ServerMessage::CommandResult {
                    request_id: RequestId(1),
                    server_instance: ServerInstanceId("server".into()),
                    result: OperationResult::Completed {
                        outcome: CommandOutcome::Failed {
                            message: "read 7.txt: access denied".into(),
                        },
                        state_revision: 1,
                    },
                }],
            ),
            (8, vec![file_contents(7, 1, "original")]),
            (12, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let second_output = second.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('r')),
            None, // Transport disconnects.
            None, // The terminal reattaches.
            key(KeyCode::Char('i')),
            key(KeyCode::Char('B')),
            key(KeyCode::Esc),
            None, // The unsuccessful reload result arrives.
            None, // The authoritative text is unchanged.
            key(KeyCode::Char('i')),
            key(KeyCode::Char('C')),
            key(KeyCode::Esc),
            None,
        ]),
        wait_at: Some(2),
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

    assert!(
        String::from_utf8(terminal.output)
            .unwrap()
            .contains("read 7.txt: access denied")
    );
    let sent = messages(&first_output);
    assert!(
        sent.iter()
            .any(|message| matches!(message, ClientMessage::Command {
        request_id: RequestId(1),
        action: CommandAction::Request { request: HostRequest::Action { name, .. }, .. },
        ..
    } if name == "reload"))
    );
    let resumed = messages(&second_output);
    let ordered: Vec<_> = resumed
        .iter()
        .filter(|message| {
            matches!(
                message,
                ClientMessage::Command { .. }
                    | ClientMessage::QueryOperation { .. }
                    | ClientMessage::Document(_)
            )
        })
        .collect();
    assert!(
        matches!(ordered.as_slice(), [
        ClientMessage::QueryOperation { request_id: RequestId(1), .. },
        ClientMessage::Document(DocumentRequest::Read { buffer_id: 7 }),
        ClientMessage::Document(DocumentRequest::Read { buffer_id: 7 }),
        ClientMessage::Document(DocumentRequest::Edit {
            buffer_id: 7,
            base_version: 1,
            changes,
            ..
        }),
    ] if changes.len() == 1 && changes[0].insert == "C"),
        "{}",
        serde_json::to_string_pretty(&ordered).unwrap()
    );
}

#[test]
fn an_exit_is_dispatched_between_earlier_and_later_reloads() {
    let phase = Arc::new(Phase::default());
    let mut registry = reload_bindings();
    registry.register_action("exit", "", CommandImplementation::Native);
    registry.binding("global", "normal", &["e"], "exit");
    let mut stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                0,
                vec![
                    attached(),
                    snapshot_with_bindings(1, two_file_state(), registry.binding_catalog()),
                    file_contents(7, 1, "original"),
                ],
            ),
            (9, vec![applied(7, 1, 2)]),
            (
                13,
                vec![ServerMessage::CommandResult {
                    request_id: RequestId(1),
                    server_instance: ServerInstanceId("server".into()),
                    result: OperationResult::Completed {
                        outcome: CommandOutcome::Applied,
                        state_revision: 1,
                    },
                }],
            ),
            (14, vec![file_contents(7, 3, "disk")]),
            (17, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let server_phase = phase.clone();
    stream.reply = Box::new(move |message| match message {
        ClientMessage::Document(DocumentRequest::Read { buffer_id: 8 }) => {
            Some(file_contents(8, 1, "other buffer"))
        }
        ClientMessage::Document(DocumentRequest::History {
            buffer_id,
            sequence,
            base_version,
            ..
        }) => Some(applied(buffer_id, sequence, base_version)),
        ClientMessage::Command {
            action: CommandAction::Request { target, .. },
            ..
        } => {
            let phase = server_phase.load(Ordering::SeqCst);
            if target == (CommandTarget::Document { buffer_id: 7 }) {
                assert!((9..13).contains(&phase));
            } else {
                assert!(phase >= 14);
            }
            None
        }
        _ => None,
    });
    let output = stream.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            key(KeyCode::Char('i')),
            key(KeyCode::Char('A')),
            key(KeyCode::Esc),
            key(KeyCode::Char('r')),
            key(KeyCode::Char('e')),
            key(KeyCode::Char(']')),
            None,
            key(KeyCode::Char('r')),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
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
    .unwrap();

    let messages = messages(&output);
    let commands = messages
        .iter()
        .filter_map(|message| match message {
            ClientMessage::Command {
                action:
                    CommandAction::Request {
                        target,
                        request: HostRequest::Action { name, .. },
                    },
                ..
            } => Some((target, name.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        [
            (&CommandTarget::Document { buffer_id: 7 }, "reload"),
            (
                &CommandTarget::View {
                    view_id: 3,
                    buffer_id: 7
                },
                "exit"
            ),
            (&CommandTarget::Document { buffer_id: 8 }, "reload"),
        ]
    );
}
