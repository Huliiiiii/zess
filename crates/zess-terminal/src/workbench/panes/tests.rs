use hegel::{TestCase, generators as gs};
use zess_plugin::{CommandImplementation, PluginRegistrar, Registry};
use zess_protocol::{
    BufferSummary, ClientMessage, ClientWorkspace,
    document::{DocumentRequest, DocumentSnapshot},
    resource::{PageDocument, PageSplit, ResourceEvent, ResourceRequest},
};
use zess_ui::{Display, DisplayRow, Frame};

use super::*;

fn state() -> SessionSnapshot {
    SessionSnapshot {
        workspace: ClientWorkspace {
            current: Some(7),
            buffers: vec![7, 8],
        },
        buffers: [7, 8]
            .into_iter()
            .map(|id| BufferSummary {
                id,
                kind: BufferKind::Text { path: None },
                modified: false,
            })
            .collect(),
        editor_options: zess_plugin::EditorOptions {
            mouse: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn snapshot(buffer_id: u64, text: &str) -> DocumentEvent {
    DocumentEvent::Snapshot(DocumentSnapshot {
        buffer_id,
        text: text.into(),
        version: 1,
        modified: false,
        readonly: false,
        path: None,
        language: None,
        history: Default::default(),
    })
}

#[test]
fn a_scratch_is_kept_when_a_pane_split_during_the_open_still_displays_it() {
    let state = state();
    let mut workbench = Workbench {
        panes: Some(DocumentPanes::new(3, 7)),
        ..Default::default()
    };
    let mut registry = Registry::<()>::default();
    registry.register_action("new", "New scratch", CommandImplementation::Native);
    registry.binding("global", "normal", &["n"], "new");
    workbench.editor.set_catalog(registry.binding_catalog());
    let effects = workbench.apply_key("n", &state, 80).unwrap();
    let [
        Effect::OpenDocument {
            view_id,
            path: None,
        },
    ] = effects.as_slice()
    else {
        panic!("expected a scratch open from the original pane");
    };
    workbench.split_pane(Axis::Vertical, &state).unwrap();
    assert!(workbench.document_opened(*view_id, 8, &state).is_none());
    let panes = workbench.workspace_view(&state).text_panes.unwrap();
    assert_eq!(
        panes.active(),
        DocumentPane {
            view_id: 4,
            buffer_id: 7
        }
    );
    assert_eq!(
        panes.panes()[0],
        DocumentPane {
            view_id: 3,
            buffer_id: 8
        }
    );
}

fn publication(subscription_id: u64, generation: u64, labels: &[&str]) -> ResourceUpdate {
    ResourceUpdate {
        cache: Default::default(),
        kind: Default::default(),
        subscription_id,
        generation,
        document_scoped: false,
        metadata: None,
        document: None,
        patch_revision: None,
        output: zess_plugin::PluginOutput {
            display: Some(Display {
                rows: labels
                    .iter()
                    .map(|label| DisplayRow {
                        id: (*label).into(),
                        text: (*label).into(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            selection: Some(zess_plugin::ItemSelection {
                primary: Some(labels[0].into()),
                selected: vec![labels[0].into()],
                anchor: Some(labels[0].into()),
            }),
            ..Default::default()
        },
    }
}

fn output(frame: &Frame) -> String {
    frame
        .rows
        .iter()
        .flat_map(|row| row.spans.iter().map(|span| span.text.as_str()))
        .collect()
}

#[test]
fn reopening_a_cached_page_does_not_apply_an_open_from_its_previous_activation() {
    let state = state();
    let mut workbench = Workbench::default();
    workbench.pages.open("files", json!({}));
    let origin = workbench.pages.subscription().unwrap();
    let page_id = workbench.pages.active_page_id().unwrap();
    assert!(workbench.pages.close(false));
    workbench.pages.open("files", json!({}));
    assert_eq!(workbench.pages.active_page_id(), Some(page_id));
    assert_ne!(workbench.pages.subscription(), Some(origin));
    workbench
        .apply_page_result(
            page_id,
            origin,
            Vec::new(),
            Some(PageDocument {
                buffer_id: 8,
                line: 0,
                fullscreen: true,
            }),
            &state,
        )
        .unwrap();
    assert!(workbench.workspace_view(&state).page_document.is_none());
    assert_eq!(workbench.pages.scope(), Some("files"));
}

#[test]
fn reconnecting_clears_the_minibuffer_for_a_cancelled_discard() {
    let state = state();
    let mut workbench = Workbench::default();
    workbench.pages.open("git", json!({}));
    let origin = workbench.pages.subscription().unwrap();
    let mut update = publication(origin, 0, &["note.txt"]);
    update.output.requests.push(HostRequest::MiniBuffer {
        purpose: "Restore working tree from index: \"note.txt\". Type yes to confirm".into(),
        text: String::new(),
        key_scope: None,
        confirmation: Some("client:1".into()),
    });
    workbench.apply_resource(update, &state).unwrap();
    workbench.paste("yes", &state).unwrap();
    assert!(workbench.editor.prompt_active());

    workbench.reconnect();

    assert!(!workbench.editor.prompt_active());
    assert_eq!(workbench.pages.scope(), Some("git"));
    assert_ne!(workbench.pages.subscription(), Some(origin));
}

#[test]
fn reconnecting_preserves_an_ordinary_minibuffer_draft() {
    let state = state();
    let mut workbench = Workbench::default();
    workbench.pages.open("files", json!({}));
    let origin = workbench.pages.subscription().unwrap();
    let mut update = publication(origin, 0, &["note.txt"]);
    update.output.requests.push(HostRequest::MiniBuffer {
        purpose: "create".into(),
        text: String::new(),
        key_scope: None,
        confirmation: None,
    });
    workbench.apply_resource(update, &state).unwrap();
    workbench.paste("new.txt", &state).unwrap();

    workbench.reconnect();

    assert!(workbench.editor.prompt_active());
    assert_eq!(workbench.editor.prompt_text(), "new.txt");
}

fn split_page(state: &SessionSnapshot) -> (Workbench, u64, u64) {
    let mut workbench = Workbench::default();
    workbench.pages.open("files", json!({}));
    let source = workbench.pages.subscription().unwrap();
    let mut update = publication(source, 0, &["first item", "second item"]);
    update.output.requests.push(HostRequest::Preview {
        path: "preview.txt".into(),
        content: zess_plugin::PreviewContent::Text {
            text: "preview data".into(),
        },
        focus: false,
    });
    workbench.apply_resource(update, state).unwrap();
    workbench.render(state, 80, 24).unwrap();
    workbench
        .native("vsplit", Value::Null, state, &mut Vec::new())
        .unwrap();
    let target = workbench.pages.subscription().unwrap();
    (workbench, source, target)
}

#[test]
fn an_explicit_preview_command_can_focus_its_page_while_the_target_stays_current() {
    let state = state();
    let mut workbench = Workbench::default();
    let mut registry = Registry::<()>::default();
    registry.buffer_command("git.preview", "Preview", "git", "preview");
    for (command, mode, key) in [
        ("command-prompt", "normal", ":"),
        ("prompt.submit", "prompt", "Enter"),
        ("prompt.cancel", "prompt", "Esc"),
    ] {
        registry.register_action(command, command, CommandImplementation::Native);
        registry
            .keymap
            .map("global", mode, vec![key.into()], Some(command.into()));
    }
    workbench.editor.set_catalog(registry.binding_catalog());
    workbench.pages.open("git", json!({}));
    let subscription_id = workbench.pages.subscription().unwrap();
    let preview = |focus, item| {
        let mut update = publication(subscription_id, 0, &[item]);
        update.output.requests.push(HostRequest::Preview {
            path: "file".into(),
            content: zess_plugin::PreviewContent::Text {
                text: "file content".into(),
            },
            focus,
        });
        update
    };
    workbench
        .apply_resource(preview(false, "file"), &state)
        .unwrap();
    let request_preview = |workbench: &mut Workbench| {
        workbench.key(":", &state, 80).unwrap();
        workbench.paste("git.preview", &state).unwrap();
        let effects = workbench.key("Enter", &state, 80).unwrap();
        assert!(matches!(
            effects.as_slice(),
            [Effect::Command(CommandAction::Invoke {
                target: CommandTarget::Page { subscription_id: id, .. },
                name,
                ..
            })] if *id == subscription_id && name == "git.preview"
        ));
    };

    request_preview(&mut workbench);
    workbench
        .apply_resource(preview(true, "file"), &state)
        .unwrap();
    assert_eq!(workbench.pages.scope(), Some("preview"));

    assert!(workbench.pages.command("preview.return", 1));
    request_preview(&mut workbench);
    workbench.key(":", &state, 80).unwrap();
    workbench
        .apply_resource(preview(true, "file"), &state)
        .unwrap();
    assert_eq!(workbench.pages.scope(), Some("git"));

    workbench.key("Esc", &state, 80).unwrap();
    request_preview(&mut workbench);
    workbench
        .apply_resource(preview(true, "other"), &state)
        .unwrap();
    assert_eq!(workbench.pages.scope(), Some("git"));
}

#[test]
fn a_page_confirmation_survives_resize_without_restoring_the_old_display() {
    let state = state();
    let mut workbench = Workbench::default();
    let mut registry = Registry::<()>::default();
    registry.register_action(
        "prompt.submit",
        "Submit prompt",
        CommandImplementation::Native,
    );
    registry.keymap.map(
        "global",
        "prompt",
        vec!["Enter".into()],
        Some("prompt.submit".into()),
    );
    workbench.editor.set_catalog(registry.binding_catalog());
    workbench.resize_pages(80, 24);
    workbench.pages.open("git", json!({}));
    let origin = workbench.pages.subscription().unwrap();
    workbench
        .apply_resource(publication(origin, 0, &["current file"]), &state)
        .unwrap();

    let purpose = "Restore working tree from index: \"current file\". Type yes to confirm";
    let mut confirmation = publication(origin, 0, &["old file"]);
    confirmation.output.requests = vec![
        HostRequest::Preview {
            path: "old file".into(),
            content: zess_plugin::PreviewContent::Text {
                text: "old preview".into(),
            },
            focus: true,
        },
        HostRequest::MiniBuffer {
            purpose: purpose.into(),
            text: String::new(),
            key_scope: None,
            confirmation: Some("client:1".into()),
        },
    ];
    workbench.resize_pages(100, 24);
    workbench.apply_resource(confirmation, &state).unwrap();

    let frame = workbench.render(&state, 100, 24).unwrap();
    assert!(output(&frame).contains("current file"));
    assert!(!output(&frame).contains("old file"));
    assert!(!output(&frame).contains("old preview"));
    assert_eq!(workbench.pages.scope(), Some("git"));
    workbench.paste("yes", &state).unwrap();
    let effects = workbench.key("Enter", &state, 100).unwrap();
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Command(CommandAction::PagePrompt {
            subscription_id, input, purpose: submitted, confirmation: Some(token), text, cancelled: false,
        }) if *subscription_id == origin && submitted == purpose && text == "yes"
            && token == "client:1"
            && input.selection.primary.as_deref() == Some("current file")
    )));
}

#[test]
fn cancelling_a_page_minibuffer_preserves_the_filter() {
    let state = state();
    let mut registry = Registry::<()>::default();
    for (command, key) in [("prompt.cancel", "Esc"), ("prompt.toggle-regex", "C-r")] {
        registry.register_action(command, command, CommandImplementation::Native);
        registry
            .keymap
            .map("global", "prompt", vec![key.into()], Some(command.into()));
    }

    for (kind, purpose, draft, query_kind) in [
        ("git", "Type yes to confirm", "no", FilterKind::Plain),
        ("files", "rename", "new-name", FilterKind::Plain),
        ("files", "find", "^new-name$", FilterKind::Regex),
    ] {
        let mut workbench = Workbench::default();
        workbench.editor.set_catalog(registry.binding_catalog());
        workbench.pages.open(kind, json!({}));
        let origin = workbench.pages.subscription().unwrap();
        let filter = PageFilter {
            kind: FilterKind::Glob,
            text: "*.txt".into(),
        };
        workbench.pages.filter(Some(filter.clone()));
        workbench
            .apply_request(
                HostRequest::MiniBuffer {
                    purpose: purpose.into(),
                    text: String::new(),
                    key_scope: None,
                    confirmation: None,
                },
                &state,
            )
            .unwrap();
        if query_kind == FilterKind::Regex {
            workbench.key("C-r", &state, 80).unwrap();
        }
        workbench.pages.take_requests();
        workbench.paste(draft, &state).unwrap();
        let requests = workbench.pages.take_requests();
        assert!(matches!(
            requests.as_slice(),
            [ClientMessage::Resource {
                request: ResourceRequest::Update {
                    subscription_id,
                    input,
                    event: ResourceEvent::MiniBufferChanged {
                        purpose: prompt, text, query_kind: kind,
                    },
                },
            }] if *subscription_id == origin && input.filter.as_ref() == Some(&filter)
                && prompt == purpose && text == draft && *kind == query_kind
        ));

        let effects = workbench.key("Esc", &state, 80).unwrap();
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Command(CommandAction::PagePrompt {
                subscription_id, purpose: prompt, text, cancelled: true, ..
            }) if *subscription_id == origin && prompt == purpose && text == draft
        )));
        workbench.page_prompt_result(origin, purpose, &zess_protocol::CommandOutcome::Applied);
        workbench
            .native("data.filter", Value::Null, &state, &mut Vec::new())
            .unwrap();
        assert_eq!(workbench.editor.prompt_text(), filter.text);
        assert!(matches!(
            workbench.editor.query_kind(),
            Some(QueryKind::Glob)
        ));
    }
}

#[test]
fn a_late_confirmation_preserves_an_open_command_prompt() {
    let state = state();
    let mut workbench = Workbench::default();
    let mut registry = Registry::<()>::default();
    registry.register_action(
        "command-prompt",
        "Command prompt",
        CommandImplementation::Native,
    );
    registry.keymap.map(
        "global",
        "normal",
        vec![":".into()],
        Some("command-prompt".into()),
    );
    workbench.editor.set_catalog(registry.binding_catalog());
    workbench.resize_pages(80, 24);
    workbench.pages.open("git", json!({}));
    let origin = workbench.pages.subscription().unwrap();
    workbench.resize_pages(100, 24);
    workbench.key(":", &state, 100).unwrap();
    workbench.paste("buffers", &state).unwrap();

    let purpose = "Type yes to confirm";
    let mut update = publication(origin, 0, &["changed file"]);
    update.output.requests.push(HostRequest::MiniBuffer {
        purpose: purpose.into(),
        text: String::new(),
        key_scope: None,
        confirmation: None,
    });
    let effects = workbench.apply_resource(update, &state).unwrap();

    assert_eq!(workbench.editor.prompt_text(), "buffers");
    assert!(matches!(
        effects.as_slice(),
        [Effect::Command(CommandAction::PagePrompt {
            subscription_id, purpose: prompt, cancelled: true, ..
        })] if *subscription_id == origin && prompt == purpose
    ));
    workbench.page_prompt_result(origin, purpose, &zess_protocol::CommandOutcome::Applied);
    assert_eq!(workbench.editor.prompt_text(), "buffers");
}

#[test]
fn a_background_page_reports_its_notice_and_cancels_its_prompt() {
    let state = state();
    let (mut workbench, origin, focused) = split_page(&state);
    workbench
        .apply_request(
            HostRequest::MiniBuffer {
                purpose: "find".into(),
                text: "foreground query".into(),
                key_scope: None,
                confirmation: None,
            },
            &state,
        )
        .unwrap();
    let mut update = publication(origin, 0, &["first item", "second item"]);
    update.output.requests = vec![
        HostRequest::Message {
            text: "background refresh failed".into(),
        },
        HostRequest::MiniBuffer {
            purpose: "rename".into(),
            text: "background prompt".into(),
            key_scope: None,
            confirmation: None,
        },
    ];
    let effects = workbench.apply_resource(update, &state).unwrap();
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::Notice(text),
            Effect::Command(CommandAction::PagePrompt { subscription_id, purpose, cancelled: true, .. }),
        ] if text == "background refresh failed" && *subscription_id == origin && purpose == "rename"
    ));
    assert_eq!(workbench.pages.subscription(), Some(focused));
    assert_eq!(workbench.editor.prompt_text(), "foreground query");
}

#[test]
fn split_pages_show_the_same_rows_immediately_and_restore_independent_selections() {
    let state = state();
    let (mut workbench, source, target) = split_page(&state);
    let frame = workbench.render(&state, 80, 24).unwrap();
    assert_eq!(output(&frame).matches("first item").count(), 2);
    assert_eq!(output(&frame).matches("second item").count(), 2);
    assert_eq!(output(&frame).matches("preview data").count(), 2);

    let mut subscriptions = workbench
        .pages
        .take_requests()
        .into_iter()
        .filter_map(|message| match message {
            ClientMessage::Resource {
                request:
                    ResourceRequest::Subscribe {
                        subscription_id,
                        page_id,
                        clone_from,
                        ..
                    },
            } => Some((subscription_id, page_id, clone_from)),
            _ => None,
        });
    let original = subscriptions.next().unwrap();
    let cloned = subscriptions.next().unwrap();
    let origin = PageSplit {
        source: original.1,
        model: original.1,
    };
    assert_eq!(original.0, source);
    assert_eq!((cloned.0, cloned.2), (target, Some(origin)));
    assert_ne!(original.1, cloned.1);
    assert!(subscriptions.next().is_none());

    workbench
        .native("focus-left", Value::Null, &state, &mut Vec::new())
        .unwrap();
    assert_eq!(workbench.pages.subscription(), Some(source));
    workbench
        .mouse(
            &MouseEvent {
                kind: MouseKind::Press,
                column: 45,
                row: 1,
            },
            &state,
            80,
            24,
        )
        .unwrap();
    assert_eq!(workbench.pages.subscription(), Some(target));
    let saved = workbench.workspace_view(&state);
    let focused = saved.text_panes.as_ref().unwrap().active().view_id;
    let positions: Vec<_> = saved
        .pages
        .iter()
        .map(|pane| &pane.pages.stack[0])
        .collect();
    assert_eq!(
        positions[0].selection.primary.as_deref(),
        Some("first item")
    );
    assert_eq!(
        positions[1].selection.primary.as_deref(),
        Some("second item")
    );
    let focused = saved
        .pages
        .iter()
        .find(|pane| pane.view_id == focused)
        .unwrap();
    assert_eq!(focused.pages.stack[0].page_id, cloned.1);

    let mut restored = Workbench::default();
    let checkpoint = serde_json::to_vec(&ClientMessage::SaveWorkspaceView {
        view: saved.clone(),
    })
    .unwrap();
    let ClientMessage::SaveWorkspaceView { view } = serde_json::from_slice(&checkpoint).unwrap()
    else {
        panic!("expected a workspace checkpoint");
    };
    restored.restore(&view, &state).unwrap();
    assert_eq!(restored.workspace_view(&state).pages, saved.pages);
    let subscriptions = |workbench: &mut Workbench| {
        workbench
            .pages
            .take_requests()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Resource {
                    request:
                        ResourceRequest::Subscribe {
                            page_id,
                            clone_from,
                            ..
                        },
                } => Some((page_id, clone_from)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        subscriptions(&mut restored),
        [(cloned.1, Some(origin)), (original.1, None)]
    );

    restored.reconnect();
    assert_eq!(
        subscriptions(&mut restored),
        [(original.1, None), (cloned.1, Some(origin))]
    );
}

#[test]
fn a_document_opened_from_an_unfocused_page_uses_its_own_pane() {
    let state = state();
    let (mut workbench, source, target) = split_page(&state);
    workbench.render(&state, 80, 24).unwrap();
    let requests = workbench.pages.take_requests();
    let (page_id, generation) = requests
        .iter()
        .find_map(|message| match message {
            ClientMessage::Resource {
                request:
                    ResourceRequest::Subscribe {
                        subscription_id,
                        page_id,
                        input,
                        ..
                    },
            } if *subscription_id == source => Some((*page_id, input.generation)),
            _ => None,
        })
        .unwrap();
    assert!(
        requests
            .iter()
            .any(|message| matches!(message, ClientMessage::Resource {
        request: ResourceRequest::Update { subscription_id, input, event: ResourceEvent::Resize },
    } if *subscription_id == source && input.generation > generation))
    );
    let mut update = publication(source, generation, &["first item", "second item"]);
    update.document = Some(PageDocument {
        buffer_id: 8,
        line: 0,
        fullscreen: false,
    });
    workbench.apply_resource(update, &state).unwrap();
    workbench
        .apply_document(
            DocumentEvent::Snapshot(DocumentSnapshot {
                buffer_id: 8,
                version: 1,
                modified: false,
                readonly: false,
                language: None,
                path: None,
                text: "opened document".into(),
                history: Default::default(),
            }),
            &state,
        )
        .unwrap();
    let frame = workbench.render(&state, 80, 24).unwrap();
    assert!(output(&frame).contains("opened document"));
    assert!(output(&frame).contains("first item"));
    assert_eq!(workbench.pages.subscription(), Some(target));

    workbench
        .mouse(
            &MouseEvent {
                kind: MouseKind::Press,
                column: 5,
                row: 0,
            },
            &state,
            80,
            24,
        )
        .unwrap();
    assert_eq!(workbench.active_document(&state).unwrap().buffer_id, 8);
    workbench
        .native("quit", Value::Null, &state, &mut Vec::new())
        .unwrap();
    assert_eq!(workbench.panes.as_ref().unwrap().panes().len(), 1);
    assert_eq!(workbench.pages.subscription(), Some(target));
    assert!(
        workbench
            .pages
            .take_requests()
            .iter()
            .any(|message| matches!(
                message,
                ClientMessage::Resource { request: ResourceRequest::Discard { page_id: discarded } }
                    if *discarded == page_id
            ))
    );
}

#[test]
fn closing_a_split_releases_its_selections_in_every_visited_buffer() {
    let state = state();
    let mut workbench = Workbench::default();
    let saved = WorkspaceView {
        text_panes: Some(DocumentPanes::new(1, 7)),
        ..Default::default()
    };
    workbench.restore(&saved, &state).unwrap();
    workbench.synchronize(&state);
    workbench
        .apply_document(snapshot(7, "abc"), &state)
        .unwrap();
    let mut effects = Vec::new();
    workbench
        .native("text.move-right", Value::Null, &state, &mut effects)
        .unwrap();
    workbench
        .native("vsplit", Value::Null, &state, &mut effects)
        .unwrap();
    let split = workbench.active_document(&state).unwrap().view_id;
    workbench
        .native("buffer-next", Value::Null, &state, &mut effects)
        .unwrap();
    workbench
        .apply_document(snapshot(8, "abc"), &state)
        .unwrap();
    workbench.render(&state, 80, 24).unwrap();
    assert_eq!(workbench.documents.position(7, split), Some((0, 1)));
    assert_eq!(workbench.documents.position(8, split), Some((0, 0)));

    workbench
        .native("quit", Value::Null, &state, &mut effects)
        .unwrap();
    assert_eq!(workbench.active_document(&state).unwrap().view_id, 1);
    assert_eq!(workbench.documents.position(7, 1), Some((0, 1)));
    for buffer in [7, 8] {
        assert_eq!(workbench.documents.position(buffer, split), None);
        assert!(
            workbench
                .documents
                .text(buffer)
                .unwrap()
                .selections(split)
                .is_empty()
        );
    }
}

fn open_document(state: &SessionSnapshot) -> Workbench {
    let mut workbench = Workbench::default();
    workbench
        .restore(
            &WorkspaceView {
                text_panes: Some(DocumentPanes::new(1, 7)),
                ..Default::default()
            },
            state,
        )
        .unwrap();
    workbench.synchronize(state);
    workbench.apply_document(snapshot(7, "abc"), state).unwrap();
    workbench
}

#[test]
fn interrupting_a_counted_command_clears_its_count_and_register() {
    let mut registry = Registry::<()>::default();
    for command in [
        "text.move-right",
        "command-prompt",
        "prompt.cancel",
        "vsplit",
        "focus-next",
    ] {
        registry.register_action(command, command, CommandImplementation::Native);
    }
    registry.binding("text", "select", &["l"], "text.move-right");
    registry.binding("global", "select", &[":"], "command-prompt");
    registry.binding("global", "prompt", &["Esc"], "prompt.cancel");
    registry.binding("global", "select", &["C-w", "v"], "vsplit");
    registry.binding("global", "select", &["C-w", "w"], "focus-next");

    let state = state();
    for interruption in [
        &["C-w", "l"][..],
        &["z"],
        &[":", "Esc"],
        &["C-w", "v", "C-w", "w"],
    ] {
        let mut workbench = open_document(&state);
        workbench.editor.set_catalog(registry.binding_catalog());
        let mut effects = Vec::new();
        workbench
            .native("text.select-mode", Value::Null, &state, &mut effects)
            .unwrap();
        workbench
            .native("text.select-register", Value::Null, &state, &mut effects)
            .unwrap();
        for key in ["+", "2", "0"] {
            workbench.key(key, &state, 80).unwrap();
        }
        assert_eq!(
            workbench.render(&state, 80, 24).unwrap().prompt,
            "20 · Register \"+"
        );

        for key in interruption {
            workbench.key(key, &state, 80).unwrap();
        }
        let frame = workbench.render(&state, 80, 24).unwrap();
        assert!(frame.prompt.is_empty(), "{interruption:?}");
        assert!(matches!(
            frame.cursor_shape,
            zess_ui::CursorShape::Underline
        ));
        assert_eq!(workbench.active_document(&state).unwrap().view_id, 1);
        assert_eq!(workbench.documents.position(7, 1), Some((0, 0)));

        workbench.key("l", &state, 80).unwrap();
        assert_eq!(workbench.documents.position(7, 1), Some((0, 1)));
    }
}

#[test]
fn write_quit_keeps_panes_open_if_the_target_has_switched_buffers() {
    let state = state();
    let mut workbench = open_document(&state);
    let mut effects = Vec::new();
    workbench
        .native("vsplit", Value::Null, &state, &mut effects)
        .unwrap();
    workbench
        .native("write-quit", Value::Null, &state, &mut effects)
        .unwrap();
    let sequence = workbench
        .documents
        .requests()
        .into_iter()
        .find_map(|request| match request {
            DocumentRequest::Save {
                buffer_id: 7,
                sequence,
                ..
            } => Some(sequence),
            _ => None,
        })
        .unwrap();
    workbench
        .native("buffer-next", Value::Null, &state, &mut effects)
        .unwrap();
    assert_eq!(workbench.active_document(&state).unwrap().buffer_id, 8);
    let expected_layout = workbench.workspace_view(&state).text_panes;

    let effects = workbench
        .apply_document(
            DocumentEvent::Saved {
                buffer_id: 7,
                sequence,
                version: 1,
            },
            &state,
        )
        .unwrap();
    assert!(effects.is_empty());
    assert_eq!(workbench.workspace_view(&state).text_panes, expected_layout);
}

#[test]
fn write_quit_keeps_its_pane_open_with_later_local_edits() {
    let state = state();
    let mut registry = Registry::<()>::default();
    for command in [
        "vsplit",
        "focus-next",
        "buffer-next",
        "write-quit",
        "text.insert",
    ] {
        registry.register_action(command, command, CommandImplementation::Native);
    }

    for replay in [false, true] {
        let mut workbench = open_document(&state);
        workbench.editor.set_catalog(registry.binding_catalog());
        let mut effects = Vec::new();
        for command in ["vsplit", "buffer-next", "focus-next"] {
            workbench
                .command(command.into(), Value::Null, &state, &mut effects)
                .unwrap();
        }
        assert_eq!(workbench.active_document(&state).unwrap().buffer_id, 7);
        workbench
            .apply_document(snapshot(8, "other document"), &state)
            .unwrap();
        workbench
            .command(
                "write-quit".into(),
                json!({"argument": "note.txt"}),
                &state,
                &mut effects,
            )
            .unwrap();
        let request = workbench
            .documents
            .requests()
            .into_iter()
            .find(|request| matches!(request, DocumentRequest::Save { buffer_id: 7, .. }))
            .unwrap();
        let DocumentRequest::Save {
            sequence, version, ..
        } = request
        else {
            unreachable!()
        };

        workbench
            .command("text.insert".into(), Value::Null, &state, &mut effects)
            .unwrap();
        workbench.paste("Y", &state).unwrap();
        workbench
            .command("focus-next".into(), Value::Null, &state, &mut effects)
            .unwrap();
        assert_eq!(workbench.active_document(&state).unwrap().buffer_id, 8);
        let panes = workbench.workspace_view(&state).text_panes;

        if replay {
            workbench.reconnect();
            assert!(workbench.documents.requests().contains(&request));
            let DocumentEvent::Snapshot(mut current) = snapshot(7, "saved elsewhere") else {
                unreachable!()
            };
            current.version = version + 1;
            current.path = Some("note.txt".into());
            workbench
                .apply_document(DocumentEvent::Snapshot(current), &state)
                .unwrap();
        }

        let effects = workbench
            .apply_document(
                DocumentEvent::Saved {
                    buffer_id: 7,
                    sequence,
                    version,
                },
                &state,
            )
            .unwrap();
        assert_eq!(workbench.workspace_view(&state).text_panes, panes);
        assert_eq!(workbench.documents.text(7).unwrap().text(), "Yabc");
        assert_eq!(workbench.documents.modified(7), Some(true));
        if replay {
            assert!(
                matches!(effects.as_slice(), [Effect::Notice(notice)] if notice.contains(":document.keep-local"))
            );
            assert!(workbench.documents.requests().is_empty());
        } else {
            assert!(effects.is_empty());
            assert!(
                matches!(workbench.documents.requests().as_slice(), [DocumentRequest::Edit { buffer_id: 7, changes, .. }] if changes.len() == 1 && changes[0].insert == "Y")
            );
        }
    }
}

#[hegel::test(derandomize = true)]
fn write_quit_closes_its_pane_after_earlier_saves_fail(tc: TestCase) {
    let earlier_saves = tc.draw(
        gs::vecs(gs::sampled_from(vec![
            ("write", true),
            ("write", false),
            ("write-quit", false),
        ]))
        .max_size(6),
    );
    let saves = [("write", false)]
        .into_iter()
        .chain(earlier_saves)
        .chain([("write-quit", true)])
        .collect::<Vec<_>>();
    let state = state();
    let mut workbench = open_document(&state);
    let mut registry = Registry::<()>::default();
    for command in ["vsplit", "focus-next", "write", "write-quit"] {
        registry.register_action(command, command, CommandImplementation::Native);
    }
    workbench.editor.set_catalog(registry.binding_catalog());
    assert_eq!(
        workbench.documents.requests(),
        [DocumentRequest::Read { buffer_id: 7 }]
    );
    let mut effects = Vec::new();
    workbench
        .command("vsplit".into(), Value::Null, &state, &mut effects)
        .unwrap();
    for (i, (command, _)) in saves.iter().enumerate() {
        workbench
            .command(
                (*command).into(),
                json!({"argument": format!("save-{i}.txt")}),
                &state,
                &mut effects,
            )
            .unwrap();
    }
    workbench
        .command("focus-next".into(), Value::Null, &state, &mut effects)
        .unwrap();
    assert!(effects.is_empty());
    let survivor = workbench.active_pane().unwrap();
    let before = workbench.workspace_view(&state).text_panes;

    let last_save = saves.len() - 1;
    for (i, (_, succeeds)) in saves.into_iter().enumerate() {
        let requests = workbench.documents.requests();
        let [
            request @ DocumentRequest::Save {
                buffer_id: 7,
                sequence,
                version: 1,
                path: Some(path),
            },
        ] = requests.as_slice()
        else {
            panic!("expected the next queued save, got {requests:?}");
        };
        assert_eq!(path, &format!("save-{i}.txt"));
        if tc.draw(gs::booleans()) {
            workbench.reconnect();
            assert_eq!(
                workbench.documents.requests(),
                [DocumentRequest::Read { buffer_id: 7 }, request.clone()]
            );
        }
        let event = if succeeds {
            DocumentEvent::Saved {
                buffer_id: 7,
                sequence: *sequence,
                version: 1,
            }
        } else {
            DocumentEvent::Failed {
                buffer_id: 7,
                sequence: *sequence,
                message: String::from("permission denied"),
            }
        };
        let effects = workbench.apply_document(event, &state).unwrap();
        if succeeds {
            assert!(effects.is_empty());
        } else {
            assert!(
                matches!(&effects[..], [Effect::Notice(message)] if message == "permission denied")
            );
        }
        if i != last_save {
            assert_eq!(workbench.workspace_view(&state).text_panes, before);
        }
    }

    let panes = workbench.workspace_view(&state).text_panes.unwrap();
    assert_eq!(panes.panes(), [survivor]);
    assert!(workbench.detach_synchronized());
}

fn clipboard_command(workbench: &mut Workbench, command: &str, state: &SessionSnapshot) -> u64 {
    let mut effects = Vec::new();
    workbench
        .native("text.select-register", Value::Null, state, &mut effects)
        .unwrap();
    workbench.key("+", state, 80).unwrap();
    workbench
        .native(command, Value::Null, state, &mut effects)
        .unwrap();
    let [Effect::ClipboardRead { id, .. }] = effects.as_slice() else {
        panic!("expected a clipboard read")
    };
    *id
}

#[test]
fn a_clipboard_reply_after_the_document_closes_leaves_the_replacement_untouched() {
    let mut state = state();
    let mut workbench = open_document(&state);
    let id = clipboard_command(&mut workbench, "text.paste-after", &state);
    state.buffers.retain(|buffer| buffer.id != 7);
    state.workspace.current = Some(8);
    state.workspace.buffers = vec![8];
    workbench.synchronize(&state);
    workbench
        .apply_document(snapshot(8, "replacement"), &state)
        .unwrap();

    assert!(
        workbench
            .paste_clipboard(id, Ok("late".into()), &state)
            .unwrap()
            .is_empty()
    );
    assert_eq!(workbench.documents.text(8).unwrap().text(), "replacement");
}

#[test]
fn closing_the_document_dismisses_its_search_prompt() {
    let mut registry = Registry::<()>::default();
    for command in ["help", "help.close"] {
        registry.register_action(command, command, CommandImplementation::Native);
    }
    registry.binding("global", "prompt", &["F1"], "help");
    registry.binding("help", "normal", &["q"], "help.close");

    for suspended in [false, true] {
        let mut state = state();
        let mut workbench = open_document(&state);
        workbench.editor.set_catalog(registry.binding_catalog());
        workbench
            .native("text.search-forward", Value::Null, &state, &mut Vec::new())
            .unwrap();
        assert!(workbench.editor.prompt_active());
        if suspended {
            workbench.key("F1", &state, 80).unwrap();
            assert!(workbench.editor.help_focused());
        }
        state.buffers.retain(|buffer| buffer.id != 7);
        state.workspace.current = Some(8);
        state.workspace.buffers = vec![8];
        workbench.synchronize(&state);
        workbench
            .apply_document(snapshot(8, "replacement"), &state)
            .unwrap();
        if suspended {
            workbench.key("q", &state, 80).unwrap();
            assert!(!workbench.editor.help_focused());
        }
        assert!(!workbench.editor.prompt_active());
        workbench.paste("new ", &state).unwrap();
        assert!(workbench.documents.text(8).unwrap().text().contains("new "));
    }
}

#[test]
fn clipboard_search_only_moves_the_selection_in_the_preview_that_requested_it() {
    let state = state();
    let mut workbench = Workbench::default();
    workbench.pages.open("files", json!({}));
    let subscription_id = workbench.pages.subscription().unwrap();
    let preview = |path: &str| {
        let mut update = publication(subscription_id, 0, &["item"]);
        update.output.requests.push(HostRequest::Preview {
            path: path.into(),
            content: zess_plugin::PreviewContent::Text { text: "abc".into() },
            focus: true,
        });
        update
    };
    workbench.apply_resource(preview("first"), &state).unwrap();
    let id = clipboard_command(&mut workbench, "text.search-next", &state);
    workbench.apply_resource(preview("second"), &state).unwrap();
    let selection = workbench.pages.event_selection().unwrap();
    workbench
        .paste_clipboard(id, Ok("c".into()), &state)
        .unwrap();
    assert_eq!(workbench.pages.event_selection().unwrap(), selection);

    let id = clipboard_command(&mut workbench, "text.search-next", &state);
    workbench.apply_resource(preview("second"), &state).unwrap();
    workbench
        .paste_clipboard(id, Ok("c".into()), &state)
        .unwrap();
    assert_ne!(workbench.pages.event_selection().unwrap(), selection);
}

#[test]
fn an_invalid_clipboard_search_pattern_reports_an_editing_error() {
    let state = state();
    let mut workbench = open_document(&state);
    let id = clipboard_command(&mut workbench, "text.search-next", &state);
    let effects = workbench
        .paste_clipboard(id, Ok("[".into()), &state)
        .unwrap();
    assert!(
        matches!(effects.as_slice(), [Effect::Notice(message)] if message.contains("invalid regular expression"))
    );
}

#[test]
fn edits_to_a_closed_buffer_can_be_copied_before_discarding_them() {
    let mut state = state();
    let mut workbench = Workbench::default();
    let saved = WorkspaceView {
        text_panes: Some(DocumentPanes::new(1, 7)),
        ..Default::default()
    };
    workbench.restore(&saved, &state).unwrap();
    workbench.synchronize(&state);
    workbench.apply_document(snapshot(7, "猫"), &state).unwrap();
    workbench
        .native("text.append", Value::Null, &state, &mut Vec::new())
        .unwrap();
    workbench.paste("犬", &state).unwrap();
    workbench
        .native("write-quit", Value::Null, &state, &mut Vec::new())
        .unwrap();

    state.buffers.retain(|buffer| buffer.id != 7);
    state.workspace.buffers = vec![8];
    state.workspace.current = Some(8);
    workbench.synchronize(&state);
    workbench.reconnect();
    assert_eq!(
        workbench.documents.requests(),
        [zess_protocol::document::DocumentRequest::Read { buffer_id: 8 }]
    );
    assert!(!workbench.documents.has_pending());
    assert!(!workbench.detach_synchronized());
    let status = workbench.render(&state, 100, 24).unwrap().status;
    assert!(status.contains(":document.keep-local 7"));
    assert!(status.contains(":document.use-workspace 7"));
    assert!(
        workbench
            .native("exit", Value::Null, &state, &mut Vec::new())
            .is_err()
    );

    workbench.apply_document(snapshot(8, ""), &state).unwrap();
    let mut effects = Vec::new();
    workbench
        .native(
            "document.keep-local",
            json!({"argument": "7"}),
            &state,
            &mut effects,
        )
        .unwrap();
    assert!(!workbench.detach_synchronized());
    workbench
        .native(
            "document.use-workspace",
            json!({"argument": "7"}),
            &state,
            &mut effects,
        )
        .unwrap();
    assert!(workbench.detach_synchronized());
    workbench
        .native("text.paste-after", Value::Null, &state, &mut effects)
        .unwrap();
    assert_eq!(workbench.documents.text(8).unwrap().text(), "猫犬");
}
