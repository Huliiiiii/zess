use super::*;
use zess_ui::{ColumnAlignment, DisplayCell, RowColumns};

#[test]
fn shared_columns_align_counts_with_a_gap_and_no_outer_padding() {
    let rows = [
        DisplayRow {
            text: "file".into(),
            cells: vec![
                DisplayCell {
                    spans: vec![Span {
                        base_style: None,
                        text: "+1".into(),
                        style: "git.added".into(),
                    }],
                },
                DisplayCell {
                    spans: vec![Span {
                        base_style: None,
                        text: "-3".into(),
                        style: "git.deleted".into(),
                    }],
                },
            ],
            ..DisplayRow::default()
        },
        DisplayRow {
            text: "other".into(),
            cells: vec![
                DisplayCell {
                    spans: vec![Span {
                        base_style: None,
                        text: "+100".into(),
                        style: "git.added".into(),
                    }],
                },
                DisplayCell {
                    spans: vec![Span {
                        base_style: None,
                        text: "-20".into(),
                        style: "git.deleted".into(),
                    }],
                },
            ],
            ..DisplayRow::default()
        },
    ];
    let columns = RowColumns::measure(&rows, ColumnAlignment::All);
    let layout = columns.layout(0, 24);
    let rendered = rows
        .iter()
        .map(|row| {
            align_row(
                vec![Span {
                    base_style: None,
                    text: row.text.clone(),
                    style: "selection".into(),
                }],
                &row.cells,
                "selection",
                &layout,
            )
        })
        .collect::<Vec<_>>();
    let texts = rendered
        .iter()
        .map(|spans| {
            spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        texts,
        ["file             +1   -3", "other          +100  -20"]
    );
    assert!(
        rendered[0]
            .iter()
            .any(|span| span.style == "selection.git.added")
    );
    assert!(
        rendered[0]
            .iter()
            .any(|span| span.style == "selection.git.deleted")
    );
}

#[test]
fn narrow_lists_keep_the_filename_before_counts() {
    let row = DisplayRow {
        text: "name".into(),
        cells: vec![DisplayCell {
            spans: vec![Span {
                base_style: None,
                text: "+12 -3".into(),
                style: "git.added".into(),
            }],
        }],
        ..DisplayRow::default()
    };
    let columns = RowColumns::measure(std::slice::from_ref(&row), ColumnAlignment::All);
    let spans = align_row(
        vec![Span {
            base_style: None,
            text: row.text.clone(),
            style: String::new(),
        }],
        &row.cells,
        "",
        &columns.layout(0, 8),
    );

    assert_eq!(
        spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "name    "
    );
}

#[test]
fn changing_alignment_remeasures_unchanged_rows() {
    let rows = [("first", ["+1", "-100"]), ("second", ["+100", "-1"])]
        .into_iter()
        .map(|(text, cells)| DisplayRow {
            text: text.into(),
            cells: cells
                .into_iter()
                .map(|text| DisplayCell {
                    spans: vec![Span {
                        base_style: None,
                        text: text.into(),
                        style: String::new(),
                    }],
                })
                .collect(),
            ..DisplayRow::default()
        })
        .collect();
    let mut display = Display {
        rows,
        rows_revision: Some(1),
        ..Display::default()
    };
    let mut location = ViewLocation::new(1);
    location.set_display(display.clone());
    assert_eq!(location.columns.layout(0, 30).cell_widths(), [4, 4]);
    assert_eq!(location.columns.layout(1, 30).cell_widths(), [4, 4]);

    display.column_alignment = ColumnAlignment::None;
    location.set_display(display);
    assert_eq!(location.columns.layout(0, 30).cell_widths(), [2, 4]);
    assert_eq!(location.columns.layout(1, 30).cell_widths(), [4, 2]);
}
