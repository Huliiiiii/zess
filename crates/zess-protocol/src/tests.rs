use super::*;
use zess_ui::{RenderContent, RenderPlan, RenderRow, RenderSpan};

#[test]
fn applying_a_delta_rebuilds_the_changed_text_and_cursor() {
    fn snapshot(text: &str, cursor: u16) -> SessionSnapshot {
        SessionSnapshot {
            views: vec![ViewSnapshot {
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
                        text: text.into(),
                    }],
                },
                selection: ViewSelection::None,
                scroll: 0,
                horizontal_scroll: 0,
                status: Vec::new(),
            }],
            render_plan: RenderPlan {
                width: 80,
                height: 24,
                cursor: Some((cursor, 0)),
                rows: vec![RenderRow {
                    spans: vec![RenderSpan {
                        base_style: None,
                        style: "text".into(),
                        content: RenderContent::Text {
                            buffer_id: 2,
                            start_char: 0,
                            end_char: text.chars().count(),
                        },
                    }],
                }],
                ..RenderPlan::default()
            },
            ..SessionSnapshot::default()
        }
    }
    let mut state = snapshot("old", 2);
    let next = snapshot("updated", 6);
    SessionDelta::between(&state, &next).apply(&mut state);
    let frame = render_snapshot(&state).unwrap();
    assert_eq!(frame.rows[0].spans[0].text, "updated");
    assert_eq!(frame.cursor, Some((6, 0)));
}
