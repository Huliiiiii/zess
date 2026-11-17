use zess_protocol::{
    TextRange,
    resource::{PageId, ResourceRequest},
    workspace_view::{
        Axis, DocumentPane, DocumentPanes, DocumentView, DocumentViewState, WorkspaceView,
    },
};

use super::*;

#[test]
fn a_replayed_page_open_is_acknowledged_after_the_layer_is_saved() {
    use zess_plugin::HostRequest;
    use zess_protocol::{
        CommandOutcome,
        resource::{PageDocument, PageSource},
        workspace_view::{
            PanePages,
            pages::{PagePosition, PageWorkspace},
        },
    };

    let phase = Arc::new(Phase::default());
    let mut state = view_state(
        BufferKind::Text { path: None },
        ViewContent::Text { lines: Vec::new() },
        &["global", "text"],
    );
    let mut registry = Registry::<()>::default();
    registry.register_command(
        "files",
        "Open Files",
        CommandImplementation::Request(HostRequest::OpenBuffer {
            kind: "files".into(),
            argument: serde_json::json!({}),
        }),
    );
    registry.register_command(
        "files.edit-layer",
        "Edit in a layer",
        CommandImplementation::BufferEvent {
            kind: "files".into(),
            event: "edit-layer".into(),
        },
    );
    registry.binding("global", "normal", &["o"], "files");
    registry.binding("files", "normal", &["e"], "files.edit-layer");
    let bindings = registry.binding_catalog();
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![
                attached(),
                snapshot_with_bindings(1, state.clone(), bindings.clone()),
                document_contents(),
            ],
        )],
        Some(4),
    );
    let first_output = first.output.clone();
    let page_id = PageId {
        context_epoch: 5,
        sequence: 1,
    };
    let saved = WorkspaceView {
        documents: vec![DocumentView {
            buffer_id: 7,
            view_id: 3,
            state: DocumentViewState {
                ranges: vec![TextRange { anchor: 0, head: 0 }],
                primary: 0,
                mode: "normal".into(),
                scroll: 0,
                horizontal_scroll: 0,
            },
        }],
        text_panes: Some(DocumentPanes::new(3, 7)),
        pages: vec![PanePages {
            view_id: 3,
            pages: PageWorkspace {
                stack: vec![PagePosition {
                    page_id,
                    clone_from: None,
                    source: PageSource {
                        kind: "files".into(),
                        argument: serde_json::json!({}),
                    },
                    source_buffer: Some(7),
                    selection: Default::default(),
                    scroll: 0,
                    horizontal_scroll: 0,
                    mode: "normal".into(),
                    filter: None,
                    preview: None,
                }],
                ..Default::default()
            },
        }],
        page_watermarks: vec![page_id],
        ..Default::default()
    };

    state.buffers.push(BufferSummary {
        id: 8,
        kind: BufferKind::Text {
            path: Some("page.txt".into()),
        },
        modified: false,
    });
    let result = || ServerMessage::CommandResult {
        request_id: RequestId(1),
        server_instance: ServerInstanceId("server".into()),
        result: OperationResult::Completed {
            outcome: CommandOutcome::PageApplied {
                page_id,
                subscription_id: 1,
                navigation: Vec::new(),
                document: Some(PageDocument {
                    buffer_id: 8,
                    line: 1,
                    fullscreen: true,
                }),
            },
            state_revision: 2,
        },
    };
    let other_terminal_page = PageId {
        context_epoch: 6,
        sequence: 1,
    };
    let mut restored = saved.clone();
    restored.page_watermarks.push(other_terminal_page);
    let mut second = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                4,
                vec![
                    ServerMessage::Attached {
                        server_instance: ServerInstanceId("server".into()),
                        context_epoch: 7,
                        workspace_view: Some(restored),
                    },
                    snapshot_with_bindings(2, state, bindings),
                    result(),
                    result(),
                ],
            ),
            (8, vec![ServerMessage::WorkspaceViewSaved]),
            (11, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let second_output = second.output.clone();
    let saves = second.output.clone();
    second.workspace_view_reply = Box::new(move || {
        let messages = messages(&saves);
        let saved_layer = messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ClientMessage::SaveWorkspaceView { view } => Some(view.page_document.is_some()),
                _ => None,
            })
            .unwrap();
        (!saved_layer).then_some(ServerMessage::WorkspaceViewSaved)
    });
    let time = phase.clone();
    second.reply = Box::new(move |message| match message {
        ClientMessage::Document(DocumentRequest::Read { buffer_id: 8 }) => {
            let ServerMessage::Document(DocumentEvent::Snapshot(mut snapshot)) =
                document_contents()
            else {
                unreachable!();
            };
            snapshot.buffer_id = 8;
            snapshot.text = "first line\nsecond line\n".into();
            Some(ServerMessage::Document(DocumentEvent::Snapshot(snapshot)))
        }
        ClientMessage::ResultAck {
            request_id: RequestId(1),
            ..
        } => {
            assert!(
                time.load(Ordering::SeqCst) >= 8,
                "result acknowledged before the workspace view was saved"
            );
            None
        }
        _ => None,
    });
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            None,
            key(KeyCode::Char('o')),
            key(KeyCode::Char('e')),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ]),
        wait_at: Some(4),
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
    assert!(messages(&first_output).iter().any(
        |message| matches!(message, ClientMessage::Command {
        request_id: RequestId(1), action: CommandAction::Invoke { name, .. }, ..
    } if name == "files.edit-layer")
    ));
    assert!(messages(&first_output).iter().any(
        |message| matches!(message, ClientMessage::SaveWorkspaceView { view } if view == &saved)
    ));
    let messages = messages(&second_output);
    let saved_position = messages
        .iter()
        .find_map(|message| match message {
            ClientMessage::SaveWorkspaceView { view }
                if view.page_document.is_some_and(|layer| layer.buffer_id == 8) =>
            {
                assert!(view.page_watermarks.contains(&other_terminal_page));
                view.documents
                    .iter()
                    .find(|document| document.buffer_id == 8)
            }
            _ => None,
        })
        .expect("the replayed layer was saved with a cursor position");
    assert_eq!(
        saved_position.state.ranges,
        [TextRange {
            anchor: 11,
            head: 12
        }]
    );
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
        1
    );
}

fn complete_document_open(
    path: Option<&str>,
    replacement: Option<u64>,
    input_after_open: KeyCode,
    reconnect: bool,
) -> WorkspaceView {
    let phase = Arc::new(Phase::default());
    let mut state = view_state(
        BufferKind::Text {
            path: Some("host.txt".into()),
        },
        ViewContent::Text { lines: Vec::new() },
        &["global", "text"],
    );
    state.buffers.push(BufferSummary {
        id: 8,
        kind: BufferKind::Text {
            path: Some("other.txt".into()),
        },
        modified: false,
    });
    state.workspace.buffers.push(8);
    let mut panes = DocumentPanes::new(3, 7);
    let (_, origin) = panes.split(Axis::Vertical);
    panes.set_buffer(origin.view_id, 8);
    let saved = WorkspaceView {
        text_panes: Some(panes),
        ..Default::default()
    };
    let mut registry = Registry::<()>::default();
    let (name, implementation) = if let Some(path) = path {
        (
            "open",
            CommandImplementation::Request(zess_plugin::HostRequest::Action {
                name: "open".into(),
                argument: serde_json::json!({"argument": path}),
            }),
        )
    } else {
        ("new", CommandImplementation::Native)
    };
    registry.register_action(name, "Open document", implementation);
    registry.register_action(
        "focus-next",
        "Focus next pane",
        CommandImplementation::Native,
    );
    registry.register_action("quit", "Close pane", CommandImplementation::Native);
    registry.binding("global", "normal", &["o"], name);
    registry.binding("global", "normal", &["f"], "focus-next");
    registry.binding("global", "normal", &["q"], "quit");
    let bindings = registry.binding_catalog();
    let initial = vec![
        ServerMessage::Attached {
            server_instance: ServerInstanceId("server".into()),
            context_epoch: 5,
            workspace_view: Some(saved.clone()),
        },
        snapshot_with_bindings(1, state.clone(), bindings.clone()),
        document_contents(),
    ];
    let result = ServerMessage::CommandResult {
        request_id: RequestId(1),
        server_instance: ServerInstanceId("server".into()),
        result: OperationResult::Completed {
            outcome: if let Some(buffer_id) = replacement {
                zess_protocol::CommandOutcome::DocumentOpened { buffer_id }
            } else {
                zess_protocol::CommandOutcome::Failed {
                    message: "open denied".into(),
                }
            },
            state_revision: 2,
        },
    };
    if replacement == Some(9) {
        state.buffers.push(BufferSummary {
            id: 9,
            kind: BufferKind::Text { path: None },
            modified: false,
        });
    }
    let snapshot = snapshot_with_bindings(2, state, bindings);
    let mut terminal = ScriptedTerminal {
        phase: phase.clone(),
        events: VecDeque::from([
            None,
            key(KeyCode::Char('o')),
            key(input_after_open),
            None,
            None,
            None,
            None,
        ]),
        wait_at: reconnect.then_some(4),
        output: Vec::new(),
        mouse: false,
    };
    let mut streams = VecDeque::new();
    let first_output;
    let completion_output;
    if reconnect {
        let first = ScriptedStream::new(phase.clone(), vec![(0, initial)], Some(4));
        first_output = first.output.clone();
        streams.push_back(first);
        let mut second = ScriptedStream::new(
            phase,
            vec![
                (
                    4,
                    vec![
                        ServerMessage::Attached {
                            server_instance: ServerInstanceId("server".into()),
                            context_epoch: 5,
                            workspace_view: Some(saved),
                        },
                        snapshot,
                    ],
                ),
                (7, vec![ServerMessage::Exited]),
            ],
            None,
        );
        let mut result = Some(result);
        second.reply = Box::new(move |message| {
            if matches!(
                message,
                ClientMessage::QueryOperation {
                    request_id: RequestId(1),
                    ..
                }
            ) {
                result.take()
            } else {
                None
            }
        });
        completion_output = second.output.clone();
        streams.push_back(second);
    } else {
        let stream = ScriptedStream::new(
            phase,
            vec![
                (0, initial),
                (4, vec![snapshot, result]),
                (7, vec![ServerMessage::Exited]),
            ],
            None,
        );
        completion_output = stream.output.clone();
        first_output = stream.output.clone();
        streams.push_back(stream);
    }
    run_connection_with_devices(
        ScriptedFactory(streams),
        &[],
        &mut terminal,
        &mut UnusedClipboard,
    )
    .unwrap();

    assert!(messages(&first_output).into_iter().any(|message| matches!(
        message,
        ClientMessage::Command { request_id: RequestId(1), action: CommandAction::OpenDocument { path: actual }, .. }
            if actual.as_deref() == path
    )));
    if replacement.is_none() {
        assert!(String::from_utf8_lossy(&terminal.output).contains("open denied"));
    }
    messages(&completion_output)
        .into_iter()
        .filter_map(|message| match message {
            ClientMessage::SaveWorkspaceView { view } => Some(view),
            _ => None,
        })
        .next_back()
        .unwrap()
}

#[test]
fn opening_the_host_document_replaces_the_pane_that_requested_it() {
    let pane = complete_document_open(Some("host.txt"), Some(7), KeyCode::Char('f'), false)
        .text_panes
        .unwrap();
    assert_eq!(
        pane.active(),
        DocumentPane {
            view_id: 3,
            buffer_id: 7
        }
    );
    assert_eq!(
        pane.panes()[1],
        DocumentPane {
            view_id: 4,
            buffer_id: 7
        }
    );
}

#[test]
fn reconnect_applies_a_new_document_to_the_pane_that_requested_it() {
    let pane = complete_document_open(None, Some(9), KeyCode::Char('f'), true)
        .text_panes
        .unwrap();
    assert_eq!(
        pane.active(),
        DocumentPane {
            view_id: 3,
            buffer_id: 7
        }
    );
    assert_eq!(
        pane.panes()[1],
        DocumentPane {
            view_id: 4,
            buffer_id: 9
        }
    );
}

#[test]
fn opening_a_document_does_not_replace_another_pane_after_the_origin_closes() {
    let pane = complete_document_open(Some("other.txt"), Some(8), KeyCode::Char('q'), false)
        .text_panes
        .unwrap();
    assert_eq!(
        pane.panes(),
        [DocumentPane {
            view_id: 3,
            buffer_id: 7
        }]
    );
}

#[test]
fn opening_failure_leaves_both_panes_on_their_documents() {
    let pane = complete_document_open(Some("bad.txt"), None, KeyCode::Char('f'), false)
        .text_panes
        .unwrap();
    assert_eq!(
        pane.active(),
        DocumentPane {
            view_id: 3,
            buffer_id: 7
        }
    );
    assert_eq!(
        pane.panes()[1],
        DocumentPane {
            view_id: 4,
            buffer_id: 8
        }
    );
}

#[test]
fn quitting_the_new_scratch_exits_after_replacing_the_unused_default_scratch() {
    let phase = Arc::new(Phase::default());
    let state = view_state(
        BufferKind::Text { path: None },
        ViewContent::Text { lines: Vec::new() },
        &["global", "text"],
    );
    let mut registry = Registry::<()>::default();
    registry.register_action("new", "New scratch", CommandImplementation::Native);
    registry.register_action("quit", "Quit", CommandImplementation::Native);
    registry.binding("global", "normal", &["n"], "new");
    registry.binding("global", "normal", &["q"], "quit");
    let bindings = registry.binding_catalog();
    let initial = snapshot_with_bindings(1, state.clone(), bindings.clone());
    let mut state = state;
    state.buffers.push(BufferSummary {
        id: 8,
        kind: BufferKind::Text { path: None },
        modified: false,
    });
    state.workspace.buffers.push(8);
    let opened = snapshot_with_bindings(2, state.clone(), bindings.clone());
    state.buffers.remove(0);
    state.workspace = ClientWorkspace {
        buffers: vec![8],
        current: Some(8),
    };
    state.views[0].buffer_id = 8;
    state.layout[0].buffer_id = 8;
    let discarded = snapshot_with_bindings(3, state.clone(), bindings);
    state.client_actions.push(zess_protocol::ClientAction {
        id: 5,
        kind: zess_protocol::ClientActionKind::Close {
            buffer_id: 8,
            force: false,
        },
    });
    let completed = |id, outcome| ServerMessage::CommandResult {
        request_id: RequestId(id),
        server_instance: ServerInstanceId("server".into()),
        result: OperationResult::Completed {
            outcome,
            state_revision: id + 1,
        },
    };
    let stream = ScriptedStream::new(
        phase.clone(),
        vec![
            (0, vec![attached(), initial, document_contents()]),
            (
                3,
                vec![
                    opened,
                    completed(
                        1,
                        zess_protocol::CommandOutcome::DocumentOpened { buffer_id: 8 },
                    ),
                ],
            ),
            (
                5,
                vec![
                    discarded,
                    completed(
                        2,
                        zess_protocol::CommandOutcome::ScratchDiscarded {
                            buffer_id: 7,
                            replacement: 8,
                        },
                    ),
                ],
            ),
            (
                7,
                vec![
                    snapshot(4, state),
                    completed(3, zess_protocol::CommandOutcome::Applied),
                ],
            ),
            (9, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let output = stream.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            None,
            key(KeyCode::Char('n')),
            None,
            None,
            None,
            key(KeyCode::Char('q')),
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
    let commands: Vec<_> = messages(&output)
        .into_iter()
        .filter_map(|message| match message {
            ClientMessage::Command { action, .. } => Some(action),
            _ => None,
        })
        .collect();
    assert!(matches!(
        commands.as_slice(),
        [CommandAction::OpenDocument { path: None }, CommandAction::DiscardScratch { buffer_id: 7, replacement: 8 }, CommandAction::Request { request: zess_plugin::HostRequest::Action { name, ..}, .. }, CommandAction::ClientExit { buffer_id: 8, .. }] if name == "quit"
    ));
}

#[test]
fn reconnect_preserves_the_latest_cursor_position_after_a_save_confirmation_is_lost() {
    let phase = Arc::new(Phase::default());
    let mut state = view_state(
        BufferKind::Text { path: None },
        ViewContent::Text { lines: Vec::new() },
        &["global", "text"],
    );
    state.views[0].selection = ViewSelection::Text {
        ranges: vec![TextRange { anchor: 0, head: 1 }],
        primary: 0,
    };
    let mut registry = Registry::<()>::default();
    for (key, command, description) in [
        ("Left", "text.move-left", "Move left"),
        ("Right", "text.move-right", "Move right"),
    ] {
        registry.register_action(command, description, CommandImplementation::Native);
        registry.binding("text", "normal", &[key], command);
    }
    let bindings = registry.binding_catalog();
    let contents = || {
        let ServerMessage::Document(DocumentEvent::Snapshot(mut snapshot)) = document_contents()
        else {
            unreachable!();
        };
        snapshot.text = "abc".into();
        ServerMessage::Document(DocumentEvent::Snapshot(snapshot))
    };
    let mut first = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![
                attached(),
                snapshot_with_bindings(1, state.clone(), bindings.clone()),
                contents(),
            ],
        )],
        Some(3),
    );
    let mut confirmations = [ServerMessage::WorkspaceViewSaved].into_iter();
    first.workspace_view_reply = Box::new(move || confirmations.next());
    let first_output = first.output.clone();

    let saved = WorkspaceView {
        documents: vec![DocumentView {
            buffer_id: 7,
            view_id: 3,
            state: DocumentViewState {
                ranges: vec![TextRange { anchor: 1, head: 2 }],
                primary: 0,
                mode: "normal".into(),
                scroll: 0,
                horizontal_scroll: 0,
            },
        }],
        text_panes: Some(DocumentPanes::new(3, 7)),
        ..Default::default()
    };
    let second = ScriptedStream::new(
        phase.clone(),
        vec![
            (
                4,
                vec![
                    ServerMessage::Attached {
                        server_instance: ServerInstanceId("server".into()),
                        context_epoch: 6,
                        workspace_view: Some(saved.clone()),
                    },
                    snapshot_with_bindings(2, state, bindings),
                    contents(),
                ],
            ),
            (6, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let second_output = second.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            None,
            key(KeyCode::Right),
            key(KeyCode::Left),
            None,
            None,
            None,
        ]),
        wait_at: Some(3),
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

    let saved_views = |output| {
        messages(output)
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::SaveWorkspaceView { view } => Some(view),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mut original = saved.clone();
    original.documents[0].state.ranges = vec![TextRange { anchor: 0, head: 1 }];
    assert_eq!(saved_views(&first_output), [original.clone(), saved]);
    assert_eq!(saved_views(&second_output), [original]);
}

#[test]
fn reconnect_preserves_a_queued_pages_identity_and_allocates_in_the_new_epoch() {
    let phase = Arc::new(Phase::default());
    let mut registry = Registry::<()>::default();
    for (key, kind) in [("f", "files"), ("g", "git")] {
        registry.register_action(
            kind,
            kind,
            CommandImplementation::Request(zess_plugin::HostRequest::OpenBuffer {
                kind: kind.into(),
                argument: serde_json::Value::Null,
            }),
        );
        registry.binding("global", "normal", &[key], kind);
    }
    let state = view_state(
        BufferKind::Data {
            name: "review".into(),
        },
        ViewContent::Items { rows: Vec::new() },
        &["global", "data", "review"],
    );
    let bindings = registry.binding_catalog();
    let first = ScriptedStream::new(
        phase.clone(),
        vec![(
            0,
            vec![
                attached(),
                snapshot_with_bindings(1, state.clone(), bindings.clone()),
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
                    ServerMessage::Attached {
                        server_instance: ServerInstanceId("server".into()),
                        context_epoch: 6,
                        workspace_view: Some(WorkspaceView::default()),
                    },
                    snapshot_with_bindings(2, state, bindings),
                ],
            ),
            (6, vec![ServerMessage::Exited]),
        ],
        None,
    );
    let second_output = second.output.clone();
    let mut terminal = ScriptedTerminal {
        phase,
        events: VecDeque::from([
            None,
            key(KeyCode::Char('f')),
            None,
            key(KeyCode::Char('g')),
            None,
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

    let subscriptions = |output| {
        messages(output)
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Resource {
                    request:
                        ResourceRequest::Subscribe {
                            page_id, source, ..
                        },
                } => Some((source.kind, page_id)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(subscriptions(&first_output).is_empty());
    assert_eq!(
        subscriptions(&second_output),
        [
            (
                "files".into(),
                PageId {
                    context_epoch: 5,
                    sequence: 1,
                },
            ),
            (
                "git".into(),
                PageId {
                    context_epoch: 6,
                    sequence: 1,
                },
            ),
        ]
    );
}
