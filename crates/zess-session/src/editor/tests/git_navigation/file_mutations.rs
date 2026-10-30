use super::*;

#[test]
fn file_keys_stage_and_unstage_without_replacing_the_working_copy() {
    let (fixture, path) = repository();
    fs::write(&path, "index\n").unwrap();
    git(&fixture, &["add", "note.txt"]);
    fs::write(&path, "worktree\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("u");
    settle(&mut session);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
    assert_eq!(fs::read_to_string(&path).unwrap(), "worktree\n");
    assert_eq!(session.focus, source);
    session.input("s");
    settle(&mut session);
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "worktree");
    assert_eq!(fs::read_to_string(&path).unwrap(), "worktree\n");
    assert_eq!(session.focus, source);
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
    session.input("d");
    assert_eq!(
        session.frame().prompt,
        "Restore working tree from index: \"note.txt\". Type yes to confirm: "
    );
    session.input("Esc");
    assert_eq!(fs::read_to_string(&path).unwrap(), "worktree\n");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "index");
    session.input("d");
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
    for key in ["s", "u", "d"] {
        session.input(key);
    }
    assert!(session.prompt.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), "working\n");
    assert_eq!(git(&fixture, &["show", ":note.txt"]), "historical");
}
