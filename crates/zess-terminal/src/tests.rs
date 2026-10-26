use super::*;

#[test]
fn a_frame_is_presented_after_all_rows_are_written() {
    let frame = Frame {
        width: 20,
        height: 3,
        rows: vec![FrameRow {
            spans: vec![zess_ui::Span {
                base_style: None,
                text: "commit history".into(),
                style: String::new(),
            }],
        }],
        ..Default::default()
    };
    let mut output = Vec::new();
    render(&mut output, &frame, 20, 3, None).unwrap();
    let output = String::from_utf8(output).unwrap();
    let begin = output.find("\u{1b}[?2026h").unwrap();
    let content = output.find("commit history").unwrap();
    let end = output.find("\u{1b}[?2026l").unwrap();
    assert!(begin < content && content < end);
    assert_eq!(output.matches("\u{1b}[?2026h").count(), 1);
    assert_eq!(output.matches("\u{1b}[?2026l").count(), 1);
}


#[test]
fn narrow_rows_keep_joined_emoji_and_following_cells_visible() {
    crossterm::style::force_color_output(true);
    let row = FrameRow {
        spans: vec![zess_ui::Span {
            base_style: None,
            text: "a👩‍👩‍👧‍👦bcdef".into(),
            style: String::new(),
        }],
    };
    for (width, expected) in [
        (3, "\x1b[0m\x1b[39m\x1b[49ma👩‍👩‍👧‍👦\x1b[0m\x1b[0m"),
        (7, "\x1b[0m\x1b[39m\x1b[49ma👩‍👩‍👧‍👦bcde\x1b[0m\x1b[0m"),
    ] {
        let mut output = Vec::new();
        render_row(&mut output, &row, width, &Default::default()).unwrap();
        assert_eq!(output, expected.as_bytes());
    }
}
