use zess_plugin::{CommandImplementation, PluginOutput, PluginRegistrar, PreviewContent, Registry};
use zess_protocol::{
    BufferSummary,
    document::DocumentSnapshot,
    editor_events::{Cursor, EditorHooks},
};

use super::*;

#[test]
fn registered_hooks_report_the_key_and_resulting_preview_selection() {
    let mut workbench = Workbench::default();
    let mut registry = Registry::<()>::default();
    registry.register_action(
        "text.move-right",
        "Move right",
        CommandImplementation::Native,
    );
    registry.binding("text", "normal", &["l"], "text.move-right");
    workbench.editor.set_catalog(registry.binding_catalog());
    workbench.pages.open("files", json!({}));
    let subscription_id = workbench.pages.subscription().unwrap();
    workbench.pages.apply(ResourceUpdate {
        cache: Default::default(),
        kind: Default::default(),
        subscription_id,
        generation: 0,
        document_scoped: false,
        output: PluginOutput {
            requests: vec![HostRequest::Preview {
                path: "file.txt".into(),
                content: PreviewContent::Text { text: "abc".into() },
                focus: true,
            }],
            ..Default::default()
        },
        metadata: None,
        patch_revision: None,
        document: None,
    });

    let mut state = SessionSnapshot::default();
    assert!(workbench.key("l", &state, 80).unwrap().is_empty());

    state.editor_hooks = EditorHooks {
        input: true,
        selection_changed: true,
    };
    let effects = workbench.key("l", &state, 80).unwrap();
    let [
        Effect::Command(CommandAction::Observe {
            source: input_source,
            event: input,
        }),
        Effect::Command(CommandAction::Observe {
            source: selection_source,
            event: selection,
        }),
    ] = effects.as_slice()
    else {
        panic!("expected the registered Input and SelectionChanged observations");
    };
    let preview = EventSource::Page {
        subscription_id,
        slot_id: Some("main".into()),
    };
    assert_eq!(input_source, &preview);
    assert_eq!(selection_source, &preview);
    assert_eq!(input, &EditorEvent::Input { key: "l".into() });
    assert_eq!(
        selection,
        &EditorEvent::SelectionChanged {
            selection: EditorSelection::Text {
                cursors: vec![Cursor {
                    row: 0,
                    column: 2,
                    anchor_row: 0,
                    anchor_column: 2,
                }],
            },
        }
    );
}

#[test]
fn pasting_reports_only_the_resulting_selection_change() {
    let state = SessionSnapshot {
        buffers: vec![BufferSummary {
            id: 7,
            kind: BufferKind::Text { path: None },
            modified: false,
        }],
        editor_hooks: EditorHooks {
            input: true,
            selection_changed: true,
        },
        ..Default::default()
    };
    let saved = WorkspaceView {
        text_panes: Some(DocumentPanes::new(1, 7)),
        ..Default::default()
    };
    let mut workbench = Workbench::default();
    workbench.restore(&saved, &state).unwrap();
    workbench.synchronize(&state);
    workbench
        .apply_document(
            DocumentEvent::Snapshot(DocumentSnapshot {
                buffer_id: 7,
                version: 1,
                modified: false,
                readonly: false,
                language: None,
                path: None,
                text: String::new(),
                history: Default::default(),
            }),
            &state,
        )
        .unwrap();
    workbench.render(&state, 80, 24).unwrap();

    assert!(workbench.paste("", &state).unwrap().is_empty());
    let effects = workbench.paste("one\ntwo", &state).unwrap();
    let [Effect::Command(CommandAction::Observe { source, event })] = effects.as_slice() else {
        panic!("expected one selection observation");
    };
    assert_eq!(
        source,
        &EventSource::View {
            buffer_id: 7,
            view_id: 1
        }
    );
    assert_eq!(
        event,
        &EditorEvent::SelectionChanged {
            selection: EditorSelection::Text {
                cursors: vec![Cursor {
                    row: 1,
                    column: 2,
                    anchor_row: 0,
                    anchor_column: 0,
                }],
            },
        },
    );
}
