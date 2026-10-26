use super::*;

#[test]
fn prefix_hints_align_title_and_keys_with_space_on_both_sides() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input("space");
    let frame = session.frame();
    let popup = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .filter(|span| ["status", "popup"].contains(&span.style.as_str()))
        .map(|span| span.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(popup[0], format!(" space{}", " ".repeat(40)));
    for expected in [
        " ?    Show buffer keys",
        " e    Open file tree",
        " g    Open Git changes",
    ] {
        assert!(popup.iter().any(|row| row.starts_with(expected)));
    }
    assert!(popup.iter().all(|row| row.ends_with(' ')));
    assert_eq!(
        popup.last().unwrap(),
        &" y    Copy selections to the system clipboard "
    );
}

#[test]
fn prefix_hints_keep_their_margins_when_unicode_is_clipped() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "register-command \"custom\" \"打开文件\"\n  lambda : argument\n    request-action \"help\" argument\nmap-modal \"text\" (list \"space\" \"z\") \"custom\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.resize(12, 24);
    session.input("space");
    let frame = session.frame();
    let popup = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| span.style == "popup" && span.text.contains('打'))
        .unwrap();
    assert_eq!(popup.text, " z    打开  ");
    assert_eq!(UnicodeWidthStr::width(popup.text.as_str()), 12);
}

#[test]
fn pending_file_actions_show_their_effective_bindings() {
    let fixture = Fixture::new();
    for (width, height, select) in [(100, 30, false), (40, 12, true)] {
        let mut session = fixture.session(&[]);
        session.resize(width, height);
        session.command("files").unwrap();
        settle(&mut session);
        if select {
            session.input("v");
        }
        session.input("y");
        session.tick();
        assert_eq!(session.pending, ["y"]);
        let frame = rendered(&mut session);
        for description in [
            "Yank relative paths",
            "Yank absolute paths",
            "Yank file names",
            "Yank file contents",
            "Yank file diffs",
        ] {
            assert!(frame.contains(description), "{frame}");
        }
    }
}

#[test]
fn scrolling_keeps_the_current_key_group_heading_visible() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(120, 12);
    session.command("help").unwrap();
    let help = session.focus;
    session.render_view(help, 120, 8);
    for _ in 0..2 {
        session.input("C-d");
    }
    let (rows, _) = session.render_view(help, 120, 8);
    assert!(session.views[&help].location.scroll > 0);
    assert_eq!(
        rows[1]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim(),
        "Text"
    );
}

#[test]
fn alternative_keys_for_the_same_command_share_one_help_row() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.command("help").unwrap();
    let rows = &session.views[&session.focus].location.display.rows;
    let bindings = rows
        .iter()
        .filter(|row| row.id.split(", ").any(|key| key == "C-w h"))
        .collect::<Vec<_>>();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].id, "C-w C-h, C-w h");
    session.scripts.as_ref().unwrap().with_keymap(|keymap| {
        let bindings = keymap.effective_bindings(&["global", "text"], "normal");
        for keys in [["C-w", "h"], ["C-w", "C-h"]] {
            assert!(bindings.iter().any(|binding| {
                binding.keys.iter().map(String::as_str).eq(keys) && binding.command == "focus-left"
            }));
        }
    });
}

#[test]
fn register_prefix_remains_visible_until_cancelled() {
    let fixture = Fixture::new();
    for files in [false, true] {
        let mut session = fixture.session(&[]);
        if files {
            session.command("files").unwrap();
        }
        session.input("\"");
        assert_eq!(
            session.frame().prompt,
            "Register: type a name · Esc to cancel"
        );
        session.input("a");
        assert_eq!(session.frame().prompt, "Register \"a");
        session.input("Esc");
        assert!(session.frame().prompt.is_empty());
    }
}

#[test]
fn help_title_stays_readable_when_focus_changes() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(120, 40);
    session.command("files").unwrap();
    let source = session.focus;
    session.input("?");
    session.input("p");
    let help = session.focus;
    for use_fallback in [false, true] {
        if use_fallback {
            session.views.get_mut(&help).unwrap().location.options["header-style"] = Value::Null;
        }
        for focus in [help, source] {
            session.focus_view(focus).unwrap();
            let frame = session.frame();
            let title = frame
                .rows
                .iter()
                .flat_map(|row| &row.spans)
                .find(|span| span.text.trim() == "Help: Files")
                .unwrap();
            assert_eq!(title.style, "ui.heading");
        }
    }
}

#[test]
fn plugin_keys_are_grouped_before_shared_and_global_keys() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "note").unwrap();
    let mut session = fixture.session(&[]);
    session.resize(120, 100);
    session.command("files").unwrap();
    session.input("?");
    let frame = session.frame();
    let headings = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .filter(|span| span.style == "status")
        .map(|span| span.text.trim())
        .filter(|text| ["Files", "Items", "Global"].contains(text))
        .collect::<Vec<_>>();
    assert_eq!(headings, ["Files", "Items", "Global"]);
    let view = &session.views[&session.focus];
    insta::assert_json_snapshot!(
        "default_key_groups",
        view.location
            .display
            .rows
            .iter()
            .map(|row| (&row.id, &row.text))
            .collect::<Vec<_>>()
    );
    session.input("%");
    let view = &session.views[&session.focus];
    assert!(!view.location.selection.selected.is_empty());
    assert!(
        view.location
            .selection
            .selected
            .iter()
            .all(|id| !id.is_empty())
    );
    assert_eq!(
        view.location.selection.selected.len(),
        view.location
            .display
            .rows
            .iter()
            .filter(|row| !row.id.is_empty())
            .count()
    );
    session.input("g");
    session.input("g");
    assert_ne!(
        session.views[&session.focus]
            .location
            .selection
            .primary
            .as_deref(),
        Some("")
    );
}

#[test]
fn pinned_groups_follow_reloaded_labels_and_effective_bindings() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "note").unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"register-key-scope "files" "Project files"
unmap-key "files" "normal" "r"
map-key "files" "normal" "z" "files.rename"
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config.clone())).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.resize(120, 100);
    session.command("files").unwrap();
    session.input("?");
    let help = rendered(&mut session);
    assert!(
        help.lines().any(|line| line.trim() == "Project files"),
        "{help}"
    );
    let view = &session.views[&session.focus];
    assert_eq!(
        view.location
            .display
            .rows
            .iter()
            .find(|row| row.id == "z")
            .unwrap()
            .text,
        "z                    Rename selection"
    );
    assert!(!view.location.display.rows.iter().any(|row| row.id == "r"));
    session.input("p");
    assert!(session.help.is_none());
    let pinned = session.focus;
    fs::write(
        &config,
        r#"register-key-scope "files" "Workspace files"
unmap-key "files" "normal" "r"
map-key "files" "normal" "b" "files.rename"
"#,
    )
    .unwrap();
    session.command("config-reload").unwrap();
    assert_eq!(session.generation, 2, "{}", session.message);
    assert_eq!(session.focus, pinned);
    let (help, _) = session.render_view(pinned, 120, 100);
    assert_eq!(
        help[1]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "Workspace files"
    );
    let rows = &session.views[&pinned].location.display.rows;
    assert_eq!(
        rows.iter()
            .filter(|row| row.text == "Workspace files")
            .count(),
        1
    );
    assert_eq!(
        rows.iter().find(|row| row.id == "b").unwrap().text,
        "b                    Rename selection"
    );
    assert!(!rows.iter().any(|row| ["r", "z"].contains(&row.id.as_str())));
}

#[test]
fn unregistered_scope_uses_its_id_as_the_group_label() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"require "zess/ui"
register-command "custom-scope"
  lambda : argument
    configure-view : obj 'key-scope "project/custom"
map-modal "project/custom" "z" "files.rename"
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.resize(120, 100);
    session.command("files").unwrap();
    session.command("custom-scope").unwrap();
    session.input("?");
    let help = rendered(&mut session);
    assert!(
        help.lines().any(|line| line.trim() == "project/custom"),
        "{help}"
    );
    let rows = &session.views[&session.focus].location.display.rows;
    assert_eq!(
        rows.iter()
            .filter(|row| row.id.is_empty())
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["project/custom", "Files", "Items", "Global"]
    );
    assert_eq!(rows[1].id, "z");
    assert_eq!(rows[1].text, "z                    Rename selection");
}

#[test]
fn aliases_show_command_documentation_before_completion_and_with_arguments() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input(":");
    for key in "lang".chars() {
        session.input(&key.to_string());
    }
    assert_eq!(session.prompt.as_ref().unwrap().text, "lang");
    let frame = session.frame();
    let popup = &frame.rows[frame.rows.len() - 3..];
    assert_eq!(
        popup
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>(),
        [
            "Set or show the current buffer language",
            "Aliases: lang",
            "set-language                   git-commit-files.page-changes",
        ]
    );
    assert!(
        popup[0].spans.iter().all(|span| span.style == "popup")
            && popup[1]
                .spans
                .iter()
                .all(|span| span.style == "popup.secondary")
    );
    assert!(
        popup[2]
            .spans
            .iter()
            .filter(|span| !span.text.trim().is_empty())
            .all(
                |span| ["selection", "selection.match", "popup", "popup.match"]
                    .contains(&span.style.as_str())
            )
    );
    for key in " rust".chars() {
        session.input(&key.to_string());
    }
    let frame = session.frame();
    let documentation = &frame.rows[frame.rows.len() - 2..];
    assert_eq!(
        documentation
            .iter()
            .map(|row| {
                row.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>(),
        ["Set or show the current buffer language", "Aliases: lang",]
    );
    assert!(
        documentation[0]
            .spans
            .iter()
            .all(|span| span.style == "popup")
            && documentation[1]
                .spans
                .iter()
                .all(|span| span.style == "popup.secondary")
    );
}

#[test]
fn fuzzy_matches_stay_highlighted_when_the_candidate_is_selected() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    let query = "st-lnguge";
    assert_eq!(session.command_candidates(query).len(), 1);
    session.input(":");
    for key in query.chars() {
        session.input(&key.to_string());
    }
    let frame = session.frame();
    assert_eq!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|span| span.style == "selection.match")
            .map(|span| span.text.as_str())
            .collect::<String>(),
        query,
    );
    session.input("Tab");
    assert_eq!(session.prompt.as_ref().unwrap().text, "set-language");
    let frame = session.frame();
    assert_eq!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|span| span.style == "selection.match")
            .map(|span| span.text.as_str())
            .collect::<String>(),
        query,
    );
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.style == "selection" && !span.text.trim().is_empty())
    );
}

#[test]
fn first_matching_command_is_selected_and_submitted() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input(":");
    for key in "hlp".chars() {
        session.input(&key.to_string());
    }
    assert_eq!(session.prompt.as_ref().unwrap().text, "hlp");
    let frame = session.frame();
    assert_eq!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|span| span.style == "selection.match")
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "hlp"
    );
    assert_eq!(
        frame.rows[frame.rows.len() - 2]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "Show buffer keys"
    );
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|span| span.style == "selection" || span.style == "selection.match")
            .all(|span| !span.text.contains(' '))
    );
    session.input("Enter");
    assert!(session.help.is_some());
}

#[test]
fn navigation_moves_from_the_first_matching_command() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 24);
    session.input(":");
    let candidates = session.command_candidates("");
    assert!(candidates.len() > 1);
    session.input("Tab");
    assert_eq!(session.prompt.as_ref().unwrap().text, candidates[1].name);
    session.input("S-Tab");
    assert_eq!(session.prompt.as_ref().unwrap().text, candidates[0].name);
}

#[test]
fn empty_and_unmatched_commands_do_not_execute_a_candidate() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.input(":");
    session.input("Enter");
    assert!(session.help.is_none());
    session.input(":");
    for key in "zzzzzzzz".chars() {
        session.input(&key.to_string());
    }
    session.input("Enter");
    assert_eq!(session.message, "error: unknown command: zzzzzzzz");
    assert!(session.help.is_none());
}

#[test]
fn exact_command_takes_priority_over_an_alias_prefix() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "register-command-alias \"helpful\" \"set-language\"\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.input(":");
    for key in "help".chars() {
        session.input(&key.to_string());
    }
    assert_eq!(session.command_candidates("help")[0].name, "help");
    session.input("Enter");
    assert!(session.help.is_some());
}

#[test]
fn command_arguments_are_submitted_without_completion() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.input(":");
    for key in "lang unknown-language".chars() {
        session.input(&key.to_string());
    }
    session.input("Enter");
    assert_eq!(
        session.message,
        "error: unknown language \"unknown-language\""
    );
}

#[test]
fn matching_aliases_complete_the_canonical_command() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        r#"register-command-alias "语言" "set-language"
register-command-alias "dialect" "set-language"
"#,
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.resize(80, 24);
    session.input(":");
    session.input("语");
    let frame = session.frame();
    assert!(
        frame
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .all(|span| span.style != "popup.match")
    );
    session.input("Tab");
    assert_eq!(session.prompt.as_ref().unwrap().text, "set-language");
    let frame = session.frame();
    let candidate = &frame.rows[frame.rows.len() - 1];
    assert!(
        candidate
            .spans
            .iter()
            .any(|span| span.style == "selection" && span.text.trim() == "set-language")
    );
    let aliases = &frame.rows[frame.rows.len() - 2];
    assert_eq!(
        aliases
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
            .trim_end(),
        "Aliases: dialect, lang, 语言"
    );
    assert!(
        aliases
            .spans
            .iter()
            .all(|span| span.style == "popup.secondary")
    );
    assert_eq!(session.command_candidates("语").len(), 1);
}

#[test]
fn command_line_entries_exclude_internal_actions() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let candidates = session.command_candidates("");
    for name in [
        "data.move-down",
        "help.close",
        "preview.focus",
        "diff.select-hunk",
        "prompt.submit",
        "text.move-right",
        "data.count-1",
        "command-prompt",
        "key-sequence-cancel",
        "browser.open-source",
    ] {
        assert!(
            !candidates.iter().any(|candidate| candidate.name == name),
            "{name}"
        );
        let error = session.command(name).unwrap_err();
        assert_eq!(error.to_string(), format!("unknown command: {name}"));
    }
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.name == "set-language")
    );
    session
        .execute_command("command-prompt", json!({}))
        .unwrap();
    assert!(matches!(
        session.prompt.as_ref().unwrap().kind,
        PromptKind::Command
    ));
    session.input("Esc");
    session.input(":");
    assert!(matches!(
        session.prompt.as_ref().unwrap().kind,
        PromptKind::Command
    ));
}

#[test]
fn command_line_names_complete_independently_of_action_ids() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let scripts = session.scripts.as_mut().unwrap();
    scripts.register_native_action("custom.save", "Save a custom buffer");
    scripts.register_command_line("custom.save", "save-custom");
    scripts.register_command_alias("save-alias", "save-custom");
    let candidates = session.command_candidates("save-alias");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].name, "save-custom");
    assert_eq!(session.command_candidates("save-custom").len(), 1);
    assert!(
        !session
            .command_candidates("")
            .iter()
            .any(|candidate| candidate.name == "custom.save")
    );
    session.input(":");
    for key in "save-alias".chars() {
        session.input(&key.to_string());
    }
    session.input("Tab");
    assert_eq!(session.prompt.as_ref().unwrap().text, "save-custom");
}
