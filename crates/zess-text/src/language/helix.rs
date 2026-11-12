use std::{
    ops::Range,
    path::Path,
    sync::{Arc, Mutex},
};

use helix_core::{
    ChangeSet, Language, Rope,
    syntax::{LanguageLoader, Loader, QueryMatchIterEvent, Syntax},
};
use zess_platform::{FileSystem, NativeFileSystem, NativePlatform, Platform};
use zess_plugin::OutlineSymbol;

use super::SyntaxProvider;

/// Loads Helix grammars and queries from the selected host runtime directory.
pub struct HelixSyntaxProvider {
    platform: Arc<dyn Platform>,
    filesystem: Arc<dyn FileSystem>,
    loader: Loader,
    registration: Mutex<()>,
}

impl Default for HelixSyntaxProvider {
    fn default() -> Self {
        Self::with_platform(Arc::new(NativePlatform), Arc::new(NativeFileSystem))
    }
}

impl HelixSyntaxProvider {
    /// Builds the catalog and retains the host context for lazy resource loading.
    pub fn with_platform(platform: Arc<dyn Platform>, filesystem: Arc<dyn FileSystem>) -> Self {
        let config = helix_loader::merge_toml_values(
            helix_loader::config::default_lang_config(),
            toml::from_str(include_str!("../../languages.toml"))
                .expect("bundled language configuration is valid"),
            3,
        )
        .try_into()
        .expect("bundled Helix language configuration is valid");
        Self {
            platform,
            filesystem,
            loader: Loader::new(config).expect("bundled language file types are valid"),
            registration: Mutex::new(()),
        }
    }

    fn initialize_runtime(&self) -> Result<(), String> {
        crate::runtime::initialize_with_platform(
            None,
            self.platform.as_ref(),
            self.filesystem.as_ref(),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    // Appending preserves highlight IDs while other buffers are being rendered.
    fn register_scopes(&self, language: Language) {
        let _registration = self.registration.lock().unwrap();
        let Some(config) = self.loader.get_config(language) else {
            return;
        };
        let mut scopes = self.loader.scopes().as_ref().clone();
        let previous_len = scopes.len();
        for (_, name) in config.highlight_query.query.captures() {
            if !scopes.iter().any(|scope| scope == name) {
                scopes.push(name.to_owned());
            }
        }
        if scopes.len() != previous_len {
            self.loader.set_scopes(scopes);
        }
    }
}

impl SyntaxProvider for HelixSyntaxProvider {
    fn language_for_path(&self, path: &Path) -> Option<Language> {
        self.loader
            .language_for_filename(&helix_stdx::path::canonicalize(path))
    }

    fn language_for_name(&self, name: &str) -> Option<Language> {
        self.loader.language_for_name(name)
    }

    fn config(&self, language: Language) -> &helix_core::syntax::config::LanguageConfiguration {
        self.loader.language(language).config()
    }

    fn has_outline(&self, language: Language) -> bool {
        self.initialize_runtime().is_ok() && self.loader.tag_query(language).is_some()
    }

    fn parse(&self, rope: &Rope, language: Language) -> Result<Syntax, String> {
        self.initialize_runtime()?;
        let syntax = Syntax::new(rope.slice(..), language, &self.loader)
            .map_err(|error| error.to_string())?;
        self.register_scopes(language);
        Ok(syntax)
    }

    fn update(
        &self,
        syntax: &mut Syntax,
        old: &Rope,
        new: &Rope,
        changes: &ChangeSet,
    ) -> Result<(), String> {
        syntax
            .update(old.slice(..), new.slice(..), changes, &self.loader)
            .map_err(|error| error.to_string())
    }

    fn highlights(
        &self,
        syntax: &Syntax,
        rope: &Rope,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, usize)> {
        let mut highlighter = syntax.highlighter(
            rope.slice(..),
            &self.loader,
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

    fn outline(&self, syntax: &Syntax, rope: &Rope) -> Vec<OutlineSymbol> {
        let mut symbols = Vec::new();
        let mut tags = syntax.tags(rope.slice(..), &self.loader, ..);
        while let Some(event) = tags.next() {
            let QueryMatchIterEvent::Match(matched) = event else {
                continue;
            };
            let Some(query) = self.loader.tag_query(tags.current_language()) else {
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
        symbols
    }

    fn scopes(&self) -> Arc<Vec<String>> {
        Arc::clone(&self.loader.scopes())
    }
}
