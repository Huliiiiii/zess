use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_ui::{DisplayRow, FrameRow, Span, clip_spans, fit};

use crate::{CommandCatalog, CommandInfo, CommandLineEntry, Keymap, ResolvedBinding};

pub struct Completion {
    pub prefix: String,
    pub index: usize,
}

#[derive(PartialEq)]
pub struct CommandCandidate {
    pub name: String,
    description: String,
    aliases: Vec<String>,
    matches: Vec<usize>,
}

impl CommandCandidate {
    fn new(command: &CommandInfo, entry: &CommandLineEntry, matches: Vec<usize>) -> Self {
        Self {
            name: entry.name.clone(),
            description: command.description.clone(),
            aliases: entry.aliases.clone(),
            matches,
        }
    }

    fn documentation(&self) -> Vec<(String, &'static str)> {
        let mut lines = vec![(self.description.clone(), "popup")];
        if !self.aliases.is_empty() {
            lines.push((
                format!("Aliases: {}", self.aliases.join(", ")),
                "popup.secondary",
            ));
        }
        lines
    }
}

pub fn command_candidates(catalog: &CommandCatalog, prefix: &str) -> Vec<CommandCandidate> {
    static MATCHER: LazyLock<Mutex<Matcher>> =
        LazyLock::new(|| Mutex::new(Matcher::new(Config::DEFAULT)));
    let pattern = Atom::new(
        prefix,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );
    let mut matcher = MATCHER.lock().unwrap();
    let mut buffer = Vec::new();
    let mut commands = catalog
        .command_line_entries()
        .filter_map(|(command, entry)| {
            std::iter::once(entry.name.as_str())
                .chain(entry.aliases.iter().map(String::as_str))
                .filter_map(|name| {
                    let mut indices = Vec::new();
                    let score = pattern.indices(
                        Utf32Str::new(name, &mut buffer),
                        &mut matcher,
                        &mut indices,
                    )?;
                    let priority = if !prefix.is_empty() && name == prefix {
                        0
                    } else if !prefix.is_empty() && name != entry.name && name.starts_with(prefix) {
                        1
                    } else {
                        2
                    };
                    Some(((priority, Reverse(score)), name, indices))
                })
                .min_by_key(|(rank, _, _)| *rank)
                .map(|(rank, name, mut indices)| {
                    if name != entry.name {
                        indices.clear();
                        pattern.indices(
                            Utf32Str::new(&entry.name, &mut buffer),
                            &mut matcher,
                            &mut indices,
                        );
                    }
                    (
                        rank,
                        CommandCandidate::new(
                            command,
                            entry,
                            indices.into_iter().map(|index| index as usize).collect(),
                        ),
                    )
                })
        })
        .collect::<Vec<_>>();
    commands.sort_by_key(|(rank, _)| *rank);
    commands
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

fn grid_cell_width(entries: &[CommandCandidate], width: usize) -> usize {
    entries
        .iter()
        .map(|candidate| UnicodeWidthStr::width(candidate.name.as_str()) + 2)
        .max()
        .unwrap_or(1)
        .min(width)
        .max(1)
}

pub enum CompletionDirection {
    Next,
    Previous,
    Up,
    Down,
}

pub fn complete_command(
    catalog: &CommandCatalog,
    text: &mut String,
    completion: &mut Option<Completion>,
    direction: CompletionDirection,
    width: usize,
) {
    let prefix = completion
        .as_ref()
        .map_or(text.as_str(), |selection| selection.prefix.as_str());
    if prefix.contains(char::is_whitespace) {
        return;
    }
    let candidates = command_candidates(catalog, prefix);
    if candidates.is_empty() {
        return;
    }
    let steps = match direction {
        CompletionDirection::Up | CompletionDirection::Down => {
            (width / grid_cell_width(&candidates, width)).max(1)
        }
        CompletionDirection::Next | CompletionDirection::Previous => 1,
    };
    let current = completion.as_ref().map_or(0, |selection| selection.index);
    let index = match direction {
        CompletionDirection::Previous | CompletionDirection::Up => {
            (current + candidates.len() - steps % candidates.len()) % candidates.len()
        }
        CompletionDirection::Next | CompletionDirection::Down => {
            (current + steps) % candidates.len()
        }
    };
    let prefix = prefix.to_owned();
    *text = candidates[index].name.clone();
    *completion = Some(Completion { prefix, index });
}

pub fn render_key_hints(
    rows: &mut [FrameRow],
    width: usize,
    title: &str,
    entries: &[(String, String)],
) {
    if entries.is_empty() || rows.len() < 3 || width < 8 {
        return;
    }
    let count = entries.len().min(rows.len() - 2).min(12);
    let key_width = entries
        .iter()
        .map(|(key, _)| UnicodeWidthStr::width(key.as_str()))
        .max()
        .unwrap_or(0)
        .min(20);
    let popup_width = (entries
        .iter()
        .map(|(_, label)| key_width + 2 + UnicodeWidthStr::width(label.as_str()) + 2)
        .max()
        .unwrap_or(0))
    .max(UnicodeWidthStr::width(title) + 2)
    .min(width)
    .min(64);
    let x = width - popup_width;
    let y = rows.len() - count - 2;
    for offset in 0..count + 1 {
        let (text, style) = if offset == 0 {
            (title.to_owned(), "status")
        } else {
            let (key, label) = &entries[offset - 1];
            (format!("{}  {label}", fit(key, key_width)), "popup")
        };
        rows[y + offset].spans = clip_spans(std::mem::take(&mut rows[y + offset].spans), x);
        rows[y + offset].spans.push(Span {
            base_style: None,
            text: format!(" {} ", fit(&text, popup_width - 2)),
            style: style.into(),
        });
    }
}

#[derive(PartialEq)]
pub struct CommandGrid {
    pub entries: Vec<CommandCandidate>,
    pub prefix: String,
    selected: Option<usize>,
    documentation: Vec<(String, &'static str)>,
    cell: usize,
    columns: usize,
    count: usize,
    start: usize,
    y: usize,
}

impl CommandGrid {
    pub fn new(
        catalog: &CommandCatalog,
        text: &str,
        completion: Option<&Completion>,
        width: usize,
        height: usize,
    ) -> Self {
        let prefix = completion.map_or(text, |completion| completion.prefix.as_str());
        let entries = if prefix.contains(char::is_whitespace) {
            Vec::new()
        } else {
            command_candidates(catalog, prefix)
        };
        let selected =
            (!entries.is_empty()).then(|| completion.map_or(0, |completion| completion.index));
        let documentation = selected
            .and_then(|index| entries.get(index))
            .map(CommandCandidate::documentation)
            .or_else(|| {
                let name = text.split_whitespace().next()?;
                let command = catalog.resolve_command_line(name)?;
                Some(
                    CommandCandidate::new(command, command.command_line.as_ref()?, Vec::new())
                        .documentation(),
                )
            })
            .unwrap_or_default();
        let documentation = if width == 0 {
            Vec::new()
        } else {
            documentation
                .into_iter()
                .flat_map(|(text, style)| {
                    let mut lines = Vec::new();
                    for line in text.lines() {
                        let mut text = String::new();
                        let mut used = 0;
                        for grapheme in line.graphemes(true) {
                            let size = UnicodeWidthStr::width(grapheme);
                            if used + size > width && !text.is_empty() {
                                lines.push((std::mem::take(&mut text), style));
                                used = 0;
                            }
                            text.push_str(grapheme);
                            used += size;
                        }
                        lines.push((text, style));
                    }
                    lines
                })
                .take(height.saturating_sub(usize::from(!entries.is_empty())))
                .collect()
        };
        let cell = grid_cell_width(&entries, width);
        let columns = (width / cell).max(1);
        let count = if width == 0 {
            0
        } else {
            entries
                .len()
                .div_ceil(columns)
                .min(height - documentation.len())
                .min(8)
        };
        let start =
            (selected.unwrap_or(0) / columns).saturating_sub(count.saturating_sub(1)) * columns;
        let y = height - count - documentation.len();
        CommandGrid {
            entries,
            prefix: prefix.to_owned(),
            selected,
            documentation,
            cell,
            columns,
            count,
            start,
            y,
        }
    }

    pub fn hit(&self, x: usize, y: usize, width: usize) -> Option<Option<usize>> {
        if x >= width || y < self.y || y >= self.y + self.documentation.len() + self.count {
            return None;
        }
        if y < self.y + self.documentation.len() {
            return Some(None);
        }
        let column = x / self.cell;
        let index = self.start + (y - self.y - self.documentation.len()) * self.columns + column;
        Some((column < self.columns && index < self.entries.len()).then_some(index))
    }

    pub fn render(&self, rows: &mut [FrameRow], width: usize) {
        for (row, (text, style)) in rows[self.y..].iter_mut().zip(&self.documentation) {
            row.spans = vec![Span {
                base_style: None,
                text: fit(text, width),
                style: (*style).into(),
            }];
        }
        for offset in 0..self.count {
            let mut spans = Vec::<Span>::new();
            for column in 0..self.columns {
                let index = self.start + offset * self.columns + column;
                if let Some(candidate) = self.entries.get(index) {
                    let base = if self.selected == Some(index) {
                        "selection"
                    } else {
                        "popup"
                    };
                    let mut used = 0;
                    for (index, grapheme) in candidate.name.graphemes(true).enumerate() {
                        if grapheme.chars().any(char::is_control) {
                            continue;
                        }
                        let size = UnicodeWidthStr::width(grapheme);
                        if used + size > self.cell {
                            break;
                        }
                        used += size;
                        let style = if candidate.matches.contains(&index) {
                            format!("{base}.match")
                        } else {
                            base.into()
                        };
                        if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
                            last.text.push_str(grapheme);
                        } else {
                            spans.push(Span {
                                base_style: None,
                                text: grapheme.into(),
                                style,
                            });
                        }
                    }
                    if used < self.cell {
                        spans.push(Span {
                            base_style: None,
                            text: " ".repeat(self.cell - used),
                            style: "popup".into(),
                        });
                    }
                }
            }
            rows[self.y + self.documentation.len() + offset].spans = clip_spans(spans, width);
        }
    }
}

pub fn binding_entries(
    commands: &CommandCatalog,
    bindings: Vec<ResolvedBinding<'_>>,
    prefix: &[String],
) -> Vec<(String, String)> {
    let mut entries = Vec::<(String, String)>::new();
    let mut groups = BTreeMap::<_, usize>::new();
    for binding in bindings {
        if !binding.keys.starts_with(prefix) {
            continue;
        }
        let command = commands.resolve(binding.command);
        let identity = command.map_or(binding.command, |command| command.id.as_str());
        let key = binding.keys[prefix.len()..].join(" ");
        if let Some(index) = groups.get(&(binding.scope, identity)).copied() {
            let (keys, _) = &mut entries[index];
            keys.push_str(", ");
            keys.push_str(&key);
        } else {
            groups.insert((binding.scope, identity), entries.len());
            entries.push((
                key,
                command
                    .map_or(binding.command, |command| command.description.as_str())
                    .to_owned(),
            ));
        }
    }
    entries
}

pub fn help_rows(
    commands: &CommandCatalog,
    keymap: &Keymap,
    scopes: &[&str],
    mode: &str,
) -> Vec<DisplayRow> {
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for binding in keymap.effective_bindings(scopes, mode) {
        groups.entry(binding.scope).or_default().push(binding);
    }
    let mut rows = Vec::new();
    for scope in scopes.iter().copied().rev() {
        let Some(scope_id) = keymap.identities().scope(scope) else {
            continue;
        };
        let Some(bindings) = groups.remove(&scope_id) else {
            continue;
        };
        rows.push(DisplayRow {
            text: keymap.scope_label(scope).to_owned(),
            style: "status".into(),
            ..DisplayRow::default()
        });
        rows.extend(
            binding_entries(commands, bindings, &[])
                .into_iter()
                .map(|(key, label)| DisplayRow {
                    id: key.clone(),
                    text: format!(
                        "{key}{} {label}",
                        " ".repeat(20usize.saturating_sub(key.width()))
                    ),
                    ..DisplayRow::default()
                }),
        );
    }
    rows
}
