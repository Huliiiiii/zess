use super::*;
use zess_filter::QueryKind::{self, Glob, Plain, Regex};

fn query(session: &mut Session, kind: QueryKind, value: &str) {
    let previous = session.prompt.as_ref().unwrap().query().unwrap().0;
    let prompt = session.prompt.as_mut().unwrap();
    prompt.text = value.into();
    prompt.cursor = value.len();
    prompt.error = None;
    if previous == kind {
        session.update_prompt().unwrap();
    } else {
        session.input(match kind {
            Glob => "A-g",
            Regex => "A-r",
            Plain => match previous {
                Glob => "A-g",
                Regex => "A-r",
                Plain => unreachable!(),
            },
        });
    }
}

#[test]
fn file_filters_support_globs_and_regexes_without_changing_literal_case() {
    let fixture = Fixture::new();
    for name in ["Alpha.rs", "beta.rs", "école.txt"] {
        fs::write(fixture.0.join(name), name).unwrap();
    }
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("/");
    for (kind, value, expected, prompt) in [
        (Plain, "alpha", vec![], "filter:alpha"),
        (Plain, "Alpha", vec!["Alpha.rs"], "filter:Alpha"),
        (
            Glob,
            "*.rs",
            vec!["Alpha.rs", "beta.rs"],
            "filter(glob):*.rs",
        ),
        (
            Regex,
            "^é.*\\.txt$",
            vec!["école.txt"],
            "filter(re):^é.*\\.txt$",
        ),
    ] {
        query(&mut session, kind, value);
        assert!(
            session.prompt.as_ref().unwrap().error.is_none(),
            "{}",
            session.message
        );
        assert_eq!(session.views[&session.focus].item_ids(), expected);
        assert_eq!(session.frame().prompt, prompt);
        assert_eq!(session.views[&session.focus].location.display.header, "");
    }
}

#[test]
fn invalid_filters_preserve_the_last_results_and_view_position_until_corrected_or_closed() {
    let fixture = Fixture::new();
    for index in 0..30 {
        fs::write(fixture.0.join(format!("file-{index:02}.txt")), "text").unwrap();
    }
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.0)
        .status()
        .unwrap();
    assert!(status.success());
    for kind in ["files", "git", "help"] {
        let mut session = fixture.session(&[]);
        session.command("files").unwrap();
        settle(&mut session);
        if kind == "git" {
            session.command("git").unwrap();
            settle(&mut session);
        } else if kind == "help" {
            session.input("?");
            session.input("p");
        }
        session.input("/");
        query(&mut session, Regex, ".");
        session
            .views
            .get_mut(&session.focus)
            .unwrap()
            .location
            .scroll = 3;
        let location = &mut session.views.get_mut(&session.focus).unwrap().location;
        location.scroll_mode = ScrollMode::UserControlled;
        let id = location
            .display
            .rows
            .iter()
            .filter(|row| !row.id.is_empty())
            .nth(4)
            .unwrap()
            .id
            .clone();
        location.selection = ItemSelection {
            primary: Some(id.clone()),
            selected: vec![id.clone()],
            anchor: Some(id),
        };
        let rows = serde_json::to_value(&location.display.rows).unwrap();
        let selection = serde_json::to_value(&location.selection).unwrap();
        for invalid_kind in [Regex, Glob] {
            query(&mut session, invalid_kind, "[");
            assert!(session.prompt.as_ref().unwrap().error.is_some(), "{kind}");
            session.tick();
            let location = &session.views[&session.focus].location;
            assert_eq!(
                serde_json::to_value(&location.display.rows).unwrap(),
                rows,
                "{kind}"
            );
            assert_eq!(
                serde_json::to_value(&location.selection).unwrap(),
                selection,
                "{kind}"
            );
            assert_eq!(location.scroll, 3, "{kind}");
            assert!(location.filter.query().kind == Regex);
            assert_eq!(location.filter.query().text, ".");
            assert!(session.prompt.as_ref().unwrap().error.is_some(), "{kind}");
            let error_row = session
                .frame()
                .rows
                .into_iter()
                .find(|row| row.spans.iter().any(|span| span.style == "error"))
                .unwrap();
            assert!(error_row.spans.iter().all(|span| !span.text.contains('\n')));
            assert!(
                error_row
                    .spans
                    .iter()
                    .any(|span| span.text.contains("unclosed")),
                "{kind}"
            );
        }
        query(&mut session, Regex, ".*");
        assert!(session.prompt.as_ref().unwrap().error.is_none());
        for close in ["Esc", "Enter"] {
            query(&mut session, Regex, "[");
            session.input(close);
            assert!(session.prompt.is_none());
            assert!(session.views[&session.focus].location.mode == ViewMode::Normal);
            let query = session.views[&session.focus].location.filter.query();
            assert!(query.kind == Regex);
            assert_eq!(query.text, ".*");
            session.input("/");
            assert!(matches!(
                session.prompt.as_ref().unwrap().query(),
                Some((Regex, ".*"))
            ));
        }
    }
}

#[test]
fn help_filters_keep_case_insensitive_matching_across_refresh_and_split() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    session.input("?");
    session.input("p");
    session.input("/");
    for (kind, value) in [(Plain, "RENAME"), (Glob, "*RENAME*"), (Regex, "RENAME")] {
        query(&mut session, kind, value);
        assert!(session.prompt.as_ref().unwrap().error.is_none());
        let rows = &session.views[&session.focus].location.display.rows;
        assert_eq!(rows.iter().filter(|row| !row.id.is_empty()).count(), 1);
        assert!(rows.iter().any(|row| row.text.contains("Rename")));
    }
    session.input("Enter");
    session.command("vsplit").unwrap();
    let rows = &session.views[&session.focus].location.display.rows;
    assert_eq!(rows.iter().filter(|row| !row.id.is_empty()).count(), 1);
    session.command("config-reload").unwrap();
    let rows = &session.views[&session.focus].location.display.rows;
    assert_eq!(rows.iter().filter(|row| !row.id.is_empty()).count(), 1);
}

#[test]
fn outline_filters_match_symbol_names_and_preserve_valid_state_after_invalid_input() {
    let fixture = Fixture::new();
    let path = fixture.0.join("example.rs");
    fs::write(&path, "fn Alpha() {}\nfn beta() {}\nfn école() {}\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.command("outline").unwrap();
    session.input("/");
    for (kind, value, expected) in [
        (Plain, "alpha", ""),
        (Glob, "A*", "Alpha"),
        (Regex, "^é", "école"),
    ] {
        query(&mut session, kind, value);
        assert!(
            session.prompt.as_ref().unwrap().error.is_none(),
            "{}",
            session.message
        );
        let rows = &session.views[&session.focus].location.display.rows;
        assert_eq!(rows.len(), usize::from(!expected.is_empty()));
        if !expected.is_empty() {
            assert!(rows[0].text.ends_with(expected));
        }
        assert_eq!(session.views[&session.focus].location.display.header, "");
    }
    let before =
        serde_json::to_value(&session.views[&session.focus].location.display.rows).unwrap();
    let selection =
        serde_json::to_value(&session.views[&session.focus].location.selection).unwrap();
    query(&mut session, Regex, "[");
    assert!(session.prompt.as_ref().unwrap().error.is_some());
    session.tick();
    assert_eq!(
        serde_json::to_value(&session.views[&session.focus].location.display.rows).unwrap(),
        before
    );
    assert_eq!(
        serde_json::to_value(&session.views[&session.focus].location.selection).unwrap(),
        selection
    );
    session.input("Enter");
    assert_eq!(session.views[&session.focus].location.display.header, "/^é");
    session.command("vsplit").unwrap();
    assert_eq!(
        serde_json::to_value(&session.views[&session.focus].location.display.rows).unwrap(),
        before
    );
}

#[test]
fn configuration_reload_restores_the_outline_filter_and_selection_when_returning_to_it() {
    let fixture = Fixture::new();
    let path = fixture.write(
        "example.rs",
        "fn alpha() {}\nfn bravo() {}\nfn bravura() {}\n",
    );
    let mut session = fixture.session(&[path]);
    session.command("outline").unwrap();
    session.execute_command("data.filter", json!({})).unwrap();
    query(&mut session, Plain, "brav");
    session.execute_command("prompt.submit", json!({})).unwrap();
    session
        .execute_command("data.move-down", json!({}))
        .unwrap();
    let view = session.focus;
    let before = &session.views[&view].location;
    let buffer = before.buffer;
    let selection = before.selection.clone();
    let rows = &before.display.rows;
    assert!(rows[0].text.ends_with("bravo"));
    assert!(rows[1].text.ends_with("bravura"));
    assert_eq!(selection.primary.as_ref(), Some(&rows[1].id));
    let rows = serde_json::to_value(rows).unwrap();

    session
        .navigate(
            view,
            Navigation::Push {
                kind: "files".into(),
                argument: json!({}),
            },
        )
        .unwrap();
    session.command("config-reload").unwrap();
    session.navigate(view, Navigation::Back).unwrap();

    let restored = &session.views[&view].location;
    assert_eq!(restored.buffer, buffer);
    assert_eq!(serde_json::to_value(&restored.display.rows).unwrap(), rows);
    assert_eq!(restored.selection, selection);
    assert_eq!(restored.display.header, "/brav");
}

#[test]
fn git_filters_keep_status_paths_and_commit_rows_as_their_matching_fields() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("src")).unwrap();
    fs::write(fixture.0.join("src/École.rs"), "first\n").unwrap();
    fs::write(fixture.0.join("other.txt"), "other\n").unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Zess Tests",
            "-c",
            "user.email=zess@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "-m",
            "Add École",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::write(fixture.0.join("src/École.rs"), "second\n").unwrap();
    fs::write(fixture.0.join("other.txt"), "changed\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("/");
    for (kind, value) in [
        (Plain, " M src/École"),
        (Glob, "**/École.rs"),
        (Glob, "src/*.rs"),
        (Regex, "^ M src/École\\.rs$"),
        (Regex, "^src/École\\.rs$"),
    ] {
        query(&mut session, kind, value);
        let ids = session.views[&session.focus].item_ids();
        assert!(ids.iter().any(|id| id == "file:src/École.rs"), "{value}");
        assert!(!ids.iter().any(|id| id == "file:other.txt"), "{value}");
    }
    query(&mut session, Plain, "école");
    assert!(session.views[&session.focus].item_ids().is_empty());
    session.input("Enter");
    session.command("git.page-history").unwrap();
    settle(&mut session);
    session.input("/");
    for (kind, value) in [
        (Plain, "Add École"),
        (Glob, "*Add École"),
        (Regex, "^[0-9a-f]{8} Add École$"),
    ] {
        query(&mut session, kind, value);
        assert_eq!(session.views[&session.focus].item_ids().len(), 1, "{value}");
    }
    session.input("Enter");
    session.input("Enter");
    settle(&mut session);
    session.input("/");
    for (kind, value) in [
        (Plain, "A src/École"),
        (Glob, "**/École.rs"),
        (Glob, "src/*.rs"),
        (Regex, "^A src/École\\.rs$"),
        (Regex, "^src/École\\.rs$"),
    ] {
        query(&mut session, kind, value);
        let ids = session.views[&session.focus].item_ids();
        assert!(ids.iter().any(|id| id == "file:src/École.rs"), "{value}");
        assert!(!ids.iter().any(|id| id == "file:other.txt"), "{value}");
        assert!(
            session.views[&session.focus]
                .location
                .display
                .header
                .starts_with("Commit › ")
        );
        assert!(
            !session.views[&session.focus]
                .location
                .display
                .header
                .contains(value)
        );
    }
}

#[test]
fn closing_help_restores_the_filter_draft_and_allows_input_to_continue() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("Alpha.rs"), "alpha").unwrap();
    fs::write(fixture.0.join("beta.rs"), "beta").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("/");
    query(&mut session, Plain, "Alpha");
    query(&mut session, Regex, "[");
    session.command("help").unwrap();
    session.execute_command("help.close", json!({})).unwrap();
    assert_eq!(session.focus, source);
    let prompt = session.active_prompt().unwrap();
    assert!(matches!(prompt.query(), Some((Regex, "["))));
    assert!(prompt.error.is_some());
    assert!(session.views[&source].location.mode == ViewMode::Filter);
    assert_eq!(session.views[&source].item_ids(), ["Alpha.rs"]);
    session.input("C-u");
    for key in ["b", "e", "t", "a"] {
        session.input(key);
    }
    assert_eq!(session.views[&source].item_ids(), ["beta.rs"]);
    session.input("Enter");
    assert!(session.prompt.is_none());
    assert!(session.views[&source].location.mode == ViewMode::Normal);
}

#[test]
fn splitting_after_filter_input_clones_normal_mode_and_the_last_valid_filter() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("Alpha.rs"), "alpha").unwrap();
    fs::write(fixture.0.join("beta.rs"), "beta").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("/");
    query(&mut session, Plain, "Alpha");
    session.command("vsplit").unwrap();
    assert!(session.prompt.is_none());
    assert_eq!(session.views.values().count(), 2);
    for view in session.views.values() {
        assert!(view.location.mode == ViewMode::Normal);
        assert_eq!(view.item_ids(), ["Alpha.rs"]);
    }
}
