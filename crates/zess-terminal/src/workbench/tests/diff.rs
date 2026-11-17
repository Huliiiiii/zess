use std::path::PathBuf;

use unicode_width::UnicodeWidthStr;
use zess_git::{
    DiffSide, DiffSource, DiffTarget, FileDiff, GitDiff, RenderedDiff, patch::PatchRow,
};
use zess_plugin::{ItemSelection, PluginOutput, PreviewContent, PreviewLayout, PreviewPane};
use zess_protocol::{
    ClientMessage,
    resource::{PagePatch, ResourceEvent, ResourceRequest},
};
use zess_ui::{Display, DisplayRow, Frame, MouseKind, StyleColor};

use super::*;

fn command(workbench: &mut Workbench, state: &SessionSnapshot, name: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    workbench
        .native(name, Value::Null, state, &mut effects)
        .unwrap();
    workbench.page_effects(state, &mut effects).unwrap();
    effects
}

fn browser() -> (Workbench, SessionSnapshot) {
    let mut state = SessionSnapshot::default();
    state.editor_options.mouse = true;
    let mut workbench = Workbench::default();
    workbench
        .apply_request(
            HostRequest::OpenBuffer {
                kind: "git".into(),
                argument: Value::Null,
            },
            &state,
        )
        .unwrap();
    (workbench, state)
}

fn file(path: &str, raw: &str, rendered: Option<&str>) -> FileDiff {
    FileDiff {
        target: DiffTarget {
            path: path.into(),
            original_path: None,
        },
        raw: raw.into(),
        rendered: rendered.map(|text| RenderedDiff {
            file_header: false,
            text: text.into(),
            repository_root: PathBuf::new(),
        }),
    }
}

fn publish(
    workbench: &mut Workbench,
    state: &SessionSnapshot,
    content: PreviewContent,
    focus: bool,
) {
    let subscription_id = workbench.pages.subscription().unwrap();
    let generation = workbench
        .pages
        .take_requests()
        .into_iter()
        .rev()
        .find_map(|message| match message {
            ClientMessage::Resource {
                request:
                    ResourceRequest::Subscribe {
                        subscription_id: id,
                        input,
                        ..
                    }
                    | ResourceRequest::Update {
                        subscription_id: id,
                        input,
                        ..
                    },
            } if id == subscription_id => Some(input.generation),
            _ => None,
        })
        .expect("expected the input for this preview publication");
    workbench
        .apply_resource(
            ResourceUpdate {
                cache: Default::default(),
                kind: Default::default(),
                document_scoped: false,
                subscription_id,
                generation,
                metadata: None,
                document: None,
                patch_revision: Some(7),
                output: PluginOutput {
                    display: Some(Display {
                        rows: vec![DisplayRow {
                            id: "file".into(),
                            text: "selected file".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    selection: Some(ItemSelection {
                        primary: Some("file".into()),
                        selected: vec!["file".into()],
                        anchor: Some("file".into()),
                    }),
                    requests: vec![HostRequest::PreviewGroup {
                        panes: vec![PreviewPane::View {
                            id: "unstaged".into(),
                            path: "working tree".into(),
                            content,
                            header: "Unstaged".into(),
                            loading: false,
                            key_scope: Some("git-diff".into()),
                        }],
                        layout: PreviewLayout::Rows,
                        focus,
                    }],
                    ..Default::default()
                },
            },
            state,
        )
        .unwrap();
}

fn git_diff(files: Vec<FileDiff>) -> PreviewContent {
    PreviewContent::GitDiff {
        diff: GitDiff {
            source: DiffSource::Changes {
                side: DiffSide::Unstaged,
            },
            files,
        },
    }
}

fn hit(frame: &Frame, marker: &str) -> (u16, u16) {
    for (row, line) in frame.rows.iter().enumerate() {
        let mut column = 0;
        for span in &line.spans {
            if let Some(offset) = span.text.find(marker) {
                return ((column + span.text[..offset].width()) as u16, row as u16);
            }
            column += span.text.width();
        }
    }
    panic!("expected a displayed row containing {marker:?}");
}

fn mouse(
    workbench: &mut Workbench,
    state: &SessionSnapshot,
    frame: &Frame,
    kind: MouseKind,
    marker: &str,
) {
    let (column, row) = hit(frame, marker);
    workbench
        .mouse(
            &MouseEvent { kind, column, row },
            state,
            frame.width,
            frame.height,
        )
        .unwrap();
}

fn selected(frame: &Frame) -> Vec<&str> {
    frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .filter(|span| {
            matches!(
                span.style.as_str(),
                "selection.git.added" | "selection.git.deleted" | "diff.selection"
            )
        })
        .map(|span| span.text.trim_end())
        .filter(|text| !text.is_empty())
        .collect()
}

fn patch(workbench: &mut Workbench, state: &SessionSnapshot) -> PagePatch {
    let effects = command(workbench, state, "diff.apply-selection");
    let [
        Effect::Command(CommandAction::ApplyPagePatch {
            subscription_id,
            request,
        }),
    ] = effects.as_slice()
    else {
        for effect in &effects {
            if let Effect::Notice(message) = effect {
                panic!("{message}");
            }
        }
        panic!("expected exactly one patch command");
    };
    assert_eq!(Some(*subscription_id), workbench.pages.subscription());
    assert_eq!(request.revision, 7);
    assert_eq!(request.side, DiffSide::Unstaged);
    request.clone()
}

const REPLACEMENT: &str = concat!(
    "diff --git a/note.txt b/note.txt\n",
    "index 1111111..2222222 100644\n",
    "--- a/note.txt\n",
    "+++ b/note.txt\n",
    "@@ -1,2 +1,4 @@\n",
    " context\n",
    "-before\n",
    "+after\t\x1b\n",
    "+second addition\n",
    "+third addition\n",
);

#[test]
fn clicking_a_raw_change_keeps_its_whole_block_selected() {
    let (mut workbench, state) = browser();
    publish(
        &mut workbench,
        &state,
        git_diff(vec![file("note.txt", REPLACEMENT, None)]),
        false,
    );
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert!(selected(&frame).is_empty());
    assert_eq!(workbench.pages.scope(), Some("git"));

    mouse(
        &mut workbench,
        &state,
        &frame,
        MouseKind::Press,
        "+third addition",
    );
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert_eq!(
        selected(&frame),
        [
            "-before",
            "+after  ␛",
            "+second addition",
            "+third addition"
        ]
    );
    assert!(frame.cursor.is_none());
    assert!(!frame.rows.iter().flat_map(|row| &row.spans).any(|span| {
        span.text.contains('\x1b')
            || span.text.contains("selected file") && span.style.starts_with("selection")
    }));
    assert_eq!(
        patch(&mut workbench, &state).rows,
        [PatchRow(6)..=PatchRow(9)]
    );

    command(&mut workbench, &state, "preview.return");
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert!(selected(&frame).is_empty());
    assert!(frame.rows.iter().flat_map(|row| &row.spans).any(|span| {
        span.text.contains("selected file") && span.style.starts_with("selection")
    }));
    assert!(frame.cursor.is_none());
}

#[test]
fn dragging_diff_rows_selects_a_range_that_survives_refresh() {
    let (mut workbench, state) = browser();
    let content = |raw| git_diff(vec![file("note.txt", raw, None)]);
    publish(&mut workbench, &state, content(REPLACEMENT), true);
    let frame = workbench.render(&state, 100, 18).unwrap();
    mouse(&mut workbench, &state, &frame, MouseKind::Press, "+after");
    publish(&mut workbench, &state, content(REPLACEMENT), false);
    let frame = workbench.render(&state, 100, 18).unwrap();
    for (kind, marker) in [
        (MouseKind::Drag, "+second addition"),
        (MouseKind::Release, "+second addition"),
        (MouseKind::Drag, "+third addition"),
    ] {
        mouse(&mut workbench, &state, &frame, kind, marker);
    }
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert_eq!(selected(&frame), ["+after  ␛", "+second addition"]);

    let frame = workbench.render(&state, 100, 9).unwrap();
    let position = workbench.pages.snapshot().stack[0].preview.clone().unwrap();
    assert!(position.scroll > 0);

    mouse(
        &mut workbench,
        &state,
        &frame,
        MouseKind::Press,
        "+second addition",
    );
    let changed = REPLACEMENT.replace("+third addition", "+updated addition");
    publish(&mut workbench, &state, content(&changed), false);
    let frame = workbench.render(&state, 100, 9).unwrap();
    mouse(&mut workbench, &state, &frame, MouseKind::Drag, "-before");
    let frame = workbench.render(&state, 100, 9).unwrap();
    assert_eq!(selected(&frame), ["+after  ␛", "+second addition"]);
    assert_eq!(
        workbench.pages.snapshot().stack[0].preview.as_ref(),
        Some(&position)
    );
    assert!(frame.cursor.is_none());
    assert_eq!(
        patch(&mut workbench, &state).rows,
        [PatchRow(7)..=PatchRow(8)]
    );
}

#[test]
fn clicking_patch_metadata_preserves_the_selected_source_line() {
    let (mut workbench, state) = browser();
    let raw = concat!(
        "diff --git a/note.txt b/note.txt\n",
        "index 1111111..2222222 100644\n",
        "--- a/note.txt\n",
        "+++ b/note.txt\n",
        "@@ -5,2 +8,2 @@\n",
        " context\n",
        "-before\n",
        "\\ No newline at end of file\n",
        "+after\n",
    );
    publish(
        &mut workbench,
        &state,
        PreviewContent::Diff { text: raw.into() },
        true,
    );
    command(&mut workbench, &state, "diff.toggle-selection");
    let frame = workbench.render(&state, 100, 18).unwrap();
    for marker in [
        "diff --git",
        "index 1111111",
        "--- a/note.txt",
        "+++ b/note.txt",
        "@@ -5,2 +8,2 @@",
        "No newline at end",
    ] {
        mouse(&mut workbench, &state, &frame, MouseKind::Press, marker);
        let frame = workbench.render(&state, 100, 18).unwrap();
        assert_eq!(selected(&frame), ["-before"], "{marker}");
    }

    command(&mut workbench, &state, "diff.range-selection");
    command(&mut workbench, &state, "diff.move-next");
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert_eq!(selected(&frame), ["-before", "+after"]);
    for (marker, expected) in [(" context", "8 "), ("-before", "6 "), ("+after", "9 ")] {
        let (_, row) = hit(&frame, marker);
        let numbers = frame.rows[usize::from(row)]
            .spans
            .iter()
            .filter(|span| span.style.starts_with("linenr"))
            .map(|span| span.text.trim_start())
            .collect::<Vec<_>>();
        assert_eq!(numbers, [expected]);
    }
    assert!(frame.cursor.is_none());
}

#[test]
fn a_mapped_change_selects_the_original_patch_rows_in_block_and_line_modes() {
    let (mut workbench, state) = browser();
    let raw = concat!(
        "diff --git a/note.txt b/note.txt\n",
        "index 1111111..2222222 100644\n",
        "--- a/note.txt\n",
        "+++ b/note.txt\n",
        "@@ -1,4 +1,4 @@\n",
        "-old-a1\n-old-a2\n+new-a1\n+new-a2\n context\n-old-b\n+new-b\n",
    );
    let rendered = concat!(
        "\x1b]1717;1;f;;;note.txt\x07note.txt for review\n",
        "\x1b]1717;1;d;1;1;note.txt\x07\x1b]1717;1;a;1;;note.txt\x07A first\n",
        "\x1b]1717;1;d;4;4;note.txt\x07\x1b]1717;1;a;4;;note.txt\x07B only\n",
        "\x1b]1717;1;d;1;2;note.txt\x07\x1b]1717;1;a;2;;note.txt\x07A second\n",
    );
    publish(
        &mut workbench,
        &state,
        git_diff(vec![file("note.txt", raw, Some(rendered))]),
        false,
    );
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert!(selected(&frame).is_empty());
    mouse(&mut workbench, &state, &frame, MouseKind::Press, "A second");
    for (next, expected) in [
        (None, vec!["A first", "A second"]),
        (Some("diff.move-next"), vec!["B only"]),
        (Some("diff.move-previous"), vec!["A first", "A second"]),
    ] {
        if let Some(next) = next {
            command(&mut workbench, &state, next);
        }
        let frame = workbench.render(&state, 100, 18).unwrap();
        assert_eq!(selected(&frame), expected);
        assert!(frame.cursor.is_none());
    }
    let request = patch(&mut workbench, &state);
    assert_eq!(request.target.path, "note.txt");
    assert_eq!(
        request.rows,
        [
            PatchRow(5)..=PatchRow(5),
            PatchRow(6)..=PatchRow(6),
            PatchRow(7)..=PatchRow(7),
            PatchRow(8)..=PatchRow(8),
        ]
    );

    command(&mut workbench, &state, "diff.toggle-selection");
    let frame = workbench.render(&state, 100, 18).unwrap();
    mouse(&mut workbench, &state, &frame, MouseKind::Press, "A second");
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert_eq!(selected(&frame), ["A second"]);
    assert_eq!(
        patch(&mut workbench, &state).rows,
        [PatchRow(6)..=PatchRow(6), PatchRow(8)..=PatchRow(8)]
    );
}

#[test]
fn clicking_a_scrolled_external_preview_focuses_the_clicked_file() {
    let (mut workbench, state) = browser();
    let output = |label| {
        (0..8)
            .map(|row| format!("\x1b[31m{label} rendered {row:02}\x1b[0m\n"))
            .collect::<String>()
    };
    let first = output("first file");
    let second = output("second file");
    let first_raw = REPLACEMENT.replace("note.txt", "first.txt");
    let second_raw = REPLACEMENT
        .replace("note.txt", "second.txt")
        .replace("before", "second before");
    publish(
        &mut workbench,
        &state,
        git_diff(vec![
            file("first.txt", &first_raw, Some(&first)),
            file("second.txt", &second_raw, Some(&second)),
        ]),
        false,
    );
    let frame = workbench.render(&state, 100, 10).unwrap();
    let (column, row) = hit(&frame, "first file rendered 00");
    assert!(frame.rows[usize::from(row)].spans.iter().any(|span| {
        span.text.contains("first file rendered 00")
            && frame.styles[&span.style].foreground == Some(StyleColor::Indexed(1))
    }));
    for _ in 0..10 {
        workbench
            .mouse(
                &MouseEvent {
                    kind: MouseKind::ScrollDown,
                    column,
                    row,
                },
                &state,
                frame.width,
                frame.height,
            )
            .unwrap();
    }
    let frame = workbench.render(&state, 100, 10).unwrap();
    assert_eq!(workbench.pages.scope(), Some("git"));
    assert!(selected(&frame).is_empty());
    mouse(
        &mut workbench,
        &state,
        &frame,
        MouseKind::Press,
        "second file rendered 07",
    );
    let frame = workbench.render(&state, 100, 10).unwrap();
    assert_eq!(workbench.pages.scope(), Some("git-diff"));
    assert!(selected(&frame).contains(&"-second before"));
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .all(|span| { !span.text.contains("file rendered") })
    );
    assert_eq!(patch(&mut workbench, &state).target.path, "second.txt");

    command(&mut workbench, &state, "diff.toggle-selection");
    command(&mut workbench, &state, "diff.move-next");
    command(&mut workbench, &state, "preview.return");
    let frame = workbench.render(&state, 100, 10).unwrap();
    hit(&frame, "second file rendered 00");
    assert!(selected(&frame).is_empty());
    command(&mut workbench, &state, "preview.focus");
    let frame = workbench.render(&state, 100, 10).unwrap();
    assert_eq!(selected(&frame), ["+after  ␛"]);
    let request = patch(&mut workbench, &state);
    assert_eq!(request.target.path, "second.txt");
    assert_eq!(request.rows, [PatchRow(7)..=PatchRow(7)]);
}

#[test]
fn a_mapped_range_with_an_unresolved_row_cannot_be_staged() {
    let (mut workbench, state) = browser();
    let rendered = concat!(
        "\x1b]1717;1;d;2;2;note.txt\x07\x1b]1717;1;a;2;;note.txt\x07replacement\n",
        "\x1b]1717;1;a;99;;note.txt\x07unresolved\n",
        "\x1b]1717;1;a;3;;note.txt\x07second addition\n",
    );
    publish(
        &mut workbench,
        &state,
        git_diff(vec![file("note.txt", REPLACEMENT, Some(rendered))]),
        true,
    );
    let frame = workbench.render(&state, 100, 18).unwrap();
    command(&mut workbench, &state, "diff.toggle-selection");
    assert_eq!(
        patch(&mut workbench, &state).rows,
        [PatchRow(6)..=PatchRow(6), PatchRow(7)..=PatchRow(7)]
    );
    for (kind, marker) in [
        (MouseKind::Press, "replacement"),
        (MouseKind::Drag, "second addition"),
        (MouseKind::Release, "second addition"),
    ] {
        mouse(&mut workbench, &state, &frame, kind, marker);
    }
    let effects = command(&mut workbench, &state, "diff.apply-selection");
    assert!(matches!(
        effects.as_slice(),
        [Effect::Notice(message)] if message == "selected row has no reliable original patch mapping"
    ));
}

#[test]
fn mapped_file_headers_preserve_selection_and_source_navigation_uses_the_clicked_file() {
    let (mut workbench, state) = browser();
    let make_file = |name: &str| {
        let path = format!("dir/{name}.txt");
        let raw = format!(
            "diff --git a/{path} b/{path}\nindex 1111111..2222222 100644\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+new\n"
        );
        let rendered = format!(
            "\x1b]1717;1;d;1;1;{path}\x07{name} old\n\x1b]1717;1;a;1;;{path}\x07{name} new\n"
        );
        let mut file = file(&path, &raw, Some(&rendered));
        file.rendered.as_mut().unwrap().file_header = true;
        file
    };
    publish(
        &mut workbench,
        &state,
        git_diff(vec![make_file("first"), make_file("second")]),
        false,
    );
    let frame = workbench.render(&state, 100, 18).unwrap();
    hit(&frame, "dir/first.txt");
    mouse(
        &mut workbench,
        &state,
        &frame,
        MouseKind::Press,
        "dir/second.txt",
    );
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert_eq!(selected(&frame), ["first old", "first new"]);
    mouse(
        &mut workbench,
        &state,
        &frame,
        MouseKind::Press,
        "second new",
    );
    let frame = workbench.render(&state, 100, 18).unwrap();
    assert_eq!(selected(&frame), ["second old", "second new"]);
    let request = patch(&mut workbench, &state);
    assert_eq!(request.target.path, "dir/second.txt");
    assert_eq!(
        request.rows,
        [PatchRow(5)..=PatchRow(5), PatchRow(6)..=PatchRow(6)]
    );
    workbench.pages.take_requests();
    command(&mut workbench, &state, "diff.edit-source");
    assert!(matches!(
        workbench.pages.take_requests().as_slice(),
        [ClientMessage::Resource {
            request: ResourceRequest::Update {
                event: ResourceEvent::OpenSource {path, line: 0, fullscreen: true},
                ..
            }
        }] if path == "dir/second.txt"
    ));
}

#[test]
fn raw_diff_starts_at_the_hunk_when_patch_headers_do_not_fit() {
    for (lines, height, first_visible) in [
        (2, 18, "diff --git a/note.txt b/note.txt"),
        (40, 8, "@@ -0,0 +1,40 @@"),
    ] {
        let (mut workbench, state) = browser();
        let mut raw = format!(
            "diff --git a/note.txt b/note.txt\nindex 1111111..2222222 100644\n--- a/note.txt\n+++ b/note.txt\n@@ -0,0 +1,{lines} @@\n"
        );
        raw.extend((0..lines).map(|line| format!("+line {line}\n")));
        publish(
            &mut workbench,
            &state,
            PreviewContent::Diff { text: raw },
            true,
        );
        let frame = workbench.render(&state, 100, height).unwrap();
        let (_, header) = hit(&frame, "Unstaged");
        assert_eq!(hit(&frame, first_visible).1, header + 1);
        assert!(selected(&frame).contains(&"+line 0"));
    }
}

#[test]
fn a_binary_diff_has_no_text_selection_or_source_jump() {
    let (mut workbench, state) = browser();
    publish(
        &mut workbench,
        &state,
        PreviewContent::Diff {
            text: "diff --git a/f b/f\nBinary files a/f and b/f differ\n".into(),
        },
        true,
    );
    for name in [
        "diff.range-selection",
        "diff.move-next",
        "diff.move-previous",
        "diff.goto-first",
        "diff.goto-last",
    ] {
        command(&mut workbench, &state, name);
    }
    let frame = workbench.render(&state, 100, 18).unwrap();
    hit(&frame, "Binary files a/f and b/f differ");
    assert!(frame.cursor.is_none());
    assert!(selected(&frame).is_empty());
    workbench.pages.take_requests();
    let effects = command(&mut workbench, &state, "diff.edit-source");
    assert!(matches!(
        effects.as_slice(),
        [Effect::Notice(message)] if message == "this diff has no selected source line"
    ));
    assert!(workbench.pages.take_requests().is_empty());
}

#[test]
fn changing_a_mapped_diff_cancels_search_and_retains_the_presearch_selection() {
    let (mut workbench, state) = browser();
    workbench.editor.set_catalog(Default::default());
    let compact = concat!(
        "\x1b]1717;1;d;2;2;note.txt\x07\x1b]1717;1;a;2;;note.txt\x07replacement\n",
        "\x1b]1717;1;a;3;;note.txt\x07second addition\n",
        "\x1b]1717;1;a;4;;note.txt\x07third addition\n",
    );
    let decorated = format!("{}{compact}", "decoration\n".repeat(16));
    let preview = |output| git_diff(vec![file("note.txt", REPLACEMENT, Some(output))]);
    publish(&mut workbench, &state, preview(&decorated), true);
    command(&mut workbench, &state, "diff.toggle-selection");
    command(&mut workbench, &state, "diff.goto-last");
    let frame = workbench.render(&state, 100, 12).unwrap();
    assert_eq!(selected(&frame), ["third addition"]);

    command(&mut workbench, &state, "diff.search-forward");
    workbench.paste("replacement", &state).unwrap();
    let frame = workbench.render(&state, 100, 13).unwrap();
    assert!(frame.prompt.contains("replacement"));
    assert_eq!(selected(&frame), ["replacement"]);
    publish(&mut workbench, &state, preview(&decorated), false);
    let frame = workbench.render(&state, 100, 13).unwrap();
    assert!(frame.prompt.contains("replacement"));
    assert_eq!(selected(&frame), ["replacement"]);

    workbench.render(&state, 110, 13).unwrap();
    publish(&mut workbench, &state, preview(compact), false);
    let frame = workbench.render(&state, 110, 13).unwrap();
    assert!(frame.prompt.is_empty());
    assert_eq!(selected(&frame), ["third addition"]);
    assert_eq!(
        patch(&mut workbench, &state).rows,
        [PatchRow(9)..=PatchRow(9)]
    );
}

#[test]
fn focusing_a_scrolled_mapped_preview_selects_the_visible_change_block() {
    let (mut workbench, state) = browser();
    let raw = concat!(
        "diff --git a/note.txt b/note.txt\n",
        "--- a/note.txt\n",
        "+++ b/note.txt\n",
        "@@ -1,3 +1,3 @@\n",
        "-old-a\n+new-a\n context\n-old-b\n+new-b\n",
    );
    let rendered = format!(
        "\x1b]1717;1;d;1;1;note.txt\x07\x1b]1717;1;a;1;;note.txt\x07first change\n{}\x1b]1717;1;d;3;3;note.txt\x07\x1b]1717;1;a;3;;note.txt\x07second change\n",
        "decoration\n".repeat(20),
    );
    publish(
        &mut workbench,
        &state,
        git_diff(vec![file("note.txt", raw, Some(&rendered))]),
        false,
    );
    let frame = workbench.render(&state, 100, 10).unwrap();
    for _ in 0..10 {
        mouse(
            &mut workbench,
            &state,
            &frame,
            MouseKind::ScrollDown,
            "first change",
        );
    }
    let frame = workbench.render(&state, 100, 10).unwrap();
    assert_eq!(workbench.pages.scope(), Some("git"));
    assert!(selected(&frame).is_empty());
    let position = hit(&frame, "second change");

    command(&mut workbench, &state, "preview.focus");
    let frame = workbench.render(&state, 100, 10).unwrap();
    assert_eq!(hit(&frame, "second change"), position);
    assert_eq!(selected(&frame), ["second change"]);
    assert_eq!(
        patch(&mut workbench, &state).rows,
        [PatchRow(7)..=PatchRow(7), PatchRow(8)..=PatchRow(8)]
    );
}
