use super::*;
use etcetera::base_strategy::{BaseStrategy, choose_base_strategy};
use std::env;

#[test]
fn a_failed_grammar_does_not_skip_the_remaining_selected_grammars() {
    let selected = grammar_configs(
        Path::new("runtime"),
        &["rust".into(), "python".into(), "wisp".into()],
    )
    .unwrap();
    let attempted = Mutex::new(Vec::new());
    let error = run_grammars(selected, "build", 2, &|_, _| {}, |grammar| {
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

#[test]
fn building_the_bundled_grammar_installs_inherited_language_queries() {
    if env::var_os("ZESS_TEST_QUERY_INSTALLATION").is_some() {
        let directory = build_grammars(None, &["wisp".into()]).unwrap();
        assert!(
            helix_loader::grammar::get_language("wisp")
                .unwrap()
                .is_some()
        );
        let query = helix_core::syntax::read_query("typescript", "highlights.scm");
        assert!(query.contains("@keyword.function"));
        assert!(query.contains("type_identifier"));
        assert!(directory.join("queries/python/highlights.scm").is_file());
        let mut buffer =
            crate::TextBuffer::new_with_path("\ndefine : example\n  #t\n", Path::new("example.w"));
        let frame = buffer.render(1, 40, 4, false);
        assert_eq!(
            frame
                .rows
                .iter()
                .flat_map(|row| &row.spans)
                .filter(|span| span.style == "keyword")
                .map(|span| span.text.as_str())
                .collect::<String>(),
            "define"
        );
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime::tests::building_the_bundled_grammar_installs_inherited_language_queries",
            "--nocapture",
        ])
        .env("ZESS_TEST_QUERY_INSTALLATION", "1")
        .env("ZESS_RUNTIME", directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn runtime_lookup_survives_replacement_of_the_running_executable() {
    if env::var_os("ZESS_TEST_DELETED_EXECUTABLE").is_some() {
        let executable = env::current_exe().unwrap();
        let runtime = executable.parent().unwrap().join("runtime");
        std::fs::remove_file(&executable).unwrap();
        assert_eq!(default_directory().unwrap(), runtime);
        std::fs::remove_dir(&runtime).unwrap();
        let strategy = choose_base_strategy().unwrap();
        assert_eq!(
            default_directory().unwrap(),
            strategy.cache_dir().join("zess/runtime")
        );
        return;
    }

    let test_binary = env::current_exe().unwrap();
    let directory = tempfile::tempdir_in(test_binary.parent().unwrap()).unwrap();
    let executable = directory.path().join("runtime-test");
    // Copying would open a writer that a parallel test's child could inherit before exec.
    std::fs::hard_link(test_binary, &executable).unwrap();
    std::fs::create_dir(directory.path().join("runtime")).unwrap();
    let output = std::process::Command::new(executable)
        .args([
            "--exact",
            "runtime::tests::runtime_lookup_survives_replacement_of_the_running_executable",
            "--nocapture",
        ])
        .env("ZESS_TEST_DELETED_EXECUTABLE", "1")
        .env_remove("ZESS_RUNTIME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
