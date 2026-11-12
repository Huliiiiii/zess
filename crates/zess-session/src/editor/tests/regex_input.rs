use super::input::{Input, TextState};
use super::*;

#[test]
fn selection_patterns_preview_edits_and_cancel_back_to_the_original_selection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "ab ax ab\n").unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["%", "v", "s"]);
    assert_eq!(input.session.frame().prompt, "select: ");
    input.keys(&["a"]);
    input.assert_state(TextState {
        source: "ab ax ab\n".into(),
        mode: TextMode::Select,
        cursors: vec![(0, 0, 0, 0), (0, 3, 0, 3), (0, 6, 0, 6)],
    });
    input.keys(&["b"]);
    input.assert_state(TextState {
        source: "ab ax ab\n".into(),
        mode: TextMode::Select,
        cursors: vec![(0, 0, 0, 1), (0, 6, 0, 7)],
    });
    input.keys(&["Backspace"]);
    input.assert_state(TextState {
        source: "ab ax ab\n".into(),
        mode: TextMode::Select,
        cursors: vec![(0, 0, 0, 0), (0, 3, 0, 3), (0, 6, 0, 6)],
    });
    input.keys(&["Esc"]);
    input.assert_state(TextState {
        source: "ab ax ab\n".into(),
        mode: TextMode::Select,
        cursors: vec![(0, 0, 0, 8)],
    });
    assert!(input.session.active_prompt().is_none());
}

#[test]
fn invalid_selection_patterns_can_be_corrected_before_submission() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "ab ax ab\n").unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["%", "s", "a", "[", "Enter"]);
    assert!(input.session.active_prompt().unwrap().error.is_some());
    input.keys(&["Backspace", "Enter"]);
    assert!(input.session.active_prompt().is_none());
    input.keys(&["d"]);
    assert_eq!(input.text_state().source, "b x b\n", "{}", input.trace());
}

#[test]
fn an_unmatched_selection_pattern_can_be_cancelled_without_losing_its_original_range() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "ab ax ab\n").unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["%", "s", "z", "Enter"]);
    assert_eq!(
        input.session.active_prompt().unwrap().error.as_deref(),
        Some("nothing selected")
    );
    input.assert_state(TextState {
        source: "ab ax ab\n".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 0, 0, 8)],
    });
    input.keys(&["Esc"]);
    input.assert_state(TextState {
        source: "ab ax ab\n".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 0, 0, 8)],
    });
    assert!(input.session.active_prompt().is_none());
}

#[test]
fn script_commands_that_return_count_actions_preserve_the_count_for_the_next_motion() {
    let fixture = Fixture::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(fixture.0.join(name), "").unwrap();
    }
    let config = fixture.0.join("init.w");
    fs::write(&config, "register-command \"two\" \"Count two\"\n  lambda : argument\n    obj 'type \"action\" 'name \"data.count-2\"\nmap-modal \"files\" \"z\" \"two\"\n").unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    let mut input = Input::new(&mut session);
    input.keys(&["z", "j"]);
    assert_eq!(
        input.session.views[&input.session.focus]
            .location
            .selection
            .primary
            .as_deref(),
        Some("c.txt")
    );
}

#[test]
fn search_previews_the_match_and_enter_keeps_that_match() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "red blue red blue\n").unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["/", "b", "l", "u", "e"]);
    input.assert_state(TextState {
        source: "red blue red blue\n".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 4, 0, 7)],
    });
    input.keys(&["Enter"]);
    input.assert_state(TextState {
        source: "red blue red blue\n".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 4, 0, 7)],
    });
    assert!(input.session.active_prompt().is_none());
    input.keys(&["n"]);
    input.assert_state(TextState {
        source: "red blue red blue\n".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 13, 0, 16)],
    });
    input.keys(&["N"]);
    input.assert_state(TextState {
        source: "red blue red blue\n".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 4, 0, 7)],
    });
}

#[test]
fn cancelling_search_keeps_select_mode_and_the_original_selection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "red blue red blue\n").unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["v", "/", "b", "Esc"]);
    input.assert_state(TextState {
        source: "red blue red blue\n".into(),
        mode: TextMode::Select,
        cursors: vec![(0, 0, 0, 0)],
    });
}

#[test]
fn leaving_a_search_restores_the_original_selection_before_focusing_another_view() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "red blue red blue\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.command("vsplit").unwrap();
    let other = session.focus;
    session.focus_next(1).unwrap();
    let source = session.focus;
    let mut input = Input::new(&mut session);
    input.keys(&["v", "/", "b"]);
    assert_eq!(input.text_state().cursors, [(0, 0, 0, 0), (0, 4, 0, 4)]);
    input.session.focus_view(other).unwrap();
    assert!(input.session.active_prompt().is_none());
    input.session.focus_view(source).unwrap();
    input.assert_state(TextState {
        source: "red blue red blue\n".into(),
        mode: TextMode::Select,
        cursors: vec![(0, 0, 0, 0)],
    });
    input.keys(&["l"]);
    assert_eq!(input.text_state().cursors, [(0, 0, 0, 1)]);
}

#[test]
fn replacing_a_prompt_target_releases_its_old_text_view() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "red blue red blue\n").unwrap();
    let mut session = fixture.session(&[path]);
    let source = session.focus;
    let buffer = session.views[&source].location.buffer;
    for key in ["/", "b"] {
        session.input(key);
    }
    assert!(session.active_prompt().is_some());
    session.command("new").unwrap();
    assert!(session.active_prompt().is_none());
    let text = session.buffers.get_mut(&buffer).unwrap().text().unwrap();
    assert_eq!(text.pending_prompt(source), None);
    assert!(text.selections(source).is_empty());
}

#[test]
fn closing_help_restores_search_input_and_preserves_its_original_selection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "red blue red blue\n").unwrap();
    let mut session = fixture.session(&[path]);
    let mut input = Input::new(&mut session);
    input.keys(&["/", "b"]);
    input.session.command("help").unwrap();
    input
        .session
        .execute_command("help.close", json!({}))
        .unwrap();
    assert_eq!(input.session.active_prompt().unwrap().text, "b");
    assert_eq!(input.text_state().cursors, [(0, 4, 0, 4)]);
    input.keys(&["l", "u", "e"]);
    assert_eq!(input.text_state().cursors, [(0, 4, 0, 7)]);
    input.keys(&["Esc"]);
    assert_eq!(input.text_state().cursors, [(0, 0, 0, 0)]);
    assert!(input.session.prompt.is_none());
}
