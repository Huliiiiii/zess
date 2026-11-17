use super::*;
use zess_protocol::document::DocumentChange;
use zess_protocol::resource::{
    PageId, PageInput, PageMetadata, PageSource, PageSplit, ResourceEvent, ResourceRequest,
};
use zess_protocol::workspace_view::WorkspaceView;
use zess_protocol::{CommandAction, CommandOutcome, CommandTarget};

fn subscribe_files(
    session: &mut Session,
    subscription_id: u64,
    page_id: PageId,
    clone_from: Option<PageSplit>,
) -> PageMetadata {
    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id,
        page_id,
        clone_from,
        source: PageSource {
            kind: "files".into(),
            argument: Value::Null,
        },
        input: PageInput {
            generation: 0,
            selection: Default::default(),
            register: None,
            mode: "normal".into(),
            filter: None,
            source_buffer: None,
            width: 40,
            active: true,
            preview_width: 39,
            preview_height: 22,
        },
    });
    session
        .take_resource_updates()
        .into_iter()
        .filter(|update| update.subscription_id == subscription_id)
        .find_map(|update| update.metadata)
        .unwrap()
}

#[test]
fn split_pages_recover_their_own_views_of_the_same_buffer() {
    let root = Fixture::new();
    let mut session = root.session(&[]);
    let source_id = PageId {
        context_epoch: 0,
        sequence: 11,
    };
    let split_id = PageId {
        context_epoch: 0,
        sequence: 12,
    };
    let origin = PageSplit {
        source: source_id,
        model: source_id,
    };
    let original = subscribe_files(&mut session, 1, source_id, None);
    let split = subscribe_files(&mut session, 2, split_id, Some(origin));
    assert_ne!(original.view_id, split.view_id);
    assert_eq!(original.buffer_id, split.buffer_id);
    session.handle_resource(ResourceRequest::Release { subscription_id: 1 });
    session.handle_resource(ResourceRequest::Release { subscription_id: 2 });
    let restored_split = subscribe_files(&mut session, 3, split_id, Some(origin));
    let restored_original = subscribe_files(&mut session, 4, source_id, None);
    assert_eq!(
        (restored_split.view_id, restored_split.buffer_id),
        (split.view_id, split.buffer_id)
    );
    assert_eq!(
        (restored_original.view_id, restored_original.buffer_id),
        (original.view_id, original.buffer_id)
    );
}

#[test]
fn replayed_splits_share_a_buffer_and_clone_the_requested_parent_view() {
    let root = Fixture::new();
    let mut session = root.session(&[]);
    let page = |sequence| PageId {
        context_epoch: 5,
        sequence,
    };
    let model = page(1);
    let parent = page(2);

    // A split can subscribe while its source remains cached in the terminal.
    let child = subscribe_files(
        &mut session,
        1,
        page(3),
        Some(PageSplit {
            source: parent,
            model,
        }),
    );
    let source = subscribe_files(
        &mut session,
        2,
        parent,
        Some(PageSplit {
            source: model,
            model,
        }),
    );
    let original = subscribe_files(&mut session, 3, model, None);
    assert_ne!(child.view_id, source.view_id);
    assert_eq!(child.buffer_id, source.buffer_id);
    assert_eq!(child.buffer_id, original.buffer_id);

    session
        .page_request(
            2,
            &Default::default(),
            HostRequest::ConfigureView {
                options: json!({ "git-status": false }),
            },
        )
        .unwrap();
    session.detach_resources(None, |_| false);
    let split = subscribe_files(
        &mut session,
        1,
        page(4),
        Some(PageSplit {
            source: parent,
            model,
        }),
    );
    assert_ne!(split.view_id, source.view_id);
    assert_eq!(split.buffer_id, source.buffer_id);
    assert_eq!(split.options["git-status"], false);
}

#[test]
fn detaching_reclaims_retired_pages_and_preserves_pages_absent_from_an_older_save() {
    let root = Fixture::new();
    let mut session = root.session(&[]);
    let retired = PageId {
        context_epoch: 5,
        sequence: 1,
    };
    let pending = PageId {
        context_epoch: 5,
        sequence: 2,
    };
    let next_client = PageId {
        context_epoch: 6,
        sequence: 2,
    };
    let retired_view = subscribe_files(&mut session, 1, retired, None).view_id;
    let pending_view = subscribe_files(&mut session, 2, pending, None).view_id;
    let mut saved = WorkspaceView {
        page_watermarks: vec![retired],
        ..Default::default()
    };
    session.detach_resources(Some(&saved), |_| false);

    let next_view = subscribe_files(&mut session, 1, next_client, None).view_id;
    assert_ne!(next_view, pending_view);
    saved.page_watermarks.push(next_client);
    session.detach_resources(Some(&saved), |_| false);

    assert_eq!(
        subscribe_files(&mut session, 1, pending, None).view_id,
        pending_view
    );
    let recreated_view = subscribe_files(&mut session, 2, retired, None).view_id;
    assert_ne!(recreated_view, retired_view);
}

#[test]
fn an_open_outline_refreshes_after_syntax_catches_up_to_document_edits() {
    let root = Fixture::new();
    let path = root.write("outline.rs", "fn original() {}\n");
    let mut session = root.session(&[path]);
    let source = session.views[&session.focus].location.buffer;
    let input = PageInput {
        generation: 0,
        selection: Default::default(),
        register: None,
        mode: "normal".into(),
        filter: None,
        source_buffer: Some(source),
        width: 40,
        active: true,
        preview_width: 39,
        preview_height: 22,
    };
    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id: 1,
        page_id: zess_protocol::resource::PageId {
            context_epoch: 0,
            sequence: 1,
        },
        clone_from: None,
        source: PageSource {
            kind: "outline".into(),
            argument: Value::Null,
        },
        input: input.clone(),
    });
    assert!(session.take_resource_updates().into_iter().any(|update| {
        update
            .output
            .display
            .is_some_and(|display| display.rows[0].text.contains("original"))
    }));

    session
        .edit_document(
            source,
            &[DocumentChange {
                start: 3,
                end: 11,
                insert: "revised".into(),
            }],
            None,
        )
        .unwrap();
    session.handle_resource(ResourceRequest::Update {
        subscription_id: 1,
        input: input.clone(),
        event: ResourceEvent::Resize,
    });
    assert!(session.take_resource_updates().is_empty());
    session.handle_resource(ResourceRequest::Update {
        subscription_id: 1,
        input,
        event: ResourceEvent::Select {
            navigation: zess_plugin::ItemNavigation::Jump,
        },
    });
    session.take_resource_updates();
    session.tick_resources(false);
    assert!(session.take_resource_updates().is_empty());

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.tick();
        if let Some(display) = session
            .take_resource_updates()
            .into_iter()
            .find_map(|update| {
                (update.subscription_id == 1)
                    .then_some(update.output.display)
                    .flatten()
            })
        {
            assert!(display.rows[0].text.contains("revised"));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn closing_a_modified_fullscreen_file_preserves_the_original_document() {
    let root = Fixture::new();
    let path = root.write("notes", "original");
    let mut session = root.session(&[path]);
    let original = session.views[&session.focus].location.buffer;

    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id: 9,
        page_id: zess_protocol::resource::PageId {
            context_epoch: 0,
            sequence: 9,
        },
        clone_from: None,
        source: PageSource {
            kind: "files".into(),
            argument: Value::Null,
        },
        input: PageInput {
            generation: 1,
            selection: Default::default(),
            register: None,
            mode: "normal".into(),
            filter: None,
            source_buffer: Some(original),
            width: 40,
            active: true,
            preview_width: 39,
            preview_height: 22,
        },
    });
    let Some(CommandOutcome::PageApplied {
        document: Some(opened),
        ..
    }) = session.begin_operation(
        "open",
        &CommandAction::Request {
            target: CommandTarget::Page {
                subscription_id: 9,
                input: Default::default(),
            },
            request: HostRequest::FileLayer {
                path: "notes".into(),
                line: 2,
            },
        },
    )
    else {
        panic!("the page command must return the opened document");
    };
    assert!(opened.fullscreen);
    assert_eq!(opened.line, 2);
    let copy = opened.buffer_id;
    assert_ne!(copy, original);
    assert_eq!(session.views[&session.focus].location.buffer, original);
    assert_eq!(session.document_snapshot(copy).unwrap().text, "original");

    session
        .edit_document(
            copy,
            &[DocumentChange {
                start: 0,
                end: 8,
                insert: "unsaved".into(),
            }],
            None,
        )
        .unwrap();
    assert!(matches!(
        session.begin_operation(
            "close",
            &CommandAction::ClosePageDocument {
                buffer_id: copy,
                force: false
            }
        ),
        Some(CommandOutcome::Failed { .. })
    ));
    assert_eq!(session.document_snapshot(copy).unwrap().text, "unsaved");
    assert_eq!(
        session.begin_operation(
            "discard",
            &CommandAction::ClosePageDocument {
                buffer_id: copy,
                force: true
            }
        ),
        Some(CommandOutcome::PageDocumentClosed { buffer_id: copy })
    );
    assert!(session.document_snapshot(copy).is_none());
    assert_eq!(
        session.document_snapshot(original).unwrap().text,
        "original"
    );
}
