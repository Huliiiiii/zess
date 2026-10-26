use super::*;

#[test]
fn browser_content_and_status_share_the_same_right_inset_in_splits() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("README.md"), "preview").unwrap();
    let mut session = fixture.session(&[]);
    session.resize(240, 16);
    session.command("files").unwrap();
    settle(&mut session);
    let left = session.focus;
    session.command("vsplit").unwrap();
    settle(&mut session);
    let right = session.focus;
    for id in [left, right] {
        let view = session.views.get_mut(&id).unwrap();
        view.location.display.status = json!({"git": {"additions":15517,"deletions":9778}});
        view.location.display.rows[0].cells = vec![zess_ui::DisplayCell {
            spans: vec![Span {
                base_style: None,
                text: "M".into(),
                style: "modified".into(),
            }],
        }];
        view.location.columns = zess_ui::RowColumns::measure(
            &view.location.display.rows,
            view.location.display.column_alignment,
        );
    }
    let frame = session.frame();
    for id in [left, right] {
        let region = session
            .mouse_regions
            .iter()
            .find(|region| region.target == RenderTarget::View(id))
            .unwrap();
        assert_eq!(region.body.x, region.pane.x);
        assert_eq!(region.body.width + 1, region.pane.width);
        assert_eq!(session.views[&id].viewport_width, region.body.width);
        let first = &frame.rows[region.body.y];
        let text = first
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        let pane = text
            .chars()
            .skip(region.pane.x)
            .take(region.pane.width)
            .collect::<String>();
        assert!(pane.starts_with("  README.md"), "{pane:?}");
        assert!(pane.ends_with("M "), "{pane:?}");
        let status = &frame.rows[region.pane.y + region.pane.height - 1];
        let text = status
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        let pane = text
            .chars()
            .skip(region.pane.x)
            .take(region.pane.width)
            .collect::<String>();
        assert!(pane.ends_with("-9778 "), "{pane:?}");
        let (rows, _) = session.render_view(id, 12, 4);
        let text = rows[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(text.starts_with("  READ"), "{text:?}");
        assert!(
            rows[0]
                .spans
                .iter()
                .all(|span| !span.style.starts_with("linenr"))
        );
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 12);
    }
}
