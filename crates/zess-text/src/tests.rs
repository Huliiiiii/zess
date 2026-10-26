use super::*;

fn rendered_text(result: &RenderResult) -> String {
    result
        .rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn styled_text(result: &RenderResult, style: &str) -> String {
    result
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .filter(|span| span.style == style)
        .map(|span| span.text.as_str())
        .collect()
}

#[test]
fn selection_and_secondary_cursor_styles_follow_focus_and_readonly_state() {
    for (readonly, focused, selected, cursors) in [
        (false, true, "01256", "79"),
        (false, false, "012567", ""),
        (true, true, "01256", "79"),
        (true, false, "", ""),
    ] {
        let mut buffer = TextBuffer::new("0123456789");
        buffer.set_readonly(readonly);
        buffer.ensure_view(1);
        buffer.views.get_mut(&1).unwrap().selection = Selection::new(
            SmallVec::from_vec(vec![Range::new(0, 3), Range::new(5, 8), Range::point(9)]),
            0,
        );

        let frame = buffer.render(1, 10, 1, focused);

        assert_eq!(rendered_text(&frame), "0123456789");
        assert_eq!(styled_text(&frame, "selection"), selected);
        assert_eq!(styled_text(&frame, "cursor"), cursors);
        assert_eq!(frame.cursor, focused.then_some((2, 0)));
    }
}

#[test]
fn selections_crossing_the_viewport_keep_their_styles_and_secondary_cursor() {
    let mut buffer = TextBuffer::new("abc\ndef\nghi\njkl");
    buffer.ensure_view(1);
    let view = buffer.views.get_mut(&1).unwrap();
    view.selection = Selection::new(
        SmallVec::from_vec(vec![Range::point(0), Range::new(2, 14)]),
        0,
    );
    view.scroll = 2;
    view.manual_viewport = true;

    let frame = buffer.render(1, 3, 2, true);

    assert_eq!(rendered_text(&frame), "ghi\njkl");
    assert_eq!(styled_text(&frame, "selection"), "ghij");
    assert_eq!(styled_text(&frame, "cursor"), "k");
    assert_eq!(frame.cursor, None);
    assert_eq!(frame.hit_positions, vec![vec![8, 9, 10], vec![12, 13, 14]]);
}

#[test]
fn selection_and_cursor_styles_override_syntax_only_within_their_ranges() {
    let mut buffer = TextBuffer::new_with_path("define x", Path::new("plugin.w"));
    buffer.ensure_view(1);
    buffer.views.get_mut(&1).unwrap().selection = Selection::new(
        SmallVec::from_vec(vec![Range::new(0, 2), Range::new(3, 5)]),
        0,
    );

    let frame = buffer.render(1, 20, 1, true);

    assert_eq!(rendered_text(&frame), "define x");
    assert_eq!(styled_text(&frame, "selection"), "dei");
    assert_eq!(styled_text(&frame, "cursor"), "n");
    assert_eq!(styled_text(&frame, "keyword"), "fe");
}

#[test]
fn preview_painting_uses_only_the_viewport_and_leaves_editing_views_unallocated() {
    let mut buffer = TextBuffer::new("\t界a\u{301}🙂Z0123456789\nsecond\nthird");
    buffer.set_readonly(true);
    let mut viewport = PreviewViewport::default();
    let text = |rows: Vec<FrameRow>| {
        rows.into_iter()
            .map(|row| {
                row.spans
                    .into_iter()
                    .map(|span| span.text)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 10, 2)),
        ["    界a\u{301}🙂Z", "second"]
    );
    assert!(buffer.views.is_empty());
    viewport.scroll = 20;
    viewport.horizontal_scroll = 20;
    let rows = buffer.render_preview(&mut viewport, 10, 2);
    assert_eq!(viewport.scroll, 1);
    assert_eq!(viewport.horizontal_scroll, 10);
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .all(|span| span.style.is_empty())
    );
    assert!(buffer.views.is_empty());
    buffer.replace_readonly("short").unwrap();
    assert_eq!(text(buffer.render_preview(&mut viewport, 10, 2)), ["short"]);
    assert_eq!(viewport.scroll, 0);
    assert_eq!(viewport.horizontal_scroll, 0);
    assert!(buffer.views.is_empty());
    viewport.horizontal_scroll = 20;
    let mut replacement = TextBuffer::new("tiny");
    assert_eq!(
        text(replacement.render_preview(&mut viewport, 10, 2)),
        ["tiny"]
    );
    assert_eq!(viewport.horizontal_scroll, 0);
    assert!(replacement.views.is_empty());
}
