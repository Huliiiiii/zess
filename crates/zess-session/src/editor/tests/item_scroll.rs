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
    session.frame();
    (fixture, session)
}

#[test]
fn half_page_navigation_moves_the_viewport_and_leaves_room_below_the_last_item() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    assert_eq!(session.views[&source].viewport_height, 20);
    session.input("C-d");
    session.frame();
    assert_eq!(selected_row(&session, source).text.trim(), "file-10.txt");
    assert_eq!(session.views[&source].location.scroll, 5);
    session.input("C-u");
    session.frame();
    assert_eq!(selected_row(&session, source).text.trim(), "file-00.txt");
    assert_eq!(session.views[&source].location.scroll, 0);
    for _ in 0..4 {
        session.input("C-d");
        session.frame();
    }
    assert_eq!(selected_row(&session, source).text.trim(), "file-39.txt");
    assert_eq!(session.views[&source].location.scroll, 34);
    let (rows, _) = session.render_view(source, 40, 21);
    let lines = rows
        .iter()
        .take(20)
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0].trim(), "file-34.txt");
    assert_eq!(lines[5].trim(), "file-39.txt");
    assert!(lines[6..].iter().all(|line| line.trim().is_empty()));
}

#[test]
fn counted_half_pages_extend_selection_and_scroll_the_viewport() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    for key in ["v", "2", "C-d"] {
        session.input(key);
    }
    session.frame();
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
        session.frame();
    }
    assert_eq!(selected_row(&session, source).text.trim(), "file-15.txt");
    assert_eq!(session.views[&source].location.scroll, 1);
    for _ in 0..10 {
        session.input("k");
        session.frame();
    }
    assert_eq!(selected_row(&session, source).text.trim(), "file-05.txt");
    assert_eq!(session.views[&source].location.scroll, 0);
}

#[test]
fn wheel_scrolling_keeps_the_selection_across_refreshes_and_repeated_ticks() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    let primary = session.views[&source].location.selection.primary.clone();
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(source))
        .unwrap();
    let (column, row) = (region.body.x as u16, region.body.y as u16);
    for expected in [3, 6] {
        session.mouse(
            frame.mouse_context,
            zess_ui::MouseEvent {
                kind: zess_ui::MouseKind::ScrollDown,
                column,
                row,
            },
        );
        session.input("R");
        settle(&mut session);
        session.frame();
        assert_eq!(session.views[&source].location.scroll, expected);
        assert_eq!(session.views[&source].location.selection.primary, primary);
        assert_eq!(session.focus, source);
    }
}

#[test]
fn wheel_scrolling_at_the_end_does_not_reverse_half_page_navigation() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    for _ in 0..4 {
        session.input("C-d");
        session.frame();
    }
    assert_eq!(session.views[&source].location.scroll, 34);
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(source))
        .unwrap();
    let (column, row) = (region.body.x as u16, region.body.y as u16);
    for (kind, expected) in [
        (zess_ui::MouseKind::ScrollDown, 34),
        (zess_ui::MouseKind::ScrollUp, 31),
        (zess_ui::MouseKind::ScrollDown, 34),
    ] {
        session.mouse(
            frame.mouse_context,
            zess_ui::MouseEvent { kind, column, row },
        );
        assert_eq!(session.views[&source].location.scroll, expected);
        session.frame();
        assert_eq!(session.views[&source].location.scroll, expected);
    }
}

#[test]
fn filtering_after_wheel_scrolling_keeps_matching_files_visible() {
    let (_fixture, mut session) = file_list();
    let source = session.focus;
    let frame = session.frame();
    let region = session
        .mouse_regions
        .iter()
        .find(|region| region.target == RenderTarget::View(source))
        .unwrap();
    let (column, row) = (region.body.x as u16, region.body.y as u16);
    session.mouse(
        frame.mouse_context,
        zess_ui::MouseEvent {
            kind: zess_ui::MouseKind::ScrollDown,
            column,
            row,
        },
    );
    session.frame();
    assert_eq!(session.views[&source].location.scroll, 3);
    for key in ["/", "3", "9", "Enter"] {
        session.input(key);
    }
    let frame = session.frame();
    assert_eq!(selected_row(&session, source).text.trim(), "file-39.txt");
    let selected = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| span.text.trim() == "file-39.txt")
        .expect("the matching file should remain visible");
    assert_eq!(selected.style, "selection.");
}
