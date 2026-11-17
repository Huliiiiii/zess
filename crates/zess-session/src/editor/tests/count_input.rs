use super::input::{Input, TextState};
use super::*;

#[test]
fn text_counts_accept_helixs_limit() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let mut input = Input::new(&mut session);
    input.keys(&["1", "0", "0", "0", "0", "0", "0", "0", "0"]);
    assert_eq!(
        input.session.command_count().value(),
        Some(100_000_000),
        "{}",
        input.trace()
    );
    input.keys(&["1"]);
    assert_eq!(
        input.session.command_count().value(),
        Some(100_000_000),
        "{}",
        input.trace()
    );
    input.keys(&["Esc"]);
    assert!(
        input.session.command_count().is_empty(),
        "{}",
        input.trace()
    );
    input.keys(&["0"]);
    assert!(
        input.session.command_count().is_empty(),
        "{}",
        input.trace()
    );
}

#[test]
fn a_digit_binding_starts_a_command_unless_a_count_is_in_progress() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(&config, "map-modal \"text\" \"2\" \"text.goto-line-end\"\n").unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    let mut input = Input::new(&mut session);
    input.keys(&["i", "a", "b", "c", "Esc", "g", "h", "2"]);
    assert!(
        input.session.command_count().is_empty(),
        "{}",
        input.trace()
    );
    input.assert_state(TextState {
        source: "abc".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 2, 0, 2)],
    });
    input.keys(&["1", "2"]);
    assert_eq!(
        input.session.command_count().value(),
        Some(12),
        "{}",
        input.trace()
    );
}

#[test]
fn item_counts_use_helixs_numeric_limit() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    let mut input = Input::new(&mut session);
    input.keys(&["1", "0", "0", "0", "0", "0", "0", "0", "0"]);
    assert_eq!(
        input.session.command_count().value(),
        Some(100_000_000),
        "{}",
        input.trace()
    );
    input.keys(&["j"]);
    assert!(
        input.session.command_count().is_empty(),
        "{}",
        input.trace()
    );
}

#[test]
fn a_digit_in_a_pending_key_sequence_remains_a_binding() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "map-modal \"text\" (list \"space\" \"2\") \"text.goto-line-end\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    let mut input = Input::new(&mut session);
    input.keys(&["i", "a", "b", "c", "Esc", "g", "h", "space", "2"]);
    assert!(
        input.session.command_count().is_empty(),
        "{}",
        input.trace()
    );
    assert!(
        input.session.key_sequence.keys().is_empty(),
        "{}",
        input.trace()
    );
    input.assert_state(TextState {
        source: "abc".into(),
        mode: TextMode::Normal,
        cursors: vec![(0, 2, 0, 2)],
    });
}
