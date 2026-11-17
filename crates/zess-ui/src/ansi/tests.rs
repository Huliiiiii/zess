use super::*;

#[test]
fn each_rendered_row_keeps_all_diff_identities_without_inheriting_them() {
    let text = AnsiText::parse(concat!(
        "\x1b]1717;1\x1b\\",
        "\x1b]1717;1;f;;;dir/a;b\x1b\\\x1b]1717;1;h;1;;dir/a;b\x1b\\header\n",
        "\x1b]1717;1;d;2;2;dir/a;b\x1b\\\x1b]1717;1;a;2;;dir/a;b\x1b\\change\n",
        "decoration\n",
        "\x1b]1717;1;a;2;;dir/a;b\x07continued\n",
        "\x1b]1717;1;c;3;;dir/a;b\x1b\\\n",
    ));
    assert!(!text.invalid_diff_metadata);
    assert_eq!(text.rows.len(), 5);
    assert_eq!(
        text.diff_metadata.iter().map(Vec::len).collect::<Vec<_>>(),
        [2, 2, 0, 1, 1]
    );
    assert_eq!(text.diff_metadata[0][0].kind, DiffLineKind::File);
    assert_eq!(text.diff_metadata[0][1].kind, DiffLineKind::Hunk);
    assert_eq!(text.diff_metadata[1][0].kind, DiffLineKind::Deletion);
    assert_eq!(text.diff_metadata[1][0].old_line, Some(2));
    assert_eq!(text.diff_metadata[1][1], text.diff_metadata[3][0]);
    assert_eq!(text.diff_metadata[1][1].path, "dir/a;b");
    assert!(text.rows[4].spans.is_empty());
}

#[test]
fn malformed_diff_metadata_is_reported_without_exposing_terminal_sequences() {
    for record in [
        "1;a;;;file",
        "1;d;2;;file",
        "1;f;1;;file",
        "2;a;1;;file",
        "1;a;1;;",
    ] {
        let text = AnsiText::parse(&format!("\x1b]1717;{record}\x1b\\visible\n"));
        assert!(text.invalid_diff_metadata, "{record}");
        assert!(text.diff_metadata[0].is_empty());
        assert_eq!(text.rows[0].spans[0].text, "visible");
    }
}

#[test]
fn unknown_diff_record_types_do_not_invalidate_known_line_identities() {
    let text = AnsiText::parse(concat!(
        "\x1b]1717;1;x;future-fields;not-a-number;another-path\x07decoration\n",
        "\x1b]1717;1;y\x07\x1b]1717;1;a;1;;file\x07new\n",
    ));
    assert!(!text.invalid_diff_metadata);
    assert!(text.diff_metadata[0].is_empty());
    assert_eq!(text.diff_metadata[1].len(), 1);
    assert_eq!(text.diff_metadata[1][0].kind, DiffLineKind::Addition);
}

#[test]
fn terminal_colors_and_resets_become_independent_span_styles() {
    let text = AnsiText::parse("\x1b[1;31mred\x1b[22;38;2;12;34;56;48;5;200mrgb\x1b[0mplain\n");
    let spans = &text.rows[0].spans;
    assert_eq!(
        spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>(),
        ["red", "rgb", "plain"]
    );
    let red = &text.styles[&spans[0].style];
    assert_eq!(red.foreground, Some(StyleColor::Indexed(1)));
    assert!(red.attributes.contains(&StyleAttribute::Bold));
    let rgb = &text.styles[&spans[1].style];
    assert_eq!(
        rgb.foreground,
        Some(StyleColor::Rgb {
            red: 12,
            green: 34,
            blue: 56
        })
    );
    assert_eq!(rgb.background, Some(StyleColor::Indexed(200)));
    assert!(!rgb.attributes.contains(&StyleAttribute::Bold));
    assert_eq!(text.styles[&spans[2].style], TextStyle::reset());
}

#[test]
fn terminal_actions_and_hyperlink_metadata_never_reach_printable_rows() {
    let text = AnsiText::parse(
        "\x1b[2J\x1b[99;99H\x1b]8;;file:///tmp/private\x1b\\link\x1b]8;;\x1b\\\x07\r\n\n\t界",
    );
    assert_eq!(text.rows.len(), 3);
    assert_eq!(text.rows[0].spans[0].text, "link");
    assert!(text.rows[1].spans.is_empty());
    assert_eq!(text.rows[2].spans[0].text, "        界");
    assert!(
        text.rows
            .iter()
            .flat_map(|row| &row.spans)
            .all(|span| !span.text.chars().any(char::is_control))
    );
}

#[test]
fn tabs_align_after_graphemes_split_by_ansi_styles() {
    let text = AnsiText::parse("\x1b[31m👩\x1b[32m\u{200d}💻\tA\x1b[34m\tB\n\x1b[31m界\tC");
    let styled_spans = text.rows[0]
        .spans
        .iter()
        .map(|span| (span.text.as_str(), text.styles[&span.style].foreground))
        .collect::<Vec<_>>();
    assert_eq!(
        styled_spans,
        [
            ("👩", Some(StyleColor::Indexed(1))),
            ("\u{200d}💻      A", Some(StyleColor::Indexed(2))),
            ("       B", Some(StyleColor::Indexed(4))),
        ]
    );
    assert_eq!(text.rows[1].spans[0].text, "界      C");
}

#[test]
fn colon_color_parameters_preserve_truecolor_and_attribute_removal() {
    let text = AnsiText::parse("\x1b[38:2::20:40:60;4:3mcolor\x1b[4:0mplain");
    let spans = &text.rows[0].spans;
    let color = &text.styles[&spans[0].style];
    assert_eq!(
        color.foreground,
        Some(StyleColor::Rgb {
            red: 20,
            green: 40,
            blue: 60
        })
    );
    assert!(color.attributes.contains(&StyleAttribute::Underline));
    assert!(
        !text.styles[&spans[1].style]
            .attributes
            .contains(&StyleAttribute::Underline)
    );
}
