use zess_protocol::resource::{CachedPreview, PreviewProjection};

use super::*;

fn preview_entry(pages: &Pages, primary: &str, revision: u64) -> PageCacheEntry {
    PageCacheEntry {
        page_id: pages.active_page_id().unwrap(),
        key: PageCacheKey::Preview {
            primary: primary.into(),
            width: 50,
            height: 23,
        },
        revision,
    }
}

fn publish(
    pages: &mut Pages,
    subscription_id: u64,
    generation: u64,
    entry: &PageCacheEntry,
    data: Option<PreviewProjection>,
    patch_revision: Option<u64>,
    focus: bool,
) {
    let mut update = rows(subscription_id, generation, &["file:a", "file:b"]);
    let PageCacheKey::Preview { primary, .. } = &entry.key else {
        unreachable!();
    };
    update.output.selection = Some(ItemSelection {
        primary: Some(primary.clone()),
        selected: vec![primary.clone()],
        anchor: Some(primary.clone()),
    });
    update.cache.preview = Some(CachedPreview {
        entry: entry.clone(),
        data,
        focus,
        request_index: 0,
    });
    update.patch_revision = patch_revision;
    pages.apply(update);
}

fn diff(path: &str, label: &str) -> PreviewProjection {
    let panes = [(DiffSide::Staged, "STAGED"), (DiffSide::Unstaged, "WORKTREE")]
        .map(|(side, title)| PreviewPane::View {
            id: title.into(),
            path: path.into(),
            header: title.into(),
            loading: false,
            key_scope: None,
            content: PreviewContent::GitDiff {
                diff: GitDiff {
                    source: DiffSource::Changes { side },
                    files: vec![FileDiff {
                        target: DiffTarget {
                            path: path.into(),
                            original_path: None,
                        },
                        raw: format!(
                            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-before\n+{label}-{title}\n"
                        ),
                        rendered: None,
                    }],
                },
            },
        })
        .into();
    PreviewProjection {
        panes,
        layout: PreviewLayout::Columns,
    }
}

fn select(pages: &mut Pages, command: &str, primary: &str) -> (u64, u64) {
    assert!(pages.command(command, 1));
    let requests = pages.take_requests();
    let [
        ClientMessage::Resource {
            request:
                ResourceRequest::Update {
                    subscription_id,
                    input,
                    event: ResourceEvent::Select { .. },
                },
        },
    ] = requests.as_slice()
    else {
        panic!("selection must publish interest in the current preview");
    };
    assert_eq!(input.selection.primary.as_deref(), Some(primary));
    (*subscription_id, input.generation)
}

fn rendered(pages: &mut Pages) -> String {
    let mut frame = Frame {
        width: 100,
        height: 24,
        ..Default::default()
    };
    assert!(pages.render(&mut frame, None, &mut Default::default(), true));
    frame
        .rows
        .into_iter()
        .flat_map(|row| row.spans)
        .map(|span| span.text)
        .collect()
}

#[test]
fn revisiting_a_git_item_displays_both_previews_before_confirmation() {
    let mut pages = Pages::default();
    pages.resize(100, 24);
    pages.command("git", 1);
    pages.take_requests();
    let id = pages.subscription().unwrap();
    let a = preview_entry(&pages, "file:a", 1);
    let b = preview_entry(&pages, "file:b", 2);
    publish(&mut pages, id, 0, &a, Some(diff("a", "A")), Some(10), true);
    assert!(pages.command("preview.return", 1));
    let (_, b_generation) = select(&mut pages, "data.move-down", "file:b");
    publish(
        &mut pages,
        id,
        b_generation,
        &b,
        Some(diff("b", "B")),
        Some(20),
        false,
    );
    assert!(rendered(&mut pages).contains("B-WORKTREE"));

    let (_, a_generation) = select(&mut pages, "data.move-up", "file:a");
    let interest = pages.cache_interest();
    assert!(matches!(
        interest,
        Some(ClientMessage::Resource {
            request: ResourceRequest::CacheInterest { entries },
        }) if entries.contains(&a) && entries.contains(&b)
    ));
    publish(
        &mut pages,
        id,
        b_generation,
        &b,
        Some(diff("b", "LATE")),
        Some(21),
        true,
    );
    let text = rendered(&mut pages);
    assert!(text.contains("A-STAGED") && text.contains("A-WORKTREE"));
    assert!(!text.contains("LATE-WORKTREE"));
    assert_eq!(pages.scope(), Some("git"));

    assert!(pages.command("diff.apply-selection", 1));
    assert!(pages.command("diff.edit-source", 1));
    assert!(pages.take_actions().is_empty());
    assert!(pages.take_requests().is_empty());

    publish(&mut pages, id, a_generation, &a, None, Some(30), false);
    assert!(pages.command("diff.apply-selection", 1));
    assert!(matches!(
        pages.take_actions().as_slice(),
        [CommandAction::ApplyPagePatch { request, .. }] if request.revision == 30
    ));
}

#[test]
fn requesting_a_commit_preview_focuses_the_cached_commit_while_confirmation_is_pending() {
    let mut pages = Pages::default();
    pages.resize(100, 24);
    pages.open("git-history", json!({}));
    pages.take_requests();
    let id = pages.subscription().unwrap();
    let commits = ["aaa1", "bbb2"];
    let first = preview_entry(&pages, commits[0], 1);
    let second = preview_entry(&pages, commits[1], 2);
    let projection = |commit: &str, message: &str, loading| PreviewProjection {
        panes: vec![PreviewPane::View {
            id: "commit-info".into(),
            path: commit.into(),
            header: "Commit".into(),
            loading,
            content: PreviewContent::Rows {
                rows: (!message.is_empty())
                    .then(|| DisplayRow {
                        text: message.into(),
                        ..Default::default()
                    })
                    .into_iter()
                    .collect(),
                column_alignment: Default::default(),
            },
            key_scope: Some("git-history-preview".into()),
        }],
        layout: PreviewLayout::Rows,
    };
    let mut update = rows(id, 0, &commits);
    update.cache.preview = Some(CachedPreview {
        entry: first.clone(),
        data: Some(projection(commits[0], "FIRST COMMIT", false)),
        focus: false,
        request_index: 0,
    });
    pages.apply(update);

    let (_, second_generation) = select(&mut pages, "data.move-down", commits[1]);
    let mut update = rows(id, second_generation, &[commits[1], commits[0]]);
    update.output.display = None;
    update.cache.preview = Some(CachedPreview {
        entry: second,
        data: Some(projection(commits[1], "SECOND COMMIT", false)),
        focus: false,
        request_index: 0,
    });
    pages.apply(update);
    assert!(rendered(&mut pages).contains("SECOND COMMIT"));

    let (_, first_generation) = select(&mut pages, "data.move-up", commits[0]);
    pages.allow_preview_focus("git-history", "preview");
    let mut update = rows(id, first_generation, &commits);
    let PreviewProjection { panes, layout } = projection(commits[0], "", true);
    update.output.requests.push(HostRequest::PreviewGroup {
        panes,
        layout,
        focus: true,
    });
    pages.apply(update);
    assert_eq!(pages.scope(), Some("git-history-preview"));
    assert!(rendered(&mut pages).contains("FIRST COMMIT"));

    let mut update = rows(id, first_generation, &commits);
    update.cache.preview = Some(CachedPreview {
        entry: first,
        data: None,
        focus: false,
        request_index: 0,
    });
    pages.apply(update);
    assert_eq!(pages.scope(), Some("git-history-preview"));
}

#[test]
fn previews_of_the_same_path_in_different_pages_do_not_replace_each_other() {
    let mut left = Pages::default();
    left.resize(100, 24);
    let mut right = left.sibling();
    left.open("git-commit-files", json!({"commit": "one"}));
    right.open("git-commit-files", json!({"commit": "two"}));
    left.take_requests();
    let left_id = left.subscription().unwrap();
    let right_id = right.subscription().unwrap();
    let a = preview_entry(&left, "file:a", 1);
    let b = preview_entry(&right, "file:b", 2);
    publish(
        &mut left,
        left_id,
        0,
        &a,
        Some(diff("a", "LEFT")),
        None,
        false,
    );
    publish(
        &mut right,
        right_id,
        0,
        &b,
        Some(diff("b", "RIGHT")),
        None,
        false,
    );
    select(&mut right, "data.move-up", "file:a");
    let text = rendered(&mut right);
    assert!(text.contains("RIGHT-WORKTREE"));
    assert!(!text.contains("LEFT-WORKTREE"));
}

#[test]
fn revisiting_a_file_displays_its_previous_preview_until_the_read_completes() {
    let mut pages = Pages::default();
    pages.resize(100, 24);
    pages.command("files", 1);
    pages.take_requests();
    let id = pages.subscription().unwrap();
    let a = preview_entry(&pages, "file:a", 1);
    let b = preview_entry(&pages, "file:b", 2);
    let text = |path: &str, text: &str| {
        Some(PreviewProjection {
            panes: vec![PreviewPane::Surface {
                id: "main".into(),
                path: path.into(),
                header: String::new(),
                loading: false,
                content: PreviewContent::Text { text: text.into() },
            }],
            layout: PreviewLayout::Rows,
        })
    };
    publish(
        &mut pages,
        id,
        0,
        &a,
        text("file:a", "FIRST FILE"),
        None,
        false,
    );
    let (_, middle) = select(&mut pages, "data.move-down", "file:b");
    publish(
        &mut pages,
        id,
        middle,
        &b,
        text("file:b", "SECOND FILE"),
        None,
        false,
    );
    assert!(rendered(&mut pages).contains("SECOND FILE"));

    let (_, returned) = select(&mut pages, "data.move-up", "file:a");
    assert!(rendered(&mut pages).contains("FIRST FILE"));
    let fresh = preview_entry(&pages, "file:a", 3);
    publish(
        &mut pages,
        id,
        returned,
        &fresh,
        text("file:a", "FRESH CONTENT"),
        None,
        false,
    );
    assert!(rendered(&mut pages).contains("FRESH CONTENT"));
}

#[test]
fn invalidating_the_displayed_revision_revokes_its_patch() {
    let mut pages = Pages::default();
    pages.resize(100, 24);
    pages.command("git", 1);
    let id = pages.subscription().unwrap();
    let before = preview_entry(&pages, "file:a", 1);
    let after = preview_entry(&pages, "file:a", 2);
    publish(
        &mut pages,
        id,
        0,
        &before,
        Some(diff("a", "OLD")),
        Some(10),
        false,
    );
    publish(
        &mut pages,
        id,
        0,
        &after,
        Some(diff("a", "NEW")),
        Some(11),
        false,
    );
    pages.invalidate_cache(&[before], ResourceCacheInvalidation::Changed);
    assert!(pages.command("diff.apply-selection", 1));
    assert!(matches!(
        pages.take_actions().as_slice(),
        [CommandAction::ApplyPagePatch { request, .. }] if request.revision == 11
    ));
    pages.invalidate_cache(&[after], ResourceCacheInvalidation::Changed);
    assert!(pages.command("diff.apply-selection", 1));
    assert!(pages.take_actions().is_empty());
    assert!(rendered(&mut pages).contains("NEW-WORKTREE"));
}
