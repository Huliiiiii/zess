use super::*;

fn select(session: &mut Session, id: &str) {
    let index = session.views[&session.focus]
        .item_ids()
        .iter()
        .position(|item| item == id)
        .unwrap();
    for character in (index + 1).to_string().chars() {
        session.input(&character.to_string());
    }
    session.input("g");
    session.input("g");
    settle(session);
}

fn yank(session: &mut Session, key: &str) -> Vec<String> {
    session.input("y");
    session.input(key);
    settle(session);
    let mut clipboard = Vec::new();
    session.flush_clipboard_writes(|text| {
        clipboard.push(text);
        Ok(())
    });
    clipboard
}

#[test]
fn file_actions_follow_the_selected_worktree_or_commit() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    for historical in [false, true] {
        if historical {
            session.command("git.page-history").unwrap();
            settle(&mut session);
            session.input("y");
            assert!(session.pending.is_empty());
            session.input("Enter");
            settle(&mut session);
        }
        session.input("y");
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
        session.input("Esc");
        for (key, expected) in [
            ("p", "note.txt".to_owned()),
            ("a", fixture.0.join("note.txt").display().to_string()),
            ("n", "note.txt".to_owned()),
            (
                "f",
                if historical {
                    "historical\n"
                } else {
                    "working\n"
                }
                .to_owned(),
            ),
        ] {
            assert_eq!(yank(&mut session, key), [expected]);
        }
        let expected = if historical {
            git(
                &fixture,
                &[
                    "show",
                    "--format=",
                    "--no-ext-diff",
                    "--no-textconv",
                    "HEAD",
                    "--",
                    "note.txt",
                ],
            )
        } else {
            git(
                &fixture,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "HEAD",
                    "--",
                    "note.txt",
                ],
            )
        };
        assert_eq!(yank(&mut session, "d"), [format!("{expected}\n")]);
    }
}

#[test]
fn file_actions_honor_the_configured_register_and_explicit_override() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    for historical in [false, true] {
        if historical {
            session.command("git.page-history").unwrap();
            settle(&mut session);
            session.input("Enter");
            settle(&mut session);
        }
        session
            .request(
                session.focus,
                HostRequest::ConfigureView {
                    options: json!({"default-yank-register":"*"}),
                },
            )
            .unwrap();
        assert!(yank(&mut session, "p").is_empty());
        let mut primary = Vec::new();
        session.flush_primary_clipboard_writes(|text| {
            primary.push(text);
            Ok(())
        });
        assert_eq!(primary, ["note.txt"]);
        session.input("\"");
        session.input("+");
        assert_eq!(yank(&mut session, "p"), ["note.txt"]);
    }
}

#[test]
fn directories_and_deleted_files_keep_their_paths_without_fabricating_contents() {
    let fixture = nested_changes();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    select(&mut session, "directory:dir");
    assert_eq!(yank(&mut session, "p"), ["dir"]);
    assert!(yank(&mut session, "f").is_empty());
    assert_eq!(
        session.message,
        "error: Select files to yank contents or diffs"
    );
    assert!(yank(&mut session, "d").is_empty());
    assert_eq!(
        session.message,
        "error: Select files to yank contents or diffs"
    );
    select(&mut session, "file:dir/nested/deleted.txt");
    assert_eq!(yank(&mut session, "p"), ["dir/nested/deleted.txt"]);
    assert!(yank(&mut session, "f").is_empty());
    assert!(session.message.contains("read "), "{}", session.message);
}

#[test]
fn historical_yanks_do_not_read_a_worktree_replacement_for_a_deleted_file() {
    let (fixture, _) = repository();
    git(&fixture, &["rm", "--quiet", "--force", "note.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "delete"]);
    fs::write(fixture.0.join("note.txt"), "replacement\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    session.command("git.page-history").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    assert_eq!(yank(&mut session, "p"), ["note.txt"]);
    assert!(yank(&mut session, "f").is_empty());
    assert_eq!(session.message, "This file does not exist in this commit");
    let diff = yank(&mut session, "d");
    assert!(diff[0].contains("-historical\n"));
    assert!(!diff[0].contains("replacement"));
}

#[test]
fn historical_renames_keep_their_destination_path_and_commit_contents() {
    let (fixture, path) = repository();
    fs::write(path, "historical\n").unwrap();
    git(&fixture, &["mv", "note.txt", "renamed.txt"]);
    git(&fixture, &["commit", "--quiet", "-m", "rename"]);
    fs::write(fixture.0.join("renamed.txt"), "replacement\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    session.command("git.page-history").unwrap();
    settle(&mut session);
    session.input("Enter");
    settle(&mut session);
    assert_eq!(yank(&mut session, "p"), ["renamed.txt"]);
    assert_eq!(yank(&mut session, "n"), ["renamed.txt"]);
    assert_eq!(yank(&mut session, "f"), ["historical\n"]);
    let diff = yank(&mut session, "d");
    assert!(diff[0].contains("rename from note.txt\n"));
    assert!(diff[0].contains("rename to renamed.txt\n"));
    assert!(!diff[0].contains("replacement"));
}
