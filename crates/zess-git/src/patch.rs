use std::ops::RangeInclusive;

use anyhow::{Context, Result, bail, ensure};

use crate::DiffSide;

/// Zero-based row in the exact raw patch shown to the user.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
pub struct PatchRow(pub usize);

#[derive(Clone, Copy, PartialEq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
    NoNewline,
}

pub struct PatchLine<'a> {
    pub row: PatchRow,
    pub kind: LineKind,
    pub raw: &'a str,
}

pub struct Hunk<'a> {
    pub row: PatchRow,
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<PatchLine<'a>>,
}

impl Hunk<'_> {
    pub fn rows(&self) -> RangeInclusive<PatchRow> {
        self.row..=self.lines.last().map_or(self.row, |line| line.row)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FileChange {
    Modified,
    Added,
    Deleted,
}

struct FilePatch<'a> {
    change: FileChange,
    header: Vec<&'a str>,
    hunks: Vec<Hunk<'a>>,
}

pub struct RawPatch<'a> {
    files: Vec<FilePatch<'a>>,
}

impl<'a> RawPatch<'a> {
    /// Parse Git's unified patch grammar; hunk lengths delimit content, so file
    /// contents resembling headers cannot become metadata.
    pub fn parse(raw: &'a str) -> Result<Self> {
        let mut files: Vec<FilePatch<'a>> = Vec::new();
        let mut lines = raw.split_inclusive('\n').enumerate().peekable();
        while let Some((row, raw)) = lines.next() {
            let line = raw.trim_end_matches('\n');
            if line.starts_with("diff --git ") {
                files.push(FilePatch {
                    change: FileChange::Modified,
                    header: vec![raw],
                    hunks: Vec::new(),
                });
                continue;
            }
            // Read-only diff consumers can also supply a unified hunk fragment.
            if files.is_empty() && line.starts_with("@@ -") {
                files.push(FilePatch {
                    change: FileChange::Modified,
                    header: Vec::new(),
                    hunks: Vec::new(),
                });
            }
            let file = files.last_mut().context("patch has no Git file header")?;
            let Some(ranges) = line.strip_prefix("@@ -") else {
                ensure!(file.hunks.is_empty(), "unexpected data after patch hunk");
                if line.starts_with("new file mode ") {
                    file.change = FileChange::Added;
                }
                if line.starts_with("deleted file mode ") {
                    file.change = FileChange::Deleted;
                }
                file.header.push(raw);
                continue;
            };
            let (old, ranges) = ranges.split_once(" +").context("invalid old hunk range")?;
            let (new, _) = ranges.split_once(" @@").context("invalid new hunk range")?;
            let (old_start, mut old_left) = parse_range(old)?;
            let (new_start, mut new_left) = parse_range(new)?;
            let mut hunk = Hunk {
                row: PatchRow(row),
                old_start,
                new_start,
                lines: Vec::new(),
            };
            while old_left != 0
                || new_left != 0
                || lines.peek().is_some_and(|(_, line)| line.starts_with('\\'))
            {
                let (row, raw) = lines.next().context("incomplete patch hunk")?;
                let kind = match raw.as_bytes().first() {
                    Some(b' ') => {
                        old_left = old_left.checked_sub(1).context("excess old context")?;
                        new_left = new_left.checked_sub(1).context("excess new context")?;
                        LineKind::Context
                    }
                    Some(b'-') => {
                        old_left = old_left.checked_sub(1).context("excess deletion")?;
                        LineKind::Deletion
                    }
                    Some(b'+') => {
                        new_left = new_left.checked_sub(1).context("excess addition")?;
                        LineKind::Addition
                    }
                    Some(b'\\') if raw.trim_end_matches('\n') == "\\ No newline at end of file" => {
                        LineKind::NoNewline
                    }
                    _ => bail!("invalid patch content line"),
                };
                hunk.lines.push(PatchLine {
                    row: PatchRow(row),
                    kind,
                    raw,
                });
            }
            file.hunks.push(hunk);
        }
        Ok(Self { files })
    }

    pub fn hunks(&self) -> impl Iterator<Item = &Hunk<'a>> {
        self.files.iter().flat_map(|file| &file.hunks)
    }

    pub fn selection(&self, side: DiffSide, rows: &[RangeInclusive<PatchRow>]) -> Result<String> {
        let selected = |row| rows.iter().any(|range| range.contains(&row));
        let reverse = side == DiffSide::Staged;
        let mut result = String::new();
        for file in &self.files {
            let changes = file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| matches!(line.kind, LineKind::Addition | LineKind::Deletion))
                .collect::<Vec<_>>();
            if !changes.iter().any(|line| selected(line.row)) {
                continue;
            }
            ensure!(
                file.header.iter().any(|line| line.starts_with("--- "))
                    && file.header.iter().any(|line| line.starts_with("+++ ")),
                "staging requires a complete raw Git patch"
            );
            let creates_index_file = matches!(
                (file.change, side),
                (FileChange::Added, DiffSide::Unstaged) | (FileChange::Deleted, DiffSide::Staged)
            );
            if creates_index_file
                || (file.change != FileChange::Modified
                    && changes.iter().all(|line| selected(line.row)))
            {
                for header in &file.header {
                    result.push_str(header);
                }
            } else {
                // Selecting content keeps an existing index file's mode, even
                // when every content change is selected. Creation/deletion
                // metadata is only needed when the index file itself changes.
                let old = file
                    .header
                    .iter()
                    .find_map(|line| line.strip_prefix("--- "))
                    .context("patch has no old path")?;
                let new = file
                    .header
                    .iter()
                    .find_map(|line| line.strip_prefix("+++ "))
                    .context("patch has no new path")?;
                let old = if old == "/dev/null\n" {
                    opposite_path(new, 'a')?
                } else {
                    old.into()
                };
                let new = if new == "/dev/null\n" {
                    opposite_path(&old, 'b')?
                } else {
                    new.into()
                };
                result.push_str(&format!("--- {old}+++ {new}"));
            }
            let mut offset = 0isize;
            for hunk in &file.hunks {
                if !hunk.lines.iter().any(|line| {
                    selected(line.row)
                        && matches!(line.kind, LineKind::Addition | LineKind::Deletion)
                }) {
                    continue;
                }
                let mut body = String::new();
                let mut old_count = 0usize;
                let mut new_count = 0usize;
                let mut deferred_context = String::new();
                let mut skipped_new = false;
                let mut lines = hunk.lines.iter().peekable();
                while let Some(line) = lines.next() {
                    let newline = if lines
                        .peek()
                        .is_some_and(|line| line.kind == LineKind::NoNewline)
                    {
                        lines.next().map(|line| line.raw).unwrap_or_default()
                    } else {
                        ""
                    };
                    let old_side = matches!(
                        (line.kind, reverse),
                        (LineKind::Deletion, false) | (LineKind::Addition, true)
                    );
                    let selected = selected(line.row);
                    let kind = if selected || line.kind == LineKind::Context {
                        line.kind
                    } else if old_side {
                        LineKind::Context
                    } else {
                        skipped_new = true;
                        continue;
                    };
                    old_count +=
                        usize::from(matches!(kind, LineKind::Context | LineKind::Deletion));
                    new_count +=
                        usize::from(matches!(kind, LineKind::Context | LineKind::Addition));
                    if !selected && old_side {
                        deferred_context.push(' ');
                        deferred_context.push_str(&line.raw[1..]);
                        deferred_context.push_str(newline);
                        continue;
                    }
                    // Retained old lines follow selected insertions in a replacement
                    // block. A skipped insertion splits that block, fixing the next
                    // selected insertion after those retained lines instead.
                    if line.kind == LineKind::Context || old_side || skipped_new {
                        body.push_str(&std::mem::take(&mut deferred_context));
                    }
                    if line.kind == LineKind::Context {
                        skipped_new = false;
                    }
                    body.push_str(line.raw);
                    body.push_str(newline);
                }
                body.push_str(&deferred_context);
                let (old_start, new_start) = if reverse {
                    let new_start = hunk.new_start;
                    let old_start = new_start.saturating_add_signed(
                        -offset + isize::from(new_count == 0) - isize::from(old_count == 0),
                    );
                    (old_start, new_start)
                } else {
                    let old_start = hunk.old_start;
                    let new_start = old_start.saturating_add_signed(
                        offset + isize::from(old_count == 0) - isize::from(new_count == 0),
                    );
                    (old_start, new_start)
                };
                result.push_str(&format!(
                    "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n{body}"
                ));
                offset += new_count as isize - old_count as isize;
            }
        }
        ensure!(
            !result.is_empty(),
            "select an added or deleted line, or press a to select its hunk"
        );
        Ok(result)
    }
}

fn parse_range(range: &str) -> Result<(usize, usize)> {
    match range.split_once(',') {
        Some((start, count)) => Ok((start.parse()?, count.parse()?)),
        None => Ok((range.parse()?, 1)),
    }
}

fn opposite_path(path: &str, side: char) -> Result<String> {
    let (quote, path) = if let Some(path) = path.strip_prefix('"') {
        ("\"", path)
    } else {
        ("", path)
    };
    ensure!(
        path.starts_with(['a', 'b']) && path.as_bytes().get(1) == Some(&b'/'),
        "patch path has no canonical prefix"
    );
    Ok(format!("{quote}{side}{}", &path[1..]))
}
