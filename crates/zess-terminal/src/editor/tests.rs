use super::*;
use zess_plugin::{CommandImplementation, Registry};
use zess_ui::{FrameRow, Span};

mod rendering;

fn commands() -> Registry<()> {
    let mut registry = Registry::default();
    for id in [
        "command-prompt",
        "prompt.submit",
        "prompt.complete-next",
        "prompt.cancel",
    ] {
        registry.register_action(id, id, CommandImplementation::Native);
    }
    registry.register_action(
        "custom.align",
        "Align selected lines",
        CommandImplementation::External(()),
    );
    registry.register_command_line("custom.align", "arrange-lines");
    registry.register_command_alias("al", "arrange-lines");
    for (mode, keys, command) in [
        ("normal", vec![":"], "command-prompt"),
        ("normal", vec!["space", "a"], "al"),
        ("prompt", vec!["Enter"], "prompt.submit"),
        ("prompt", vec!["Tab"], "prompt.complete-next"),
        ("prompt", vec!["Esc"], "prompt.cancel"),
    ] {
        registry.keymap.map(
            "global",
            mode,
            keys.into_iter().map(String::from).collect(),
            Some(command.into()),
        );
    }
    registry
}

fn send(editor: &mut ClientEditor, key: &str) -> ClientInput {
    editor
        .handle_key(key, &["global", "text"], "normal", 60, 10)
        .unwrap()
}

fn frame_at_width(editor: &ClientEditor, width: u16) -> Frame {
    let mut frame = Frame {
        width,
        height: 10,
        rows: vec![
            FrameRow {
                spans: vec![Span {
                    text: "document".into(),
                    style: String::new(),
                    base_style: None,
                }]
            };
            9
        ],
        ..Frame::default()
    };
    editor.render(&mut frame, &["global", "text"], "normal");
    frame
}

fn frame(editor: &ClientEditor) -> Frame {
    frame_at_width(editor, 60)
}

#[test]
fn key_hints_identify_the_script_that_receives_a_completed_chord() {
    let mut editor = ClientEditor::default();
    editor.set_catalog(commands().binding_catalog());
    assert!(matches!(send(&mut editor, " "), ClientInput::Consumed));
    let frame = frame(&editor);
    assert!(frame.prompt.contains("space"));
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.text.contains("Align selected lines"))
    );
    let ClientInput::Command { name, argument } = send(&mut editor, "a") else {
        panic!("no command")
    };
    assert_eq!(name, "custom.align");
    assert_eq!(argument, json!({}));
}

#[test]
fn command_completion_submits_the_registered_action_with_its_arguments() {
    let mut editor = ClientEditor::default();
    editor.set_catalog(commands().binding_catalog());
    for key in [":", "a", "l", "Tab"] {
        assert!(matches!(send(&mut editor, key), ClientInput::Consumed));
    }
    assert_eq!(frame(&editor).prompt, ":arrange-lines");
    assert!(editor.paste(" row\r\n三"));
    assert_eq!(frame(&editor).prompt, ":arrange-lines row 三");
    let ClientInput::Command { name, argument } = send(&mut editor, "Enter") else {
        panic!("no command")
    };
    assert_eq!(name, "custom.align");
    assert_eq!(argument, json!({ "argument": "row 三" }));
    assert!(!editor.prompt_active());
}

#[test]
fn replacing_the_native_prompt_command_runs_the_registered_script() {
    let mut registry = commands();
    registry.register_action(
        "command-prompt",
        "A custom prompt",
        CommandImplementation::External(()),
    );
    let mut editor = ClientEditor::default();
    editor.set_catalog(registry.binding_catalog());
    let ClientInput::Command { name, .. } = send(&mut editor, ":") else {
        panic!("no command")
    };
    assert_eq!(name, "command-prompt");
    assert!(!editor.prompt_active());
}

#[test]
fn a_minibuffer_uses_its_own_bindings_for_query_edits_and_submission() {
    let mut registry = commands();
    registry.register_action(
        "prompt.toggle-regex",
        "Toggle regex",
        CommandImplementation::Native,
    );
    registry.keymap.map(
        "find",
        "prompt",
        vec!["Tab".into()],
        Some("prompt.toggle-regex".into()),
    );
    let mut editor = ClientEditor::default();
    editor.set_catalog(registry.binding_catalog());
    editor.open_prompt(
        "find".into(),
        "中".into(),
        vec!["global".into(), "minibuffer".into(), "find".into()],
        Some(QueryKind::Plain),
    );
    assert!(matches!(
        send(&mut editor, "Tab"),
        ClientInput::Prompt(PromptEvent::Changed)
    ));
    assert!(matches!(editor.query_kind(), Some(QueryKind::Regex)));
    assert_eq!(frame(&editor).prompt, "find(re):中");
    assert!(matches!(
        send(&mut editor, "文"),
        ClientInput::Prompt(PromptEvent::Changed)
    ));
    assert!(matches!(
        send(&mut editor, "Enter"),
        ClientInput::Prompt(PromptEvent::Submit)
    ));
    assert_eq!(editor.prompt_text(), "中文");
    editor.set_prompt_error("try a different query".into());
    let frame = frame(&editor);
    assert!(
        frame
            .rows
            .last()
            .unwrap()
            .spans
            .iter()
            .any(|span| span.text.trim_end() == "try a different query")
    );
    assert!(matches!(
        send(&mut editor, "Esc"),
        ClientInput::Prompt(PromptEvent::Cancel)
    ));
}
