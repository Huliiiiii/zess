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
        text(buffer.render_preview(&mut viewport, 10, 2, false)),
        ["    界a\u{301}🙂Z", "second"]
    );
    assert!(buffer.views.is_empty());
    viewport.scroll = 20;
    viewport.horizontal_scroll = 20;
    let rows = buffer.render_preview(&mut viewport, 10, 2, false);
    assert_eq!(viewport.scroll, 1);
    assert_eq!(viewport.horizontal_scroll, 10);
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .all(|span| span.style.is_empty())
    );
    assert!(buffer.views.is_empty());
    buffer.replace_readonly("short").unwrap();
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 10, 2, false)),
        ["short"]
    );
    assert_eq!(viewport.scroll, 0);
    assert_eq!(viewport.horizontal_scroll, 0);
    assert!(buffer.views.is_empty());
    viewport.horizontal_scroll = 20;
    let mut replacement = TextBuffer::new("tiny");
    assert_eq!(
        text(replacement.render_preview(&mut viewport, 10, 2, false)),
        ["tiny"]
    );
    assert_eq!(viewport.horizontal_scroll, 0);
    assert!(replacement.views.is_empty());
}

#[test]
fn wrapped_previews_scroll_through_visual_lines_and_reflow_after_resize() {
    let mut buffer = TextBuffer::new("abcdefghijklmnopqrstuvwx\ntail");
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
        text(buffer.render_preview(&mut viewport, 8, 2, true)),
        ["abcdefgh", "ijklmnop"]
    );
    let layout = buffer.preview_wrap.as_ref().unwrap();
    assert_eq!(layout.rows.len(), 3);
    assert!(!layout.complete);
    viewport.scroll = 99;
    viewport.horizontal_scroll = 99;
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 8, 2, true)),
        ["qrstuvwx", "tail"]
    );
    assert_eq!(viewport.scroll, 2);
    assert_eq!(viewport.horizontal_scroll, 0);
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 24, 2, true)),
        ["abcdefghijklmnopqrstuvwx", "tail"]
    );
    assert_eq!(viewport.scroll, 0);
    buffer.replace_readonly("short").unwrap();
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 8, 2, true)),
        ["short"]
    );
    assert!(buffer.views.is_empty());
}

#[test]
fn wrapped_previews_preserve_graphemes_and_expand_tabs() {
    let mut buffer = TextBuffer::new("\t界a\u{301}🙂z0123456789");
    let mut viewport = PreviewViewport::default();
    let rows = buffer.render_preview(&mut viewport, 8, 8, true);
    let text = rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        text.iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) <= 8),
        "{text:?}"
    );
    assert_eq!(text.concat(), "    界a\u{301}🙂z0123456789");
    viewport.scroll = 0;
    let rows = buffer.render_preview(&mut viewport, 8, 1, false);
    assert_eq!(
        rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "    界a\u{301} "
    );
}

#[test]
fn wrapped_previews_align_tabs_with_their_visual_line() {
    let mut buffer = TextBuffer::new("aaaaaaa🙂\tZ");
    let mut viewport = PreviewViewport::default();
    let rows = buffer.render_preview(&mut viewport, 8, 3, true);
    let text = rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(text, ["aaaaaaa", "🙂  Z"]);
}

#[test]
fn wrapped_previews_keep_text_after_tabs_and_wide_characters_in_narrow_viewports() {
    let mut buffer = TextBuffer::new("\t界Z");
    for width in [1, 2, 3] {
        let mut viewport = PreviewViewport::default();
        let rows = buffer.render_preview(&mut viewport, width, 12, true);
        let text = rows
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(
            text.iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= width)
        );
        assert!(text.concat().ends_with('Z'), "{width}: {text:?}");
    }
}

#[test]
fn wrapped_previews_preserve_empty_lines_and_the_final_line() {
    let mut buffer = TextBuffer::new("abcd\n\n界e\u{301}🙂\n");
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
        text(buffer.render_preview(&mut viewport, 4, 2, true)),
        ["abcd", ""]
    );
    viewport.scroll = 2;
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 4, 2, true)),
        ["界e\u{301}", "🙂"]
    );
    viewport.scroll = 99;
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 4, 2, true)),
        ["🙂", ""]
    );
    assert_eq!(viewport.scroll, 3);
    viewport.scroll = 1;
    assert_eq!(
        text(buffer.render_preview(&mut viewport, 4, 3, true)),
        ["", "界e\u{301}", "🙂"]
    );
}

#[test]
fn wrapped_previews_page_through_a_long_single_line_in_both_directions() {
    let lines = (0..10_000)
        .map(|index| format!("{index:05}界🙂e\u{301}|"))
        .collect::<Vec<_>>();
    let mut buffer = TextBuffer::new(&lines.concat());
    let mut viewport = PreviewViewport::default();
    for start in [0, 5000, 5001, 9997, 5000] {
        viewport.scroll = start;
        let rows = buffer.render_preview(&mut viewport, 11, 3, true);
        let text = rows
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(text, lines[start..start + 3]);
        assert_eq!(viewport.scroll, start);
    }
}

#[test]
fn command_count_applies_to_text_movement_and_is_consumed() {
    let mut buffer = TextBuffer::new("abcdefgh");
    buffer.push_command_count(1, 0);
    assert_eq!(buffer.view(1).command_count().value(), None);
    buffer.push_command_count(1, 3);
    assert_eq!(buffer.view(1).command_count().to_string(), "3");

    buffer.execute(1, TextCommand::MoveRight).unwrap();
    assert_eq!(buffer.view(1).cursor(), (0, 3));
    assert_eq!(buffer.view(1).command_count().value(), None);
}

#[test]
fn horizontal_scroll_stays_at_zero_when_lines_fit_the_viewport() {
    let mut buffer = TextBuffer::new("abc\nwxyz");

    assert_eq!(rendered_text(&buffer.render(1, 4, 2, false)), "abc\nwxyz");

    buffer.scroll_view(1, 20, 0);

    assert_eq!(rendered_text(&buffer.render(1, 4, 2, false)), "abc\nwxyz");
}

#[test]
fn preview_scroll_stops_when_the_last_line_reaches_the_bottom() {
    let mut buffer = TextBuffer::new("one\ntwo\nthree\nfour");
    assert_eq!(
        rendered_text(&buffer.render(1, 20, 3, false)),
        "one\ntwo\nthree"
    );
    buffer.scroll_view(1, 0, 20);
    assert_eq!(
        rendered_text(&buffer.render(1, 20, 3, false)),
        "two\nthree\nfour"
    );
    assert_eq!(
        rendered_text(&buffer.render(1, 20, 5, false)),
        "one\ntwo\nthree\nfour"
    );
}

#[test]
fn wisp_files_highlight_special_forms_strings_and_comments() {
    let mut buffer = TextBuffer::new_with_path(
        "\ndefine : greet name\n  string-append \"hello\" name ; comment\n",
        Path::new("plugin.w"),
    );
    let frame = buffer.render(1, 80, 4, false);
    for (fragment, style) in [
        ("define", "keyword"),
        ("\"hello\"", "string"),
        ("; comment", "comment"),
    ] {
        assert_eq!(styled_text(&frame, style), fragment);
    }
}

#[test]
fn preview_highlights_follow_the_latest_contents_after_background_parsing() {
    let mut buffer = TextBuffer::new_preview_with_path("// old comment\n", Path::new("test.rs"));
    buffer.set_readonly(true);
    buffer.replace_readonly("fn latest() {}\n").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !buffer.poll_syntax() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let rendered = buffer.render(1, 80, 1, false);
    assert_eq!(styled_text(&rendered, "keyword.function"), "fn");
    assert_eq!(styled_text(&rendered, "comment.line"), "");
    assert!(buffer.syntax_error().is_none());

    buffer.replace_readonly("// new comment\n").unwrap();
    while !buffer.poll_syntax() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let rendered = buffer.render(1, 80, 1, false);
    assert_eq!(styled_text(&rendered, "comment.line"), "// new comment");
    assert_eq!(styled_text(&rendered, "keyword.function"), "");
}

#[test]
fn surround_and_comment_operations_use_the_current_selection() {
    let mut buffer = TextBuffer::new_with_path("one\ntwo\n", Path::new("test.rs"));
    buffer.execute_command(1, "text.select-lines").unwrap();
    buffer.execute_command(1, "text.toggle-comments").unwrap();
    assert_eq!(buffer.text(), "// one\ntwo\n");
    buffer.execute_command(1, "text.toggle-comments").unwrap();
    assert_eq!(buffer.text(), "one\ntwo\n");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.surround-add").unwrap();
    buffer.input_text(1, "(").unwrap();
    assert_eq!(buffer.text(), "(one\ntwo\n)");
    buffer
        .execute_command(1, "text.collapse-selection")
        .unwrap();
    buffer.execute_command(1, "text.move-left").unwrap();
    buffer.execute_command(1, "text.surround-replace").unwrap();
    buffer.input_text(1, "(").unwrap();
    buffer.input_text(1, "[").unwrap();
    assert_eq!(buffer.text(), "[one\ntwo\n]");
    buffer.execute_command(1, "text.surround-delete").unwrap();
    buffer.input_text(1, "[").unwrap();
    assert_eq!(buffer.text(), "one\ntwo\n");
}

#[test]
fn toggling_rust_documentation_comments_removes_the_complete_token() {
    let mut buffer = TextBuffer::new_with_path("/// docs\n", Path::new("test.rs"));
    buffer.execute_command(1, "text.select-lines").unwrap();
    buffer.execute_command(1, "text.toggle-comments").unwrap();
    assert_eq!(buffer.text(), "docs\n");
}

#[test]
fn horizontal_scroll_uses_display_columns_for_wide_text_tabs_and_crlf() {
    let mut buffer = TextBuffer::new("中\tX\r\n");

    assert_eq!(rendered_text(&buffer.render(1, 4, 1, false)), "中  ");

    buffer.scroll_view(1, 20, 0);

    assert_eq!(rendered_text(&buffer.render(1, 4, 1, false)), "   X");

    buffer.scroll_view(1, -20, 0);

    assert_eq!(rendered_text(&buffer.render(1, 4, 1, false)), "中  ");
}

#[test]
fn widening_the_viewport_or_shortening_content_clamps_horizontal_scroll() {
    let mut buffer = TextBuffer::new("abcdefghijkl");

    buffer.render(1, 4, 1, false);
    buffer.scroll_view(1, 20, 0);
    assert_eq!(rendered_text(&buffer.render(1, 4, 1, false)), "ijkl");

    assert_eq!(rendered_text(&buffer.render(1, 9, 1, false)), "defghijkl");

    buffer.set_readonly(true);
    buffer.replace_readonly("abc").unwrap();

    assert_eq!(rendered_text(&buffer.render(1, 9, 1, false)), "abc");
}

#[test]
fn clipboard_copy_preserves_selection_order_and_newlines_in_readonly_buffers() {
    let mut buffer = TextBuffer::new("中文\n甲乙--末尾");
    buffer.ensure_view(1);
    buffer.views.get_mut(&1).unwrap().selection = Selection::new(
        SmallVec::from_vec(vec![Range::new(0, 3), Range::new(7, 9)]),
        1,
    );
    buffer.set_readonly(true);
    buffer
        .registers
        .values
        .lock()
        .unwrap()
        .insert('"', vec!["原有".into()]);
    let selection = buffer.views[&1].selection.clone();
    let version = buffer.version();

    let effect = buffer.execute_command(1, "text.yank-to-clipboard").unwrap();

    assert_eq!(effect.clipboard.as_deref(), Some("中文\n\n末尾"));
    assert!(!effect.changed);
    assert_eq!(buffer.text(), "中文\n甲乙--末尾");
    assert_eq!(buffer.version(), version);
    assert_eq!(buffer.views[&1].selection.ranges(), selection.ranges());
    assert_eq!(
        buffer.views[&1].selection.primary_index(),
        selection.primary_index()
    );
    assert_eq!(
        buffer.registers.values.lock().unwrap().get(&'"').unwrap(),
        &["原有"]
    );
}

#[test]
fn ordinary_yank_and_edits_keep_using_internal_registers() {
    for command in ["text.yank", "text.delete", "text.change"] {
        let mut buffer = TextBuffer::new("中文");
        buffer.execute_command(1, "text.select-all").unwrap();

        let effect = buffer.execute_command(1, command).unwrap();

        assert!(effect.clipboard.is_none());
        assert_eq!(
            buffer.registers.values.lock().unwrap().get(&'"').unwrap(),
            &["中文"]
        );
        assert_eq!(
            buffer.text(),
            if command == "text.yank" { "中文" } else { "" }
        );
    }
}

#[test]
fn joining_multiple_selections_is_one_undoable_edit() {
    let original = "one\r\n  two\r\nthree\r\n four";
    let mut buffer = TextBuffer::new(original);
    buffer.ensure_view(1);
    buffer.views.get_mut(&1).unwrap().selection = Selection::new(
        SmallVec::from_vec(vec![Range::new(0, 1), Range::new(12, 13)]),
        1,
    );
    let selection = buffer.views[&1].selection.clone();

    assert!(buffer.execute_command(1, "text.join").unwrap().changed);
    assert_eq!(buffer.text(), "one two\r\nthree four");
    buffer.execute_command(1, "text.undo").unwrap();
    assert_eq!(buffer.text(), original);
    assert_eq!(buffer.views[&1].selection.ranges(), selection.ranges());
    assert_eq!(buffer.views[&1].selection.primary_index(), 1);
    buffer.execute_command(1, "text.redo").unwrap();
    assert_eq!(buffer.text(), "one two\r\nthree four");
}

#[test]
fn joining_lines_uses_selection_bounds_and_ignores_the_count_like_helix() {
    let mut buffer = TextBuffer::new("one\n two\n three\n four");
    buffer.execute_command(1, "text.count-3").unwrap();
    buffer.execute_command(1, "text.join").unwrap();
    assert_eq!(buffer.text(), "one two\n three\n four");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.join").unwrap();
    assert_eq!(buffer.text(), "one two three four");
}

#[test]
fn joining_and_selecting_spaces_skips_blank_joined_lines() {
    let mut buffer = TextBuffer::new("one\n  \ntwo\n three");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.join-select-space").unwrap();
    assert_eq!(buffer.text(), "one two three");
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(3, 4), Range::new(7, 8)]
    );
}

#[test]
fn joining_rust_comments_removes_matching_comment_leaders() {
    let mut buffer = TextBuffer::new_with_path(
        "/// one\n /// two\n // three\n // four",
        Path::new("sample.rs"),
    );
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.join").unwrap();
    assert_eq!(buffer.text(), "/// one two // three four");
}

fn choose_register(buffer: &mut TextBuffer, name: &str) {
    buffer.execute_command(1, "text.select-register").unwrap();
    buffer.input_text(1, name).unwrap();
}

#[test]
fn named_registers_are_shared_across_buffers_and_selected_for_one_command() {
    let registers = Registers::default();
    let mut source = TextBuffer::new("named");
    source.set_registers(registers.clone());
    source.execute_command(1, "text.select-all").unwrap();
    choose_register(&mut source, "a");
    source.execute_command(1, "text.yank").unwrap();
    let mut target = TextBuffer::new("X");
    target.set_registers(registers);
    target.execute_command(1, "text.yank").unwrap();
    choose_register(&mut target, "a");
    target.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(target.text(), "Xnamed");
    assert_eq!(target.views[&1].selection.ranges(), &[Range::new(1, 6)]);
    target.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(target.text(), "XnamedX");
    choose_register(&mut target, "a");
    target.execute_command(1, "text.move-left").unwrap();
    target.execute_command(1, "text.paste-before").unwrap();
    assert_eq!(target.text(), "XnameXdX");
}

#[test]
fn deleting_to_the_black_hole_preserves_the_default_register() {
    let mut buffer = TextBuffer::new("ab");
    buffer.execute_command(1, "text.yank").unwrap();
    buffer.execute_command(1, "text.move-right").unwrap();
    choose_register(&mut buffer, "_");
    buffer.execute_command(1, "text.delete").unwrap();
    assert_eq!(buffer.text(), "a");
    choose_register(&mut buffer, "_");
    buffer.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(buffer.text(), "a");
    buffer.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(buffer.text(), "aa");
}

#[test]
fn special_registers_read_current_selection_indices_contents_and_file_path() {
    let mut buffer = TextBuffer::new_with_path("ab cd", Path::new("sample.txt"));
    buffer.ensure_view(1);
    buffer.views.get_mut(&1).unwrap().selection = Selection::new(
        SmallVec::from_vec(vec![Range::new(0, 2), Range::new(3, 5)]),
        0,
    );
    choose_register(&mut buffer, "#");
    buffer.execute_command(1, "text.replace-with-yank").unwrap();
    assert_eq!(buffer.text(), "1 2");
    choose_register(&mut buffer, ".");
    buffer.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(buffer.text(), "11 22");
    buffer.execute_command(1, "text.keep-primary").unwrap();
    choose_register(&mut buffer, "%");
    buffer.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(buffer.text(), "11sample.txt 22");
    choose_register(&mut buffer, "#");
    assert!(buffer.execute_command(1, "text.yank").is_err());
    assert_eq!(buffer.text(), "11sample.txt 22");
}

#[test]
fn linewise_paste_inserts_complete_lines_and_selects_the_inserted_text() {
    let registers = Registers::default();
    let mut source = TextBuffer::new("one\ntwo\n");
    source.set_registers(registers.clone());
    source.execute_command(1, "text.select-lines").unwrap();
    source.execute_command(1, "text.yank").unwrap();
    let mut target = TextBuffer::new("abc\ndef");
    target.set_registers(registers);
    target.execute_command(1, "text.move-right").unwrap();
    target.execute_command(1, "text.count-2").unwrap();
    target.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(target.text(), "abc\none\none\ndef");
    assert_eq!(target.views[&1].selection.ranges(), &[Range::new(4, 12)]);
}

#[test]
fn searches_use_shared_registers_across_buffers() {
    let registers = Registers::default();
    let mut source = TextBuffer::new("alpha beta");
    source.set_registers(registers.clone());
    source.execute_command(1, "text.search-forward").unwrap();
    source
        .submit_prompt(1, TextPromptKind::SearchForward, "beta")
        .unwrap();
    let mut target = TextBuffer::new("beta alpha beta");
    target.set_registers(registers);
    target.execute_command(1, "text.search-next").unwrap();
    assert_eq!(target.views[&1].selection.ranges(), &[Range::new(11, 15)]);
    choose_register(&mut target, "/");
    target.execute_command(1, "text.paste-before").unwrap();
    assert_eq!(target.text(), "beta alpha betabeta");
}

#[test]
fn language_indentation_applies_to_tab_and_selected_lines() {
    let mut buffer = TextBuffer::new_with_path("x", Path::new("example.w"));
    buffer.execute_command(1, "text.insert").unwrap();
    buffer.execute_command(1, "text.insert-tab").unwrap();
    assert_eq!(buffer.text(), "  x");
    buffer.execute_command(1, "text.escape").unwrap();
    buffer.execute_command(1, "text.indent").unwrap();
    assert_eq!(buffer.text(), "    x");
    buffer.execute_command(1, "text.unindent").unwrap();
    assert_eq!(buffer.text(), "  x");
}

#[test]
fn scheme_and_wisp_files_use_two_space_indentation() {
    for path in [
        "example.scm",
        "example.ss",
        "example.sld",
        "example.w",
        "example.wisp",
    ] {
        let mut buffer = TextBuffer::new_with_path("\tx", Path::new(path));
        assert_eq!(
            buffer.language_id(),
            Some(if path.ends_with('w') || path.ends_with("wisp") {
                "wisp"
            } else {
                "scheme"
            })
        );
        assert_eq!(rendered_text(&buffer.render(1, 10, 1, false)), "  x");
        buffer.execute_command(1, "text.unindent").unwrap();
        assert_eq!(buffer.text(), "x");
        buffer.execute_command(1, "text.count-2").unwrap();
        buffer.execute_command(1, "text.indent").unwrap();
        assert_eq!(buffer.text(), "    x");
    }
}

#[test]
fn explicit_language_controls_indentation_after_file_detection() {
    let mut buffer = TextBuffer::new_with_path("x", Path::new("example.rs"));
    buffer.set_language(Some("scheme")).unwrap();
    buffer.execute_command(1, "text.indent").unwrap();
    assert_eq!(buffer.text(), "  x");
    buffer.set_language(None).unwrap();
    buffer.execute_command(1, "text.indent").unwrap();
    assert_eq!(buffer.text(), "    x");
}

#[test]
fn changing_language_clamps_scroll_to_the_new_tab_width() {
    let mut buffer = TextBuffer::new_with_path("\tx", Path::new("example.rs"));
    buffer.render(1, 2, 1, false);
    buffer.scroll_view(1, 20, 0);
    buffer.set_language(Some("scheme")).unwrap();
    assert_eq!(rendered_text(&buffer.render(1, 2, 1, false)), " x");
}

#[test]
fn indentation_changes_each_selected_line_once_and_respects_the_count() {
    let mut buffer = TextBuffer::new("a b\n\n  c\n\td");
    buffer.ensure_view(1);
    buffer.views.get_mut(&1).unwrap().selection = Selection::new(
        SmallVec::from_vec(vec![Range::new(0, 1), Range::new(2, 3), Range::new(5, 11)]),
        0,
    );
    buffer.execute_command(1, "text.count-2").unwrap();
    buffer.execute_command(1, "text.indent").unwrap();
    assert_eq!(buffer.text(), "        a b\n\n        c\n       \td");
    buffer.execute_command(1, "text.count-2").unwrap();
    buffer.execute_command(1, "text.unindent").unwrap();
    assert_eq!(buffer.text(), "a b\n\nc\nd");
}

#[test]
fn case_conversion_handles_unicode_expansion_and_undo() {
    let mut buffer = TextBuffer::new("aß中");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.uppercase").unwrap();
    assert_eq!(buffer.text(), "ASS中");
    buffer.execute_command(1, "text.undo").unwrap();
    assert_eq!(buffer.text(), "aß中");
    buffer.execute_command(1, "text.switch-case").unwrap();
    assert_eq!(buffer.text(), "ASS中");
    buffer.execute_command(1, "text.lowercase").unwrap();
    assert_eq!(buffer.text(), "ass中");
}

#[test]
fn splitting_selections_on_newlines_preserves_line_contents() {
    let mut buffer = TextBuffer::new("ab\r\ncd\nef");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.split-lines").unwrap();
    assert_eq!(
        buffer.views[&1]
            .selection
            .fragments(buffer.rope.slice(..))
            .collect::<Vec<_>>(),
        ["ab", "cd", "ef"]
    );
}

#[test]
fn flipping_selections_swaps_the_anchor_and_cursor() {
    let mut buffer = TextBuffer::new("abc");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.flip-selections").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(3, 0)]);
}

#[test]
fn bracket_matching_extends_selections_in_select_mode() {
    let mut buffer = TextBuffer::new("(a(b)c)");
    buffer.execute_command(1, "text.match-brackets").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(6, 7)]);
    buffer.execute_command(1, "text.select-mode").unwrap();
    buffer.execute_command(1, "text.match-brackets").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(7, 0)]);
}

#[test]
fn editing_commands_leave_select_mode_so_subsequent_movement_does_not_extend() {
    for command in [
        "text.delete",
        "text.delete-without-yank",
        "text.yank",
        "text.yank-to-clipboard",
        "text.paste-before",
        "text.paste-after",
        "text.replace-with-yank",
        "text.indent",
        "text.unindent",
        "text.switch-case",
        "text.lowercase",
        "text.uppercase",
    ] {
        let mut buffer = TextBuffer::new("ab cd");
        buffer.execute_command(1, "text.yank").unwrap();
        buffer.execute_command(1, "text.select-all").unwrap();
        buffer.execute_command(1, "text.select-mode").unwrap();
        buffer.execute_command(1, command).unwrap();
        assert_eq!(buffer.view(1).mode(), "normal", "{command}");
        buffer.execute_command(1, "text.move-left").unwrap();
        assert!(buffer.views[&1].selection.primary().len() <= 1, "{command}");
    }
}

#[test]
fn changing_without_yanking_preserves_the_default_register() {
    let mut buffer = TextBuffer::new("ab");
    buffer.execute_command(1, "text.yank").unwrap();
    buffer.execute_command(1, "text.move-right").unwrap();
    buffer
        .execute_command(1, "text.change-without-yank")
        .unwrap();
    assert_eq!(buffer.view(1).mode(), "insert");
    buffer.input_text(1, "c").unwrap();
    buffer.execute_command(1, "text.escape").unwrap();
    buffer.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(buffer.text(), "aca");
}

#[test]
fn clipboard_registers_read_and_write_the_requested_selection() {
    let mut buffer = TextBuffer::new("ab");
    choose_register(&mut buffer, "+");
    let effect = buffer.execute_command(1, "text.yank").unwrap();
    assert_eq!(effect.clipboard.as_deref(), Some("a"));
    choose_register(&mut buffer, "+");
    let request = buffer
        .execute_command(1, "text.paste-after")
        .unwrap()
        .clipboard_read
        .unwrap();
    assert_eq!(request.register, '+');
    buffer
        .complete_clipboard_read(1, request, "X".into())
        .unwrap();
    assert_eq!(buffer.text(), "aXb");
    choose_register(&mut buffer, "*");
    let effect = buffer.execute_command(1, "text.yank").unwrap();
    assert_eq!(effect.primary_clipboard.as_deref(), Some("X"));
    choose_register(&mut buffer, "*");
    let request = buffer
        .execute_command(1, "text.paste-after")
        .unwrap()
        .clipboard_read
        .unwrap();
    assert_eq!(request.register, '*');
    buffer
        .complete_clipboard_read(1, request, "Y".into())
        .unwrap();
    assert_eq!(buffer.text(), "aXYb");
}

#[test]
fn joining_a_readonly_buffer_keeps_its_text_and_selections() {
    let mut buffer = TextBuffer::new("a\n b");
    buffer.execute_command(1, "text.select-all").unwrap();
    let selection = buffer.views[&1].selection.clone();
    buffer.set_readonly(true);
    let effect = buffer.execute_command(1, "text.join").unwrap();
    assert!(!effect.changed);
    assert_eq!(effect.message.as_deref(), Some("buffer is read-only"));
    assert_eq!(buffer.text(), "a\n b");
    assert_eq!(buffer.views[&1].selection.ranges(), selection.ranges());
}

#[test]
fn changing_whole_lines_keeps_the_following_line_separate_and_inherits_its_indentation() {
    for command in ["text.change", "text.change-without-yank"] {
        let mut buffer = TextBuffer::new("one\n  two\n");
        buffer.execute_command(1, "text.select-lines").unwrap();
        buffer.execute_command(1, command).unwrap();
        buffer.input_text(1, "X").unwrap();
        buffer.execute_command(1, "text.escape").unwrap();
        assert_eq!(buffer.text(), "  X\n  two\n", "{command}");
        buffer.execute_command(1, "text.undo").unwrap();
        assert_eq!(buffer.text(), "one\n  two\n", "{command}");
    }
}

#[test]
fn cancelling_register_selection_discards_the_count_and_keeps_select_mode() {
    let mut buffer = TextBuffer::new("abcd");
    buffer.execute_command(1, "text.select-mode").unwrap();
    buffer.execute_command(1, "text.count-2").unwrap();
    buffer.execute_command(1, "text.select-register").unwrap();
    buffer.input_text(1, "Esc").unwrap();
    assert_eq!(buffer.view(1).mode(), "select");
    assert!(!buffer.has_pending_input(1));
    buffer.execute_command(1, "text.move-right").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 2)]);
}

#[test]
fn pending_commands_explain_the_expected_input_and_allow_escape_to_cancel() {
    for (command, label) in [
        ("text.select-register", "Register: type a name"),
        ("text.find-forward", "Find forward: type a character"),
        ("text.find-backward", "Find backward: type a character"),
        ("text.till-forward", "Till forward: type a character"),
        ("text.till-backward", "Till backward: type a character"),
        ("text.replace-character", "Replace: type a character"),
        ("text.textobject-inside", "Inside: type a text object"),
        ("text.textobject-around", "Around: type a text object"),
        ("text.surround-add", "Add surround: type a delimiter"),
        (
            "text.surround-replace",
            "Replace surround: type the old delimiter",
        ),
        ("text.surround-delete", "Delete surround: type a delimiter"),
    ] {
        let mut buffer = TextBuffer::new("(alpha beta)");
        buffer.execute_command(1, "text.select-mode").unwrap();
        buffer.execute_command(1, "text.count-2").unwrap();
        buffer.execute_command(1, command).unwrap();
        assert!(buffer.has_pending_input(1), "{command}");
        assert_eq!(
            buffer.pending_input(1),
            format!("{label} · Esc to cancel"),
            "{command}"
        );
        buffer.input_text(1, "Esc").unwrap();
        assert!(!buffer.has_pending_input(1), "{command}");
        assert!(buffer.pending_input(1).is_empty(), "{command}");
        assert!(buffer.view(1).command_count().is_empty(), "{command}");
        assert_eq!(buffer.view(1).mode(), "select", "{command}");
        assert_eq!(buffer.text(), "(alpha beta)", "{command}");
        assert_eq!(
            buffer.views[&1].selection.ranges(),
            &[Range::new(0, 1)],
            "{command}"
        );
    }
}

#[test]
fn selected_registers_remain_visible_until_used_or_cleared() {
    let mut buffer = TextBuffer::new("alpha");
    choose_register(&mut buffer, "a");
    assert_eq!(buffer.pending_input(1), "Register \"a");
    buffer.execute_command(1, "text.yank").unwrap();
    assert!(buffer.pending_input(1).is_empty());
    choose_register(&mut buffer, "a");
    buffer.execute_command(1, "text.escape").unwrap();
    assert!(buffer.pending_input(1).is_empty());
}

#[test]
fn replacing_a_surround_explains_when_to_enter_the_new_delimiter() {
    let mut buffer = TextBuffer::new("(alpha)");
    buffer.execute_command(1, "text.move-right").unwrap();
    buffer.execute_command(1, "text.surround-replace").unwrap();
    buffer.input_text(1, "(").unwrap();
    assert_eq!(
        buffer.pending_input(1),
        "Replace surround: type the new delimiter · Esc to cancel"
    );
    buffer.input_text(1, "Esc").unwrap();
    assert!(buffer.pending_input(1).is_empty());
    assert_eq!(buffer.text(), "(alpha)");
}

#[test]
fn word_jump_prompts_show_which_label_letter_is_still_needed() {
    let mut buffer = TextBuffer::new("alpha beta");
    buffer.render(1, 40, 2, true);
    buffer.execute_command(1, "text.goto-word").unwrap();
    assert_eq!(
        buffer.pending_input(1),
        "Goto word: type a two-letter label · Esc to cancel"
    );
    buffer.input_text(1, "a").unwrap();
    assert_eq!(
        buffer.pending_input(1),
        "Goto word: a_ (type the second letter) · Esc to cancel"
    );
    buffer.input_text(1, "a").unwrap();
    assert!(buffer.pending_input(1).is_empty());
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(6, 10)]);
}

#[test]
fn selecting_lines_extends_in_the_requested_direction() {
    let mut buffer = TextBuffer::new("a\nb\nc\nd");
    buffer.execute_command(1, "text.move-down").unwrap();
    buffer
        .execute_command(1, "text.select-lines-above")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(4, 2)]);
    buffer
        .execute_command(1, "text.select-lines-above")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(4, 0)]);
    buffer.execute_command(1, "text.extend-lines").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(4, 0)]);
    buffer.execute_command(1, "text.select-lines").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 6)]);
}

#[test]
fn repeating_insert_replays_text_deletion_newlines_tabs_and_cursor_movement() {
    let history = InsertHistory::default();
    let mut source = TextBuffer::new("");
    source.set_insert_history(history.clone());
    source.execute_command(1, "text.insert").unwrap();
    for ch in ["a", "b", "c"] {
        source.input_text(1, ch).unwrap();
    }
    source.execute_command(1, "text.delete-backward").unwrap();
    source.input_text(1, "X").unwrap();
    source.execute_command(1, "text.insert-newline").unwrap();
    source.input_text(1, "y").unwrap();
    source.input_text(1, "z").unwrap();
    source.execute_command(1, "text.move-left").unwrap();
    source.execute_command(1, "text.delete-backward").unwrap();
    source.input_text(1, "Q").unwrap();
    source.execute_command(1, "text.insert-tab").unwrap();
    source.execute_command(1, "text.escape").unwrap();
    assert_eq!(source.text(), "abX\nQ    z");
    let mut target = TextBuffer::new("tail");
    target.set_insert_history(history);
    target.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(target.text(), "abX\nQ    ztail");
    assert_eq!(target.view(1).mode(), "normal");
}

#[test]
fn repeated_insertions_preserve_the_original_sequence_and_have_separate_undo_steps() {
    let mut buffer = TextBuffer::new("z");
    buffer.execute_command(1, "text.append").unwrap();
    buffer.input_text(1, "a").unwrap();
    buffer.input_text(1, "b").unwrap();
    buffer.execute_command(1, "text.escape").unwrap();
    buffer.execute_command(1, "text.count-2").unwrap();
    buffer.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(buffer.text(), "zababab");
    buffer.execute_command(1, "text.undo").unwrap();
    assert_eq!(buffer.text(), "zabab");
    buffer.execute_command(1, "text.undo").unwrap();
    assert_eq!(buffer.text(), "zab");
    buffer.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(buffer.text(), "zabab");
    buffer.execute_command(1, "text.undo").unwrap();
    buffer.execute_command(1, "text.undo").unwrap();
    assert_eq!(buffer.text(), "z");
}

#[test]
fn repeating_open_lines_preserves_the_entry_command_and_its_count() {
    let mut buffer = TextBuffer::new("base");
    buffer.execute_command(1, "text.count-2").unwrap();
    buffer.execute_command(1, "text.open-below").unwrap();
    buffer.input_text(1, "X").unwrap();
    buffer.execute_command(1, "text.escape").unwrap();
    assert_eq!(buffer.text(), "base\n\nX");
    buffer.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(buffer.text(), "base\n\nX\n\nX");
    buffer.execute_command(1, "text.undo").unwrap();
    assert_eq!(buffer.text(), "base\n\nX");
}

#[test]
fn repeating_change_uses_the_current_selection_and_original_register() {
    let mut buffer = TextBuffer::new("ab");
    choose_register(&mut buffer, "a");
    buffer.execute_command(1, "text.change").unwrap();
    buffer.input_text(1, "X").unwrap();
    buffer.execute_command(1, "text.escape").unwrap();
    buffer.execute_command(1, "text.move-right").unwrap();
    buffer.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(buffer.text(), "XX");
    choose_register(&mut buffer, "a");
    buffer.execute_command(1, "text.paste-after").unwrap();
    assert_eq!(buffer.text(), "XXb");
}

#[test]
fn unfinished_insertions_and_other_views_do_not_replace_the_completed_history() {
    let history = InsertHistory::default();
    let mut source = TextBuffer::new("");
    source.set_insert_history(history.clone());
    source.execute_command(1, "text.insert").unwrap();
    source.input_text(1, "A").unwrap();
    source.execute_command(1, "text.escape").unwrap();
    source.execute_command(1, "text.append").unwrap();
    source.input_text(1, "B").unwrap();
    source.clone_view(1, 2);
    source.execute_command(2, "text.escape").unwrap();
    let mut target = TextBuffer::new("");
    target.set_insert_history(history);
    target.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(target.text(), "A");
    assert_eq!(source.text(), "AB");
    assert_eq!(source.view(1).mode(), "insert");
    source.input_text(1, "C").unwrap();
    source.execute_command(1, "text.escape").unwrap();
    target.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(target.text(), "ABC");
}

#[test]
fn repeat_insert_rejects_reentry_without_corrupting_the_recording() {
    let mut buffer = TextBuffer::new("");
    buffer.execute_command(1, "text.insert").unwrap();
    buffer.input_text(1, "A").unwrap();
    assert!(buffer.execute_command(1, "text.repeat-insert").is_err());
    buffer.input_text(1, "B").unwrap();
    buffer.execute_command(1, "text.escape").unwrap();
    buffer.execute_command(1, "text.repeat-insert").unwrap();
    assert_eq!(buffer.text(), "AABB");
    buffer.set_readonly(true);
    assert!(
        !buffer
            .execute_command(1, "text.repeat-insert")
            .unwrap()
            .changed
    );
    assert_eq!(buffer.text(), "AABB");
}

#[test]
fn selection_pattern_previews_always_start_from_the_original_selection() {
    let mut buffer = TextBuffer::new("red blue red");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.select-regex").unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::Select, "red")
        .unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(0, 3), Range::new(9, 12)]
    );
    buffer
        .preview_prompt(1, TextPromptKind::Select, "blue")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(4, 8)]);
    buffer
        .submit_prompt(1, TextPromptKind::Select, "red|blue")
        .unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(0, 3), Range::new(4, 8), Range::new(9, 12)]
    );
    assert!(!buffer.has_pending_input(1));
    assert_eq!(buffer.text(), "red blue red");
}

#[test]
fn cancelling_selection_patterns_restores_the_original_selection_and_mode() {
    let mut buffer = TextBuffer::new("red blue red");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.select-mode").unwrap();
    buffer.execute_command(1, "text.select-regex").unwrap();
    buffer.input_text(1, "r").unwrap();
    assert_eq!(buffer.view(1).selection_count(), 2);
    buffer.input_text(1, "Esc").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 12)]);
    assert_eq!(buffer.view(1).mode(), "select");
    assert!(!buffer.has_pending_input(1));
}

#[test]
fn invalid_or_unmatched_selection_patterns_keep_the_original_selection() {
    let mut buffer = TextBuffer::new("red blue red");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.select-regex").unwrap();
    for pattern in ["[", "missing", ""] {
        buffer
            .preview_prompt(1, TextPromptKind::Select, "red")
            .unwrap();
        buffer
            .preview_prompt(1, TextPromptKind::Select, pattern)
            .unwrap();
        assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 12)]);
    }
    buffer
        .preview_prompt(1, TextPromptKind::Select, "blue")
        .unwrap();
    let error = buffer
        .submit_prompt(1, TextPromptKind::Select, "missing")
        .unwrap_err();
    assert_eq!(error.to_string(), "nothing selected");
    assert_eq!(buffer.view(1).cursor(), (0, 11));
    assert!(buffer.has_pending_input(1));
    buffer
        .submit_prompt(1, TextPromptKind::Select, "blue")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(4, 8)]);
}

#[test]
fn selection_patterns_use_document_boundaries_instead_of_selection_boundaries() {
    let mut buffer = TextBuffer::new("prefix\nalpha\nsuffix");
    buffer.execute_command(1, "text.move-down").unwrap();
    buffer.execute_command(1, "text.select-lines").unwrap();
    buffer.execute_command(1, "text.select-regex").unwrap();
    buffer
        .submit_prompt(1, TextPromptKind::Select, "^|$")
        .unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(7, 8), Range::new(12, 13)]
    );

    let mut buffer = TextBuffer::new("foobar");
    buffer.execute_command(1, "text.count-3").unwrap();
    buffer.execute_command(1, "text.move-right").unwrap();
    buffer.execute_command(1, "text.select-regex").unwrap();
    let error = buffer
        .submit_prompt(1, TextPromptKind::Select, r"\b")
        .unwrap_err();
    assert_eq!(error.to_string(), "nothing selected");
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(3, 4)]);
}

#[test]
fn selection_patterns_follow_helix_smart_case_and_crlf_line_anchors() {
    let mut buffer = TextBuffer::new("Alpha\r\nalpha\r\n");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.select-regex").unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::Select, "^alpha$")
        .unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(0, 5), Range::new(7, 12)]
    );
    buffer
        .submit_prompt(1, TextPromptKind::Select, "^Alpha$")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 5)]);
}

#[test]
fn splitting_patterns_preserve_empty_fragments_as_block_cursors() {
    let mut buffer = TextBuffer::new(",a,,b,");
    buffer.execute_command(1, "text.select-all").unwrap();
    buffer.execute_command(1, "text.split-regex").unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::Split, "a")
        .unwrap();
    buffer.submit_prompt(1, TextPromptKind::Split, ",").unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[
            Range::new(0, 1),
            Range::new(1, 2),
            Range::new(3, 4),
            Range::new(4, 5),
        ]
    );
}

#[test]
fn word_jump_labels_choose_nearby_words_in_both_directions() {
    let mut buffer = TextBuffer::new("alpha beta gamma");
    buffer
        .execute_command(1, "text.move-word-start-forward")
        .unwrap();
    buffer.render(1, 40, 2, true);
    buffer.execute_command(1, "text.goto-word").unwrap();
    let rendered = buffer.render(1, 40, 2, true);
    assert_eq!(rendered_text(&rendered), "abpha aata acmma");
    assert!(
        rendered.rows[0]
            .spans
            .iter()
            .any(|span| span.style == "pending")
    );
    buffer.input_text(1, "a").unwrap();
    assert!(buffer.has_pending_input(1));
    buffer.input_text(1, "b").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 5)]);
    assert!(!buffer.has_pending_input(1));
    assert_eq!(
        rendered_text(&buffer.render(1, 40, 2, true)),
        "alpha beta gamma"
    );
    assert_eq!(buffer.text(), "alpha beta gamma");
}

#[test]
fn word_jumps_extend_the_primary_selection_in_select_mode() {
    let mut buffer = TextBuffer::new("alpha beta gamma");
    buffer
        .execute_command(1, "text.move-word-start-forward")
        .unwrap();
    buffer.execute_command(1, "text.select-mode").unwrap();
    buffer.render(1, 40, 2, true);
    buffer.execute_command(1, "text.goto-word").unwrap();
    buffer.input_text(1, "a").unwrap();
    buffer.input_text(1, "c").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 16)]);
    assert_eq!(buffer.view(1).mode(), "select");
}

#[test]
fn word_jump_cancellation_restores_text_without_moving_the_selection() {
    for keys in [["Esc", ""], ["z", ""], ["a", "z"], ["a", "Esc"]] {
        let mut buffer = TextBuffer::new("alpha beta");
        buffer.render(1, 40, 2, true);
        buffer.execute_command(1, "text.goto-word").unwrap();
        for key in keys.into_iter().filter(|key| !key.is_empty()) {
            buffer.input_text(1, key).unwrap();
        }
        assert!(!buffer.has_pending_input(1));
        assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 1)]);
        assert_eq!(rendered_text(&buffer.render(1, 40, 2, true)), "alpha beta");
    }
}

#[test]
fn word_jump_labels_only_cover_visible_words_with_multiple_graphemes() {
    let mut buffer = TextBuffer::new("alpha\nbeta a == γδ\ngamma");
    buffer.render(1, 40, 1, true);
    buffer.scroll_view(1, 0, 1);
    buffer.render(1, 40, 1, true);
    buffer.execute_command(1, "text.goto-word").unwrap();
    assert_eq!(
        rendered_text(&buffer.render(1, 40, 1, true)),
        "aata a == ab"
    );
    buffer.input_text(1, "a").unwrap();
    buffer.input_text(1, "b").unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(16, 18)]);
}

#[test]
fn custom_text_modes_keep_their_names_and_use_normal_movement() {
    let mut buffer = TextBuffer::new("ab");
    buffer.set_mode(1, "navigate/自定义").unwrap();
    assert_eq!(buffer.view(1).mode(), "navigate/自定义");
    assert_eq!(
        buffer.view(1).text_mode(),
        TextMode::Custom("navigate/自定义".into())
    );
    buffer.execute(1, TextCommand::MoveRight).unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(1, 2)]);
    assert_eq!(buffer.view(1).mode(), "navigate/自定义");

    buffer.set_text_mode(1, TextMode::Insert).unwrap();
    buffer.input_text(1, "X").unwrap();
    buffer.set_mode(1, "navigate/自定义").unwrap();
    assert_eq!(buffer.text(), "aXb");
    assert_eq!(buffer.view(1).mode(), "navigate/自定义");
    buffer.execute(1, TextCommand::Undo).unwrap();
    assert_eq!(buffer.text(), "ab");
}

#[test]
fn replacing_readonly_text_cancels_pattern_previews_before_remapping_selections() {
    let mut buffer = TextBuffer::new("one two three");
    buffer.set_readonly(true);
    buffer.execute(1, TextCommand::SelectAll).unwrap();
    buffer.execute(1, TextCommand::SelectRegex).unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::Select, "three")
        .unwrap();
    buffer.replace_readonly("one two three").unwrap();
    assert_eq!(buffer.pending_prompt(1), Some(TextPromptKind::Select));
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(8, 13)]);

    buffer.replace_readonly("x").unwrap();
    assert_eq!(buffer.pending_prompt(1), None);
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 1)]);
    buffer
        .preview_prompt(1, TextPromptKind::Select, "x")
        .unwrap();
    let error = buffer
        .submit_prompt(1, TextPromptKind::Select, "x")
        .unwrap_err();
    assert_eq!(error.to_string(), "text prompt is no longer active");
    buffer.execute(1, TextCommand::Escape).unwrap();
    assert_eq!(rendered_text(&buffer.render(1, 40, 2, true)), "x");
    assert_eq!(buffer.view(1).cursor(), (0, 0));
}

#[test]
fn edits_in_another_view_restore_and_map_the_original_pattern_selection() {
    let mut buffer = TextBuffer::new("one two three");
    buffer.execute(1, TextCommand::SelectAll).unwrap();
    buffer.execute(1, TextCommand::SelectRegex).unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::Select, "three")
        .unwrap();
    buffer.execute(2, TextCommand::SelectMode).unwrap();
    buffer.execute(2, TextCommand::Count3).unwrap();
    buffer.execute(2, TextCommand::MoveRight).unwrap();
    buffer.execute(2, TextCommand::Delete).unwrap();

    assert_eq!(buffer.text(), "two three");
    assert!(!buffer.has_pending_input(1));
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 9)]);
    buffer
        .preview_prompt(1, TextPromptKind::Select, "two")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 9)]);
    buffer.cancel_pending_command(1);
    assert_eq!(rendered_text(&buffer.render(1, 40, 2, true)), "two three");
}

#[test]
fn replacing_readonly_text_removes_word_jump_labels_and_targets() {
    let mut buffer = TextBuffer::new("alpha beta gamma");
    buffer.set_readonly(true);
    buffer.render(1, 40, 2, true);
    buffer.execute(1, TextCommand::GotoWord).unwrap();
    assert!(buffer.has_pending_input(1));
    buffer.replace_readonly("x").unwrap();
    assert!(!buffer.has_pending_input(1));
    buffer.input_text(1, "a").unwrap();
    buffer.input_text(1, "a").unwrap();
    assert_eq!(rendered_text(&buffer.render(1, 40, 2, true)), "x");
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(0, 1)]);
}

#[test]
fn edits_in_another_view_cancel_pending_surround_replacement() {
    let mut buffer = TextBuffer::new("(alpha) beta");
    buffer.execute(1, TextCommand::SurroundReplace).unwrap();
    buffer.input_text(1, "(").unwrap();
    assert!(buffer.has_pending_input(1));
    buffer.execute(2, TextCommand::SelectAll).unwrap();
    buffer.execute(2, TextCommand::Delete).unwrap();
    assert!(!buffer.has_pending_input(1));
    buffer.input_text(1, "[").unwrap();
    assert_eq!(buffer.text(), "");
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::point(0)]);
    assert_eq!(rendered_text(&buffer.render(1, 40, 2, true)), "");
}

#[test]
fn plain_text_has_no_language_features() {
    for path in [None, Some(Path::new("notes.txt"))] {
        let mut buffer = path.map_or_else(
            || TextBuffer::new("\nfn alpha() {}\n"),
            |path| TextBuffer::new_with_path("\nfn alpha() {}\n", path),
        );
        let frame = buffer.render(1, 40, 3, false);
        assert_eq!(rendered_text(&frame), "\nfn alpha() {}\n");
        assert!(
            frame
                .rows
                .iter()
                .flat_map(|row| &row.spans)
                .all(|span| span.style.is_empty())
        );
        assert_eq!(buffer.language_id(), None);
        assert!(buffer.outline().is_empty());
        assert!(buffer.language.is_none());
    }
}

#[test]
fn rust_indentation_works_before_the_first_render() {
    let mut buffer = TextBuffer::new_with_path("fn main() {", Path::new("first.rs"));
    buffer.execute(1, TextCommand::AppendLineEnd).unwrap();
    buffer.execute(1, TextCommand::InsertNewline).unwrap();
    assert_eq!(buffer.text(), "fn main() {\n    ");
    buffer.input_text(1, "}").unwrap();
    buffer.execute(1, TextCommand::Escape).unwrap();
    assert_eq!(buffer.text(), "fn main() {\n}");
    let outline = buffer.outline();
    assert_eq!(outline.len(), 1);
    assert_eq!(outline[0].name, "main");
    assert_eq!(
        rendered_text(&buffer.render(1, 40, 2, false)),
        "fn main() {\n}"
    );
}

#[test]
fn rust_buffers_keep_their_highlights_and_outlines_independent() {
    let mut first = TextBuffer::new_with_path("\nfn alpha() {}\n", Path::new("first.rs"));
    let mut second = TextBuffer::new_with_path("\nstruct Beta;\n", Path::new("second.rs"));
    assert_eq!(
        styled_text(&first.render(1, 40, 3, false), "keyword.function"),
        "fn"
    );
    assert_eq!(
        styled_text(&second.render(1, 40, 3, false), "keyword.storage.type"),
        "struct"
    );
    assert_eq!(first.outline()[0].name, "alpha");
    assert_eq!(second.outline()[0].name, "Beta");

    first.set_readonly(true);
    first.replace_readonly("\nfn changed() {}\n").unwrap();
    assert_eq!(first.outline()[0].name, "changed");
    assert_eq!(
        styled_text(&first.render(1, 40, 3, false), "keyword.function"),
        "fn"
    );
    assert_eq!(second.outline()[0].name, "Beta");
    assert_eq!(
        styled_text(&second.render(1, 40, 3, false), "keyword.storage.type"),
        "struct"
    );
}

#[test]
fn wisp_buffers_keep_their_highlights_independent_after_updates() {
    let mut first =
        TextBuffer::new_with_path("\ndefine : alpha\n  \"one\"\n", Path::new("first.w"));
    let mut second =
        TextBuffer::new_with_path("\ndefine : beta\n  #t ; keep\n", Path::new("second.w"));
    assert_eq!(
        styled_text(&first.render(1, 40, 4, false), "string"),
        "\"one\""
    );
    assert_eq!(
        styled_text(&second.render(1, 40, 4, false), "comment"),
        "; keep"
    );

    first.set_readonly(true);
    first
        .replace_readonly("\nlambda : x\n  \"changed\"\n")
        .unwrap();
    assert_eq!(
        styled_text(&first.render(1, 40, 4, false), "string"),
        "\"changed\""
    );
    let frame = second.render(1, 40, 4, false);
    assert_eq!(styled_text(&frame, "comment"), "; keep");
    assert_eq!(styled_text(&frame, "constant"), "#t");
}

#[test]
fn explicit_languages_override_file_name_detection() {
    let mut buffer =
        TextBuffer::new_with_path("\ndefine : alpha\n  \"one\"\n", Path::new("preview.rs"));
    assert_eq!(buffer.language_id(), Some("rust"));
    buffer.set_language(Some("wisp")).unwrap();
    assert_eq!(buffer.language_id(), Some("wisp"));
    assert_eq!(
        styled_text(&buffer.render(1, 40, 4, false), "keyword"),
        "define"
    );
    assert_eq!(
        styled_text(&buffer.render(1, 40, 4, false), "string"),
        "\"one\""
    );
    buffer.set_language(None).unwrap();
    assert_eq!(buffer.language_id(), None);
    assert!(buffer.outline().is_empty());
    assert!(
        buffer
            .render(1, 40, 4, false)
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .all(|span| span.style.is_empty())
    );
    buffer.set_language(Some("rust")).unwrap();
    assert!(buffer.set_language(Some("unknown-language")).is_err());
    assert_eq!(buffer.language_id(), Some("rust"));
}

#[test]
fn rust_syntax_follows_edits_undo_and_redo() {
    let mut buffer = TextBuffer::new_with_path("\nfn original() {}\n", Path::new("history.rs"));
    assert_eq!(buffer.outline()[0].name, "original");
    buffer.execute(1, TextCommand::SelectAll).unwrap();
    buffer.execute(1, TextCommand::Change).unwrap();
    for ch in "\nstruct Updated;\n".chars() {
        if ch == '\n' {
            buffer.execute(1, TextCommand::InsertNewline).unwrap();
        } else {
            buffer.input_text(1, &ch.to_string()).unwrap();
        }
    }
    buffer.execute(1, TextCommand::Escape).unwrap();
    assert_eq!(buffer.outline()[0].name, "Updated");
    assert_eq!(
        styled_text(&buffer.render(1, 40, 3, false), "keyword.storage.type"),
        "struct"
    );
    buffer.execute(1, TextCommand::Undo).unwrap();
    buffer.execute(1, TextCommand::GotoFileStart).unwrap();
    assert_eq!(buffer.outline()[0].name, "original");
    assert_eq!(
        styled_text(&buffer.render(1, 40, 3, false), "keyword.function"),
        "fn"
    );
    buffer.execute(1, TextCommand::Redo).unwrap();
    assert_eq!(buffer.outline()[0].name, "Updated");
    assert_eq!(
        styled_text(&buffer.render(1, 40, 3, false), "keyword.storage.type"),
        "struct"
    );
}

#[test]
fn wisp_highlights_follow_edits_undo_and_redo() {
    let mut buffer =
        TextBuffer::new_with_path("\ndefine : alpha\n  \"one\"\n", Path::new("history.wisp"));
    assert_eq!(buffer.language_id(), Some("wisp"));
    assert_eq!(
        styled_text(&buffer.render(1, 40, 4, false), "string"),
        "\"one\""
    );
    buffer.execute(1, TextCommand::SelectAll).unwrap();
    buffer.execute(1, TextCommand::Change).unwrap();
    for ch in "\nlambda : x\n  #t ; changed\n".chars() {
        if ch == '\n' {
            buffer.execute(1, TextCommand::InsertNewline).unwrap();
        } else {
            buffer.input_text(1, &ch.to_string()).unwrap();
        }
    }
    buffer.execute(1, TextCommand::Escape).unwrap();
    assert_eq!(
        styled_text(&buffer.render(1, 40, 4, false), "comment"),
        "; changed"
    );
    buffer.execute(1, TextCommand::Undo).unwrap();
    buffer.execute(1, TextCommand::GotoFileStart).unwrap();
    assert_eq!(
        styled_text(&buffer.render(1, 40, 4, false), "string"),
        "\"one\""
    );
    assert_eq!(styled_text(&buffer.render(1, 40, 4, false), "comment"), "");
    buffer.execute(1, TextCommand::Redo).unwrap();
    assert_eq!(
        styled_text(&buffer.render(1, 40, 4, false), "comment"),
        "; changed"
    );
    assert_eq!(styled_text(&buffer.render(1, 40, 4, false), "string"), "");
}

#[test]
fn saving_with_another_extension_updates_the_detected_language() {
    let directory = tempfile::tempdir().unwrap();
    let mut buffer = TextBuffer::new("\nfn original() {}\n");
    buffer
        .save(Some(&directory.path().join("original.rs")))
        .unwrap();
    assert_eq!(buffer.language_id(), Some("rust"));
    assert_eq!(buffer.outline()[0].name, "original");
    buffer
        .save(Some(&directory.path().join("changed.w")))
        .unwrap();
    assert_eq!(buffer.language_id(), Some("wisp"));
    assert!(buffer.outline().is_empty());
}

#[test]
fn saving_preserves_an_explicit_language_choice() {
    let directory = tempfile::tempdir().unwrap();
    let mut buffer = TextBuffer::new("\ndefine : alpha\n");
    buffer.set_language(Some("wisp")).unwrap();
    buffer
        .save(Some(&directory.path().join("preview.rs")))
        .unwrap();
    assert_eq!(buffer.language_id(), Some("wisp"));
    buffer.set_language(None).unwrap();
    buffer
        .save(Some(&directory.path().join("plain.rs")))
        .unwrap();
    assert_eq!(buffer.language_id(), None);
    assert!(buffer.outline().is_empty());
}

#[test]
fn reloading_updates_the_outline_and_highlights() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reload.rs");
    fs::write(&path, "\nfn original() {}\n").unwrap();
    let mut buffer = TextBuffer::open(&path).unwrap();
    assert_eq!(buffer.outline()[0].name, "original");
    fs::write(&path, "\nstruct Updated;\n").unwrap();
    buffer.reload(1).unwrap();
    assert_eq!(buffer.outline()[0].name, "Updated");
    assert_eq!(
        styled_text(&buffer.render(1, 40, 3, false), "keyword.storage.type"),
        "struct"
    );
}

#[test]
fn scrolling_into_a_multiline_comment_preserves_its_highlight() {
    let mut buffer =
        TextBuffer::new_with_path("/*\none\ntwo\nthree\n*/\n", Path::new("comment.rs"));
    buffer.render(1, 40, 2, false);
    buffer.scroll_view(1, 0, 2);
    let frame = buffer.render(1, 40, 2, false);
    assert_eq!(rendered_text(&frame), "two\nthree");
    assert_eq!(styled_text(&frame, "comment.block"), "twothree");
}

#[test]
fn missing_runtime_keeps_text_editable() {
    if std::env::var_os("ZESS_TEST_MISSING_RUNTIME").is_some() {
        let mut buffer = TextBuffer::new_with_path("\nfn original() {}\n", Path::new("missing.rs"));
        let frame = buffer.render(1, 40, 3, false);
        assert_eq!(rendered_text(&frame), "\nfn original() {}\n");
        assert!(
            frame
                .rows
                .iter()
                .flat_map(|row| &row.spans)
                .all(|span| span.style.is_empty())
        );
        assert!(buffer.syntax_error().is_some());
        assert!(buffer.outline().is_empty());
        buffer.execute(1, TextCommand::Insert).unwrap();
        for ch in "hello".chars() {
            buffer.input_text(1, &ch.to_string()).unwrap();
        }
        buffer.execute(1, TextCommand::Escape).unwrap();
        assert_eq!(buffer.text(), "hello\nfn original() {}\n");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let executable = directory
        .path()
        .join("text-tests")
        .with_extension(std::env::consts::EXE_EXTENSION);
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let result = std::process::Command::new(executable)
        .args([
            "--exact",
            "tests::missing_runtime_keeps_text_editable",
            "--nocapture",
        ])
        .env("ZESS_TEST_MISSING_RUNTIME", "1")
        .env("ZESS_RUNTIME", directory.path())
        .env("HELIX_RUNTIME", runtime::initialize(None).unwrap())
        .env("XDG_CONFIG_HOME", directory.path())
        .env("HOME", directory.path())
        .env_remove("CARGO_MANIFEST_DIR")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn joined_emoji_occupy_two_display_columns() {
    let mut buffer = TextBuffer::new("\n👨‍👩‍👧‍👦x\n");
    let frame = buffer.render(1, 3, 2, false);
    assert_eq!(rendered_text(&frame), "\n👨‍👩‍👧‍👦x");

    let frame = buffer.render(1, 2, 2, false);
    assert_eq!(rendered_text(&frame), "\n👨‍👩‍👧‍👦");
}

#[test]
fn zero_width_graphemes_keep_following_text_aligned_with_mouse_selection() {
    let mut buffer = TextBuffer::new("a\u{200b}b");
    let frame = buffer.render(1, 3, 1, false);

    assert_eq!(rendered_text(&frame), "a b");
    assert_eq!(frame.hit_positions[0], [0, 1, 2]);

    buffer.select_at(1, frame.hit_positions[0][2], false);
    buffer.execute_command(1, "text.delete").unwrap();

    assert_eq!(buffer.text(), "a\u{200b}");
}

#[test]
fn half_pages_move_the_text_viewport_and_leave_space_below_the_last_line() {
    let text = (0..40)
        .map(|index| format!("line-{index:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut buffer = TextBuffer::new(&text);
    buffer.render(1, 20, 20, true);
    buffer.execute(1, TextCommand::PageHalfDown).unwrap();
    let frame = buffer.render(1, 20, 20, true);
    assert_eq!(buffer.view(1).cursor().0, 10);
    assert_eq!(frame.scroll, 5);
    assert_eq!(
        rendered_text(&frame),
        (5..25)
            .map(|index| format!("line-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    buffer.execute(1, TextCommand::PageHalfUp).unwrap();
    assert_eq!(buffer.render(1, 20, 20, true).scroll, 0);
    assert_eq!(buffer.view(1).cursor().0, 0);
    for _ in 0..4 {
        buffer.execute(1, TextCommand::PageHalfDown).unwrap();
        buffer.render(1, 20, 20, true);
    }
    let frame = buffer.render(1, 20, 20, true);
    assert_eq!(buffer.view(1).cursor().0, 39);
    assert_eq!(frame.scroll, 34);
    assert_eq!(frame.rows.len(), 6);
    assert_eq!(
        rendered_text(&frame),
        "line-34\nline-35\nline-36\nline-37\nline-38\nline-39"
    );
}

#[test]
fn half_pages_keep_short_text_at_the_top() {
    let mut buffer = TextBuffer::new("one\ntwo\nthree");
    buffer.render(1, 20, 20, true);
    buffer.execute(1, TextCommand::PageHalfDown).unwrap();
    assert_eq!(buffer.render(1, 20, 20, true).scroll, 0);
    assert_eq!(buffer.view(1).cursor().0, 2);
}

#[test]
fn counted_half_pages_extend_text_selections_and_scroll_the_viewport() {
    let mut buffer = TextBuffer::new(&"line\n".repeat(40));
    buffer.render(1, 20, 20, true);
    for command in [
        TextCommand::SelectMode,
        TextCommand::Count2,
        TextCommand::PageHalfDown,
    ] {
        buffer.execute(1, command).unwrap();
    }
    assert_eq!(buffer.render(1, 20, 20, true).scroll, 15);
    let selection = &buffer.selections(1)[0];
    assert_eq!((selection.row, selection.anchor_row), (20, 0));
}

#[test]
fn search_previews_and_submission_use_the_original_selection() {
    let mut buffer = TextBuffer::new("red blue red blue");
    buffer.execute_command(1, "text.search-forward").unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::SearchForward, "blue")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(4, 8)]);
    buffer
        .preview_prompt(1, TextPromptKind::SearchForward, "red")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(9, 12)]);
    buffer
        .submit_prompt(1, TextPromptKind::SearchForward, "blue")
        .unwrap();
    assert_eq!(buffer.views[&1].selection.ranges(), &[Range::new(4, 8)]);
}

#[test]
fn searches_follow_smart_case_and_crlf_line_anchors() {
    for (pattern, expected) in [
        ("^alpha$", Range::new(8, 13)),
        ("^Alpha$", Range::new(15, 20)),
    ] {
        let mut buffer = TextBuffer::new("prefix\r\nALPHA\r\nAlpha\r\nalpha\r\n");
        buffer.execute_command(1, "text.search-forward").unwrap();
        buffer
            .submit_prompt(1, TextPromptKind::SearchForward, pattern)
            .unwrap();
        assert_eq!(buffer.views[&1].selection.ranges(), &[expected]);
    }
}

#[test]
fn searches_start_outside_the_primary_range_and_preserve_its_direction() {
    for (selection, prompt, expected) in [
        (
            Range::new(8, 0),
            TextPromptKind::SearchForward,
            Range::new(17, 13),
        ),
        (
            Range::new(0, 8),
            TextPromptKind::SearchBackward,
            Range::new(13, 17),
        ),
    ] {
        let mut buffer = TextBuffer::new("red blue red blue");
        buffer.ensure_view(1);
        buffer.views.get_mut(&1).unwrap().selection =
            Selection::single(selection.anchor, selection.head);
        let command = if prompt == TextPromptKind::SearchForward {
            "text.search-forward"
        } else {
            "text.search-backward"
        };
        buffer.execute_command(1, command).unwrap();
        buffer.submit_prompt(1, prompt, "blue").unwrap();
        assert_eq!(buffer.views[&1].selection.ranges(), &[expected]);
    }
}

#[test]
fn ordinary_searches_preserve_secondary_selections() {
    let mut buffer = TextBuffer::new("red blue red blue");
    buffer.ensure_view(1);
    buffer.views.get_mut(&1).unwrap().selection = Selection::new(
        SmallVec::from_vec(vec![Range::new(0, 3), Range::new(9, 12)]),
        0,
    );
    buffer.execute_command(1, "text.search-forward").unwrap();
    buffer
        .submit_prompt(1, TextPromptKind::SearchForward, "blue")
        .unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(4, 8), Range::new(9, 12)]
    );
}

#[test]
fn select_mode_adds_the_prompt_match_and_repeated_searches_move_only_the_primary() {
    let mut buffer = TextBuffer::new("red blue red blue");
    buffer.ensure_view(1);
    buffer.views.get_mut(&1).unwrap().selection = Selection::single(0, 3);
    buffer.execute_command(1, "text.select-mode").unwrap();
    buffer.execute_command(1, "text.search-forward").unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::SearchForward, "blue")
        .unwrap();
    buffer
        .submit_prompt(1, TextPromptKind::SearchForward, "blue")
        .unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(0, 3), Range::new(4, 8)]
    );
    buffer.execute_command(1, "text.search-next").unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(0, 3), Range::new(13, 17)]
    );
    buffer.execute_command(1, "text.search-previous").unwrap();
    assert_eq!(
        buffer.views[&1].selection.ranges(),
        &[Range::new(0, 3), Range::new(4, 8)]
    );
}

#[test]
fn cancelling_a_search_restores_the_selection_mode_and_viewport() {
    let mut buffer = TextBuffer::new(&format!("{}target\n", "line\n".repeat(80)));
    buffer.execute_command(1, "text.select-mode").unwrap();
    buffer.render(1, 20, 4, true);
    let original = buffer.views[&1].clone();
    buffer.execute_command(1, "text.search-forward").unwrap();
    buffer
        .preview_prompt(1, TextPromptKind::SearchForward, "target")
        .unwrap();
    buffer.render(1, 20, 4, true);
    assert!(buffer.views[&1].scroll > original.scroll);
    buffer.input_text(1, "Esc").unwrap();
    let view = &buffer.views[&1];
    assert_eq!(view.selection, original.selection);
    assert_eq!(view.mode, original.mode);
    assert_eq!(
        (view.scroll, view.horizontal_scroll, view.manual_viewport),
        (
            original.scroll,
            original.horizontal_scroll,
            original.manual_viewport
        )
    );
}

#[test]
fn selection_rendering_preserves_unicode_reverse_ranges_and_empty_cursors() {
    let mut buffer = TextBuffer::new("a\u{301} 中z");
    buffer.ensure_view(1);
    let view = buffer.views.get_mut(&1).unwrap();
    view.mode = TextMode::Insert;
    view.selection = Selection::new(
        SmallVec::from_vec(vec![Range::new(0, 2), Range::new(4, 3), Range::point(4)]),
        0,
    );
    let frame = buffer.render(1, 20, 2, true);
    assert_eq!(rendered_text(&frame), "a\u{301} 中z");
    assert_eq!(styled_text(&frame, "selection"), "a\u{301}");
    assert_eq!(styled_text(&frame, "cursor"), "中z");
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
fn reloading_maps_each_views_selection_through_the_file_changes() {
    let mut buffer = TextBuffer::new("first\nsecond\nthird\n");
    buffer.goto_line(1, 1);
    buffer.goto_line(2, 2);

    buffer.reload_contents("new\nfirst\nsecond\nthird\n");

    assert_eq!(buffer.view(1).cursor(), (2, 0));
    assert_eq!(buffer.view(2).cursor(), (3, 0));
    assert!(!buffer.modified());
    buffer.execute(1, TextCommand::Undo).unwrap();
    assert_eq!(buffer.text(), "first\nsecond\nthird\n");
    assert!(buffer.modified());
    buffer.execute(1, TextCommand::Redo).unwrap();
    assert_eq!(buffer.text(), "new\nfirst\nsecond\nthird\n");
    assert!(!buffer.modified());
}

#[test]
fn reloading_a_hidden_buffer_preserves_its_history_without_creating_a_view() {
    let mut buffer = TextBuffer::new("before\n");

    buffer.reload_contents("after\n");

    assert_eq!(buffer.text(), "after\n");
    assert!(buffer.views.is_empty());
    assert!(!buffer.modified());
    buffer.execute(1, TextCommand::Undo).unwrap();
    assert_eq!(buffer.text(), "before\n");
}

#[test]
fn undoing_a_manual_reload_restores_the_requesting_views_selection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("note.txt");
    fs::write(&path, "first\nsecond\nthird\n").unwrap();
    let mut buffer = TextBuffer::open(&path).unwrap();
    buffer.goto_line(1, 1);
    buffer.goto_line(2, 2);
    fs::write(&path, "new\nfirst\nsecond\nthird\n").unwrap();

    buffer.reload(2).unwrap();
    buffer.execute(2, TextCommand::Undo).unwrap();

    assert_eq!(buffer.text(), "first\nsecond\nthird\n");
    assert_eq!(buffer.view(2).cursor(), (2, 0));
}
