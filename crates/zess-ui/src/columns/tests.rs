use super::*;
use crate::{DisplayCell, Span};

fn row(text: &str, cells: &[&str]) -> DisplayRow {
    DisplayRow {
        text: text.into(),
        cells: cells
            .iter()
            .map(|text| DisplayCell {
                spans: vec![Span {
                    base_style: None,
                    text: (*text).into(),
                    style: String::new(),
                }],
            })
            .collect(),
        ..DisplayRow::default()
    }
}

#[test]
fn columns_share_the_widest_cell_across_rows() {
    let columns = RowColumns::measure(
        &[row("file", &["+1", "-999"]), row("other", &["+100", "-2"])],
        ColumnAlignment::All,
    );
    let layout = columns.layout(0, 24);

    assert_eq!(layout.text_width(), 12);
    assert_eq!(layout.cell_widths(), [4, 4]);
    assert_eq!(layout.gap(), 2);
}

#[test]
fn narrow_lists_reserve_their_width_for_names() {
    let columns = RowColumns::measure(
        &[row("long file name", &["+100", "-2"])],
        ColumnAlignment::All,
    );
    let layout = columns.layout(0, 12);

    assert_eq!(layout.text_width(), 12);
    assert!(layout.cell_widths().is_empty());
    assert_eq!(columns.horizontal_scroll_limit(12), 2);
}

#[test]
fn widths_include_wide_characters_and_tree_indentation() {
    let mut row = row("界", &["界+1"]);
    row.depth = 2;
    let columns = RowColumns::measure(&[row], ColumnAlignment::All);

    assert_eq!(columns.layout(0, 12).cell_widths(), [4]);
    assert_eq!(columns.layout(0, 12).text_width(), 6);
    assert_eq!(columns.horizontal_scroll_limit(4), 2);
}

#[test]
fn unaligned_rows_use_their_own_cell_widths() {
    let columns = RowColumns::measure(
        &[row("file", &["+1", "-999"]), row("other", &["+100", "-2"])],
        ColumnAlignment::None,
    );

    assert_eq!(columns.layout(0, 24).cell_widths(), [2, 4]);
    assert_eq!(columns.layout(1, 24).cell_widths(), [4, 2]);
    assert_eq!(columns.layout(0, 24).text_width(), 14);
    assert_eq!(columns.layout(1, 24).text_width(), 14);
}

#[test]
fn consecutive_alignment_stops_at_rows_without_cells() {
    let rows = [
        row("a", &["+100", "-2"]),
        row("b", &["+1", "-999"]),
        row("directory", &[]),
        row("c", &["+2", "-3"]),
        row("d", &["+4", "-50"]),
    ];
    let columns = RowColumns::measure(&rows, ColumnAlignment::Consecutive);

    for index in [0, 1] {
        assert_eq!(columns.layout(index, 30).cell_widths(), [4, 4]);
    }
    assert!(columns.layout(2, 30).cell_widths().is_empty());
    for index in [3, 4] {
        assert_eq!(columns.layout(index, 30).cell_widths(), [2, 3]);
    }
}

#[test]
fn horizontal_scroll_uses_each_groups_name_space() {
    let rows = [row("long filename", &["+100", "-999"]), row("folder", &[])];
    let columns = RowColumns::measure(&rows, ColumnAlignment::Consecutive);

    assert_eq!(columns.horizontal_scroll_limit(24), 1);
    assert_eq!(columns.horizontal_scroll_limit(12), 1);
    assert_eq!(columns.layout(0, 12).text_width(), 12);
    assert!(columns.layout(0, 12).cell_widths().is_empty());
}
