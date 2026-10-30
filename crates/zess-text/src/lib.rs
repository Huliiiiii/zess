//! Helix-core backed editing primitives for Zess terminal views.
//!
//! Editing, grapheme movement, selection mapping, and branching history use
//! Helix's MPL-2.0 core at the revision pinned in Cargo.toml.
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use helix_core::{
    Assoc, Range, Rope, Selection, SmallVec, Transaction, char_idx_at_visual_offset,
    chars::char_is_word,
    comment,
    doc_formatter::{DocumentFormatter, FormattedGrapheme, TextFormat},
    graphemes::{Grapheme, next_grapheme_boundary, prev_grapheme_boundary},
    history::{History, State},
    movement::{self, Direction, Movement},
    surround,
    syntax::{Highlight, HighlightEvent, OverlayHighlighter, OverlayHighlights},
    text_annotations::TextAnnotations,
    textobject::{self, TextObject},
    unicode::segmentation::UnicodeSegmentation,
    visual_offset_from_block,
};
use helix_stdx::rope::RopeSliceExt;
use unicode_width::UnicodeWidthStr;
use zess_plugin::{CommandCount, OutlineSymbol};
use zess_ui::{FrameRow, Span};

mod cursor;
pub use cursor::Cursor;

mod language;
pub mod runtime;

use language::LanguageAttachment;

#[cfg(test)]
mod tests;

macro_rules! text_commands {
    ($( $variant:ident => ($id:literal, $description:literal) ),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub enum TextCommand {
            $( $variant, )+
        }

        impl TextCommand {
            pub fn parse(id: &str) -> Option<Self> {
                match id {
                    $( $id => Some(Self::$variant), )+
                    _ => None,
                }
            }

            pub fn id(self) -> &'static str {
                match self {
                    $( Self::$variant => $id, )+
                }
            }
        }

        /// Named editing operations available to configurable keymaps and command callers.
        pub const TEXT_COMMANDS: &[(&str, &str)] = &[
            $( ($id, $description), )+
        ];
    };
}

text_commands! {
    Append => ("text.append", "Enter insert mode after selections"),
    AppendLineEnd => ("text.append-line-end", "Enter insert mode at line ends"),
    Change => ("text.change", "Yank and replace selections in insert mode"),
    ChangeWithoutYank => ("text.change-without-yank", "Replace selections in insert mode without yanking"),
    CollapseSelection => ("text.collapse-selection", "Collapse selections to their cursors"),
    CopySelectionDown => ("text.copy-selection-down", "Copy each selection to following matching lines"),
    Count0 => ("text.count-0", "Append 0 to the command count"),
    Count1 => ("text.count-1", "Append 1 to the command count"),
    Count2 => ("text.count-2", "Append 2 to the command count"),
    Count3 => ("text.count-3", "Append 3 to the command count"),
    Count4 => ("text.count-4", "Append 4 to the command count"),
    Count5 => ("text.count-5", "Append 5 to the command count"),
    Count6 => ("text.count-6", "Append 6 to the command count"),
    Count7 => ("text.count-7", "Append 7 to the command count"),
    Count8 => ("text.count-8", "Append 8 to the command count"),
    Count9 => ("text.count-9", "Append 9 to the command count"),
    Delete => ("text.delete", "Yank and delete selections"),
    DeleteWithoutYank => ("text.delete-without-yank", "Delete selections without yanking"),
    DeleteBackward => ("text.delete-backward", "Delete previous graphemes in insert mode"),
    DeleteForward => ("text.delete-forward", "Delete next graphemes in insert mode"),
    Escape => ("text.escape", "Enter normal mode and cancel pending input"),
    ExtendLines => ("text.extend-lines", "Select complete lines containing selections"),
    FindBackward => ("text.find-backward", "Select to the previous occurrence of a character"),
    FindForward => ("text.find-forward", "Select to the next occurrence of a character"),
    GotoFileEnd => ("text.goto-file-end", "Go to the last line, or the counted line"),
    GotoFileStart => ("text.goto-file-start", "Go to the first line, or the counted line"),
    GotoFirstNonwhitespace => ("text.goto-first-nonwhitespace", "Go to the first nonwhitespace character of each line"),
    GotoLineEnd => ("text.goto-line-end", "Go to each line end"),
    GotoLineStart => ("text.goto-line-start", "Go to each line start"),
    GotoWord => ("text.goto-word", "Jump to a two-character word label"),
    Insert => ("text.insert", "Enter insert mode before selections"),
    InsertLineStart => ("text.insert-line-start", "Enter insert mode at first nonwhitespace characters"),
    InsertNewline => ("text.insert-newline", "Insert newlines with indentation"),
    InsertTab => ("text.insert-tab", "Insert indentation"),
    Indent => ("text.indent", "Indent selected lines"),
    Unindent => ("text.unindent", "Unindent selected lines"),
    Join => ("text.join", "Join selected lines"),
    JoinSelectSpace => ("text.join-select-space", "Join lines and select inserted spaces"),
    SwitchCase => ("text.switch-case", "Switch the case of selected text"),
    Lowercase => ("text.lowercase", "Convert selected text to lowercase"),
    Uppercase => ("text.uppercase", "Convert selected text to uppercase"),
    FlipSelections => ("text.flip-selections", "Swap selection anchors and cursors"),
    SplitLines => ("text.split-lines", "Split selections on newlines"),
    MergeSelections => ("text.merge-selections", "Merge all selections"),
    SurroundAdd => ("text.surround-add", "Surround selections"),
    SurroundReplace => ("text.surround-replace", "Replace surrounding pairs"),
    SurroundDelete => ("text.surround-delete", "Delete surrounding pairs"),
    ToggleComments => ("text.toggle-comments", "Toggle line comments"),
    ToggleBlockComments => ("text.toggle-block-comments", "Toggle block comments"),
    ToggleLineComments => ("text.toggle-line-comments", "Toggle line comments"),
    MatchBrackets => ("text.match-brackets", "Move to matching brackets"),
    KeepPrimary => ("text.keep-primary", "Keep only the primary selection"),
    MoveDown => ("text.move-down", "Move down"),
    MoveLeft => ("text.move-left", "Move left"),
    MoveLongWordEndForward => ("text.move-long-word-end-forward", "Move to the next long word end"),
    MoveLongWordStartBackward => ("text.move-long-word-start-backward", "Move to the previous long word start"),
    MoveLongWordStartForward => ("text.move-long-word-start-forward", "Move to the next long word start"),
    MoveRight => ("text.move-right", "Move right"),
    MoveUp => ("text.move-up", "Move up"),
    MoveWordEndForward => ("text.move-word-end-forward", "Move to the next word end"),
    MoveWordStartBackward => ("text.move-word-start-backward", "Move to the previous word start"),
    MoveWordStartForward => ("text.move-word-start-forward", "Move to the next word start"),
    OpenAbove => ("text.open-above", "Insert lines above and enter insert mode"),
    OpenBelow => ("text.open-below", "Insert lines below and enter insert mode"),
    PageHalfDown => ("text.page-half-down", "Move down half a viewport"),
    PageHalfUp => ("text.page-half-up", "Move up half a viewport"),
    PasteAfter => ("text.paste-after", "Paste register contents after selections"),
    PasteBefore => ("text.paste-before", "Paste register contents before selections"),
    Redo => ("text.redo", "Redo edits"),
    RepeatInsert => ("text.repeat-insert", "Repeat the last completed insertion"),
    ReplaceCharacter => ("text.replace-character", "Replace selected graphemes with a character"),
    ReplaceWithYank => ("text.replace-with-yank", "Replace selections with register contents"),
    SearchBackward => ("text.search-backward", "Prompt for a backward regular expression search"),
    SearchForward => ("text.search-forward", "Prompt for a forward regular expression search"),
    SearchNext => ("text.search-next", "Find the next search match"),
    SearchPrevious => ("text.search-previous", "Find the previous search match"),
    SelectAll => ("text.select-all", "Select the whole buffer"),
    SelectLines => ("text.select-lines", "Select and extend complete lines"),
    SelectLinesAbove => ("text.select-lines-above", "Select and extend complete lines above"),
    SelectMode => ("text.select-mode", "Toggle selection extension mode"),
    SelectRegex => ("text.select-regex", "Select regular expression matches within selections"),
    SelectRegister => ("text.select-register", "Choose the register for the next register operation"),
    SplitRegex => ("text.split-regex", "Split selections on regular expression matches"),
    TextObjectAround => ("text.textobject-around", "Select around a text object"),
    TextObjectInside => ("text.textobject-inside", "Select inside a text object"),
    TillBackward => ("text.till-backward", "Select until before the previous occurrence of a character"),
    TillForward => ("text.till-forward", "Select until before the next occurrence of a character"),
    Undo => ("text.undo", "Undo edits"),
    Yank => ("text.yank", "Copy selections to a register"),
    YankToClipboard => ("text.yank-to-clipboard", "Copy selections to the system clipboard"),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum TextMode {
    #[default]
    Normal,
    Insert,
    Select,
    Custom(String),
}

impl TextMode {
    pub fn parse(mode: &str) -> Self {
        match mode {
            "normal" => Self::Normal,
            "insert" => Self::Insert,
            "select" => Self::Select,
            _ => Self::Custom(mode.into()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Normal => "normal",
            Self::Insert => "insert",
            Self::Select => "select",
            Self::Custom(name) => name,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextPromptKind {
    Select,
    Split,
    SearchForward,
    SearchBackward,
}

impl TextPromptKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Split => "split",
            Self::SearchForward => "search",
            Self::SearchBackward => "reverse search",
        }
    }
}

#[derive(Clone, Default)]
enum PendingInput {
    #[default]
    None,
    Regex {
        kind: TextPromptKind,
        selection: Selection,
        scroll: usize,
        horizontal_scroll: usize,
        manual_viewport: bool,
        input: String,
    },
    GotoWord {
        words: Vec<Range>,
        first: Option<u8>,
    },
    Find {
        direction: Direction,
        till: bool,
    },
    ReplaceCharacter,
    Register,
    TextObject(TextObject),
    SurroundAdd,
    SurroundReplace,
    SurroundDelete,
    SurroundReplaceWith(Vec<usize>),
}

impl PendingInput {
    fn label(&self) -> &'static str {
        match self {
            Self::None => "",
            Self::Regex { kind, .. } => kind.label(),
            Self::GotoWord { .. } => "Goto word: type a two-letter label",
            Self::Find {
                direction: Direction::Forward,
                till: false,
            } => "Find forward: type a character",
            Self::Find {
                direction: Direction::Backward,
                till: false,
            } => "Find backward: type a character",
            Self::Find {
                direction: Direction::Forward,
                till: true,
            } => "Till forward: type a character",
            Self::Find {
                direction: Direction::Backward,
                till: true,
            } => "Till backward: type a character",
            Self::ReplaceCharacter => "Replace: type a character",
            Self::Register => "Register: type a name",
            Self::TextObject(TextObject::Inside) => "Inside: type a text object",
            Self::TextObject(TextObject::Around) => "Around: type a text object",
            Self::TextObject(TextObject::Movement) => "Type a text object",
            Self::SurroundAdd => "Add surround: type a delimiter",
            Self::SurroundReplace => "Replace surround: type the old delimiter",
            Self::SurroundDelete => "Delete surround: type a delimiter",
            Self::SurroundReplaceWith(_) => "Replace surround: type the new delimiter",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum TextInput {
    Character(char),
    Escape,
    Enter,
    Backspace,
    Unsupported,
}

impl TextInput {
    fn parse(key: &str) -> Self {
        match key {
            "Esc" => Self::Escape,
            "Enter" | "Return" => Self::Enter,
            "Backspace" => Self::Backspace,
            "Space" => Self::Character(' '),
            _ => {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) => Self::Character(ch),
                    _ => Self::Unsupported,
                }
            }
        }
    }

    fn character(self) -> Option<char> {
        match self {
            Self::Character(ch) => Some(ch),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct TextView {
    selection: Selection,
    mode: TextMode,
    cursor: (usize, usize),
    scroll: usize,
    horizontal_scroll: usize,
    manual_viewport: bool,
    viewport_width: usize,
    viewport_height: usize,
    viewport_version: u64,
    count: CommandCount,
    pending: PendingInput,
    register: Option<char>,
    prompt_register: char,
    insert_recording: Option<InsertReplay>,
}

impl Default for TextView {
    fn default() -> Self {
        Self {
            selection: Selection::point(0),
            mode: TextMode::Normal,
            cursor: (0, 0),
            scroll: 0,
            horizontal_scroll: 0,
            manual_viewport: false,
            viewport_width: 0,
            viewport_height: 1,
            viewport_version: 0,
            count: CommandCount::default(),
            pending: PendingInput::None,
            register: None,
            prompt_register: '/',
            insert_recording: None,
        }
    }
}

impl TextView {
    /// Name used by plugin keymaps and status displays.
    pub fn mode(&self) -> &str {
        self.mode.as_str()
    }
    pub fn text_mode(&self) -> TextMode {
        self.mode.clone()
    }
    pub fn command_count(&self) -> CommandCount {
        self.count
    }
    pub fn selection_count(&self) -> usize {
        self.selection.len()
    }
    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    fn cancel_pending_input(&mut self) {
        if let PendingInput::Regex {
            selection,
            scroll,
            horizontal_scroll,
            manual_viewport,
            ..
        } = std::mem::take(&mut self.pending)
        {
            self.selection = selection;
            self.scroll = scroll;
            self.horizontal_scroll = horizontal_scroll;
            self.manual_viewport = manual_viewport;
        }
        self.count.clear();
        self.register = None;
    }
}

#[derive(Default, Debug)]
pub struct TextEffect {
    pub changed: bool,
    pub prompt: Option<TextPromptKind>,
    pub message: Option<String>,
    pub clipboard: Option<String>,
    pub primary_clipboard: Option<String>,
    pub clipboard_read: Option<ClipboardRead>,
}

#[derive(Debug)]
pub struct ClipboardRead {
    pub register: char,
    pub command: TextCommand,
    pub count: usize,
}

pub struct RenderResult {
    /// Character positions for rendered cells, produced alongside the grapheme layout.
    pub hit_positions: Vec<Vec<usize>>,
    pub rows: Vec<FrameRow>,
    pub cursor: Option<(u16, u16)>,
    pub scroll: usize,
}

/// Registers are shared by all buffers belonging to one editor session.
#[derive(Clone)]
pub struct Registers {
    values: Arc<Mutex<HashMap<char, Vec<String>>>>,
    search_register: Arc<Mutex<char>>,
}

impl Registers {
    pub fn search_pattern(&self) -> Option<String> {
        let register = *self.search_register.lock().unwrap();
        self.values
            .lock()
            .unwrap()
            .get(&register)
            .and_then(|values| values.first().cloned())
    }

    pub fn set_search_pattern(&self, pattern: String) {
        self.values.lock().unwrap().insert('/', vec![pattern]);
        *self.search_register.lock().unwrap() = '/';
    }

    pub fn write(&self, register: char, values: Vec<String>) -> Result<Option<String>> {
        zess_plugin::validate_register_write(register)?;
        match register {
            '_' => {}
            '+' | '*' => {
                let contents = values.join("\n");
                self.values.lock().unwrap().insert(register, values);
                return Ok(Some(contents));
            }
            _ => {
                self.values.lock().unwrap().insert(register, values);
            }
        }
        Ok(None)
    }
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            values: Arc::default(),
            search_register: Arc::new(Mutex::new('/')),
        }
    }
}

/// Completed insertions shared by the text buffers of one editor session.
#[derive(Clone, Default)]
pub struct InsertHistory(Arc<Mutex<Option<InsertReplay>>>);

#[derive(Clone)]
struct InsertReplay {
    command: TextCommand,
    count: usize,
    register: Option<char>,
    events: Vec<InsertEvent>,
}

#[derive(Clone)]
enum InsertEvent {
    Command(TextCommand),
    Input(TextInput),
}

struct EditGroup {
    original: State,
    transaction: Transaction,
    view_id: u64,
}

enum LanguageDetection {
    Automatic,
    Explicit,
}

pub struct TextBuffer {
    rope: Rope,
    line_width: Option<(u64, usize, usize)>,
    preview_wrap: Option<PreviewWrap>,
    path: Option<PathBuf>,
    version: u64,
    history: History,
    saved_revision: usize,
    group: Option<EditGroup>,
    views: HashMap<u64, TextView>,
    registers: Registers,
    insert_history: InsertHistory,
    replaying_insert: bool,
    reading_clipboard: bool,
    readonly: bool,
    language: Option<LanguageAttachment>,
    language_detection: LanguageDetection,
}

impl TextBuffer {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            line_width: None,
            preview_wrap: None,
            path: None,
            version: 0,
            history: History::default(),
            saved_revision: 0,
            group: None,
            views: HashMap::new(),
            registers: Registers::default(),
            insert_history: InsertHistory::default(),
            replaying_insert: false,
            reading_clipboard: false,
            readonly: false,
            language: None,
            language_detection: LanguageDetection::Automatic,
        }
    }

    pub fn new_with_path(text: &str, path: &Path) -> Self {
        let mut buffer = Self::new(text);
        buffer.path = Some(path.to_owned());
        buffer.language = language::detect(path);
        buffer
    }

    /// Builds a preview whose initial syntax parsing runs outside the UI thread.
    pub fn new_preview_with_path(text: &str, path: &Path) -> Self {
        let mut buffer = Self::new_with_path(text, path);
        if let Some(language) = &mut buffer.language {
            language.start_background(&buffer.rope);
        }
        buffer
    }

    /// Adopts completed background syntax and reports a change requiring redraw.
    pub fn poll_syntax(&mut self) -> bool {
        self.language
            .as_mut()
            .is_some_and(|language| language.poll(&self.rope))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        Ok(Self::new_with_path(&text, path))
    }

    pub fn save(&mut self, path: Option<&Path>) -> Result<()> {
        self.commit_group();

        let path = path
            .or(self.path.as_deref())
            .context("buffer has no file name")?
            .to_owned();

        fs::write(&path, self.text()).with_context(|| format!("write {}", path.display()))?;
        if matches!(self.language_detection, LanguageDetection::Automatic) {
            let detected = language::detect(&path);
            if detected.as_ref().map(LanguageAttachment::id) != self.language_id() {
                self.language = detected;
                self.invalidate_viewport_widths();
            }
        }
        self.path = Some(path);
        self.saved_revision = self.history.current_revision();

        Ok(())
    }

    pub fn reload(&mut self, id: u64) -> Result<()> {
        let path = self.path.as_deref().context("buffer has no file name")?;
        let contents =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        self.ensure_view(id);
        self.reload_with_selection(&contents, self.views[&id].selection.clone());
        Ok(())
    }

    /// Applies a disk snapshot to every attached view, including hidden buffers.
    pub fn reload_contents(&mut self, contents: &str) {
        let selection = self
            .views
            .iter()
            .min_by_key(|(id, _)| *id)
            .map_or_else(|| Selection::point(0), |(_, view)| view.selection.clone());
        self.reload_with_selection(contents, selection);
    }

    fn reload_with_selection(&mut self, contents: &str, selection: Selection) {
        self.commit_group();
        if self.rope != contents {
            let original = State {
                doc: self.rope.clone(),
                selection,
            };
            let transaction =
                helix_core::diff::compare_ropes(&self.rope, &Rope::from_str(contents));
            let selection = original.selection.clone().map(transaction.changes());
            self.apply_without_history(None, &transaction, None);
            self.history
                .commit_revision(&transaction.with_selection(selection), &original);
        }
        self.saved_revision = self.history.current_revision();
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }
    pub fn modified(&self) -> bool {
        self.history.current_revision() != self.saved_revision
            || self
                .group
                .as_ref()
                .is_some_and(|g| !g.transaction.changes().is_empty())
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }
    pub fn view(&self, id: u64) -> &TextView {
        &self.views[&id]
    }

    pub fn push_command_count(&mut self, id: u64, digit: u8) {
        self.ensure_view(id);
        self.views.get_mut(&id).unwrap().count.push(digit);
    }

    /// Changes editing mode and clears any partially entered operation.
    pub fn set_mode(&mut self, id: u64, mode: &str) -> Result<()> {
        self.set_text_mode(id, TextMode::parse(mode))
    }

    pub fn set_text_mode(&mut self, id: u64, mode: TextMode) -> Result<()> {
        self.ensure_view(id);

        if self.readonly && mode == TextMode::Insert {
            anyhow::bail!("buffer is read-only");
        }
        self.cancel_pending_command(id);

        let was_insert = self.views[&id].mode == TextMode::Insert;

        if mode != TextMode::Insert {
            self.commit_group();
        } else if !was_insert {
            self.begin_group(id);
        }

        let slice = self.rope.slice(..);
        let view = self.views.get_mut(&id).unwrap();

        if was_insert && mode == TextMode::Normal {
            view.selection = view
                .selection
                .clone()
                .transform(|range| Range::point(prev_grapheme_boundary(slice, range.head)))
                .ensure_invariants(slice);
        } else if was_insert && mode != TextMode::Insert {
            view.selection = view.selection.clone().ensure_invariants(slice);
        } else if !was_insert && mode == TextMode::Insert {
            view.selection = view
                .selection
                .clone()
                .transform(|range| Range::point(range.from()));
        }

        view.mode = mode.clone();
        view.pending = PendingInput::None;
        view.count = CommandCount::default();
        view.register = None;
        if !self.replaying_insert {
            if was_insert && mode != TextMode::Insert {
                if let Some(mut recording) = view.insert_recording.take() {
                    if !matches!(recording.events.last(), Some(InsertEvent::Command(command)) if *command == TextCommand::Escape)
                    {
                        recording
                            .events
                            .push(InsertEvent::Command(TextCommand::Escape));
                    }
                    *self.insert_history.0.lock().unwrap() = Some(recording);
                }
            } else if !was_insert && mode == TextMode::Insert {
                view.insert_recording = Some(InsertReplay {
                    command: TextCommand::Insert,
                    count: 1,
                    register: None,
                    events: Vec::new(),
                });
            }
        }
        self.refresh_cursors();

        Ok(())
    }

    pub fn has_pending_input(&self, id: u64) -> bool {
        self.views
            .get(&id)
            .is_some_and(|view| !matches!(view.pending, PendingInput::None))
    }

    pub fn pending_prompt(&self, id: u64) -> Option<TextPromptKind> {
        match &self.views.get(&id)?.pending {
            PendingInput::Regex { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Cancels pending input, restoring the selection from before a pattern preview.
    pub fn cancel_pending_command(&mut self, id: u64) {
        if let Some(view) = self.views.get_mut(&id) {
            view.cancel_pending_input();
        }
        self.refresh_cursors();
    }

    pub fn pending_input(&self, id: u64) -> String {
        let Some(view) = self.views.get(&id) else {
            return String::new();
        };
        let label = match &view.pending {
            PendingInput::None => {
                return view
                    .register
                    .map(|register| format!("Register \"{register}"))
                    .unwrap_or_default();
            }
            PendingInput::GotoWord {
                first: Some(first), ..
            } => format!(
                "Goto word: {}_ (type the second letter)",
                char::from(b'a' + first)
            ),
            _ => view.pending.label().to_owned(),
        };
        format!("{label} · Esc to cancel")
    }

    pub fn replace_readonly(&mut self, contents: &str) -> Result<()> {
        if !self.readonly {
            anyhow::bail!("cannot replace contents of a writable text buffer");
        }

        if self.rope == contents {
            return Ok(());
        }

        let old_slice = self.rope.slice(..);
        let new_rope = Rope::from_str(contents);
        let new_slice = new_rope.slice(..);
        let remap_position = |position: usize| {
            let old_row = old_slice.char_to_line(position.min(old_slice.len_chars()));
            let old_column = position.saturating_sub(old_slice.line_to_char(old_row));
            let row = old_row.min(new_slice.len_lines().saturating_sub(1));
            let start = new_slice.line_to_char(row);
            let line = new_slice.line(row).to_string();
            let line_length = line.trim_end_matches(['\r', '\n']).chars().count();

            start + old_column.min(line_length)
        };

        for view in self.views.values_mut() {
            view.cancel_pending_input();
            let ranges = view
                .selection
                .iter()
                .map(|range| Range::new(remap_position(range.anchor), remap_position(range.head)))
                .collect();

            view.selection =
                Selection::new(ranges, view.selection.primary_index()).ensure_invariants(new_slice);
            view.scroll = view.scroll.min(new_slice.len_lines().saturating_sub(1));
        }

        if let Some(language) = &mut self.language {
            let transaction = Transaction::change(
                &self.rope,
                std::iter::once((0, self.rope.len_chars(), Some(contents.into()))),
            );
            language.update(&self.rope, &new_rope, transaction.changes());
        }
        self.rope = new_rope;
        self.version += 1;
        self.refresh_cursors();

        Ok(())
    }

    pub fn set_registers(&mut self, registers: Registers) {
        self.registers = registers;
    }

    pub fn complete_clipboard_read(
        &mut self,
        id: u64,
        read: ClipboardRead,
        text: String,
    ) -> Result<TextEffect> {
        self.registers
            .values
            .lock()
            .unwrap()
            .insert(read.register, vec![text]);
        let view = self
            .views
            .get_mut(&id)
            .context("clipboard view no longer exists")?;
        view.register = Some(read.register);
        view.count = read.count.into();
        self.reading_clipboard = true;
        let result = self.execute(id, read.command);
        self.reading_clipboard = false;
        result
    }

    pub fn set_insert_history(&mut self, history: InsertHistory) {
        self.insert_history = history;
    }

    pub fn set_readonly(&mut self, readonly: bool) {
        self.commit_group();
        self.readonly = readonly;

        if readonly {
            let slice = self.rope.slice(..);

            for view in self.views.values_mut() {
                view.cancel_pending_input();
                view.mode = TextMode::Normal;
                view.selection = view.selection.clone().ensure_invariants(slice);
            }
            self.refresh_cursors();
        }
    }

    /// Previews patterns against the selection captured when the prompt opened.
    pub fn preview_prompt(
        &mut self,
        id: u64,
        prompt: TextPromptKind,
        value: &str,
    ) -> Result<TextEffect> {
        self.ensure_view(id);
        if self.pending_prompt(id) != Some(prompt) {
            return Ok(TextEffect::default());
        }
        let view = self.views.get_mut(&id).unwrap();
        if let PendingInput::Regex {
            selection,
            scroll,
            horizontal_scroll,
            manual_viewport,
            ..
        } = &view.pending
        {
            view.selection = selection.clone();
            view.scroll = *scroll;
            view.horizontal_scroll = *horizontal_scroll;
            view.manual_viewport = *manual_viewport;
        }
        if !value.is_empty()
            && let Ok(regex) = self.build_regex(value)
        {
            let movement = if self.views[&id].mode == TextMode::Select {
                Movement::Extend
            } else {
                Movement::Move
            };
            let _ = self.regex_selection(id, prompt, &regex, movement);
        }
        self.refresh_cursors();
        Ok(TextEffect::default())
    }

    pub fn submit_prompt(
        &mut self,
        id: u64,
        prompt: TextPromptKind,
        value: &str,
    ) -> Result<TextEffect> {
        self.ensure_view(id);
        anyhow::ensure!(
            self.pending_prompt(id) == Some(prompt),
            "text prompt is no longer active"
        );

        let view = self.views.get_mut(&id).unwrap();
        if let PendingInput::Regex {
            selection,
            scroll,
            horizontal_scroll,
            manual_viewport,
            ..
        } = &view.pending
        {
            view.selection = selection.clone();
            view.scroll = *scroll;
            view.horizontal_scroll = *horizontal_scroll;
            view.manual_viewport = *manual_viewport;
        }
        let register = view.prompt_register;
        let result = if value.is_empty() {
            Ok(())
        } else {
            let movement = if self.views[&id].mode == TextMode::Select {
                Movement::Extend
            } else {
                Movement::Move
            };
            self.build_regex(value)
                .and_then(|regex| self.regex_selection(id, prompt, &regex, movement))
        };
        self.refresh_cursors();
        result?;
        let clipboard = self.write_register(register, vec![value.into()])?;
        let view = self.views.get_mut(&id).unwrap();
        view.pending = PendingInput::None;
        if matches!(
            prompt,
            TextPromptKind::SearchForward | TextPromptKind::SearchBackward
        ) {
            *self.registers.search_register.lock().unwrap() = register;
        }
        Ok(TextEffect {
            clipboard: if register == '*' {
                None
            } else {
                clipboard.clone()
            },
            primary_clipboard: if register == '*' { clipboard } else { None },
            ..TextEffect::default()
        })
    }

    pub fn ensure_view(&mut self, id: u64) {
        let slice = self.rope.slice(..);

        self.views.entry(id).or_insert_with(|| TextView {
            selection: Selection::point(0).ensure_invariants(slice),
            ..TextView::default()
        });
    }

    pub fn remove_view(&mut self, id: u64) {
        if self.group.as_ref().is_some_and(|group| group.view_id == id) {
            self.commit_group();
        }

        self.views.remove(&id);
    }

    pub fn clone_view(&mut self, from: u64, to: u64) {
        self.ensure_view(from);

        let view = self.views[&from].clone();

        self.views.insert(
            to,
            TextView {
                mode: TextMode::Normal,
                pending: PendingInput::None,
                insert_recording: None,
                selection: view
                    .selection
                    .clone()
                    .ensure_invariants(self.rope.slice(..)),
                ..view
            },
        );
    }

    pub fn goto_line(&mut self, id: u64, line: usize) {
        self.ensure_view(id);

        let pos = self
            .rope
            .line_to_char(line.min(self.rope.len_lines().saturating_sub(1)));
        let selection = Selection::point(pos).ensure_invariants(self.rope.slice(..));

        self.views.get_mut(&id).unwrap().selection = selection;
        self.refresh_cursors();
    }

    pub fn selections(&self, id: u64) -> Vec<Cursor> {
        let Some(view) = self.views.get(&id) else {
            return Vec::new();
        };
        let slice = self.rope.slice(..);

        view.selection
            .iter()
            .map(|range| {
                let pos = range.cursor(slice);
                let row = slice.char_to_line(pos);
                let anchor = if range.anchor > range.head {
                    prev_grapheme_boundary(slice, range.anchor)
                } else {
                    range.anchor
                };
                let anchor_row = slice.char_to_line(anchor);

                Cursor {
                    row,
                    column: pos - slice.line_to_char(row),
                    anchor_row,
                    anchor_column: anchor - slice.line_to_char(anchor_row),
                }
            })
            .collect()
    }

    pub fn set_selections(&mut self, id: u64, cursors: &[Cursor], primary: usize) {
        self.ensure_view(id);

        if cursors.is_empty() {
            return;
        }

        let slice = self.rope.slice(..);
        let position = |row: usize, col: usize| {
            let row = row.min(slice.len_lines() - 1);
            let start = slice.line_to_char(row);
            let line = slice.line(row);
            let length = line
                .to_string()
                .trim_end_matches(['\r', '\n'])
                .chars()
                .count();

            start + col.min(length)
        };
        let ranges = cursors
            .iter()
            .map(|cursor| {
                Range::point(position(cursor.anchor_row, cursor.anchor_column)).put_cursor(
                    slice,
                    position(cursor.row, cursor.column),
                    true,
                )
            })
            .collect();

        self.views.get_mut(&id).unwrap().selection =
            Selection::new(ranges, primary.min(cursors.len() - 1)).ensure_invariants(slice);
        self.refresh_cursors();
    }

    fn refresh_cursors(&mut self) {
        let slice = self.rope.slice(..);

        for view in self.views.values_mut() {
            let pos = if view.mode == TextMode::Insert {
                view.selection.primary().head
            } else {
                view.selection.primary().cursor(slice)
            }
            .min(slice.len_chars());
            let row = slice.char_to_line(pos);

            view.cursor = (row, pos - slice.line_to_char(row));
        }
    }

    fn begin_group(&mut self, id: u64) {
        if self.group.as_ref().is_some_and(|group| group.view_id != id) {
            self.commit_group();
        }

        if self.group.is_none() {
            self.group = Some(EditGroup {
                original: State {
                    doc: self.rope.clone(),
                    selection: self.views[&id].selection.clone(),
                },
                transaction: Transaction::new(&self.rope),
                view_id: id,
            });
        }
    }

    fn commit_group(&mut self) {
        if let Some(group) = self.group.take()
            && !group.transaction.changes().is_empty()
        {
            let selection = self
                .views
                .get(&group.view_id)
                .map(|view| view.selection.clone())
                .unwrap_or_else(|| group.original.selection.clone());

            self.history.commit_revision(
                &group.transaction.with_selection(selection),
                &group.original,
            );
        }
    }

    fn apply(&mut self, id: u64, transaction: Transaction, selection: Option<Selection>) {
        if self.readonly {
            return;
        }

        self.begin_group(id);
        self.apply_without_history(Some(id), &transaction, selection);

        let group = self.group.as_mut().unwrap();

        group.transaction = group.transaction.clone().compose(transaction);
    }

    fn apply_without_history(
        &mut self,
        id: Option<u64>,
        transaction: &Transaction,
        selection: Option<Selection>,
    ) {
        if self.readonly {
            return;
        }

        let old_rope = self.language.as_ref().map(|_| self.rope.clone());
        if !transaction.apply(&mut self.rope) {
            return;
        }

        if let (Some(language), Some(old_rope)) = (&mut self.language, old_rope) {
            language.update(&old_rope, &self.rope, transaction.changes());
        }

        for view in self.views.values_mut() {
            if !transaction.changes().is_empty() {
                view.cancel_pending_input();
            }
            view.selection = view.selection.clone().map(transaction.changes());
        }

        if let Some(id) = id
            && let Some(selection) = selection.or_else(|| transaction.selection().cloned())
        {
            self.views.get_mut(&id).unwrap().selection = selection;
        }

        let slice = self.rope.slice(..);

        for view in self.views.values_mut() {
            if view.mode != TextMode::Insert {
                view.selection = view.selection.clone().ensure_invariants(slice);
            }
        }

        self.version += 1;
        self.refresh_cursors();
    }

    fn replace_selections(&mut self, id: u64, values: &[String], insert: bool, before: bool) {
        if values.is_empty() {
            return;
        }

        let selection = self.views[&id].selection.clone();
        let transaction = Transaction::change(
            &self.rope,
            selection.iter().enumerate().map(|(index, range)| {
                let (from, to) = if insert {
                    let pos = if before { range.from() } else { range.to() };

                    (pos, pos)
                } else {
                    (range.from(), range.to())
                };

                (
                    from,
                    to,
                    Some(values[index.min(values.len() - 1)].as_str().into()),
                )
            }),
        );
        let selection = selection.transform(|range| {
            let pos = if insert && !before {
                range.to()
            } else {
                range.from()
            };

            Range::point(transaction.changes().map_pos(pos, Assoc::After))
        });

        self.apply(id, transaction, Some(selection));
    }

    fn read_register(&self, id: u64, register: char) -> Result<Option<Vec<String>>> {
        Ok(match register {
            '_' => Some(Vec::new()),
            '#' => Some(
                (1..=self.views[&id].selection.len())
                    .map(|i| i.to_string())
                    .collect(),
            ),
            '.' => Some(
                self.views[&id]
                    .selection
                    .fragments(self.rope.slice(..))
                    .map(|s| s.into_owned())
                    .collect(),
            ),
            '%' => Some(vec![
                self.path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "[scratch]".into()),
            ]),
            _ => self
                .registers
                .values
                .lock()
                .unwrap()
                .get(&register)
                .cloned(),
        })
    }

    fn yank(&mut self, id: u64, register: char) -> Result<Option<String>> {
        let view = &self.views[&id];
        let values = view
            .selection
            .fragments(self.rope.slice(..))
            .map(|part| part.into_owned())
            .collect::<Vec<_>>();

        self.write_register(register, values)
    }

    fn write_register(&self, register: char, values: Vec<String>) -> Result<Option<String>> {
        self.registers.write(register, values)
    }

    /// Executes a registered editing operation, returning an error for unknown command IDs.
    pub fn execute_command(&mut self, id: u64, command: &str) -> Result<TextEffect> {
        let command = TextCommand::parse(command)
            .with_context(|| format!("unknown text command: {command}"))?;
        self.execute(id, command)
    }

    pub fn execute(&mut self, id: u64, command: TextCommand) -> Result<TextEffect> {
        self.ensure_view(id);
        if !matches!(
            command,
            TextCommand::GotoWord
                | TextCommand::SelectRegex
                | TextCommand::SplitRegex
                | TextCommand::SearchForward
                | TextCommand::SearchBackward
        ) {
            self.views.get_mut(&id).unwrap().manual_viewport = false;
        }

        let old_version = self.version;
        let mut effect = TextEffect::default();
        let was_insert = self.views[&id].mode == TextMode::Insert;
        if was_insert
            && !self.replaying_insert
            && command != TextCommand::RepeatInsert
            && let Some(recording) = &mut self.views.get_mut(&id).unwrap().insert_recording
        {
            recording.events.push(InsertEvent::Command(command));
        }

        if command == TextCommand::Escape {
            self.set_text_mode(id, TextMode::Normal)?;

            return Ok(effect);
        }

        if let Some(digit) = match command {
            TextCommand::Count0 => Some(0),
            TextCommand::Count1 => Some(1),
            TextCommand::Count2 => Some(2),
            TextCommand::Count3 => Some(3),
            TextCommand::Count4 => Some(4),
            TextCommand::Count5 => Some(5),
            TextCommand::Count6 => Some(6),
            TextCommand::Count7 => Some(7),
            TextCommand::Count8 => Some(8),
            TextCommand::Count9 => Some(9),
            _ => None,
        } {
            self.push_command_count(id, digit);

            return Ok(effect);
        }

        if self.readonly
            && matches!(
                command,
                TextCommand::Insert
                    | TextCommand::Append
                    | TextCommand::InsertLineStart
                    | TextCommand::AppendLineEnd
                    | TextCommand::OpenBelow
                    | TextCommand::OpenAbove
                    | TextCommand::Change
                    | TextCommand::ChangeWithoutYank
                    | TextCommand::Delete
                    | TextCommand::DeleteWithoutYank
                    | TextCommand::PasteAfter
                    | TextCommand::PasteBefore
                    | TextCommand::ReplaceWithYank
                    | TextCommand::ReplaceCharacter
                    | TextCommand::Undo
                    | TextCommand::Redo
                    | TextCommand::DeleteBackward
                    | TextCommand::DeleteForward
                    | TextCommand::InsertNewline
                    | TextCommand::InsertTab
                    | TextCommand::Indent
                    | TextCommand::Unindent
                    | TextCommand::Join
                    | TextCommand::JoinSelectSpace
                    | TextCommand::SwitchCase
                    | TextCommand::Lowercase
                    | TextCommand::Uppercase
                    | TextCommand::RepeatInsert
                    | TextCommand::SurroundAdd
                    | TextCommand::SurroundReplace
                    | TextCommand::SurroundDelete
                    | TextCommand::ToggleComments
                    | TextCommand::ToggleBlockComments
                    | TextCommand::ToggleLineComments
            )
        {
            self.views.get_mut(&id).unwrap().register = None;
            self.views.get_mut(&id).unwrap().count = CommandCount::default();
            effect.message = Some("buffer is read-only".into());

            return Ok(effect);
        }

        let had_count = self.views[&id].count.value().is_some();
        let count = self.views[&id].count.value().unwrap_or(1);
        let pending = match command {
            TextCommand::FindForward => PendingInput::Find {
                direction: Direction::Forward,
                till: false,
            },
            TextCommand::FindBackward => PendingInput::Find {
                direction: Direction::Backward,
                till: false,
            },
            TextCommand::TillForward => PendingInput::Find {
                direction: Direction::Forward,
                till: true,
            },
            TextCommand::TillBackward => PendingInput::Find {
                direction: Direction::Backward,
                till: true,
            },
            TextCommand::ReplaceCharacter => PendingInput::ReplaceCharacter,
            TextCommand::SelectRegister => PendingInput::Register,
            TextCommand::TextObjectInside => PendingInput::TextObject(TextObject::Inside),
            TextCommand::TextObjectAround => PendingInput::TextObject(TextObject::Around),
            TextCommand::SurroundAdd => PendingInput::SurroundAdd,
            TextCommand::SurroundReplace => PendingInput::SurroundReplace,
            TextCommand::SurroundDelete => PendingInput::SurroundDelete,
            _ => PendingInput::None,
        };

        if !matches!(pending, PendingInput::None) {
            if !matches!(pending, PendingInput::Register) {
                self.views.get_mut(&id).unwrap().register = None;
            }
            self.views.get_mut(&id).unwrap().pending = pending;
            return Ok(effect);
        }

        self.views.get_mut(&id).unwrap().count = CommandCount::default();
        let selected_register = self.views.get_mut(&id).unwrap().register.take();
        let register = selected_register.unwrap_or('"');

        let read_register = if matches!(
            command,
            TextCommand::SearchNext | TextCommand::SearchPrevious
        ) {
            selected_register.unwrap_or(*self.registers.search_register.lock().unwrap())
        } else {
            register
        };
        if !self.reading_clipboard
            && matches!(read_register, '+' | '*')
            && matches!(
                command,
                TextCommand::PasteAfter
                    | TextCommand::PasteBefore
                    | TextCommand::ReplaceWithYank
                    | TextCommand::SearchNext
                    | TextCommand::SearchPrevious
            )
        {
            effect.clipboard_read = Some(ClipboardRead {
                register: read_register,
                command,
                count,
            });
            return Ok(effect);
        }

        if matches!(
            command,
            TextCommand::DeleteBackward
                | TextCommand::DeleteForward
                | TextCommand::InsertNewline
                | TextCommand::InsertTab
        ) {
            if self.views[&id].mode != TextMode::Insert {
                return Ok(effect);
            }

            match command {
                TextCommand::DeleteBackward | TextCommand::DeleteForward => {
                    let selection = self.views[&id].selection.clone();
                    let slice = self.rope.slice(..);
                    let transaction = Transaction::delete(
                        &self.rope,
                        selection.iter().map(|range| {
                            if command == TextCommand::DeleteBackward {
                                (prev_grapheme_boundary(slice, range.head), range.head)
                            } else {
                                (range.head, next_grapheme_boundary(slice, range.head))
                            }
                        }),
                    );

                    self.apply(id, transaction, None);
                }
                TextCommand::InsertNewline => {
                    let positions = self.views[&id]
                        .selection
                        .iter()
                        .map(|range| range.head)
                        .collect::<Vec<_>>();
                    let values = positions
                        .into_iter()
                        .map(|pos| format!("\n{}", self.newline_indentation(pos)))
                        .collect::<Vec<_>>();

                    self.replace_selections(id, &values, false, false);
                }
                _ => self.replace_selections(
                    id,
                    &[self.indentation().0.as_str().into()],
                    false,
                    false,
                ),
            }

            self.refresh_cursors();
            effect.changed = old_version != self.version;

            return Ok(effect);
        }

        if self.views[&id].mode == TextMode::Insert
            && matches!(
                command,
                TextCommand::MoveLeft
                    | TextCommand::MoveRight
                    | TextCommand::MoveUp
                    | TextCommand::MoveDown
                    | TextCommand::MoveWordStartForward
                    | TextCommand::MoveWordStartBackward
                    | TextCommand::MoveWordEndForward
                    | TextCommand::MoveLongWordStartForward
                    | TextCommand::MoveLongWordStartBackward
                    | TextCommand::MoveLongWordEndForward
            )
        {
            self.commit_group();
        }

        match command {
            TextCommand::ToggleComments
            | TextCommand::ToggleLineComments
            | TextCommand::ToggleBlockComments => {
                let selection = self.views[&id].selection.clone();
                let config = self.language.as_ref().map(LanguageAttachment::config);
                let line_tokens = config.and_then(|config| config.comment_tokens.as_deref());
                let block_tokens = config.and_then(|config| config.block_comment_tokens.as_deref());
                let slice = self.rope.slice(..);
                let row = slice.char_to_line(selection.primary().cursor(slice));
                let line_token = line_tokens.and_then(|tokens| {
                    comment::get_comment_token(slice, tokens, row)
                        .or_else(|| tokens.first().map(String::as_str))
                });
                let transaction = if let Some(tokens) = &block_tokens
                    && command == TextCommand::ToggleBlockComments
                {
                    comment::toggle_block_comments(&self.rope, &selection, tokens)
                } else if let Some(tokens) = &block_tokens
                    && command == TextCommand::ToggleComments
                {
                    let split = comment::split_lines_of_selection(self.rope.slice(..), &selection);
                    let (lines_commented, lines) =
                        comment::find_block_comments(tokens, self.rope.slice(..), &split);
                    let (selection_commented, changes) =
                        comment::find_block_comments(tokens, self.rope.slice(..), &selection);
                    if lines_commented {
                        comment::create_block_comment_transaction(&self.rope, &split, true, lines).0
                    } else if selection_commented {
                        comment::create_block_comment_transaction(
                            &self.rope, &selection, true, changes,
                        )
                        .0
                    } else {
                        comment::toggle_line_comments(&self.rope, &selection, line_token)
                    }
                } else {
                    comment::toggle_line_comments(&self.rope, &selection, line_token)
                };
                self.apply(id, transaction, None);
                self.views.get_mut(&id).unwrap().mode = TextMode::Normal;
            }
            TextCommand::SelectMode => {
                let view = self.views.get_mut(&id).unwrap();

                view.mode = if view.mode == TextMode::Select {
                    TextMode::Normal
                } else {
                    TextMode::Select
                };
            }
            TextCommand::GotoWord => self.start_word_jump(id),
            TextCommand::MoveLeft
            | TextCommand::MoveDown
            | TextCommand::MoveUp
            | TextCommand::MoveRight
            | TextCommand::MoveWordStartForward
            | TextCommand::MoveWordStartBackward
            | TextCommand::MoveWordEndForward
            | TextCommand::MoveLongWordStartForward
            | TextCommand::MoveLongWordStartBackward
            | TextCommand::MoveLongWordEndForward
            | TextCommand::GotoLineStart
            | TextCommand::GotoFirstNonwhitespace
            | TextCommand::GotoLineEnd
            | TextCommand::PageHalfUp
            | TextCommand::PageHalfDown => self.move_selection(id, command, count),
            TextCommand::GotoFileStart | TextCommand::GotoFileEnd => {
                let line = if had_count || command == TextCommand::GotoFileStart {
                    count.saturating_sub(1)
                } else {
                    self.rope.len_lines() - 1
                };
                let slice = self.rope.slice(..);
                let pos = slice.line_to_char(line.min(slice.len_lines() - 1));
                let extend = self.views[&id].mode == TextMode::Select;
                let selection = self.views[&id]
                    .selection
                    .clone()
                    .transform(|range| range.put_cursor(slice, pos, extend));

                self.views.get_mut(&id).unwrap().selection = selection.ensure_invariants(slice);
            }
            TextCommand::SelectLines | TextCommand::ExtendLines => {
                self.select_lines(id, count, command == TextCommand::ExtendLines)
            }
            TextCommand::SelectLinesAbove => {
                let slice = self.rope.slice(..);
                let view = self.views.get_mut(&id).unwrap();
                view.selection = view
                    .selection
                    .clone()
                    .transform(|range| {
                        let (first, last) = range.line_range(slice);
                        let end = slice.line_to_char((last + 1).min(slice.len_lines()));
                        let selected =
                            range.from() == slice.line_to_char(first) && range.to() == end;
                        Range::new(
                            end,
                            slice
                                .line_to_char(first.saturating_sub(count - usize::from(!selected))),
                        )
                    })
                    .ensure_invariants(slice);
            }
            TextCommand::Join | TextCommand::JoinSelectSpace => {
                self.join_lines(id, command == TextCommand::JoinSelectSpace);
            }
            TextCommand::Indent | TextCommand::Unindent => {
                let (style, tab_width) = self.indentation();
                let indent_width = style.indent_width(tab_width);
                let slice = self.rope.slice(..);
                let mut lines = self.views[&id]
                    .selection
                    .iter()
                    .flat_map(|range| {
                        let (first, last) = range.line_range(slice);
                        first..=last
                    })
                    .collect::<Vec<_>>();
                lines.sort_unstable();
                lines.dedup();
                let transaction = Transaction::change(
                    &self.rope,
                    lines.into_iter().filter_map(|row| {
                        let line = slice.line(row);
                        let start = slice.line_to_char(row);
                        if command == TextCommand::Indent {
                            if line.chars().all(char::is_whitespace) {
                                return None;
                            }
                            let offset = match style {
                                helix_core::indent::IndentStyle::Spaces(_) => {
                                    line.chars()
                                        .take_while(|ch| matches!(ch, ' ' | '\t'))
                                        .count()
                                        % indent_width
                                }
                                helix_core::indent::IndentStyle::Tabs => 0,
                            };
                            let indent = style.as_str().repeat(count);
                            Some((start, start, Some(indent[offset..].into())))
                        } else {
                            let mut width = 0;
                            let mut end = start;
                            for ch in line.chars() {
                                match ch {
                                    ' ' => width += 1,
                                    '\t' => width = (width / tab_width + 1) * tab_width,
                                    _ => break,
                                }
                                end += 1;
                                if width >= count * indent_width {
                                    break;
                                }
                            }
                            (end > start).then_some((start, end, None))
                        }
                    }),
                );
                self.apply(id, transaction, None);
            }
            TextCommand::SwitchCase | TextCommand::Lowercase | TextCommand::Uppercase => {
                let transaction = Transaction::change_by_selection(
                    &self.rope,
                    &self.views[&id].selection,
                    |range| {
                        let text = range.fragment(self.rope.slice(..));
                        let value = match command {
                            TextCommand::Lowercase => text.to_lowercase(),
                            TextCommand::Uppercase => text.to_uppercase(),
                            _ => text
                                .chars()
                                .map(|ch| {
                                    if ch.is_lowercase() {
                                        ch.to_uppercase().collect::<String>()
                                    } else {
                                        ch.to_lowercase().collect::<String>()
                                    }
                                })
                                .collect(),
                        };
                        (range.from(), range.to(), Some(value.into()))
                    },
                );
                self.apply(id, transaction, None);
            }
            TextCommand::FlipSelections => {
                let view = self.views.get_mut(&id).unwrap();
                view.selection = view.selection.clone().transform(|range| range.flip());
            }
            TextCommand::SplitLines => {
                let view = self.views.get_mut(&id).unwrap();
                view.selection =
                    helix_core::selection::split_on_newline(self.rope.slice(..), &view.selection);
            }
            TextCommand::MergeSelections => {
                let view = self.views.get_mut(&id).unwrap();
                view.selection = view.selection.clone().merge_ranges();
            }
            TextCommand::MatchBrackets => {
                let slice = self.rope.slice(..);
                let view = self.views.get_mut(&id).unwrap();
                let extend = view.mode == TextMode::Select;
                view.selection = view
                    .selection
                    .clone()
                    .transform(|range| {
                        helix_core::match_brackets::find_matching_bracket_plaintext(
                            slice,
                            range.cursor(slice),
                        )
                        .map(|pos| range.put_cursor(slice, pos, extend))
                        .unwrap_or(range)
                    })
                    .ensure_invariants(slice);
            }
            TextCommand::Insert
            | TextCommand::Append
            | TextCommand::InsertLineStart
            | TextCommand::AppendLineEnd => {
                self.begin_group(id);

                let slice = self.rope.slice(..);
                let selection = self.views[&id].selection.clone().transform(|range| {
                    let cursor = range.cursor(slice);
                    let row = slice.char_to_line(cursor);
                    let pos = match command {
                        TextCommand::Insert => range.from(),
                        TextCommand::Append => range.to(),
                        TextCommand::InsertLineStart => {
                            slice.line_to_char(row)
                                + slice
                                    .line(row)
                                    .chars()
                                    .take_while(|c| c.is_whitespace() && *c != '\n' && *c != '\r')
                                    .count()
                        }
                        _ => self.line_end(row),
                    };

                    Range::point(pos)
                });
                let view = self.views.get_mut(&id).unwrap();

                view.selection = selection;
                view.mode = TextMode::Insert;
            }
            TextCommand::OpenBelow | TextCommand::OpenAbove => {
                self.open_lines(id, command == TextCommand::OpenAbove, count)
            }
            TextCommand::Delete
            | TextCommand::Change
            | TextCommand::DeleteWithoutYank
            | TextCommand::ChangeWithoutYank => {
                let changing = matches!(
                    command,
                    TextCommand::Change | TextCommand::ChangeWithoutYank
                );
                let slice = self.rope.slice(..);
                let whole_lines = changing
                    && self.views[&id].selection.iter().all(|range| {
                        let (first, last) = range.line_range(slice);
                        range.slice(slice).len_lines() >= 2
                            && range.from() == slice.line_to_char(first)
                            && range.to() == slice.line_to_char((last + 1).min(slice.len_lines()))
                    });
                if matches!(command, TextCommand::Delete | TextCommand::Change) {
                    effect.clipboard = self.yank(id, register)?;
                }

                if changing {
                    self.views.get_mut(&id).unwrap().mode = TextMode::Insert;
                }

                self.replace_selections(id, &[String::new()], false, false);
                if whole_lines {
                    self.open_lines(id, true, count);
                }
            }
            TextCommand::Yank => {
                effect.clipboard = self.yank(id, register)?;
                effect.message = Some(format!(
                    "yanked {} selection(s)",
                    self.views[&id].selection.len()
                ));
            }
            TextCommand::YankToClipboard => {
                effect.clipboard = Some(
                    self.views[&id]
                        .selection
                        .fragments(self.rope.slice(..))
                        .map(|part| part.into_owned())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                effect.message = Some("Clipboard write requested".into());
            }
            TextCommand::PasteAfter | TextCommand::PasteBefore | TextCommand::ReplaceWithYank => {
                let values = self.read_register(id, register)?;

                if let Some(values) = values.filter(|values| !values.is_empty()) {
                    let values = values
                        .into_iter()
                        .map(|text| text.repeat(count))
                        .collect::<Vec<_>>();

                    if command == TextCommand::ReplaceWithYank {
                        let transaction = Transaction::change(
                            &self.rope,
                            self.views[&id]
                                .selection
                                .iter()
                                .enumerate()
                                .map(|(index, range)| {
                                    (
                                        range.from(),
                                        range.to(),
                                        Some(values[index.min(values.len() - 1)].as_str().into()),
                                    )
                                }),
                        );
                        self.apply(id, transaction, None);
                    } else {
                        self.paste(id, &values, command == TextCommand::PasteBefore);
                    }
                }
            }
            TextCommand::Undo | TextCommand::Redo => {
                self.commit_group();

                for _ in 0..count {
                    let transaction = if command == TextCommand::Undo {
                        self.history.undo()
                    } else {
                        self.history.redo()
                    }
                    .cloned();

                    if let Some(transaction) = transaction {
                        self.apply_without_history(Some(id), &transaction, None);
                    } else {
                        break;
                    }
                }
            }
            TextCommand::RepeatInsert => {
                effect = self.repeat_insert(id, count)?;
            }
            TextCommand::SelectRegex
            | TextCommand::SplitRegex
            | TextCommand::SearchForward
            | TextCommand::SearchBackward => {
                let kind = match command {
                    TextCommand::SelectRegex => TextPromptKind::Select,
                    TextCommand::SplitRegex => TextPromptKind::Split,
                    TextCommand::SearchForward => TextPromptKind::SearchForward,
                    _ => TextPromptKind::SearchBackward,
                };
                let view = self.views.get_mut(&id).unwrap();
                view.pending = PendingInput::Regex {
                    kind,
                    selection: view.selection.clone(),
                    scroll: view.scroll,
                    horizontal_scroll: view.horizontal_scroll,
                    manual_viewport: view.manual_viewport,
                    input: String::new(),
                };
                view.prompt_register = selected_register.unwrap_or('/');
                effect.prompt = Some(kind);
            }
            TextCommand::SearchNext | TextCommand::SearchPrevious => {
                let register =
                    selected_register.unwrap_or(*self.registers.search_register.lock().unwrap());
                let pattern = self
                    .read_register(id, register)?
                    .and_then(|values| values.first().cloned())
                    .unwrap_or_default();

                if !pattern.is_empty() {
                    let regex = self.build_regex(&pattern)?;
                    for _ in 0..count {
                        self.regex_selection(
                            id,
                            if command == TextCommand::SearchNext {
                                TextPromptKind::SearchForward
                            } else {
                                TextPromptKind::SearchBackward
                            },
                            &regex,
                            Movement::Move,
                        )?;
                    }
                }
            }
            TextCommand::CollapseSelection => {
                let slice = self.rope.slice(..);
                let view = self.views.get_mut(&id).unwrap();

                view.selection = view
                    .selection
                    .clone()
                    .cursors(slice)
                    .ensure_invariants(slice);
            }
            TextCommand::KeepPrimary => {
                let view = self.views.get_mut(&id).unwrap();

                view.selection = view.selection.clone().into_single();
            }
            TextCommand::SelectAll => {
                self.views.get_mut(&id).unwrap().selection =
                    Selection::single(0, self.rope.len_chars())
            }
            TextCommand::CopySelectionDown => {
                let slice = self.rope.slice(..);
                let selection = self.views[&id].selection.clone();
                let mut ranges = SmallVec::with_capacity(selection.len() * (count + 1));
                let format = TextFormat::default();
                let annotations = TextAnnotations::default();
                let mut primary = selection.primary_index();

                let column_at = |pos| {
                    visual_offset_from_block(
                        slice,
                        slice.line_to_char(slice.char_to_line(pos)),
                        pos,
                        &format,
                        &annotations,
                    )
                    .0
                    .col
                };
                let position_at = |row, column| {
                    char_idx_at_visual_offset(
                        slice,
                        slice.line_to_char(row),
                        0,
                        column,
                        &format,
                        &annotations,
                    )
                    .0
                };

                ranges.extend_from_slice(selection.ranges());

                for (index, range) in selection.iter().enumerate() {
                    let head = range.cursor(slice);
                    let anchor = if range.anchor > range.head {
                        prev_grapheme_boundary(slice, range.anchor)
                    } else {
                        range.anchor
                    };
                    let head_row = slice.char_to_line(head);
                    let anchor_row = slice.char_to_line(anchor);
                    let height = head_row.abs_diff(anchor_row) + 1;
                    let head_column = column_at(head);
                    let anchor_column = column_at(anchor);
                    let mut copied = 0;

                    for offset in 1.. {
                        let target_head_row = head_row + offset * height;
                        let target_anchor_row = anchor_row + offset * height;

                        if target_head_row >= slice.len_lines()
                            || target_anchor_row >= slice.len_lines()
                        {
                            break;
                        }

                        let head = position_at(target_head_row, head_column);
                        let anchor = position_at(target_anchor_row, anchor_column);

                        if column_at(head) != head_column || column_at(anchor) != anchor_column {
                            continue;
                        }

                        if index == selection.primary_index() {
                            primary = ranges.len();
                        }

                        ranges.push(Range::point(anchor).put_cursor(slice, head, true));
                        copied += 1;

                        if copied == count {
                            break;
                        }
                    }
                }

                self.views.get_mut(&id).unwrap().selection =
                    Selection::new(ranges, primary).ensure_invariants(slice);
            }
            TextCommand::Escape
            | TextCommand::Count0
            | TextCommand::Count1
            | TextCommand::Count2
            | TextCommand::Count3
            | TextCommand::Count4
            | TextCommand::Count5
            | TextCommand::Count6
            | TextCommand::Count7
            | TextCommand::Count8
            | TextCommand::Count9
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
            | TextCommand::DeleteBackward
            | TextCommand::DeleteForward
            | TextCommand::InsertNewline
            | TextCommand::InsertTab => unreachable!("text command is handled above"),
        }

        if register == '*' {
            effect.primary_clipboard = effect.clipboard.take();
        }
        if self.views[&id].mode == TextMode::Select
            && matches!(
                command,
                TextCommand::Delete
                    | TextCommand::DeleteWithoutYank
                    | TextCommand::Yank
                    | TextCommand::YankToClipboard
                    | TextCommand::PasteBefore
                    | TextCommand::PasteAfter
                    | TextCommand::ReplaceWithYank
                    | TextCommand::Indent
                    | TextCommand::Unindent
                    | TextCommand::SwitchCase
                    | TextCommand::Lowercase
                    | TextCommand::Uppercase
            )
        {
            self.views.get_mut(&id).unwrap().mode = TextMode::Normal;
        }

        if self.views[&id].mode != TextMode::Insert {
            self.commit_group();
        }

        if !was_insert && self.views[&id].mode == TextMode::Insert && !self.replaying_insert {
            self.views.get_mut(&id).unwrap().insert_recording = Some(InsertReplay {
                command,
                count,
                register: selected_register,
                events: Vec::new(),
            });
        }

        self.refresh_cursors();
        effect.changed = old_version != self.version;

        Ok(effect)
    }

    /// Consumes an operation argument or a literal character in insert mode.
    pub fn input_text(&mut self, id: u64, key: &str) -> Result<TextEffect> {
        self.input_event(id, TextInput::parse(key))
    }

    fn input_event(&mut self, id: u64, key: TextInput) -> Result<TextEffect> {
        self.ensure_view(id);

        let old_version = self.version;
        let pending = self.views[&id].pending.clone();
        let mut effect = TextEffect::default();
        let character = key.character();

        if self.views[&id].mode == TextMode::Insert
            && !self.replaying_insert
            && let Some(recording) = &mut self.views.get_mut(&id).unwrap().insert_recording
        {
            recording.events.push(InsertEvent::Input(key));
        }

        if key == TextInput::Escape && !matches!(pending, PendingInput::None) {
            self.cancel_pending_command(id);
            return Ok(effect);
        }

        if let PendingInput::Regex {
            kind,
            mut input,
            selection,
            scroll,
            horizontal_scroll,
            manual_viewport,
        } = pending
        {
            if key == TextInput::Enter {
                effect = self.submit_prompt(id, kind, &input)?;
            } else {
                if key == TextInput::Backspace {
                    input.pop();
                } else if let Some(ch) = character {
                    input.push(ch);
                }
                self.views.get_mut(&id).unwrap().pending = PendingInput::Regex {
                    kind,
                    selection,
                    scroll,
                    horizontal_scroll,
                    manual_viewport,
                    input: input.clone(),
                };
                self.preview_prompt(id, kind, &input)?;
            }
        } else if let PendingInput::GotoWord { words, first } = pending {
            let Some(ch) = character.filter(char::is_ascii_lowercase) else {
                self.cancel_pending_command(id);
                return Ok(effect);
            };
            let index = (ch as u8 - b'a') as usize;
            if let Some(first) = first {
                let view = self.views.get_mut(&id).unwrap();
                if let Some(mut range) = words.get(usize::from(first) * 26 + index).copied() {
                    range = if view.mode == TextMode::Select {
                        let primary = view.selection.primary();
                        let anchor = if range.anchor < range.head {
                            range.anchor.min(primary.from())
                        } else {
                            range.anchor.max(primary.to())
                        };
                        Range::new(anchor, range.head)
                    } else {
                        range.with_direction(Direction::Forward)
                    };
                    view.selection = Selection::single(range.anchor, range.head)
                        .ensure_invariants(self.rope.slice(..));
                    view.manual_viewport = false;
                }
            } else if index * 26 < words.len() {
                self.views.get_mut(&id).unwrap().pending = PendingInput::GotoWord {
                    words,
                    first: Some(index as u8),
                };
                return Ok(effect);
            }
            self.cancel_pending_command(id);
        } else if !matches!(pending, PendingInput::None) {
            let count = self.views[&id].count.value().unwrap_or(1);

            self.views.get_mut(&id).unwrap().pending = PendingInput::None;
            if !matches!(pending, PendingInput::Register) {
                self.views.get_mut(&id).unwrap().count = CommandCount::default();
            }

            match pending {
                PendingInput::SurroundAdd if key == TextInput::Enter || character.is_some() => {
                    let (open, close) = if key == TextInput::Enter {
                        ("\n".to_owned(), "\n".to_owned())
                    } else {
                        let (open, close) =
                            helix_core::match_brackets::get_pair(character.unwrap());
                        (open.to_string(), close.to_string())
                    };
                    let selection = self.views[&id].selection.clone();
                    let mut changes = Vec::new();
                    let mut ranges = SmallVec::new();
                    let mut offset = 0;
                    for range in &selection {
                        changes.push((range.from(), range.from(), Some(open.as_str().into())));
                        changes.push((range.to(), range.to(), Some(close.as_str().into())));
                        ranges.push(
                            Range::new(range.from() + offset, range.to() + offset + 2)
                                .with_direction(range.direction()),
                        );
                        offset += 2;
                    }
                    let transaction = Transaction::change(&self.rope, changes.into_iter());
                    self.apply(
                        id,
                        transaction,
                        Some(Selection::new(ranges, selection.primary_index())),
                    );
                    self.views.get_mut(&id).unwrap().mode = TextMode::Normal;
                }
                PendingInput::SurroundReplace | PendingInput::SurroundDelete
                    if character.is_some() =>
                {
                    let ch = character.unwrap();
                    let positions = surround::get_surround_pos(
                        None,
                        self.rope.slice(..),
                        &self.views[&id].selection,
                        (ch != 'm').then_some(ch),
                        count,
                    )
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                    if matches!(pending, PendingInput::SurroundReplace) {
                        let view = self.views.get_mut(&id).unwrap();
                        view.pending = PendingInput::SurroundReplaceWith(positions);
                    } else {
                        let mut positions = positions;
                        positions.sort_unstable();
                        let transaction = Transaction::delete(
                            &self.rope,
                            positions.into_iter().map(|pos| (pos, pos + 1)),
                        );
                        self.apply(id, transaction, None);
                        self.views.get_mut(&id).unwrap().mode = TextMode::Normal;
                    }
                }
                PendingInput::SurroundReplaceWith(positions) if character.is_some() => {
                    let (open, close) = helix_core::match_brackets::get_pair(character.unwrap());
                    let mut changes = positions
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .flat_map(|pair| [(pair[0], open), (pair[1], close)])
                        .collect::<Vec<_>>();
                    changes.sort_unstable_by_key(|(pos, _)| *pos);
                    let transaction = Transaction::change(
                        &self.rope,
                        changes
                            .into_iter()
                            .map(|(pos, ch)| (pos, pos + 1, Some(ch.to_string().into()))),
                    );
                    self.apply(id, transaction, None);
                    self.views.get_mut(&id).unwrap().mode = TextMode::Normal;
                }
                PendingInput::TextObject(object) => {
                    if let Some(ch) = character {
                        let slice = self.rope.slice(..);
                        let selection =
                            self.views[&id]
                                .selection
                                .clone()
                                .transform(|range| match ch {
                                    'w' | 'W' => textobject::textobject_word(
                                        slice,
                                        range,
                                        object,
                                        count,
                                        ch == 'W',
                                    ),
                                    'p' => textobject::textobject_paragraph(
                                        slice, range, object, count,
                                    ),
                                    _ => textobject::textobject_pair_surround(
                                        None, slice, range, object, ch, count,
                                    ),
                                });

                        self.views.get_mut(&id).unwrap().selection =
                            selection.ensure_invariants(slice);
                    }
                }
                PendingInput::Find { direction, till } if character.is_some() => {
                    self.find_character(id, direction, till, character.unwrap(), count)
                }
                PendingInput::ReplaceCharacter if !self.readonly => {
                    let replacement = if key == TextInput::Enter {
                        Some('\n')
                    } else {
                        character
                    };

                    if let Some(replacement) = replacement {
                        let values = self.views[&id]
                            .selection
                            .iter()
                            .map(|range| {
                                replacement.to_string().repeat(
                                    range
                                        .fragment(self.rope.slice(..))
                                        .graphemes(true)
                                        .count()
                                        .max(1),
                                )
                            })
                            .collect::<Vec<_>>();

                        self.replace_selections(id, &values, false, false);
                    }
                }
                PendingInput::Register => {
                    if let Some(ch) = character {
                        self.views.get_mut(&id).unwrap().register = Some(ch);
                    }
                }
                _ => {}
            }
        } else if self.views[&id].mode == TextMode::Insert
            && !self.readonly
            && let Some(ch) = character
        {
            if ch == '}' && self.is_rust() {
                self.insert_closing_brace(id);
            } else {
                self.replace_selections(id, &[ch.to_string()], false, false);
            }
        }

        if self.views[&id].mode != TextMode::Insert {
            self.commit_group();
        }

        self.refresh_cursors();
        effect.changed = old_version != self.version;

        Ok(effect)
    }

    fn line_end(&self, row: usize) -> usize {
        let line = self.rope.line(row);

        self.rope.line_to_char(row)
            + line
                .to_string()
                .trim_end_matches(['\r', '\n'])
                .chars()
                .count()
    }

    fn repeat_insert(&mut self, id: u64, count: usize) -> Result<TextEffect> {
        anyhow::ensure!(!self.replaying_insert, "Cannot repeat insert recursively");
        anyhow::ensure!(
            self.views[&id].mode != TextMode::Insert,
            "Finish the current insertion before repeating"
        );
        let Some(recording) = self.insert_history.0.lock().unwrap().clone() else {
            return Ok(TextEffect::default());
        };
        self.replaying_insert = true;
        let result = (|| {
            let mut effect = TextEffect::default();
            for _ in 0..count {
                let view = self.views.get_mut(&id).unwrap();
                view.count = recording.count.into();
                view.register = recording.register;
                let entry = self.execute(id, recording.command)?;
                effect.clipboard = entry.clipboard.or(effect.clipboard);
                effect.primary_clipboard = entry.primary_clipboard.or(effect.primary_clipboard);
                for event in &recording.events {
                    let step = match event {
                        InsertEvent::Command(command) => self.execute(id, *command)?,
                        InsertEvent::Input(input) => self.input_event(id, *input)?,
                    };
                    effect.clipboard = step.clipboard.or(effect.clipboard);
                    effect.primary_clipboard = step.primary_clipboard.or(effect.primary_clipboard);
                }
            }
            Ok(effect)
        })();
        self.replaying_insert = false;
        result
    }

    fn join_lines(&mut self, id: u64, select_space: bool) {
        let slice = self.rope.slice(..);
        let mut changes = Vec::new();
        for range in &self.views[&id].selection {
            let (first, mut last) = range.line_range(slice);
            if first == last {
                last = (last + 1).min(slice.len_lines() - 1);
            }
            let comment_token = |row| {
                let line = slice.line(row).to_string();
                let line = line.trim_start_matches([' ', '\t']);
                ["///", "//!", "//"]
                    .into_iter()
                    .find(|token| line.starts_with(token))
            };
            let mut token = self.is_rust().then(|| comment_token(first)).flatten();
            for row in first..last {
                let from = self.line_end(row);
                let mut to = slice.line_to_char(row + 1);
                while to < slice.len_chars() && matches!(slice.char(to), ' ' | '\t') {
                    to += 1;
                }
                if self.is_rust()
                    && let Some(next_token) = comment_token(row + 1)
                {
                    if token == Some(next_token) {
                        to += next_token.chars().count();
                        while to < slice.len_chars() && matches!(slice.char(to), ' ' | '\t') {
                            to += 1;
                        }
                    } else {
                        token = Some(next_token);
                    }
                }
                let separator = (to != self.line_end(row + 1)).then(|| " ".into());
                changes.push((from, to, separator));
            }
        }
        changes.sort_unstable_by_key(|(from, _, _)| *from);
        changes.dedup();
        if changes.is_empty() {
            return;
        }
        let transaction = Transaction::change(&self.rope, changes.iter().cloned());
        let selection = if select_space {
            let ranges = changes
                .iter()
                .filter(|(_, _, value)| value.is_some())
                .map(|(from, _, _)| {
                    let pos = transaction.changes().map_pos(*from, Assoc::Before);
                    Range::new(pos, pos + 1)
                })
                .collect::<SmallVec<_>>();
            (!ranges.is_empty()).then(|| Selection::new(ranges, 0))
        } else {
            None
        };
        self.apply(id, transaction, selection);
    }

    fn paste(&mut self, id: u64, values: &[String], before: bool) {
        if values.is_empty() {
            return;
        }
        let slice = self.rope.slice(..);
        let selection = self.views[&id].selection.clone();
        let linewise = values
            .iter()
            .any(|value| helix_core::line_ending::get_line_ending_of_str(value).is_some());
        let mut offset = 0;
        let mut ranges = SmallVec::new();
        let transaction = Transaction::change(
            &self.rope,
            selection.iter().enumerate().map(|(index, range)| {
                let pos = match (before, linewise) {
                    (true, true) => slice.line_to_char(slice.char_to_line(range.from())),
                    (false, true) => {
                        slice.line_to_char((range.line_range(slice).1 + 1).min(slice.len_lines()))
                    }
                    (true, false) => range.from(),
                    (false, false) => range.to(),
                };
                let value = &values[index.min(values.len() - 1)];
                let len = value.chars().count();
                ranges.push(
                    Range::new(pos + offset, pos + offset + len).with_direction(range.direction()),
                );
                offset += len;
                (pos, pos, Some(value.as_str().into()))
            }),
        );
        let selection = (self.views[&id].mode == TextMode::Normal)
            .then(|| Selection::new(ranges, selection.primary_index()));
        self.apply(id, transaction, selection);
    }

    fn start_word_jump(&mut self, id: u64) {
        let slice = self.rope.slice(..);
        let view = &self.views[&id];
        let start = slice.line_to_char(view.scroll.min(slice.len_lines() - 1));
        let end = slice.line_to_char((view.scroll + view.viewport_height).min(slice.len_lines()));
        let cursor = view.selection.primary().cursor(slice);
        let mut forward = Range::point(cursor);
        let mut backward = Range::point(cursor);
        let mut words = Vec::new();

        // Keep Helix's alternating, nearest-word-first label order.
        if slice.get_char(cursor).is_some_and(|ch| !ch.is_whitespace()) {
            let word_end = movement::move_next_word_end(slice, forward, 1);
            if word_end.anchor == cursor {
                forward = word_end;
            }
            let word_start = movement::move_prev_word_start(slice, backward, 1);
            if word_start.anchor == next_grapheme_boundary(slice, cursor) {
                backward = word_start;
            }
        }
        'outer: loop {
            let mut changed = false;
            while forward.head < end {
                forward = movement::move_next_word_end(slice, forward, 1);
                if slice
                    .slice(..forward.head)
                    .graphemes_rev()
                    .take(2)
                    .take_while(|grapheme| grapheme.chars().all(char_is_word))
                    .count()
                    != 2
                {
                    continue;
                }
                changed = true;
                forward.anchor += slice
                    .chars_at(forward.anchor)
                    .take_while(|&ch| !char_is_word(ch))
                    .count();
                if forward.from() < start || forward.to() > end {
                    continue;
                }
                words.push(forward);
                if words.len() == 26 * 26 {
                    break 'outer;
                }
                break;
            }
            while backward.head > start {
                backward = movement::move_prev_word_start(slice, backward, 1);
                if slice
                    .slice(backward.head..)
                    .graphemes()
                    .take(2)
                    .take_while(|grapheme| grapheme.chars().all(char_is_word))
                    .count()
                    != 2
                {
                    continue;
                }
                changed = true;
                backward.anchor -= slice
                    .chars_at(backward.anchor)
                    .reversed()
                    .take_while(|&ch| !char_is_word(ch))
                    .count();
                if backward.from() < start || backward.to() > end {
                    continue;
                }
                words.push(backward);
                if words.len() == 26 * 26 {
                    break 'outer;
                }
                break;
            }
            if !changed {
                break;
            }
        }

        let view = self.views.get_mut(&id).unwrap();
        view.pending = if words.is_empty() {
            PendingInput::None
        } else {
            PendingInput::GotoWord { words, first: None }
        };
    }

    fn move_selection(&mut self, id: u64, command: TextCommand, count: usize) {
        let slice = self.rope.slice(..);
        let extend = self.views[&id].mode == TextMode::Select;
        let insert = self.views[&id].mode == TextMode::Insert;
        let behaviour = if extend {
            Movement::Extend
        } else {
            Movement::Move
        };
        let page_scroll = match command {
            TextCommand::PageHalfDown => Some(zess_ui::HalfPageScroll::new(
                self.views[&id].viewport_height,
                count,
                zess_ui::ScrollDirection::Down,
            )),
            TextCommand::PageHalfUp => Some(zess_ui::HalfPageScroll::new(
                self.views[&id].viewport_height,
                count,
                zess_ui::ScrollDirection::Up,
            )),
            _ => None,
        };
        let vertical_count = page_scroll
            .as_ref()
            .map_or(count, |scroll| scroll.distance());
        let selection = self.views[&id].selection.clone().transform(|range| {
            let cursor = range.cursor(slice);
            let row = slice.char_to_line(cursor);
            let mut annotations = TextAnnotations::default();
            let format = TextFormat::default();
            let moved = match command {
                TextCommand::MoveLeft => movement::move_horizontally(
                    slice,
                    range,
                    Direction::Backward,
                    count,
                    behaviour,
                    &format,
                    &mut annotations,
                ),
                TextCommand::MoveRight => movement::move_horizontally(
                    slice,
                    range,
                    Direction::Forward,
                    count,
                    behaviour,
                    &format,
                    &mut annotations,
                ),
                TextCommand::MoveDown | TextCommand::PageHalfDown => movement::move_vertically(
                    slice,
                    range,
                    Direction::Forward,
                    vertical_count,
                    behaviour,
                    &format,
                    &mut annotations,
                ),
                TextCommand::MoveUp | TextCommand::PageHalfUp => movement::move_vertically(
                    slice,
                    range,
                    Direction::Backward,
                    vertical_count,
                    behaviour,
                    &format,
                    &mut annotations,
                ),
                TextCommand::MoveWordStartForward => {
                    movement::move_next_word_start(slice, range, count)
                }
                TextCommand::MoveWordStartBackward => {
                    movement::move_prev_word_start(slice, range, count)
                }
                TextCommand::MoveWordEndForward => {
                    movement::move_next_word_end(slice, range, count)
                }
                TextCommand::MoveLongWordStartForward => {
                    movement::move_next_long_word_start(slice, range, count)
                }
                TextCommand::MoveLongWordStartBackward => {
                    movement::move_prev_long_word_start(slice, range, count)
                }
                TextCommand::MoveLongWordEndForward => {
                    movement::move_next_long_word_end(slice, range, count)
                }
                TextCommand::GotoLineStart => {
                    range.put_cursor(slice, slice.line_to_char(row), extend)
                }
                TextCommand::GotoFirstNonwhitespace => {
                    let start = slice.line_to_char(row);
                    let offset = slice
                        .line(row)
                        .chars()
                        .take_while(|c| c.is_whitespace() && *c != '\n' && *c != '\r')
                        .count();

                    range.put_cursor(slice, start + offset, extend)
                }
                TextCommand::GotoLineEnd => {
                    let end = self.line_end(row);
                    let end = if end > slice.line_to_char(row) {
                        prev_grapheme_boundary(slice, end)
                    } else {
                        end
                    };

                    range.put_cursor(slice, end, extend)
                }
                _ => range,
            };
            let moved = if extend
                && matches!(
                    command,
                    TextCommand::MoveWordStartForward
                        | TextCommand::MoveWordStartBackward
                        | TextCommand::MoveWordEndForward
                        | TextCommand::MoveLongWordStartForward
                        | TextCommand::MoveLongWordStartBackward
                        | TextCommand::MoveLongWordEndForward
                ) {
                Range::new(range.anchor, moved.head)
            } else {
                moved
            };

            if insert {
                Range::point(moved.cursor(slice))
            } else {
                moved
            }
        });

        let view = self.views.get_mut(&id).unwrap();
        view.selection = if insert {
            selection
        } else {
            selection.ensure_invariants(slice)
        };
        if let Some(page_scroll) = page_scroll {
            let row = slice.char_to_line(view.selection.primary().cursor(slice));
            view.scroll = page_scroll.viewport_start(view.scroll, row, 5);
        }
    }

    fn select_lines(&mut self, id: u64, count: usize, bounds_only: bool) {
        let slice = self.rope.slice(..);
        let selection = self.views[&id].selection.clone().transform(|range| {
            let (first, last) = range.line_range(slice);
            let from = slice.line_to_char(first);
            let end = if last + 1 == slice.len_lines() {
                slice.len_chars()
            } else {
                slice.line_to_char(last + 1)
            };
            let additional = if bounds_only {
                1
            } else {
                count + usize::from(range.from() == from && range.to() == end)
            };
            let to_line = (last + additional).min(slice.len_lines());
            let to = if to_line == slice.len_lines() {
                slice.len_chars()
            } else {
                slice.line_to_char(to_line)
            };

            if bounds_only {
                Range::new(from, to).with_direction(range.direction())
            } else {
                Range::new(from, to)
            }
        });

        self.views.get_mut(&id).unwrap().selection = selection.ensure_invariants(slice);
    }

    fn open_lines(&mut self, id: u64, above: bool, count: usize) {
        self.begin_group(id);

        let selection = self.views[&id].selection.clone();
        let mut changes = Vec::new();
        let mut positions = Vec::new();

        for range in &selection {
            let row = self.rope.char_to_line(range.cursor(self.rope.slice(..)));
            let pos = if above {
                self.rope.line_to_char(row)
            } else {
                self.line_end(row)
            };
            let indentation = if above {
                self.line_indentation(self.line_end(row))
            } else {
                self.newline_indentation(pos)
            };
            let text = if above {
                format!("{indentation}\n").repeat(count)
            } else {
                format!("\n{indentation}").repeat(count)
            };

            positions.push((pos, indentation.chars().count()));
            changes.push((pos, pos, Some(text.into())));
        }

        let transaction = Transaction::change(&self.rope, changes.into_iter());
        let selection = Selection::new(
            positions
                .into_iter()
                .map(|(pos, indentation)| {
                    Range::point(if above {
                        transaction.changes().map_pos(pos, Assoc::Before) + indentation
                    } else {
                        transaction.changes().map_pos(pos, Assoc::After)
                    })
                })
                .collect(),
            selection.primary_index(),
        );

        self.views.get_mut(&id).unwrap().mode = TextMode::Insert;
        self.apply(id, transaction, Some(selection));
    }

    pub fn language_id(&self) -> Option<&str> {
        self.language.as_ref().map(LanguageAttachment::id)
    }

    pub fn has_outline(&self) -> bool {
        self.language
            .as_ref()
            .is_some_and(LanguageAttachment::has_outline)
    }

    /// Selects a configured language, or disables language features for plain text.
    pub fn set_language(&mut self, id: Option<&str>) -> Result<()> {
        let language = id
            .map(|id| {
                language::loader()
                    .language_for_name(id)
                    .map(LanguageAttachment::new)
                    .with_context(|| format!("unknown language {id:?}"))
            })
            .transpose()?;
        self.language = language;
        self.language_detection = LanguageDetection::Explicit;
        self.invalidate_viewport_widths();
        Ok(())
    }

    fn invalidate_viewport_widths(&mut self) {
        for view in self.views.values_mut() {
            view.viewport_version = self.version.wrapping_sub(1);
        }
    }

    pub fn syntax_error(&self) -> Option<&str> {
        self.language.as_ref().and_then(LanguageAttachment::error)
    }

    fn is_rust(&self) -> bool {
        self.language_id() == Some("rust")
    }

    fn indentation(&self) -> (helix_core::indent::IndentStyle, usize) {
        self.language
            .as_ref()
            .and_then(LanguageAttachment::indentation)
            .unwrap_or((helix_core::indent::IndentStyle::Spaces(4), 4))
    }

    fn line_indentation(&self, pos: usize) -> String {
        let start = self.rope.line_to_char(self.rope.char_to_line(pos));

        self.rope
            .slice(start..pos)
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .collect()
    }

    fn newline_indentation(&mut self, pos: usize) -> String {
        let mut indentation = self.line_indentation(pos);
        let indent_style = self.indentation().0;

        if self.is_rust() {
            let text = self.text();

            let syntax = self
                .language
                .as_mut()
                .and_then(|language| language.syntax(&self.rope));

            let byte = self.rope.char_to_byte(pos);
            let line = self.rope.char_to_line(pos);
            let start = self.rope.line_to_byte(line);
            let prefix = text[start..byte].trim_end();

            if prefix.ends_with('{') {
                let byte = start + prefix.len() - 1;
                let opener = syntax.and_then(|syntax| {
                    syntax.descendant_for_byte_range(byte as u32, (byte + 1) as u32)
                });

                if opener.is_some_and(|node| node.kind() == "{") {
                    indentation.push_str(indent_style.as_str());
                }
            }
        }

        indentation
    }

    fn insert_closing_brace(&mut self, id: u64) {
        self.begin_group(id);

        let text = self.text();

        let syntax = self
            .language
            .as_mut()
            .and_then(|language| language.syntax(&self.rope));

        let slice = self.rope.slice(..);
        let selection = self.views[&id].selection.clone().transform(|range| {
            let byte = slice.char_to_byte(range.head);
            let byte = text[..byte].trim_end().len().saturating_sub(1);
            let mut node = syntax.and_then(|syntax| {
                syntax.descendant_for_byte_range(byte as u32, (byte + 1).min(text.len()) as u32)
            });
            let code_boundary = node
                .as_ref()
                .is_some_and(|node| matches!(node.kind(), "{" | "}" | ";" | ")" | "]"));

            while let Some(current) = node {
                if current.kind() == "ERROR" && !code_boundary {
                    return range;
                }

                if matches!(
                    current.kind(),
                    "string_literal"
                        | "raw_string_literal"
                        | "char_literal"
                        | "line_comment"
                        | "block_comment"
                ) {
                    return range;
                }

                node = current.parent();
            }

            let start = slice.line_to_char(slice.char_to_line(range.head));
            let prefix = slice.slice(start..range.head).to_string();

            if prefix.chars().all(|ch| ch == ' ' || ch == '\t') {
                let removed = if prefix.ends_with('\t') {
                    1
                } else {
                    prefix
                        .chars()
                        .rev()
                        .take_while(|ch| *ch == ' ')
                        .take(4)
                        .count()
                };

                Range::new(range.head - removed, range.head)
            } else {
                range
            }
        });

        self.views.get_mut(&id).unwrap().selection = selection;
        self.replace_selections(id, &["}".into()], false, false);
    }

    fn find_character(
        &mut self,
        id: u64,
        direction: Direction,
        till: bool,
        target: char,
        count: usize,
    ) {
        let slice = self.rope.slice(..);
        let forward = direction == Direction::Forward;
        let extend = self.views[&id].mode == TextMode::Select;
        let selection = self.views[&id].selection.clone().transform(|range| {
            let start = range.cursor(slice);
            let found = if forward {
                slice
                    .chars_at((start + 1).min(slice.len_chars()))
                    .enumerate()
                    .filter(|(_, ch)| *ch == target)
                    .nth(count - 1)
                    .map(|(offset, _)| start + offset + 1)
            } else {
                let mut chars = slice.chars_at(start);

                chars.reverse();
                chars
                    .enumerate()
                    .filter(|(_, ch)| *ch == target)
                    .nth(count - 1)
                    .map(|(offset, _)| start - offset - 1)
            };

            if let Some(mut pos) = found {
                if till {
                    pos = if forward {
                        prev_grapheme_boundary(slice, pos)
                    } else {
                        next_grapheme_boundary(slice, pos)
                    };
                }

                range.put_cursor(slice, pos, extend)
            } else {
                range
            }
        });

        self.views.get_mut(&id).unwrap().selection = selection.ensure_invariants(slice);
    }

    fn build_regex(&self, pattern: &str) -> Result<helix_stdx::rope::Regex> {
        helix_stdx::rope::RegexBuilder::new()
            .syntax(
                helix_stdx::rope::Config::new()
                    .case_insensitive(!pattern.chars().any(char::is_uppercase))
                    .multi_line(true)
                    .crlf(
                        helix_core::line_ending::auto_detect_line_ending(&self.rope)
                            == Some(helix_core::LineEnding::Crlf),
                    ),
            )
            .build(pattern)
            .context("invalid regular expression")
    }

    fn regex_selection(
        &mut self,
        id: u64,
        operation: TextPromptKind,
        regex: &helix_stdx::rope::Regex,
        movement: Movement,
    ) -> Result<()> {
        let slice = self.rope.slice(..);
        let view = self.views.get_mut(&id).unwrap();
        if matches!(operation, TextPromptKind::Select | TextPromptKind::Split) {
            let selection = if operation == TextPromptKind::Select {
                helix_core::selection::select_on_matches(slice, &view.selection, regex)
                    .context("nothing selected")?
            } else {
                helix_core::selection::split_on_matches(slice, &view.selection, regex)
            };
            view.selection = selection.ensure_invariants(slice);
            view.manual_viewport = false;
            return Ok(());
        }

        let primary = view.selection.primary();
        let start = if operation == TextPromptKind::SearchBackward {
            helix_core::graphemes::ensure_grapheme_boundary_prev(slice, primary.from())
        } else {
            helix_core::graphemes::ensure_grapheme_boundary_next(slice, primary.to())
        };
        let start = slice.char_to_byte(start);
        let found = if operation == TextPromptKind::SearchBackward {
            regex
                .find_iter(slice.regex_input_at_bytes(..start))
                .last()
                .or_else(|| regex.find_iter(slice.regex_input_at_bytes(start..)).last())
        } else {
            regex
                .find(slice.regex_input_at_bytes(start..))
                .or_else(|| regex.find(slice.regex_input()))
        };

        if let Some(found) = found
            && found.end() != 0
        {
            let range = Range::new(
                slice.byte_to_char(found.start()),
                slice.byte_to_char(found.end()),
            )
            .with_direction(primary.direction());
            view.selection = match movement {
                Movement::Extend => view.selection.clone().push(range),
                Movement::Move => view
                    .selection
                    .clone()
                    .replace(view.selection.primary_index(), range),
            }
            .ensure_invariants(slice);
            view.manual_viewport = false;
        }
        Ok(())
    }

    pub fn outline(&mut self) -> Vec<OutlineSymbol> {
        self.language
            .as_mut()
            .map_or_else(Vec::new, |language| language.outline(&self.rope))
    }

    /// Move or extend the primary Helix selection to a rendered character position.
    pub fn select_at(&mut self, id: u64, position: usize, extend: bool) {
        self.ensure_view(id);
        self.commit_group();
        self.cancel_pending_command(id);
        let slice = self.rope.slice(..);
        let position = position.min(slice.len_chars());
        let view = self.views.get_mut(&id).unwrap();
        let selection = if extend {
            let range = view.selection.primary().put_cursor(slice, position, true);
            Selection::single(range.anchor, range.head)
        } else {
            Selection::point(position)
        };
        view.selection = if view.mode == TextMode::Insert && !extend {
            selection
        } else {
            selection.ensure_invariants(slice)
        };
        view.manual_viewport = false;
        self.refresh_cursors();
    }

    pub fn scroll_view(&mut self, id: u64, horizontal: isize, vertical: isize) {
        self.ensure_view(id);
        let line_count = self.rope.len_lines();
        let maximum_column = if horizontal == 0 {
            None
        } else {
            Some(self.maximum_line_width())
        };

        let view = self.views.get_mut(&id).unwrap();
        let maximum_row = line_count.saturating_sub(view.viewport_height.max(1));
        view.scroll = view.scroll.saturating_add_signed(vertical).min(maximum_row);

        if let Some(maximum_column) = maximum_column {
            view.horizontal_scroll = view
                .horizontal_scroll
                .saturating_add_signed(horizontal)
                .min(maximum_column.saturating_sub(view.viewport_width));
        }

        view.manual_viewport = true;
    }

    fn maximum_line_width(&mut self) -> usize {
        let tab_width = self.indentation().1;
        if let Some((version, tabs, width)) = self.line_width {
            if version == self.version && tabs == tab_width {
                return width;
            }
        }
        let width = self
            .rope
            .lines()
            .map(|line| visual_width(line.to_string().trim_end_matches(['\r', '\n']), tab_width))
            .max()
            .unwrap_or(0);
        self.line_width = Some((self.version, tab_width, width));
        width
    }

    /// Renders text without allocating editing state.
    pub fn render_preview(
        &mut self,
        viewport: &mut PreviewViewport,
        width: usize,
        height: usize,
        soft_wrap: bool,
    ) -> Vec<FrameRow> {
        if soft_wrap && width > 0 {
            return self.render_wrapped_preview(viewport, width, height);
        }
        if viewport.horizontal_scroll > 0 {
            viewport.horizontal_scroll = viewport
                .horizontal_scroll
                .min(self.maximum_line_width().saturating_sub(width));
        }
        viewport.width = width;
        viewport.height = height;
        viewport.scroll = viewport
            .scroll
            .min(self.line_count().saturating_sub(height.max(1)));
        let tab_width = self.indentation().1;
        paint_text(
            &self.rope,
            &mut self.language,
            self.readonly,
            tab_width,
            TextPaint {
                viewport: PreviewViewport {
                    scroll: viewport.scroll,
                    horizontal_scroll: viewport.horizontal_scroll,
                    width,
                    height,
                },
                selection: None,
                focused: false,
                jump_labels: &HashMap::new(),
            },
        )
        .0
    }

    fn render_wrapped_preview(
        &mut self,
        viewport: &mut PreviewViewport,
        width: usize,
        height: usize,
    ) -> Vec<FrameRow> {
        let tab_width = self.indentation().1;
        let format = TextFormat {
            tab_width: tab_width as u16,
            ..TextFormat::default()
        };
        let slice = self.rope.slice(..);
        let annotations = TextAnnotations::default();
        if self.preview_wrap.as_ref().is_none_or(|layout| {
            layout.version != self.version || layout.width != width || layout.tabs != tab_width
        }) {
            self.preview_wrap = Some(PreviewWrap {
                version: self.version,
                width,
                tabs: tab_width,
                rows: Vec::new(),
                complete: false,
            });
        }
        let layout = self.preview_wrap.as_mut().unwrap();
        let requested = viewport
            .scroll
            .saturating_add(height.max(1))
            .saturating_add(1);
        if !layout.complete && layout.rows.len() < requested {
            let start = layout.rows.last().copied().unwrap_or(0);
            let base = layout.rows.len().saturating_sub(1);
            let mut formatter = wrap_preview_graphemes(
                DocumentFormatter::new_at_prev_checkpoint(
                    slice.slice(start..),
                    &format,
                    &annotations,
                    0,
                ),
                width,
                format.tab_width,
            );
            loop {
                let Some(grapheme) = formatter.next() else {
                    layout.complete = true;
                    break;
                };
                if base + grapheme.visual_pos.row == layout.rows.len() {
                    layout.rows.push(start + grapheme.char_idx);
                }
                if grapheme.source.is_eof() {
                    layout.complete = true;
                    break;
                }
                if layout.rows.len() >= requested {
                    break;
                }
            }
        }
        viewport.width = width;
        viewport.height = height;
        viewport.horizontal_scroll = 0;
        if layout.complete {
            viewport.scroll = viewport
                .scroll
                .min(layout.rows.len().saturating_sub(height.max(1)));
        }
        let Some(&start_char) = layout.rows.get(viewport.scroll) else {
            return Vec::new();
        };
        let end_char = layout
            .rows
            .get(viewport.scroll.saturating_add(height))
            .copied()
            .unwrap_or(slice.len_chars());
        let start_byte = slice.char_to_byte(start_char);
        let end_byte = slice.char_to_byte(end_char);
        let highlights = self.language.as_mut().map_or_else(Vec::new, |language| {
            language.highlights(&self.rope, start_byte..end_byte)
        });
        let scopes = self.language.as_ref().map(|_| language::loader().scopes());
        let mut highlight_index = 0;
        let mut rows = Vec::<FrameRow>::new();
        for grapheme in wrap_preview_graphemes(
            DocumentFormatter::new_at_prev_checkpoint(
                slice.slice(start_char..),
                &format,
                &annotations,
                0,
            ),
            width,
            format.tab_width,
        ) {
            let row = grapheme.visual_pos.row;
            if row >= height {
                break;
            }
            rows.resize_with(row + 1, FrameRow::default);
            if grapheme.source.is_eof() || matches!(grapheme.raw, Grapheme::Newline) {
                continue;
            }
            let position = slice.char_to_byte(start_char + grapheme.char_idx);
            while highlights
                .get(highlight_index)
                .is_some_and(|(range, _)| range.end <= position)
            {
                highlight_index += 1;
            }
            let style = highlights
                .get(highlight_index)
                .filter(|(range, _)| range.contains(&position))
                .and_then(|(_, index)| scopes.as_ref().and_then(|scopes| scopes.get(*index)))
                .map_or("", String::as_str);
            let used = rows[row]
                .spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                .sum::<usize>();
            if grapheme.visual_pos.col > used {
                rows[row].spans.push(Span {
                    base_style: None,
                    text: " ".repeat(grapheme.visual_pos.col - used),
                    style: String::new(),
                });
            }
            let value = if grapheme.width() > width {
                " ".repeat(width)
            } else {
                grapheme.raw.to_string()
            };
            let text = value
                .chars()
                .map(|ch| match ch {
                    '\0'..='\u{1f}' => char::from_u32(0x2400 + ch as u32).unwrap(),
                    '\u{7f}' => '\u{2421}',
                    ch if ch.is_control() => '\u{fffd}',
                    ch => ch,
                })
                .collect::<String>();
            if let Some(last) = rows[row]
                .spans
                .last_mut()
                .filter(|span| span.style == style)
            {
                last.text.push_str(&text);
            } else {
                rows[row].spans.push(Span {
                    base_style: None,
                    text,
                    style: style.into(),
                });
            }
        }
        rows
    }

    pub fn render(&mut self, id: u64, width: usize, height: usize, focused: bool) -> RenderResult {
        self.ensure_view(id);
        self.refresh_cursors();
        let tab_width = self.indentation().1;

        let view = &self.views[&id];
        let maximum_column = if !view.manual_viewport
            || view.horizontal_scroll == 0
            || (view.viewport_width == width && view.viewport_version == self.version)
        {
            None
        } else {
            Some(self.maximum_line_width())
        };
        let slice = self.rope.slice(..);
        let view = self.views.get_mut(&id).unwrap();
        if let Some(maximum_column) = maximum_column {
            view.horizontal_scroll = view
                .horizontal_scroll
                .min(maximum_column.saturating_sub(width));
        }
        view.viewport_width = width;
        view.viewport_height = height;
        view.viewport_version = self.version;
        if view.manual_viewport {
            view.scroll = view
                .scroll
                .min(slice.len_lines().saturating_sub(height.max(1)));
        }
        let (row, _) = view.cursor;
        let cursor_char = if view.mode == TextMode::Insert {
            view.selection.primary().head
        } else {
            view.selection.primary().cursor(slice)
        };
        let line_start = slice.line_to_char(row);
        let cursor_column =
            visual_width(&slice.slice(line_start..cursor_char).to_string(), tab_width);

        if !view.manual_viewport && row < view.scroll {
            view.scroll = row;
        } else if !view.manual_viewport && row >= view.scroll + height.max(1) {
            view.scroll = row + 1 - height.max(1);
        }

        if !view.manual_viewport && cursor_column < view.horizontal_scroll {
            view.horizontal_scroll = cursor_column;
        } else if !view.manual_viewport && cursor_column >= view.horizontal_scroll + width.max(1) {
            view.horizontal_scroll = cursor_column + 1 - width.max(1);
        }

        let words = match &view.pending {
            PendingInput::GotoWord { words, .. } => words.as_slice(),
            _ => &[],
        };
        let jump_labels = words
            .iter()
            .enumerate()
            .flat_map(|(index, range)| {
                [
                    (range.from(), char::from(b'a' + (index / 26) as u8)),
                    (
                        next_grapheme_boundary(slice, range.from()),
                        char::from(b'a' + (index % 26) as u8),
                    ),
                ]
            })
            .collect::<HashMap<_, _>>();
        let (rows, hit_positions) = paint_text(
            &self.rope,
            &mut self.language,
            self.readonly,
            tab_width,
            TextPaint {
                viewport: PreviewViewport {
                    scroll: view.scroll,
                    horizontal_scroll: view.horizontal_scroll,
                    width,
                    height,
                },
                selection: Some(&view.selection),
                focused,
                jump_labels: &jump_labels,
            },
        );

        let cursor = if focused
            && width > 0
            && height > 0
            && row >= view.scroll
            && row < view.scroll + height
            && cursor_column >= view.horizontal_scroll
            && cursor_column < view.horizontal_scroll + width
        {
            Some((
                (cursor_column - view.horizontal_scroll) as u16,
                (row - view.scroll) as u16,
            ))
        } else {
            None
        };

        RenderResult {
            hit_positions,
            rows,
            cursor,
            scroll: view.scroll,
        }
    }
}

#[derive(Default)]
pub struct PreviewViewport {
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub width: usize,
    pub height: usize,
}

struct PreviewWrap {
    version: u64,
    width: usize,
    tabs: usize,
    rows: Vec<usize>,
    complete: bool,
}

fn wrap_preview_graphemes<'a>(
    formatter: DocumentFormatter<'a>,
    width: usize,
    tab_width: u16,
) -> impl Iterator<Item = FormattedGrapheme<'a>> {
    formatter.scan((0, 0), move |(row, column), mut grapheme| {
        grapheme.raw.change_position(*column, tab_width);
        let newline = matches!(grapheme.raw, Grapheme::Newline);
        if !newline
            && !grapheme.source.is_eof()
            && *column > 0
            && *column + grapheme.width() > width
        {
            *row += 1;
            *column = 0;
            grapheme.raw.change_position(0, tab_width);
        }
        grapheme.visual_pos = helix_core::Position::new(*row, *column);
        if newline {
            *row += 1;
            *column = 0;
        } else if !grapheme.source.is_eof() {
            *column += grapheme.width();
        }
        Some(grapheme)
    })
}

struct TextPaint<'a> {
    viewport: PreviewViewport,
    selection: Option<&'a Selection>,
    focused: bool,
    jump_labels: &'a HashMap<usize, char>,
}

fn paint_text(
    rope: &Rope,
    language: &mut Option<LanguageAttachment>,
    readonly: bool,
    tab_width: usize,
    TextPaint {
        viewport,
        selection,
        focused,
        jump_labels,
    }: TextPaint<'_>,
) -> (Vec<FrameRow>, Vec<Vec<usize>>) {
    let width = viewport.width;
    let height = viewport.height;
    let slice = rope.slice(..);
    let start_byte = slice.line_to_byte(viewport.scroll);
    let end_row = (viewport.scroll + height).min(slice.len_lines());
    let end_byte = if end_row == slice.len_lines() {
        slice.len_bytes()
    } else {
        slice.line_to_byte(end_row)
    };
    let highlights = language.as_mut().map_or_else(Vec::new, |language| {
        language.highlights(&rope, start_byte..end_byte)
    });
    let scopes = language.as_ref().map(|_| language::loader().scopes());
    let mut highlight_index = 0;

    let start_char = slice.byte_to_char(start_byte);
    let end_char = slice.byte_to_char(end_byte);
    let ranges = selection.map_or(&[][..], Selection::ranges);
    let first_range = ranges.partition_point(|range| {
        range.to() < start_char || (range.to() == start_char && range.from() != start_char)
    });
    let end_range = ranges.partition_point(|range| range.from() < end_char);
    let visible_ranges = &ranges[first_range..end_range];
    let mut overlays = OverlayHighlighter::new([
        OverlayHighlights::Homogeneous {
            highlight: Highlight::new(0),
            ranges: visible_ranges
                .iter()
                .filter(|range| (!readonly || focused) && range.from() < range.to())
                .map(|range| range.from()..range.to())
                .collect(),
        },
        OverlayHighlights::Homogeneous {
            highlight: Highlight::new(1),
            ranges: visible_ranges
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    selection.is_some_and(|selection| {
                        focused && first_range + index != selection.primary_index()
                    })
                })
                .map(|(_, range)| range.cursor(slice))
                .filter(|pos| start_char <= *pos && *pos < end_char)
                .map(|pos| pos..pos + 1)
                .collect(),
        },
    ]);
    let overlay_styles = ["selection", "cursor"];
    let mut overlay_style = "";

    let mut rows = Vec::new();

    let mut hit_positions = Vec::new();
    for line_index in viewport.scroll..(viewport.scroll + height).min(slice.len_lines()) {
        let line = slice.line(line_index);
        let text = line.to_string();
        let text = text.trim_end_matches(['\r', '\n']);
        let start_char = slice.line_to_char(line_index);
        let start_byte = slice.line_to_byte(line_index);
        let mut char_offset = 0;
        let mut column = 0;
        let mut spans: Vec<Span> = Vec::new();
        let mut hits = Vec::with_capacity(width);

        for (byte, grapheme) in text.grapheme_indices(true) {
            let display_width = if grapheme == "\t" {
                tab_width - column % tab_width
            } else {
                UnicodeWidthStr::width(grapheme)
            };
            let grapheme_width = display_width.max(1);
            let pos = start_char + char_offset;
            while overlays.next_event_offset() <= pos {
                let (event, highlights) = overlays.advance();
                if matches!(event, HighlightEvent::Refresh) {
                    overlay_style = "";
                }
                if let Some(highlight) = highlights.last() {
                    overlay_style = overlay_styles[highlight.idx()];
                }
            }
            let label = jump_labels.get(&pos);
            let style = if label.is_some() {
                "pending"
            } else if !overlay_style.is_empty() {
                overlay_style
            } else {
                let position = start_byte + byte;
                while highlights
                    .get(highlight_index)
                    .is_some_and(|(range, _)| range.end <= position)
                {
                    highlight_index += 1;
                }
                highlights
                    .get(highlight_index)
                    .filter(|(range, _)| range.contains(&position))
                    .and_then(|(_, index)| scopes.as_ref().and_then(|scopes| scopes.get(*index)))
                    .map_or("", String::as_str)
            };
            let visible_start = column.max(viewport.horizontal_scroll);
            let visible_end = (column + grapheme_width).min(viewport.horizontal_scroll + width);

            if visible_start < visible_end {
                hits.extend(std::iter::repeat_n(pos, visible_end - visible_start));
                let value = if let Some(label) = label {
                    format!("{label}{}", " ".repeat(visible_end - visible_start - 1))
                } else if grapheme == "\t"
                    || visible_start != column
                    || visible_end != column + grapheme_width
                {
                    " ".repeat(visible_end - visible_start)
                } else if display_width == 0 {
                    " ".into()
                } else if grapheme.chars().any(char::is_control) {
                    grapheme
                        .chars()
                        .map(|ch| match ch {
                            '\0'..='\u{1f}' => char::from_u32(0x2400 + ch as u32).unwrap(),
                            '\u{7f}' => '\u{2421}',
                            ch if ch.is_control() => '\u{fffd}',
                            ch => ch,
                        })
                        .collect()
                } else {
                    grapheme.into()
                };

                if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
                    last.text.push_str(&value);
                } else {
                    spans.push(Span {
                        base_style: None,
                        text: value,
                        style: style.into(),
                    });
                }
            }

            column += grapheme_width;
            char_offset += grapheme.chars().count();

            if column >= viewport.horizontal_scroll + width {
                break;
            }
        }

        hits.resize(width, start_char + text.chars().count());
        hit_positions.push(hits);
        rows.push(FrameRow { spans });
    }

    (rows, hit_positions)
}

fn visual_width(text: &str, tab_width: usize) -> usize {
    text.graphemes(true).fold(0, |column, grapheme| {
        column
            + if grapheme == "\t" {
                tab_width - column % tab_width
            } else {
                UnicodeWidthStr::width(grapheme).max(1)
            }
    })
}
