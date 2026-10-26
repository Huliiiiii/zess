use super::*;

#[test]
fn a_failed_grammar_does_not_skip_the_remaining_selected_grammars() {
    let selected = grammar_configs(
        Path::new("runtime"),
        &["rust".into(), "python".into(), "wisp".into()],
    )
    .unwrap();
    let attempted = Mutex::new(Vec::new());
    let error = run_grammars(selected, "build", 2, |grammar| {
        attempted.lock().unwrap().push(grammar.grammar_id.clone());
        if grammar.grammar_id == "rust" || grammar.grammar_id == "python" {
            anyhow::bail!("missing source");
        }
        Ok(())
    })
    .unwrap_err();
    let mut attempted = attempted.into_inner().unwrap();
    attempted.sort();
    assert_eq!(attempted, ["python", "rust", "wisp"]);
    assert_eq!(
        error.to_string(),
        "grammar build failed:\npython: missing source\nrust: missing source"
    );
}

#[test]
fn default_grammar_selection_preserves_the_upstream_exclusions() {
    let grammars = grammar_configs(Path::new("runtime"), &[]).unwrap();
    assert!(
        grammars
            .iter()
            .any(|grammar| grammar.grammar_id == "python")
    );
    assert!(grammars.iter().any(|grammar| grammar.grammar_id == "wisp"));
    for name in ["wren", "gemini"] {
        assert!(!grammars.iter().any(|grammar| grammar.grammar_id == name));
        let selected = grammar_configs(Path::new("runtime"), &[name.into()]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].grammar_id, name);
    }
}

#[test]
fn selected_grammars_keep_the_pinned_sources_and_subdirectories() {
    let grammars = grammar_configs(Path::new("runtime"), &["typescript".into()]).unwrap();
    assert_eq!(grammars.len(), 1);
    assert_eq!(grammars[0].grammar_id, "typescript");
    let GrammarSource::Git {
        remote,
        revision,
        subpath,
    } = &grammars[0].source
    else {
        panic!("TypeScript grammar must use its pinned Git source");
    };
    assert_eq!(
        remote,
        "https://github.com/tree-sitter/tree-sitter-typescript"
    );
    assert_eq!(revision, "75b3874edb2dc714fb1fd77a32013d0f8699989f");
    assert_eq!(subpath.as_deref(), Some("typescript"));
}
