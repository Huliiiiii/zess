use std::ops::Range;

use helix_core::RopeSlice;
use helix_stdx::rope::RopeSliceExt;
use unicode_width::UnicodeWidthStr;

use super::{PreviewViewport, TextBuffer};

#[cfg(test)]
mod tests;

/// Visible source on one logical line. Positions and highlights use document character offsets.
pub struct SourceLine {
    pub line: usize,
    pub first_char: usize,
    pub first_column: usize,
    pub end_of_line: bool,
    pub highlights: Vec<(Range<usize>, String)>,
    pub text: String,
}

fn cell_width(grapheme: RopeSlice<'_>, column: usize, tab_width: usize) -> usize {
    if grapheme == "\t" {
        tab_width - column % tab_width
    } else {
        grapheme
            .as_str()
            .map_or_else(
                || UnicodeWidthStr::width(grapheme.to_string().as_str()),
                UnicodeWidthStr::width,
            )
            .max(1)
    }
}

impl TextBuffer {
    pub fn tab_width(&self) -> usize {
        self.indentation().1
    }

    fn source_fragment(
        &mut self,
        line: usize,
        start: usize,
        end: usize,
        first_column: usize,
    ) -> SourceLine {
        let end_of_line = end == self.rope.line_to_char(line) + self.rope.line(line).len_chars();
        let bytes = self.rope.char_to_byte(start)..self.rope.char_to_byte(end);
        let mut highlights = Vec::new();
        if !bytes.is_empty()
            && let Some(language) = &mut self.language
        {
            let spans = language.highlights(&self.rope, bytes.clone());
            let scopes = language.scopes();
            for (range, index) in spans {
                let left = range.start.max(bytes.start);
                let right = range.end.min(bytes.end);
                if left < right
                    && let Some(scope) = scopes.get(index)
                {
                    highlights.push((
                        self.rope.byte_to_char(left)..self.rope.byte_to_char(right),
                        scope.clone(),
                    ));
                }
            }
        }
        SourceLine {
            line,
            first_char: start,
            first_column,
            end_of_line,
            highlights,
            text: self.rope.slice(start..end).to_string(),
        }
    }

    fn unwrapped_source_lines(
        &mut self,
        scroll: usize,
        horizontal: usize,
        width: usize,
        height: usize,
    ) -> Vec<SourceLine> {
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let tab_width = self.tab_width();
        let right = horizontal.saturating_add(width);
        let mut lines = Vec::new();
        for line in scroll..scroll.saturating_add(height).min(self.rope.len_lines()) {
            let mut pos = self.rope.line_to_char(line);
            let mut column = 0;
            let mut start = None;
            for grapheme in self.rope.line(line).graphemes() {
                if grapheme == "\n" || grapheme == "\r\n" || grapheme == "\r" {
                    start.get_or_insert((pos, column));
                    pos += grapheme.len_chars();
                    break;
                }
                if column >= right {
                    break;
                }
                let next = column + cell_width(grapheme, column, tab_width);
                if next > horizontal {
                    start.get_or_insert((pos, column));
                }
                pos += grapheme.len_chars();
                column = next;
            }
            let (first_char, first_column) = start.unwrap_or((pos, column));
            lines.push(self.source_fragment(line, first_char, pos, first_column));
        }
        lines
    }

    pub fn view_source_lines(&mut self, id: u64, width: usize, height: usize) -> Vec<SourceLine> {
        self.layout_text_view(id, width, height);
        let view = self.view(id);
        self.unwrapped_source_lines(view.scroll(), view.horizontal_scroll(), width, height)
    }

    pub fn surface_source_lines(
        &mut self,
        viewport: &mut PreviewViewport,
        width: usize,
        height: usize,
        soft_wrap: bool,
    ) -> Vec<SourceLine> {
        viewport.hit_positions.clear();
        if !soft_wrap || width == 0 {
            self.layout_unwrapped_preview(viewport, width, height);
            return self.unwrapped_source_lines(
                viewport.scroll,
                viewport.horizontal_scroll,
                width,
                height,
            );
        }
        let Some(range) = self.layout_wrapped_preview(viewport, width, height) else {
            return Vec::new();
        };
        if height == 0 {
            return Vec::new();
        }
        let rows = &self.preview_wrap.as_ref().unwrap().rows;
        let last_row = (viewport.scroll.saturating_add(height) - 1).min(rows.len() - 1);
        let last_line = self.rope.char_to_line(rows[last_row]);
        let tab_width = self.tab_width();
        let mut lines = Vec::new();
        for line in self.rope.char_to_line(range.start)..=last_line {
            let row_start = self.rope.line_to_char(line);
            let start = row_start.max(range.start);
            let end = (row_start + self.rope.line(line).len_chars()).min(range.end);
            let first_column = self
                .rope
                .slice(row_start..start)
                .graphemes()
                .fold(0, |col, g| col + cell_width(g, col, tab_width));
            lines.push(self.source_fragment(line, start, end, first_column));
        }
        lines
    }
}
