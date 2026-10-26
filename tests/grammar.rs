use std::process::Command;

#[test]
fn invalid_grammar_invocations_fail_without_creating_the_runtime_directory() {
    let fixture = tempfile::tempdir().unwrap();
    let runtime = fixture.path().join("syntax");
    for arguments in [
        vec!["--grammar"],
        vec!["--grammar", "download"],
        vec!["--grammar", "build", "note.rs"],
        vec!["--grammar", "build", "--workspace", "."],
        vec!["--grammar", "build", "--socket", "server.sock"],
        vec!["--grammar", "build", "--config", "init.w"],
        vec!["attach", "--grammar", "build"],
        vec!["server", "--grammar", "build"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_zess"))
            .args(&arguments)
            .current_dir(fixture.path())
            .env("ZESS_RUNTIME", &runtime)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        assert!(!runtime.exists(), "{arguments:?}");
    }
}

#[test]
fn grammar_build_reports_missing_sources_without_starting_a_session() {
    let fixture = tempfile::tempdir().unwrap();
    let runtime = fixture.path().join("syntax");
    let output = Command::new(env!("CARGO_BIN_EXE_zess"))
        .args(["--grammar", "build", "rust"])
        .current_dir(fixture.path())
        .env("ZESS_RUNTIME", &runtime)
        .env("XDG_RUNTIME_DIR", fixture.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "Error: grammar build failed:".to_owned(),
            format!(
                "rust: grammar source is missing at {}; run zess --grammar fetch rust",
                runtime.join("grammars/sources/rust/src").display()
            ),
        ]
    );
    assert!(!runtime.join("grammars/sources/rust").exists());
    assert!(!fixture.path().join("zess/run").exists());
}
