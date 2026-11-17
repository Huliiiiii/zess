use std::path::Path;

use super::*;

#[test]
fn horizontal_clipping_preserves_graphemes_and_the_original_line_ending() {
    let mut text = TextBuffer::new("xy界e\u{301}\r\nlast");
    let mut viewport = PreviewViewport {
        horizontal_scroll: 3,
        ..Default::default()
    };
    let lines = text.surface_source_lines(&mut viewport, 2, 1, false);
    assert_eq!(viewport.horizontal_scroll, 3);
    assert_eq!(lines[0].first_column, 2);
    assert_eq!(lines[0].first_char, 2);
    assert_eq!(lines[0].text, "界e\u{301}\r\n");
    assert!(lines[0].end_of_line);
}

#[test]
fn wrapped_fragments_start_at_the_first_visible_grapheme_and_include_empty_eof() {
    let mut text = TextBuffer::new("a界bcdef\r\n");
    let mut viewport = PreviewViewport {
        scroll: 1,
        ..Default::default()
    };
    let lines = text.surface_source_lines(&mut viewport, 4, 2, true);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (lines[0].line, lines[0].first_char, lines[0].first_column),
        (0, 3, 4)
    );
    assert_eq!(lines[0].text, "cdef\r\n");
    assert!(lines[0].end_of_line);
    assert_eq!(
        (lines[1].line, lines[1].first_char, lines[1].first_column),
        (1, 9, 0)
    );
    assert_eq!(lines[1].text, "");
    assert!(lines[1].end_of_line);
}

#[test]
fn syntax_highlights_use_document_character_offsets_after_multibyte_text() {
    let mut text = TextBuffer::new_with_path("; 猫\r\ndefine : f x\r\n", Path::new("plugin.w"));
    crate::tests::wait_for_highlight_refresh(&mut text);
    text.goto_line(1, 1);
    let lines = text.view_source_lines(1, 6, 1);
    assert_eq!(
        (lines[0].line, lines[0].first_char, lines[0].text.as_str()),
        (1, 5, "define")
    );
    assert!(!lines[0].end_of_line);
    assert!(lines[0].highlights.contains(&(5..11, "keyword".into())));
}
