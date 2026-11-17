use std::collections::HashSet;

use zess_filter::{Case, CompileError, Filter, Query};
use zess_plugin::{BindingCatalog, ItemSelection, Keymap, command_menu::help_rows};
use zess_ui::{
    DisplayRow, Frame, FrameRow, ItemJump, JumpInput, MouseEvent, MouseKind, ScrollMode, Span,
    clip_spans, fit, scroll_limit_with_margin, scroll_with_margin, skip_columns,
};

#[cfg(test)]
mod tests;

pub(super) enum HelpAction {
    Consumed,
    Close,
    Unhandled,
}

#[derive(Clone, Copy)]
enum Presentation {
    Overlay,
    Pinned { focused: bool },
}

pub(super) struct Help {
    title: String,
    source_scopes: Vec<String>,
    source_mode: String,
    source_rows: Vec<DisplayRow>,
    rows: Vec<DisplayRow>,
    filter: Filter,
    jump: Option<ItemJump>,
    selection: ItemSelection,
    selecting: bool,
    presentation: Presentation,
    scroll: usize,
    scroll_mode: ScrollMode,
}

impl Help {
    pub(super) fn new(
        catalog: &BindingCatalog,
        keymap: &Keymap,
        scopes: &[&str],
        mode: &str,
        title: &str,
    ) -> Self {
        let mut help = Self {
            title: format!("Help: {title}"),
            source_scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            source_mode: mode.to_owned(),
            source_rows: Vec::new(),
            rows: Vec::new(),
            filter: Filter::default(),
            jump: None,
            selection: ItemSelection::default(),
            selecting: false,
            presentation: Presentation::Overlay,
            scroll: 0,
            scroll_mode: ScrollMode::FollowSelection,
        };
        help.refresh(catalog, keymap);
        help
    }

    fn project_rows(&mut self) {
        self.jump = None;
        self.rows.clone_from(&self.source_rows);
        if !self.filter.is_empty() {
            let visible = self
                .source_rows
                .iter()
                .filter(|row| self.filter.is_match(&row.text))
                .map(|row| row.id.as_str())
                .collect::<HashSet<_>>();
            self.rows.retain(|row| visible.contains(row.id.as_str()));
        }
        let available = |id: &str| self.rows.iter().any(|row| row.id == id);
        self.selection.selected.retain(|id| available(id));
        if !self.selection.primary.as_deref().is_some_and(available) {
            self.selection.primary = self
                .rows
                .iter()
                .find(|row| !row.id.is_empty())
                .map(|row| row.id.clone());
        }
        if !self.selection.anchor.as_deref().is_some_and(available) {
            self.selection.anchor = self.selection.primary.clone();
        }
        if self.selection.selected.is_empty() {
            self.selection
                .selected
                .extend(self.selection.primary.iter().cloned());
        }
    }

    pub(super) fn refresh(&mut self, catalog: &BindingCatalog, keymap: &Keymap) {
        let scopes = self
            .source_scopes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        self.source_rows = help_rows(&catalog.commands, keymap, &scopes, &self.source_mode);

        self.project_rows();
    }

    pub(super) fn query(&self) -> &Query {
        self.filter.query()
    }

    pub(super) fn filter(&mut self, query: Query) -> Result<(), CompileError> {
        if &query != self.filter.query() {
            self.filter = Filter::compile(&query, Case::Insensitive)?;
            self.selection = ItemSelection::default();
            self.scroll = 0;
            self.scroll_mode = ScrollMode::FollowSelection;
            self.project_rows();
        }
        Ok(())
    }

    pub(super) fn focused(&self) -> bool {
        match self.presentation {
            Presentation::Overlay => true,
            Presentation::Pinned { focused } => focused,
        }
    }

    pub(super) fn mode(&self) -> &str {
        if self.selecting { "select" } else { "normal" }
    }

    fn top(&self, height: u16) -> usize {
        match self.presentation {
            Presentation::Overlay => 0,
            Presentation::Pinned { .. } => usize::from(height.saturating_sub(1)) / 2,
        }
    }

    pub(super) fn source_height(&self, height: u16) -> u16 {
        match self.presentation {
            Presentation::Overlay => height,
            Presentation::Pinned { .. } => (self.top(height) as u16 + 1).min(height),
        }
    }

    fn available(&self, height: u16) -> usize {
        usize::from(height.saturating_sub(1)).saturating_sub(self.top(height) + 2)
    }

    fn current_row(&self) -> usize {
        self.rows
            .iter()
            .position(|row| Some(&row.id) == self.selection.primary.as_ref())
            .unwrap_or_default()
    }

    fn first_row(&self, height: u16) -> usize {
        match self.scroll_mode {
            ScrollMode::FollowSelection => {
                scroll_with_margin(self.scroll, self.current_row(), self.available(height), 5)
            }
            ScrollMode::UserControlled => self.scroll.min(scroll_limit_with_margin(
                self.rows.len(),
                self.available(height),
                5,
            )),
        }
    }

    fn group(&self, first: usize) -> Option<&DisplayRow> {
        self.rows
            .iter()
            .take(first.saturating_add(1))
            .rfind(|row| row.id.is_empty())
    }

    fn content_start(&self, first: usize) -> usize {
        first + usize::from(self.rows.get(first).is_some_and(|row| row.id.is_empty()))
    }

    fn select(&mut self, row: usize, extend: bool, height: u16) {
        let id = &self.rows[row].id;
        if id.is_empty() {
            return;
        }
        if self.selecting || extend {
            let anchor = self
                .rows
                .iter()
                .position(|row| Some(&row.id) == self.selection.anchor.as_ref())
                .unwrap_or(row);
            self.selection.selected = self.rows[anchor.min(row)..=anchor.max(row)]
                .iter()
                .filter(|row| !row.id.is_empty())
                .map(|row| row.id.clone())
                .collect();
        } else {
            self.selection.selected = vec![id.clone()];
            self.selection.anchor = Some(id.clone());
        }
        self.selection.primary = Some(id.clone());
        self.scroll_mode = ScrollMode::FollowSelection;
        self.scroll = self.first_row(height);
    }

    pub(super) fn pending_key(&mut self, key: &str, height: u16) -> bool {
        let Some(jump) = self.jump.take() else {
            return false;
        };
        match jump.input(key) {
            JumpInput::Waiting(jump) => self.jump = Some(jump),
            JumpInput::Selected(id) => {
                if let Some(row) = self.rows.iter().position(|row| row.id == id) {
                    self.select(row, false, height);
                }
            }
            JumpInput::Cancelled => {}
        }
        true
    }

    pub(super) fn command(&mut self, name: &str, height: u16) -> HelpAction {
        if let Presentation::Pinned { focused } = &mut self.presentation {
            match name {
                "focus-next" => {
                    *focused = !*focused;
                    return HelpAction::Consumed;
                }
                "focus-up" if *focused => {
                    *focused = false;
                    return HelpAction::Consumed;
                }
                "focus-down" if !*focused => {
                    *focused = true;
                    return HelpAction::Consumed;
                }
                _ => {}
            }
        }
        if !self.focused() {
            return HelpAction::Unhandled;
        }
        match name {
            "data.goto-word" => {
                let start = self.content_start(self.first_row(height));
                self.jump = ItemJump::new(
                    self.rows
                        .iter()
                        .skip(start)
                        .take(self.available(height))
                        .map(|row| row.id.as_str()),
                );
            }
            "help.close" | "quit" | "quit!" => return HelpAction::Close,
            "help.pin" => self.presentation = Presentation::Pinned { focused: true },
            "help" | "focus-up" | "focus-down" | "focus-left" | "focus-right" | "focus-next"
            | "data.refresh" => {}
            "data.select-mode" => {
                self.selecting = !self.selecting;
                if self.selecting {
                    self.selection.anchor = self.selection.primary.clone();
                }
            }
            "data.escape" | "data.keep-primary" => {
                self.selecting = false;
                self.selection.selected = self.selection.primary.iter().cloned().collect();
                self.selection.anchor = self.selection.primary.clone();
            }
            "data.select-all" => {
                self.selection.selected = self
                    .rows
                    .iter()
                    .filter(|row| !row.id.is_empty())
                    .map(|row| row.id.clone())
                    .collect();
            }
            _ => {
                let (step, extend) = match name {
                    "data.goto-first" => (isize::MIN, false),
                    "data.goto-last" => (isize::MAX, false),
                    "help.scroll-down" | "data.move-down" | "data.copy-selection-down" => {
                        (1, false)
                    }
                    "help.scroll-up" | "data.move-up" => (-1, false),
                    "help.page-half-down" | "data.page-half-down" => {
                        ((self.available(height) / 2).max(1) as isize, false)
                    }
                    "help.page-half-up" | "data.page-half-up" => {
                        (-((self.available(height) / 2).max(1) as isize), false)
                    }
                    "data.extend-lines" => (1, true),
                    _ => return HelpAction::Unhandled,
                };
                let rows = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(|(i, row)| (!row.id.is_empty()).then_some(i))
                    .collect::<Vec<_>>();
                if !rows.is_empty() {
                    let current = rows.binary_search(&self.current_row()).unwrap_or_default();
                    let target = rows[current.saturating_add_signed(step).min(rows.len() - 1)];
                    if name == "data.copy-selection-down" {
                        let id = self.rows[target].id.clone();
                        if !self.selection.selected.contains(&id) {
                            self.selection.selected.push(id.clone());
                        }
                        self.selection.anchor = Some(id.clone());
                        self.selection.primary = Some(id);
                        self.scroll_mode = ScrollMode::FollowSelection;
                        self.scroll = self.first_row(height);
                    } else {
                        self.select(target, extend, height);
                    }
                }
            }
        }
        HelpAction::Consumed
    }

    pub(super) fn mouse(&mut self, event: &MouseEvent, height: u16) -> bool {
        self.jump = None;
        let y = usize::from(event.row);
        let top = self.top(height);
        if y >= usize::from(height.saturating_sub(1)) {
            return true;
        }
        if y < top {
            if matches!(event.kind, MouseKind::Press | MouseKind::DoubleClick) {
                self.presentation = Presentation::Pinned { focused: false };
            }
            return false;
        }
        match event.kind {
            MouseKind::Press | MouseKind::DoubleClick => {
                if let Presentation::Pinned { focused } = &mut self.presentation {
                    *focused = true;
                }
                let first = self.first_row(height);
                let heading = usize::from(self.group(first).is_some());
                if let Some(offset) = y
                    .checked_sub(top + 1 + heading)
                    .filter(|&offset| offset < self.available(height))
                {
                    let row = self.content_start(first) + offset;
                    if row < self.rows.len() {
                        self.select(row, false, height);
                    }
                }
            }
            MouseKind::ScrollUp | MouseKind::ScrollDown => {
                let step = if matches!(event.kind, MouseKind::ScrollUp) {
                    -3
                } else {
                    3
                };
                self.scroll = self.first_row(height).saturating_add_signed(step).min(
                    scroll_limit_with_margin(self.rows.len(), self.available(height), 5),
                );
                self.scroll_mode = ScrollMode::UserControlled;
            }
            _ => {}
        }
        true
    }

    pub(super) fn render(&self, frame: &mut Frame) {
        let top = self.top(frame.height);
        let first = self.first_row(frame.height);
        let group = self.group(first);
        let start = self.content_start(first);
        let width = usize::from(frame.width);
        frame.rows.resize_with(
            usize::from(frame.height.saturating_sub(1)),
            FrameRow::default,
        );
        for (line, row) in frame.rows[top..].iter_mut().enumerate() {
            let (text, style, label) = if line == 0 {
                (self.title.as_str(), "ui.heading", None)
            } else if line == 1
                && let Some(group) = group
            {
                (group.text.as_str(), group.style.as_str(), None)
            } else {
                let offset = line - 1 - usize::from(group.is_some());
                self.rows
                    .get(start + offset)
                    .filter(|_| offset < self.available(frame.height))
                    .map_or(("", "", None), |row| {
                        let selected = self.focused() && self.selection.selected.contains(&row.id);
                        (
                            row.text.as_str(),
                            if selected {
                                "selection"
                            } else {
                                row.style.as_str()
                            },
                            self.jump.as_ref().and_then(|jump| jump.label(&row.id)),
                        )
                    })
            };
            let span = Span {
                base_style: None,
                text: fit(text, width),
                style: style.to_owned(),
            };
            row.spans = if let Some(label) = label {
                let mut body = [span];
                skip_columns(&mut body, 2);
                let [body] = body;
                clip_spans(
                    vec![
                        Span {
                            base_style: None,
                            text: label,
                            style: "pending".into(),
                        },
                        body,
                    ],
                    width,
                )
            } else {
                vec![span]
            };
        }
        if self.focused() {
            frame.cursor = None;
            frame.prompt = self.jump.as_ref().map(ItemJump::prompt).unwrap_or_default();
        }
    }
}
