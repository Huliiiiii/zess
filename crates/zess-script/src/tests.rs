use steel::steel_vm::register_fn::RegisterFn;

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use steel::{rvals::Custom, steel_vm::builtin::BuiltInModule};
use zess_plugin::ItemSelection;

fn input(view_id: ViewId, event: &str) -> PluginInput {
    PluginInput {
        buffer_id: 1,
        view_id,
        generation: 1,
        preview_debounce: zess_plugin::DEFAULT_PREVIEW_DEBOUNCE,
        preview_area: None,
        event: event.into(),
        argument: Value::Null,
        query: None,
        options: Value::Null,
        mode: "normal".into(),
        selection: ItemSelection::default(),
        register: None,
        context: zess_plugin::PluginContext::default(),
    }
}

#[test]
fn host_json_arrays_reach_scripts_as_lists() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"(register-event "arrays"
                (lambda (argument)
                    (and (list? argument) (list? (car argument))
                         (equal? (car argument) '(1 2)))))"#,
            Path::new("arrays.scm"),
        )
        .unwrap();
    assert_eq!(
        runtime
            .event("arrays", &serde_json::json!([[1, 2], []]))
            .unwrap(),
        serde_json::json!(true)
    );
}

#[test]
fn default_statusline_preserves_plugin_status_spans() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    let status = serde_json::json!(["prefix", {"text": "changed", "style": "git.added"}]);
    let output = runtime
        .format_statusline("default", &serde_json::json!({"plugin_status": status}))
        .unwrap();
    assert_eq!(output["center"], status);
}

#[test]
fn deferred_yanks_keep_the_register_captured_by_the_script() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
require "zess/buffer"
require "zess/ui"
register-buffer "deferred-yank"
  lambda : input
    if (equal? (input-event input) "yank")
      buffer-result null (or (input-register input) #\") null
        buffer-output #f #f '()
      buffer-result null #f null
        buffer-output #f #f
          list : request-yank (list "saved") : buffer-input-view input
"#,
            Path::new("deferred-yank.w"),
        )
        .unwrap();
    for register in [Some('+'), Some('*'), Some('a'), None] {
        let mut launch = input(1, "yank");
        launch.register = register;
        assert!(
            runtime
                .dispatch("deferred-yank", &launch)
                .unwrap()
                .requests
                .is_empty()
        );
        let mut tick = input(1, "tick");
        tick.register = Some('z');
        let output = runtime.dispatch("deferred-yank", &tick).unwrap();
        assert!(matches!(
            output.requests.as_slice(),
            [HostRequest::Yank { values, register: Some(destination) }]
                if values == &["saved"] && *destination == register.unwrap_or('"')
        ));
    }
}

#[test]
fn yank_requests_accept_an_optional_character_register() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
require "zess/ui"
register-command "default-yank" "Yank to the current register"
  lambda : argument
    request-yank : list "saved"
register-command "named-yank" "Yank to a named register"
  lambda : argument
    request-yank (list "saved") #\a
register-command "invalid-yank" "Reject a string register"
  lambda : argument
    request-yank (list "saved") "a"
"#,
            Path::new("register-yank.w"),
        )
        .unwrap();
    for (command, destination) in [("default-yank", None), ("named-yank", Some('a'))] {
        assert!(matches!(
            runtime.command(command, &Value::Null).unwrap(),
            CommandOutput::Request(HostRequest::Yank { values, register })
                if values == ["saved"] && register == destination
        ));
    }
    let Err(error) = runtime.command("invalid-yank", &Value::Null) else {
        panic!("string register was accepted");
    };
    assert!(
        error
            .root_cause()
            .to_string()
            .contains("Expected character"),
        "{error:#}"
    );
}

#[test]
fn minibuffer_requests_accept_an_optional_key_scope() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
require "zess/ui"
register-command "plain-input" "Ask for input"
  lambda : argument
    request-mini-buffer "answer" "initial"
register-command "scoped-input" "Ask for scoped input"
  lambda : argument
    request-mini-buffer "answer" "initial" "custom.input"
register-command "extra-input-argument" "Reject an extra argument"
  lambda : argument
    request-mini-buffer "answer" "initial" "custom.input" "extra"
"#,
            Path::new("input.w"),
        )
        .unwrap();
    for (command, expected_scope) in [
        ("plain-input", None),
        ("scoped-input", Some("custom.input")),
    ] {
        let CommandOutput::Request(HostRequest::MiniBuffer {
            purpose,
            text,
            key_scope,
        }) = runtime.command(command, &Value::Null).unwrap()
        else {
            panic!("script did not return a minibuffer request");
        };
        assert_eq!(purpose, "answer");
        assert_eq!(text, "initial");
        assert_eq!(key_scope.as_deref(), expected_scope);
    }
    assert!(
        runtime
            .command("extra-input-argument", &Value::Null)
            .is_err()
    );
}

#[test]
fn scripts_can_declare_typed_responsive_preview_groups() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
require "zess/buffer"
require "zess/ui"
register-command "pair" "Show paired previews"
  lambda : argument
    request-preview-group
      list
        preview-pane-header
          preview-pane "before" "note.txt" : preview-text "old"
          . "Before"
        preview-pane-loading
          preview-pane-scope
            preview-pane "after" "note.txt" : preview-text "new"
            . "custom-preview"
          . #t
      preview-layout-responsive 60
      . #f
"#,
            Path::new("pair.w"),
        )
        .unwrap();
    let CommandOutput::Request(HostRequest::PreviewGroup {
        panes,
        layout,
        focus,
    }) = runtime.command("pair", &Value::Null).unwrap()
    else {
        panic!("script did not return a preview group")
    };
    assert!(matches!(
        layout,
        zess_plugin::PreviewLayout::Responsive { min_pane_width: 60 }
    ));
    assert!(!focus);
    assert_eq!(panes.len(), 2);
    let zess_plugin::PreviewPane::View {
        id,
        header,
        content,
        ..
    } = &panes[0]
    else {
        panic!("expected an interactive pane")
    };
    assert_eq!(id, "before");
    assert_eq!(header, "Before");
    assert!(matches!(content, zess_plugin::PreviewContent::Text { text } if text == "old"));
    let zess_plugin::PreviewPane::View {
        id,
        key_scope,
        loading,
        content,
        ..
    } = &panes[1]
    else {
        panic!("expected an interactive pane")
    };
    assert_eq!(id, "after");
    assert_eq!(key_scope.as_deref(), Some("custom-preview"));
    assert!(*loading);
    assert!(matches!(content, zess_plugin::PreviewContent::Text { text } if text == "new"));
}

#[test]
fn script_structs_and_closures_survive_between_buffer_updates() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
require "zess/buffer"
struct counter (next) #:transparent
struct location (count) #:transparent
register-buffer "counter"
  lambda : input
    let*
      : old : buffer-input-state input
        local : buffer-input-view input
        count : if (counter? old) ((counter-next old)) 1
        local-count : if (location? local) (+ (location-count local) 1) 1
      buffer-result
        counter : lambda () : + count 1
        location local-count
        . null
        buffer-output
          buffer-display "Counter" ""
            list : display-row "value" (to-string (list count local-count)) "" 0
            . #f ""
          . #f '()
"#,
            Path::new("counter.w"),
        )
        .unwrap();
    let output = runtime.dispatch("counter", &input(10, "init")).unwrap();
    assert_eq!(output.display.unwrap().rows[0].text, "(1 1)");
    runtime.engine.run("(#%gc-collect)").unwrap();
    let output = runtime.dispatch("counter", &input(20, "init")).unwrap();
    assert_eq!(output.display.unwrap().rows[0].text, "(2 1)");
    let output = runtime.dispatch("counter", &input(10, "render")).unwrap();
    assert_eq!(output.display.unwrap().rows[0].text, "(3 2)");
}

#[derive(Clone)]
struct Counter(Arc<AtomicUsize>);
impl Custom for Counter {}

#[test]
fn native_objects_pass_through_script_apis_without_serialization() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    let mut module = BuiltInModule::new("example/counter");
    module.register_fn("increment!", |counter: &Counter| {
        counter.0.fetch_add(1, Ordering::Relaxed);
        counter.clone()
    });
    runtime.register_native_module(module);
    runtime
        .load_source(
            "require-builtin example/counter\nregister-api \"counter.increment\" increment!\n",
            Path::new("counter.w"),
        )
        .unwrap();
    let counter = Counter(Arc::new(AtomicUsize::new(0)));
    let result: Counter = runtime.api("counter.increment", counter.clone()).unwrap();
    assert!(Arc::ptr_eq(&counter.0, &result.0));
    assert_eq!(result.0.load(Ordering::Relaxed), 1);
}

#[test]
fn invalid_buffer_output_does_not_replace_the_previous_script_state() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
require "zess/buffer"
struct counter (value) #:transparent
register-buffer "counter"
  lambda : input
    let*
      : old : buffer-input-state input
        value : if (counter? old) (+ (counter-value old) 1) 1
      buffer-result (counter value) null null
        if (equal? (input-event input) "invalid")
          . "wrong output type"
          buffer-output
            buffer-display "Counter" "" (list (display-row "value" (to-string value) "" 0)) #f ""
            . #f '()
"#,
            Path::new("counter.w"),
        )
        .unwrap();
    runtime.dispatch("counter", &input(10, "init")).unwrap();
    assert!(runtime.dispatch("counter", &input(10, "invalid")).is_err());
    let output = runtime.dispatch("counter", &input(10, "render")).unwrap();
    assert_eq!(output.display.unwrap().rows[0].text, "2");
}

#[test]
fn view_options_replace_the_previous_configuration() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            "set-view-options! 'example : obj 'statusline \"old\" 'gutter #f\n\
             set-view-options! \"example\" : obj 'statusline \"new\"\n",
            Path::new("options.w"),
        )
        .unwrap();
    assert_eq!(
        runtime.view_options("example"),
        serde_json::json!({ "statusline": "new" })
    );
}

#[test]
fn view_options_apply_to_every_kind_only_when_all_names_are_valid() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            "set-view-options! '(first \"second\") : obj 'statusline \"shared\"\n\
             set-view-options! (array 'third \"fourth\") : obj 'statusline \"shared\"\n",
            Path::new("options.w"),
        )
        .unwrap();
    for kind in ["first", "second", "third", "fourth"] {
        assert_eq!(
            runtime.view_options(kind),
            serde_json::json!({ "statusline": "shared" })
        );
    }
    assert!(
        runtime
            .load_source(
                "set-view-options! '(first 42) : obj 'statusline \"changed\"\n",
                Path::new("invalid.w"),
            )
            .is_err()
    );
    assert_eq!(
        runtime.view_options("first"),
        serde_json::json!({ "statusline": "shared" })
    );
}

#[test]
fn editor_options_validate_types_before_replacing_settings() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    assert_eq!(
        runtime.editor_options(),
        EditorOptions {
            mouse: true,
            scroll_lines: 3,
            preview_debounce: Duration::from_millis(150),
        }
    );
    runtime
        .load_source(
            "set-editor-options! : obj 'mouse #f 'scroll-lines 5 'preview-debounce-ms 0\n",
            Path::new("options.w"),
        )
        .unwrap();
    assert_eq!(
        runtime.editor_options(),
        EditorOptions {
            mouse: false,
            scroll_lines: 5,
            preview_debounce: Duration::ZERO,
        }
    );
    runtime
        .load_source(
            "set-editor-options! : obj 'preview-debounce-ms 275\n",
            Path::new("options.w"),
        )
        .unwrap();
    assert_eq!(
        runtime.editor_options().preview_debounce,
        Duration::from_millis(275)
    );
    for (source, error) in [
        (
            "set-editor-options! : obj 'mouse 1\n",
            "mouse must be a boolean",
        ),
        (
            "set-editor-options! : obj 'scroll-lines 0\n",
            "scroll-lines must be a positive integer no greater than isize::MAX",
        ),
        (
            "set-editor-options! : obj 'scroll-lines -1\n",
            "scroll-lines must be a positive integer no greater than isize::MAX",
        ),
        (
            "set-editor-options! : obj 'scroll-lines 1.5\n",
            "scroll-lines must be a positive integer no greater than isize::MAX",
        ),
        (
            "set-editor-options! : obj 'preview-debounce-ms -1\n",
            "preview-debounce-ms must be a non-negative integer",
        ),
        (
            "set-editor-options! : obj 'preview-debounce-ms 1.5\n",
            "preview-debounce-ms must be a non-negative integer",
        ),
    ] {
        assert_eq!(
            runtime
                .load_source(source, Path::new("invalid.w"))
                .unwrap_err()
                .root_cause()
                .to_string(),
            format!("Error: TypeMismatch: {error}")
        );
        assert_eq!(
            runtime.editor_options(),
            EditorOptions {
                mouse: false,
                scroll_lines: 5,
                preview_debounce: Duration::from_millis(275),
            }
        );
    }
}

#[test]
fn commands_can_register_commands_while_they_run() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"(register-command "install" "Install a command"
            (lambda (argument)
                (register-command "installed" "Installed command" (lambda (value) value))
                argument))"#,
            Path::new("commands.scm"),
        )
        .unwrap();
    assert!(
        matches!(runtime.command("install", &Value::String("first".into())).unwrap(), CommandOutput::Value(Value::String(value)) if value == "first")
    );
    assert!(
        matches!(runtime.command("installed", &Value::String("second".into())).unwrap(), CommandOutput::Value(Value::String(value)) if value == "second")
    );
    runtime.with_commands(|commands| {
        assert_eq!(
            commands.resolve("installed").unwrap().description,
            "Installed command"
        )
    });
}

#[test]
fn command_implementations_preserve_their_argument_sources() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.register_native_command("action", "Native action");
    PluginRegistrar::command(
        &mut runtime,
        "request",
        "Fixed request",
        HostRequest::Action {
            name: "fixed".into(),
            argument: Value::from(1),
        },
    );
    runtime.buffer_command("event", "Buffer event", "browser", "open");

    assert!(
        matches!(runtime.command("action", &Value::from(2)).unwrap(), CommandOutput::Request(HostRequest::Action { name, argument }) if name == "action" && argument == Value::from(2))
    );
    assert!(
        matches!(runtime.command("request", &Value::from(2)).unwrap(), CommandOutput::Request(HostRequest::Action { name, argument }) if name == "fixed" && argument == Value::from(1))
    );
    assert!(
        matches!(runtime.command("event", &Value::from(2)).unwrap(), CommandOutput::Request(HostRequest::BufferEvent { kind, event, argument }) if kind == "browser" && event == "open" && argument == Value::from(2))
    );
}

#[test]
fn detaching_a_view_from_one_buffer_preserves_its_state_in_other_buffers() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"require "zess/buffer"
register-buffer "counter"
  lambda : input
    let*
      : old : buffer-input-view input
        count : if (number? old) (+ old 1) 1
      buffer-result null count null
        buffer-output
          buffer-display "Counter" "" (list (display-row "value" (to-string count) "" 0)) #f ""
          . #f '()
"#,
            Path::new("counter.w"),
        )
        .unwrap();
    let first = input(10, "init");
    let second = PluginInput {
        buffer_id: 2,
        ..input(10, "init")
    };
    assert_eq!(
        runtime
            .dispatch("counter", &first)
            .unwrap()
            .display
            .unwrap()
            .rows[0]
            .text,
        "1"
    );
    assert_eq!(
        runtime
            .dispatch("counter", &second)
            .unwrap()
            .display
            .unwrap()
            .rows[0]
            .text,
        "1"
    );
    runtime.remove_buffer_view(1, 10);
    assert_eq!(
        runtime
            .dispatch("counter", &first)
            .unwrap()
            .display
            .unwrap()
            .rows[0]
            .text,
        "1"
    );
    assert_eq!(
        runtime
            .dispatch("counter", &second)
            .unwrap()
            .display
            .unwrap()
            .rows[0]
            .text,
        "2"
    );
}

#[test]
fn passive_preview_surfaces_reject_view_key_scopes() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
require "zess/buffer"
require "zess/ui"
register-command "invalid-surface" "Assign a view key scope to a surface"
  lambda : argument
    request-preview-group
      list
        preview-pane-scope
          preview-surface "main" "note.txt" : preview-text "content"
          . "custom-preview"
      preview-layout-rows
      . #f
"#,
            Path::new("surface.w"),
        )
        .unwrap();
    let error = runtime
        .command("invalid-surface", &Value::Null)
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("only preview views have a key scope"),
        "{error}"
    );
}

#[test]
fn interpreter_replacement_preserves_key_context_identities() {
    let mut previous = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    previous
        .load_source(
            r#"
(register-mode "browser" "select")
(map-key "browser" "select" "j" "old")
"#,
            Path::new("previous.scm"),
        )
        .unwrap();
    let identities = previous.identities();
    let kind = identities.buffer_kind("browser").unwrap();
    let scope = identities.scope("browser").unwrap();
    let mode = identities.mode("select").unwrap();
    drop(previous);

    let mut reloaded = ScriptRuntime::with_identities(|_, _| Value::Null, identities).unwrap();
    reloaded
        .load_source(
            r#"
(register-mode "other" "normal")
(register-mode "browser" "select")
(map-key "browser" "select" "j" "new")
"#,
            Path::new("reloaded.scm"),
        )
        .unwrap();
    assert!(reloaded.modes()[1].kind == kind);
    assert!(reloaded.modes()[1].name == mode);
    assert!(
        matches!(reloaded.resolve_keys_ids(&[scope], mode, &["j".into()]), KeyResolution::Command(command) if command == "new")
    );
}

#[test]
fn internal_actions_execute_without_a_command_line_entry() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"
register-action "internal.move" "Move selection"
  lambda : argument
    string-append "" "moved"
register-command-alias "step" "internal.move"
register-command "public-move" "Move through the command line"
  lambda : argument
    string-append "" "moved publicly"
"#,
            Path::new("actions.w"),
        )
        .unwrap();
    for name in ["internal.move", "step"] {
        assert!(runtime.resolve_command_line(name).is_err());
        assert!(
            matches!(runtime.command(name, &Value::Null).unwrap(), CommandOutput::Value(value) if value == "moved")
        );
    }
    assert_eq!(
        runtime.resolve_command_line("public-move").unwrap(),
        "public-move"
    );
    runtime
        .load_source(
            r#"register-command "internal.move" "Publish movement"
  lambda : argument
    string-append "" "published"
"#,
            Path::new("publish.w"),
        )
        .unwrap();
    assert_eq!(
        runtime.resolve_command_line("step").unwrap(),
        "internal.move"
    );
    runtime
        .load_source(
            r#"register-action "internal.move" "Internal movement"
  lambda : argument
    string-append "" "private"
"#,
            Path::new("internal.w"),
        )
        .unwrap();
    assert!(runtime.resolve_command_line("step").is_err());
    assert!(
        matches!(runtime.command("step", &Value::Null).unwrap(), CommandOutput::Value(value) if value == "private")
    );
}

struct SourceDirectory(std::path::PathBuf);

impl SourceDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zess-script-sources-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for SourceDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn disk_configuration_resolves_imports_relative_to_its_own_directory() {
    let directory = SourceDirectory::new();
    directory.write("helper.scm", "(provide answer) (define answer 42)");
    let path = directory.write(
        "config.scm",
        "(require \"helper.scm\") (register-event \"answer\" (lambda (_) answer))",
    );
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.load(&path).unwrap();
    assert_eq!(runtime.event("answer", &Value::Null).unwrap(), 42);
}

#[test]
fn wisp_load_errors_report_the_original_unicode_source_location() {
    let directory = SourceDirectory::new();
    let path = directory.write(
        "config.w",
        "; 前言\ndefine 界 \"文字\"\nlist 界 : missing-name 1\n",
    );
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    let error = runtime.load(&path).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains(&format!("{}:3:10", path.display())),
        "{message}"
    );
    assert!(error.downcast_ref::<SteelErr>().is_some());
}

#[test]
fn module_callback_errors_retain_the_original_wisp_call_sites() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .register_module_source(
            "failing",
            "; 引言\nprovide fail\ndefine : fail value\n  list \"界\" : error value\n",
            Path::new("plugins/失败.w"),
        )
        .unwrap();
    runtime
        .load_source(
            r#"require "failing"
register-event "fail"
  lambda : value
    list : fail "wrong"
"#,
            Path::new("callbacks.w"),
        )
        .unwrap();
    let error = runtime.event("fail", &Value::Null).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("plugins/失败.w:4:14"),
        "{message} {:?}",
        error.downcast_ref::<SteelErr>()
    );
    assert!(message.contains("callbacks.w:4:"), "{message}");
    let original = error.downcast_ref::<SteelErr>().unwrap();
    assert!(original.span().is_some());
    assert!(original.stack_trace().is_some());
}

#[test]
fn module_load_errors_report_the_modules_own_source() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .register_module_source(
            "invalid",
            "; 说明\nlist \"界\" : unknown-function 1\n",
            Path::new("plugins/invalid.w"),
        )
        .unwrap();
    let error = runtime
        .load_source("require \"invalid\"\n", Path::new("config.w"))
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("plugins/invalid.w:2:12"), "{message}");
}

#[test]
fn native_call_errors_report_the_original_wisp_call_site() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .load_source(
            r#"; 说明
register-event "native-failure"
  lambda : value
    set-editor-options! : obj 'mouse "界"
"#,
            Path::new("native.w"),
        )
        .unwrap();
    let error = runtime.event("native-failure", &Value::Null).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("native.w:4:5"), "{message}");
    assert!(message.contains("mouse must be a boolean"), "{message}");
}

#[test]
fn builtin_plugins_preserve_configuration_bindings_and_initialize_once() {
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime.load_builtin_plugins().unwrap();
    runtime
        .load_source(
            r#"
define-mode "custom" "probe" ("x" "outline")
set-view-options! "help" : obj 'gutter #t
register-event "builtin-bindings"
  lambda : value
    and
      procedure? preview-statusline
      procedure? diff-statusline
      procedure? map-modal
      procedure? set-mode-labels!
      procedure? request-open-buffer
      procedure? outline-handler
      null? : outline-tail "missing" '()
      not : outline-children? '()
      null? : outline-project '() '() : filter-compile : query "plain" ""
      outline-view? : outline-view '() : filter-compile : query "plain" ""
      equal? 7 : outline-state-version : outline-state "source" 7
      hash? ___outline-view-options___
      hash? ___outline-state-options___
      equal? struct:outline-view struct:outline-view
      equal? struct:outline-state struct:outline-state
"#,
            Path::new("builtin-configuration.w"),
        )
        .unwrap();
    assert_eq!(
        runtime.event("builtin-bindings", &Value::Null).unwrap(),
        true
    );
    assert!(
        matches!(runtime.command("outline", &serde_json::json!({})).unwrap(), CommandOutput::Request(HostRequest::OpenBuffer { kind, .. }) if kind == "outline")
    );
    runtime.load_builtin_plugins().unwrap();
    assert_eq!(runtime.view_options("help")["gutter"], true);
    assert_eq!(
        runtime.event("builtin-bindings", &Value::Null).unwrap(),
        true
    );
}

#[test]
fn builtin_plugins_load_outside_the_source_checkout() {
    const CHILD: &str = "ZESS_SCRIPT_BUILTIN_SOURCE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
        runtime.load_builtin_plugins().unwrap();
        assert!(runtime.commands().contains(&"outline".to_owned()));
        assert_eq!(
            runtime
                .format_statusline("default", &serde_json::json!({"plugin_status":"ready"}))
                .unwrap()["center"],
            "ready"
        );
        return;
    }
    let directory = SourceDirectory::new();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::builtin_plugins_load_outside_the_source_checkout",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .current_dir(&directory.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn register_source_pressure(runtime: &mut ScriptRuntime) {
    // Steel collects its source cache after compilation once it exceeds 8 MiB.
    let source = format!(
        ";{}\n(provide pressure-value) (define pressure-value 1)",
        "x".repeat(8 * 1024 * 1024)
    );
    runtime
        .register_module_source("source-pressure", &source, Path::new("pressure.scm"))
        .unwrap();
}

#[test]
fn wisp_callback_locations_survive_engine_source_collection() {
    let directory = SourceDirectory::new();
    let path = directory.write(
        "collected.w",
        r#"require "source-pressure"
register-event "collected"
  lambda : value
    error "failure after source collection"
"#,
    );
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    register_source_pressure(&mut runtime);
    runtime.load(&path).unwrap();
    assert!(runtime.engine.get_source_id(&path).is_none());
    let error = runtime.event("collected", &Value::Null).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains(&format!("{}:4:5", path.display())),
        "{message}"
    );
    let span = error.downcast_ref::<SteelErr>().unwrap().span().unwrap();
    assert!(
        runtime
            .engine
            .get_source(&span.source_id.unwrap())
            .is_none()
    );
}

#[test]
fn wisp_module_load_locations_survive_engine_source_collection() {
    let directory = SourceDirectory::new();
    let path = directory.write(
        "config.scm",
        "(require \"source-pressure\") (require \"invalid-after-collection\")",
    );
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    register_source_pressure(&mut runtime);
    runtime
        .register_module_source(
            "invalid-after-collection",
            "; 注释\nlist \"界\" : define\n",
            Path::new("plugins/collected.w"),
        )
        .unwrap();
    let error = runtime.load(&path).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        runtime
            .engine
            .get_source_id(Path::new("invalid-after-collection"))
            .is_none(),
        "{message}"
    );
    assert!(message.contains("plugins/collected.w:2:12"), "{message}");
}

#[test]
fn callback_stack_frames_follow_calls_from_the_failure_outward() {
    let directory = SourceDirectory::new();
    let path = directory.write(
        "outer.w",
        r#"require "middle"
register-event "stack-order"
  lambda : value
    list : middle value
"#,
    );
    let mut runtime = ScriptRuntime::new(|_, _| Value::Null).unwrap();
    runtime
        .register_module_source(
            "inner",
            "provide inner\ndefine : inner value\n  error \"inner failure\"\n",
            Path::new("plugins/inner.w"),
        )
        .unwrap();
    runtime
        .register_module_source(
            "middle",
            "require \"inner\"\nprovide middle\ndefine : middle value\n  list : inner value\n",
            Path::new("plugins/middle.w"),
        )
        .unwrap();
    runtime.load(&path).unwrap();
    let error = runtime.event("stack-order", &Value::Null).unwrap_err();
    let message = format!("{error:#}");
    let inner = message
        .find("plugins/inner.w:")
        .unwrap_or_else(|| panic!("{message}"));
    let middle = message
        .find("plugins/middle.w:")
        .unwrap_or_else(|| panic!("{message}"));
    let outer = message
        .find(&format!("{}:", path.display()))
        .unwrap_or_else(|| panic!("{message}"));
    assert!(inner < middle && middle < outer, "{message}");
}
