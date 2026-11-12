use std::{
    ops::{Range, RangeInclusive},
    path::{Component, Path},
};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_git::patch::{LineKind, PatchRow, RawPatch};
use zess_ui::{DiffLineKind, DiffLineMetadata, FrameRow, TextStyle};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct FileIndex(usize);

#[derive(Clone, Copy, Default, PartialEq)]
pub(super) enum Presentation {
    #[default]
    Raw,
    Rendered,
}

pub(super) struct Rendered {
    pub rows: Vec<FrameRow>,
    pub styles: std::collections::HashMap<String, TextStyle>,
    pub width: usize,
    files: Vec<FileIndex>,
    text: String,
    metadata: Vec<Vec<DiffLineMetadata>>,
    mapping: RenderedMapping,
}

enum RenderedMapping {
    DisplayOnly,
    SourceMapped {
        index: DiffIndex,
        raw_rows: Vec<Vec<usize>>,
    },
}

impl RenderedMapping {
    fn index(&self) -> Option<&DiffIndex> {
        match self {
            Self::DisplayOnly => None,
            Self::SourceMapped { index, .. } => Some(index),
        }
    }

    fn raw_rows(&self) -> &[Vec<usize>] {
        match self {
            Self::DisplayOnly => &[],
            Self::SourceMapped { raw_rows, .. } => raw_rows,
        }
    }
}

pub(super) struct Diff {
    pub source: Option<zess_git::DiffSource>,
    pub files: Vec<zess_git::FileDiff>,
    pub raw: String,
    pub index: DiffIndex,
    hunks: Vec<Hunk>,
    pub width: usize,
    pub line_number_width: usize,
    pub rendered: Option<Rendered>,
}

pub(super) struct DiffIndex {
    pub rows: Vec<Row>,
    selectable: Vec<SelectableRow>,
    pub blocks: Vec<RangeInclusive<usize>>,
    block_raw_rows: Vec<Vec<usize>>,
}

impl Diff {
    pub fn index(&self, presentation: Presentation) -> &DiffIndex {
        if presentation == Presentation::Rendered
            && let Some(index) = self
                .rendered
                .as_ref()
                .and_then(|rendered| rendered.mapping.index())
        {
            return index;
        }
        &self.index
    }

    pub fn mapped_rendering(&self) -> bool {
        self.rendered.as_ref().is_some_and(|rendered| {
            matches!(rendered.mapping, RenderedMapping::SourceMapped { .. })
        })
    }

    fn rendered_index(
        &self,
        rendered: &Rendered,
        workspace: &Path,
    ) -> Option<(DiffIndex, Vec<Vec<usize>>)> {
        if !rendered.metadata.iter().any(|records| !records.is_empty()) {
            return None;
        }
        let mut identities = std::collections::HashMap::new();
        for (raw_row, row) in self.index.rows.iter().enumerate() {
            let (kind, old_line) = match row.kind {
                Some(LineKind::Context) => (DiffLineKind::Context, None),
                Some(LineKind::Addition) => (DiffLineKind::Addition, None),
                Some(LineKind::Deletion) => (DiffLineKind::Deletion, row.old_line),
                _ => continue,
            };
            identities
                .entry((row.file?.0, kind, row.patch_new_line?, old_line))
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some(raw_row));
        }
        let mut index = DiffIndex {
            rows: Vec::new(),
            selectable: Vec::new(),
            blocks: Vec::new(),
            block_raw_rows: Vec::new(),
        };
        let mut raw_rows = Vec::new();
        let mut row_blocks = Vec::new();
        let mut groups: Vec<usize> = (0..self.index.blocks.len()).collect();
        let mut offset = 0;
        for (display_row, line) in rendered.text.split_inclusive('\n').enumerate() {
            let file = *rendered.files.get(display_row)?;
            let mut mapped = Vec::new();
            let mut kind = None;
            let mut mapping_failed = false;
            for metadata in &rendered.metadata[display_row] {
                let line_kind = match metadata.kind {
                    DiffLineKind::File | DiffLineKind::Hunk => continue,
                    DiffLineKind::Context => LineKind::Context,
                    DiffLineKind::Addition => LineKind::Addition,
                    DiffLineKind::Deletion => LineKind::Deletion,
                };
                kind.get_or_insert(line_kind);
                let raw_row = (|| {
                    let path = Path::new(&metadata.path);
                    let path = if path.is_absolute() {
                        path.to_owned()
                    } else {
                        self.files[file.0]
                            .rendered
                            .as_ref()?
                            .repository_root
                            .join(path)
                    };
                    let mut normalized = std::path::PathBuf::new();
                    for component in path.components() {
                        match component {
                            Component::Normal(part) => normalized.push(part),
                            Component::RootDir => normalized.push(component.as_os_str()),
                            Component::CurDir => {}
                            _ => return None,
                        }
                    }
                    if normalized != workspace.join(&self.files[file.0].target.path) {
                        return None;
                    }
                    *identities.get(&(
                        file.0,
                        metadata.kind.clone(),
                        metadata.new_line?,
                        metadata.old_line,
                    ))?
                })();
                if let Some(raw_row) = raw_row {
                    if !mapped.contains(&raw_row) {
                        mapped.push(raw_row);
                    }
                } else {
                    mapping_failed = true;
                }
            }
            let mut blocks = mapped
                .iter()
                .filter_map(|row| self.index.rows[*row].block)
                .collect::<Vec<_>>();
            blocks.sort_unstable();
            blocks.dedup();
            // A display line is indivisible. Its hunk includes every complete
            // raw block represented on that line, including shared wrapped rows.
            if let Some((first, remaining)) = blocks.split_first() {
                let group = groups[*first];
                for block in remaining {
                    let previous = groups[*block];
                    if previous == group {
                        continue;
                    }
                    for candidate in &mut groups {
                        if *candidate == previous {
                            *candidate = group;
                        }
                    }
                }
            } else if mapping_failed {
                blocks.push(groups.len());
                groups.push(groups.len());
            }
            let first = mapped.first().map(|row| &self.index.rows[*row]);
            let row = Row {
                bytes: offset..offset + line.len(),
                file: Some(file),
                patch_row: None,
                old_line: mapped.iter().find_map(|row| self.index.rows[*row].old_line),
                new_line: mapped.iter().find_map(|row| self.index.rows[*row].new_line),
                patch_new_line: first.and_then(|row| row.patch_new_line),
                source_line: mapped
                    .iter()
                    .rev()
                    .find_map(|row| self.index.rows[*row].source_line),
                kind,
                hunk: first.and_then(|row| row.hunk),
                block: None,
                mapping_failed,
            };
            if row.is_selectable() {
                index.selectable.push(SelectableRow(display_row));
            }
            index.rows.push(row);
            raw_rows.push(mapped);
            row_blocks.push(blocks);
            offset += line.len();
        }
        let mut group_raw_rows = vec![Vec::new(); groups.len()];
        for (block, rows) in self.index.blocks.iter().enumerate() {
            group_raw_rows[groups[block]].extend(
                rows.clone()
                    .filter(|row| self.index.rows[*row].is_selectable()),
            );
        }
        let mut display_groups = std::collections::HashMap::new();
        for (display_row, blocks) in row_blocks.iter().enumerate() {
            let Some(raw_block) = blocks.first() else {
                continue;
            };
            let group = groups[*raw_block];
            let block = *display_groups.entry(group).or_insert_with(|| {
                index.blocks.push(display_row..=display_row);
                index
                    .block_raw_rows
                    .push(std::mem::take(&mut group_raw_rows[group]));
                index.blocks.len() - 1
            });
            index.rows[display_row].block = Some(block);
            index.blocks[block] = *index.blocks[block].start()..=display_row;
        }
        Some((index, raw_rows))
    }

    pub fn raw_row(&self, presentation: Presentation, row: usize) -> Option<usize> {
        match presentation {
            Presentation::Rendered if self.mapped_rendering() => self
                .rendered
                .as_ref()?
                .mapping
                .raw_rows()
                .get(row)?
                .last()
                .copied(),
            _ => self.index.rows.get(row).map(|_| row),
        }
    }

    fn selection_raw_rows(
        &self,
        presentation: Presentation,
        selection: &Selection,
    ) -> Option<Vec<usize>> {
        let index = self.index(presentation);
        let mut rows = Vec::new();
        for row in selection.rows(index)? {
            if !selection.contains(index, row) {
                continue;
            }
            if index.rows[row].mapping_failed {
                return None;
            }
            if presentation == Presentation::Rendered && self.mapped_rendering() {
                rows.extend_from_slice(self.rendered.as_ref()?.mapping.raw_rows().get(row)?);
            } else {
                rows.push(row);
            }
        }
        if matches!(selection, Selection::Block { .. })
            && presentation == Presentation::Rendered
            && self.mapped_rendering()
            && let Some(block) = selection.row().and_then(|row| index.rows[row].block)
        {
            rows = index.block_raw_rows[block].clone();
        }
        rows.sort_unstable();
        rows.dedup();
        Some(rows)
    }

    pub fn selected_patch_rows(
        &self,
        presentation: Presentation,
        selection: &Selection,
    ) -> anyhow::Result<(&zess_git::FileDiff, Vec<RangeInclusive<PatchRow>>)> {
        use anyhow::{Context, ensure};
        if presentation == Presentation::Rendered {
            ensure!(
                self.mapped_rendering(),
                "diff has no reliable rendered mapping"
            );
        }
        let raw_rows = self
            .selection_raw_rows(presentation, selection)
            .context("selected row has no reliable original patch mapping")?;
        let file = self
            .file_at(
                *raw_rows
                    .first()
                    .context("this diff has no selected text lines")?,
            )
            .context("selected line has no Git file")?;
        ensure!(
            raw_rows.iter().all(|row| self
                .file_at(*row)
                .is_some_and(|candidate| candidate.target == file.target)),
            "cannot stage a selection across files"
        );
        let rows = if presentation == Presentation::Raw {
            vec![
                selection
                    .patch_rows(&self.index)
                    .context("selected rows have no patch identity")?,
            ]
        } else {
            raw_rows
                .into_iter()
                .map(|row| {
                    let row = self.index.rows[row]
                        .patch_row
                        .context("selected row has no patch identity")?;
                    Ok(row..=row)
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        };
        Ok((file, rows))
    }

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
                    patch_row: Some(PatchRow(index)),
                    old_line: None,
                    new_line: None,
                    patch_new_line: None,
                    source_line: None,
                    kind: None,
                    hunk: None,
                    block: None,
                    mapping_failed: false,
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
                    row.patch_new_line = Some(new);
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
            index: DiffIndex {
                rows,
                selectable,
                blocks,
                block_raw_rows: Vec::new(),
            },
            hunks,
            width,
            line_number_width,
            rendered: None,
        }
    }

    pub fn from_git(diff: zess_git::GitDiff, workspace: &Path) -> Self {
        let mut result = Self::new(String::new());
        result.source = Some(diff.source);
        result.files = diff.files;
        let external = result.files.iter().any(|file| file.rendered.is_some());
        let mut rendered = Rendered {
            rows: Vec::new(),
            styles: std::collections::HashMap::new(),
            width: 0,
            files: Vec::new(),
            text: String::new(),
            metadata: Vec::new(),
            mapping: RenderedMapping::DisplayOnly,
        };
        let mut valid_metadata = true;
        for (file, patch) in result.files.iter().enumerate() {
            let parsed = Self::new(patch.raw.clone());
            if external {
                let text = zess_ui::AnsiText::parse(
                    patch
                        .rendered
                        .as_ref()
                        .map(|rendered| rendered.text.as_str())
                        .unwrap_or(&patch.raw),
                );
                if patch
                    .rendered
                    .as_ref()
                    .is_some_and(|rendered| rendered.file_header)
                {
                    let title = match &patch.target.original_path {
                        Some(original) => format!("{original} → {}", patch.target.path),
                        None => patch.target.path.clone(),
                    };
                    let title = display_line(&title);
                    rendered.width = rendered.width.max(UnicodeWidthStr::width(title.as_str()));
                    rendered.text.push_str(&title);
                    rendered.text.push('\n');
                    rendered.rows.push(FrameRow {
                        spans: vec![zess_ui::Span {
                            text: title,
                            style: "ui.heading".into(),
                            base_style: None,
                        }],
                    });
                    rendered.files.push(FileIndex(file));
                    rendered.metadata.push(Vec::new());
                }
                valid_metadata &= !text.invalid_diff_metadata;
                for row in &text.rows {
                    for span in &row.spans {
                        rendered.text.push_str(&span.text);
                    }
                    rendered.text.push('\n');
                }
                rendered.metadata.extend(text.diff_metadata);
                rendered
                    .files
                    .extend(std::iter::repeat_n(FileIndex(file), text.rows.len()));
                rendered.width = rendered.width.max(
                    text.rows
                        .iter()
                        .map(|row| {
                            row.spans
                                .iter()
                                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                                .sum()
                        })
                        .max()
                        .unwrap_or(0),
                );
                rendered.rows.extend(text.rows);
                rendered.styles.extend(text.styles);
            }
            let row_offset = result.index.rows.len();
            let byte_offset = result.raw.len();
            let hunk_offset = result.hunks.len();
            let block_offset = result.index.blocks.len();
            result.raw.push_str(&patch.raw);
            for (local, mut row) in parsed.index.rows.into_iter().enumerate() {
                row.bytes = row.bytes.start + byte_offset..row.bytes.end + byte_offset;
                row.file = Some(FileIndex(file));
                row.patch_row = Some(PatchRow(local));
                row.hunk = row.hunk.map(|hunk| hunk + hunk_offset);
                row.block = row.block.map(|block| block + block_offset);
                result.index.rows.push(row);
            }
            result
                .hunks
                .extend(parsed.hunks.into_iter().map(|mut hunk| {
                    hunk.file = Some(FileIndex(file));
                    hunk
                }));
            result.index.blocks.extend(
                parsed
                    .index
                    .blocks
                    .into_iter()
                    .map(|block| block.start() + row_offset..=block.end() + row_offset),
            );
            result.index.selectable.extend(
                parsed
                    .index
                    .selectable
                    .into_iter()
                    .map(|row| SelectableRow(row.0 + row_offset)),
            );
            result.width = result.width.max(parsed.width);
            result.line_number_width = result.line_number_width.max(parsed.line_number_width);
        }
        if external {
            if valid_metadata {
                if let Some((index, raw_rows)) = result.rendered_index(&rendered, workspace) {
                    rendered.mapping = RenderedMapping::SourceMapped { index, raw_rows };
                }
            }
            result.rendered = Some(rendered);
        }
        result
    }

    pub fn matches_git(&self, diff: &zess_git::GitDiff) -> bool {
        self.source.as_ref() == Some(&diff.source)
            && self.files.len() == diff.files.len()
            && self.files.iter().zip(&diff.files).all(|(left, right)| {
                left.target == right.target
                    && left.raw == right.raw
                    && left.rendered == right.rendered
            })
    }

    pub fn display_rows(&self, presentation: Presentation) -> usize {
        match (presentation, &self.rendered) {
            (Presentation::Rendered, Some(rendered)) => rendered.rows.len(),
            _ => self.index.rows.len(),
        }
    }

    pub fn display_width(&self, presentation: Presentation) -> usize {
        match (presentation, &self.rendered) {
            (Presentation::Rendered, Some(rendered)) => rendered.width,
            _ => self.width,
        }
    }

    pub fn raw_selection_at_rendered(&self, row: usize) -> Selection {
        let Some(file) = self
            .rendered
            .as_ref()
            .and_then(|rendered| rendered.files.get(row))
        else {
            return self.index.initial_selection();
        };
        if self.mapped_rendering()
            && let Some(rendered) = &self.rendered
            && let Some(raw) = (row..rendered.rows.len())
                .chain((0..row).rev())
                .filter(|row| rendered.files[*row] == *file)
                .find_map(|row| rendered.mapping.raw_rows()[row].last().copied())
        {
            let raw = if self.index.rows[raw].block.is_some() {
                raw
            } else {
                self.index
                    .blocks
                    .iter()
                    .filter(|block| self.index.rows[*block.start()].file == Some(*file))
                    .min_by_key(|block| block.start().abs_diff(raw))
                    .map_or(raw, |block| *block.start())
            };
            return Selection::Block {
                head: self.index.selectable_at(raw),
            };
        }
        let head = self
            .index
            .blocks
            .iter()
            .find(|block| self.index.rows[*block.start()].file == Some(*file))
            .and_then(|block| self.index.selectable_at(*block.start()))
            .or_else(|| {
                self.index
                    .selectable
                    .iter()
                    .find(|head| self.index.rows[head.0].file == Some(*file))
                    .copied()
            });
        Selection::Block { head }
    }

    pub fn rendered_row_at_raw(&self, row: usize) -> usize {
        if let Some(rendered) = &self.rendered
            && let Some(row) = rendered
                .mapping
                .raw_rows()
                .iter()
                .position(|rows| rows.contains(&row))
        {
            return row;
        }
        let file = self.index.rows.get(row).and_then(|row| row.file);
        self.rendered
            .as_ref()
            .and_then(|rendered| {
                rendered
                    .files
                    .iter()
                    .position(|candidate| Some(*candidate) == file)
            })
            .unwrap_or(0)
    }

    pub fn reconcile_rendered_scroll(&self, old: &Self, row: usize) -> usize {
        let Some(previous) = old.rendered.as_ref() else {
            return 0;
        };
        let Some(file) = previous.files.get(row) else {
            return 0;
        };
        let target = &old.files[file.0].target;
        let offset = row
            - previous
                .files
                .iter()
                .position(|candidate| candidate == file)
                .unwrap();
        let Some(rendered) = &self.rendered else {
            return 0;
        };
        let Some(file) = self.files.iter().position(|file| &file.target == target) else {
            return 0;
        };
        let Some(start) = rendered
            .files
            .iter()
            .position(|candidate| candidate.0 == file)
        else {
            return 0;
        };
        let len = rendered.files[start..]
            .iter()
            .take_while(|candidate| candidate.0 == file)
            .count();
        start + offset.min(len.saturating_sub(1))
    }

    pub fn file_at(&self, row: usize) -> Option<&zess_git::FileDiff> {
        self.files.get(self.index.rows.get(row)?.file?.0)
    }

    pub fn line(&self, row: usize) -> &str {
        self.index
            .rows
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
                    .index
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

    pub fn search(
        &self,
        presentation: Presentation,
        pattern: &str,
        row: Option<usize>,
        forward: bool,
    ) -> anyhow::Result<Option<SelectableRow>> {
        let index = self.index(presentation);
        let text = if presentation == Presentation::Rendered && self.mapped_rendering() {
            &self.rendered.as_ref().unwrap().text
        } else {
            &self.raw
        };
        let crlf = text
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
        for found in regex.find_iter(text) {
            let found_row = index
                .rows
                .partition_point(|row| row.bytes.end <= found.start())
                .min(index.rows.len().saturating_sub(1));
            let Some(found_head) = index.selectable_at(found_row) else {
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
}

impl DiffIndex {
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
    pub patch_row: Option<PatchRow>,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    patch_new_line: Option<usize>,
    pub source_line: Option<usize>,
    pub kind: Option<LineKind>,
    pub hunk: Option<usize>,
    pub block: Option<usize>,
    mapping_failed: bool,
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

    /// Display bounds for scrolling; use `contains` for exact membership.
    pub fn rows(&self, diff: &DiffIndex) -> Option<RangeInclusive<usize>> {
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

    pub fn contains(&self, diff: &DiffIndex, row: usize) -> bool {
        if diff.selectable_at(row).is_none() {
            return false;
        }
        if matches!(self, Self::Block { .. })
            && let Some(block) = self.row().and_then(|head| diff.rows[head].block)
        {
            return diff.rows[row].block == Some(block);
        }
        self.rows(diff).is_some_and(|range| range.contains(&row))
    }

    pub fn patch_rows(&self, diff: &DiffIndex) -> Option<RangeInclusive<PatchRow>> {
        let range = self.rows(diff)?;
        Some(diff.rows[*range.start()].patch_row?..=diff.rows[*range.end()].patch_row?)
    }

    pub fn select_at(&mut self, diff: &DiffIndex, row: usize, extend: bool) -> bool {
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

    pub fn move_by(&mut self, diff: &DiffIndex, forward: bool, force_block: bool) {
        let Some(row) = self.row() else {
            self.set_head(diff.initial_selection().head());
            return;
        };
        if matches!(self, Self::Block { .. }) || force_block {
            let target = if let Some(current) = diff.rows[row].block {
                if forward {
                    diff.blocks.get(current + 1)
                } else {
                    current
                        .checked_sub(1)
                        .and_then(|block| diff.blocks.get(block))
                }
            } else if forward {
                diff.blocks.iter().find(|block| *block.start() > row)
            } else {
                diff.blocks.iter().rev().find(|block| *block.start() < row)
            };
            if let Some(block) = target {
                self.set_head(diff.selectable_at(*block.start()));
            }
        } else {
            self.move_by_lines(diff, if forward { 1 } else { -1 });
        }
    }

    pub fn move_by_lines(&mut self, diff: &DiffIndex, amount: isize) {
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

    pub fn move_to(&mut self, diff: &DiffIndex, row: usize) {
        self.set_head(if matches!(self, Self::Block { .. }) {
            diff.selectable_at(row)
                .filter(|head| diff.rows[head.0].block.is_some())
                .or_else(|| {
                    diff.blocks
                        .iter()
                        .find(|block| *block.end() >= row)
                        .or_else(|| diff.blocks.last())
                        .and_then(|block| diff.selectable_at(*block.start()))
                        .or_else(|| diff.nearest(row))
                })
        } else {
            diff.nearest(row)
        });
    }

    pub fn move_to_edge(&mut self, diff: &DiffIndex, forward: bool, count: Option<usize>) {
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

    pub fn focus_visible(&mut self, diff: &DiffIndex, visible: Range<usize>) {
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

    pub fn select_block(&mut self, diff: &DiffIndex) {
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

    pub fn reconcile(
        &mut self,
        old: &Diff,
        new: &Diff,
        old_presentation: Presentation,
        new_presentation: Presentation,
    ) -> bool {
        let old_index = old.index(old_presentation);
        let new_index = new.index(new_presentation);
        if matches!(self, Self::Line { head: None }) && !old_index.selectable.is_empty() {
            return false;
        }
        let previous = old.selection_raw_rows(old_presentation, self);
        if previous.is_none() && self.row().is_some() {
            *self = Self::Line { head: None };
            return true;
        }
        let mut identities = std::collections::HashMap::new();
        for head in &new.index.selectable {
            let row = &new.index.rows[head.0];
            identities
                .entry((
                    row.file.map(|file| &new.files[file.0].target),
                    row.old_line,
                    row.new_line,
                    new.line(head.0),
                ))
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some(head.0));
        }
        let matching_raw = |raw: usize| {
            let row = &old.index.rows[raw];
            identities
                .get(&(
                    row.file.map(|file| &old.files[file.0].target),
                    row.old_line,
                    row.new_line,
                    old.line(raw),
                ))
                .copied()
                .flatten()
        };
        if let Some(previous) = previous
            && !previous.is_empty()
            && let Some(mut raw_rows) = previous
                .into_iter()
                .map(matching_raw)
                .collect::<Option<Vec<_>>>()
        {
            raw_rows.sort_unstable();
            raw_rows.dedup();
            let mut positions =
                if new_presentation == Presentation::Rendered && new.mapped_rendering() {
                    let mut positions: Vec<Option<usize>> = vec![None; raw_rows.len()];
                    let previous = self.row().unwrap_or(0);
                    for (display_row, rows) in new
                        .rendered
                        .as_ref()
                        .unwrap()
                        .mapping
                        .raw_rows()
                        .iter()
                        .enumerate()
                    {
                        for row in rows {
                            if let Ok(index) = raw_rows.binary_search(row)
                                && positions[index].is_none_or(|row| {
                                    display_row.abs_diff(previous) < row.abs_diff(previous)
                                })
                            {
                                positions[index] = Some(display_row);
                            }
                        }
                    }
                    positions.into_iter().flatten().collect()
                } else {
                    raw_rows.clone()
                };
            positions.sort_unstable();
            positions.dedup();
            let first = positions
                .first()
                .and_then(|row| new_index.selectable_at(*row));
            let last = positions
                .last()
                .and_then(|row| new_index.selectable_at(*row));
            let candidate = match (first, last) {
                (Some(head), _) if matches!(self, Self::Block { .. }) => {
                    Self::Block { head: Some(head) }
                }
                (Some(first), Some(last))
                    if first.0 != last.0 || matches!(self, Self::Range { .. }) =>
                {
                    let reverse = matches!(self, Self::Range { anchor, head } if anchor.0 > head.0);
                    if reverse {
                        Self::Range {
                            anchor: last,
                            head: first,
                        }
                    } else {
                        Self::Range {
                            anchor: first,
                            head: last,
                        }
                    }
                }
                (Some(head), _) => Self::Line { head: Some(head) },
                _ => Self::Line { head: None },
            };
            if new
                .selection_raw_rows(new_presentation, &candidate)
                .as_ref()
                == Some(&raw_rows)
            {
                *self = candidate;
                return false;
            }
            *self = Self::Line { head: None };
            return true;
        }
        if old.raw == new.raw && self.row().is_some() {
            *self = Self::Line { head: None };
            return true;
        }
        let project = |raw: usize, previous: usize| {
            if new_presentation == Presentation::Rendered && new.mapped_rendering() {
                new.rendered
                    .as_ref()?
                    .mapping
                    .raw_rows()
                    .iter()
                    .enumerate()
                    .filter(|(_, rows)| rows.contains(&raw))
                    .min_by_key(|(row, _)| row.abs_diff(previous))
                    .and_then(|(row, _)| new_index.selectable_at(row))
            } else {
                new_index.selectable_at(raw)
            }
        };
        let map = |head: SelectableRow| {
            let raw = old.raw_row(old_presentation, head.0)?;
            let previous = &old.index.rows[raw];
            let same_file = |row: usize| {
                previous.file.map(|file| &old.files[file.0].target)
                    == new.index.rows[row]
                        .file
                        .map(|file| &new.files[file.0].target)
            };
            let raw = matching_raw(raw)
                .or_else(|| {
                    previous.block.and_then(|_| {
                        new.index
                            .blocks
                            .iter()
                            .filter(|block| same_file(*block.start()))
                            .min_by_key(|block| block.start().abs_diff(raw))
                            .map(|block| *block.start())
                    })
                })
                .or_else(|| {
                    new.index
                        .selectable
                        .iter()
                        .filter(|row| same_file(row.0))
                        .min_by_key(|row| row.0.abs_diff(raw))
                        .map(|row| row.0)
                })?;
            project(raw, head.0)
        };
        let head = self
            .head()
            .and_then(map)
            .or_else(|| new_index.initial_selection().head());
        *self = match *self {
            Self::Block { .. } => Self::Block { head },
            Self::Line { .. } => Self::Line { head },
            Self::Range { anchor, .. } => match (map(anchor), head) {
                (Some(anchor), Some(head)) => Self::Range { anchor, head },
                _ => Self::Line { head },
            },
        };
        if matches!(self, Self::Block { .. })
            && self
                .row()
                .is_some_and(|row| new_index.rows[row].block.is_none())
        {
            self.select_nearest_block(new_index);
        }
        false
    }

    fn select_nearest_block(&mut self, diff: &DiffIndex) {
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

    pub fn toggle(&mut self, diff: &DiffIndex) {
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
    pub scroll_mode: super::ScrollMode,
}

impl Search {
    pub fn restore(&self, view: &mut super::View) {
        view.location.diff_selection = self.selection.clone();
        view.location.scroll = self.scroll;
        view.location.horizontal_scroll = self.horizontal_scroll;
        view.location.scroll_mode = self.scroll_mode;
    }
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
