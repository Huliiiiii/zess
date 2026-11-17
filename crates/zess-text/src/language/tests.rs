use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::{
    TextBuffer,
    tests::{rendered_text, styled_text, wait_for_highlight_refresh},
};

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

struct ControlledSyntax {
    catalog: HelixSyntaxProvider,
    caller: thread::ThreadId,
    started: mpsc::Sender<String>,
    resume: Mutex<Receiver<()>>,
}

impl ControlledSyntax {
    fn pause(&self, rope: &Rope) -> Result<(), String> {
        assert_ne!(thread::current().id(), self.caller);
        self.started
            .send(rope.to_string())
            .map_err(|error| error.to_string())?;
        self.resume
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())
    }
}

impl SyntaxProvider for ControlledSyntax {
    fn language_for_path(&self, path: &Path) -> Option<Language> {
        self.catalog.language_for_path(path)
    }

    fn language_for_name(&self, name: &str) -> Option<Language> {
        self.catalog.language_for_name(name)
    }

    fn config(&self, language: Language) -> &helix_core::syntax::config::LanguageConfiguration {
        self.catalog.config(language)
    }

    fn has_outline(&self, language: Language) -> bool {
        self.catalog.has_outline(language)
    }

    fn parse(&self, rope: &Rope, language: Language) -> Result<Syntax, String> {
        self.pause(rope)?;
        self.catalog.parse(rope, language)
    }

    fn update(
        &self,
        syntax: &mut Syntax,
        old: &Rope,
        new: &Rope,
        changes: &ChangeSet,
    ) -> Result<(), String> {
        self.pause(new)?;
        self.catalog.update(syntax, old, new, changes)
    }

    fn highlights(
        &self,
        syntax: &Syntax,
        rope: &Rope,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, usize)> {
        self.catalog.highlights(syntax, rope, range)
    }

    fn outline(&self, syntax: &Syntax, rope: &Rope) -> Vec<OutlineSymbol> {
        self.catalog.outline(syntax, rope)
    }

    fn scopes(&self) -> Arc<Vec<String>> {
        self.catalog.scopes()
    }
}

#[test]
fn rendering_and_editing_continue_while_syntax_catches_up() {
    let (started, parsing) = mpsc::channel();
    let (resume, released) = mpsc::channel();
    let provider = Arc::new(ControlledSyntax {
        catalog: HelixSyntaxProvider::default(),
        caller: thread::current().id(),
        started,
        resume: Mutex::new(released),
    });
    let mut buffer = TextBuffer::new_with_path_and_capabilities(
        "\nfn example() {}",
        Path::new("example.rs"),
        Arc::new(zess_platform::NativeFileSystem),
        provider,
    );
    let wait_for_request = |buffer: &mut TextBuffer| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                !buffer.poll_syntax(),
                "an outdated result requested a redraw"
            );
            if let Ok(text) = parsing.try_recv() {
                break text;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    };

    let frame = buffer.render(1, 40, 2, false);
    assert_eq!(rendered_text(&frame), "\nfn example() {}");
    assert_eq!(styled_text(&frame, "keyword.function"), "");
    assert_eq!(wait_for_request(&mut buffer), "\nfn example() {}");
    resume.send(()).unwrap();
    wait_for_highlight_refresh(&mut buffer);
    let frame = buffer.render(1, 40, 2, false);
    assert_eq!(styled_text(&frame, "keyword.function"), "fn");

    buffer.execute_command(1, "text.goto-file-end").unwrap();
    buffer.execute_command(1, "text.append-line-end").unwrap();
    buffer.paste_text(1, " // i").unwrap();
    assert_eq!(wait_for_request(&mut buffer), "\nfn example() {} // i");
    buffer.input_text(1, "n").unwrap();
    buffer.input_text(1, "k").unwrap();
    buffer.execute_command(1, "text.escape").unwrap();
    buffer.execute_command(1, "text.goto-file-start").unwrap();
    resume.send(()).unwrap();

    assert_eq!(wait_for_request(&mut buffer), "\nfn example() {} // ink");
    let frame = buffer.render(1, 40, 2, false);
    assert_eq!(rendered_text(&frame), "\nfn example() {} // ink");
    assert_eq!(styled_text(&frame, "comment.line"), "");

    resume.send(()).unwrap();
    wait_for_highlight_refresh(&mut buffer);
    let frame = buffer.render(1, 40, 2, false);
    assert_eq!(styled_text(&frame, "comment.line"), "// ink");
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
