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
    doc_formatter::TextFormat,
    graphemes::{next_grapheme_boundary, prev_grapheme_boundary},
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
            }
        }
        self.path = Some(path);
        self.saved_revision = self.history.current_revision();

        Ok(())
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

    fn indentation(&self) -> (helix_core::indent::IndentStyle, usize) {
        self.language
            .as_ref()
            .and_then(LanguageAttachment::indentation)
            .unwrap_or((helix_core::indent::IndentStyle::Spaces(4), 4))
    }

    pub fn syntax_error(&self) -> Option<&str> {
        self.language.as_ref().and_then(LanguageAttachment::error)
    }

    pub fn outline(&mut self) -> Vec<OutlineSymbol> {
        self.language
            .as_mut()
            .map_or_else(Vec::new, |language| language.outline(&self.rope))
    }
}

#[derive(Default)]
pub struct PreviewViewport {
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub width: usize,
    pub height: usize,
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
