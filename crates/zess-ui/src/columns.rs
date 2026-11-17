use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::{DisplayRow, rows::spans_width};

#[cfg(test)]
mod tests;

const COLUMN_GAP: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnAlignment {
    None,
    Consecutive,
    #[default]
    All,
}

#[derive(Clone, Default)]
pub struct RowColumns {
    groups: Vec<ColumnGroup>,
    row_groups: Vec<usize>,
}

#[derive(Clone, Default)]
struct ColumnGroup {
    text_width: usize,
    cell_widths: Vec<usize>,
}

impl RowColumns {
    pub fn measure(rows: &[DisplayRow], alignment: ColumnAlignment) -> Self {
        let mut columns = Self::default();
        for (index, row) in rows.iter().enumerate() {
            let new_group = match alignment {
                ColumnAlignment::None => true,
                ColumnAlignment::Consecutive => {
                    index == 0 || row.cells.is_empty() || rows[index - 1].cells.is_empty()
                }
                ColumnAlignment::All => index == 0,
            };
            if new_group {
                columns.groups.push(ColumnGroup::default());
            }
            columns.row_groups.push(columns.groups.len() - 1);
            let group = columns.groups.last_mut().unwrap();
            group.text_width = group
                .text_width
                .max(row.depth.min(30) * 2 + UnicodeWidthStr::width(row.text.as_str()));
            group
                .cell_widths
                .resize(group.cell_widths.len().max(row.cells.len()), 0);
            for (width, cell) in group.cell_widths.iter_mut().zip(&row.cells) {
                *width = (*width).max(spans_width(&cell.spans));
            }
        }
        columns
    }

    pub fn layout(&self, row: usize, width: usize) -> RowLayout<'_> {
        self.row_groups
            .get(row)
            .map(|group| self.groups[*group].layout(width))
            .unwrap_or(RowLayout {
                text_width: width,
                cell_widths: &[],
            })
    }

    pub fn horizontal_scroll_limit(&self, width: usize) -> usize {
        self.groups
            .iter()
            .map(|group| {
                group
                    .text_width
                    .saturating_sub(group.layout(width).text_width)
            })
            .max()
            .unwrap_or_default()
    }
}

impl ColumnGroup {
    fn layout(&self, width: usize) -> RowLayout<'_> {
        let cells_width =
            self.cell_widths.iter().sum::<usize>() + COLUMN_GAP * self.cell_widths.len();
        let show_cells = !self.cell_widths.is_empty() && cells_width <= width / 2;
        RowLayout {
            text_width: if show_cells {
                width - cells_width
            } else {
                width
            },
            cell_widths: if show_cells { &self.cell_widths } else { &[] },
        }
    }
}

pub struct RowLayout<'a> {
    text_width: usize,
    cell_widths: &'a [usize],
}

impl RowLayout<'_> {
    pub fn text_width(&self) -> usize {
        self.text_width
    }

    pub fn cell_widths(&self) -> &[usize] {
        self.cell_widths
    }

    pub fn gap(&self) -> usize {
        COLUMN_GAP
    }
}
