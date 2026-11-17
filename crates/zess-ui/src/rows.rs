use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{DisplayCell, RowLayout, Span};

#[cfg(test)]
mod tests;

pub fn fit(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        if grapheme.chars().any(char::is_control) {
            continue;
        }
        let size = UnicodeWidthStr::width(grapheme);
        if used + size > width {
            break;
        }
        output.push_str(grapheme);
        used += size;
    }
    output.push_str(&" ".repeat(width - used));
    output
}

fn joined_text<'a>(mut parts: impl Iterator<Item = &'a str>) -> Cow<'a, str> {
    let first = parts.next().unwrap_or_default();
    let Some(second) = parts.next() else {
        return Cow::Borrowed(first);
    };

    // A style change can occur inside a grapheme in ANSI output.
    let mut text = String::with_capacity(first.len() + second.len());
    text.push_str(first);
    text.push_str(second);
    text.extend(parts);
    Cow::Owned(text)
}

fn printable_ascii_width<'a>(mut parts: impl Iterator<Item = &'a str>) -> Option<usize> {
    parts.try_fold(0, |width, text| {
        text.bytes()
            .all(|byte| matches!(byte, b' '..=b'~'))
            .then_some(width + text.len())
    })
}

/// Returns the byte length and display width of the longest prefix that fits.
/// A grapheme may cross part boundaries.
pub fn fitting_prefix<'a>(
    parts: impl Iterator<Item = &'a str> + Clone,
    width: usize,
) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }
    if let Some(length) = printable_ascii_width(parts.clone()) {
        let size = width.min(length);
        return (size, size);
    }
    let text = joined_text(parts);
    let mut bytes = 0;
    let mut cells = 0;
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme);
        if cells + size > width {
            break;
        }
        bytes += grapheme.len();
        cells += size;
    }
    (bytes, cells)
}

pub fn spans_width(spans: &[Span]) -> usize {
    fitting_prefix(spans.iter().map(|span| span.text.as_str()), usize::MAX).1
}

fn clip_row(mut spans: Vec<Span>, width: usize, fill_style: &str) -> Vec<Span> {
    if width == 0 {
        return Vec::new();
    }
    let (mut bytes, cells) = fitting_prefix(spans.iter().map(|span| span.text.as_str()), width);
    let padding = width - cells;
    for (index, span) in spans.iter_mut().enumerate() {
        if bytes < span.text.len() {
            span.text.truncate(bytes);
            span.text.extend(std::iter::repeat_n(' ', padding));
            let count = index + usize::from(!span.text.is_empty());
            spans.truncate(count);
            return spans;
        }
        bytes -= span.text.len();
    }
    if padding > 0 {
        spans.push(Span {
            base_style: None,
            text: " ".repeat(padding),
            style: fill_style.into(),
        });
    }
    spans
}

pub fn clip_spans(spans: Vec<Span>, width: usize) -> Vec<Span> {
    clip_row(spans, width, "")
}

pub fn align_row(
    spans: Vec<Span>,
    cells: &[DisplayCell],
    style: &str,
    layout: &RowLayout<'_>,
) -> Vec<Span> {
    let mut spans = clip_row(spans, layout.text_width(), style);
    for (index, width) in layout.cell_widths().iter().enumerate() {
        let cell = cells
            .get(index)
            .map(|cell| cell.spans.as_slice())
            .unwrap_or_default();
        let used = spans_width(cell);
        spans.push(Span {
            base_style: None,
            text: " ".repeat(layout.gap() + width.saturating_sub(used)),
            style: style.into(),
        });
        spans.extend(cell.iter().map(|span| Span {
            base_style: span.base_style.clone(),
            text: span.text.clone(),
            style: if span.style.is_empty() {
                style.into()
            } else if style == "selection" || style.starts_with("selection.") {
                format!("selection.{}", span.style)
            } else {
                span.style.clone()
            },
        }));
    }
    spans
}

fn skipped_prefix(text: &str, mut skip: usize) -> (usize, usize) {
    let mut bytes = 0;
    let mut padding = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        if skip == 0 {
            break;
        }
        let cells = if grapheme.starts_with(char::is_control) {
            0
        } else {
            UnicodeWidthStr::width(grapheme)
        };
        padding = cells.saturating_sub(skip);
        skip = skip.saturating_sub(cells);
        bytes = index + grapheme.len();
    }
    (bytes, padding)
}

pub fn skip_columns(spans: &mut [Span], skip: usize) {
    if skip == 0 {
        return;
    }
    let (mut bytes, padding) = match spans {
        [span] => skipped_prefix(&span.text, skip),
        _ => {
            let parts = spans.iter().map(|span| span.text.as_str());
            if let Some(length) = printable_ascii_width(parts.clone()) {
                (skip.min(length), 0)
            } else {
                let text = joined_text(parts);
                skipped_prefix(&text, skip)
            }
        }
    };
    for span in spans {
        let len = span.text.len();
        if bytes <= len {
            span.text.replace_range(..bytes, &" ".repeat(padding));
            break;
        }
        bytes -= len;
        span.text.clear();
    }
}

pub fn item_label(
    row: &crate::DisplayRow,
    style: &str,
    jump: Option<&crate::ItemJump>,
) -> Vec<Span> {
    let label = jump.and_then(|jump| jump.label(&row.id));
    let mut spans = Vec::new();
    let indent = "  ".repeat(row.depth.min(30));
    if let Some(label) = label {
        spans.push(Span {
            base_style: None,
            text: indent,
            style: style.to_owned(),
        });
        spans.push(Span {
            base_style: None,
            text: label,
            style: "pending".into(),
        });
        let (bytes, padding) = skipped_prefix(&row.text, 2);
        let rest = &row.text[bytes..];
        spans.push(Span {
            base_style: None,
            text: format!("{}{rest}", " ".repeat(padding)),
            style: style.to_owned(),
        });
    } else {
        spans.push(Span {
            base_style: None,
            text: format!("{indent}{}", row.text),
            style: style.to_owned(),
        });
    }
    spans
}
