use super::*;
use zess_protocol::{ViewArea, ViewContent, ViewSnapshot};

fn review() -> SessionSnapshot {
    let view = ViewSnapshot {
        view_id: 4,
        buffer_id: 7,
        kind: "review".into(),
        key_scopes: vec!["global".into(), "data".into(), "review".into()],
        title: "Review".into(),
        header: "Review queue".into(),
        tabs: vec![],
        loading: false,
        column_alignment: ColumnAlignment::default(),
        version: None,
        mode: "normal".into(),
        text_metrics: None,
        statusline: None,
        options: Some(serde_json::json!({"statusline":""})),
        content: ViewContent::Items {
            rows: vec![SemanticRow {
                line_number: None,
                id: "doc".into(),
                text: "Chapter".into(),
                cells: vec![vec![span("draft".into(), "comment")]],
                style: "item".into(),
                depth: 1,
            }],
        },
        scroll: 0,
        horizontal_scroll: 0,
        selection: ViewSelection::Items {
            ids: vec!["doc".into()],
            primary: Some("doc".into()),
        },
    };
    SessionSnapshot {
        focused_view: Some(4),
        views: vec![view],
        layout: vec![ViewArea {
            view_id: Some(4),
            buffer_id: 7,
            page_id: None,
            slot_id: None,
            x: 0,
            y: 0,
            width: 28,
            height: 4,
        }],
        ..Default::default()
    }
}

#[test]
fn a_plugin_view_projects_the_selected_item_and_its_columns_at_the_terminal_width() {
    let state = review();
    let wide = render(&state, 28, 5).unwrap();
    assert_eq!(wide.rows[0].spans[0].text.trim_end(), "Review queue");
    assert_eq!(wide.rows[1].spans[0].text, "  Chapter");
    assert_eq!(wide.rows[1].spans[0].style, "selection.item");
    assert!(
        wide.rows[1]
            .spans
            .iter()
            .any(|s| s.style == "selection.comment" && s.text == "draft")
    );
    let narrow = render(&state, 8, 3).unwrap();
    let visible = narrow.rows[1]
        .spans
        .iter()
        .map(|s| s.text.as_str())
        .collect::<String>();
    assert_eq!(visible, "  Chapte");
}

#[test]
fn a_layout_cannot_display_a_view_whose_buffer_has_been_replaced() {
    let mut state = review();
    state.layout[0].buffer_id = 8;
    assert!(render(&state, 28, 5).is_err());
}

#[test]
fn a_prompt_cursor_must_address_a_character_boundary_in_its_owners_input() {
    let mut state = review();
    state.prompt = Some(zess_protocol::PromptSnapshot {
        label: "query: ".into(),
        error: None,
        owner_buffer: 7,
        kind: "query".into(),
        text: "λ".into(),
        cursor: 1,
        owner_view: 4,
    });
    assert!(state.validate().is_err());
    state.prompt.as_mut().unwrap().cursor = 2;
    assert!(state.validate().is_ok());
    for (width, height) in [(0, 0), (0, 1), (1, 0), (1, 1), (2, 2), (80, 24)] {
        let frame = render(&state, width, height).unwrap();
        assert_eq!(frame.rows.len(), usize::from(height.saturating_sub(1)));
    }
    state.prompt.as_mut().unwrap().owner_buffer = 8;
    assert!(state.validate().is_err());
}

#[test]
fn a_text_surface_declares_the_metrics_for_its_source_fragments() {
    let mut state = review();
    state.surfaces.push(SurfaceSnapshot {
        text_metrics: None,
        column_alignment: ColumnAlignment::default(),
        soft_wrap: true,
        page_id: 1,
        slot_id: "preview".into(),
        buffer_id: 2,
        header: String::new(),
        loading: false,
        content: ViewContent::Text {
            lines: vec![zess_protocol::VisibleTextLine {
                line: 3,
                first_char: 12,
                first_column: 0,
                text: "source".into(),
                highlights: vec![],
                end_of_line: true,
            }],
        },
        scroll: 3,
        horizontal_scroll: 0,
    });
    assert!(state.validate().is_err());
    let surface = &mut state.surfaces[0];
    surface.text_metrics = Some(zess_protocol::TextMetrics {
        line_count: 3,
        tab_width: 4,
    });
    assert!(state.validate().is_err());
    state.surfaces[0].text_metrics.as_mut().unwrap().line_count = 4;
    assert!(state.validate().is_ok());
    state.surfaces[0].text_metrics.as_mut().unwrap().tab_width = 0;
    assert!(state.validate().is_err());
}
