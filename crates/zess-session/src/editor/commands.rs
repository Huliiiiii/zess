use super::*;

macro_rules! native_commands {
    ($kind:ident, $table:ident, { $( $variant:ident => ($id:literal, $description:literal) ),+ $(,)? }) => {
        #[derive(Clone, Copy, PartialEq)]
        pub(super) enum $kind {
            $( $variant, )+
        }

        impl $kind {
            fn parse(id: &str) -> Option<Self> {
                match id {
                    $( $id => Some(Self::$variant), )+
                    _ => None,
                }
            }
        }

        const $table: &[(&str, &str)] = &[
            $( ($id, $description), )+
        ];
    };
}

native_commands!(EditorCommand, EDITOR_COMMANDS, {
    Open => ("open", "Open a file"),
    Write => ("write", "Save the current buffer"),
    WriteQuit => ("write-quit", "Save and close the view"),
    Quit => ("quit", "Close the current view"),
    QuitForce => ("quit!", "Close the view without saving"),
    Exit => ("exit", "Stop the server"),
    New => ("new", "Open a scratch buffer"),
    Vsplit => ("vsplit", "Split vertically"),
    Hsplit => ("hsplit", "Split horizontally"),
    Buffer => ("buffer", "Switch to a buffer"),
    Buffers => ("buffers", "List buffer IDs"),
    BufferNext => ("buffer-next", "Next buffer"),
    BufferPrevious => ("buffer-previous", "Previous buffer"),
    Reload => ("reload", "Discard changes and reload the current file"),
    SetLanguage => ("set-language", "Set or show the current buffer language"),
    ConfigReload => ("config-reload", "Restart plugin scripts"),
    CommandPrompt => ("command-prompt", "Enter a command"),
    Help => ("help", "Show buffer keys"),
    KeySequenceCancel => ("key-sequence-cancel", "Cancel key sequence"),
    FocusLeft => ("focus-left", "Focus left"),
    FocusRight => ("focus-right", "Focus right"),
    FocusUp => ("focus-up", "Focus up"),
    FocusDown => ("focus-down", "Focus down"),
    FocusNext => ("focus-next", "Focus next window"),
    BrowserOpenSource => ("browser.open-source", "Finish browsing and open source"),
});

native_commands!(DataCommand, DATA_COMMANDS, {
    SelectRegister => ("data.select-register", "Choose the register for the next command"),
    Filter => ("data.filter", "Filter items"),
    SelectMode => ("data.select-mode", "Toggle selection mode"),
    Escape => ("data.escape", "Reset selection"),
    CopySelectionDown => ("data.copy-selection-down", "Add next item to selection"),
    KeepPrimary => ("data.keep-primary", "Keep primary selection"),
    SelectAll => ("data.select-all", "Select all items"),
    MoveDown => ("data.move-down", "Move selection down"),
    MoveUp => ("data.move-up", "Move selection up"),
    GotoWord => ("data.goto-word", "Jump to a two-character item label"),
    GotoFirst => ("data.goto-first", "First item"),
    GotoLast => ("data.goto-last", "Last item"),
    PageHalfDown => ("data.page-half-down", "Half-page down"),
    PageHalfUp => ("data.page-half-up", "Half-page up"),
    ExtendLines => ("data.extend-lines", "Extend selection down"),
    Refresh => ("data.refresh", "Refresh buffer"),
});

native_commands!(HelpCommand, HELP_COMMANDS, {
    Pin => ("help.pin", "Keep help in a split"),
    Close => ("help.close", "Close help"),
    ScrollDown => ("help.scroll-down", "Scroll help down"),
    ScrollUp => ("help.scroll-up", "Scroll help up"),
    PageHalfDown => ("help.page-half-down", "Scroll help half-page down"),
    PageHalfUp => ("help.page-half-up", "Scroll help half-page up"),
});

native_commands!(PreviewCommand, PREVIEW_COMMANDS, {
    DiffSearchForward => ("diff.search-forward", "Search diff forward"),
    DiffSearchBackward => ("diff.search-backward", "Search diff backward"),
    DiffSearchNext => ("diff.search-next", "Next diff search match"),
    DiffSearchPrevious => ("diff.search-previous", "Previous diff search match"),
    DiffMoveNext => ("diff.move-next", "Move selection down"),
    DiffMovePrevious => ("diff.move-previous", "Move selection up"),
    DiffGotoFirst => ("diff.goto-first", "First diff line"),
    DiffGotoLast => ("diff.goto-last", "Last diff line"),
    DiffPageHalfDown => ("diff.page-half-down", "Half-page down"),
    DiffPageHalfUp => ("diff.page-half-up", "Half-page up"),
    DiffToggleSelection => ("diff.toggle-selection", "Switch line / hunk selection"),
    DiffRangeSelection => ("diff.range-selection", "Toggle range selection"),
    DiffSelectHunk => ("diff.select-hunk", "Select current change block"),
    DiffApplySelection => ("diff.apply-selection", "Stage / unstage selected diff lines"),
    DiffHunkNext => ("diff.hunk-next", "Next change block"),
    DiffHunkPrevious => ("diff.hunk-previous", "Previous change block"),
    DiffEditSource => ("diff.edit-source", "Edit source at this line"),
    MoveLeft => ("preview.move-left", "Scroll preview left"),
    MoveDown => ("preview.move-down", "Scroll preview down"),
    MoveUp => ("preview.move-up", "Scroll preview up"),
    MoveRight => ("preview.move-right", "Scroll preview right"),
    Focus => ("preview.focus", "Focus preview"),
    OpenSource => ("preview.open-source", "Open preview source"),
    Return => ("preview.return", "Return from preview to its browser"),
});

native_commands!(PromptCommand, PROMPT_COMMANDS, {
    Submit => ("prompt.submit", "Submit prompt"),
    Cancel => ("prompt.cancel", "Cancel prompt"),
    DeleteBackward => ("prompt.delete-backward", "Delete previous character"),
    Clear => ("prompt.clear", "Clear prompt"),
    CompleteNext => ("prompt.complete-next", "Next command completion"),
    CompletePrevious => ("prompt.complete-previous", "Previous command completion"),
    CompleteLeft => ("prompt.complete-left", "Previous command completion"),
    CompleteRight => ("prompt.complete-right", "Next command completion"),
    CompleteUp => ("prompt.complete-up", "Previous completion row"),
    CompleteDown => ("prompt.complete-down", "Next completion row"),
    MoveLeft => ("prompt.move-left", "Move input left"),
    MoveRight => ("prompt.move-right", "Move input right"),
    MoveHome => ("prompt.move-home", "Move to start of input"),
    MoveEnd => ("prompt.move-end", "Move to end of input"),
    DeleteForward => ("prompt.delete-forward", "Delete next character"),
    ToggleGlob => ("prompt.toggle-glob", "Toggle glob matching"),
    ToggleRegex => ("prompt.toggle-regex", "Toggle regex matching"),
});

const COUNT_COMMANDS: &[(u8, &str, &str)] = &[
    (0, "data.count-0", "Append count digit 0"),
    (1, "data.count-1", "Append count digit 1"),
    (2, "data.count-2", "Append count digit 2"),
    (3, "data.count-3", "Append count digit 3"),
    (4, "data.count-4", "Append count digit 4"),
    (5, "data.count-5", "Append count digit 5"),
    (6, "data.count-6", "Append count digit 6"),
    (7, "data.count-7", "Append count digit 7"),
    (8, "data.count-8", "Append count digit 8"),
    (9, "data.count-9", "Append count digit 9"),
];

pub(super) fn command_line_infos() -> impl Iterator<Item = (&'static str, &'static str)> {
    EDITOR_COMMANDS.iter().copied().filter(|(id, _)| {
        !matches!(
            *id,
            "command-prompt" | "key-sequence-cancel" | "browser.open-source"
        )
    })
}

pub(super) fn command_infos() -> impl Iterator<Item = (&'static str, &'static str)> {
    EDITOR_COMMANDS
        .iter()
        .chain(DATA_COMMANDS)
        .chain(HELP_COMMANDS)
        .chain(PREVIEW_COMMANDS)
        .chain(PROMPT_COMMANDS)
        .chain(TEXT_COMMANDS)
        .copied()
        .chain(
            COUNT_COMMANDS
                .iter()
                .map(|(_, id, description)| (*id, *description)),
        )
}

#[derive(Clone, Copy)]
pub(super) enum NativeAction {
    Text(TextCommand),
    Prompt(PromptCommand),
    Data(DataCommand),
    Help(HelpCommand),
    Preview(PreviewCommand),
    Count(u8),
    Editor(EditorCommand),
}

impl NativeAction {
    pub(super) fn resolve(name: &str) -> Result<Self> {
        let name = match ALIASES.iter().find(|(alias, _)| *alias == name) {
            Some((_, command)) => command,
            None => name,
        };
        if let Some(command) = TextCommand::parse(name) {
            return Ok(Self::Text(command));
        }
        if let Some(command) = PromptCommand::parse(name) {
            return Ok(Self::Prompt(command));
        }
        if let Some(command) = DataCommand::parse(name) {
            return Ok(Self::Data(command));
        }
        if let Some(command) = HelpCommand::parse(name) {
            return Ok(Self::Help(command));
        }
        if let Some(command) = PreviewCommand::parse(name) {
            return Ok(Self::Preview(command));
        }
        if let Some(command) = EditorCommand::parse(name) {
            return Ok(Self::Editor(command));
        }
        if let Some((digit, _, _)) = COUNT_COMMANDS.iter().find(|(_, id, _)| *id == name) {
            return Ok(Self::Count(*digit));
        }
        bail!("unknown native action {name}")
    }
}

pub(super) const ALIASES: &[(&str, &str)] = &[
    ("e", "open"),
    ("edit", "open"),
    ("o", "open"),
    ("w", "write"),
    ("q", "quit"),
    ("q!", "quit!"),
    ("wq", "write-quit"),
    ("x", "write-quit"),
    ("xit", "write-quit"),
    ("n", "new"),
    ("vs", "vsplit"),
    ("sp", "hsplit"),
    ("hs", "hsplit"),
    ("split", "hsplit"),
    ("b", "buffer"),
    ("bn", "buffer-next"),
    ("bnext", "buffer-next"),
    ("bp", "buffer-previous"),
    ("bprev", "buffer-previous"),
    ("rl", "reload"),
    ("lang", "set-language"),
];

impl Session {
    pub(super) fn native_command(
        &mut self,
        origin: ViewId,
        action: NativeAction,
        argument: Value,
    ) -> Result<()> {
        let command = match action {
            NativeAction::Text(command) => {
                if origin != self.focus
                    && matches!(
                        command,
                        TextCommand::GotoWord
                            | TextCommand::SelectRegex
                            | TextCommand::SplitRegex
                            | TextCommand::SearchForward
                            | TextCommand::SearchBackward
                            | TextCommand::FindForward
                            | TextCommand::FindBackward
                            | TextCommand::TillForward
                            | TextCommand::TillBackward
                            | TextCommand::ReplaceCharacter
                            | TextCommand::SelectRegister
                            | TextCommand::TextObjectInside
                            | TextCommand::TextObjectAround
                            | TextCommand::SurroundAdd
                            | TextCommand::SurroundReplace
                            | TextCommand::SurroundDelete
                    )
                {
                    bail!("text input requires the focused view");
                }
                let buffer = self.views[&origin].location.buffer;
                let effect = self
                    .buffers
                    .get_mut(&buffer)
                    .and_then(Buffer::text)
                    .context("command requires a text buffer")?
                    .execute(origin, command)?;
                return self.text_effect_for(origin, effect);
            }
            NativeAction::Prompt(command) => {
                anyhow::ensure!(
                    origin == self.focus,
                    "prompt commands require the focused view"
                );
                return self.prompt_command(command);
            }
            NativeAction::Data(command) => return self.data_command(origin, command),
            NativeAction::Help(command) => {
                anyhow::ensure!(
                    origin == self.focus,
                    "help interaction requires the focused view"
                );
                return self.help_command(command);
            }
            NativeAction::Preview(command) => {
                if origin != self.focus
                    && matches!(
                        command,
                        PreviewCommand::DiffSearchForward | PreviewCommand::DiffSearchBackward
                    )
                {
                    bail!("preview interaction requires the focused view");
                }
                return match command {
                    PreviewCommand::DiffSearchForward => self.begin_diff_search(true),
                    PreviewCommand::DiffSearchBackward => self.begin_diff_search(false),
                    PreviewCommand::DiffSearchNext => self.search_diff(origin, true),
                    PreviewCommand::DiffSearchPrevious => self.search_diff(origin, false),
                    PreviewCommand::DiffMoveNext => self.move_diff(origin, true, false),
                    PreviewCommand::DiffMovePrevious => self.move_diff(origin, false, false),
                    PreviewCommand::DiffGotoFirst
                    | PreviewCommand::DiffGotoLast
                    | PreviewCommand::DiffPageHalfDown
                    | PreviewCommand::DiffPageHalfUp => self.navigate_diff(origin, command),
                    PreviewCommand::DiffToggleSelection => self.toggle_diff_selection(origin),
                    PreviewCommand::DiffRangeSelection => self.range_diff_selection(origin),
                    PreviewCommand::DiffSelectHunk => self.select_diff_hunk(origin),
                    PreviewCommand::DiffApplySelection => self.apply_diff_selection(origin),
                    PreviewCommand::DiffHunkNext => self.move_diff(origin, true, true),
                    PreviewCommand::DiffHunkPrevious => self.move_diff(origin, false, true),
                    PreviewCommand::DiffEditSource => self.edit_diff_source(origin),
                    PreviewCommand::OpenSource => self.open_preview_source(origin),
                    PreviewCommand::MoveLeft => self.move_preview(origin, ViewDirection::Left),
                    PreviewCommand::MoveRight => self.move_preview(origin, ViewDirection::Right),
                    PreviewCommand::MoveUp => self.move_preview(origin, ViewDirection::Up),
                    PreviewCommand::MoveDown => self.move_preview(origin, ViewDirection::Down),
                    PreviewCommand::Focus => self.focus_preview(origin),
                    PreviewCommand::Return => self.return_from_preview(origin),
                };
            }
            NativeAction::Count(digit) => {
                let view = self.views.get_mut(&origin).unwrap();
                if !matches!(
                    self.buffers[&view.location.buffer],
                    Buffer::Data { .. } | Buffer::Items { .. }
                ) {
                    bail!("command requires an item buffer");
                }
                view.count.push(digit);
                return Ok(());
            }
            NativeAction::Editor(command) => command,
        };
        if command == EditorCommand::BrowserOpenSource {
            return self.open_browser_source(origin, argument);
        }
        if origin != self.focus
            && matches!(
                command,
                EditorCommand::CommandPrompt
                    | EditorCommand::Help
                    | EditorCommand::KeySequenceCancel
                    | EditorCommand::FocusLeft
                    | EditorCommand::FocusDown
                    | EditorCommand::FocusUp
                    | EditorCommand::FocusRight
                    | EditorCommand::FocusNext
            )
        {
            bail!("editor interaction requires the focused view");
        }
        let argument = argument
            .get("argument")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        match command {
            EditorCommand::CommandPrompt => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Command,
                    text: String::new(),
                    target: super::PromptTarget {
                        view: self.focus,
                        buffer: self.views[&self.focus].location.buffer,
                    },
                    completion: None,
                    error: None,
                    cursor: 0,
                })
            }
            EditorCommand::Help => self.show_help(),
            EditorCommand::KeySequenceCancel => {
                self.key_sequence.clear();
                self.views.get_mut(&self.focus).unwrap().count.clear();
            }
            EditorCommand::FocusLeft => self.focus_direction(ViewDirection::Left)?,
            EditorCommand::FocusDown => self.focus_direction(ViewDirection::Down)?,
            EditorCommand::FocusUp => self.focus_direction(ViewDirection::Up)?,
            EditorCommand::FocusRight => self.focus_direction(ViewDirection::Right)?,
            EditorCommand::FocusNext => self.focus_next(1)?,
            EditorCommand::Exit => {
                self.request_stop(false)?;
            }
            EditorCommand::Quit => self.close_view_from(origin, false)?,
            EditorCommand::QuitForce => self.close_view_from(origin, true)?,
            EditorCommand::Write => {
                let buffer = self.views[&origin].location.buffer;
                let file_layer =
                    matches!(self.views.layer_content(origin), Some(LayerContent::File));
                match self.buffers.get_mut(&buffer).unwrap() {
                    Buffer::Text(_) if self.views.parent(origin).is_some() && !file_layer => {
                        self.close_view_from(origin, false)?
                    }
                    Buffer::Text(text) => {
                        let path = (!argument.is_empty()).then(|| self.workspace.join(argument));
                        text.save(path.as_deref())?;
                        self.operation_outcome = Some(zess_protocol::CommandOutcome::Saved {
                            buffer_id: buffer,
                            version: text.version(),
                        });
                        self.services.borrow_mut().invalidate_fs();
                        self.message = format!("{} written", text.path().unwrap().display());
                    }
                    Buffer::Data { .. } => self.dispatch(origin, "save", Value::Null)?,
                    _ => bail!("buffer is read only"),
                }
                if let Err(error) = self.rebind_file_watch(buffer) {
                    self.message = format!("buffer saved; file watch: {error:#}");
                }
            }
            EditorCommand::WriteQuit => {
                let original_buffer = self.views[&origin].location.buffer;
                self.native_command(
                    origin,
                    NativeAction::Editor(EditorCommand::Write),
                    json!({"argument": argument}),
                )?;
                if self
                    .views
                    .get(&origin)
                    .is_some_and(|view| view.location.buffer == original_buffer)
                {
                    self.close_view_from(origin, false)?;
                }
            }
            EditorCommand::Open => {
                self.ensure_buffer_switch_allowed(origin)?;
                let buffer = self.load_file(Path::new(argument))?;
                self.switch_buffer_with(origin, buffer, Value::Null)?;
            }
            EditorCommand::New => {
                self.ensure_buffer_switch_allowed(origin)?;
                let text = TextBuffer::with_capabilities(
                    "",
                    self.services.borrow().file_system(),
                    self.syntax_provider.clone(),
                );
                let buffer = self.add_buffer(Buffer::Text(text));
                self.switch_buffer_with(origin, buffer, Value::Null)?;
            }
            EditorCommand::Vsplit => self.split(origin, Axis::Vertical)?,
            EditorCommand::Hsplit => self.split(origin, Axis::Horizontal)?,
            EditorCommand::BufferNext | EditorCommand::BufferPrevious => {
                self.ensure_buffer_switch_allowed(origin)?;
                let owner = self.preview_source(origin).unwrap_or(origin);
                self.close_plugin_navigation(self.views.page_id(owner), false)?;
                let ids = self
                    .buffers
                    .keys()
                    .copied()
                    .filter(|id| self.is_switchable_buffer(*id))
                    .collect::<Vec<_>>();
                let buffer = self.views[&owner].location.buffer;
                let next = if command == EditorCommand::BufferPrevious {
                    ids.iter().rev().find(|&&id| id < buffer).or(ids.last())
                } else {
                    ids.iter().find(|&&id| id > buffer).or(ids.first())
                }
                .copied()
                .context("no selectable editor buffers")?;
                self.switch_buffer_with(owner, next, Value::Null)?;
            }
            EditorCommand::Buffer => {
                let id = argument.parse()?;
                if !self.is_switchable_buffer(id) {
                    bail!("buffer {id} is not a selectable editor buffer");
                }
                self.switch_buffer_with(origin, id, Value::Null)?;
            }
            EditorCommand::Buffers => {
                self.message = self
                    .buffers
                    .keys()
                    .filter(|id| self.is_switchable_buffer(**id))
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            EditorCommand::SetLanguage => {
                let buffer = self.views[&origin].location.buffer;
                let text = self
                    .buffers
                    .get_mut(&buffer)
                    .and_then(Buffer::text)
                    .context("command requires a text buffer")?;
                if argument.is_empty() {
                    self.message = text.language_id().unwrap_or("text").to_owned();
                } else {
                    text.set_language((argument != "text").then_some(argument))?;
                }
            }
            EditorCommand::Reload => {
                let buffer = self.views[&origin].location.buffer;
                let Some(Buffer::Text(text)) = self.buffers.get_mut(&buffer) else {
                    bail!("command requires a file buffer");
                };
                text.reload(origin)?;
                self.rebind_file_watch(buffer)?;
                self.services.borrow_mut().invalidate_fs();
                self.message = "File reloaded".into();
                self.refresh_context_plugins()?;
            }
            EditorCommand::ConfigReload => {
                if self
                    .buffers
                    .values()
                    .any(|buffer| matches!(buffer, Buffer::Data { kind, modified: true, .. } if !self.scripts.as_ref().is_some_and(|runtime|runtime.is_native_buffer(kind))))
                {
                    bail!("save or undo pending buffer changes before reloading");
                }
                if !self.reload() {
                    return Ok(());
                }
                self.cancel_input_context()?;
                let ids = self
                    .views
                    .iter()
                    .filter_map(|(id, view)| {
                        matches!(self.buffers[&view.location.buffer], Buffer::Data { .. })
                            .then_some(*id)
                    })
                    .collect::<Vec<_>>();
                for id in ids {
                    let has_layer = self.views.child(id).is_some();
                    let view = self.views.get_mut(&id).unwrap();
                    let kind = match &self.buffers[&view.location.buffer] {
                        Buffer::Data { kind, .. } => kind,
                        _ => unreachable!(),
                    };
                    if self
                        .scripts
                        .as_ref()
                        .is_some_and(|runtime| runtime.is_native_buffer(kind))
                    {
                        self.dispatch(id, "render", Value::Null)?;
                        continue;
                    }
                    if !has_layer {
                        view.location.memories.clear();
                    }
                    view.location.mode = ViewMode::Normal;
                    if self.scripts.is_some() {
                        self.dispatch(id, "init", Value::Null)?;
                    }
                }
                self.emit_event("reload", json!({"generation":self.generation}))?;
            }
            EditorCommand::BrowserOpenSource => {
                unreachable!("handled before parsing command arguments")
            }
        }
        Ok(())
    }
}
