use super::*;

#[test]
fn delta_side_by_side_output_maps_both_change_blocks_to_the_original_patch() {
    // Captured from delta 0.20.1 --paging=never --side-by-side --width=44,
    // with OSC1717=V1, CLICOLOR_FORCE=1 and FORCE_COLOR=1.
    let diff = Diff::from_git(
        zess_git::GitDiff {
            source: zess_git::DiffSource::Changes {
                side: zess_git::DiffSide::Unstaged,
            },
            files: vec![zess_git::FileDiff {
                target: zess_git::DiffTarget {
                    path: "note.txt".into(),
                    original_path: None,
                },
                raw: include_str!("tests/fixtures/delta-0.20.1-side-by-side.diff").into(),
                rendered: Some(zess_git::RenderedDiff {
                    file_header: false,
                    text: include_str!("tests/fixtures/delta-0.20.1-side-by-side.ansi").into(),
                    repository_root: "/workspace".into(),
                }),
            }],
        },
        Path::new("/workspace"),
    );
    assert!(diff.mapped_rendering());
    let index = diff.index(Presentation::Rendered);
    let mut selection = index.initial_selection();
    assert_eq!(selection.rows(index).unwrap(), 6..=7);
    let (file, rows) = diff
        .selected_patch_rows(Presentation::Rendered, &selection)
        .unwrap();
    assert_eq!(
        RawPatch::parse(&file.raw)
            .unwrap()
            .selection(zess_git::DiffSide::Unstaged, &rows)
            .unwrap(),
        "--- a/note.txt\n+++ b/note.txt\n@@ -1,3 +1,3 @@\n-old\n+new\n context\n second old\n"
    );
    selection.move_by(index, true, true);
    assert_eq!(selection.rows(index).unwrap(), 9..=9);
}

fn rendered_diff(raw: &str, rendered: &str) -> Diff {
    Diff::from_git(
        zess_git::GitDiff {
            source: zess_git::DiffSource::Changes {
                side: zess_git::DiffSide::Unstaged,
            },
            files: vec![zess_git::FileDiff {
                target: zess_git::DiffTarget {
                    path: "file;name".into(),
                    original_path: None,
                },
                raw: raw.into(),
                rendered: Some(zess_git::RenderedDiff {
                    file_header: false,
                    text: rendered.into(),
                    repository_root: "/workspace".into(),
                }),
            }],
        },
        Path::new("/workspace"),
    )
}

#[test]
fn rendered_changes_select_all_their_patch_lines_and_wrapped_rows_share_a_block() {
    let diff = rendered_diff(
        "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,4 +1,4 @@\n first\n-old\n+new\n middle\n-last\n+next\n",
        concat!(
            "\x1b]1717;1;f;;;file;name\x07\x1b]1717;1;h;1;;file;name\x07header\n",
            "\x1b]1717;1;c;1;;file;name\x07first\n",
            "\x1b]1717;1;d;2;2;file;name\x07\x1b]1717;1;a;2;;file;name\x07old -> new\n",
            "decoration\n",
            "\x1b]1717;1;d;2;2;file;name\x07\x1b]1717;1;a;2;;file;name\x07continued\n",
            "\x1b]1717;1;c;3;;file;name\x07middle\n",
            "\x1b]1717;1;d;4;4;file;name\x07\x1b]1717;1;a;4;;file;name\x07last -> next\n",
        ),
    );
    assert!(diff.mapped_rendering());
    let index = diff.index(Presentation::Rendered);
    let mut selection = index.initial_selection();
    assert!(!selection.select_at(index, 0, false));
    assert!(!selection.select_at(index, 3, false));
    assert!(selection.select_at(index, 4, false));
    let (file, rows) = diff
        .selected_patch_rows(Presentation::Rendered, &selection)
        .unwrap();
    assert_eq!(file.target.path, "file;name");
    assert_eq!(
        rows.iter()
            .map(|row| (row.start().0, row.end().0))
            .collect::<Vec<_>>(),
        [(5, 5), (6, 6)]
    );
    let patch = RawPatch::parse(&file.raw)
        .unwrap()
        .selection(zess_git::DiffSide::Unstaged, &rows)
        .unwrap();
    assert_eq!(
        patch,
        "--- a/file;name\n+++ b/file;name\n@@ -1,4 +1,4 @@\n first\n-old\n+new\n middle\n last\n"
    );
    selection.select_block(index);
    assert_eq!(selection.rows(index).unwrap(), 2..=4);
    assert_eq!(
        diff.selected_patch_rows(Presentation::Rendered, &selection)
            .unwrap()
            .1
            .len(),
        2
    );
    selection.move_by(index, true, true);
    assert_eq!(selection.row(), Some(6));
    assert_eq!(
        diff.search(Presentation::Rendered, "continued", Some(1), true)
            .unwrap()
            .map(|row| row.0),
        Some(4)
    );
    assert!(
        diff.search(Presentation::Rendered, "decoration|header", None, true)
            .unwrap()
            .is_none()
    );
}

#[test]
fn hidden_context_and_partial_blocks_still_select_the_complete_original_block() {
    let raw = "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n first\n-old\n+new\n last\n";
    let diff = rendered_diff(raw, "\x1b]1717;1;d;2;2;file;name\x07old\n");
    assert!(diff.mapped_rendering());
    let index = diff.index(Presentation::Rendered);
    let (file, rows) = diff
        .selected_patch_rows(Presentation::Rendered, &index.initial_selection())
        .unwrap();
    assert_eq!(
        RawPatch::parse(&file.raw)
            .unwrap()
            .selection(zess_git::DiffSide::Unstaged, &rows)
            .unwrap(),
        "--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n first\n-old\n+new\n last\n"
    );
}

#[test]
fn malformed_metadata_keeps_the_raw_diff_for_actions() {
    let raw = "@@ -1 +1 @@\n-old\n+new\n";
    let diff = rendered_diff(
        raw,
        "\x1b]1717;1;d;1;;file;name\x07old\n\x1b]1717;1;a;1;;file;name\x07new\n",
    );
    assert!(!diff.mapped_rendering());
    assert_eq!(diff.index(Presentation::Raw).rows.len(), 3);
    assert!(
        diff.selected_patch_rows(Presentation::Rendered, &diff.index.initial_selection())
            .is_err()
    );
}

#[test]
fn unresolved_or_ambiguous_change_identities_reject_the_selected_patch() {
    for (raw, rendered) in [
        (
            "@@ -1 +1 @@\n-old\n+new\n",
            "\x1b]1717;1;d;1;1;another-file\x07old\n\x1b]1717;1;a;1;;file;name\x07new\n",
        ),
        (
            "@@ -1 +1 @@\n-old\n+new\n",
            "\x1b]1717;1;d;0;1;file;name\x07old\n\x1b]1717;1;a;1;;file;name\x07new\n",
        ),
        (
            "@@ -1 +1 @@\n-old\n+new\n@@ -1 +1 @@\n-old\n+new\n",
            "\x1b]1717;1;d;1;1;file;name\x07old\n\x1b]1717;1;a;1;;file;name\x07new\n",
        ),
    ] {
        let diff = rendered_diff(raw, rendered);
        assert!(diff.mapped_rendering(), "{rendered:?}");
        assert!(
            diff.selected_patch_rows(
                Presentation::Rendered,
                &diff.index(Presentation::Rendered).initial_selection()
            )
            .is_err(),
            "{rendered:?}"
        );
    }
}

#[test]
fn metadata_titles_and_unknown_records_never_become_selectable() {
    let diff = rendered_diff(
        "@@ -1 +1 @@\n-old\n+new\n",
        concat!(
            "\x1b]1717;1;f;;;file;name\x07\x1b]1717;1;h;1;;file;name\x07header only\n",
            "\x1b]1717;1;x;future;;unrelated\x07unknown only\n",
        ),
    );
    assert!(diff.mapped_rendering());
    let index = diff.index(Presentation::Rendered);
    let mut selection = index.initial_selection();
    assert!(!selection.select_at(index, 0, false));
    assert!(!selection.select_at(index, 1, false));
    assert!(
        diff.selected_patch_rows(Presentation::Rendered, &selection)
            .is_err()
    );
}

#[test]
fn a_split_display_block_excludes_interleaved_changes_from_highlighting_and_staging() {
    let diff = rendered_diff(
        "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n-old\n+new\n middle\n-last\n+next\n",
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07old\n",
            "\x1b]1717;1;d;3;3;file;name\x07\x1b]1717;1;a;3;;file;name\x07last -> next\n",
            "\x1b]1717;1;a;1;;file;name\x07new\n",
        ),
    );
    let index = diff.index(Presentation::Rendered);
    let selection = index.initial_selection();
    assert_eq!(
        (0..3)
            .map(|row| selection.contains(index, row))
            .collect::<Vec<_>>(),
        [true, false, true]
    );
    let (file, rows) = diff
        .selected_patch_rows(Presentation::Rendered, &selection)
        .unwrap();
    assert_eq!(
        RawPatch::parse(&file.raw)
            .unwrap()
            .selection(zess_git::DiffSide::Unstaged, &rows)
            .unwrap(),
        "--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n-old\n+new\n middle\n last\n"
    );
}

#[test]
fn hunk_navigation_keeps_change_order_from_a_later_display_fragment() {
    let diff = rendered_diff(
        "@@ -1,3 +1,3 @@\n-old\n+new\n middle\n-last\n+next\n",
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07old\n",
            "\x1b]1717;1;d;3;3;file;name\x07\x1b]1717;1;a;3;;file;name\x07last -> next\n",
            "\x1b]1717;1;a;1;;file;name\x07new\n",
        ),
    );
    let index = diff.index(Presentation::Rendered);
    let mut selection = index.initial_selection();
    assert!(selection.select_at(index, 2, false));
    selection.move_by(index, false, false);
    assert_eq!(selection.row(), Some(2));
    selection.move_by(index, true, false);
    assert_eq!(selection.row(), Some(1));
    selection.move_by(index, false, false);
    assert_eq!(selection.rows(index), Some(0..=2));
    assert!(!selection.contains(index, 1));
    selection.move_to(index, 2);
    assert_eq!(selection.row(), Some(2));
    selection.move_by(index, true, false);
    assert_eq!(selection.row(), Some(1));
}

#[test]
fn one_display_line_selects_every_complete_change_block_it_represents() {
    let diff = rendered_diff(
        "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n-old\n+new\n middle\n-last\n+next\n",
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07old\n",
            "\x1b]1717;1;a;1;;file;name\x07\x1b]1717;1;d;3;3;file;name\x07new / last\n",
            "\x1b]1717;1;a;3;;file;name\x07next\n",
        ),
    );
    let index = diff.index(Presentation::Rendered);
    let selection = index.initial_selection();
    assert_eq!(
        (0..3)
            .map(|row| selection.contains(index, row))
            .collect::<Vec<_>>(),
        [true, true, true]
    );
    let (file, rows) = diff
        .selected_patch_rows(Presentation::Rendered, &selection)
        .unwrap();
    assert_eq!(
        RawPatch::parse(&file.raw)
            .unwrap()
            .selection(zess_git::DiffSide::Unstaged, &rows)
            .unwrap(),
        "--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n-old\n+new\n middle\n-last\n+next\n"
    );
}

#[test]
fn resizing_preserves_the_original_block_when_its_display_rows_are_interleaved() {
    let raw = "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n-old\n+new\n middle\n-last\n+next\n";
    let old = rendered_diff(
        raw,
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07\x1b]1717;1;a;1;;file;name\x07old -> new\n",
            "\x1b]1717;1;d;3;3;file;name\x07\x1b]1717;1;a;3;;file;name\x07last -> next\n",
        ),
    );
    let new = rendered_diff(
        raw,
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07old\n",
            "\x1b]1717;1;d;3;3;file;name\x07\x1b]1717;1;a;3;;file;name\x07last -> next\n",
            "\x1b]1717;1;a;1;;file;name\x07new\n",
        ),
    );
    let mut selection = old.index(Presentation::Rendered).initial_selection();
    assert!(!selection.reconcile(&old, &new, Presentation::Rendered, Presentation::Rendered));
    let index = new.index(Presentation::Rendered);
    assert_eq!(
        (0..3)
            .map(|row| selection.contains(index, row))
            .collect::<Vec<_>>(),
        [true, false, true]
    );
    let (file, rows) = new
        .selected_patch_rows(Presentation::Rendered, &selection)
        .unwrap();
    assert_eq!(
        RawPatch::parse(&file.raw)
            .unwrap()
            .selection(zess_git::DiffSide::Unstaged, &rows)
            .unwrap(),
        "--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n-old\n+new\n middle\n last\n"
    );
}

#[test]
fn refreshing_after_staging_moves_the_selection_to_a_remaining_change_block() {
    let old = rendered_diff(
        "@@ -1,3 +1,3 @@\n-old\n+new\n middle\n-last\n+next\n",
        "unsupported renderer\n",
    );
    let new = rendered_diff(
        "@@ -1,3 +1,3 @@\n new\n middle\n-last\n+next\n",
        "unsupported renderer\n",
    );
    let mut selection = old.index.initial_selection();
    assert!(!selection.reconcile(&old, &new, Presentation::Raw, Presentation::Raw));
    assert!(matches!(selection, Selection::Block { .. }));
    assert_eq!(selection.rows(&new.index), Some(3..=4));
    let (_, rows) = new
        .selected_patch_rows(Presentation::Raw, &selection)
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|rows| (rows.start().0, rows.end().0))
            .collect::<Vec<_>>(),
        [(3, 4)]
    );
}

#[test]
fn a_valid_identity_cannot_hide_an_unresolved_identity_on_the_same_line() {
    let diff = rendered_diff(
        "@@ -1 +1 @@\n-old\n+new\n",
        "\x1b]1717;1;d;1;1;file;name\x07\x1b]1717;1;a;99;;file;name\x07old -> unknown\n",
    );
    let index = diff.index(Presentation::Rendered);
    let mut selection = Selection::Line { head: None };
    assert!(selection.select_at(index, 0, false));
    assert!(
        diff.selected_patch_rows(Presentation::Rendered, &selection)
            .is_err()
    );
}

#[test]
fn an_unresolved_identity_between_valid_range_endpoints_rejects_the_entire_selection() {
    let diff = rendered_diff(
        "@@ -1 +1 @@\n-old\n+new\n",
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07old\n",
            "\x1b]1717;1;a;99;;file;name\x07unknown\n",
            "\x1b]1717;1;a;1;;file;name\x07new\n",
        ),
    );
    let index = diff.index(Presentation::Rendered);
    let mut selection = Selection::Line { head: None };
    assert!(selection.select_at(index, 0, false));
    assert!(selection.select_at(index, 2, true));
    assert!(selection.contains(index, 1));
    assert!(
        diff.selected_patch_rows(Presentation::Rendered, &selection)
            .is_err()
    );
}

#[test]
fn absolute_metadata_paths_resolve_against_the_renderer_working_directory() {
    let diff = rendered_diff(
        "@@ -0,0 +1 @@\n+new\n",
        "\x1b]1717;1;a;1;;/workspace/./file;name\x07new\n",
    );
    assert!(diff.mapped_rendering());
    let diff = rendered_diff(
        "@@ -0,0 +1 @@\n+new\n",
        "\x1b]1717;1;a;1;;/another/file;name\x07new\n",
    );
    assert!(diff.mapped_rendering());
    assert!(
        diff.selected_patch_rows(
            Presentation::Rendered,
            &diff.index(Presentation::Rendered).initial_selection()
        )
        .is_err()
    );
}

#[test]
fn repository_relative_metadata_resolves_in_a_subdirectory_workspace() {
    for path in ["sub/file", "/repo/sub/file"] {
        let diff = Diff::from_git(
            zess_git::GitDiff {
                source: zess_git::DiffSource::Changes {
                    side: zess_git::DiffSide::Unstaged,
                },
                files: vec![zess_git::FileDiff {
                    target: zess_git::DiffTarget {
                        path: "file".into(),
                        original_path: None,
                    },
                    raw: "@@ -0,0 +1 @@\n+new\n".into(),
                    rendered: Some(zess_git::RenderedDiff {
                        file_header: false,
                        text: format!(
                            "\x1b]1717;1;x;future;;unrelated\x07\x1b]1717;1;a;1;;{path}\x07new\n"
                        ),
                        repository_root: "/repo".into(),
                    }),
                }],
            },
            Path::new("/repo/sub"),
        );
        assert!(diff.mapped_rendering(), "{path}");
        let (_, rows) = diff
            .selected_patch_rows(
                Presentation::Rendered,
                &diff.index(Presentation::Rendered).initial_selection(),
            )
            .unwrap();
        assert_eq!((rows[0].start().0, rows[0].end().0), (1, 1));
    }
}

#[test]
fn falling_back_to_raw_keeps_the_selected_change_block() {
    let raw = "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n-old\n+new\n middle\n-last\n+next\n";
    let old = rendered_diff(
        raw,
        concat!(
            "\x1b]1717;1;h;1;;file;name\x07header\n",
            "\x1b]1717;1;d;1;1;file;name\x07\x1b]1717;1;a;1;;file;name\x07old -> new\n",
            "\x1b]1717;1;c;2;;file;name\x07middle\n",
            "\x1b]1717;1;d;3;3;file;name\x07\x1b]1717;1;a;3;;file;name\x07last -> next\n",
        ),
    );
    let new = rendered_diff(raw, "unsupported renderer\n");
    let mut selection = old.index(Presentation::Rendered).initial_selection();
    assert!(selection.select_at(old.index(Presentation::Rendered), 3, false));
    assert!(!selection.reconcile(&old, &new, Presentation::Rendered, Presentation::Raw));
    assert_eq!(selection.rows(&new.index).unwrap(), 7..=8);
    let (file, rows) = new
        .selected_patch_rows(Presentation::Raw, &selection)
        .unwrap();
    let patch = RawPatch::parse(&file.raw)
        .unwrap()
        .selection(zess_git::DiffSide::Unstaged, &rows)
        .unwrap();
    assert_eq!(
        patch,
        "--- a/file;name\n+++ b/file;name\n@@ -1,3 +1,3 @@\n old\n middle\n-last\n+next\n"
    );
}

#[test]
fn a_layout_change_requires_reselection_when_the_same_lines_cannot_be_selected_exactly() {
    let raw = "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,2 +1,2 @@\n-old one\n-old two\n+new one\n+new two\n";
    let rendered = rendered_diff(
        raw,
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07\x1b]1717;1;a;1;;file;name\x07first pair\n",
            "\x1b]1717;1;d;1;2;file;name\x07\x1b]1717;1;a;2;;file;name\x07second pair\n",
        ),
    );
    let plain = rendered_diff(raw, "unsupported renderer\n");
    let mut selection = Selection::Line { head: None };
    assert!(selection.select_at(rendered.index(Presentation::Rendered), 0, false));
    assert!(selection.reconcile(&rendered, &plain, Presentation::Rendered, Presentation::Raw));
    assert!(selection.row().is_none());
    assert!(
        plain
            .selected_patch_rows(Presentation::Raw, &selection)
            .is_err()
    );
    assert!(!selection.reconcile(&plain, &plain, Presentation::Raw, Presentation::Raw));
    assert!(selection.row().is_none());
    selection.move_by(&plain.index, true, false);
    assert_eq!(selection.row(), Some(4));
    assert!(
        plain
            .selected_patch_rows(Presentation::Raw, &selection)
            .is_ok()
    );
    assert!(selection.reconcile(&plain, &rendered, Presentation::Raw, Presentation::Rendered));
    assert!(selection.row().is_none());
}

#[test]
fn unrelated_hunk_updates_still_require_reselection_when_the_layout_cannot_preserve_selected_lines()
{
    let raw = "diff --git a/file;name b/file;name\n--- a/file;name\n+++ b/file;name\n@@ -1,2 +1,2 @@\n-old one\n-old two\n+new one\n+new two\n";
    let rendered = rendered_diff(
        &format!("{raw}@@ -20 +20 @@\n-before\n+after\n"),
        concat!(
            "\x1b]1717;1;d;1;1;file;name\x07\x1b]1717;1;a;1;;file;name\x07first pair\n",
            "\x1b]1717;1;d;1;2;file;name\x07\x1b]1717;1;a;2;;file;name\x07second pair\n",
        ),
    );
    let plain = rendered_diff(
        &format!("{raw}@@ -20 +20 @@\n-before\n+updated\n"),
        "unsupported renderer\n",
    );
    let mut selection = Selection::Line { head: None };
    assert!(selection.select_at(rendered.index(Presentation::Rendered), 0, false));
    assert!(selection.reconcile(&rendered, &plain, Presentation::Rendered, Presentation::Raw));
    assert!(selection.row().is_none());
    assert!(
        plain
            .selected_patch_rows(Presentation::Raw, &selection)
            .is_err()
    );
}

#[test]
fn refreshing_an_unresolved_selection_requires_reselection_instead_of_preserving_its_valid_half() {
    let old = rendered_diff(
        "@@ -1 +1 @@\n-old\n+new\n",
        "\x1b]1717;1;d;1;1;file;name\x07\x1b]1717;1;a;99;;file;name\x07old -> unknown\n",
    );
    let new = rendered_diff(
        "@@ -1 +1 @@\n-old\n+new\n@@ -20 +20 @@\n-before\n+after\n",
        "unsupported renderer\n",
    );
    let mut selection = Selection::Line { head: None };
    assert!(selection.select_at(old.index(Presentation::Rendered), 0, false));
    assert!(selection.reconcile(&old, &new, Presentation::Rendered, Presentation::Raw));
    assert!(selection.row().is_none());
    assert!(
        new.selected_patch_rows(Presentation::Raw, &selection)
            .is_err()
    );
}

#[test]
fn rendered_selections_cannot_stage_changes_from_multiple_files() {
    let diff = Diff::from_git(
        zess_git::GitDiff {
            source: zess_git::DiffSource::Changes {
                side: zess_git::DiffSide::Unstaged,
            },
            files: ["first", "second"]
                .into_iter()
                .map(|path| zess_git::FileDiff {
                    target: zess_git::DiffTarget {
                        path: path.into(),
                        original_path: None,
                    },
                    raw: "@@ -0,0 +1 @@\n+new\n".into(),
                    rendered: Some(zess_git::RenderedDiff {
                        file_header: false,
                        text: format!("\x1b]1717;1;a;1;;{path}\x07new\n"),
                        repository_root: "/workspace".into(),
                    }),
                })
                .collect(),
        },
        Path::new("/workspace"),
    );
    assert!(diff.mapped_rendering());
    let index = diff.index(Presentation::Rendered);
    let mut selection = index.initial_selection();
    assert!(selection.select_at(index, 0, false));
    assert!(selection.select_at(index, 1, true));
    let error = diff
        .selected_patch_rows(Presentation::Rendered, &selection)
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "cannot stage a selection across files");
}

#[test]
fn context_separates_change_blocks_within_and_between_hunks() {
    let diff = Diff::new("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,5 +1,5 @@\n one\n-old\n+new\n middle\n-last\n+next\n end\n@@ -20 +20 @@\n-before\n+after\n".into());
    assert_eq!(diff.index.blocks, [5..=6, 8..=9, 12..=13]);
    let mut selection = diff.index.initial_selection();
    assert_eq!(selection.rows(&diff.index).unwrap(), 5..=6);
    selection.move_by(&diff.index, true, false);
    assert_eq!(selection.rows(&diff.index).unwrap(), 8..=9);
    selection.move_by(&diff.index, true, false);
    assert_eq!(selection.rows(&diff.index).unwrap(), 12..=13);
    selection.toggle(&diff.index);
    assert_eq!(selection.rows(&diff.index).unwrap(), 12..=12);
    selection.toggle_range();
    selection.move_by(&diff.index, true, false);
    assert_eq!(selection.rows(&diff.index).unwrap(), 12..=13);
    assert_eq!(diff.index.rows[12].source_line, Some(19));
    assert_eq!(diff.index.rows[13].new_line, Some(20));
    assert_eq!(diff.line(13), "+after");
}

#[test]
fn a_whole_file_change_initially_selects_its_entire_block() {
    for (raw, rows) in [
        ("@@ -1 +1 @@\n-old\n+new\n", 1..=2),
        ("@@ -0,0 +1,2 @@\n+first\n+second\n", 1..=2),
        ("@@ -1,2 +0,0 @@\n-first\n-second\n", 1..=2),
    ] {
        let diff = Diff::new(raw.into());
        let selection = diff.index.initial_selection();
        assert!(matches!(selection, Selection::Block { .. }), "{raw}");
        assert_eq!(selection.rows(&diff.index).unwrap(), rows, "{raw}");
    }
}

#[test]
fn context_without_changes_initially_selects_one_source_line() {
    let diff = Diff::new("@@ -1 +1 @@\n context\n".into());
    let selection = diff.index.initial_selection();
    assert!(matches!(selection, Selection::Line { .. }));
    assert_eq!(selection.rows(&diff.index).unwrap(), 1..=1);
}

#[test]
fn metadata_and_binary_patches_keep_their_raw_rows_without_changes() {
    for raw in [
        "",
        "diff --git a/f b/f\nold mode 100644\nnew mode 100755\n",
        "diff --git a/f b/f\nBinary files a/f and b/f differ\n",
    ] {
        let diff = Diff::new(raw.into());
        assert_eq!(diff.raw, raw);
        assert!(diff.index.blocks.is_empty());
        assert!(diff.index.initial_selection().rows(&diff.index).is_none());
        assert!(diff.index.initial_selection().row().is_none());
    }
}

#[test]
fn refreshing_context_keeps_the_selected_deletion_among_rows_with_the_same_new_line() {
    let old = Diff::new("@@ -1,3 +1,1 @@\n-aaa\n-bbb\n context\n".into());
    let new = Diff::new("@@ -1,3 +1,1 @@\n-aaa\n-bbb\n context updated\n".into());
    let mut selection = Selection::Range {
        anchor: SelectableRow(1),
        head: SelectableRow(2),
    };
    selection.reconcile(&old, &new, Presentation::Raw, Presentation::Raw);
    assert_eq!(selection.row(), Some(2));
    assert_eq!(selection.rows(&new.index).unwrap(), 1..=2);
    assert_eq!(new.line(selection.row().unwrap()), "-bbb");
    assert_eq!(new.index.rows[selection.row().unwrap()].old_line, Some(2));
}

#[test]
fn search_uses_smart_case_and_line_anchors_for_crlf_patches() {
    let diff = Diff::new("@@ -1,2 +1,2 @@\r\n context\r\n-old\r\n+Alpha\r\n".into());
    assert_eq!(
        diff.search(Presentation::Raw, "^\\+alpha$", Some(0), true)
            .unwrap()
            .map(|head| head.0),
        Some(3)
    );
    assert_eq!(
        diff.search(Presentation::Raw, "^\\+ALPHA$", Some(0), true)
            .unwrap()
            .map(|head| head.0),
        None
    );
    assert_eq!(
        diff.search(Presentation::Raw, "^ context$", Some(3), false)
            .unwrap()
            .map(|head| head.0),
        Some(1)
    );
    assert_eq!(
        diff.search(Presentation::Raw, "^\\+alpha$", Some(3), true)
            .unwrap()
            .map(|head| head.0),
        Some(3)
    );
}

#[test]
fn source_lines_follow_insertions_and_deletions_in_the_next_diff() {
    for (raw, expected) in [
        ("@@ -1,3 +1,4 @@\n+X\n A\n B\n C\n", [1, 2, 3, 4]),
        ("@@ -1,3 +1,2 @@\n A\n-B\n C\n", [0, 1, 1, 2]),
        ("@@ -0,0 +1 @@\n+X\n", [1, 2, 3, 4]),
        ("@@ -1,0 +2 @@\n+X\n", [0, 2, 3, 4]),
    ] {
        let diff = Diff::new(raw.into());
        for (line, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                diff.map_old_line(None, line),
                expected,
                "{raw:?}: line {line}"
            );
        }
    }
}

#[test]
fn line_navigation_skips_patch_headers_and_no_newline_markers() {
    let diff = Diff::new("diff --git a/f b/f\nindex abc..def 100644\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-before\n\\ No newline at end of file\n+after\n\\ No newline at end of file\n@@ -10 +10 @@\n-tail\n+end\n".into());
    let mut selection = diff.index.initial_selection();
    selection.toggle(&diff.index);
    assert_eq!(selection.row(), Some(5));
    selection.move_by(&diff.index, false, false);
    assert_eq!(selection.row(), Some(5));
    for expected in [7, 10, 11, 11] {
        selection.move_by(&diff.index, true, false);
        assert_eq!(selection.row(), Some(expected));
    }
    for expected in [10, 7, 5, 5] {
        selection.move_by(&diff.index, false, false);
        assert_eq!(selection.row(), Some(expected));
    }
    selection.move_by_lines(&diff.index, 2);
    assert_eq!(selection.row(), Some(10));
    selection.move_to_edge(&diff.index, true, None);
    assert_eq!(selection.row(), Some(11));
    selection.move_to_edge(&diff.index, false, None);
    assert_eq!(selection.row(), Some(5));
}

#[test]
fn ranges_include_source_lines_and_exclude_the_metadata_between_them() {
    let diff = Diff::new("@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n@@ -10,2 +10,2 @@\n context\n-tail\n+end\n".into());
    let mut selection = diff.index.initial_selection();
    assert_eq!(selection.rows(&diff.index).unwrap(), 1..=3);
    assert!(!selection.contains(&diff.index, 2));
    for row in [0, 2, 4, 5] {
        assert!(!selection.select_at(&diff.index, row, false));
        assert_eq!(selection.row(), Some(1));
        assert!(matches!(selection, Selection::Block { .. }));
    }
    assert!(selection.select_at(&diff.index, 1, false));
    assert!(selection.select_at(&diff.index, 8, true));
    assert_eq!(selection.rows(&diff.index).unwrap(), 1..=8);
    for row in 0..diff.index.rows.len() {
        assert_eq!(
            selection.contains(&diff.index, row),
            [1, 3, 6, 7, 8].contains(&row)
        );
    }
    assert!(!selection.select_at(&diff.index, 5, true));
    assert_eq!(selection.row(), Some(8));
    let patch_rows = selection.patch_rows(&diff.index).unwrap();
    assert_eq!((patch_rows.start().0, patch_rows.end().0), (1, 8));
    assert!(
        diff.search(Presentation::Raw, "newline|@@", selection.row(), true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        diff.search(Presentation::Raw, "context", selection.row(), true)
            .unwrap()
            .unwrap()
            .0,
        6
    );
}

#[test]
fn clicking_keeps_the_selection_mode_and_moves_its_head() {
    let diff = Diff::new("@@ -1,3 +1,3 @@\n-old\n+new\n context\n-last\n+next\n".into());
    let index = &diff.index;
    let mut selection = index.initial_selection();
    assert!(selection.select_at(index, 5, false));
    assert!(matches!(selection, Selection::Block { .. }));
    assert_eq!(selection.rows(index).unwrap(), 4..=5);
    assert!(selection.select_at(index, 3, false));
    assert!(matches!(selection, Selection::Block { .. }));
    assert_eq!(selection.rows(index).unwrap(), 4..=5);

    selection.toggle(index);
    assert!(selection.select_at(index, 2, false));
    assert!(matches!(selection, Selection::Line { .. }));
    assert_eq!(selection.rows(index).unwrap(), 2..=2);

    selection.toggle_range();
    assert!(selection.select_at(index, 5, false));
    assert!(matches!(selection, Selection::Range { .. }));
    assert_eq!(selection.rows(index).unwrap(), 2..=5);
    assert!(selection.select_at(index, 1, false));
    assert_eq!(selection.rows(index).unwrap(), 1..=2);
}

#[test]
fn dragging_selects_a_range_from_the_pressed_line() {
    let diff = Diff::new("@@ -1,3 +1,3 @@\n-old\n+new\n context\n-last\n+next\n".into());
    let index = &diff.index;
    let mut selection = index.initial_selection();
    assert!(selection.select_at(index, 2, false));
    assert!(selection.select_at(index, 5, true));
    assert!(matches!(selection, Selection::Range { .. }));
    assert_eq!(selection.rows(index).unwrap(), 2..=5);
    assert!(selection.select_at(index, 1, true));
    assert_eq!(selection.rows(index).unwrap(), 1..=2);
}

#[test]
fn patches_without_source_lines_never_create_a_selection() {
    for raw in [
        "",
        "diff --git a/f b/f\nBinary files a/f and b/f differ\n",
        "diff --git a/f b/g\nsimilarity index 100%\nrename from f\nrename to g\n",
        "@@ -0,0 +0,0 @@\n",
        "@@ -1,2 +1,2 @@\n-old\n+new\n",
    ] {
        let diff = Diff::new(raw.into());
        let mut selection = diff.index.initial_selection();
        selection.move_to(&diff.index, 0);
        selection.move_to_edge(&diff.index, true, None);
        selection.move_by(&diff.index, true, false);
        selection.move_by_lines(&diff.index, -10);
        selection.select_block(&diff.index);
        selection.toggle(&diff.index);
        selection.toggle_range();
        selection.focus_visible(&diff.index, 0..20);
        assert!(!selection.select_at(&diff.index, 0, true));
        assert!(selection.row().is_none(), "{raw}");
        assert!(selection.rows(&diff.index).is_none(), "{raw}");
        assert!(selection.patch_rows(&diff.index).is_none(), "{raw}");
        assert!(
            diff.search(Presentation::Raw, ".", None, true)
                .unwrap()
                .is_none(),
            "{raw}"
        );
    }
}

#[test]
fn refreshing_a_diff_clears_the_selection_when_its_text_hunks_disappear() {
    let old = Diff::new("@@ -1 +1 @@\n-old\n+new\n".into());
    let empty = Diff::new("diff --git a/f b/g\nrename from f\nrename to g\n".into());
    let mut selection = old.index.initial_selection();
    selection.toggle_range();
    selection.reconcile(&old, &empty, Presentation::Raw, Presentation::Raw);
    assert!(selection.row().is_none());
    assert!(selection.rows(&empty.index).is_none());
    assert!(matches!(selection, Selection::Line { head: None }));
    selection.reconcile(&empty, &old, Presentation::Raw, Presentation::Raw);
    assert!(matches!(selection, Selection::Line { head: Some(_) }));
    assert_eq!(selection.row(), Some(1));
}

#[test]
fn a_range_keeps_its_anchor_while_its_head_moves_between_source_lines() {
    let diff = Diff::new("@@ -1 +1 @@\n-old\n+new\n".into());
    let mut selection = diff.index.initial_selection();
    selection.toggle_range();
    selection.move_by_lines(&diff.index, 1);
    let Selection::Range { anchor, head } = selection else {
        panic!("moving the range head must preserve the range");
    };
    assert_eq!((anchor.0, head.0), (1, 2));
    assert_eq!(selection.rows(&diff.index).unwrap(), 1..=2);
    selection.move_by_lines(&diff.index, -1);
    let Selection::Range { anchor, head } = selection else {
        panic!("a collapsed range must retain both endpoints");
    };
    assert_eq!((anchor.0, head.0), (1, 1));
    assert_eq!(selection.rows(&diff.index).unwrap(), 1..=1);
}

#[test]
fn a_range_clears_when_the_viewport_contains_only_patch_metadata() {
    let diff = Diff::new("@@ -1 +1 @@\n-old\n+new\n".into());
    let mut selection = diff.index.initial_selection();
    selection.toggle_range();
    selection.move_by_lines(&diff.index, 1);
    selection.focus_visible(&diff.index, 0..1);
    assert!(matches!(selection, Selection::Line { head: None }));
    assert!(selection.rows(&diff.index).is_none());
    selection.toggle_range();
    assert!(matches!(selection, Selection::Line { head: None }));
    selection.move_by(&diff.index, true, false);
    assert!(matches!(selection, Selection::Line { head: Some(_) }));
    assert_eq!(selection.rows(&diff.index).unwrap(), 1..=1);
}

#[test]
fn refreshing_identical_lines_keeps_the_selected_file_when_file_order_changes() {
    let make = |paths: &[&str]| {
        Diff::from_git(
            zess_git::GitDiff {
                source: zess_git::DiffSource::Changes {
                    side: zess_git::DiffSide::Unstaged,
                },
                files: paths
                    .iter()
                    .map(|path| zess_git::FileDiff {
                        target: zess_git::DiffTarget {
                            path: (*path).into(),
                            original_path: None,
                        },
                        raw: "@@ -1 +1 @@\n-same\n+same change\n".into(),
                        rendered: None,
                    })
                    .collect(),
            },
            Path::new("/workspace"),
        )
    };
    let old = make(&["first", "second"]);
    let new = make(&["second", "first"]);
    let mut selection = Selection::Line { head: None };
    assert!(selection.select_at(&old.index, 5, false));
    selection.reconcile(&old, &new, Presentation::Raw, Presentation::Raw);
    let row = selection.row().unwrap();
    assert_eq!(new.file_at(row).unwrap().target.path, "second");
    assert_eq!(new.line(row), "+same change");
    assert_eq!(new.index.rows[row].patch_row.unwrap().0, 2);
}

#[test]
fn aggregate_metadata_and_no_newline_markers_never_become_selectable() {
    let diff = Diff::from_git(zess_git::GitDiff {
        source: zess_git::DiffSource::Changes { side: zess_git::DiffSide::Unstaged },
        files: vec![
            zess_git::FileDiff {
                target: zess_git::DiffTarget { path: "binary".into(), original_path: None },
                raw: "diff --git a/binary b/binary\nBinary files a/binary and b/binary differ\n".into(),
                rendered: None,
            },
            zess_git::FileDiff {
                target: zess_git::DiffTarget { path: "text".into(), original_path: None },
                raw: "diff --git a/text b/text\n--- a/text\n+++ b/text\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n".into(),
                rendered: None,
            },
        ],
    }, Path::new("/workspace"));
    let mut selection = diff.index.initial_selection();
    for row in 0..diff.index.rows.len() {
        let selected = selection.select_at(&diff.index, row, false);
        assert_eq!(selected, [6, 8].contains(&row));
    }
}

#[test]
fn staged_line_mapping_uses_only_the_matching_file() {
    let diff = Diff::from_git(
        zess_git::GitDiff {
            source: zess_git::DiffSource::Changes {
                side: zess_git::DiffSide::Unstaged,
            },
            files: ["first", "second"]
                .into_iter()
                .zip(["@@ -0,0 +1,2 @@\n+x\n+y\n", "@@ -0,0 +1 @@\n+z\n"])
                .map(|(path, raw)| zess_git::FileDiff {
                    target: zess_git::DiffTarget {
                        path: path.into(),
                        original_path: None,
                    },
                    raw: raw.into(),
                    rendered: None,
                })
                .collect(),
        },
        Path::new("/workspace"),
    );
    assert_eq!(diff.map_old_line(Some(&diff.files[0].target), 0), 2);
    assert_eq!(diff.map_old_line(Some(&diff.files[1].target), 0), 1);
}

#[test]
fn refreshing_external_output_keeps_the_browsed_file_after_reordering() {
    let make = |files: &[(&str, &str)]| {
        Diff::from_git(
            zess_git::GitDiff {
                source: zess_git::DiffSource::Changes {
                    side: zess_git::DiffSide::Unstaged,
                },
                files: files
                    .iter()
                    .map(|(path, rendered)| zess_git::FileDiff {
                        target: zess_git::DiffTarget {
                            path: (*path).into(),
                            original_path: None,
                        },
                        raw: "@@ -1 +1 @@\n-before\n+after\n".into(),
                        rendered: Some(zess_git::RenderedDiff {
                            file_header: false,
                            text: (*rendered).into(),
                            repository_root: "/workspace".into(),
                        }),
                    })
                    .collect(),
            },
            Path::new("/workspace"),
        )
    };
    let old = make(&[("first", "first\n"), ("second", "second\nsecond detail\n")]);
    let new = make(&[("second", "second\nsecond detail\n"), ("first", "first\n")]);
    let row = new.reconcile_rendered_scroll(&old, 2);
    assert_eq!(row, 1);
    let selection = new.raw_selection_at_rendered(row);
    assert_eq!(
        new.file_at(selection.row().unwrap()).unwrap().target.path,
        "second"
    );
    assert_eq!(new.rendered_row_at_raw(selection.row().unwrap()), 0);
}
