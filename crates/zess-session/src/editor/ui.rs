use super::commands::{DataCommand, HelpCommand};
use std::cmp::Reverse;
use std::collections::BTreeMap;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_plugin::{CommandCatalog, CommandInfo, CommandLineEntry, ResolvedBinding};
use zess_ui::{FrameRow, Span};

use super::{Buffer, DisplayRow, Prompt, PromptKind, RegisterInput, Session, fit};

impl Prompt {
    pub(super) fn render_input(&self) -> (String, usize) {
        let (prefix, text, cursor) = if let Some((kind, query)) = self.query() {
            let purpose = match self.kind {
                PromptKind::Filter(_) => "Filter",
                _ => "Find",
            };
            let prefix = match kind {
                zess_filter::QueryKind::Plain => format!("{purpose}: "),
                zess_filter::QueryKind::Glob => format!("{purpose} (glob): "),
                zess_filter::QueryKind::Regex => format!("{purpose} (regex): "),
            };
            (prefix, query, self.cursor)
        } else {
            let prefix = match &self.kind {
                PromptKind::Command => ":".to_owned(),
                PromptKind::Text(kind) => format!("{}: ", kind.label()),
                PromptKind::DiffSearch(search) => if search.forward { "/" } else { "?" }.to_owned(),
                PromptKind::Filter(_) => unreachable!("filters have a query"),
                PromptKind::MiniBuffer { purpose, .. } => format!("{purpose}: "),
            };
            (prefix, self.text.as_str(), self.cursor)
        };
        let column =
            UnicodeWidthStr::width(prefix.as_str()) + UnicodeWidthStr::width(&text[..cursor]);
        (format!("{prefix}{text}"), column)
    }
}

pub(super) struct Completion {
    prefix: String,
    index: usize,
}

pub(super) struct CommandCandidate {
    pub name: String,
    description: String,
    aliases: Vec<String>,
    label: String,
    matches: Vec<usize>,
}

impl CommandCandidate {
    fn new(command: &CommandInfo, entry: &CommandLineEntry, matches: Vec<usize>) -> Self {
        Self {
            name: entry.name.clone(),
            description: command.description.clone(),
            aliases: entry.aliases.clone(),
            label: entry.name.clone(),
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

pub(super) struct Help {
    pub view: u64,
    pub source: super::pages::FocusTarget,
}

#[derive(Clone)]
pub(super) struct HelpContext {
    scopes: Vec<String>,
    mode: String,
}

impl Session {
    fn help_rows(&self, context: &HelpContext) -> Vec<DisplayRow> {
        let Some(scripts) = &self.scripts else {
            return Vec::new();
        };
        let scopes = context
            .scopes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        scripts.with_keymap(|keymap| {
            scripts.with_commands(|commands| {
                let mut groups = BTreeMap::<_, Vec<_>>::new();
                for binding in keymap.effective_bindings(&scopes, &context.mode) {
                    groups.entry(binding.scope).or_default().push(binding);
                }
                let mut rows = Vec::new();
                for scope in scopes.into_iter().rev() {
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
                    rows.extend(binding_entries(commands, bindings, &[]).into_iter().map(
                        |(key, label)| DisplayRow {
                            id: key.clone(),
                            text: format!("{key:20} {label}"),
                            ..DisplayRow::default()
                        },
                    ));
                }
                rows
            })
        })
    }

    pub(super) fn command_candidates(&self, prefix: &str) -> Vec<CommandCandidate> {
        self.scripts
            .as_ref()
            .map(|scripts| {
                scripts.with_commands(|catalog| {
                    let pattern = Atom::new(
                        prefix,
                        CaseMatching::Smart,
                        Normalization::Smart,
                        AtomKind::Fuzzy,
                        false,
                    );
                    let mut matcher = helix_core::fuzzy::MATCHER.lock();
                    matcher.config = Config::DEFAULT;
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
                                    let priority = if !prefix.is_empty() && name != entry.name {
                                        if name == prefix {
                                            0
                                        } else if name.starts_with(prefix) {
                                            1
                                        } else {
                                            2
                                        }
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
                                            indices
                                                .into_iter()
                                                .map(|index| index as usize)
                                                .collect(),
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
                })
            })
            .unwrap_or_default()
    }

    pub(super) fn command_grid_columns(&self, prompt: &Prompt) -> usize {
        let prefix = prompt
            .completion
            .as_ref()
            .map_or(prompt.text.as_str(), |completion| {
                completion.prefix.as_str()
            });
        let candidates = self.command_candidates(prefix);
        let cell = candidates
            .iter()
            .map(|candidate| UnicodeWidthStr::width(candidate.label.as_str()) + 2)
            .max()
            .unwrap_or(1)
            .min(usize::from(self.width))
            .max(1);
        (usize::from(self.width) / cell).max(1)
    }

    pub(super) fn complete_command(&self, prompt: &mut Prompt, backwards: bool, steps: usize) {
        let prefix = prompt
            .completion
            .as_ref()
            .map_or(prompt.text.as_str(), |completion| {
                completion.prefix.as_str()
            });
        if prefix.contains(char::is_whitespace) {
            return;
        }
        let candidates = self.command_candidates(prefix);
        if candidates.is_empty() {
            return;
        }
        let index = match &prompt.completion {
            Some(completion) if backwards => {
                (completion.index + candidates.len() - steps % candidates.len()) % candidates.len()
            }
            Some(completion) => (completion.index + steps) % candidates.len(),
            None if backwards => candidates.len() - 1,
            None => 0,
        };
        let prefix = prefix.to_owned();
        prompt.text = candidates[index].name.clone();
        prompt.completion = Some(Completion { prefix, index });
    }

    pub(super) fn show_help(&mut self) {
        if self.help.is_some()
            || matches!(&self.buffers[&self.views[&self.focus].location.buffer],Buffer::Items { kind,.. } if kind == "help")
        {
            return;
        }
        let source = self.focus;
        let (scopes, mode) = self.key_context();
        let context = HelpContext {
            scopes: scopes.into_iter().map(str::to_owned).collect(),
            mode: mode.to_owned(),
        };
        let source_view = &self.views[&source];
        let title = match &self.buffers[&source_view.location.buffer] {
            Buffer::Text(_) | Buffer::Snapshot { .. } => "Text".to_owned(),
            Buffer::PreviewText { .. } => "Preview".into(),
            Buffer::PreviewDiff { .. } => "Diff".into(),
            Buffer::PreviewRows { .. } => "Preview".into(),
            Buffer::Data { kind, .. } | Buffer::Items { kind, .. } => {
                if source_view.location.display.title.is_empty() {
                    kind.clone()
                } else {
                    source_view.location.display.title.clone()
                }
            }
        };
        let display = super::Display {
            title: format!("Help: {title}"),
            header: format!("Help: {title}"),
            rows: self.help_rows(&context),
            ..super::Display::default()
        };
        let buffer = self.add_buffer(Buffer::Items {
            kind: "help".into(),
            path: String::new(),
            display,
            help_context: Some(context),
        });
        let view = self.create_page(buffer);
        let source = self.views.focus_target(source);
        self.views.get_mut(&view).unwrap().return_to = Some(source);
        self.pending.clear();
        self.activate_view(view);
        self.help = Some(Help { view, source });
    }

    pub(super) fn refresh_help_buffers(&mut self) {
        let contexts = self
            .buffers
            .iter()
            .filter_map(|(id, buffer)| match buffer {
                Buffer::Items {
                    help_context: Some(context),
                    ..
                } => Some((*id, context.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (id, context) in contexts {
            let rows = self.help_rows(&context);
            if let Buffer::Items { display, .. } = self.buffers.get_mut(&id).unwrap() {
                display.rows = rows;
                for view in self
                    .views
                    .values_mut()
                    .filter(|view| view.location.buffer == id)
                {
                    view.project_items(display);
                }
            }
        }
    }

    pub(super) fn close_help_overlay(&mut self) {
        if let Some(help) = self.help.take() {
            if self
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.view == help.view)
            {
                self.prompt = None;
            }
            let buffer = self.views[&help.view].location.buffer;
            self.retire_page(self.views.page_id(help.view));
            self.buffers.remove(&buffer);
            if self.focus == help.view {
                let focus = self
                    .views
                    .resolve_focus(help.source)
                    .unwrap_or_else(|| self.first_layout_view());
                self.activate_view(focus);
            }
        }
    }

    pub(super) fn help_command(&mut self, command: HelpCommand) -> anyhow::Result<()> {
        match command {
            HelpCommand::Close => {
                if self.help.is_some() {
                    self.close_help_overlay();
                    self.pending.clear();
                } else {
                    self.close_view(false)?;
                }
            }
            HelpCommand::Pin => {
                if self.fullscreen_layer().is_some() {
                    anyhow::bail!("close the fullscreen layer before pinning help");
                }
                if let Some(help) = self.help.take() {
                    let mut root = self
                        .views
                        .resolve_focus(help.source)
                        .unwrap_or_else(|| self.first_layout_view());
                    while let Some(parent) = self.views[&root].parent {
                        root = parent;
                    }
                    self.layout.as_mut().unwrap().split(
                        self.views.page_id(root),
                        self.views.page_id(help.view),
                        super::Axis::Horizontal,
                    );
                    self.activate_view(help.view);
                }
            }
            HelpCommand::ScrollDown => self.data_command(DataCommand::MoveDown)?,
            HelpCommand::ScrollUp => self.data_command(DataCommand::MoveUp)?,
            HelpCommand::PageHalfDown => self.data_command(DataCommand::PageHalfDown)?,
            HelpCommand::PageHalfUp => self.data_command(DataCommand::PageHalfUp)?,
        }
        Ok(())
    }
}

fn overlay(
    rows: &mut [FrameRow],
    width: usize,
    title: &str,
    entries: &[(String, String)],
    selected: Option<usize>,
    scroll: usize,
) {
    if entries.is_empty() || rows.len() < 3 || width < 8 {
        return;
    }
    let count = entries.len().min(rows.len() - 2).min(12);
    let scroll = selected
        .map_or(scroll, |index| index.saturating_sub(count - 1))
        .min(entries.len().saturating_sub(count));
    let key_width = entries
        .iter()
        .map(|(key, _)| UnicodeWidthStr::width(key.as_str()))
        .max()
        .unwrap_or(0)
        .min(20);
    let popup_width = (entries
        .iter()
        .map(|(_, label)| key_width + UnicodeWidthStr::width(label.as_str()) + 4)
        .max()
        .unwrap_or(0))
    .max(UnicodeWidthStr::width(title) + 2)
    .min(width)
    .min(64);
    let x = width - popup_width;
    let y = rows.len() - count - 2;
    for offset in 0..count + 1 {
        let (text, style) = if offset == 0 {
            (format!(" {title}"), "status")
        } else {
            let index = scroll + offset - 1;
            let (key, label) = &entries[index];
            (
                format!(
                    " {} {} {label}",
                    if selected == Some(index) { ">" } else { " " },
                    fit(key, key_width)
                ),
                if selected == Some(index) {
                    "selection"
                } else {
                    "popup"
                },
            )
        };
        let left = super::clip_spans(std::mem::take(&mut rows[y + offset].spans), x);
        rows[y + offset].spans = left;
        let used = rows[y + offset]
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum::<usize>();
        if used < x {
            rows[y + offset].spans.push(Span {
                base_style: None,
                text: " ".repeat(x - used),
                style: String::new(),
            });
        }
        rows[y + offset].spans.push(Span {
            base_style: None,
            text: fit(&text, popup_width),
            style: style.into(),
        });
    }
}

fn command_grid(
    rows: &mut [FrameRow],
    width: usize,
    entries: &[CommandCandidate],
    selected: Option<usize>,
    documentation: &[(String, &str)],
) {
    if rows.is_empty() || width == 0 {
        return;
    }
    let documentation = documentation
        .iter()
        .flat_map(|(text, style)| text.lines().map(move |line| (line, *style)))
        .flat_map(|(line, style)| {
            let mut lines = Vec::new();
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
            lines
        })
        .take(rows.len().saturating_sub(usize::from(!entries.is_empty())))
        .collect::<Vec<_>>();
    let cell = (entries
        .iter()
        .map(|candidate| UnicodeWidthStr::width(candidate.label.as_str()) + 2)
        .max()
        .unwrap_or(1))
    .min(width);
    let columns = (width / cell).max(1);
    let count = entries
        .len()
        .div_ceil(columns)
        .min(rows.len() - documentation.len())
        .min(8);
    let start = (selected.unwrap_or(0) / columns).saturating_sub(count.saturating_sub(1)) * columns;
    let y = rows.len() - count - documentation.len();
    for (row, (text, style)) in rows[y..].iter_mut().zip(&documentation) {
        row.spans = vec![Span {
            base_style: None,
            text: fit(text, width),
            style: (*style).into(),
        }];
    }
    for offset in 0..count {
        let mut spans = Vec::<Span>::new();
        for column in 0..columns {
            let index = start + offset * columns + column;
            if let Some(candidate) = entries.get(index) {
                let base = if selected == Some(index) {
                    "selection"
                } else {
                    "popup"
                };
                for (index, grapheme) in fit(&candidate.label, cell).graphemes(true).enumerate() {
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
            }
        }
        rows[y + documentation.len() + offset].spans = super::clip_spans(spans, width);
    }
}

fn binding_entries(
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
