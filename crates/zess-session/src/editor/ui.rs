use zess_plugin::command_menu::{CommandCandidate, command_candidates, help_rows};

use super::commands::{DataCommand, HelpCommand};
use super::{Buffer, DisplayRow, Prompt, PromptKind, Session};

impl Prompt {
    pub(super) fn label(&self) -> String {
        if let Some((kind, _)) = self.query() {
            let purpose = if matches!(self.kind, PromptKind::Filter(_)) {
                "filter"
            } else {
                "find"
            };
            return match kind {
                zess_filter::QueryKind::Plain => format!("{purpose}:"),
                zess_filter::QueryKind::Glob => format!("{purpose}(glob):"),
                zess_filter::QueryKind::Regex => format!("{purpose}(re):"),
            };
        }
        match &self.kind {
            PromptKind::Command => ":".into(),
            PromptKind::Text(kind) => format!("{}: ", kind.label()),
            PromptKind::DiffSearch(search) => if search.forward { "/" } else { "?" }.into(),
            PromptKind::Filter(_) => unreachable!("filters have a query"),
            PromptKind::MiniBuffer { purpose, .. } => format!("{purpose}: "),
        }
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
            scripts.with_commands(|commands| help_rows(commands, keymap, &scopes, &context.mode))
        })
    }

    pub(super) fn command_candidates(&self, prefix: &str) -> Vec<CommandCandidate> {
        self.scripts
            .as_ref()
            .map(|scripts| scripts.with_commands(|catalog| command_candidates(catalog, prefix)))
            .unwrap_or_default()
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
}
