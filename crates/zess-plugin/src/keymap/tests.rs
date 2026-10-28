use super::*;

#[test]
fn successive_keys_follow_the_prefix_and_restart_after_a_command_or_miss() {
    let mut keymap = Keymap::default();
    keymap.map(
        "global",
        "normal",
        vec!["g".into(), "w".into()],
        Some("jump".into()),
    );
    keymap.map("global", "normal", vec!["j".into()], Some("down".into()));
    let mut state = KeyState::default();
    for (key, expected) in [
        ("g", KeyResolution::Prefix),
        ("w", KeyResolution::Command("jump")),
        ("j", KeyResolution::Command("down")),
        ("g", KeyResolution::Prefix),
        ("x", KeyResolution::Unbound),
        ("j", KeyResolution::Command("down")),
    ] {
        let actual = keymap.advance(&mut state, &["global"], "normal", key);
        assert!(match (actual, expected) {
            (KeyResolution::Command(actual), KeyResolution::Command(expected)) =>
                actual == expected,
            (KeyResolution::Prefix, KeyResolution::Prefix)
            | (KeyResolution::Unbound, KeyResolution::Unbound) => true,
            _ => false,
        });
    }
}

#[test]
fn changing_scopes_or_mode_discards_the_previous_prefix() {
    let mut keymap = Keymap::default();
    keymap.map(
        "global",
        "normal",
        vec!["g".into(), "x".into()],
        Some("old-prefix".into()),
    );
    keymap.map("local", "normal", vec!["x".into()], Some("local-x".into()));
    keymap.map(
        "global",
        "insert",
        vec!["x".into()],
        Some("insert-x".into()),
    );
    let mut state = KeyState::default();
    for (scopes, mode, expected) in [
        (&["global", "local"][..], "normal", "local-x"),
        (&["global"][..], "insert", "insert-x"),
    ] {
        assert!(matches!(
            keymap.advance(&mut state, &["global"], "normal", "g"),
            KeyResolution::Prefix
        ));
        assert!(
            matches!(keymap.advance(&mut state, scopes, mode, "x"), KeyResolution::Command(command) if command == expected)
        );
    }
}

#[test]
fn editing_bindings_cannot_resume_an_obsolete_prefix() {
    let mut keymap = Keymap::default();
    keymap.map(
        "global",
        "normal",
        vec!["g".into(), "x".into()],
        Some("old-prefix".into()),
    );
    let mut state = KeyState::default();
    assert!(matches!(
        keymap.advance(&mut state, &["global"], "normal", "g"),
        KeyResolution::Prefix
    ));
    keymap.map("global", "normal", vec!["x".into()], Some("new-x".into()));
    assert!(matches!(
        keymap.advance(&mut state, &["global"], "normal", "x"),
        KeyResolution::Command("new-x")
    ));
}

#[test]
fn a_new_keymap_cannot_resume_another_keymaps_prefix() {
    let identities = IdentityRegistry::default();
    let mut first = Keymap::with_identities(identities.clone());
    first.map(
        "global",
        "normal",
        vec!["g".into(), "x".into()],
        Some("old-prefix".into()),
    );
    let mut second = Keymap::with_identities(identities);
    second.map("global", "normal", vec!["x".into()], Some("new-x".into()));
    let mut state = KeyState::default();
    assert!(matches!(
        first.advance(&mut state, &["global"], "normal", "g"),
        KeyResolution::Prefix
    ));
    assert!(matches!(
        second.advance(&mut state, &["global"], "normal", "x"),
        KeyResolution::Command("new-x")
    ));
}

#[test]
fn scoped_mappings_replace_conflicting_sequences_and_preserve_the_winning_scope() {
    let mut keymap = Keymap::default();
    keymap.register_scope("browser", "Files");
    keymap.map(
        "global",
        "normal",
        vec!["g".into()],
        Some("global-g".into()),
    );
    keymap.map(
        "global",
        "normal",
        vec!["x".into(), "x".into()],
        Some("remove".into()),
    );
    keymap.map(
        "global",
        "normal",
        vec!["z".into(), "a".into()],
        Some("fold".into()),
    );
    keymap.map("global", "normal", vec!["k".into()], Some("up".into()));
    keymap.map(
        "browser",
        "normal",
        vec!["g".into(), "g".into()],
        Some("first".into()),
    );
    keymap.map("browser", "normal", vec!["x".into()], None);
    keymap.map("browser", "normal", vec!["z".into()], Some("toggle".into()));
    keymap.map(
        "browser",
        "select",
        vec!["k".into()],
        Some("select-up".into()),
    );

    let bindings = keymap.effective_bindings(&["global", "browser"], "normal");
    assert_eq!(
        bindings
            .iter()
            .map(|binding| (
                binding.keys.join(" "),
                binding.command,
                keymap.identities().scope_name(binding.scope).to_string()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("g g".into(), "first", "browser".into()),
            ("k".into(), "up", "global".into()),
            ("z".into(), "toggle", "browser".into()),
        ]
    );
    assert_eq!(
        keymap.scope_label(&keymap.identities().scope_name(bindings[0].scope)),
        "Files"
    );
    assert!(matches!(
        keymap.resolve(&["global", "browser"], "normal", &["g".into()]),
        KeyResolution::Prefix
    ));
    assert!(matches!(
        keymap.resolve(&["global", "browser"], "normal", &["g".into(), "g".into()]),
        KeyResolution::Command("first")
    ));
    assert!(matches!(
        keymap.resolve(&["global", "browser"], "normal", &["x".into(), "x".into()]),
        KeyResolution::Unbound
    ));
}

#[test]
fn new_mappings_replace_results_in_every_previously_used_context() {
    let mut keymap = Keymap::default();
    keymap.map("global", "normal", vec!["a".into()], Some("old".into()));
    keymap.map("global", "select", vec!["a".into()], Some("select".into()));
    assert!(matches!(
        keymap.resolve(&["global"], "normal", &["a".into()]),
        KeyResolution::Command("old")
    ));
    assert!(matches!(
        keymap.resolve(&["global", "browser"], "normal", &["a".into()]),
        KeyResolution::Command("old")
    ));
    assert!(matches!(
        keymap.resolve(&["global"], "select", &["a".into()]),
        KeyResolution::Command("select")
    ));

    keymap.map("global", "normal", vec!["a".into()], Some("new".into()));
    for scopes in [&["global"][..], &["global", "browser"][..]] {
        assert!(matches!(
            keymap.resolve(scopes, "normal", &["a".into()]),
            KeyResolution::Command("new")
        ));
        let bindings = keymap.effective_bindings(scopes, "normal");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].command, "new");
    }
    assert!(matches!(
        keymap.resolve(&["global"], "select", &["a".into()]),
        KeyResolution::Command("select")
    ));
}

#[test]
fn scope_order_determines_which_command_wins() {
    let mut keymap = Keymap::default();
    keymap.map("first", "normal", vec!["a".into()], Some("first".into()));
    keymap.map("second", "normal", vec!["a".into()], Some("second".into()));
    for _ in 0..2 {
        assert!(matches!(
            keymap.resolve(&["first", "second"], "normal", &["a".into()]),
            KeyResolution::Command("second")
        ));
        assert!(matches!(
            keymap.resolve(&["second", "first"], "normal", &["a".into()]),
            KeyResolution::Command("first")
        ));
    }
}

#[test]
fn unmapping_a_longer_sequence_preserves_the_shorter_command() {
    let mut keymap = Keymap::default();
    keymap.map("global", "normal", vec!["g".into()], Some("goto".into()));
    keymap.map("browser", "normal", vec!["g".into(), "g".into()], None);
    assert!(matches!(
        keymap.resolve(&["global", "browser"], "normal", &["g".into()]),
        KeyResolution::Command("goto")
    ));
    assert!(matches!(
        keymap.resolve(&["global", "browser"], "normal", &["g".into(), "g".into()]),
        KeyResolution::Unbound
    ));
}

#[test]
fn unmapping_the_last_sequence_removes_its_prefixes() {
    let mut keymap = Keymap::default();
    keymap.map(
        "global",
        "normal",
        vec!["g".into(), "g".into()],
        Some("goto".into()),
    );
    assert!(matches!(
        keymap.resolve(&["global"], "normal", &["g".into()]),
        KeyResolution::Prefix
    ));
    keymap.map("global", "normal", vec!["g".into(), "g".into()], None);
    assert!(matches!(
        keymap.resolve(&["global"], "normal", &["g".into()]),
        KeyResolution::Unbound
    ));
    assert!(matches!(
        keymap.resolve(&["global"], "normal", &[]),
        KeyResolution::Unbound
    ));
    assert!(keymap.effective_bindings(&["global"], "normal").is_empty());
}

#[test]
fn unmapping_a_replacement_does_not_restore_the_displaced_command() {
    let mut keymap = Keymap::default();
    keymap.map(
        "global",
        "normal",
        vec!["g".into()],
        Some("original".into()),
    );
    keymap.map(
        "global",
        "normal",
        vec!["g".into(), "g".into()],
        Some("replacement".into()),
    );
    keymap.map("global", "normal", vec!["g".into(), "g".into()], None);
    assert!(matches!(
        keymap.resolve(&["global"], "normal", &["g".into()]),
        KeyResolution::Unbound
    ));
    assert!(keymap.effective_bindings(&["global"], "normal").is_empty());
}
