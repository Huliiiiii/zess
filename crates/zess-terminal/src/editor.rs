use std::borrow::Cow;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_filter::{Query, QueryKind};
use zess_plugin::command_menu::{
    CommandGrid, Completion, CompletionDirection, binding_entries, command_candidates,
    complete_command, render_key_hints,
};
use zess_protocol::bindings::{
    BindingCatalog, BindingCommand, HostRequest, KeyResolution, KeyState, Keymap,
};
use zess_ui::{CursorShape, Frame, MouseEvent, MouseKind};

mod help;
#[cfg(test)]
mod tests;

use help::{Help, HelpAction};

pub(super) enum PromptEvent {
    Changed,
    Submit,
    Cancel,
}

pub(super) enum ClientInput {
    Consumed,
    CancelledKeySequence,
    Unbound,
    Prompt(PromptEvent),
    Command { name: String, argument: Value },
}

#[derive(Default)]
enum PromptKind {
    #[default]
    Command,
    HelpFilter(QueryKind),
    External {
        label: String,
        scopes: Vec<String>,
        query: Option<QueryKind>,
    },
}

#[derive(Default)]
struct Prompt {
    kind: PromptKind,
    error: Option<String>,
    text: String,
    cursor: usize,
    completion: Option<Completion>,
}

pub(crate) fn render_prompt(
    frame: &mut Frame,
    label: &str,
    text: &str,
    cursor: usize,
    error: Option<&str>,
) {
    if let Some(error) = error
        && let Some(row) = frame.rows.last_mut()
    {
        let width = usize::from(frame.width);
        let text = if width > 0 && UnicodeWidthStr::width(error) > width {
            let mut cells = 1;
            let mut start = error.len();
            for (index, glyph) in error.grapheme_indices(true).rev() {
                cells += UnicodeWidthStr::width(glyph);
                if cells > width {
                    break;
                }
                start = index;
            }
            zess_ui::fit(&format!("…{}", &error[start..]), width)
        } else {
            zess_ui::fit(error, width)
        };
        row.spans = vec![zess_ui::Span {
            text,
            style: "error".into(),
            base_style: None,
        }];
    }
    let mut column = UnicodeWidthStr::width(label) + UnicodeWidthStr::width(&text[..cursor]);
    let value = format!("{label}{text}");
    let mut start = 0;
    for grapheme in value.graphemes(true) {
        if column < usize::from(frame.width) {
            break;
        }
        start += grapheme.len();
        column = column.saturating_sub(UnicodeWidthStr::width(grapheme));
    }
    frame.prompt = value[start..].to_owned();
    frame.cursor_shape = CursorShape::Bar;
    frame.cursor = Some((column as u16, frame.height.saturating_sub(1)));
}

impl Prompt {
    fn previous_char(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_char(&self) -> usize {
        self.cursor
            + self.text[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8)
    }

    fn insert(&mut self, text: &str) {
        self.error = None;
        self.completion = None;
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn edit(&mut self, command: &str, catalog: &BindingCatalog, width: u16) -> bool {
        self.error = None;
        let command = if matches!(
            self.kind,
            PromptKind::HelpFilter(_) | PromptKind::External { query: Some(_), .. }
        ) {
            match command {
                "prompt.complete-left" => "prompt.move-left",
                "prompt.complete-right" => "prompt.move-right",
                other => other,
            }
        } else {
            command
        };
        match command {
            "prompt.move-home" => self.cursor = 0,
            "prompt.move-end" => self.cursor = self.text.len(),
            "prompt.move-left" => self.cursor = self.previous_char(),
            "prompt.move-right" => self.cursor = self.next_char(),
            "prompt.delete-backward" => {
                let start = self.previous_char();
                self.text.drain(start..self.cursor);
                self.cursor = start;
                self.completion = None;
            }
            "prompt.delete-forward" => {
                self.text.drain(self.cursor..self.next_char());
                self.completion = None;
            }
            "prompt.clear" => {
                self.text.clear();
                self.cursor = 0;
                self.completion = None;
            }
            "prompt.toggle-regex" | "prompt.toggle-glob" => {
                if let PromptKind::HelpFilter(query)
                | PromptKind::External {
                    query: Some(query), ..
                } = &mut self.kind
                {
                    let next = if command == "prompt.toggle-regex" {
                        QueryKind::Regex
                    } else {
                        QueryKind::Glob
                    };
                    *query = if *query == next {
                        QueryKind::Plain
                    } else {
                        next
                    };
                }
            }
            _ => {
                let direction = match command {
                    "prompt.complete-left" | "prompt.complete-previous" => {
                        CompletionDirection::Previous
                    }
                    "prompt.complete-right" | "prompt.complete-next" => CompletionDirection::Next,
                    "prompt.complete-up" => CompletionDirection::Up,
                    "prompt.complete-down" => CompletionDirection::Down,
                    _ => return false,
                };
                if !matches!(self.kind, PromptKind::Command) {
                    return true;
                }
                complete_command(
                    &catalog.commands,
                    &mut self.text,
                    &mut self.completion,
                    direction,
                    width.into(),
                );
                self.cursor = self.text.len();
            }
        }
        true
    }

    fn submit(self, catalog: &BindingCatalog) -> Result<ClientInput> {
        let text = if self.completion.is_none()
            && !self.text.is_empty()
            && !self.text.contains(char::is_whitespace)
        {
            command_candidates(&catalog.commands, &self.text)
                .into_iter()
                .next()
                .map_or(self.text, |candidate| candidate.name)
        } else {
            self.text
        };
        let text = text.trim();
        if text.is_empty() {
            return Ok(ClientInput::Consumed);
        }
        let (name, argument) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let command = catalog
            .commands
            .resolve_command_line(name)
            .with_context(|| format!("unknown command: {name}"))?;
        Ok(ClientInput::Command {
            name: command.id.clone(),
            argument: json!({ "argument": argument.trim() }),
        })
    }

    fn render(&self, catalog: &BindingCatalog, frame: &mut Frame) {
        frame
            .rows
            .resize_with(frame.height.saturating_sub(1).into(), Default::default);
        let (label, query) = match &self.kind {
            PromptKind::Command => {
                self.grid(catalog, frame.width, frame.height)
                    .render(&mut frame.rows, frame.width.into());
                (":", None)
            }
            PromptKind::HelpFilter(query) => ("filter", Some(query)),
            PromptKind::External { label, query, .. } => (label.as_str(), query.as_ref()),
        };
        let label = match query {
            None => Cow::Borrowed(label),
            Some(QueryKind::Plain) => Cow::Owned(format!("{label}:")),
            Some(QueryKind::Regex) => Cow::Owned(format!("{label}(re):")),
            Some(QueryKind::Glob) => Cow::Owned(format!("{label}(glob):")),
        };
        render_prompt(
            frame,
            &label,
            &self.text,
            self.cursor,
            self.error.as_deref(),
        );
    }

    fn grid(&self, catalog: &BindingCatalog, width: u16, height: u16) -> CommandGrid {
        CommandGrid::new(
            &catalog.commands,
            &self.text,
            self.completion.as_ref(),
            width.into(),
            height.saturating_sub(1).into(),
        )
    }
}

struct Bindings {
    catalog: BindingCatalog,
    keymap: Keymap,
}

struct OpenHelp {
    page: Help,
    suspended: Option<Prompt>,
}

fn key_context<'a>(
    prompt: Option<&'a Prompt>,
    help: Option<&'a OpenHelp>,
    scopes: &'a [&'a str],
    mode: &'a str,
) -> (Cow<'a, [&'a str]>, &'a str) {
    match prompt.map(|prompt| &prompt.kind) {
        Some(PromptKind::Command | PromptKind::HelpFilter(_)) => {
            (Cow::Borrowed(&["global"]), "prompt")
        }
        Some(PromptKind::External { scopes, .. }) => (
            Cow::Owned(scopes.iter().map(String::as_str).collect()),
            "prompt",
        ),
        None => match help.filter(|help| help.page.focused()) {
            Some(help) => (Cow::Borrowed(&["global", "data", "help"]), help.page.mode()),
            None => (Cow::Borrowed(scopes), mode),
        },
    }
}

#[derive(Default)]
pub(super) struct ClientEditor {
    bindings: Option<Bindings>,
    prompt: Option<Prompt>,
    help: Option<OpenHelp>,
    prefix: Vec<String>,
    matching: KeyState,
}

impl ClientEditor {
    pub(super) fn set_catalog(&mut self, catalog: BindingCatalog) {
        let keymap = catalog.keymap();
        if let Some(help) = &mut self.help {
            help.page.refresh(&catalog, &keymap);
        }
        self.bindings = Some(Bindings { catalog, keymap });
        self.prefix.clear();
        self.matching = KeyState::default();
        if let Some(prompt) = &mut self.prompt {
            prompt.completion = None;
        }
    }

    pub(super) fn catalog(&self) -> Option<&BindingCatalog> {
        Some(&self.bindings.as_ref()?.catalog)
    }

    pub(super) fn prompt_active(&self) -> bool {
        self.prompt.is_some()
    }

    pub(super) fn help_focused(&self) -> bool {
        self.help.as_ref().is_some_and(|help| help.page.focused())
    }

    pub(super) fn source_height(&self, height: u16) -> u16 {
        self.help
            .as_ref()
            .map_or(height, |help| help.page.source_height(height))
    }

    pub(super) fn open_prompt(
        &mut self,
        label: String,
        text: String,
        scopes: Vec<String>,
        query: Option<QueryKind>,
    ) {
        self.reset_input();
        self.prompt = Some(Prompt {
            cursor: text.len(),
            text,
            kind: PromptKind::External {
                label,
                scopes,
                query,
            },
            ..Prompt::default()
        });
    }

    pub(super) fn external_prompt(&self) -> bool {
        matches!(
            self.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::External { .. })
        )
    }

    pub(super) fn prompt_text(&self) -> &str {
        self.prompt
            .as_ref()
            .map_or("", |prompt| prompt.text.as_str())
    }

    pub(super) fn query_kind(&self) -> Option<QueryKind> {
        match &self.prompt.as_ref()?.kind {
            PromptKind::External { query, .. } => *query,
            PromptKind::HelpFilter(kind) => Some(*kind),
            PromptKind::Command => None,
        }
    }

    pub(super) fn set_prompt_error(&mut self, message: String) {
        if let Some(prompt) = &mut self.prompt {
            prompt.error = Some(message);
        }
    }

    pub(super) fn reset_input(&mut self) {
        self.prompt = None;
        self.prefix.clear();
        self.matching.reset();
    }

    pub(super) fn close_external_prompt(&mut self) {
        if self.external_prompt() {
            self.reset_input();
        }
        if let Some(help) = &mut self.help {
            help.suspended
                .take_if(|prompt| matches!(prompt.kind, PromptKind::External { .. }));
        }
    }

    fn prompt_event(&mut self, event: PromptEvent) -> ClientInput {
        let Some(prompt) = &mut self.prompt else {
            return ClientInput::Consumed;
        };
        match prompt.kind {
            PromptKind::Command => ClientInput::Consumed,
            PromptKind::External { .. } => ClientInput::Prompt(event),
            PromptKind::HelpFilter(kind) => {
                match event {
                    PromptEvent::Changed => {
                        let help = &mut self.help.as_mut().unwrap().page;
                        let query = Query {
                            kind,
                            text: prompt.text.clone(),
                        };
                        prompt.error = help.filter(query).err().map(|error| error.to_string());
                    }
                    PromptEvent::Submit | PromptEvent::Cancel => self.reset_input(),
                }
                ClientInput::Consumed
            }
        }
    }

    pub(super) fn paste(&mut self, text: &str) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return self.help_focused();
        };
        self.matching.reset();
        self.prefix.clear();
        prompt.insert(&text.replace("\r\n", "\n").replace(['\r', '\n'], " "));
        if !self.external_prompt() {
            self.prompt_event(PromptEvent::Changed);
        }
        true
    }

    pub(super) fn handle_key(
        &mut self,
        key: &str,
        scopes: &[&str],
        mode: &str,
        width: u16,
        height: u16,
    ) -> Result<ClientInput> {
        if self.prompt.is_none()
            && let Some(help) = &mut self.help
            && help.page.focused()
            && help.page.pending_key(key, height)
        {
            return Ok(ClientInput::Consumed);
        }
        let Some(bindings) = &self.bindings else {
            return Ok(ClientInput::Unbound);
        };
        let (key_scopes, key_mode) =
            key_context(self.prompt.as_ref(), self.help.as_ref(), scopes, mode);
        let key = if key == " " { "space" } else { key };
        if bindings
            .keymap
            .synchronize(&mut self.matching, &key_scopes, key_mode)
        {
            self.prefix.clear();
        }
        let command = match bindings
            .keymap
            .advance(&mut self.matching, &key_scopes, key_mode, key)
        {
            KeyResolution::Prefix => {
                self.prefix.push(key.to_owned());
                return Ok(ClientInput::Consumed);
            }
            KeyResolution::Unbound => {
                if !self.prefix.is_empty() {
                    self.prefix.clear();
                    return Ok(ClientInput::CancelledKeySequence);
                }
                if let Some(prompt) = &mut self.prompt {
                    let text = if key == "space" { " " } else { key };
                    if text.chars().count() == 1 && !text.contains(['\n', '\r']) {
                        prompt.insert(text);
                        if !matches!(prompt.kind, PromptKind::Command) {
                            return Ok(self.prompt_event(PromptEvent::Changed));
                        }
                    } else {
                        prompt.completion = None;
                    }
                    return Ok(ClientInput::Consumed);
                }
                return Ok(if self.help_focused() {
                    ClientInput::Consumed
                } else {
                    ClientInput::Unbound
                });
            }
            KeyResolution::Command(command) => command,
        };
        self.prefix.clear();
        let name = bindings
            .catalog
            .commands
            .resolve(command)
            .with_context(|| format!("unknown command: {command}"))?
            .id
            .clone();
        self.dispatch(name, json!({}), scopes, mode, width, height)
    }

    fn dispatch(
        &mut self,
        name: String,
        argument: Value,
        scopes: &[&str],
        mode: &str,
        width: u16,
        height: u16,
    ) -> Result<ClientInput> {
        let catalog = &self.bindings.as_ref().unwrap().catalog;
        let native = match catalog.implementation(&name) {
            Some(BindingCommand::Native) => Some(name.as_str()),
            Some(BindingCommand::Request(HostRequest::Action { name, .. })) => Some(name.as_str()),
            _ => None,
        };
        if native == Some("help") {
            if self.help.is_none() {
                let Bindings { catalog, keymap } = self.bindings.as_ref().unwrap();
                let (scopes, mode) = key_context(self.prompt.as_ref(), None, scopes, mode);
                let title = keymap.scope_label(scopes.last().copied().unwrap_or("global"));
                self.help = Some(OpenHelp {
                    page: Help::new(catalog, keymap, &scopes, mode, title),
                    suspended: self.prompt.take(),
                });
            }
            return Ok(ClientInput::Consumed);
        }
        if self.prompt.is_none()
            && let (Some(help), Some(command)) = (&mut self.help, native)
        {
            if help.page.focused() && command == "data.filter" {
                let query = help.page.query();
                self.prompt = Some(Prompt {
                    text: query.text.clone(),
                    cursor: query.text.len(),
                    kind: PromptKind::HelpFilter(query.kind),
                    ..Prompt::default()
                });
                return Ok(ClientInput::Consumed);
            }
            match help.page.command(command, height) {
                HelpAction::Consumed => return Ok(ClientInput::Consumed),
                HelpAction::Close => {
                    self.prompt = self.help.take().unwrap().suspended;
                    return Ok(ClientInput::Consumed);
                }
                HelpAction::Unhandled => {}
            }
        }
        match native {
            Some("command-prompt") => self.prompt = Some(Prompt::default()),
            Some("prompt.cancel")
                if self
                    .prompt
                    .as_ref()
                    .is_some_and(|p| !matches!(p.kind, PromptKind::Command)) =>
            {
                return Ok(self.prompt_event(PromptEvent::Cancel));
            }
            Some("prompt.submit")
                if self
                    .prompt
                    .as_ref()
                    .is_some_and(|p| !matches!(p.kind, PromptKind::Command)) =>
            {
                return Ok(self.prompt_event(PromptEvent::Submit));
            }
            Some("prompt.cancel") if self.prompt.is_some() => self.prompt = None,
            Some("prompt.submit") if self.prompt.is_some() => {
                return match self.prompt.take().unwrap().submit(catalog)? {
                    ClientInput::Command { name, argument } => {
                        self.dispatch(name, argument, scopes, mode, width, height)
                    }
                    input => Ok(input),
                };
            }
            Some(command)
                if self
                    .prompt
                    .as_mut()
                    .is_some_and(|prompt| prompt.edit(command, catalog, width)) =>
            {
                return Ok(self.prompt_event(PromptEvent::Changed));
            }
            _ => return Ok(ClientInput::Command { name, argument }),
        }
        Ok(ClientInput::Consumed)
    }

    pub(super) fn mouse(&mut self, event: &MouseEvent, width: u16, height: u16) -> bool {
        let Some(prompt) = &mut self.prompt else {
            self.prefix.clear();
            self.matching.reset();
            return self
                .help
                .as_mut()
                .is_some_and(|help| help.page.mouse(event, height));
        };
        if !matches!(prompt.kind, PromptKind::Command)
            || !matches!(event.kind, MouseKind::Press | MouseKind::DoubleClick)
            || (height > 0 && event.row == height - 1 && event.column < width)
        {
            return true;
        }
        let catalog = &self.bindings.as_ref().unwrap().catalog;
        let grid = prompt.grid(catalog, width, height);
        match grid.hit(event.column.into(), event.row.into(), width.into()) {
            Some(Some(index)) => {
                prompt.text = grid.entries[index].name.clone();
                prompt.cursor = prompt.text.len();
                prompt.completion = Some(Completion {
                    prefix: grid.prefix,
                    index,
                });
                true
            }
            Some(None) => true,
            None => {
                self.reset_input();
                self.help
                    .as_mut()
                    .is_some_and(|help| help.page.mouse(event, height))
            }
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, scopes: &[&str], mode: &str) {
        let Some(Bindings { catalog, keymap }) = &self.bindings else {
            return;
        };
        if let Some(help) = &self.help {
            help.page.render(frame);
        }
        if let Some(prompt) = &self.prompt {
            prompt.render(catalog, frame);
        } else if !self.prefix.is_empty() {
            let (scopes, mode) =
                key_context(self.prompt.as_ref(), self.help.as_ref(), scopes, mode);
            let bindings = keymap.effective_bindings(&scopes, mode);
            let entries = binding_entries(&catalog.commands, bindings, &self.prefix);
            let keys = self.prefix.join(" ");
            render_key_hints(&mut frame.rows, frame.width.into(), &keys, &entries);
            let prefix_prompt = format!("{keys} … · Esc to cancel");
            if !frame.prompt.is_empty() {
                frame.prompt.push_str(" · ");
            }
            frame.prompt.push_str(&prefix_prompt);
            frame.cursor = None;
        }
    }
}
