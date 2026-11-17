use super::input::Input;
use super::*;

#[test]
fn line_insertion_preserves_crlf_when_saved() {
    for (commands, expected) in [
        (
            &["text.insert", "text.insert-newline"][..],
            "\r\nfirst\r\nsecond\r\n",
        ),
        (&["text.open-above"][..], "\r\nfirst\r\nsecond\r\n"),
        (&["text.open-below"][..], "first\r\n\r\nsecond\r\n"),
    ] {
        let fixture = Fixture::new();
        let path = fixture.write("source.txt", "first\r\nsecond\r\n");
        let mut session = fixture.session(&[path.clone()]);
        for command in commands {
            session.execute_command(command, Value::Null).unwrap();
        }
        session.execute_command("text.escape", Value::Null).unwrap();
        session.command("w").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), expected, "{commands:?}");
    }
}

#[test]
fn a_mode_change_does_not_consume_the_first_inserted_character_as_an_old_prefix() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.input("g");
    session.set_mode(session.focus, "insert").unwrap();
    assert!(session.key_sequence.keys().is_empty());
    session.input("x");
    assert_eq!(Input::new(&mut session).text_state().source, "x");
}

#[test]
fn a_count_in_another_mode_cannot_restore_a_previous_modes_prefix() {
    let fixture = Fixture::new();
    let path = fixture.0.join("source.txt");
    fs::write(&path, "first\nsecond\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.input("g");
    session.set_mode(session.focus, "select").unwrap();
    session.input("2");
    assert!(session.key_sequence.keys().is_empty());
    session.set_mode(session.focus, "normal").unwrap();
    session.input("e");
    assert_eq!(
        Input::new(&mut session).text_state().cursors,
        [(0, 0, 0, 4)]
    );
}

#[test]
fn named_registers_can_be_yanked_and_pasted_between_buffers_using_default_keys() {
    let fixture = Fixture::new();
    let path = fixture.0.join("source.txt");
    let content = "first line\nsecond line\n";
    fs::write(&path, content).unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["%", "\"", "a", "y"]);
    input.command("new");
    input.keys(&["\"", "a", "p"]);
    assert_eq!(input.text_state().source, content, "{}", input.trace());
}

#[test]
fn unfinished_counts_are_cleared_by_unbound_keys_and_context_changes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("source.txt");
    fs::write(&path, "abcdef\n").unwrap();
    for interruption in [&["z"][..], &[":", "Esc"], &["C-w", "v"]] {
        let mut session = fixture.session(std::slice::from_ref(&path));
        let mut input = Input::new(&mut session);
        input.keys(&["2"]);
        input.keys(interruption);
        input.keys(&["l"]);
        assert_eq!(
            input.text_state().cursors,
            [(0, 1, 0, 1)],
            "{}",
            input.trace()
        );
    }
}

#[test]
fn choosing_a_register_does_not_leak_across_the_command_prompt() {
    let fixture = Fixture::new();
    let path = fixture.0.join("source.txt");
    fs::write(&path, "abc\n").unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["\"", "a", "y", "l", "\"", "a", ":", "Esc", "y"]);
    input.command("new");
    input.keys(&["\"", "a", "p"]);
    assert_eq!(input.text_state().source, "a", "{}", input.trace());
}

#[test]
fn repeat_insert_uses_the_last_completed_insertion_across_buffers() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let mut input = Input::new(&mut session);
    input.keys(&["i", "h", "e", "l", "l", "o", "Esc"]);
    input.command("new");
    input.keys(&["."]);
    assert_eq!(input.text_state().source, "hello", "{}", input.trace());
    input.keys(&["u"]);
    assert_eq!(input.text_state().source, "", "{}", input.trace());
}

#[test]
fn script_commands_can_forward_counted_text_actions() {
    let fixture = Fixture::new();
    let path = fixture.0.join("source.txt");
    fs::write(&path, "abcdef\n").unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "register-command \"custom-right\" \"Move right\"\n  lambda : argument\n    obj 'type \"action\" 'name \"text.move-right\"\nmap-modal \"text\" \"z\" \"custom-right\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[path], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    let mut input = Input::new(&mut session);
    input.keys(&["2", "z"]);
    assert_eq!(
        input.text_state().cursors,
        [(0, 2, 0, 2)],
        "{}",
        input.trace()
    );
}

#[test]
fn cancelling_a_clipboard_read_rejects_its_later_reply() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    for key in ["\"", "+", "p"] {
        session.input(key);
    }
    let (request, _) = session.clipboard_request().unwrap();
    session.input("Esc");
    assert!(session.clipboard_request().is_none());
    session.complete_clipboard_read(request, Some("stale".into()), None);
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("clipboard target is no longer a text buffer");
    };
    assert_eq!(text.text(), "");
}

#[test]
fn item_jump_waits_for_both_letters_and_can_be_cancelled() {
    let fixture = Fixture::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(fixture.0.join(name), "").unwrap();
    }
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.snapshot();
    for key in ["g", "w"] {
        session.input(key);
    }
    assert_eq!(
        session.views[&session.focus]
            .jump
            .as_ref()
            .unwrap()
            .prompt(),
        "Goto item: type a highlighted label · Esc to cancel"
    );
    session.input("a");
    assert_eq!(
        session.views[&session.focus]
            .jump
            .as_ref()
            .unwrap()
            .prompt(),
        "Goto item: a_ · type the second letter · Esc to cancel"
    );
    session.input("Esc");
    assert!(session.views[&session.focus].jump.is_none());
    assert_eq!(selected_row(&session, session.focus).id, "a.txt");
    for key in ["g", "w", "a", "b"] {
        session.input(key);
    }
    assert!(session.views[&session.focus].jump.is_none());
    assert_eq!(selected_row(&session, session.focus).id, "b.txt");
}

#[test]
fn counts_and_key_prefixes_remain_separate_until_cancelled() {
    let fixture = Fixture::new();
    for files in [false, true] {
        let mut session = fixture.session(&[]);
        if files {
            session.command("files").unwrap();
            settle(&mut session);
        }
        for key in ["2", "C-w"] {
            session.input(key);
        }
        assert_eq!(session.command_count().value(), Some(2));
        assert_eq!(session.key_sequence.keys(), ["C-w"]);
        session.input("Esc");
        assert!(session.key_sequence.keys().is_empty());
        assert!(session.command_count().is_empty());
    }
}

#[test]
fn publishing_updated_bindings_clears_the_previous_prefix() {
    let scripts = Fixture::new();
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.input("g");
    assert_eq!(session.key_sequence.keys(), ["g"]);
    session
        .scripts
        .as_mut()
        .unwrap()
        .load(&scripts.write(
            "updated-keys.w",
            "map-modal \"text\" \"z\" \"text.move-right\"\n",
        ))
        .unwrap();
    session.snapshot();
    assert!(session.key_sequence.keys().is_empty());
}
