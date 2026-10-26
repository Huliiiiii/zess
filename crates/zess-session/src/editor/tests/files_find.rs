use super::*;

fn nested_files(fixture: &Fixture) {
    fs::create_dir_all(fixture.0.join("deep/closed/leaf")).unwrap();
    fs::write(fixture.0.join("deep/closed/leaf/first.txt"), "first\n").unwrap();
    fs::write(fixture.0.join("deep/closed/leaf/second.txt"), "second\n").unwrap();
    fs::write(fixture.0.join("root.txt"), "root\n").unwrap();
}

fn type_text(session: &mut Session, text: &str) {
    for character in text.chars() {
        session.input(&character.to_string());
    }
}

#[test]
fn recursive_queries_match_file_paths_and_open_the_selected_match() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    for (kind_key, query) in [
        (None, "leaf/first.txt"),
        (Some("A-g"), "**/first.?xt"),
        (Some("A-r"), r"^deep/.*/first\.txt$"),
        (Some("A-r"), r"leaf/first\.txt"),
    ] {
        let mut session = fixture.session(&[]);
        session.command("files").unwrap();
        settle(&mut session);
        session.input("f");
        if let Some(key) = kind_key {
            session.input(key);
        }
        type_text(&mut session, query);
        settle(&mut session);
        assert!(session.prompt.as_ref().unwrap().error.is_none(), "{query}");
        assert_eq!(
            session.views[&session.focus].item_ids(),
            ["deep/closed/leaf/first.txt"],
            "{query}"
        );
        session.input("Enter");
        settle(&mut session);
        let buffer = session.views[&session.focus].location.buffer;
        let Buffer::Text(text) = &session.buffers[&buffer] else {
            panic!("matching file was not opened: {query}");
        };
        assert_eq!(text.text(), "first\n", "{query}");
    }
}

#[test]
fn literal_queries_match_names_containing_query_kind_prefixes() {
    let fixture = Fixture::new();
    for name in ["re:notes.txt", "glob:notes.txt", "notes.txt"] {
        fs::write(fixture.0.join(name), name).unwrap();
    }
    for entry in ["/", "f"] {
        for query in ["re:", "glob:"] {
            let mut session = fixture.session(&[]);
            session.command("files").unwrap();
            settle(&mut session);
            session.input(entry);
            type_text(&mut session, query);
            settle(&mut session);
            assert!(matches!(
                session.prompt.as_ref().unwrap().query(),
                Some((zess_filter::QueryKind::Plain, text)) if text == query
            ));
            assert_eq!(
                session.views[&session.focus].item_ids(),
                [format!("{query}notes.txt")]
            );
        }
    }
}

#[test]
fn invalid_recursive_queries_preserve_results_and_selection_until_corrected() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("f");
    session.input("A-r");
    type_text(&mut session, ".");
    settle(&mut session);
    session.input("Down");
    let view = &session.views[&session.focus];
    let expected = view.item_ids();
    let selection = serde_json::to_value(&view.location.selection).unwrap();
    session.input("[");
    session.tick();
    let view = &session.views[&session.focus];
    assert_eq!(view.item_ids(), expected);
    assert_eq!(
        serde_json::to_value(&view.location.selection).unwrap(),
        selection
    );
    assert!(session.prompt.as_ref().unwrap().error.is_some());
    session.input("Backspace");
    assert!(session.prompt.as_ref().unwrap().error.is_none());
    assert_eq!(session.views[&session.focus].item_ids(), expected);
}

#[test]
fn query_kind_shortcuts_toggle_matching_and_allow_keymap_overrides() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("one.rs"), "one").unwrap();
    fs::write(fixture.0.join("two.txt"), "two").unwrap();
    for entry in ["/", "f"] {
        let mut session = fixture.session(&[]);
        session.command("files").unwrap();
        settle(&mut session);
        session.input(entry);
        type_text(&mut session, "*.rs");
        settle(&mut session);
        assert!(session.views[&session.focus].item_ids().is_empty());
        session.input("Left");
        let cursor = session.prompt.as_ref().unwrap().cursor;
        session.input("A-g");
        assert_eq!(session.prompt.as_ref().unwrap().text, "*.rs");
        assert_eq!(session.prompt.as_ref().unwrap().cursor, cursor);
        assert_eq!(
            session.frame().prompt,
            format!(
                "{}(glob):*.rs",
                if entry == "/" { "filter" } else { "find" }
            )
        );
        assert_eq!(session.views[&session.focus].item_ids(), ["one.rs"]);
        assert!(matches!(
            session.prompt.as_ref().unwrap().query(),
            Some((zess_filter::QueryKind::Glob, "*.rs"))
        ));
        session.input("A-g");
        assert!(session.views[&session.focus].item_ids().is_empty());
        session.input("A-r");
        assert_eq!(session.prompt.as_ref().unwrap().cursor, cursor);
        assert_eq!(
            session.frame().prompt,
            format!("{}(re):*.rs", if entry == "/" { "filter" } else { "find" })
        );
        assert!(session.prompt.as_ref().unwrap().error.is_some());
        session.input("A-r");
        assert!(matches!(
            session.prompt.as_ref().unwrap().query(),
            Some((zess_filter::QueryKind::Plain, "*.rs"))
        ));
        session
            .scripts
            .as_mut()
            .unwrap()
            .load_source(
                "map-key \"global\" \"prompt\" \"A-g\" \"prompt.clear\"",
                Path::new("query-shortcut.w"),
            )
            .unwrap();
        session.input("A-g");
        assert_eq!(session.prompt.as_ref().unwrap().text, "");
        assert!(session.prompt.as_ref().unwrap().error.is_none());
    }
}

#[test]
fn query_editing_keeps_the_matching_kind_when_moving_or_clearing_input() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("one.rs"), "one").unwrap();
    for entry in ["/", "f"] {
        let mut session = fixture.session(&[]);
        session.command("files").unwrap();
        settle(&mut session);
        session.input(entry);
        session.input("A-r");
        type_text(&mut session, "école");
        for key in ["Home", "Left", "Backspace"] {
            session.input(key);
        }
        assert_eq!(session.prompt.as_ref().unwrap().text, "école");
        for key in ["X", "Delete"] {
            session.input(key);
        }
        assert_eq!(session.prompt.as_ref().unwrap().text, "Xcole");
        for key in ["End", "Backspace"] {
            session.input(key);
        }
        assert_eq!(session.prompt.as_ref().unwrap().text, "Xcol");
        session.input("C-u");
        assert_eq!(session.prompt.as_ref().unwrap().text, "");
        assert!(matches!(
            session.prompt.as_ref().unwrap().query(),
            Some((zess_filter::QueryKind::Regex, ""))
        ));
        assert!(session.prompt.as_ref().unwrap().error.is_none());
    }
}

#[test]
fn create_and_rename_input_preserves_the_source_selection() {
    let fixture = Fixture::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(fixture.0.join(name), name).unwrap();
    }
    let mut session = fixture.session(&[]);
    session.scripts.as_mut().unwrap().load_source(
        "map-key \"files\" \"insert\" \"x\" \"data.move-down\"\nmap-key \"files\" \"prompt\" \"y\" \"data.move-down\"",
        Path::new("file-input-key.w"),
    ).unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    session.input("j");
    settle(&mut session);
    let source = session.focus;
    let selected = selected_row(&session, source).id.clone();
    for command in ["files.create", "files.rename"] {
        session.command(command).unwrap();
        session.input("C-u");
        for key in [
            "Down", "Up", "Tab", "S-Tab", "C-n", "C-p", "x", "y", "j", "k",
        ] {
            session.input(key);
            assert_eq!(
                selected_row(&session, source).id,
                selected,
                "{command}: {key}"
            );
        }
        assert_eq!(
            session.frame().prompt,
            format!(
                "{}: xyjk",
                if command == "files.create" {
                    "create"
                } else {
                    "rename"
                }
            )
        );
        session.input("Esc");
        assert!(session.prompt.is_none());
        assert_eq!(selected_row(&session, source).id, selected);
        assert!(!session.modified());
    }
}

#[test]
fn minibuffer_input_preserves_the_text_and_its_mode() {
    let fixture = Fixture::new();
    let path = fixture.0.join("source.txt");
    fs::write(&path, "source\n").unwrap();
    let mut session = fixture.session(&[path]);
    session
        .scripts
        .as_mut()
        .unwrap()
        .load_source(
            r#"
require "zess/ui"
register-command "ask" "Ask for input"
  lambda : argument
    request-mini-buffer "answer" ""
map-key "text" "insert" "x" "text.insert-newline"
map-key "text" "prompt" "y" "text.insert-newline"
"#,
            Path::new("text-prompt.w"),
        )
        .unwrap();
    session.input("i");
    session.command("ask").unwrap();
    for key in ["x", "y", "Down", "Up", "Left", "Backspace", "z"] {
        session.input(key);
    }
    assert_eq!(session.frame().prompt, "answer: zy");
    session.input("Esc");
    assert!(session.prompt.is_none());
    let Buffer::Text(text) = &session.buffers[&session.views[&session.focus].location.buffer]
    else {
        panic!("source is no longer a text buffer");
    };
    assert_eq!(text.text(), "source\n");
    assert_eq!(text.view(session.focus).mode(), "insert");
}

#[test]
fn find_opens_the_selected_file_and_restores_the_editor() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    let original_path = fixture.0.join("root.txt");
    let mut session = fixture.session(&[original_path.clone()]);
    let editor = session.focus;
    let original_buffer = session.views[&editor].location.buffer;
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert!(
        !session.views[&source]
            .item_ids()
            .contains(&"deep/closed/leaf/first.txt".into())
    );
    session.input("f");
    type_text(&mut session, "closed/leaf/");
    settle(&mut session);
    assert_eq!(session.views[&source].location.mode.as_str(), "find");
    assert_eq!(
        session.views[&source].item_ids(),
        ["deep/closed/leaf/first.txt", "deep/closed/leaf/second.txt"]
    );
    session.input("Tab");
    assert_eq!(
        selected_row(&session, source).id,
        "deep/closed/leaf/second.txt"
    );
    session.input("S-Tab");
    assert_eq!(
        selected_row(&session, source).id,
        "deep/closed/leaf/first.txt"
    );
    session.input("Tab");
    session.input("Enter");
    settle(&mut session);
    assert!(session.prompt.is_none());
    assert_eq!(session.focus, editor);
    assert!(session.preview_target(editor).is_none());
    let Buffer::Text(text) = &session.buffers[&session.views[&editor].location.buffer] else {
        panic!("selected file was not opened");
    };
    assert_eq!(
        text.path(),
        Some(fixture.0.join("deep/closed/leaf/second.txt").as_path())
    );
    assert_eq!(text.text(), "second\n");
    let Buffer::Text(original) = &session.buffers[&original_buffer] else {
        panic!("original buffer was lost");
    };
    assert_eq!(original.path(), Some(original_path.as_path()));
    assert_eq!(original.text(), "root\n");
}

#[test]
fn empty_search_keeps_the_prompt_open() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("f");
    type_text(&mut session, "missing");
    settle(&mut session);
    session.input("Enter");
    assert!(session.prompt.is_some());
    assert_eq!(session.focus, source);
    assert!(session.views[&source].item_ids().is_empty());
    session.input("Esc");
    assert!(session.prompt.is_none());
    assert_eq!(session.views[&source].location.mode.as_str(), "normal");
}

#[test]
fn failed_open_keeps_search_available() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("f");
    type_text(&mut session, "first.txt");
    settle(&mut session);
    fs::remove_file(fixture.0.join("deep/closed/leaf/first.txt")).unwrap();
    fs::create_dir(fixture.0.join("deep/closed/leaf/first.txt")).unwrap();
    session.input("Enter");
    assert!(session.prompt.is_some());
    assert_eq!(session.focus, source);
    assert_eq!(
        session.views[&source].item_ids(),
        ["deep/closed/leaf/first.txt"]
    );
    session.input("Esc");
    assert!(session.prompt.is_none());
    assert_eq!(session.views[&source].location.mode.as_str(), "normal");
}

#[test]
fn incoming_search_results_preserve_the_selected_file() {
    let fixture = Fixture::new();
    for index in 0..20 {
        let directory = fixture.0.join(format!("directory-{index:02}"));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("file.txt"), "content\n").unwrap();
    }
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("f");
    wait_until(&mut session, |session| {
        session.views[&source].location.display.rows.len() >= 2
            && session.views[&source].location.display.loading
    });
    session.input("Down");
    let selected = selected_row(&session, source).id.clone();
    let count = session.views[&source].location.display.rows.len();
    settle(&mut session);
    assert!(session.views[&source].location.display.rows.len() > count);
    assert_eq!(selected_row(&session, source).id, selected);
}

#[test]
fn escape_restores_the_tree_and_selection() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("j");
    settle(&mut session);
    let source = session.focus;
    let original = session.views[&source].location.selection.clone();
    let rows = session.views[&source].item_ids();
    session.input("f");
    type_text(&mut session, "first");
    session.input("Esc");
    let stats = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert!(
        stats["jobs"].as_u64().unwrap() <= 1,
        "only the restored preview may still be loading: {stats}"
    );
    settle(&mut session);
    assert!(session.prompt.is_none());
    assert_eq!(
        session.views[&source].location.selection.primary,
        original.primary
    );
    assert_eq!(
        session.views[&source].location.selection.selected,
        original.selected
    );
    assert_eq!(
        session.views[&source].location.selection.anchor,
        original.anchor
    );
    assert_eq!(session.views[&source].item_ids(), rows);
    session.input("f");
    type_text(&mut session, "second");
    settle(&mut session);
    assert_eq!(
        session.views[&source].item_ids(),
        ["deep/closed/leaf/second.txt"]
    );
    session.input("Esc");
    settle(&mut session);
    assert_eq!(
        session.views[&source].location.selection.primary,
        original.primary
    );
    assert_eq!(
        session.views[&source].location.selection.selected,
        original.selected
    );
    assert_eq!(
        session.views[&source].location.selection.anchor,
        original.anchor
    );
    assert_eq!(session.views[&source].item_ids(), rows);
}

#[test]
fn recursive_search_respects_the_ignored_paths_setting() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    fs::create_dir_all(fixture.0.join("ignored/deeper")).unwrap();
    fs::write(fixture.0.join("ignored/deeper/secret.txt"), "secret\n").unwrap();
    fs::write(fixture.0.join("deep/closed/leaf/hidden.txt"), "hidden\n").unwrap();
    fs::write(fixture.0.join(".gitignore"), "ignored/\nhidden.txt\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("f");
    settle(&mut session);
    let paths = session.views[&source].item_ids();
    assert!(paths.contains(&"deep/closed/leaf/first.txt".into()));
    assert!(!paths.contains(&"ignored/deeper/secret.txt".into()));
    assert!(!paths.contains(&"deep/closed/leaf/hidden.txt".into()));
    session.input("Esc");
    session.command("files.toggle_ignored").unwrap();
    session.input("f");
    type_text(&mut session, "secret");
    settle(&mut session);
    assert_eq!(
        session.views[&source].item_ids(),
        ["ignored/deeper/secret.txt"]
    );
    session.input("Enter");
    settle(&mut session);
    let Buffer::Text(text) = &session.buffers[&session.views[&session.focus].location.buffer]
    else {
        panic!("selected file was not opened");
    };
    assert_eq!(
        text.path(),
        Some(fixture.0.join("ignored/deeper/secret.txt").as_path())
    );
}

#[test]
fn find_uses_configured_bindings_and_appears_in_help() {
    let fixture = Fixture::new();
    nested_files(&fixture);
    let mut session = fixture.session(&[]);
    session.scripts.as_mut().unwrap().load_source(
        "map-modal \"files\" \"F\" \"files.find\"\nmap-key \"files.find\" \"prompt\" \"C-j\" \"data.move-down\"",
        Path::new("custom-key.w"),
    ).unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    session.input("?");
    let help = session.help.as_ref().unwrap().view;
    assert!(
        session.views[&help]
            .location
            .display
            .rows
            .iter()
            .find(|row| row.id.split(", ").any(|key| key == "F"))
            .unwrap()
            .text
            .ends_with("Find files recursively")
    );
    session.input("q");
    session.input("F");
    type_text(&mut session, "leaf/");
    settle(&mut session);
    session.input("C-j");
    assert_eq!(
        selected_row(&session, session.focus).id,
        "deep/closed/leaf/second.txt"
    );
}

#[test]
fn filtering_shows_the_query_once_and_preserves_git_page_headers() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("needle.txt"), "content\n").unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.0)
        .status()
        .unwrap();
    assert!(status.success());
    for kind in ["files", "git"] {
        let mut session = fixture.session(&[]);
        session.command(kind).unwrap();
        settle(&mut session);
        let source = session.focus;
        session.input("/");
        type_text(&mut session, "needle");
        assert_eq!(session.frame().prompt, "filter:needle");
        assert_eq!(session.views[&source].location.display.header, "");
        if kind == "git" {
            let (rows, _) = session.render_view(source, 80, 24);
            assert_eq!(
                rows[0]
                    .spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
                    .trim_end(),
                "[Changes]  History"
            );
        }
        session.input("Enter");
        assert_eq!(
            session.views[&source].location.display.header,
            if kind == "git" {
                "  /needle"
            } else {
                "/needle"
            }
        );
        let (rows, _) = session.render_view(source, 80, 24);
        assert_eq!(
            rows[0]
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
                .trim_end(),
            if kind == "git" {
                "[Changes]  History    /needle"
            } else {
                "/needle"
            }
        );
        assert_eq!(
            session.views[&source].item_ids(),
            [if kind == "git" {
                "file:needle.txt"
            } else {
                "needle.txt"
            }]
        );
        session.input("/");
        type_text(&mut session, "missing");
        session.input("Esc");
        assert_eq!(
            session.views[&source].location.display.header,
            if kind == "git" {
                "  /needlemissing"
            } else {
                "/needlemissing"
            }
        );
        let (rows, _) = session.render_view(source, 80, 24);
        assert_eq!(
            rows[0]
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
                .trim_end(),
            if kind == "git" {
                "[Changes]  History    /needlemissing"
            } else {
                "/needlemissing"
            }
        );
        assert!(session.views[&source].item_ids().is_empty());
    }
}
