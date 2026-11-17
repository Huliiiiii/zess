use super::*;

fn file_list() -> (Fixture, Session) {
    let fixture = Fixture::new();
    for index in 0..40 {
        fs::write(fixture.0.join(format!("file-{index:02}.txt")), "").unwrap();
    }
    let mut session = fixture.session(&[]);
    session.resize(80, 22);
    session.command("files").unwrap();
    settle(&mut session);
    session.snapshot();
    (fixture, session)
}

#[test]
fn half_page_navigation_moves_the_viewport_and_leaves_room_below_the_last_item() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    assert_eq!(session.views[&source].viewport_height, 20);
    session.input("C-d");
    session.snapshot();
    assert_eq!(selected_row(&session, source).text.trim(), "file-10.txt");
    assert_eq!(session.views[&source].location.scroll, 5);
    session.input("C-u");
    session.snapshot();
    assert_eq!(selected_row(&session, source).text.trim(), "file-00.txt");
    assert_eq!(session.views[&source].location.scroll, 0);
    for _ in 0..4 {
        session.input("C-d");
        session.snapshot();
    }
    assert_eq!(selected_row(&session, source).text.trim(), "file-39.txt");
    assert_eq!(session.views[&source].location.scroll, 34);
    let view = view_snapshot(&mut session, source);
    let tail = &item_rows(&view.content)[view.scroll..];
    assert_eq!(tail.first().unwrap().text.trim(), "file-34.txt");
    assert_eq!(tail.last().unwrap().text.trim(), "file-39.txt");
    assert_eq!(tail.len(), 6);
}

#[test]
fn counted_half_pages_extend_selection_and_scroll_the_viewport() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    for key in ["v", "2", "C-d"] {
        session.input(key);
    }
    session.snapshot();
    assert_eq!(selected_row(&session, source).text.trim(), "file-20.txt");
    assert_eq!(session.views[&source].location.selection.selected.len(), 21);
    assert_eq!(session.views[&source].location.scroll, 15);
}

#[test]
fn line_navigation_keeps_room_above_and_below_the_selection() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    for _ in 0..15 {
        session.input("j");
        session.snapshot();
    }
    assert_eq!(selected_row(&session, source).text.trim(), "file-15.txt");
    assert_eq!(session.views[&source].location.scroll, 1);
    for _ in 0..10 {
        session.input("k");
        session.snapshot();
    }
    assert_eq!(selected_row(&session, source).text.trim(), "file-05.txt");
    assert_eq!(session.views[&source].location.scroll, 0);
}
