use super::*;

#[test]
fn applying_a_delta_updates_text_and_selection_together() {
    fn snapshot(text: &str, cursor: usize) -> SessionSnapshot {
        SessionSnapshot {
            views: vec![ViewSnapshot {
                text_metrics: None,
                statusline: None,
                options: None,
                key_scopes: vec!["global".into(), "text".into()],
                view_id: 1,
                buffer_id: 2,
                kind: "text".into(),
                title: "note".into(),
                header: String::new(),
                tabs: Vec::new(),
                loading: false,
                column_alignment: Default::default(),
                version: Some(1),
                mode: "normal".into(),
                content: ViewContent::Text {
                    lines: vec![VisibleTextLine {
                        line: 0,
                        first_char: 0,
                        first_column: 0,
                        end_of_line: true,
                        highlights: vec![],
                        text: text.into(),
                    }],
                },
                selection: ViewSelection::Text {
                    ranges: vec![TextRange {
                        anchor: cursor,
                        head: cursor,
                    }],
                    primary: 0,
                },
                scroll: 0,
                horizontal_scroll: 0,
            }],
            ..SessionSnapshot::default()
        }
    }
    let mut state = snapshot("old", 2);
    let next = snapshot("updated", 6);
    SessionDelta::between(&state, &next).apply(&mut state);
    let ViewContent::Text { lines } = &state.views[0].content else {
        panic!("expected document text")
    };
    assert_eq!(lines[0].text, "updated");
    assert_eq!(
        state.views[0].selection,
        ViewSelection::Text {
            ranges: vec![TextRange { anchor: 6, head: 6 }],
            primary: 0
        }
    );
}
