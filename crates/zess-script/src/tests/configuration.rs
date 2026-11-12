use super::*;

#[test]
fn declaration_blocks_evaluate_values_and_preserve_each_update_rule() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.load_builtin_plugins().unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            r#"define mouse "not an option name"
define kinds : list "git" "git-commit-files"
configure-editor
  mouse #f
  scroll-lines : + 2 3
configure-editor
  preview-debounce-ms 0
configure-views kinds
  stats-alignment : string-append "con" "secutive"
  diff-file-header #t
configure-views 'git
  gutter #f
"#,
        ))
        .unwrap();
    let editor = runtime.editor_options();
    assert!(!editor.mouse);
    assert_eq!(editor.scroll_lines, 5);
    assert_eq!(editor.preview_debounce, Duration::ZERO);
    assert_eq!(
        runtime.view_options("git"),
        serde_json::json!({"gutter": false})
    );
    assert_eq!(
        runtime.view_options("git-commit-files"),
        serde_json::json!({"stats-alignment": "consecutive", "diff-file-header": true})
    );
}

#[test]
fn paired_labels_and_formats_accept_mappings_and_keep_the_last_key() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            r#"require "zess/status"
set-mode-labels! : obj 'insert "INSERT"
set-status-formats! : obj 'items "{selected}/{items}"
set-mode-labels! 'normal "FIRST" "normal" "NORMAL"
set-status-formats! 'pending "FIRST:{count}" "pending" "待处理 {count}"
"#,
        ))
        .unwrap();
    let mut context = zess_plugin::StatusContext {
        width: 80,
        mode: Some("normal".into()),
        active: true,
        plugin_status_data: serde_json::json!({"files": {"pending_count": 2}}),
        ..Default::default()
    };
    let line = runtime.format_statusline("default", &context).unwrap();
    assert_eq!(line.left[0].text, " NORMAL ");
    assert_eq!(line.right[0].text, "待处理 2 · 0/0");
    context.mode = Some("insert".into());
    assert_eq!(
        runtime.format_statusline("default", &context).unwrap().left[0].text,
        " INSERT "
    );
}

#[test]
fn invalid_paired_updates_leave_labels_and_formats_unchanged() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            "require \"zess/status\"\nset-mode-labels! 'normal \"NORMAL\"\nset-status-formats! 'pending \"P:{count}\"\n",
        ))
        .unwrap();
    for update in [
        "set-mode-labels! 'normal \"changed\" 'insert 42",
        "set-mode-labels! 'normal \"changed\" 'insert",
        "set-status-formats! 'pending \"changed\" 'items \"{unknown}\"",
        "set-status-formats! 'pending \"changed\" 'items",
    ] {
        assert!(
            runtime.load(&directory.write("invalid.w", update)).is_err(),
            "{update}"
        );
        let line = runtime
            .format_statusline(
                "default",
                &zess_plugin::StatusContext {
                    active: true,
                    width: 80,
                    mode: Some("normal".into()),
                    plugin_status_data: serde_json::json!({"files": {"pending_count": 2}}),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(line.left[0].text, " NORMAL ");
        assert_eq!(line.right[0].text, "P:2 · 0 selected / 0 items");
    }
}
