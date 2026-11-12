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
                PromptKind::Filter(_) => "filter",
                _ => "find",
            };
            let prefix = match kind {
                zess_filter::QueryKind::Plain => format!("{purpose}:"),
                zess_filter::QueryKind::Glob => format!("{purpose}(glob):"),
                zess_filter::QueryKind::Regex => format!("{purpose}(re):"),
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
    pub(super) prefix: String,
    pub(super) index: usize,
}

#[derive(PartialEq)]
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
    pub suspended: Option<Prompt>,
}

#[derive(Clone)]
pub(super) struct HelpContext {
    scopes: Vec<String>,
    mode: String,
}

impl Session {
    pub(super) fn pending_input(&self) -> String {
        let view = &self.views[&self.focus];
        if let Some(request) = &self.clipboard_read
            && request.view == self.focus
        {
            return format!(
                "Reading register \"{} · Esc to cancel",
                request.operation.register
            );
        }
        if let Some(jump) = &view.jump {
            return if jump.input.is_empty() {
                "Goto item: type a highlighted label · Esc to cancel".into()
            } else {
                format!(
                    "Goto item: {}_ · type the second letter · Esc to cancel",
                    jump.input
                )
            };
        }
        let pending = match &self.buffers[&view.location.buffer] {
            Buffer::Text(text)
            | Buffer::Snapshot { text, .. }
            | Buffer::PreviewText { text, .. } => text.pending_input(self.focus),
            _ => match view.register {
                RegisterInput::None => String::new(),
                RegisterInput::Selecting => "Register: type a name · Esc to cancel".into(),
                RegisterInput::Selected(register) => format!("Register \"{register}"),
            },
        };
        let mut parts = Vec::new();
        let count = self.command_count();
        if !count.is_empty() {
            parts.push(count.to_string());
        }
        if !pending.is_empty() {
            parts.push(pending);
        }
        if !self.key_sequence.keys().is_empty() {
            parts.push(format!(
                "{} … · Esc to cancel",
                self.key_sequence.keys().join(" ")
            ));
        }
        parts.join(" · ")
    }

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
                                    let priority = if !prefix.is_empty() && name == prefix {
                                        0
                                    } else if !prefix.is_empty()
                                        && name != entry.name
                                        && name.starts_with(prefix)
                                    {
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
            None if backwards => (candidates.len() - steps % candidates.len()) % candidates.len(),
            None => steps % candidates.len(),
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
            role: super::ItemsRole::Standalone,
            kind: "help".into(),
            path: String::new(),
            display,
            help_context: Some(context),
        });
        let view = self.create_page(buffer);
        let source = self.views.focus_target(source);
        self.views.get_mut(&view).unwrap().return_to = Some(source);
        self.key_sequence.clear();
        let suspended = self.prompt.take();
        self.activate_view(view);
        self.help = Some(Help {
            view,
            source,
            suspended,
        });
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

    pub(super) fn close_help_overlay(&mut self) -> anyhow::Result<()> {
        if let Some(help) = &self.help
            && self
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.target.view == help.view)
        {
            self.cancel_prompt()?;
        }
        if let Some(mut help) = self.help.take() {
            let buffer = self.views[&help.view].location.buffer;
            let page = self.views.page_id(help.view);
            let source = self
                .views
                .resolve_focus(help.source)
                .unwrap_or_else(|| self.first_layout_view());
            while let Some(child) = self.views.layer_in_page(page) {
                self.reparent_layer_to_source(child, source);
            }
            self.retire_page(page)?;
            self.buffers.remove(&buffer);
            if self.focus == help.view {
                let mut focus = source;
                while let Some(child) = self.views.child(focus) {
                    focus = child;
                }
                self.activate_view(focus);
            }
            if let Some(prompt) = help.suspended.take() {
                self.prompt = Some(prompt);
                self.invalidate_prompt()?;
            }
        }
        Ok(())
    }

    pub(super) fn help_command(&mut self, command: HelpCommand) -> anyhow::Result<()> {
        match command {
            HelpCommand::Close => {
                if self.help.is_some() {
                    self.close_help_overlay()?;
                    self.key_sequence.clear();
                } else {
                    self.close_view(false)?;
                }
            }
            HelpCommand::Pin => {
                if self.fullscreen_layer().is_some() {
                    anyhow::bail!("close the fullscreen layer before pinning help");
                }
                if let Some(help) = &self.help {
                    let view = help.view;
                    let mut root = self
                        .views
                        .resolve_focus(help.source)
                        .unwrap_or_else(|| self.first_layout_view());
                    while let Some(parent) = self.views.parent(root) {
                        root = parent;
                    }
                    let source_page = self.views.page_id(root);
                    let mut pages = Vec::new();
                    self.layout.as_ref().unwrap().leaves(&mut pages);
                    anyhow::ensure!(
                        pages.contains(&source_page),
                        "help source is no longer in the layout"
                    );
                    self.cancel_suspended_prompt()?;
                    anyhow::ensure!(
                        self.layout.as_mut().unwrap().split(
                            source_page,
                            self.views.page_id(view),
                            super::Axis::Horizontal,
                        ),
                        "help source is no longer in the layout"
                    );
                    self.help = None;
                    self.activate_view(view);
                }
            }
            HelpCommand::ScrollDown => self.data_command(self.focus, DataCommand::MoveDown)?,
            HelpCommand::ScrollUp => self.data_command(self.focus, DataCommand::MoveUp)?,
            HelpCommand::PageHalfDown => {
                self.data_command(self.focus, DataCommand::PageHalfDown)?
            }
            HelpCommand::PageHalfUp => self.data_command(self.focus, DataCommand::PageHalfUp)?,
        }
        Ok(())
    }

    pub(super) fn render_help(&mut self, rows: &mut [FrameRow], width: usize) {
        if let Some(help) = &self.help {
            let id = help.view;
            let (rendered, _) = self.render_view(id, width, rows.len());
            for (target, row) in rows.iter_mut().zip(rendered) {
                *target = row;
            }
        }
        if let Some(prompt) = self.active_prompt() {
            if matches!(prompt.kind, PromptKind::Command)
                && let Some(grid) = &self.command_mouse
            {
                grid.render(rows, width);
            }
        } else if !self.key_sequence.keys().is_empty() {
            let (scopes, mode) = self.key_context();
            let entries = self
                .scripts
                .as_ref()
                .map(|scripts| {
                    scripts.with_keymap(|keymap| {
                        scripts.with_commands(|commands| {
                            binding_entries(
                                commands,
                                keymap.effective_bindings(&scopes, &mode),
                                self.key_sequence.keys(),
                            )
                        })
                    })
                })
                .unwrap_or_default();
            overlay(rows, width, &self.key_sequence.keys().join(" "), &entries);
        }
    }
}

fn overlay(rows: &mut [FrameRow], width: usize, title: &str, entries: &[(String, String)]) {
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
            text: format!(" {} ", fit(&text, popup_width - 2)),
            style: style.into(),
        });
    }
}

#[derive(PartialEq)]
pub(super) struct CommandGrid {
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

impl Session {
    pub(super) fn command_grid(&self, prompt: &Prompt, width: usize, height: usize) -> CommandGrid {
        let prefix = prompt
            .completion
            .as_ref()
            .map_or(prompt.text.as_str(), |completion| {
                completion.prefix.as_str()
            });
        let entries = if prefix.contains(char::is_whitespace) {
            Vec::new()
        } else {
            self.command_candidates(prefix)
        };
        let selected = (!entries.is_empty()).then(|| {
            prompt
                .completion
                .as_ref()
                .map_or(0, |completion| completion.index)
        });
        let documentation = selected
            .and_then(|index| entries.get(index))
            .map(CommandCandidate::documentation)
            .or_else(|| {
                let name = prompt.text.split_whitespace().next()?;
                self.scripts.as_ref()?.with_commands(|catalog| {
                    catalog.resolve_command_line(name).map(|command| {
                        CommandCandidate::new(
                            command,
                            command.command_line.as_ref().unwrap(),
                            Vec::new(),
                        )
                        .documentation()
                    })
                })
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
        let cell = entries
            .iter()
            .map(|candidate| UnicodeWidthStr::width(candidate.label.as_str()) + 2)
            .max()
            .unwrap_or(1)
            .min(width)
            .max(1);
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
}

impl CommandGrid {
    pub(super) fn hit(&self, x: usize, y: usize, width: usize) -> Option<Option<usize>> {
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

    fn render(&self, rows: &mut [FrameRow], width: usize) {
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
                    for (index, grapheme) in candidate.label.graphemes(true).enumerate() {
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
            rows[self.y + self.documentation.len() + offset].spans =
                super::clip_spans(spans, width);
        }
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
