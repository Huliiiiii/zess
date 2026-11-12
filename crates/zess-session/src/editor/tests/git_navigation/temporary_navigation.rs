use super::*;

#[test]
fn temporary_file_previews_reject_buffer_switches_and_window_splits() {
    for browser in ["files", "git"] {
        let (fixture, path) = repository();
        let mut session = fixture.session(&[path]);
        let editor = session.views[&session.focus].location.buffer;
        session.command("git").unwrap();
        settle(&mut session);
        let root = session.focus;
        session.input("e");
        let layer = session.focus;
        let temporary = session.views[&layer].location.buffer;
        session.command(browser).unwrap();
        settle(&mut session);
        session.focus_preview(session.focus).unwrap();
        let preview = session.focus;
        if browser == "git" {
            assert_ne!(preview, layer);
        } else {
            assert_eq!(preview, layer);
        }
        let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
        let views = session
            .views
            .iter()
            .map(|(id, _)| id)
            .copied()
            .collect::<HashSet<_>>();
        for command in [
            "bn",
            "bp",
            "new",
            "edit note.txt",
            &format!("buffer {editor}"),
            "vsplit",
            "hsplit",
        ] {
            assert!(session.command(command).is_err(), "{browser}: {command}");
            assert_eq!(session.focus, preview);
            assert_eq!(session.views.parent(layer), Some(root));
            assert_eq!(session.views.child(root), Some(layer));
            assert_eq!(
                session.buffers.keys().copied().collect::<HashSet<_>>(),
                buffers
            );
            assert_eq!(
                session
                    .views
                    .iter()
                    .map(|(id, _)| id)
                    .copied()
                    .collect::<HashSet<_>>(),
                views
            );
        }
        session.command("q").unwrap();
        assert_eq!(session.focus, layer);
        assert_eq!(session.views[&layer].location.buffer, temporary);
        session.command("q").unwrap();
        assert_eq!(session.focus, root);
        assert!(!session.exiting());
    }
}

#[test]
fn pending_file_operations_block_a_temporary_file_browser_from_closing() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "working\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("e");
    let layer = session.focus;
    let temporary = session.views[&layer].location.buffer;
    session.command("files").unwrap();
    settle(&mut session);
    for key in ["a", "n", "e", "w", ".", "t", "x", "t", "Enter"] {
        session.input(key);
    }
    assert!(!fixture.0.join("new.txt").exists());
    assert!(session.command("q").is_err());
    assert_eq!(session.focus, layer);
    assert_ne!(session.views[&layer].location.buffer, temporary);
    assert!(
        session.views[&layer]
            .location
            .display
            .rows
            .iter()
            .any(|row| row.id == "new.txt" && row.text == "  + new.txt [new, unsaved]")
    );
    session.command("w").unwrap();
    wait_until(&mut session, |_| fixture.0.join("new.txt").exists());
    settle(&mut session);
    session.command("q").unwrap();
    assert_eq!(session.views[&layer].location.buffer, temporary);
    assert!(!session.exiting());
}

#[test]
fn closing_a_browser_returns_to_the_unsaved_temporary_file() {
    for command in ["files", "git"] {
        let (fixture, path) = repository();
        let mut session = fixture.session(&[]);
        session.resize(160, 24);
        session.command("files").unwrap();
        settle(&mut session);
        let root = session.focus;
        let layout = panes(&session);
        session.input("j");
        settle(&mut session);
        session.input("e");
        let layer = session.focus;
        let temporary = session.views[&layer].location.buffer;
        for key in ["i", "!", "Esc"] {
            session.input(key);
        }
        let selection = serde_json::to_value(
            session
                .buffers
                .get_mut(&temporary)
                .unwrap()
                .text()
                .unwrap()
                .selections(layer),
        )
        .unwrap();
        let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();

        session.command(command).unwrap();
        settle(&mut session);
        assert_eq!(session.focus, layer, "{}", session.message);
        assert_eq!(session.views.parent(layer), Some(root));
        assert_eq!(session.views.child(root), Some(layer));
        let (id, text) = if command == "files" {
            ("note.txt", "  note.txt")
        } else {
            ("file:note.txt", "note.txt")
        };
        assert_eq!(
            session.views[&layer]
                .location
                .display
                .rows
                .iter()
                .find(|row| row.id == id)
                .unwrap()
                .text,
            text
        );
        assert!(
            session.views.page(layer).previews.as_ref().is_some(),
            "{}",
            session.message
        );
        let browser = session.views[&layer].location.buffer;
        session.command("buffers").unwrap();
        let candidates = session
            .message
            .split_whitespace()
            .map(|id| id.parse::<u64>().unwrap())
            .collect::<Vec<_>>();
        assert!(!candidates.contains(&temporary));
        assert!(!candidates.contains(&browser));
        assert_eq!(panes(&session), layout);

        session.focus_preview(session.focus).unwrap();
        session.command("q").unwrap();
        assert!(!session.exiting());
        assert_eq!(session.focus, layer);
        assert_eq!(session.views[&layer].location.buffer, temporary);
        let text = session.buffers.get_mut(&temporary).unwrap().text().unwrap();
        assert_eq!(text.text(), "!working\n");
        assert_eq!(
            serde_json::to_value(text.selections(layer)).unwrap(),
            selection
        );
        assert!(text.modified());
        assert_eq!(
            session.buffers.keys().copied().collect::<HashSet<_>>(),
            buffers
        );
        assert!(session.command("q").is_err());
        session.command("w").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "!working\n");
        session.command("q").unwrap();
        assert!(!session.exiting());
        assert_eq!(session.focus, root);
        assert!(!session.buffers.contains_key(&temporary));
        assert_eq!(panes(&session), layout);
        session.command("q").unwrap();
        assert!(!session.exiting());
        session.command("q").unwrap();
        assert!(!session.exiting());
    }
}

#[test]
fn git_page_and_commit_navigation_preserve_the_temporary_file_return_point() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.resize(160, 24);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("j");
    settle(&mut session);
    session.input("e");
    let layer = session.focus;
    let temporary = session.views[&layer].location.buffer;
    let buffers = session.buffers.keys().copied().collect::<HashSet<_>>();
    session.command("git").unwrap();
    settle(&mut session);
    session.command("git.page-history").unwrap();
    settle(&mut session);
    assert_eq!(
        selected_row(&session, layer).id,
        git(&fixture, &["rev-parse", "HEAD"])
    );
    let history = session.views[&layer].location.buffer;
    session.input("Enter");
    settle(&mut session);
    assert_eq!(selected_row(&session, layer).id, "file:note.txt");
    session.input("Esc");
    settle(&mut session);
    assert_eq!(session.views[&layer].location.buffer, history);
    session.input("Enter");
    settle(&mut session);
    let commit = session.views[&layer].location.buffer;
    session.command("git-commit-files.page-changes").unwrap();
    settle(&mut session);
    session.command("git.page-history").unwrap();
    settle(&mut session);
    assert_eq!(session.views[&layer].location.buffer, commit);
    assert_eq!(selected_row(&session, layer).id, "file:note.txt");

    session.focus_preview(session.focus).unwrap();
    session.command("q").unwrap();
    assert_eq!(session.views[&session.focus].location.buffer, temporary);
    assert_eq!(session.focus, layer);
    assert!(!session.exiting());
    assert_eq!(
        session.buffers.keys().copied().collect::<HashSet<_>>(),
        buffers
    );
    let text = session.buffers.get_mut(&temporary).unwrap().text().unwrap();
    assert_eq!(text.path(), Some(fixture.0.join("note.txt").as_path()));
    assert_eq!(text.text(), "working\n");
}

#[test]
fn reopening_a_browser_from_its_preview_keeps_the_temporary_file_return_point() {
    let (fixture, _) = repository();
    let mut session = fixture.session(&[]);
    session.resize(160, 24);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("j");
    settle(&mut session);
    session.input("e");
    let layer = session.focus;
    let temporary = session.views[&layer].location.buffer;
    session.command("git").unwrap();
    settle(&mut session);
    session.focus_preview(session.focus).unwrap();
    let preview = session.focus;
    assert_ne!(preview, layer, "{}", session.message);
    assert!(
        session
            .visible_rectangles()
            .iter()
            .any(|(id, _)| *id == RenderTarget::View(preview))
    );
    let Buffer::PreviewDiff { path, diff } =
        &session.buffers[&session.views[&preview].location.buffer]
    else {
        panic!("expected temporary file diff preview")
    };
    assert_eq!(path, "note.txt");
    assert_eq!(
        diff.raw,
        format!(
            "{}\n",
            git(
                &fixture,
                &["diff", "--no-ext-diff", "--no-textconv", "--", "note.txt"]
            )
        )
    );
    session.command("files").unwrap();
    settle(&mut session);
    assert_eq!(session.focus, layer);
    assert!(!session.views.contains_key(&preview));
    session.command("q").unwrap();
    assert_eq!(session.views[&layer].location.buffer, temporary);
    assert!(!session.exiting());
}
