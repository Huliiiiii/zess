use std::ops::{Range, RangeInclusive};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_git::patch::{LineKind, PatchRow, RawPatch};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct FileIndex(usize);

pub(super) struct Diff {
    pub source: Option<zess_git::DiffSource>,
    pub files: Vec<zess_git::FileDiff>,
    pub raw: String,
    pub rows: Vec<Row>,
    hunks: Vec<Hunk>,
    selectable: Vec<SelectableRow>,
    pub blocks: Vec<RangeInclusive<usize>>,
    pub width: usize,
    pub line_number_width: usize,
}

impl Diff {
    pub fn new(raw: String) -> Self {
        let mut offset = 0;
        let mut rows = raw
            .split_inclusive('\n')
            .enumerate()
            .map(|(index, line)| {
                let bytes = offset..offset + line.len();
                offset += line.len();
                Row {
                    bytes,
                    file: None,
                    patch_row: PatchRow(index),
                    old_line: None,
                    new_line: None,
                    source_line: None,
                    kind: None,
                    hunk: None,
                    block: None,
                }
            })
            .collect::<Vec<_>>();
        let mut blocks: Vec<RangeInclusive<usize>> = Vec::new();
        let mut hunks = Vec::new();
        if let Ok(patch) = RawPatch::parse(&raw) {
            for (index, hunk) in patch.hunks().enumerate() {
                let (mut old, mut new) = (hunk.old_start, hunk.new_start);
                rows[hunk.row.0].hunk = Some(index);
                let mut block = None;
                for line in &hunk.lines {
                    let row = &mut rows[line.row.0];
                    row.kind = Some(line.kind);
                    row.hunk = Some(index);
                    let (old_line, new_line) = match line.kind {
                        LineKind::Context => {
                            block = None;
                            (Some(old), Some(new))
                        }
                        LineKind::Deletion => (Some(old), None),
                        LineKind::Addition => (None, Some(new)),
                        LineKind::NoNewline => continue,
                    };
                    row.old_line = old_line;
                    row.new_line = new_line;
                    row.source_line = Some(new.saturating_sub(1));
                    if matches!(line.kind, LineKind::Addition | LineKind::Deletion) {
                        let index = *block.get_or_insert_with(|| {
                            blocks.push(line.row.0..=line.row.0);
                            blocks.len() - 1
                        });
                        row.block = Some(index);
                        let start = *blocks[index].start();
                        blocks[index] = start..=line.row.0;
                    }
                    old += usize::from(old_line.is_some());
                    new += usize::from(new_line.is_some());
                }
                let old_len = old - hunk.old_start;
                let new_len = new - hunk.new_start;
                hunks.push(Hunk {
                    file: None,
                    old_start: if old_len == 0 {
                        hunk.old_start
                    } else {
                        hunk.old_start.saturating_sub(1)
                    },
                    old_len,
                    new_start: if new_len == 0 {
                        hunk.new_start
                    } else {
                        hunk.new_start.saturating_sub(1)
                    },
                    new_len,
                });
            }
        }
        let selectable = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_selectable())
            .map(|(row, _)| SelectableRow(row))
            .collect();
        let width = rows
            .iter()
            .map(|row| UnicodeWidthStr::width(display_line(&raw[row.bytes.clone()]).as_str()))
            .max()
            .unwrap_or(0);
        let line_number_width = rows
            .iter()
            .filter_map(|row| row.new_line.or(row.old_line))
            .max()
            .unwrap_or(0)
            .to_string()
            .len();
        Self {
            source: None,
            files: Vec::new(),
            raw,
            rows,
            hunks,
            selectable,
            blocks,
            width,
            line_number_width,
        }
    }

    pub fn from_git(diff: zess_git::GitDiff) -> Self {
        let mut result = Self::new(String::new());
        result.source = Some(diff.source);
        result.files = diff.files;
        for (file, patch) in result.files.iter().enumerate() {
            let parsed = Self::new(patch.raw.clone());
            let row_offset = result.rows.len();
            let byte_offset = result.raw.len();
            let hunk_offset = result.hunks.len();
            let block_offset = result.blocks.len();
            result.raw.push_str(&patch.raw);
            for (local, mut row) in parsed.rows.into_iter().enumerate() {
                row.bytes = row.bytes.start + byte_offset..row.bytes.end + byte_offset;
                row.file = Some(FileIndex(file));
                row.patch_row = PatchRow(local);
                row.hunk = row.hunk.map(|hunk| hunk + hunk_offset);
                row.block = row.block.map(|block| block + block_offset);
                result.rows.push(row);
            }
            result
                .hunks
                .extend(parsed.hunks.into_iter().map(|mut hunk| {
                    hunk.file = Some(FileIndex(file));
                    hunk
                }));
            result.blocks.extend(
                parsed
                    .blocks
                    .into_iter()
                    .map(|block| block.start() + row_offset..=block.end() + row_offset),
            );
            result.selectable.extend(
                parsed
                    .selectable
                    .into_iter()
                    .map(|row| SelectableRow(row.0 + row_offset)),
            );
            result.width = result.width.max(parsed.width);
            result.line_number_width = result.line_number_width.max(parsed.line_number_width);
        }
        result
    }

    pub fn matches_git(&self, diff: &zess_git::GitDiff) -> bool {
        self.source.as_ref() == Some(&diff.source)
            && self.files.len() == diff.files.len()
            && self
                .files
                .iter()
                .zip(&diff.files)
                .all(|(left, right)| left.target == right.target && left.raw == right.raw)
    }

    pub fn file_at(&self, row: usize) -> Option<&zess_git::FileDiff> {
        self.files.get(self.rows.get(row)?.file?.0)
    }

    pub fn line(&self, row: usize) -> &str {
        self.rows
            .get(row)
            .map(|row| self.raw[row.bytes.clone()].trim_end_matches('\n'))
            .unwrap_or("")
    }

    pub fn display_line(&self, row: usize) -> String {
        display_line(self.line(row))
    }

    pub fn map_old_line(&self, target: Option<&zess_git::DiffTarget>, line: usize) -> usize {
        let file = if let Some(target) = target {
            let Some(index) = self.files.iter().position(|file| &file.target == target) else {
                return line;
            };
            Some(FileIndex(index))
        } else {
            None
        };
        let mut offset = 0;
        for (index, hunk) in self.hunks.iter().enumerate() {
            if hunk.file != file {
                continue;
            }
            if line < hunk.old_start {
                break;
            }
            if line < hunk.old_start + hunk.old_len {
                return self
                    .rows
                    .iter()
                    .find(|row| row.hunk == Some(index) && row.old_line == Some(line + 1))
                    .and_then(|row| row.source_line)
                    .unwrap_or(hunk.new_start);
            }
            offset =
                (hunk.new_start + hunk.new_len) as isize - (hunk.old_start + hunk.old_len) as isize;
        }
        line.saturating_add_signed(offset)
    }

    pub fn initial_selection(&self) -> Selection {
        let head = self
            .blocks
            .first()
            .and_then(|block| self.selectable_at(*block.start()))
            .or_else(|| self.selectable.first().copied());
        if self.blocks.is_empty() {
            Selection::Line { head }
        } else {
            Selection::Block { head }
        }
    }

    pub fn search(
        &self,
        pattern: &str,
        row: Option<usize>,
        forward: bool,
    ) -> anyhow::Result<Option<SelectableRow>> {
        let crlf = self
            .raw
            .split_inclusive('\n')
            .next()
            .is_some_and(|line| line.ends_with("\r\n"));
        let regex = helix_core::regex::RegexBuilder::new(pattern)
            .case_insensitive(!pattern.chars().any(char::is_uppercase))
            .multi_line(true)
            .crlf(crlf)
            .build()?;
        let mut first = None;
        let mut last = None;
        let mut next = None;
        let mut previous = None;
        for found in regex.find_iter(&self.raw) {
            let found_row = self
                .rows
                .partition_point(|row| row.bytes.end <= found.start())
                .min(self.rows.len().saturating_sub(1));
            let Some(found_head) = self.selectable_at(found_row) else {
                continue;
            };
            first.get_or_insert(found_head);
            last = Some(found_head);
            if row.is_none_or(|row| found_row > row) {
                next.get_or_insert(found_head);
            }
            if row.is_none_or(|row| found_row < row) {
                previous = Some(found_head);
            }
        }
        Ok(if forward {
            next.or(first)
        } else {
            previous.or(last)
        })
    }

    fn selectable_at(&self, row: usize) -> Option<SelectableRow> {
        self.rows
            .get(row)
            .filter(|row| row.is_selectable())
            .map(|_| SelectableRow(row))
    }

    fn nearest(&self, row: usize) -> Option<SelectableRow> {
        self.selectable
            .iter()
            .min_by_key(|head| head.0.abs_diff(row))
            .copied()
    }
}

struct Hunk {
    file: Option<FileIndex>,
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
}

pub(super) struct Row {
    bytes: Range<usize>,
    file: Option<FileIndex>,
    pub patch_row: PatchRow,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub source_line: Option<usize>,
    pub kind: Option<LineKind>,
    pub hunk: Option<usize>,
    pub block: Option<usize>,
}

impl Row {
    fn is_selectable(&self) -> bool {
        matches!(
            self.kind,
            Some(LineKind::Context | LineKind::Addition | LineKind::Deletion)
        )
    }
}

#[derive(Clone, Copy)]
pub(super) struct SelectableRow(usize);

#[derive(Clone)]
pub(super) enum Selection {
    Block {
        head: Option<SelectableRow>,
    },
    Line {
        head: Option<SelectableRow>,
    },
    Range {
        anchor: SelectableRow,
        head: SelectableRow,
    },
}

impl Default for Selection {
    fn default() -> Self {
        Self::Block { head: None }
    }
}

impl Selection {
    fn head(&self) -> Option<SelectableRow> {
        match *self {
            Self::Block { head } | Self::Line { head } => head,
            Self::Range { head, .. } => Some(head),
        }
    }

    pub fn row(&self) -> Option<usize> {
        self.head().map(|head| head.0)
    }

    pub fn set_head(&mut self, head: Option<SelectableRow>) {
        *self = match *self {
            Self::Block { .. } => Self::Block { head },
            Self::Line { .. } => Self::Line { head },
            Self::Range { anchor, .. } => match head {
                Some(head) => Self::Range { anchor, head },
                None => Self::Line { head: None },
            },
        };
    }

    pub fn rows(&self, diff: &Diff) -> Option<RangeInclusive<usize>> {
        let row = self.row()?;
        Some(match *self {
            Self::Block { .. } => diff.rows[row]
                .block
                .map(|block| diff.blocks[block].clone())
                .unwrap_or(row..=row),
            Self::Line { .. } => row..=row,
            Self::Range { anchor, .. } => anchor.0.min(row)..=anchor.0.max(row),
        })
    }

    pub fn contains(&self, diff: &Diff, row: usize) -> bool {
        diff.selectable_at(row).is_some()
            && self.rows(diff).is_some_and(|range| range.contains(&row))
    }

    pub fn patch_rows(&self, diff: &Diff) -> Option<RangeInclusive<PatchRow>> {
        self.rows(diff)
            .map(|range| diff.rows[*range.start()].patch_row..=diff.rows[*range.end()].patch_row)
    }

    pub fn select_at(&mut self, diff: &Diff, row: usize, extend: bool) -> bool {
        let Some(head) = diff.selectable_at(row) else {
            return false;
        };
        if extend {
            *self = Self::Range {
                anchor: match *self {
                    Self::Range { anchor, .. } => anchor,
                    _ => self.head().unwrap_or(head),
                },
                head,
            };
        } else {
            self.set_head(Some(head));
            if matches!(self, Self::Block { .. }) && diff.rows[head.0].block.is_none() {
                self.select_nearest_block(diff);
            }
        }
        true
    }

    pub fn move_by(&mut self, diff: &Diff, forward: bool, force_block: bool) {
        let Some(row) = self.row() else {
            self.set_head(diff.initial_selection().head());
            return;
        };
        if matches!(self, Self::Block { .. }) || force_block {
            let current = diff.rows[row].block;
            let target = if forward {
                diff.blocks
                    .iter()
                    .enumerate()
                    .find(|(i, block)| Some(*i) != current && *block.start() > row)
            } else {
                diff.blocks
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(i, block)| Some(*i) != current && *block.start() < row)
            };
            if let Some((_, block)) = target {
                self.set_head(diff.selectable_at(*block.start()));
            }
        } else {
            self.move_by_lines(diff, if forward { 1 } else { -1 });
        }
    }

    pub fn move_by_lines(&mut self, diff: &Diff, amount: isize) {
        let Some(row) = self.row() else {
            self.set_head(diff.initial_selection().head());
            return;
        };
        let index = diff.selectable.partition_point(|head| head.0 < row);
        self.set_head(
            diff.selectable
                .get(
                    index
                        .saturating_add_signed(amount)
                        .min(diff.selectable.len().saturating_sub(1)),
                )
                .copied(),
        );
    }

    pub fn move_to(&mut self, diff: &Diff, row: usize) {
        self.set_head(if matches!(self, Self::Block { .. }) {
            diff.blocks
                .iter()
                .find(|block| *block.end() >= row)
                .or_else(|| diff.blocks.last())
                .and_then(|block| diff.selectable_at(*block.start()))
                .or_else(|| diff.nearest(row))
        } else {
            diff.nearest(row)
        });
    }

    pub fn move_to_edge(&mut self, diff: &Diff, forward: bool, count: Option<usize>) {
        let row = if let Some(count) = count {
            diff.selectable
                .get(
                    count
                        .saturating_sub(1)
                        .min(diff.selectable.len().saturating_sub(1)),
                )
                .map(|head| head.0)
                .unwrap_or(0)
        } else if forward {
            usize::MAX
        } else {
            0
        };
        self.move_to(diff, row);
    }

    pub fn focus_visible(&mut self, diff: &Diff, visible: Range<usize>) {
        if self.row().is_some_and(|row| visible.contains(&row)) {
            return;
        }
        self.set_head(
            diff.selectable
                .iter()
                .find(|head| visible.contains(&head.0) && diff.rows[head.0].block.is_some())
                .copied()
                .or_else(|| {
                    let middle = visible.start + visible.len() / 2;
                    diff.selectable
                        .iter()
                        .filter(|head| visible.contains(&head.0))
                        .min_by_key(|head| head.0.abs_diff(middle))
                        .copied()
                }),
        );
    }

    pub fn select_block(&mut self, diff: &Diff) {
        let head = if self.row().is_none_or(|row| diff.rows[row].block.is_none()) {
            diff.blocks
                .first()
                .and_then(|block| diff.selectable_at(*block.start()))
                .or_else(|| diff.selectable.first().copied())
        } else {
            self.head()
        };
        *self = Self::Block { head };
    }

    pub fn toggle_range(&mut self) {
        if let Some(head) = self.head() {
            *self = match self {
                Self::Range { .. } => Self::Line { head: Some(head) },
                _ => Self::Range { anchor: head, head },
            };
        }
    }

    pub fn reconcile(&mut self, old: &Diff, new: &Diff) {
        let map = |head: SelectableRow| {
            let index = head.0;
            let previous = &old.rows[index];
            new.selectable
                .iter()
                .filter(|head| {
                    let next = &new.rows[head.0];
                    old.file_at(index).map(|file| &file.target)
                        == new.file_at(head.0).map(|file| &file.target)
                        && previous.kind == next.kind
                        && previous.old_line == next.old_line
                        && previous.new_line == next.new_line
                        && old.line(index) == new.line(head.0)
                })
                .min_by_key(|head| head.0.abs_diff(index))
                .copied()
                .or_else(|| {
                    previous.block.and_then(|_| {
                        new.blocks
                            .iter()
                            .filter(|block| {
                                old.file_at(index).map(|file| &file.target)
                                    == new.file_at(*block.start()).map(|file| &file.target)
                            })
                            .min_by_key(|block| block.start().abs_diff(index))
                            .and_then(|block| new.selectable_at(*block.start()))
                    })
                })
                .or_else(|| {
                    new.selectable
                        .iter()
                        .filter(|head| {
                            old.file_at(index).map(|file| &file.target)
                                == new.file_at(head.0).map(|file| &file.target)
                        })
                        .min_by_key(|head| head.0.abs_diff(index))
                        .copied()
                })
        };
        let head = self
            .head()
            .and_then(map)
            .or_else(|| new.initial_selection().head());
        *self = match *self {
            Self::Block { .. } => Self::Block { head },
            Self::Line { .. } => Self::Line { head },
            Self::Range { anchor, .. } => match (map(anchor), head) {
                (Some(anchor), Some(head)) => Self::Range { anchor, head },
                _ => Self::Line { head },
            },
        };
        if matches!(self, Self::Block { .. })
            && self.row().is_some_and(|row| new.rows[row].block.is_none())
        {
            self.select_nearest_block(new);
        }
    }

    fn select_nearest_block(&mut self, diff: &Diff) {
        if let Some(row) = self.row() {
            if let Some(block) = diff
                .blocks
                .iter()
                .min_by_key(|block| block.start().abs_diff(row))
            {
                self.set_head(diff.selectable_at(*block.start()));
            }
        }
    }

    pub fn toggle(&mut self, diff: &Diff) {
        let head = self.head();
        *self = if matches!(self, Self::Block { .. }) {
            Self::Line { head }
        } else {
            Self::Block { head }
        };
        if matches!(self, Self::Block { .. })
            && self.row().is_some_and(|row| diff.rows[row].block.is_none())
        {
            self.select_nearest_block(diff);
        }
    }
}

pub(super) struct Search {
    pub forward: bool,
    pub selection: Selection,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub manual_viewport: bool,
}

fn display_line(raw: &str) -> String {
    let mut text = String::new();
    let mut column = 0;
    for grapheme in raw.trim_end_matches(['\r', '\n']).graphemes(true) {
        if grapheme == "\t" {
            let width = 4 - column % 4;
            text.extend(std::iter::repeat_n(' ', width));
            column += width;
        } else {
            for ch in grapheme.chars() {
                text.push(match ch {
                    '\0'..='\u{1f}' => char::from_u32(0x2400 + ch as u32).unwrap(),
                    '\u{7f}' => '\u{2421}',
                    ch if ch.is_control() => '\u{fffd}',
                    ch => ch,
                });
            }
            column += UnicodeWidthStr::width(grapheme).max(1);
        }
    }
    text
}
