use super::*;

fn git(fixture: &Fixture, arguments: &[&str]) {
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
        .args(arguments)
        .current_dir(&fixture.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn non_text_views_cannot_use_another_buffers_outline() {
    for kind in ["files", "git", "diff", "items", "help"] {
        let fixture = Fixture::new();
        let path = fixture.0.join("example.rs");
        fs::write(&path, "fn original() {}\n").unwrap();
        let mut session = fixture.session(&[path.clone()]);
        match kind {
            "files" => {
                session.command("files").unwrap();
                settle(&mut session);
            }
            "git" | "diff" => {
                git(&fixture, &["init", "--quiet"]);
                git(&fixture, &["add", "example.rs"]);
                git(&fixture, &["commit", "--quiet", "-m", "initial"]);
                fs::write(&path, "fn changed() {}\n").unwrap();
                session.command("git").unwrap();
                settle(&mut session);
                if kind == "diff" {
                    session.focus_preview(session.focus).unwrap();
                    assert!(matches!(
                        session.buffers[&session.views[&session.focus].location.buffer],
                        Buffer::PreviewDiff { .. }
                    ));
                }
            }
            "items" => {
                let buffer = session.add_buffer(Buffer::Items {
                    role: ItemsRole::Standalone,
                    kind: "results".into(),
                    path: String::new(),
                    display: Display::default(),
                    help_context: None,
                });
                session.switch_buffer(buffer).unwrap();
            }
            "help" => session.command("help").unwrap(),
            _ => unreachable!(),
        }
        let focus = session.focus;
        let buffer = session.views[&focus].location.buffer;
        let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
        {
            let (scopes, mode) = session.key_context();
            session.scripts.as_ref().unwrap().with_keymap(|keymap| {
                assert!(
                    keymap
                        .effective_bindings(&scopes, mode)
                        .iter()
                        .all(|binding| binding.command != "outline"),
                    "{kind}"
                );
            });
        }
        session.input("space");
        session.input("o");
        assert_eq!(session.focus, focus, "{kind}");
        assert_eq!(session.views[&focus].location.buffer, buffer, "{kind}");
        assert_eq!(
            session.buffers.keys().copied().collect::<HashSet<_>>(),
            buffers,
            "{kind}"
        );
        assert!(session.command("outline").is_err(), "{kind}");
        assert_eq!(session.focus, focus, "{kind}");
        assert_eq!(session.views[&focus].location.buffer, buffer, "{kind}");
        assert_eq!(
            session.buffers.keys().copied().collect::<HashSet<_>>(),
            buffers,
            "{kind}"
        );
    }
}

#[test]
fn plain_text_cannot_open_an_outline() {
    let fixture = Fixture::new();
    let path = fixture.0.join("example.rs");
    fs::write(&path, "fn original() {}\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.command("lang text").unwrap();
    let buffer = session.views[&session.focus].location.buffer;
    assert!(session.command("outline").is_err());
    assert_eq!(session.views[&session.focus].location.buffer, buffer);
}

#[test]
fn text_context_does_not_require_outline_support() {
    let scripts = Fixture::new();
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "plain text\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    session
        .scripts
        .as_mut()
        .unwrap()
        .load(&scripts.write(
            "text-info.w",
            r#"
require "zess/buffer"
require "zess/ui"
register-buffer "text-info"
  lambda : input
    buffer-result #f #f : buffer-input-memory input
      buffer-output
        buffer-display (text-context-path (input-text input)) "" '() #f ""
        . #f '()
register-context "text-info" "text.path" : array "init"
register-command "source-info" "Show text source"
  lambda : argument
    request-open-buffer "text-info" argument
"#,
        ))
        .unwrap();
    session.command("source-info").unwrap();
    assert_eq!(
        session.views[&session.focus].location.display.title,
        path.to_string_lossy()
    );
}

#[test]
fn languages_with_no_symbols_can_open_an_outline() {
    let fixture = Fixture::new();
    let path = fixture.0.join("example.rs");
    fs::write(&path, "// no definitions\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.input("space");
    session.input("o");
    assert!(matches!(
        &session.buffers[&session.views[&session.focus].location.buffer],
        Buffer::Data { kind, .. } if kind == "outline"
    ));
    assert!(
        session.views[&session.focus]
            .location
            .display
            .rows
            .is_empty()
    );
}

#[test]
fn snapshot_outlines_return_to_their_historical_source() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let snapshot = FileSnapshot {
        id: SnapshotId {
            repository: fixture.0.to_string_lossy().into_owned(),
            commit: "historical".into(),
            path: "example.rs".into(),
            blob: "historical-blob".into(),
        },
        text: "// historical source\nfn historical() {}\n".into(),
    };
    session
        .open_snapshot(session.focus, snapshot, SnapshotPresentation::Editor)
        .unwrap();
    let source = session.views[&session.focus].location.buffer;
    session.command("outline").unwrap();
    assert_eq!(
        session.views[&session.focus].location.display.rows[0].text,
        "  function_item historical"
    );
    session.input("Enter");
    assert_eq!(session.views[&session.focus].location.buffer, source);
    let text = session
        .buffers
        .get_mut(&source)
        .and_then(Buffer::text)
        .unwrap();
    assert_eq!(text.view(session.focus).cursor().0, 1);
}

#[test]
fn split_outlines_keep_their_source_when_another_text_view_gains_focus() {
    let fixture = Fixture::new();
    let first = fixture.0.join("first.rs");
    let second = fixture.0.join("second.rs");
    fs::write(&first, "fn original() {}\n").unwrap();
    fs::write(&second, "fn unrelated() {}\n").unwrap();
    let mut session = fixture.session(&[first]);
    let source = session.views[&session.focus].location.buffer;
    session.command("outline").unwrap();
    let outline = session.focus;
    session.command("vsplit").unwrap();
    let cloned = session.focus;
    session.focus_view(outline).unwrap();
    let unrelated = session.load_file(&second).unwrap();
    session.switch_buffer(unrelated).unwrap();
    session.focus_view(cloned).unwrap();
    session.dispatch(cloned, "refresh", Value::Null).unwrap();
    assert_eq!(
        session.views[&cloned]
            .location
            .display
            .rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["  function_item original"]
    );
    session.input("Enter");
    assert_eq!(session.views[&session.focus].location.buffer, source);
    assert_eq!(session.views[&outline].location.buffer, unrelated);
}

#[test]
fn an_outline_keeps_its_rows_until_reloaded_source_syntax_is_available() {
    let fixture = Fixture::new();
    let path = fixture.write("example.rs", "fn original() {}\n");
    let mut session = fixture.session(&[path.clone()]);
    let source = session.views[&session.focus].location.buffer;
    session.command("outline").unwrap();
    let outline = session.focus;

    fs::write(path, "fn revised() {}\n").unwrap();
    session.reload_document(source).unwrap();
    assert_eq!(
        session.views[&outline].location.display.rows[0].text,
        "  function_item original"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.tick();
        if session.views[&outline].location.display.rows[0].text == "  function_item revised" {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}
