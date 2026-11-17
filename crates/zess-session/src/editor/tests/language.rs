use super::*;

#[test]
fn plain_text_sessions_remain_editable_when_the_syntax_runtime_is_invalid() {
    if std::env::var_os("ZESS_TEST_PLAIN_TEXT_RUNTIME").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "editor::tests::language::plain_text_sessions_remain_editable_when_the_syntax_runtime_is_invalid",
            ])
            .env("ZESS_TEST_PLAIN_TEXT_RUNTIME", "1")
            .env("ZESS_RUNTIME", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let fixture = Fixture::new();
    let path = fixture.write("notes.txt", "plain text");
    let mut session = fixture.session(std::slice::from_ref(&path));
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    session.command("w").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "!plain text");
    let state = session.snapshot();
    let focused = state
        .views
        .iter()
        .find(|view| Some(view.view_id) == state.focused_view)
        .unwrap();
    let zess_protocol::ViewContent::Text { lines } = &focused.content else {
        panic!("expected a text view");
    };
    assert_eq!(lines[0].text, "!plain text");
    session.command("lang rust").unwrap();
    session.snapshot();
    let buffer = session.views[&session.focus].location.buffer;
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.document(buffer).unwrap().syntax_error().is_none() {
        assert!(Instant::now() < deadline);
        session.tick();
        std::thread::yield_now();
    }
    let text = session.document(buffer).unwrap();
    assert_eq!(
        text.syntax_error(),
        Some("ZESS_RUNTIME must name a directory")
    );
    assert_eq!(text.text(), "!plain text");
}

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
fn a_script_command_uses_its_target_after_focus_moves() {
    use zess_protocol::{CommandAction, CommandOutcome, CommandTarget};

    let fixture = Fixture::new();
    let config = fixture.write(
        "config.w",
        r#"require "zess/ui"
register-command "choose-language" "Choose language"
  lambda : argument
    request-action "set-language" argument
register-command-alias "choose" "choose-language"
"#,
    );
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    let view_id = session.focus;
    let buffer_id = session.views[&view_id].location.buffer;
    session.command("vsplit").unwrap();
    session.command("new").unwrap();
    let focused = session.focus;
    let invoke = |argument| CommandAction::Invoke {
        target: CommandTarget::View { view_id, buffer_id },
        name: "choose".into(),
        argument: json!({"argument": argument}),
    };

    let Some(CommandOutcome::ClientRequest { request }) =
        session.begin_operation("resolve", &invoke("rust"))
    else {
        panic!("script should return its requested native action to the client");
    };
    assert_eq!(
        request,
        HostRequest::Action {
            name: "set-language".into(),
            argument: json!({"argument":"rust"})
        }
    );
    let target = CommandTarget::View { view_id, buffer_id };
    assert_eq!(
        session.begin_operation(
            "set",
            &CommandAction::Request {
                target: target.clone(),
                request,
            },
        ),
        Some(CommandOutcome::Applied)
    );
    assert_eq!(
        session.begin_operation(
            "read",
            &CommandAction::Invoke {
                target,
                name: "lang".into(),
                argument: json!({})
            }
        ),
        Some(CommandOutcome::Message {
            message: "rust".into()
        })
    );
    assert_eq!(session.focus, focused);
    session.command("lang").unwrap();
    assert_eq!(session.message, "text");

    session.focus_view(view_id).unwrap();
    session.command("new").unwrap();
    assert!(matches!(
        session.begin_operation("stale", &invoke("rust")),
        Some(CommandOutcome::Failed { .. })
    ));
    session.command("lang").unwrap();
    assert_eq!(session.message, "text");
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
            argument: Value::Null,
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
            role: ItemsRole::Standalone,
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
