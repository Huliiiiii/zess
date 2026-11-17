use super::*;

#[test]
fn key_hints_align_rows_and_keep_their_margins_when_unicode_is_clipped() {
    let mut registry = commands();
    registry.register_action(
        "custom.open",
        "打开文件",
        CommandImplementation::External(()),
    );
    registry.keymap.map(
        "text",
        "normal",
        vec!["space".into(), "z".into()],
        Some("custom.open".into()),
    );
    registry.keymap.map(
        "text",
        "normal",
        vec!["space".into(), "C-x".into()],
        Some("custom.align".into()),
    );
    let mut editor = ClientEditor::default();
    editor.set_catalog(registry.binding_catalog());
    assert!(matches!(send(&mut editor, " "), ClientInput::Consumed));

    let wide = frame(&editor);
    let hints = wide
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .filter(|span| matches!(span.style.as_str(), "status" | "popup"))
        .collect::<Vec<_>>();
    assert_eq!(hints[0].text.trim_end(), " space");
    assert!(
        hints
            .iter()
            .any(|span| span.text.starts_with(" z    打开文件"))
    );
    assert!(
        hints
            .iter()
            .any(|span| span.text.starts_with(" C-x  Align selected lines"))
    );
    let title_width = hints[0].text.width();
    assert!(hints.iter().all(|span| {
        span.text.starts_with(' ') && span.text.ends_with(' ') && span.text.width() == title_width
    }));

    let narrow = frame_at_width(&editor, 12);
    let label = narrow
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| span.style == "popup" && span.text.contains('打'))
        .unwrap();
    assert_eq!(label.text, " z    打开  ");
    assert_eq!(label.text.width(), 12);
}

#[test]
fn an_alias_shows_command_documentation_with_and_without_arguments() {
    let mut registry = commands();
    registry.register_command_alias("排列", "arrange-lines");
    let mut editor = ClientEditor::default();
    editor.set_catalog(registry.binding_catalog());
    assert!(matches!(send(&mut editor, ":"), ClientInput::Consumed));
    assert!(editor.paste("排"));

    let before_arguments = frame(&editor);
    let spans = &before_arguments.rows.last().unwrap().spans;
    assert!(
        spans
            .iter()
            .any(|span| span.style == "selection" && span.text == "arrange-lines")
    );
    assert!(spans.iter().all(|span| !span.style.ends_with(".match")));

    editor.reset_input();
    assert!(matches!(send(&mut editor, ":"), ClientInput::Consumed));
    assert!(editor.paste("排列 row"));
    let with_arguments = frame(&editor);
    let documentation = |frame: &Frame| {
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|span| matches!(span.style.as_str(), "popup" | "popup.secondary"))
            .filter(|span| !span.text.trim().is_empty())
            .map(|span| (span.text.trim_end().to_owned(), span.style.clone()))
            .collect::<Vec<_>>()
    };
    let expected = [
        ("Align selected lines".to_owned(), "popup".to_owned()),
        ("Aliases: al, 排列".to_owned(), "popup.secondary".to_owned()),
    ];
    assert_eq!(documentation(&before_arguments), expected);
    assert_eq!(documentation(&with_arguments), expected);
    assert!(
        with_arguments
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .all(|span| !span.style.starts_with("selection"))
    );
}

#[test]
fn a_fuzzy_candidate_keeps_its_match_highlight_after_completion() {
    let mut editor = ClientEditor::default();
    editor.set_catalog(commands().binding_catalog());
    assert!(matches!(send(&mut editor, ":"), ClientInput::Consumed));
    assert!(editor.paste("rrng-lns"));

    for expected_prompt in [":rrng-lns", ":arrange-lines"] {
        let frame = frame(&editor);
        assert_eq!(frame.prompt, expected_prompt);
        let spans = &frame.rows.last().unwrap().spans;
        let matched = spans
            .iter()
            .filter(|span| span.style == "selection.match")
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(matched, "rrng-lns");
        assert!(
            spans
                .iter()
                .any(|span| span.style == "selection" && !span.text.is_empty())
        );
        assert!(
            spans
                .iter()
                .filter(|span| span.style.starts_with("selection"))
                .all(|span| !span.text.contains(' '))
        );
        assert!(matches!(send(&mut editor, "Tab"), ClientInput::Consumed));
    }

    let ClientInput::Command { name, .. } = send(&mut editor, "Enter") else {
        panic!("no command");
    };
    assert_eq!(name, "custom.align");
}
