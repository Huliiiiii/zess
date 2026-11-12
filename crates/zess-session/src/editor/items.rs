use super::ScrollMode;
use super::*;

pub(super) struct ItemJump {
    pub targets: Vec<String>,
    pub input: String,
}

impl View {
    pub(super) fn horizontal_scroll_limit(&self) -> usize {
        self.location
            .columns
            .horizontal_scroll_limit(self.viewport_width)
    }

    pub(super) fn item_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for row in &self.location.display.rows {
            if !row.id.is_empty() && seen.insert(&row.id) {
                ids.push(row.id.clone());
            }
        }
        ids
    }

    pub(super) fn project_items(&mut self, display: &Display) {
        self.jump = None;
        self.location.set_display(display.clone());
        if !self.location.filter.is_empty() {
            let visible = display
                .rows
                .iter()
                .filter(|row| self.location.filter.is_match(&row.text))
                .map(|row| row.id.as_str())
                .collect::<HashSet<_>>();
            self.location
                .display
                .rows
                .retain(|row| visible.contains(row.id.as_str()));
        }
        self.location.columns = zess_ui::RowColumns::measure(
            &self.location.display.rows,
            self.location.display.column_alignment,
        );
        self.reconcile_selection();
    }

    pub(super) fn reconcile_selection(&mut self) {
        let ids = self.item_ids();
        let visible = ids.iter().collect::<HashSet<_>>();
        let mut selected = HashSet::new();
        self.location
            .selection
            .selected
            .retain(|id| visible.contains(id) && selected.insert(id.clone()));
        if !self
            .location
            .selection
            .primary
            .as_ref()
            .is_some_and(|id| ids.contains(id))
        {
            self.location.selection.primary = ids.first().cloned();
        }
        if !self
            .location
            .selection
            .anchor
            .as_ref()
            .is_some_and(|id| ids.contains(id))
        {
            self.location.selection.anchor = self.location.selection.primary.clone();
        }
        if let Some(primary) = &self.location.selection.primary
            && !self.location.selection.selected.contains(primary)
        {
            self.location.selection.selected.push(primary.clone());
        }
    }

    fn select_primary(&mut self) {
        self.location.selection.selected =
            self.location.selection.primary.iter().cloned().collect();
        self.location.selection.anchor = self.location.selection.primary.clone();
    }
}

impl Session {
    pub(super) fn data_command(&mut self, id: ViewId, command: DataCommand) -> Result<()> {
        if !matches!(
            self.buffers[&self.views[&id].location.buffer],
            Buffer::Data { .. } | Buffer::Items { .. }
        ) {
            bail!("command requires an item buffer");
        }
        if command == DataCommand::GotoWord {
            anyhow::ensure!(id == self.focus, "item jump requires the focused view");
            return self.start_item_jump();
        }
        if command == DataCommand::SelectRegister {
            self.views.get_mut(&id).unwrap().register = RegisterInput::Selecting;
            return Ok(());
        }
        if command == DataCommand::Filter {
            anyhow::ensure!(id == self.focus, "item filter requires the focused view");
            self.set_mode(id, "filter")?;
            return Ok(());
        }
        if command == DataCommand::Refresh {
            return self.dispatch(id, "refresh", Value::Null);
        }
        let view = self.views.get_mut(&id).unwrap();
        view.jump = None;
        view.location.scroll_mode = ScrollMode::FollowSelection;
        let count = std::mem::take(&mut view.count).value();
        let repeat = count.unwrap_or(1).max(1);
        view.reconcile_selection();
        let ids = view.item_ids();
        if ids.is_empty()
            && !matches!(
                command,
                DataCommand::SelectMode
                    | DataCommand::Escape
                    | DataCommand::KeepPrimary
                    | DataCommand::SelectAll
            )
        {
            return Ok(());
        }
        let current = view
            .location
            .selection
            .primary
            .as_ref()
            .and_then(|id| ids.iter().position(|other| id == other))
            .unwrap_or(0);
        let last = ids.len().saturating_sub(1);
        let page_scroll = match command {
            DataCommand::PageHalfDown => Some(zess_ui::HalfPageScroll::new(
                view.viewport_height,
                repeat,
                zess_ui::ScrollDirection::Down,
            )),
            DataCommand::PageHalfUp => Some(zess_ui::HalfPageScroll::new(
                view.viewport_height,
                repeat,
                zess_ui::ScrollDirection::Up,
            )),
            _ => None,
        };
        match command {
            DataCommand::SelectMode => {
                if view.location.mode == ViewMode::Select {
                    view.location.mode = ViewMode::Normal;
                } else {
                    view.location.mode = ViewMode::Select;
                    view.location.selection.anchor = view.location.selection.primary.clone();
                }
            }
            DataCommand::Escape | DataCommand::KeepPrimary => {
                view.location.mode = ViewMode::Normal;
                view.select_primary();
            }
            DataCommand::CopySelectionDown => {
                let next = ids[(current + 1).min(last)].clone();
                if !view.location.selection.selected.contains(&next) {
                    view.location.selection.selected.push(next.clone());
                }
                view.location.selection.primary = Some(next.clone());
                view.location.selection.anchor = Some(next);
            }
            DataCommand::SelectAll => view.location.selection.selected = ids.clone(),
            DataCommand::MoveDown
            | DataCommand::MoveUp
            | DataCommand::GotoFirst
            | DataCommand::GotoLast
            | DataCommand::PageHalfDown
            | DataCommand::PageHalfUp
            | DataCommand::ExtendLines => {
                let target = match command {
                    DataCommand::MoveDown | DataCommand::ExtendLines => {
                        current.saturating_add(repeat).min(last)
                    }
                    DataCommand::MoveUp => current.saturating_sub(repeat),
                    DataCommand::GotoFirst => {
                        count.map_or(0, |count| count.saturating_sub(1).min(last))
                    }
                    DataCommand::GotoLast => {
                        count.map_or(last, |count| count.saturating_sub(1).min(last))
                    }
                    DataCommand::PageHalfDown => current
                        .saturating_add(page_scroll.as_ref().unwrap().distance())
                        .min(last),
                    _ => current.saturating_sub(page_scroll.as_ref().unwrap().distance()),
                };
                if let Some(page_scroll) = page_scroll {
                    let row = view
                        .location
                        .display
                        .rows
                        .iter()
                        .position(|row| row.id == ids[target])
                        .unwrap();
                    view.location.scroll = page_scroll.viewport_start(view.location.scroll, row, 5);
                }
                view.location.selection.primary = Some(ids[target].clone());
                if view.location.mode == ViewMode::Select || command == DataCommand::ExtendLines {
                    let anchor = view
                        .location
                        .selection
                        .anchor
                        .as_ref()
                        .and_then(|id| ids.iter().position(|other| id == other))
                        .unwrap_or(current);
                    view.location.selection.selected =
                        ids[anchor.min(target)..=anchor.max(target)].to_vec();
                } else {
                    view.select_primary();
                }
            }
            DataCommand::SelectRegister
            | DataCommand::GotoWord
            | DataCommand::Filter
            | DataCommand::Refresh => {
                unreachable!("handled before selection movement")
            }
        }
        let navigation = match command {
            DataCommand::MoveDown | DataCommand::MoveUp | DataCommand::ExtendLines => {
                Some(if count.is_some() {
                    ItemNavigation::Jump
                } else {
                    ItemNavigation::Step
                })
            }
            DataCommand::CopySelectionDown => Some(ItemNavigation::Step),
            DataCommand::GotoFirst
            | DataCommand::GotoLast
            | DataCommand::PageHalfDown
            | DataCommand::PageHalfUp => Some(ItemNavigation::Jump),
            _ => None,
        };
        self.dispatch(
            id,
            "select",
            serde_json::to_value(SelectionChange { navigation })?,
        )
    }
}

impl Session {
    fn start_item_jump(&mut self) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        view.count.clear();
        let mut seen = HashSet::new();
        let targets = view
            .location
            .display
            .rows
            .iter()
            .skip(view.location.scroll)
            .take(view.viewport_height)
            .filter(|row| !row.id.is_empty() && seen.insert(row.id.clone()))
            .take(26 * 26)
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();
        if !targets.is_empty() {
            view.jump = Some(ItemJump {
                targets,
                input: String::new(),
            });
        }
        Ok(())
    }

    pub(super) fn item_jump_input(&mut self, key: &str) -> Result<()> {
        let view = self.views.get_mut(&self.focus).unwrap();
        let mut jump = view.jump.take().unwrap();
        if key.len() != 1 || !key.as_bytes()[0].is_ascii_lowercase() {
            return Ok(());
        }
        jump.input.push_str(key);
        let outer = usize::from(jump.input.as_bytes()[0] - b'a') * 26;
        if jump.input.len() == 1 {
            if outer < jump.targets.len() {
                view.jump = Some(jump);
            }
            return Ok(());
        }
        let index = outer + usize::from(jump.input.as_bytes()[1] - b'a');
        let Some(target) = jump.targets.get(index) else {
            return Ok(());
        };
        let ids = view.item_ids();
        let Some(target_index) = ids.iter().position(|id| id == target) else {
            return Ok(());
        };
        if view.location.mode == ViewMode::Select {
            let anchor = view
                .location
                .selection
                .anchor
                .as_ref()
                .and_then(|id| ids.iter().position(|other| other == id))
                .unwrap_or(target_index);
            view.location.selection.selected =
                ids[anchor.min(target_index)..=anchor.max(target_index)].to_vec();
        } else {
            view.location.selection.selected = vec![target.clone()];
            view.location.selection.anchor = Some(target.clone());
        }
        view.location.selection.primary = Some(target.clone());
        view.location.scroll_mode = ScrollMode::FollowSelection;
        self.dispatch(
            self.focus,
            "select",
            serde_json::to_value(SelectionChange {
                navigation: Some(ItemNavigation::Jump),
            })?,
        )
    }
}
