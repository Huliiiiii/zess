use super::*;
use zess_protocol::SourceHighlight;

fn viewport() -> TextViewport<'static> {
    TextViewport {
        width: 8,
        height: 6,
        gutter: 0,
        horizontal_scroll: 0,
        tab_width: 4,
        selection: None,
        mode: "normal",
        focused: true,
        readonly: false,
        soft_wrap: false,
    }
}

fn line(number: usize, start: usize, text: &str) -> VisibleTextLine {
    VisibleTextLine {
        line: number,
        first_char: start,
        first_column: 0,
        end_of_line: true,
        highlights: vec![],
        text: text.into(),
    }
}

#[test]
fn a_scrolled_fragment_keeps_its_source_highlights_and_cursor_columns() {
    let mut line = line(4, 9, "\t界a\u{301}b\r\n");
    line.first_column = 3;
    line.highlights = vec![SourceHighlight {
        start: 10,
        end: 11,
        style: "keyword".into(),
    }];
    let selection = ViewSelection::Text {
        ranges: vec![
            TextRange {
                anchor: 11,
                head: 13,
            },
            TextRange {
                anchor: 13,
                head: 14,
            },
        ],
        primary: 0,
    };
    let (rows, cursor) = render(
        &[line],
        TextViewport {
            horizontal_scroll: 5,
            width: 6,
            gutter: 2,
            selection: Some(&selection),
            ..viewport()
        },
    );
    assert_eq!(cursor, Some((3, 0)));
    assert_eq!(
        rows[0]
            .spans
            .iter()
            .map(|span| (span.text.as_str(), span.style.as_str()))
            .collect::<Vec<_>>(),
        [
            ("5 ", "linenr.current"),
            (" ", "keyword"),
            ("a\u{301}", "selection"),
            ("b", "cursor")
        ]
    );
}

#[test]
fn wrapped_previews_start_at_the_visible_fragment_and_realign_tabs_after_a_wrap() {
    let mut first = line(9, 30, "ab界\tc\r\n");
    first.first_column = 11;
    let (rows, cursor) = render(
        &[first, line(10, 37, "d")],
        TextViewport {
            width: 3,
            soft_wrap: true,
            readonly: true,
            focused: false,
            ..viewport()
        },
    );
    let rows = rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(rows, ["ab", "界", "   ", "c", "d"]);
    assert_eq!(cursor, None);
}

#[test]
fn a_selection_ending_after_crlf_places_the_block_cursor_on_the_line_ending() {
    let lines = [line(0, 0, "ab\r\n"), line(1, 4, "")];
    let selection = ViewSelection::Text {
        ranges: vec![TextRange { anchor: 2, head: 4 }],
        primary: 0,
    };
    for (mode, expected) in [("normal", (2, 0)), ("insert", (0, 1))] {
        assert_eq!(
            render(
                &lines,
                TextViewport {
                    mode,
                    selection: Some(&selection),
                    ..viewport()
                }
            )
            .1,
            Some(expected)
        );
    }
}

#[test]
fn a_truncated_fragment_does_not_display_a_cursor_at_its_right_edge() {
    let mut fragment = line(0, 0, "ab");
    fragment.end_of_line = false;
    let selection = ViewSelection::Text {
        ranges: vec![TextRange { anchor: 2, head: 2 }],
        primary: 0,
    };
    assert_eq!(
        render(
            &[fragment],
            TextViewport {
                selection: Some(&selection),
                ..viewport()
            }
        )
        .1,
        None
    );
}
