use std::path::Path;

use crate::TextBuffer;

use super::*;

mod resources;

struct UnavailableSyntax {
    catalog: HelixSyntaxProvider,
}

impl SyntaxProvider for UnavailableSyntax {
    fn language_for_path(&self, _path: &Path) -> Option<Language> {
        self.catalog.language_for_name("python")
    }

    fn language_for_name(&self, name: &str) -> Option<Language> {
        self.catalog.language_for_name(name)
    }

    fn config(&self, language: Language) -> &helix_core::syntax::config::LanguageConfiguration {
        self.catalog.config(language)
    }

    fn has_outline(&self, _language: Language) -> bool {
        false
    }

    fn parse(&self, _rope: &Rope, _language: Language) -> Result<Syntax, String> {
        Err("syntax resources unavailable from provider".into())
    }

    fn update(
        &self,
        _syntax: &mut Syntax,
        _old: &Rope,
        _new: &Rope,
        _changes: &ChangeSet,
    ) -> Result<(), String> {
        unreachable!()
    }

    fn highlights(
        &self,
        _syntax: &Syntax,
        _rope: &Rope,
        _range: Range<usize>,
    ) -> Vec<(Range<usize>, usize)> {
        unreachable!()
    }

    fn outline(&self, _syntax: &Syntax, _rope: &Rope) -> Vec<OutlineSymbol> {
        unreachable!()
    }

    fn scopes(&self) -> Arc<Vec<String>> {
        Arc::new(Vec::new())
    }
}

#[test]
fn previews_use_the_supplied_language_catalog_and_report_its_missing_resources() {
    let provider = Arc::new(UnavailableSyntax {
        catalog: HelixSyntaxProvider::default(),
    });
    let mut buffer = TextBuffer::new_preview_with_capabilities(
        "print(1)\n",
        Path::new("notes.txt"),
        Arc::new(zess_platform::NativeFileSystem),
        provider,
    );
    assert_eq!(buffer.language_id(), Some("python"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while buffer.syntax_error().is_none() && std::time::Instant::now() < deadline {
        buffer.poll_syntax();
        std::thread::yield_now();
    }
    assert_eq!(
        buffer.syntax_error(),
        Some("syntax resources unavailable from provider")
    );
    assert_eq!(buffer.text(), "print(1)\n");
}

#[test]
fn file_names_use_the_complete_language_catalog() {
    for (path, language) in [
        ("example.py", "python"),
        ("example.ts", "typescript"),
        ("component.tsx", "tsx"),
        ("Cargo.toml", "toml"),
        ("flake.lock", "json"),
        ("Makefile", "make"),
        ("example.lua", "lua"),
        ("example.wisp", "wisp"),
    ] {
        let buffer = TextBuffer::new_with_path("", Path::new(path));
        assert_eq!(buffer.language_id(), Some(language), "{path}");
    }
}

#[test]
fn indentation_uses_the_detected_language_configuration() {
    for (path, indented) in [("example.py", "    x"), ("example.json", "  x")] {
        let mut buffer = TextBuffer::new_with_path("x", Path::new(path));
        buffer.execute_command(1, "text.indent").unwrap();
        assert_eq!(buffer.text(), indented, "{path}");
    }
}

#[test]
fn explicit_language_comments_use_the_catalog_rules() {
    let mut buffer = TextBuffer::new_with_path("print(1)", Path::new("notes.txt"));
    buffer.set_language(Some("lua")).unwrap();
    buffer
        .execute_command(1, "text.toggle-line-comments")
        .unwrap();
    assert_eq!(buffer.text(), "-- print(1)");
    buffer
        .execute_command(1, "text.toggle-line-comments")
        .unwrap();
    assert_eq!(buffer.text(), "print(1)");
}
