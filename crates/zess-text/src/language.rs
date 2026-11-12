use std::{
    ops::Range,
    path::Path,
    sync::{
        Arc, OnceLock, Weak,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

use helix_core::{ChangeSet, Language, Rope, indent::IndentStyle, syntax::Syntax};
use zess_plugin::OutlineSymbol;

#[cfg(test)]
mod tests;

mod helix;

pub use helix::HelixSyntaxProvider;

/// Owns language resources, grammar loading, and syntax queries.
/// Implementations may load resources while parsing, updating, or querying syntax.
/// Language IDs and syntax values must be used with the provider that produced them.
pub trait SyntaxProvider: Send + Sync {
    fn language_for_path(&self, path: &Path) -> Option<Language>;
    fn language_for_name(&self, name: &str) -> Option<Language>;
    fn config(&self, language: Language) -> &helix_core::syntax::config::LanguageConfiguration;
    fn has_outline(&self, language: Language) -> bool;
    fn parse(&self, rope: &Rope, language: Language) -> Result<Syntax, String>;
    fn update(
        &self,
        syntax: &mut Syntax,
        old: &Rope,
        new: &Rope,
        changes: &ChangeSet,
    ) -> Result<(), String>;
    fn highlights(
        &self,
        syntax: &Syntax,
        rope: &Rope,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, usize)>;
    fn outline(&self, syntax: &Syntax, rope: &Rope) -> Vec<OutlineSymbol>;
    fn scopes(&self) -> Arc<Vec<String>>;
}

pub(crate) fn default_provider() -> Arc<dyn SyntaxProvider> {
    static PROVIDER: OnceLock<Arc<HelixSyntaxProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(HelixSyntaxProvider::default()))
        .clone()
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
    provider: Arc<dyn SyntaxProvider>,
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
                        let syntax = request.provider.parse(&request.rope, request.language);
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
    provider: Arc<dyn SyntaxProvider>,
    language: Language,
    syntax: SyntaxState,
    outline: Option<Vec<OutlineSymbol>>,
}

impl LanguageAttachment {
    pub(crate) fn new(language: Language, provider: Arc<dyn SyntaxProvider>) -> Self {
        Self {
            provider,
            language,
            syntax: SyntaxState::Pending,
            outline: None,
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.config().language_id
    }

    pub(crate) fn config(&self) -> &helix_core::syntax::config::LanguageConfiguration {
        self.provider.config(self.language)
    }

    pub(crate) fn has_outline(&self) -> bool {
        self.provider.has_outline(self.language)
    }

    pub(crate) fn indentation(&self) -> Option<(IndentStyle, usize)> {
        let indent = self.config().indent.as_ref()?;
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
                provider: self.provider.clone(),
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
            Ok(Ok(syntax)) => SyntaxState::BackgroundReady(syntax),
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
            self.syntax = match self.provider.parse(rope, self.language) {
                Ok(syntax) => SyntaxState::Ready(syntax),
                Err(error) => SyntaxState::Unavailable(error),
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
            && let Err(error) = self.provider.update(syntax, old, new, changes)
        {
            self.syntax = SyntaxState::Unavailable(error.to_string());
        }
    }

    pub(crate) fn scopes(&self) -> Arc<Vec<String>> {
        self.provider.scopes()
    }

    pub(crate) fn highlights(
        &mut self,
        rope: &Rope,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, usize)> {
        let provider = self.provider.clone();
        self.syntax(rope)
            .map_or_else(Vec::new, |syntax| provider.highlights(syntax, rope, range))
    }

    pub(crate) fn outline(&mut self, rope: &Rope) -> Vec<OutlineSymbol> {
        if let Some(outline) = &self.outline {
            return outline.clone();
        }
        let provider = self.provider.clone();
        let symbols = self
            .syntax(rope)
            .map_or_else(Vec::new, |syntax| provider.outline(syntax, rope));
        self.outline = Some(symbols.clone());
        symbols
    }
}
