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

pub(crate) fn default_provider() -> Arc<HelixSyntaxProvider> {
    static PROVIDER: OnceLock<Arc<HelixSyntaxProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(HelixSyntaxProvider::default()))
        .clone()
}

struct SyntaxEdits {
    old: Rope,
    changes: Vec<ChangeSet>,
}

impl SyntaxEdits {
    fn new(old: &Rope, changes: &ChangeSet) -> Self {
        Self {
            old: old.clone(),
            changes: vec![changes.clone()],
        }
    }
}

enum ParseWork {
    Full,
    Incremental { syntax: Syntax, edits: SyntaxEdits },
}

impl ParseWork {
    fn run(
        self,
        provider: &dyn SyntaxProvider,
        language: Language,
        rope: &Rope,
    ) -> Result<Syntax, String> {
        match self {
            Self::Full => provider.parse(rope, language),
            Self::Incremental { mut syntax, edits } => {
                let changes = edits
                    .changes
                    .into_iter()
                    .reduce(ChangeSet::compose)
                    .unwrap();
                provider.update(&mut syntax, &edits.old, rope, &changes)?;
                Ok(syntax)
            }
        }
    }
}

enum SyntaxState {
    Dormant,
    Pending(ParseWork),
    Running {
        result: Receiver<Result<Syntax, String>>,
        _subscription: Arc<()>,
        edits: Option<SyntaxEdits>,
    },
    Ready(Syntax),
    Unavailable(String),
}

impl From<Result<Syntax, String>> for SyntaxState {
    fn from(result: Result<Syntax, String>) -> Self {
        match result {
            Ok(syntax) => Self::Ready(syntax),
            Err(error) => Self::Unavailable(error),
        }
    }
}

struct ParseRequest {
    provider: Arc<dyn SyntaxProvider>,
    language: Language,
    rope: Rope,
    work: ParseWork,
    result: mpsc::Sender<Result<Syntax, String>>,
    subscription: Weak<()>,
}

fn parser() -> Result<&'static SyncSender<ParseRequest>, String> {
    static PARSER: OnceLock<Result<SyncSender<ParseRequest>, String>> = OnceLock::new();
    PARSER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<ParseRequest>(4);
            thread::Builder::new()
                .name("zess-syntax".into())
                .spawn(move || {
                    for request in receiver {
                        if request.subscription.strong_count() == 0 {
                            continue;
                        }
                        let syntax = request.work.run(
                            request.provider.as_ref(),
                            request.language,
                            &request.rope,
                        );
                        let _ = request.result.send(syntax);
                    }
                })
                .map_err(|error| format!("cannot start syntax parser: {error}"))?;
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
            syntax: SyntaxState::Dormant,
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
            SyntaxState::Unavailable(error) => Some(error),
            _ => None,
        }
    }

    pub(crate) fn request(&mut self) {
        if matches!(self.syntax, SyntaxState::Dormant) {
            self.syntax = SyntaxState::Pending(ParseWork::Full);
        }
    }

    pub(crate) fn finished(&self) -> bool {
        matches!(
            self.syntax,
            SyntaxState::Ready(_) | SyntaxState::Unavailable(_)
        )
    }

    fn receive(&mut self) -> bool {
        let SyntaxState::Running { result, edits, .. } = &mut self.syntax else {
            return false;
        };
        let result = match result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("syntax parser stopped without a result".into()),
        };

        if let Some(edits) = edits.take() {
            self.syntax = SyntaxState::Pending(result.map_or(ParseWork::Full, |syntax| {
                ParseWork::Incremental { syntax, edits }
            }));
            return false;
        }

        self.syntax = result.into();
        true
    }

    pub(crate) fn poll(&mut self, rope: &Rope) -> bool {
        if self.receive() {
            return true;
        }
        if !matches!(self.syntax, SyntaxState::Pending(_)) {
            return false;
        }

        let sender = match parser() {
            Ok(sender) => sender,
            Err(error) => {
                self.syntax = SyntaxState::Unavailable(error);
                return true;
            }
        };
        let SyntaxState::Pending(work) =
            std::mem::replace(&mut self.syntax, SyntaxState::Pending(ParseWork::Full))
        else {
            unreachable!()
        };
        let (result, receiver) = mpsc::channel();
        let subscription = Arc::new(());
        match sender.try_send(ParseRequest {
            provider: self.provider.clone(),
            language: self.language,
            rope: rope.clone(),
            work,
            result,
            subscription: Arc::downgrade(&subscription),
        }) {
            Ok(()) => {
                self.syntax = SyntaxState::Running {
                    result: receiver,
                    _subscription: subscription,
                    edits: None,
                };
                false
            }
            Err(TrySendError::Full(request)) => {
                self.syntax = SyntaxState::Pending(request.work);
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.syntax = SyntaxState::Unavailable("syntax parser stopped".into());
                true
            }
        }
    }

    fn ready_syntax(&self) -> Option<&Syntax> {
        match &self.syntax {
            SyntaxState::Ready(syntax) => Some(syntax),
            _ => None,
        }
    }

    /// Explicit syntax-dependent commands need the current tree to finish the operation.
    pub(crate) fn syntax_for_command(&mut self, rope: &Rope) -> Option<&Syntax> {
        self.receive();
        if !self.finished() {
            let work =
                match std::mem::replace(&mut self.syntax, SyntaxState::Pending(ParseWork::Full)) {
                    SyntaxState::Pending(work) => work,
                    SyntaxState::Dormant | SyntaxState::Running { .. } => ParseWork::Full,
                    _ => unreachable!(),
                };
            self.syntax = work.run(self.provider.as_ref(), self.language, rope).into();
        }
        self.ready_syntax()
    }

    pub(crate) fn update(&mut self, old: &Rope, changes: &ChangeSet) {
        if changes.is_empty() {
            return;
        }
        self.outline = None;
        match &mut self.syntax {
            SyntaxState::Dormant | SyntaxState::Pending(ParseWork::Full) => {}
            SyntaxState::Pending(ParseWork::Incremental { edits, .. }) => {
                edits.changes.push(changes.clone());
            }
            SyntaxState::Running { edits, .. } => match edits {
                Some(edits) => edits.changes.push(changes.clone()),
                None => *edits = Some(SyntaxEdits::new(old, changes)),
            },
            SyntaxState::Ready(_) => {
                let SyntaxState::Ready(syntax) =
                    std::mem::replace(&mut self.syntax, SyntaxState::Pending(ParseWork::Full))
                else {
                    unreachable!()
                };
                self.syntax = SyntaxState::Pending(ParseWork::Incremental {
                    syntax,
                    edits: SyntaxEdits::new(old, changes),
                });
            }
            SyntaxState::Unavailable(_) => {
                self.syntax = SyntaxState::Pending(ParseWork::Full);
            }
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
        self.request();
        self.ready_syntax().map_or_else(Vec::new, |syntax| {
            self.provider.highlights(syntax, rope, range)
        })
    }

    pub(crate) fn outline(&mut self, rope: &Rope) -> Vec<OutlineSymbol> {
        if let Some(outline) = &self.outline {
            return outline.clone();
        }
        let provider = self.provider.clone();
        let symbols = self
            .syntax_for_command(rope)
            .map_or_else(Vec::new, |syntax| provider.outline(syntax, rope));
        self.outline = Some(symbols.clone());
        symbols
    }
}
