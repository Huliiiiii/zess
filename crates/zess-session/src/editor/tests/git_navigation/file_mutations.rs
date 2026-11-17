use super::*;

#[test]
fn file_operations_stage_and_unstage_without_replacing_the_working_copy() {
    let (fixture, path) = repository();
    fs::write(&path, "index\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "worktree\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(
        session.begin_operation(
            "unstage-file",
            &zess_protocol::CommandAction::Command {
                name: "git.unstage".into(),
                argument: json!({}),
            }
        ),
        None
    );
    settle(&mut session);
    assert_eq!(
        session.take_operation_completions(),
        vec![(
            "unstage-file".into(),
            zess_protocol::CommandOutcome::Applied
        )]
    );
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    assert_eq!(fs::read_to_string(&path).unwrap(), "worktree\n");
    assert_eq!(session.focus, source);
    session.execute_command("git.stage", json!({})).unwrap();
    settle(&mut session);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "worktree");
    assert_eq!(fs::read_to_string(&path).unwrap(), "worktree\n");
    assert_eq!(session.focus, source);
}

#[test]
fn the_selected_file_can_be_staged_when_line_counts_fail() {
    let (fixture, _) = repository();
    git(&fixture, &["update-index", "--split-index"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);

    let source = session.focus;
    let status = &session.views[&source].location.display.status;
    assert_eq!(status["git"]["state"], "failed");
    let error = status["error"].as_str().unwrap();
    assert!(
        error.contains("count changed lines in working tree"),
        "{error}"
    );
    assert!(
        error.contains("unsupported mandatory extension: 'link'"),
        "{error}"
    );
    assert_eq!(selected_row(&session, source).id, "file:note.txt");

    session.execute_command("git.stage", json!({})).unwrap();
    settle(&mut session);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "working");
}

#[test]
fn ignore_keys_apply_to_the_selected_directory_and_refresh_the_tree() {
    for (key, destination, message) in [
        ("g", ".gitignore", "Ignore rules added to .gitignore"),
        (
            "l",
            ".git/info/exclude",
            "Ignore rules added to repository-local info/exclude",
        ),
    ] {
        let (fixture, _) = repository();
        fs::create_dir(fixture.0.join("build cache")).unwrap();
        fs::write(fixture.0.join("build cache/result"), "output\n").unwrap();
        fs::write(fixture.0.join("keep.txt"), "keep\n").unwrap();
        let before = fs::read(fixture.0.join(destination)).unwrap_or_default();
        let mut session = fixture.session(&[]);
        session.command("git").unwrap();
        settle(&mut session);
        session.input("g");
        session.input("g");
        session.input("i");
        assert_eq!(session.key_sequence.keys(), ["i"]);
        session.input(key);
        settle(&mut session);
        assert_eq!(session.message, message);
        let mut expected = before;
        expected.extend_from_slice(b"/build\\ cache/\n");
        assert_eq!(fs::read(fixture.0.join(destination)).unwrap(), expected);
        assert_eq!(
            git(&fixture, &["check-ignore", "build cache/result"]),
            "build cache/result"
        );
        assert!(
            session.views[&session.focus]
                .item_ids()
                .iter()
                .all(|id| id != "directory:build cache")
        );
        assert!(
            session.views[&session.focus]
                .item_ids()
                .iter()
                .any(|id| id == "file:keep.txt")
        );
        assert_eq!(git(&fixture, &["diff", "--cached", "--name-only"]), "");
        if key == "l" {
            assert!(!fixture.0.join(".gitignore").exists());
        }
    }
}

#[test]
fn discard_confirmation_can_be_cancelled_before_restoring_the_staged_copy() {
    let (fixture, path) = repository();
    fs::write(&path, "index\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "worktree\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    session.execute_command("git.discard", json!({})).unwrap();
    assert_eq!(
        prompt_text(&mut session),
        "Restore working tree from index: \"note.txt\". Type yes to confirm: "
    );
    session.input("Esc");
    assert_eq!(fs::read_to_string(&path).unwrap(), "worktree\n");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "index");
    session.execute_command("git.discard", json!({})).unwrap();
    for key in ["y", "e", "s", "Enter"] {
        session.input(key);
    }
    settle(&mut session);
    assert_eq!(fs::read_to_string(&path).unwrap(), "index\n");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "index");
}

#[test]
fn history_has_no_file_mutation_bindings() {
    let (fixture, path) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    for key in ["C-w", "2"] {
        session.input(key);
    }
    settle(&mut session);
    for key in ["s", "u", "d", "i", "g", "i", "l"] {
        session.input(key);
    }
    assert!(session.prompt.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
}
