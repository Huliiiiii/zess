use super::*;
use zess_git::patch::PatchRow;
use zess_protocol::resource::{PageInput, PagePatch, PageSource, ResourceRequest};
use zess_protocol::{CommandAction, CommandOutcome};

#[test]
fn staging_rows_requires_the_published_diff_revision() {
    let (fixture, path) = repository();
    fs::write(&path, "historical\nfirst addition\nsecond addition\n").unwrap();
    let mut session = fixture.session(&[]);
    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id: 7,
        page_id: zess_protocol::resource::PageId {
            context_epoch: 0,
            sequence: 7,
        },
        clone_from: None,
        source: PageSource {
            kind: "git".into(),
            argument: Value::Null,
        },
        input: PageInput {
            generation: 1,
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
    let selection = std::cell::Cell::new(None);
    wait_until(&mut session, |session| {
        for update in session.take_resource_updates() {
            let Some(revision) = update.patch_revision else {
                continue;
            };
            for request in update.output.requests {
                let HostRequest::PreviewGroup { panes, .. } = request else {
                    continue;
                };
                for pane in panes {
                    let PreviewPane::View {
                        content: PreviewContent::GitDiff { diff },
                        ..
                    } = pane
                    else {
                        continue;
                    };
                    let zess_git::DiffSource::Changes { side } = diff.source else {
                        continue;
                    };
                    for file in diff.files {
                        if let Some(row) =
                            file.raw.lines().position(|line| line == "+second addition")
                        {
                            selection.set(Some(PagePatch {
                                revision,
                                side,
                                target: file.target,
                                rows: vec![PatchRow(row)..=PatchRow(row)],
                            }));
                            return true;
                        }
                    }
                }
            }
        }
        false
    });
    let selection = selection.take().unwrap();
    let stale = CommandAction::ApplyPagePatch {
        subscription_id: 7,
        request: PagePatch {
            revision: selection.revision.wrapping_sub(1),
            ..selection.clone()
        },
    };
    assert!(
        matches!(session.begin_operation("stale", &stale), Some(CommandOutcome::Failed{message}) if message.contains("preview changed"))
    );
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    let apply = CommandAction::ApplyPagePatch {
        subscription_id: 7,
        request: selection,
    };
    assert_eq!(session.begin_operation("apply", &apply), None);
    wait_until(&mut session, |session| {
        session
            .take_operation_completions()
            .contains(&("apply".into(), CommandOutcome::Applied))
    });
    assert_eq!(
        git(&fixture, &["show", ":note.txt"]),
        "historical\nsecond addition"
    );
}
