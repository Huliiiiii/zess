use super::*;

fn contents(session: &Session, buffer: BufferId) -> String {
    match &session.buffers[&buffer] {
        Buffer::Text(text) => text.text(),
        _ => panic!("expected a file buffer"),
    }
}

#[test]
fn external_changes_refresh_clean_files_without_changing_the_view() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "first\nunchanged\nlast\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    let view = session.focus;
    let buffer = session.views[&view].location.buffer;
    session.input("j");
    fs::write(&path, "new\nfirst\nunchanged\nlast\n").unwrap();

    wait_until(&mut session, |session| {
        contents(session, buffer) == "new\nfirst\nunchanged\nlast\n"
    });

    assert_eq!(session.focus, view);
    assert_eq!(session.views[&view].location.buffer, buffer);
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        unreachable!()
    };
    assert!(!text.modified());
    assert_eq!(text.view(view).cursor(), (2, 0));
    session.command("w").unwrap();
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "new\nfirst\nunchanged\nlast\n"
    );
}

#[test]
fn local_edits_delay_external_changes_until_undo_restores_the_saved_file() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "original\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    let buffer = session.views[&session.focus].location.buffer;
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    let sentinel = fixture.0.join("sentinel.txt");
    fs::write(&sentinel, "before\n").unwrap();
    let sentinel_buffer = session.load_file(&sentinel).unwrap();
    fs::write(&path, "external\n").unwrap();
    fs::write(&sentinel, "after\n").unwrap();

    wait_until(&mut session, |session| {
        contents(session, sentinel_buffer) == "after\n"
    });
    assert_eq!(contents(&session, buffer), "!original\n");
    session.input("u");
    wait_until(&mut session, |session| {
        contents(session, buffer) == "external\n"
    });
    assert!(!session.modified());
}

#[test]
fn deleting_a_file_keeps_its_contents_and_recreation_refreshes_it() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "original\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    let buffer = session.views[&session.focus].location.buffer;
    fs::remove_file(&path).unwrap();
    let missing_file = fs::read_to_string(&path).unwrap_err();
    let expected_error = format!("file reload: read {}: {missing_file}", path.display());
    wait_until(&mut session, |session| session.message == expected_error);
    assert_eq!(contents(&session, buffer), "original\n");

    fs::write(&path, "recreated\n").unwrap();
    wait_until(&mut session, |session| {
        contents(session, buffer) == "recreated\n"
    });
}

#[test]
fn separate_file_layers_keep_local_edits_when_the_same_file_changes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "original\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    let parent = session.focus;
    let original = session.views[&parent].location.buffer;
    session.open_file_layer(parent, "note.txt", 0).unwrap();
    let layer = session.focus;
    let copy = session.views[&layer].location.buffer;
    assert_ne!(original, copy);
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    fs::write(&path, "external\n").unwrap();

    wait_until(&mut session, |session| {
        contents(session, original) == "external\n"
    });
    assert_eq!(contents(&session, copy), "!original\n");
    assert_eq!(session.focus, layer);
    session.input("u");
    wait_until(&mut session, |session| {
        contents(session, copy) == "external\n"
    });
}

#[test]
fn write_quit_retains_edits_when_the_file_has_changed_on_disk() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "original\n");
    let mut session = fixture.session(&[path.clone()]);
    let view = session.focus;
    let buffer = session.views[&view].location.buffer;
    session.execute_command("text.insert", Value::Null).unwrap();
    session.input("!");
    session.execute_command("text.escape", Value::Null).unwrap();

    let modified_at = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, "external\n").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified_at)
        .unwrap();

    let error = session.command("wq").unwrap_err();
    assert!(format!("{error:#}").contains("file changed on disk"));
    assert_eq!(fs::read_to_string(path).unwrap(), "external\n");
    assert_eq!(session.focus, view);
    assert_eq!(session.views[&view].location.buffer, buffer);
    assert_eq!(contents(&session, buffer), "!original\n");
    assert!(session.modified());
}

#[test]
fn saving_a_stale_file_layer_preserves_the_other_buffers_write() {
    let fixture = Fixture::new();
    let path = fixture.write("note.txt", "original\n");
    let mut session = fixture.session(&[path.clone()]);
    let parent = session.focus;
    let buffer_id = session.views[&parent].location.buffer;
    session.execute_command("text.insert", Value::Null).unwrap();
    session.input("a");
    session.execute_command("text.escape", Value::Null).unwrap();
    session.open_file_layer(parent, "note.txt", 0).unwrap();
    let layer_buffer = session.views[&session.focus].location.buffer;
    assert_ne!(buffer_id, layer_buffer);
    session.execute_command("text.insert", Value::Null).unwrap();
    session.input("b");
    session.execute_command("text.escape", Value::Null).unwrap();

    let Buffer::Text(text) = &session.buffers[&buffer_id] else {
        unreachable!()
    };
    let version = text.document_version();
    assert!(matches!(
        session.begin_operation(
            "save-original",
            &zess_protocol::CommandAction::Save { buffer_id, version },
        ),
        Some(zess_protocol::CommandOutcome::Saved { buffer_id: saved_buffer, version: saved })
            if saved_buffer == buffer_id && saved > version
    ));

    let error = session.command("w").unwrap_err();
    assert!(format!("{error:#}").contains("file changed on disk"));
    assert_eq!(fs::read_to_string(path).unwrap(), "aoriginal\n");
    assert_eq!(contents(&session, layer_buffer), "boriginal\n");
}

#[test]
fn writing_a_new_file_preserves_contents_created_after_opening() {
    let fixture = Fixture::new();
    let path = fixture.0.join("new.txt");
    let mut session = fixture.session(&[path.clone()]);
    session.execute_command("text.insert", Value::Null).unwrap();
    session.input("!");
    session.execute_command("text.escape", Value::Null).unwrap();
    fs::write(&path, "external\n").unwrap();

    let error = session.command("w").unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "external\n");
    assert!(session.modified());
}

#[test]
fn hidden_files_continue_observing_external_changes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("hidden.txt");
    fs::write(&path, "original\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    let buffer = session.views[&session.focus].location.buffer;
    session.command("new").unwrap();
    assert!(
        session
            .views
            .values()
            .all(|view| view.location.buffer != buffer)
    );
    fs::write(&path, "external\n").unwrap();

    wait_until(&mut session, |session| {
        contents(session, buffer) == "external\n"
    });
    session.command(&format!("buffer {buffer}")).unwrap();
    let view = session.focus;
    assert_eq!(session.views[&view].location.buffer, buffer);
    let snapshot = view_snapshot(&mut session, view);
    let line = &text_lines(&snapshot.content)[0];
    assert_eq!(line.line, 0);
    assert_eq!(line.text, "external\n");
}

#[test]
fn saving_under_a_new_name_stops_observing_the_old_file() {
    let fixture = Fixture::new();
    let path = fixture.0.join("old.txt");
    fs::write(&path, "original\n").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    let buffer = session.views[&session.focus].location.buffer;
    fs::write(&path, "old change\n").unwrap();
    session.command("w new.txt").unwrap();
    fs::write(&path, "obsolete\n").unwrap();
    fs::write(fixture.0.join("new.txt"), "new change\n").unwrap();

    wait_until(&mut session, |session| {
        contents(session, buffer) == "new change\n"
    });
    assert_eq!(fs::read_to_string(path).unwrap(), "obsolete\n");
}

#[test]
fn files_outside_the_workspace_are_observed() {
    let workspace = Fixture::new();
    let external = Fixture::new();
    let path = external.0.join("note.txt");
    fs::write(&path, "original\n").unwrap();
    let mut session = workspace.session(&[path.clone()]);
    let buffer = session.views[&session.focus].location.buffer;
    fs::write(&path, "external\n").unwrap();
    wait_until(&mut session, |session| {
        contents(session, buffer) == "external\n"
    });
}
