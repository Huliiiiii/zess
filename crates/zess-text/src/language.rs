use std::{
    ops::Range,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

use helix_core::{
    ChangeSet, Language, Rope,
    indent::IndentStyle,
    syntax::{LanguageLoader, Loader, QueryMatchIterEvent, Syntax},
};
use zess_plugin::OutlineSymbol;

#[cfg(test)]
mod tests;

pub(crate) fn loader() -> &'static Loader {
    static LOADER: OnceLock<Loader> = OnceLock::new();
    LOADER.get_or_init(|| {
        crate::runtime::initialize(None).expect("initialize Zess syntax runtime");
        let config = helix_loader::merge_toml_values(
            helix_loader::config::default_lang_config(),
            toml::from_str(include_str!("../languages.toml"))
                .expect("bundled language configuration is valid"),
            3,
        )
        .try_into()
        .expect("bundled Helix language configuration is valid");
        Loader::new(config).expect("bundled language file types are valid")
    })
}

pub(crate) fn detect(path: &Path) -> Option<LanguageAttachment> {
    loader()
        .language_for_filename(&helix_stdx::path::canonicalize(path))
        .map(LanguageAttachment::new)
}

enum SyntaxState {
    Pending,
    BackgroundPending,
    Background {
        result: Receiver<Result<Syntax, String>>,
        _subscription: Arc<()>,
    },
    Ready(Syntax),
    BackgroundReady(Syntax),
    Unavailable(String),
    BackgroundUnavailable(String),
}

struct ParseRequest {
    language: Language,
    rope: Rope,
    result: mpsc::Sender<Result<Syntax, String>>,
    subscription: Weak<()>,
}

fn parser() -> Result<&'static SyncSender<ParseRequest>, String> {
    static PARSER: OnceLock<Result<SyncSender<ParseRequest>, String>> = OnceLock::new();
    PARSER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<ParseRequest>(4);
            thread::Builder::new()
                .name("zess-preview-syntax".into())
                .spawn(move || {
                    for request in receiver {
                        if request.subscription.strong_count() == 0 {
                            continue;
                        }
                        let syntax =
                            Syntax::new(request.rope.slice(..), request.language, loader())
                                .map_err(|error| error.to_string());
                        let _ = request.result.send(syntax);
                    }
                })
                .map_err(|error| format!("cannot start preview parser: {error}"))?;
            Ok(sender)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(crate) struct LanguageAttachment {
    language: Language,
    syntax: SyntaxState,
    outline: Option<Vec<OutlineSymbol>>,
}

impl LanguageAttachment {
    pub(crate) fn new(language: Language) -> Self {
        Self {
            language,
            syntax: SyntaxState::Pending,
            outline: None,
        }
    }

    pub(crate) fn id(&self) -> &str {
        &loader().language(self.language).config().language_id
    }

    pub(crate) fn config(&self) -> &helix_core::syntax::config::LanguageConfiguration {
        loader().language(self.language).config()
    }

    pub(crate) fn has_outline(&self) -> bool {
        loader().tag_query(self.language).is_some()
    }

    pub(crate) fn indentation(&self) -> Option<(IndentStyle, usize)> {
        let language = loader().language(self.language);
        let indent = language.config().indent.as_ref()?;
        Some((IndentStyle::from_str(&indent.unit), indent.tab_width))
    }

    pub(crate) fn error(&self) -> Option<&str> {
        match &self.syntax {
            SyntaxState::Unavailable(error) | SyntaxState::BackgroundUnavailable(error) => {
                Some(error)
            }
            _ => None,
        }
    }

    pub(crate) fn start_background(&mut self, rope: &Rope) {
        self.syntax = SyntaxState::BackgroundPending;
        self.poll(rope);
    }

    pub(crate) fn poll(&mut self, rope: &Rope) -> bool {
        if matches!(self.syntax, SyntaxState::BackgroundPending) {
            let sender = match parser() {
                Ok(sender) => sender,
                Err(error) => {
                    self.syntax = SyntaxState::BackgroundUnavailable(error);
                    return true;
                }
            };
            let (result, receiver) = mpsc::channel();
            let subscription = Arc::new(());
            match sender.try_send(ParseRequest {
                language: self.language,
                rope: rope.clone(),
                result,
                subscription: Arc::downgrade(&subscription),
            }) {
                Ok(()) => {
                    self.syntax = SyntaxState::Background {
                        result: receiver,
                        _subscription: subscription,
                    };
                }
                Err(TrySendError::Full(_)) => return false,
                Err(TrySendError::Disconnected(_)) => {
                    self.syntax =
                        SyntaxState::BackgroundUnavailable("preview parser stopped".into());
                    return true;
                }
            }
        }
        let SyntaxState::Background { result, .. } = &self.syntax else {
            return false;
        };
        self.syntax = match result.try_recv() {
            Ok(Ok(syntax)) => {
                register_scopes(self.language);
                SyntaxState::BackgroundReady(syntax)
            }
            Ok(Err(error)) => SyntaxState::BackgroundUnavailable(error),
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                SyntaxState::BackgroundUnavailable("preview parser stopped without a result".into())
            }
        };
        self.outline = None;
        true
    }

    pub(crate) fn syntax(&mut self, rope: &Rope) -> Option<&Syntax> {
        self.poll(rope);
        if matches!(self.syntax, SyntaxState::Pending) {
            self.syntax = match Syntax::new(rope.slice(..), self.language, loader()) {
                Ok(syntax) => {
                    register_scopes(self.language);
                    SyntaxState::Ready(syntax)
                }
                Err(error) => SyntaxState::Unavailable(error.to_string()),
            };
        }
        match &self.syntax {
            SyntaxState::Ready(syntax) | SyntaxState::BackgroundReady(syntax) => Some(syntax),
            _ => None,
        }
    }

    pub(crate) fn update(&mut self, old: &Rope, new: &Rope, changes: &ChangeSet) {
        if changes.is_empty() {
            return;
        }
        self.outline = None;
        if matches!(
            self.syntax,
            SyntaxState::BackgroundPending
                | SyntaxState::Background { .. }
                | SyntaxState::BackgroundReady(_)
                | SyntaxState::BackgroundUnavailable(_)
        ) {
            self.start_background(new);
            return;
        }
        if let SyntaxState::Ready(syntax) = &mut self.syntax
            && let Err(error) = syntax.update(old.slice(..), new.slice(..), changes, loader())
        {
            self.syntax = SyntaxState::Unavailable(error.to_string());
        }
    }

    pub(crate) fn highlights(
        &mut self,
        rope: &Rope,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, usize)> {
        let Some(syntax) = self.syntax(rope) else {
            return Vec::new();
        };
        let mut highlighter = syntax.highlighter(
            rope.slice(..),
            loader(),
            range.start as u32..range.end as u32,
        );
        let mut position = range.start;
        let mut spans = Vec::new();
        while position < range.end {
            while highlighter.next_event_offset() as usize <= position {
                highlighter.advance();
            }
            let end = (highlighter.next_event_offset() as usize).min(range.end);
            if let Some(highlight) = highlighter.active_highlights().last() {
                spans.push((position..end, highlight.idx()));
            }
            position = end;
        }
        spans
    }

    pub(crate) fn outline(&mut self, rope: &Rope) -> Vec<OutlineSymbol> {
        if let Some(outline) = &self.outline {
            return outline.clone();
        }
        let mut symbols = Vec::new();
        if let Some(syntax) = self.syntax(rope) {
            let mut tags = syntax.tags(rope.slice(..), loader(), ..);
            while let Some(event) = tags.next() {
                let QueryMatchIterEvent::Match(matched) = event else {
                    continue;
                };
                let Some(query) = loader().tag_query(tags.current_language()) else {
                    continue;
                };
                let mut definition = None;
                let mut name = None;
                for captured in &matched.nodes {
                    let capture = query.query.capture_name(captured.capture);
                    if capture.starts_with("definition.") {
                        definition = Some(&captured.node);
                    } else if capture == "name" {
                        name = Some(&captured.node);
                    }
                }
                let Some(definition) = definition else {
                    continue;
                };
                let name = name.unwrap_or(definition);
                let byte = definition.start_byte() as usize;
                let line = rope.byte_to_line(byte);
                let mut depth = 0;
                let mut parent = definition.parent();
                while let Some(node) = parent {
                    depth += 1;
                    parent = node.parent();
                }
                symbols.push(OutlineSymbol {
                    id: format!("{}:{byte}", definition.kind()),
                    name: rope
                        .byte_slice(name.start_byte() as usize..name.end_byte() as usize)
                        .to_string(),
                    kind: definition.kind().into(),
                    line,
                    column: byte - rope.line_to_byte(line),
                    depth,
                });
            }
        }
        self.outline = Some(symbols.clone());
        symbols
    }
}

// Appending preserves existing highlight IDs while other buffers are being rendered.
fn register_scopes(language: Language) {
    static REGISTRATION: Mutex<()> = Mutex::new(());
    let _registration = REGISTRATION.lock().unwrap();
    let loader = loader();
    let Some(config) = loader.get_config(language) else {
        return;
    };
    let mut scopes = loader.scopes().as_ref().clone();
    let previous_len = scopes.len();
    for (_, name) in config.highlight_query.query.captures() {
        if !scopes.iter().any(|scope| scope == name) {
            scopes.push(name.to_owned());
        }
    }
    if scopes.len() != previous_len {
        loader.set_scopes(scopes);
    }
}
