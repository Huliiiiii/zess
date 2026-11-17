use super::*;

mod diff;
mod pointer;
mod rendering;

#[test]
fn restoring_a_closed_document_layer_returns_to_its_page() {
    let mut workbench = Workbench::default();
    workbench.panes = Some(DocumentPanes::new(1, 7));
    workbench.pages.open("git", json!({}));
    workbench.pages.suspend();
    workbench.page_document = Some(DocumentLayer {
        buffer_id: 8,
        previous: workbench.active_pane().unwrap(),
    });
    workbench.panes.as_mut().unwrap().activate_buffer(8);
    let state = SessionSnapshot {
        buffers: vec![zess_protocol::BufferSummary {
            id: 7,
            kind: BufferKind::Text { path: None },
            modified: false,
        }],
        ..Default::default()
    };
    let saved = workbench.workspace_view(&state);
    let mut reattached = Workbench::default();

    reattached.restore(&saved, &state).unwrap();

    assert_eq!(
        reattached.active_pane(),
        Some(DocumentPane {
            view_id: 1,
            buffer_id: 7,
        })
    );
    assert_eq!(reattached.pages.scope(), Some("git"));
    assert!(reattached.workspace_view(&state).page_document.is_none());
}

#[test]
fn restoring_the_workspace_view_adopts_its_draft() {
    let state = SessionSnapshot::default();
    let mut workbench = Workbench::default();
    workbench
        .open_layer("commit".into(), "old".into(), false)
        .unwrap();
    let mut saved = workbench.workspace_view(&state);
    saved.draft.as_mut().unwrap().text = "new".into();

    workbench.restore(&saved, &state).unwrap();
    assert_eq!(workbench.workspace_view(&state).draft, saved.draft);

    saved.draft = None;
    workbench.restore(&saved, &state).unwrap();
    assert!(workbench.workspace_view(&state).draft.is_none());
}
