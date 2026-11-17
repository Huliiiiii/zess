use super::*;

#[test]
fn file_yank_bindings_are_exported_in_both_modes() {
    let fixture = Fixture::new();
    let session = fixture.session(&[]);
    let catalog = session.binding_catalog();
    let keymap = catalog.keymap();
    for mode in ["normal", "select"] {
        let bindings = keymap.effective_bindings(&["global", "data", "files"], mode);
        let entries =
            zess_plugin::command_menu::binding_entries(&catalog.commands, bindings, &["y".into()]);
        for description in [
            "Yank relative paths",
            "Yank absolute paths",
            "Yank file names",
            "Yank file contents",
            "Yank file diffs",
        ] {
            assert!(
                entries.iter().any(|(_, label)| label == description),
                "{mode}: {description}"
            );
        }
    }
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
fn plugin_keys_are_grouped_before_shared_and_global_keys() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "note").unwrap();
    let mut session = fixture.session(&[]);
    session.resize(120, 100);
    session.command("files").unwrap();
    session.input("?");
    let id = session.focus;
    let view = view_snapshot(&mut session, id);
    let rows = item_rows(&view.content);
    let headings = rows
        .iter()
        .filter(|row| row.id.is_empty())
        .map(|row| (row.text.as_str(), row.style.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        headings,
        [
            ("Files", "status"),
            ("Items", "status"),
            ("Global", "status")
        ]
    );
    insta::assert_json_snapshot!(
        "default_key_groups",
        rows.iter()
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
    let id = session.focus;
    let view = view_snapshot(&mut session, id);
    let rows = item_rows(&view.content);
    assert_eq!(rows[0].text, "Project files");
    assert_eq!(
        rows.iter().find(|row| row.id == "z").unwrap().text,
        "z                    Rename selection"
    );
    assert!(!rows.iter().any(|row| row.id == "r"));
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
    let view = view_snapshot(&mut session, pinned);
    let rows = item_rows(&view.content);
    assert_eq!(rows[0].text, "Workspace files");
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
    let id = session.focus;
    let view = view_snapshot(&mut session, id);
    let rows = item_rows(&view.content);
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
fn the_best_fuzzy_command_match_is_submitted_without_completion() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.input(":");
    for key in "hlp".chars() {
        session.input(&key.to_string());
    }
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
    let config = fixture.write(
        "config.w",
        r#"register-command-alias "语言" "set-language"
register-command-alias "dialect" "set-language"
"#,
    );
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    assert!(session.generation > 0, "{}", session.message);
    session.input(":");
    session.input("语");
    assert_eq!(session.command_candidates("语").len(), 1);
    session.input("Tab");
    assert_eq!(session.prompt.as_ref().unwrap().text, "set-language");
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

#[test]
fn opening_a_draft_from_help_keeps_it_attached_to_the_original_source() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session.command("help").unwrap();
    let help = session.focus;
    session
        .request(
            help,
            HostRequest::Layer {
                purpose: "draft".into(),
                text: "draft text".into(),
                inline: true,
            },
        )
        .unwrap();
    let draft = session.focus;
    assert!(session.help.is_none());
    assert!(!session.views.contains_key(&help));
    assert_eq!(session.views.parent(draft), Some(source));
    assert_eq!(session.views.child(source), Some(draft));
    assert_eq!(session.focus, draft);
    session.command("write").unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(session.views.child(source), None);
    assert_eq!(
        session.views[&source].location.memories["draft:draft"]["text"],
        "draft text"
    );
}
