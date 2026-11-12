use super::*;

fn pasted(session: &mut Session) -> String {
    session.command("new").unwrap();
    session.input("p");
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("new did not open a text buffer");
    };
    text.text()
}

fn replaced_lines(session: &mut Session) -> String {
    session.command("new").unwrap();
    for key in ["i", "x", "Enter", "x", "Esc", "%", "A-s", "R"] {
        session.input(key);
    }
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("new did not open a text buffer");
    };
    text.text()
}

#[test]
fn explicitly_selected_unnamed_register_is_shared_with_text() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join("folder/file.txt"), "content\n").unwrap();
    for (key, expected) in [
        ("p", "folder/file.txt".to_owned()),
        ("a", fixture.0.join("folder/file.txt").display().to_string()),
        ("n", "file.txt".to_owned()),
    ] {
        let mut session = fixture.session(&[]);
        session.command("files").unwrap();
        settle(&mut session);
        session.command("files.right").unwrap();
        settle(&mut session);
        let view = session.views.get_mut(&session.focus).unwrap();
        view.location.selection = ItemSelection {
            primary: Some("folder/file.txt".into()),
            selected: vec!["folder/file.txt".into()],
            anchor: Some("folder/file.txt".into()),
        };
        for key in ["\"", "\"", "y", key] {
            session.input(key);
        }
        assert_eq!(pasted(&mut session), expected);
        assert!(!session.flush_clipboard_writes(|_| panic!("ordinary yank copied to clipboard")));
    }
}

#[test]
fn path_yanks_use_the_system_clipboard_by_default() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    for (command, expected) in [
        ("files.yank-path", "note.txt".to_owned()),
        (
            "files.yank-absolute-path",
            fixture.0.join("note.txt").display().to_string(),
        ),
        ("files.yank-name", "note.txt".to_owned()),
    ] {
        session.execute_command(command, json!({})).unwrap();
        let mut clipboard = Vec::new();
        session.flush_clipboard_writes(|text| {
            clipboard.push(text);
            Ok(())
        });
        assert_eq!(clipboard, [expected]);
    }
    assert_eq!(pasted(&mut session), "");
}

#[test]
fn configured_yank_register_applies_after_reload_and_explicit_registers_take_priority() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let config = fixture.0.join("config.w");
    let configure = |register| {
        fs::write(
            &config,
            format!("set-view-options! \"files\" : obj 'default-yank-register {register}\n"),
        )
        .unwrap();
    };
    configure(r#""a""#);
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    session.input("j");
    settle(&mut session);
    for key in ["y", "n", "\"", "+", "y", "n"] {
        session.input(key);
    }
    let mut clipboard = Vec::new();
    session.flush_clipboard_writes(|text| {
        clipboard.push(text);
        Ok(())
    });
    assert_eq!(clipboard, ["note.txt"]);
    configure(r#""*""#);
    assert!(session.reload(), "{}", session.message);
    for key in ["y", "n"] {
        session.input(key);
    }
    let mut primary = Vec::new();
    session.flush_primary_clipboard_writes(|text| {
        primary.push(text);
        Ok(())
    });
    assert_eq!(primary, ["note.txt"]);
    session.command("new").unwrap();
    for key in ["\"", "a", "p"] {
        session.input(key);
    }
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("new did not open a text buffer");
    };
    assert_eq!(text.text(), "note.txt");
}

#[test]
fn path_yanks_use_the_selected_clipboard() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    for (key, expected) in [
        ("p", "note.txt".to_owned()),
        ("a", fixture.0.join("note.txt").display().to_string()),
        ("n", "note.txt".to_owned()),
    ] {
        for register in ["+", "*"] {
            for key in ["\"", register, "y", key] {
                session.input(key);
            }
            let mut clipboard = Vec::new();
            let mut primary = Vec::new();
            session.flush_clipboard_writes(|text| {
                clipboard.push(text);
                Ok(())
            });
            session.flush_primary_clipboard_writes(|text| {
                primary.push(text);
                Ok(())
            });
            if register == "+" {
                assert_eq!(clipboard, [expected.clone()]);
                assert!(primary.is_empty());
            } else {
                assert_eq!(primary, [expected.clone()]);
                assert!(clipboard.is_empty());
            }
        }
    }
}

#[test]
fn a_named_register_is_used_for_one_command_and_shared_with_text() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    for key in ["\"", "a", "y", "n", "\"", "\"", "y", "a"] {
        session.input(key);
    }
    assert_eq!(
        pasted(&mut session),
        fixture.0.join("note.txt").display().to_string()
    );
    session.command("new").unwrap();
    for key in ["\"", "a", "p"] {
        session.input(key);
    }
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("new did not open a text buffer");
    };
    assert_eq!(text.text(), "note.txt");
    assert!(!session.flush_clipboard_writes(|_| panic!("named yank copied to clipboard")));
}

#[test]
fn cancelling_register_selection_discards_the_count_and_keeps_select_mode() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    for key in ["v", "3", "\"", "Esc"] {
        session.input(key);
    }
    assert!(session.command_count().is_empty());
    let view = &session.views[&session.focus];
    assert!(matches!(view.register, RegisterInput::None));
    assert!(view.location.mode == ViewMode::Select);
}

#[test]
fn choosing_a_digit_register_preserves_the_count_until_the_command() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    for key in ["3", "\"", "2", "y"] {
        session.input(key);
    }
    assert_eq!(session.command_count().value(), Some(3));
    assert!(matches!(
        session.views[&session.focus].register,
        RegisterInput::Selected('2')
    ));
    session.input("p");
    assert!(session.command_count().is_empty());
    assert!(matches!(
        session.views[&session.focus].register,
        RegisterInput::None
    ));
    session.command("new").unwrap();
    for key in ["\"", "2", "p"] {
        session.input(key);
    }
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("new did not open a text buffer");
    };
    assert_eq!(text.text(), "note.txt");
}

#[test]
fn a_failed_command_discards_the_selected_register() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    for key in ["\"", "#", "y", "p"] {
        session.input(key);
    }
    assert!(session.message.contains("does not support writing"));
    for key in ["\"", "\"", "y", "p"] {
        session.input(key);
    }
    assert_eq!(pasted(&mut session), "note.txt");
}

#[test]
fn clicking_the_same_view_discards_the_register_prefix() {
    use zess_ui::{MouseEvent, MouseKind};

    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    for keys in [&["3", "\""][..], &["3", "\"", "*"][..]] {
        session.command("files").unwrap();
        settle(&mut session);
        for key in keys {
            session.input(key);
        }
        let frame = session.frame();
        let region = session
            .mouse_regions
            .iter()
            .find(|region| region.target == RenderTarget::View(session.focus))
            .unwrap();
        session.mouse(
            frame.mouse_context,
            MouseEvent {
                kind: MouseKind::Press,
                column: region.body.x as u16,
                row: region.body.y as u16,
            },
        );
        assert!(session.command_count().is_empty());
        for key in ["y", "p"] {
            session.input(key);
        }
        let mut clipboard = Vec::new();
        session.flush_clipboard_writes(|text| {
            clipboard.push(text);
            Ok(())
        });
        assert_eq!(clipboard, ["note.txt"]);
        assert!(
            !session.flush_primary_clipboard_writes(|_| panic!("click retained primary register"))
        );
    }
}

#[test]
fn content_yanks_copy_all_selected_files_to_the_system_clipboard() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "first").unwrap();
    fs::write(fixture.0.join("b.txt"), "second").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session
        .views
        .get_mut(&session.focus)
        .unwrap()
        .location
        .selection = ItemSelection {
        primary: Some("b.txt".into()),
        selected: vec!["b.txt".into(), "a.txt".into()],
        anchor: Some("a.txt".into()),
    };
    session
        .execute_command("files.yank-content", json!({}))
        .unwrap();
    settle(&mut session);
    let mut clipboard = Vec::new();
    session.flush_clipboard_writes(|text| {
        clipboard.push(text);
        Ok(())
    });
    assert_eq!(clipboard, ["second\nfirst"]);
}

#[test]
fn diff_yanks_copy_the_working_changes_from_head() {
    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Zess Tests",
                "-c",
                "user.email=zess@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    fs::write(fixture.0.join("note.txt"), "old\n").unwrap();
    git(&["init", "--quiet"]);
    git(&["add", "note.txt"]);
    git(&["commit", "--quiet", "-m", "initial"]);
    fs::write(fixture.0.join("note.txt"), "new\n").unwrap();
    let expected = git(&[
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--abbrev=7",
        "HEAD",
        "--",
        "note.txt",
    ]);
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session
        .views
        .get_mut(&session.focus)
        .unwrap()
        .location
        .selection = ItemSelection {
        primary: Some("note.txt".into()),
        selected: vec!["note.txt".into()],
        anchor: Some("note.txt".into()),
    };
    session
        .execute_command("files.yank-diff", json!({}))
        .unwrap();
    settle(&mut session);
    let mut clipboard = Vec::new();
    session.flush_clipboard_writes(|text| {
        clipboard.push(text);
        Ok(())
    });
    assert_eq!(clipboard, [expected]);
}

#[test]
fn a_read_only_content_yank_does_not_discard_a_later_clipboard_yank() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    for key in ["\"", "#", "y", "f"] {
        session.input(key);
    }
    assert!(session.message.contains("does not support writing"));
    for key in ["\"", "+", "y", "n"] {
        session.input(key);
    }
    settle(&mut session);
    let mut clipboard = Vec::new();
    session.flush_clipboard_writes(|text| {
        clipboard.push(text);
        Ok(())
    });
    assert_eq!(clipboard, ["note.txt"]);
}

#[test]
fn a_deferred_script_yank_uses_the_register_selected_at_launch() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"
require "zess/buffer"
require "zess/ui"
register-buffer "deferred-yank"
  lambda : input
    let*
      : event : input-event input
        pending : buffer-input-view input
      cond
        : equal? event "yank"
          buffer-result null (or (input-register input) #\") null
            buffer-output #f #f : list : request-schedule 10
        : and (equal? event "tick") (char? pending)
          buffer-result null #f null
            buffer-output #f #f : list : request-yank (list "saved") pending
        else
          buffer-result null pending null
            buffer-output #f #f '()
register-command "deferred" "Open a deferred yank buffer"
  lambda : argument
    request-open-buffer "deferred-yank" : obj
register-buffer-command "deferred.yank" "Yank later" "deferred-yank" "yank"
map-modal "deferred-yank" "\"" "data.select-register"
map-modal "deferred-yank" "y" "deferred.yank"
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("deferred").unwrap();
    for key in ["\"", "+", "y", "\"", "*"] {
        session.input(key);
    }
    assert!(!session.flush_clipboard_writes(|_| panic!("script published before its tick")));
    settle(&mut session);
    let mut clipboard = Vec::new();
    session.flush_clipboard_writes(|text| {
        clipboard.push(text);
        Ok(())
    });
    assert_eq!(clipboard, ["saved"]);
    assert!(!session.flush_primary_clipboard_writes(|_| panic!("script used a later register")));
}

#[test]
fn script_commands_can_yank_through_the_shared_host_request() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(&config, "register-command \"custom-yank\" \"Yank values\"\n  lambda : argument\n    request-yank : list \"custom\" \"values\"\n").unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.command("custom-yank").unwrap();
    assert_eq!(replaced_lines(&mut session), "custom\nvalues");
}
