use std::path::Path;

use crate::TextBuffer;

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
