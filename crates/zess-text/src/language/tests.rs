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
