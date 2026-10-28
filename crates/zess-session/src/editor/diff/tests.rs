use super::*;

#[test]
fn context_separates_change_blocks_within_and_between_hunks() {
    let diff = Diff::new("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,5 +1,5 @@\n one\n-old\n+new\n middle\n-last\n+next\n end\n@@ -20 +20 @@\n-before\n+after\n".into());
    assert_eq!(diff.blocks, [5..=6, 8..=9, 12..=13]);
    let mut selection = diff.initial_selection();
    assert_eq!(selection.rows(&diff).unwrap(), 5..=6);
    selection.move_by(&diff, true, false);
    assert_eq!(selection.rows(&diff).unwrap(), 8..=9);
    selection.move_by(&diff, true, false);
    assert_eq!(selection.rows(&diff).unwrap(), 12..=13);
    selection.toggle(&diff);
    assert_eq!(selection.rows(&diff).unwrap(), 12..=12);
    selection.toggle_range();
    selection.move_by(&diff, true, false);
    assert_eq!(selection.rows(&diff).unwrap(), 12..=13);
    assert_eq!(diff.rows[12].source_line, Some(19));
    assert_eq!(diff.rows[13].new_line, Some(20));
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
        let selection = diff.initial_selection();
        assert!(matches!(selection, Selection::Block { .. }), "{raw}");
        assert_eq!(selection.rows(&diff).unwrap(), rows, "{raw}");
    }
}

#[test]
fn context_without_changes_initially_selects_one_source_line() {
    let diff = Diff::new("@@ -1 +1 @@\n context\n".into());
    let selection = diff.initial_selection();
    assert!(matches!(selection, Selection::Line { .. }));
    assert_eq!(selection.rows(&diff).unwrap(), 1..=1);
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
        assert!(diff.blocks.is_empty());
        assert!(diff.initial_selection().rows(&diff).is_none());
        assert!(diff.initial_selection().row().is_none());
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
    selection.reconcile(&old, &new);
    assert_eq!(selection.row(), Some(2));
    assert_eq!(selection.rows(&new).unwrap(), 1..=2);
    assert_eq!(new.line(selection.row().unwrap()), "-bbb");
    assert_eq!(new.rows[selection.row().unwrap()].old_line, Some(2));
}

#[test]
fn search_uses_smart_case_and_line_anchors_for_crlf_patches() {
    let diff = Diff::new("@@ -1,2 +1,2 @@\r\n context\r\n-old\r\n+Alpha\r\n".into());
    assert_eq!(
        diff.search("^\\+alpha$", Some(0), true)
            .unwrap()
            .map(|head| head.0),
        Some(3)
    );
    assert_eq!(
        diff.search("^\\+ALPHA$", Some(0), true)
            .unwrap()
            .map(|head| head.0),
        None
    );
    assert_eq!(
        diff.search("^ context$", Some(3), false)
            .unwrap()
            .map(|head| head.0),
        Some(1)
    );
    assert_eq!(
        diff.search("^\\+alpha$", Some(3), true)
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
    let mut selection = diff.initial_selection();
    selection.toggle(&diff);
    assert_eq!(selection.row(), Some(5));
    selection.move_by(&diff, false, false);
    assert_eq!(selection.row(), Some(5));
    for expected in [7, 10, 11, 11] {
        selection.move_by(&diff, true, false);
        assert_eq!(selection.row(), Some(expected));
    }
    for expected in [10, 7, 5, 5] {
        selection.move_by(&diff, false, false);
        assert_eq!(selection.row(), Some(expected));
    }
    selection.move_by_lines(&diff, 2);
    assert_eq!(selection.row(), Some(10));
    selection.move_to_edge(&diff, true, None);
    assert_eq!(selection.row(), Some(11));
    selection.move_to_edge(&diff, false, None);
    assert_eq!(selection.row(), Some(5));
}

#[test]
fn ranges_include_source_lines_and_exclude_the_metadata_between_them() {
    let diff = Diff::new("@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n@@ -10,2 +10,2 @@\n context\n-tail\n+end\n".into());
    let mut selection = diff.initial_selection();
    assert_eq!(selection.rows(&diff).unwrap(), 1..=3);
    assert!(!selection.contains(&diff, 2));
    for row in [0, 2, 4, 5] {
        assert!(!selection.select_at(&diff, row, false));
        assert_eq!(selection.row(), Some(1));
        assert!(matches!(selection, Selection::Block { .. }));
    }
    assert!(selection.select_at(&diff, 1, false));
    assert!(selection.select_at(&diff, 8, true));
    assert_eq!(selection.rows(&diff).unwrap(), 1..=8);
    for row in 0..diff.rows.len() {
        assert_eq!(
            selection.contains(&diff, row),
            [1, 3, 6, 7, 8].contains(&row)
        );
    }
    assert!(!selection.select_at(&diff, 5, true));
    assert_eq!(selection.row(), Some(8));
    let patch_rows = selection.patch_rows(&diff).unwrap();
    assert_eq!((patch_rows.start().0, patch_rows.end().0), (1, 8));
    assert!(
        diff.search("newline|@@", selection.row(), true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        diff.search("context", selection.row(), true)
            .unwrap()
            .unwrap()
            .0,
        6
    );
}

#[test]
fn clicking_keeps_the_selection_mode_and_moves_its_head() {
    let diff = Diff::new("@@ -1,3 +1,3 @@\n-old\n+new\n context\n-last\n+next\n".into());
    let index = &diff;
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
    let index = &diff;
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
        let mut selection = diff.initial_selection();
        selection.move_to(&diff, 0);
        selection.move_to_edge(&diff, true, None);
        selection.move_by(&diff, true, false);
        selection.move_by_lines(&diff, -10);
        selection.select_block(&diff);
        selection.toggle(&diff);
        selection.toggle_range();
        selection.focus_visible(&diff, 0..20);
        assert!(!selection.select_at(&diff, 0, true));
        assert!(selection.row().is_none(), "{raw}");
        assert!(selection.rows(&diff).is_none(), "{raw}");
        assert!(selection.patch_rows(&diff).is_none(), "{raw}");
        assert!(diff.search(".", None, true).unwrap().is_none(), "{raw}");
    }
}

#[test]
fn refreshing_a_diff_clears_the_selection_when_its_text_hunks_disappear() {
    let old = Diff::new("@@ -1 +1 @@\n-old\n+new\n".into());
    let empty = Diff::new("diff --git a/f b/g\nrename from f\nrename to g\n".into());
    let mut selection = old.initial_selection();
    selection.toggle_range();
    selection.reconcile(&old, &empty);
    assert!(selection.row().is_none());
    assert!(selection.rows(&empty).is_none());
    assert!(matches!(selection, Selection::Line { head: None }));
    selection.reconcile(&empty, &old);
    assert!(matches!(selection, Selection::Line { head: Some(_) }));
    assert_eq!(selection.row(), Some(1));
}

#[test]
fn a_range_keeps_its_anchor_while_its_head_moves_between_source_lines() {
    let diff = Diff::new("@@ -1 +1 @@\n-old\n+new\n".into());
    let mut selection = diff.initial_selection();
    selection.toggle_range();
    selection.move_by_lines(&diff, 1);
    let Selection::Range { anchor, head } = selection else {
        panic!("moving the range head must preserve the range");
    };
    assert_eq!((anchor.0, head.0), (1, 2));
    assert_eq!(selection.rows(&diff).unwrap(), 1..=2);
    selection.move_by_lines(&diff, -1);
    let Selection::Range { anchor, head } = selection else {
        panic!("a collapsed range must retain both endpoints");
    };
    assert_eq!((anchor.0, head.0), (1, 1));
    assert_eq!(selection.rows(&diff).unwrap(), 1..=1);
}

#[test]
fn a_range_clears_when_the_viewport_contains_only_patch_metadata() {
    let diff = Diff::new("@@ -1 +1 @@\n-old\n+new\n".into());
    let mut selection = diff.initial_selection();
    selection.toggle_range();
    selection.move_by_lines(&diff, 1);
    selection.focus_visible(&diff, 0..1);
    assert!(matches!(selection, Selection::Line { head: None }));
    assert!(selection.rows(&diff).is_none());
    selection.toggle_range();
    assert!(matches!(selection, Selection::Line { head: None }));
    selection.move_by(&diff, true, false);
    assert!(matches!(selection, Selection::Line { head: Some(_) }));
    assert_eq!(selection.rows(&diff).unwrap(), 1..=1);
}

#[test]
fn refreshing_identical_lines_keeps_the_selected_file_when_file_order_changes() {
    let make = |paths: &[&str]| {
        Diff::from_git(zess_git::GitDiff {
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
                })
                .collect(),
        })
    };
    let old = make(&["first", "second"]);
    let new = make(&["second", "first"]);
    let mut selection = Selection::Line { head: None };
    assert!(selection.select_at(&old, 5, false));
    selection.reconcile(&old, &new);
    let row = selection.row().unwrap();
    assert_eq!(new.file_at(row).unwrap().target.path, "second");
    assert_eq!(new.line(row), "+same change");
    assert_eq!(new.rows[row].patch_row.0, 2);
}

#[test]
fn aggregate_metadata_and_no_newline_markers_never_become_selectable() {
    let diff = Diff::from_git(zess_git::GitDiff {
        source: zess_git::DiffSource::Changes { side: zess_git::DiffSide::Unstaged },
        files: vec![
            zess_git::FileDiff {
                target: zess_git::DiffTarget { path: "binary".into(), original_path: None },
                raw: "diff --git a/binary b/binary\nBinary files a/binary and b/binary differ\n".into(),
            },
            zess_git::FileDiff {
                target: zess_git::DiffTarget { path: "text".into(), original_path: None },
                raw: "diff --git a/text b/text\n--- a/text\n+++ b/text\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n".into(),
            },
        ],
    });
    let mut selection = diff.initial_selection();
    for row in 0..diff.rows.len() {
        let selected = selection.select_at(&diff, row, false);
        assert_eq!(selected, [6, 8].contains(&row));
    }
}

#[test]
fn staged_line_mapping_uses_only_the_matching_file() {
    let diff = Diff::from_git(zess_git::GitDiff {
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
            })
            .collect(),
    });
    assert_eq!(diff.map_old_line(Some(&diff.files[0].target), 0), 2);
    assert_eq!(diff.map_old_line(Some(&diff.files[1].target), 0), 1);
}
