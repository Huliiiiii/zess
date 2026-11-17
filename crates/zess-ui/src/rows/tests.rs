use crate::{
    AnsiText, DisplayRow, ItemJump, Span, StyleColor, clip_spans, item_label, skip_columns,
};

#[test]
fn horizontal_clipping_respects_grapheme_boundaries_and_columns() {
    for (skip, expected) in [(1, "👩‍💻界Z"), (2, " 界Z"), (3, "界Z"), (4, " Z"), (5, "Z")]
    {
        let mut spans = [
            Span {
                base_style: None,
                text: "e\u{301}".into(),
                style: "prefix".into(),
            },
            Span {
                base_style: None,
                text: "👩‍💻界Z".into(),
                style: "body".into(),
            },
        ];

        skip_columns(&mut spans, skip);

        assert_eq!(spans.map(|span| span.text), ["", expected], "skip {skip}");
    }
}

#[test]
fn horizontal_clipping_respects_graphemes_split_by_ansi_styles() {
    for (text, skip, expected) in [
        ("\x1b[31me\x1b[32m\u{301}X", 1, "X"),
        ("\x1b[31m👩\x1b[32m\u{200d}💻X", 1, " X"),
        ("\x1b[31m👩\x1b[32m\u{200d}💻X", 2, "X"),
    ] {
        let mut decoded = AnsiText::parse(text);
        let spans = &mut decoded.rows[0].spans;

        skip_columns(spans, skip);

        assert_eq!(
            spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<Vec<_>>(),
            ["", expected],
            "{text:?} after skipping {skip} columns"
        );
        assert_eq!(
            decoded.styles[&spans[1].style].foreground,
            Some(StyleColor::Indexed(2))
        );
    }
}

#[test]
fn right_clipping_keeps_or_omits_a_grapheme_split_by_ansi_styles() {
    let mut decoded = AnsiText::parse("a\x1b[31m👩\x1b[32m\u{200d}💻X");
    let row = decoded.rows.pop().unwrap();
    for (width, expected) in [(0, ""), (2, "a "), (3, "a👩‍💻"), (4, "a👩‍💻X")] {
        let clipped = clip_spans(row.spans.clone(), width);
        let text = clipped
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(text, expected, "{width} columns");
    }
}

#[test]
fn a_jump_label_replaces_the_first_two_display_columns() {
    let jump = ItemJump::new(["item"].into_iter()).unwrap();
    for (text, visible_body) in [
        ("ae\u{301}tail", "tail"),
        ("👩‍💻tail", "tail"),
        ("a界tail", " tail"),
    ] {
        let row = DisplayRow {
            id: "item".into(),
            text: text.into(),
            depth: 1,
            ..DisplayRow::default()
        };

        let spans = item_label(&row, "body", Some(&jump));

        let rendered = spans
            .iter()
            .map(|span| (span.text.as_str(), span.style.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            [("  ", "body"), ("aa", "pending"), (visible_body, "body")],
            "{text}"
        );
    }
}
