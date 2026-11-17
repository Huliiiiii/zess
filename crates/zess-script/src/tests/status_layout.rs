use super::*;
use zess_plugin::{StatusContent, StatusContext};

#[test]
fn mode_labels_fit_whole_graphemes_and_reserve_the_same_columns_when_inactive() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            "require \"zess/status\"\nset-mode-labels! 'normal \"👩‍💻界\"\n",
        ))
        .unwrap();
    let config = runtime.binding_catalog().status;
    for (width, active, expected) in [
        (6, true, " 👩‍💻界 "),
        (6, false, "      "),
        (4, true, "👩‍💻界"),
        (4, false, "    "),
        (3, true, "👩‍💻"),
        (3, false, "  "),
        (1, true, ""),
    ] {
        let context = StatusContext {
            width,
            active,
            ..Default::default()
        };
        let local = config.format_local("default", &context).unwrap();
        let script = runtime.format_statusline("default", &context).unwrap();
        assert_eq!(local.left[0].text, expected);
        assert_eq!(script.left[0].text, expected);
    }
}

#[test]
fn a_grapheme_split_across_status_spans_leaves_room_for_the_right_region() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            r#"require "zess/status"
register-statusline "custom"
  lambda : context
    statusline (list (status-span "👩‍" "left") (status-span "💻" "right")) "" "OK"
"#,
        ))
        .unwrap();
    let line = runtime
        .format_statusline("custom", &StatusContext::default())
        .unwrap();
    let row = line.render(4, 0, true);
    assert_eq!(
        row.iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "👩‍💻OK"
    );
}

#[test]
fn binding_catalog_exports_status_configuration_for_client_rendering() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            r#"require "zess/status"
set-mode-labels! : obj 'normal "N"
set-status-formats! : obj 'pending "Waiting {count}"
set-view-options! 'preview : obj 'statusline "preview" 'gutter #f
statusline-layout files
  left mode " " title
  right
    join " · " pending (join "/" items custom)
register-statusline-component 'custom
  lambda : context
    . "custom"
"#,
        ))
        .unwrap();
    let catalog = runtime.binding_catalog();
    let status = serde_json::to_value(catalog.status).unwrap();
    assert_eq!(status["mode_labels"]["normal"], "N");
    assert_eq!(status["status_formats"]["pending"], "Waiting {count}");
    assert_eq!(status["view_options"]["preview"]["gutter"], false);
    assert_eq!(
        status["layouts"]["files"]["left"],
        serde_json::json!([
            {"Component": "mode"}, {"Literal": " "}, {"Component": "title"}
        ])
    );
    assert_eq!(
        status["layouts"]["files"]["right"],
        serde_json::json!([
            {"Join": {"separator": " · ", "elements": [
                {"Component": "pending"},
                {"Join": {"separator": "/", "elements": [{"Component": "items"}, {"Component": "custom"}]}}
            ]}}
        ])
    );
    assert!(
        status["builtin_statuslines"]
            .as_array()
            .unwrap()
            .contains(&Value::String("default".into()))
    );
    assert!(
        status["builtin_components"]
            .as_array()
            .unwrap()
            .contains(&Value::String("title".into()))
    );
    assert!(
        !status["builtin_components"]
            .as_array()
            .unwrap()
            .contains(&Value::String("custom".into()))
    );
    let revision = runtime.binding_revision();
    runtime
        .engine
        .run("(set-mode-labels! (hash 'normal \"NORMAL\"))")
        .unwrap();
    assert!(runtime.binding_revision() > revision);
    assert_eq!(
        runtime.binding_catalog().status.mode_labels["normal"],
        "NORMAL"
    );
}

#[test]
fn plugins_can_register_components_and_declare_layouts_from_their_own_module() {
    let directory = SourceDirectory::new();
    directory.write(
        "plugin.w",
        r#"provide install
require "zess/status"
define : install
  statusline-layout plugin-buffer
    center plugin-part
  register-statusline-component 'plugin-part
    lambda : context
      status-span (get context 'title "") "plugin.role"
"#,
    );
    let config = directory.write("config.w", "require \"plugin\"\ninstall\n");
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.preload_modules(&directory.0).unwrap();
    runtime.load(&config).unwrap();
    runtime.validate_statusline_layouts().unwrap();
    runtime.engine.run("(#%gc-collect)").unwrap();
    let line = runtime
        .format_statusline(
            "default",
            &StatusContext {
                kind: "plugin-buffer".into(),
                title: "dynamic title".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(line.center[0].text, "dynamic title");
    assert_eq!(line.center[0].style, "plugin.role");
}

#[test]
fn source_positions_use_the_old_line_when_the_new_line_is_absent() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.load_builtin_plugins().unwrap();
    runtime
        .load(&directory.write(
            "left.w",
            r#"require "zess/status"
register-statusline-component 'custom
  lambda : context
    . "Configured left"
statusline-layout diff
  left custom
"#,
        ))
        .unwrap();
    let context = StatusContext {
        kind: "diff".into(),
        content: StatusContent::Diff {
            old_line: Some(42),
            new_line: None,
            selection_count: 1,
        },
        ..Default::default()
    };
    let line = runtime.format_statusline("diff", &context).unwrap();
    assert_eq!(line.left[0].text, "Configured left");
    assert_eq!(
        line.right
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "42"
    );

    runtime
        .load(&directory.write(
            "right.w",
            "require \"zess/status\"\nstatusline-layout diff\n  right source-position\n",
        ))
        .unwrap();
    let line = runtime.format_statusline("default", &context).unwrap();
    assert_eq!(line.right[0].text, "42");
}

#[test]
fn layouts_keep_order_styles_and_only_join_nonempty_components() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            r#"require "zess/status"
statusline-layout files
  right
    join " · " pending items custom
  left mode " " title modified loading
register-statusline-component 'custom
  lambda : context
    status-span (get context 'title "") "plugin.custom"
"#,
        ))
        .unwrap();
    runtime.validate_statusline_layouts().unwrap();
    let mut context = StatusContext {
        kind: "files".into(),
        active: true,
        width: 80,
        mode: Some("normal".into()),
        title: "Files".into(),
        content: StatusContent::Items {
            selection_count: 1,
            primary: None,
            item_count: 4,
        },
        plugin_status_data: serde_json::json!({"files": {"pending_count": 2}}),
        ..Default::default()
    };
    let line = runtime.format_statusline("default", &context).unwrap();
    assert!(line.center.iter().all(|span| span.text.is_empty()));
    assert_eq!(
        line.left
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        " NOR  Files"
    );
    assert_eq!(
        line.right
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "2 pending · 1 selected / 4 items · Files"
    );
    assert_eq!(line.right.last().unwrap().style, "plugin.custom");
    assert_eq!(line.left[0].style, "status.normal");
    context.title.clear();
    context.plugin_status_data = serde_json::json!({"files": {"pending_count": 0}});
    let line = runtime.format_statusline("default", &context).unwrap();
    assert_eq!(
        line.right
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "1 selected / 4 items"
    );
}

#[test]
fn nested_joins_preserve_spans_and_skip_empty_groups() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            r#"require "zess/status"
statusline-layout files
  right
    join "/" "a" (join ":" "" git) "" "b"
"#,
        ))
        .unwrap();
    let mut context = StatusContext {
        kind: "files".into(),
        ..Default::default()
    };
    let line = runtime.format_statusline("default", &context).unwrap();
    assert_eq!(
        line.right
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "a/b"
    );
    context.plugin_status =
        serde_json::json!({"git": {"state": "ready", "additions": 3, "deletions": 2}});
    let line = runtime.format_statusline("default", &context).unwrap();
    assert_eq!(
        line.right
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "a/+3  -2/b"
    );
    assert!(
        line.right
            .iter()
            .any(|span| span.text == "+3" && span.style == "git.added")
    );
    assert!(
        line.right
            .iter()
            .any(|span| span.text == "-2" && span.style == "git.deleted")
    );
}

#[test]
fn layout_syntax_errors_do_not_replace_the_previous_layout() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            "require \"zess/status\"\nstatusline-layout files\n  left \"previous\"\n",
        ))
        .unwrap();
    for layout in [
        "statusline-layout files\n  left \"changed\"\n  bottom title\n",
        "statusline-layout files\n  left title\n  left mode\n",
        "statusline-layout files\n  right\n    join 4 title\n",
        "statusline-layout files\n  right\n    style \"error\" title\n",
        "statusline-layout files\n  right 42\n",
    ] {
        assert!(
            runtime.load(&directory.write("invalid.w", layout)).is_err(),
            "{layout}"
        );
        let line = runtime
            .format_statusline(
                "default",
                &StatusContext {
                    kind: "files".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(line.left[0].text, "previous");
    }
}

#[test]
fn component_references_are_checked_after_registration_and_conflicts_are_rejected() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load(&directory.write(
            "layout.w",
            "require \"zess/status\"\nstatusline-layout plugin-buffer\n  center plugin-part\n",
        ))
        .unwrap();
    assert!(
        runtime
            .validate_statusline_layouts()
            .unwrap_err()
            .to_string()
            .contains("unknown statusline component plugin-part")
    );
    runtime
        .load(&directory.write(
            "component.w",
            "register-statusline-component 'plugin-part : lambda (context) \"ready\"\n",
        ))
        .unwrap();
    runtime.validate_statusline_layouts().unwrap();
    let error = runtime
        .load(&directory.write(
            "duplicate.w",
            "register-statusline-component \"plugin-part\" : lambda (context) \"replacement\"\n",
        ))
        .unwrap_err();
    assert!(format!("{error:#}").contains("already registered: plugin-part"));
    let line = runtime
        .format_statusline(
            "default",
            &StatusContext {
                kind: "plugin-buffer".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(line.center[0].text, "ready");
}

#[test]
fn only_builtin_formatters_use_the_layout_for_the_current_kind() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.load_builtin_plugins().unwrap();
    runtime
        .load(&directory.write(
            "config.w",
            r#"statusline-layout files
  left "files layout"
statusline-layout preview
  left "preview layout"
statusline-layout diff
  left "diff layout"
"#,
        ))
        .unwrap();
    for (formatter, kind, expected) in [
        ("default", "files", "files layout"),
        ("preview", "preview", "preview layout"),
        ("diff", "diff", "diff layout"),
    ] {
        let context = StatusContext {
            kind: kind.into(),
            ..Default::default()
        };
        assert_eq!(
            runtime.format_statusline(formatter, &context).unwrap().left[0].text,
            expected
        );
        runtime.load(&directory.write("custom.w", &format!("register-statusline \"{formatter}\" : lambda (context) : statusline \"custom\" \"\" \"\"\n"))).unwrap();
        assert_eq!(
            runtime.format_statusline(formatter, &context).unwrap().left[0].text,
            "custom"
        );
    }
}

#[test]
fn partial_updates_preserve_default_and_previously_configured_regions() {
    let directory = SourceDirectory::new();
    for formatter in ["default", "preview", "diff"] {
        let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
        runtime.load_builtin_plugins().unwrap();
        let context = StatusContext {
            kind: formatter.into(),
            title: "Title".into(),
            active: true,
            width: 80,
            mode: Some("normal".into()),
            plugin_status: serde_json::json!("plugin status"),
            ..Default::default()
        };
        let original = runtime.format_statusline(formatter, &context).unwrap();
        runtime
            .load(&directory.write(
                "first.w",
                &format!("statusline-layout {formatter}\n  right \"configured right\"\n"),
            ))
            .unwrap();
        let line = runtime.format_statusline(formatter, &context).unwrap();
        assert_eq!(
            line.left
                .iter()
                .map(|span| (&span.text, &span.style))
                .collect::<Vec<_>>(),
            original
                .left
                .iter()
                .map(|span| (&span.text, &span.style))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            line.center
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>(),
            "plugin status"
        );
        assert_eq!(line.right[0].text, "configured right");
        runtime
            .load(&directory.write(
                "second.w",
                &format!("statusline-layout {formatter}\n  center \"configured center\"\n"),
            ))
            .unwrap();
        let line = runtime.format_statusline(formatter, &context).unwrap();
        assert_eq!(line.right[0].text, "configured right");
        assert_eq!(line.center[0].text, "configured center");
        runtime
            .load(&directory.write(
                "empty.w",
                &format!("statusline-layout {formatter}\n  left\n"),
            ))
            .unwrap();
        let line = runtime.format_statusline(formatter, &context).unwrap();
        assert!(line.left.is_empty());
        assert_eq!(line.center[0].text, "configured center");
        assert_eq!(line.right[0].text, "configured right");
    }
}

#[test]
fn replacing_a_region_removes_its_old_component_references() {
    let directory = SourceDirectory::new();
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.load(&directory.write("config.w", "require \"zess/status\"\nstatusline-layout files\n  right missing-part\nstatusline-layout files\n  right \"replacement\"\n")).unwrap();
    runtime.validate_statusline_layouts().unwrap();
}
