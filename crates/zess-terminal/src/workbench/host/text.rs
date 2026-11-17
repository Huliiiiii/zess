use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_protocol::{TextRange, ViewSelection, VisibleTextLine};
use zess_ui::{FrameRow, Span, fit};

#[cfg(test)]
mod tests;

pub(super) struct TextViewport<'a> {
    pub width: usize,
    pub height: usize,
    pub gutter: usize,
    pub horizontal_scroll: usize,
    pub tab_width: usize,
    pub soft_wrap: bool,
    pub selection: Option<&'a ViewSelection>,
    pub mode: &'a str,
    pub focused: bool,
    pub readonly: bool,
}

fn cursor_at(range: &TextRange, start: usize, end: usize, insert: bool) -> bool {
    if !insert && range.head > range.anchor {
        start < range.head && range.head <= end
    } else {
        start == range.head
    }
}

fn glyph_width(grapheme: &str, column: usize, tab_width: usize) -> usize {
    if grapheme == "\t" {
        tab_width - column % tab_width
    } else {
        UnicodeWidthStr::width(grapheme).max(1)
    }
}

fn visible_glyph(grapheme: &str, cells: usize, visible: usize, wrapped: bool) -> Cow<'_, str> {
    if cells != visible || grapheme == "\t" {
        return " ".repeat(visible).into();
    }
    if !wrapped && UnicodeWidthStr::width(grapheme) == 0 {
        return " ".into();
    }
    if grapheme.chars().any(char::is_control) {
        return grapheme
            .chars()
            .map(|ch| match ch {
                '\0'..='\u{1f}' => char::from_u32(0x2400 + ch as u32).unwrap(),
                '\u{7f}' => '\u{2421}',
                ch if ch.is_control() => '\u{fffd}',
                ch => ch,
            })
            .collect::<String>()
            .into();
    }
    grapheme.into()
}

fn append(row: &mut FrameRow, text: &str, style: &str) {
    if let Some(last) = row.spans.last_mut().filter(|span| span.style == style) {
        last.text.push_str(text);
    } else {
        row.spans.push(Span {
            text: text.to_owned(),
            style: style.to_owned(),
            base_style: None,
        });
    }
}

pub(super) fn render(
    lines: &[VisibleTextLine],
    viewport: TextViewport<'_>,
) -> (Vec<FrameRow>, Option<(u16, u16)>) {
    let width = viewport.width.saturating_sub(viewport.gutter);
    let mut rows = Vec::new();
    let mut cursor = None;
    if width == 0 || viewport.height == 0 {
        return (rows, cursor);
    }
    let (ranges, primary) = match viewport.selection {
        Some(ViewSelection::Text { ranges, primary }) => (ranges.as_slice(), *primary),
        _ => (&[][..], 0),
    };
    let tab_width = viewport.tab_width.max(1);
    let left = if viewport.soft_wrap {
        0
    } else {
        viewport.horizontal_scroll
    };
    let right = left.saturating_add(width);

    for line in lines {
        if rows.len() == viewport.height {
            break;
        }
        let first_row = rows.len();
        rows.push(FrameRow::default());
        let mut primary_line = false;
        let mut locate_cursor = |position, end, column, row| {
            if ranges
                .get(primary)
                .is_some_and(|range| cursor_at(range, position, end, viewport.mode == "insert"))
            {
                primary_line = true;
                if viewport.focused && (left..right).contains(&column) {
                    cursor = Some(((column - left + viewport.gutter) as u16, row as u16));
                }
            }
        };
        let mut column = if viewport.soft_wrap {
            0
        } else {
            line.first_column
        };
        if column > left {
            append(
                &mut rows[first_row],
                &" ".repeat((column - left).min(width)),
                "",
            );
        }
        let content = line.text.trim_end_matches(['\r', '\n']);
        let mut position = line.first_char;
        let mut highlights = line.highlights.as_slice();
        let mut rendered_end = true;
        for grapheme in content.graphemes(true) {
            let end = position + grapheme.chars().count();
            let mut cells = glyph_width(grapheme, column, tab_width);
            if viewport.soft_wrap && column != 0 && cells > width.saturating_sub(column) {
                if rows.len() == viewport.height {
                    rendered_end = false;
                    break;
                }
                rows.push(FrameRow::default());
                column = 0;
                cells = glyph_width(grapheme, column, tab_width);
            }
            locate_cursor(position, end, column, rows.len() - 1);
            let visible = column
                .saturating_add(cells)
                .min(right)
                .saturating_sub(column.max(left));
            if visible > 0 {
                while highlights
                    .first()
                    .is_some_and(|scope| scope.end <= position)
                {
                    highlights = &highlights[1..];
                }
                let mut style = highlights
                    .first()
                    .filter(|scope| scope.start <= position)
                    .map_or("", |scope| scope.style.as_str());
                let i = ranges.partition_point(|range| {
                    range.head.max(range.anchor) <= position
                        && range.head.min(range.anchor) < position
                });
                if let Some(range) = ranges.get(i) {
                    let selected = range.head.min(range.anchor) <= position
                        && position < range.head.max(range.anchor);
                    if viewport.focused && i != primary && cursor_at(range, position, end, false) {
                        style = "cursor";
                    } else if (viewport.focused || !viewport.readonly) && selected {
                        style = "selection";
                    }
                }
                let value = visible_glyph(grapheme, cells, visible, viewport.soft_wrap);
                append(rows.last_mut().unwrap(), &value, style);
            }
            position = end;
            column = column.saturating_add(cells);
        }
        if rendered_end && line.end_of_line {
            let end = position + line.text[content.len()..].chars().count();
            locate_cursor(position, end, column, rows.len() - 1);
        }
        if viewport.gutter > 0 {
            let number = line.line + 1;
            let digits = viewport.gutter - 1;
            for (index, row) in rows[first_row..].iter_mut().enumerate() {
                let text = if index == 0 {
                    format!("{number:>digits$} ")
                } else {
                    String::new()
                };
                row.spans.insert(
                    0,
                    Span {
                        text: fit(&text, viewport.gutter),
                        style: if viewport.focused && primary_line {
                            "linenr.current"
                        } else {
                            "linenr"
                        }
                        .into(),
                        base_style: None,
                    },
                );
            }
        }
    }
    (rows, cursor)
}
