use super::*;
use zess_plugin::{CommandImplementation, Registry};

fn registry() -> Registry<()> {
    let mut registry = Registry::default();
    registry.keymap.register_scope("text", "Document");
    registry.keymap.register_scope("global", "Everywhere");
    registry.register_action("save", "Save revision", CommandImplementation::Native);
    registry.register_action("exit", "Exit editor", CommandImplementation::Native);
    registry
        .keymap
        .map("global", "normal", vec!["q".into()], Some("exit".into()));
    for key in ["s", "w"] {
        registry
            .keymap
            .map("text", "normal", vec![key.into()], Some("save".into()));
    }
    registry
}

fn source() -> Frame {
    Frame {
        width: 54,
        height: 11,
        cursor: Some((1, 1)),
        rows: vec![
            FrameRow {
                spans: vec![Span {
                    text: "source document".into(),
                    style: String::new(),
                    base_style: None,
                }],
            };
            10
        ],
        ..Frame::default()
    }
}

fn lines(frame: &Frame) -> Vec<&str> {
    frame
        .rows
        .iter()
        .map(|row| row.spans[0].text.trim_end())
        .collect()
}

#[test]
fn pinned_help_keeps_source_content_visible_when_focus_moves() {
    let registry = registry();
    let catalog = registry.binding_catalog();
    let mut help = Help::new(
        &catalog,
        &registry.keymap,
        &["global", "text"],
        "normal",
        "Text",
    );
    let mut frame = source();
    help.render(&mut frame);
    let title = &frame.rows[0].spans[0];
    assert_eq!(title.text.trim_end(), "Help: Text");
    assert_eq!(title.style, "ui.heading");
    assert!(frame.cursor.is_none());

    help.command("help.pin", 11);
    help.render(&mut frame);
    assert_eq!(frame.rows[5].spans[0].style, "ui.heading");

    help.command("focus-up", 11);
    let mut frame = source();
    help.render(&mut frame);
    assert_eq!(help.source_height(11), 6);
    assert_eq!(&lines(&frame)[..5], &["source document"; 5]);
    assert_eq!(
        &lines(&frame)[5..8],
        &[
            "Help: Text",
            "Document",
            "s, w                 Save revision"
        ]
    );
    assert_eq!(frame.rows[5].spans[0].style, "ui.heading");
    assert_eq!(frame.cursor, Some((1, 1)));
    assert!(matches!(help.command("quit", 11), HelpAction::Unhandled));

    assert!(help.mouse(
        &MouseEvent {
            kind: MouseKind::Press,
            column: 5,
            row: 7
        },
        11
    ));
    assert!(matches!(help.command("help.close", 11), HelpAction::Close));
}

#[test]
fn help_refreshes_groups_and_bindings_for_its_original_context() {
    let mut registry = registry();
    let mut help = Help::new(
        &registry.binding_catalog(),
        &registry.keymap,
        &["global", "text"],
        "normal",
        "Text",
    );
    registry.keymap.register_scope("text", "Local text");
    registry
        .keymap
        .map("text", "normal", vec!["s".into()], None);
    registry
        .keymap
        .map("text", "normal", vec!["z".into()], Some("save".into()));
    help.refresh(&registry.binding_catalog(), &registry.keymap);
    let mut frame = source();
    help.render(&mut frame);
    assert_eq!(
        &lines(&frame)[..5],
        &[
            "Help: Text",
            "Local text",
            "w, z                 Save revision",
            "Everywhere",
            "q                    Exit editor"
        ]
    );
}

#[test]
fn scrolling_past_a_group_heading_keeps_its_label_above_the_bindings() {
    let mut registry = Registry::<()>::default();
    registry.keymap.register_scope("global", "Global keys");
    for name in 'a'..='z' {
        let name = name.to_string();
        registry.register_action(&name, &name, CommandImplementation::Native);
        registry
            .keymap
            .map("global", "normal", vec![name.clone()], Some(name));
    }

    let mut help = Help::new(
        &registry.binding_catalog(),
        &registry.keymap,
        &["global"],
        "normal",
        "Text",
    );
    help.command("data.goto-last", 11);
    let mut frame = source();
    help.render(&mut frame);
    let rows = lines(&frame);
    assert_eq!(rows[1], "Global keys");
    assert!(rows[2].starts_with('v'));
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.style == "selection" && span.text.trim_end().ends_with('z'))
    );
}

#[test]
fn an_invalid_filter_keeps_the_last_matching_help_rows() {
    use zess_filter::QueryKind;
    let registry = registry();
    let mut help = Help::new(
        &registry.binding_catalog(),
        &registry.keymap,
        &["global", "text"],
        "normal",
        "Text",
    );
    help.filter(Query {
        kind: QueryKind::Plain,
        text: "REVISION".into(),
    })
    .unwrap();
    assert!(
        help.filter(Query {
            kind: QueryKind::Regex,
            text: "[".into()
        })
        .is_err()
    );
    let mut frame = source();
    help.render(&mut frame);
    assert_eq!(help.query().text, "REVISION");
    assert_eq!(
        &lines(&frame)[..3],
        &["Help: Text", "s, w                 Save revision", ""]
    );
}

#[test]
fn an_item_jump_selects_the_help_binding_with_the_typed_label() {
    let registry = registry();
    let mut help = Help::new(
        &registry.binding_catalog(),
        &registry.keymap,
        &["global", "text"],
        "normal",
        "Text",
    );
    help.command("data.goto-word", 11);
    let mut frame = source();
    help.render(&mut frame);
    assert!(
        frame.rows[4]
            .spans
            .iter()
            .any(|span| span.text == "ab" && span.style == "pending")
    );

    assert!(help.pending_key("a", 11));
    help.render(&mut frame);
    assert!(frame.prompt.contains("a_"));
    assert!(help.pending_key("b", 11));
    help.render(&mut frame);
    assert!(frame.prompt.is_empty());
    assert!(
        frame.rows[4]
            .spans
            .iter()
            .any(|span| span.text.contains("Exit editor") && span.style == "selection")
    );
}

#[test]
fn selecting_all_help_bindings_excludes_group_headings() {
    let mut registry = registry();
    registry.keymap.map(
        "project/custom",
        "normal",
        vec!["x".into()],
        Some("save".into()),
    );
    let mut help = Help::new(
        &registry.binding_catalog(),
        &registry.keymap,
        &["global", "text", "project/custom"],
        "normal",
        "Text",
    );
    help.command("data.select-all", 11);
    let mut frame = source();
    help.render(&mut frame);
    let rows = frame.rows[1..7]
        .iter()
        .map(|row| (row.spans[0].text.trim_end(), row.spans[0].style.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("project/custom", "status"),
            ("x                    Save revision", "selection"),
            ("Document", "status"),
            ("s, w                 Save revision", "selection"),
            ("Everywhere", "status"),
            ("q                    Exit editor", "selection")
        ]
    );

    help.command("data.escape", 11);
    help.command("data.goto-first", 11);
    help.render(&mut frame);
    assert_eq!(frame.rows[1].spans[0].style, "status");
    assert_eq!(frame.rows[2].spans[0].style, "selection");
}
