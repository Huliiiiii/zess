use super::*;

#[test]
fn language_command_shows_and_changes_the_current_language() {
    let fixture = Fixture::new();
    let path = fixture.0.join("source.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    let mut session = fixture.session(&[path]);

    session.command("set-language").unwrap();
    assert_eq!(session.message, "rust");
    session.command("lang wisp").unwrap();
    session.command("set-language").unwrap();
    assert_eq!(session.message, "wisp");
    let error = session.command("lang unknown-language").unwrap_err();
    assert_eq!(error.to_string(), "unknown language \"unknown-language\"");
    session.command("lang").unwrap();
    assert_eq!(session.message, "wisp");
    session.command("set-language text").unwrap();
    session.command("lang").unwrap();
    assert_eq!(session.message, "text");
}

#[test]
fn language_alias_has_one_completion_entry() {
    let fixture = Fixture::new();
    let session = fixture.session(&[]);
    let candidates = session.command_candidates("lang");
    assert_eq!(candidates[0].name, "set-language");
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.name == "set-language")
            .count(),
        1
    );
    assert!(
        !session
            .command_candidates("")
            .iter()
            .any(|candidate| candidate.name == "lang")
    );
}

#[test]
fn script_actions_use_the_language_command() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "require \"zess/ui\"\nregister-command \"choose-language\" \"Choose language\"\n  lambda : argument\n    request-action \"set-language\" argument\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("choose-language rust").unwrap();
    session.command("lang").unwrap();
    assert_eq!(session.message, "rust");
}

#[test]
fn language_command_supports_read_only_text_and_rejects_non_text_buffers() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let buffer = session.views[&session.focus].location.buffer;
    session.buffers.insert(
        buffer,
        Buffer::PreviewText {
            path: "preview".into(),
            snapshot: None,
            text: TextBuffer::new("preview"),
        },
    );
    session.command("lang rust").unwrap();
    session.command("lang").unwrap();
    assert_eq!(session.message, "rust");

    session.buffers.insert(
        buffer,
        Buffer::Data {
            kind: "test".into(),
            modified: false,
            text_source: None,
        },
    );
    let error = session.command("lang wisp").unwrap_err();
    assert_eq!(error.to_string(), "command requires a text buffer");
    assert!(
        matches!(&session.buffers[&buffer], Buffer::Data { kind, modified: false, .. } if kind == "test")
    );
    session.buffers.insert(
        buffer,
        Buffer::Items {
            kind: "test-items".into(),
            path: "items".into(),
            display: Display::default(),
            help_context: None,
        },
    );
    let error = session.command("lang wisp").unwrap_err();
    assert_eq!(error.to_string(), "command requires a text buffer");
    assert!(
        matches!(&session.buffers[&buffer], Buffer::Items { kind, path, .. } if kind == "test-items" && path == "items")
    );
}
