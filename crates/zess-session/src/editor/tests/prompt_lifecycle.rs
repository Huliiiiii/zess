use super::*;

fn cancellable_session(fixture: &Fixture) -> Session {
    let scripts = Fixture::new();
    let mut session = fixture.session(&[]);
    session
        .scripts
        .as_mut()
        .unwrap()
        .load(&scripts.write(
            "cancellable.w",
            r#"
require "zess/buffer"
require "zess/ui"
define cancel-attempts 0
define : cancelling? input
  or
    equal? (input-event input) "layer_result"
    and
      equal? (input-event input) "mode_changed"
      equal? (input-mode input) "normal"
register-buffer "cancellable"
  lambda : input
    when (cancelling? input)
      set! cancel-attempts : + cancel-attempts 1
      when (= cancel-attempts 1)
        error "cancellation rejected"
    buffer-result null null null
      buffer-output
        buffer-display (if (cancelling? input) "cancelled" "ready") "" '() #f ""
        . #f
        if (cancelling? input) (list (request-mode "normal")) '()
register-command "cancellable" "Open a cancellable input"
  lambda : argument
    request-open-buffer "cancellable" argument
"#,
        ))
        .unwrap();
    session.command("cancellable").unwrap();
    session
}

#[test]
fn failed_minibuffer_cancellation_keeps_the_input_and_focus_for_retry() {
    let fixture = Fixture::new();
    let mut session = cancellable_session(&fixture);
    let source = session.focus;
    session.command("vsplit").unwrap();
    let other = session.focus;
    session.focus_view(source).unwrap();
    session
        .request(
            source,
            HostRequest::MiniBuffer {
                purpose: "retry".into(),
                text: "draft".into(),
                key_scope: None,
            },
        )
        .unwrap();
    assert!(session.focus_view(other).is_err());
    assert_eq!(session.focus, source);
    assert!(session.help.is_none());
    assert_eq!(session.views.values().count(), 2);
    let prompt = session.active_prompt().unwrap();
    assert_eq!(prompt.text, "draft");
    assert!(prompt.error.is_some());
    session.input("Esc");
    assert!(session.prompt.is_none());
    assert_eq!(session.focus, source);
    assert!(session.views[&source].location.mode == ViewMode::Normal);
    assert_eq!(session.views[&source].location.display.title, "cancelled");
}

#[test]
fn failed_filter_cancellation_keeps_filter_mode_and_allows_retry() {
    let fixture = Fixture::new();
    let mut session = cancellable_session(&fixture);
    let source = session.focus;
    session.command("vsplit").unwrap();
    let other = session.focus;
    session.focus_view(source).unwrap();
    session.execute_command("data.filter", json!({})).unwrap();
    for key in ["n", "e", "e", "d", "l", "e"] {
        session.input(key);
    }
    assert!(session.focus_view(other).is_err());
    assert_eq!(session.focus, source);
    assert!(session.help.is_none());
    assert!(session.views[&source].location.mode == ViewMode::Filter);
    let prompt = session.active_prompt().unwrap();
    assert_eq!(prompt.text, "needle");
    assert!(prompt.error.is_some());
    session.input("Esc");
    assert!(session.prompt.is_none());
    assert!(session.views[&source].location.mode == ViewMode::Normal);
    assert_eq!(session.views[&source].location.display.title, "cancelled");
}

#[test]
fn buffer_replacement_waits_for_minibuffer_cancellation_to_succeed() {
    let fixture = Fixture::new();
    let mut session = cancellable_session(&fixture);
    let source = session.focus;
    let buffer = session.views[&source].location.buffer;
    session
        .request(
            source,
            HostRequest::MiniBuffer {
                purpose: "retry".into(),
                text: "draft".into(),
                key_scope: None,
            },
        )
        .unwrap();
    assert!(session.command("new").is_err());
    assert_eq!(session.views[&source].location.buffer, buffer);
    assert_eq!(session.active_prompt().unwrap().text, "draft");
    session.command("new").unwrap();
    assert!(session.prompt.is_none());
    assert_ne!(session.views[&source].location.buffer, buffer);
    let current = session.views[&source].location.buffer;
    assert!(matches!(&session.buffers[&current], Buffer::Text(text) if text.text().is_empty()));
    session.input("i");
    session.input("x");
    let current = session.views[&source].location.buffer;
    assert!(matches!(&session.buffers[&current], Buffer::Text(text) if text.text() == "x"));
}

#[test]
fn failed_help_pinning_keeps_suspended_input_available_when_help_is_closed() {
    let fixture = Fixture::new();
    let mut session = cancellable_session(&fixture);
    let source = session.focus;
    session
        .request(
            source,
            HostRequest::MiniBuffer {
                purpose: "retry".into(),
                text: "draft".into(),
                key_scope: None,
            },
        )
        .unwrap();
    session.command("help").unwrap();
    assert!(session.execute_command("help.pin", json!({})).is_err());
    assert!(session.help.is_some());
    session.execute_command("help.close", json!({})).unwrap();
    assert_eq!(session.focus, source);
    let prompt = session.active_prompt().unwrap();
    assert_eq!(prompt.text, "draft");
    assert!(prompt.error.is_some());
    session.input("Esc");
    assert!(session.prompt.is_none());
    assert_eq!(session.views[&source].location.display.title, "cancelled");
}

#[test]
fn retiring_a_source_waits_for_its_suspended_input_to_cancel() {
    let fixture = Fixture::new();
    let mut session = cancellable_session(&fixture);
    let source = session.focus;
    let page = session.views.page_id(source);
    session
        .request(
            source,
            HostRequest::MiniBuffer {
                purpose: "retry".into(),
                text: "draft".into(),
                key_scope: None,
            },
        )
        .unwrap();
    session.command("help").unwrap();
    assert!(session.retire_page(page).is_err());
    assert!(session.views.contains_key(&source));
    assert!(session.help.is_some());
    session.execute_command("help.close", json!({})).unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(session.active_prompt().unwrap().text, "draft");
    session.input("Esc");
    assert!(session.prompt.is_none());
    assert_eq!(session.views[&source].location.display.title, "cancelled");
}

#[test]
fn retiring_a_page_keeps_its_attached_layer_recoverable() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    let page = session.views.page_id(source);
    session
        .request(
            source,
            HostRequest::Layer {
                purpose: "draft".into(),
                text: "draft text".into(),
                inline: true,
            },
        )
        .unwrap();
    let layer = session.focus;
    assert!(session.retire_page(page).is_err());
    assert!(session.views.contains_key(&source));
    assert_eq!(session.views.parent(layer), Some(source));
    assert_eq!(session.views.child(source), Some(layer));
    session.command("write").unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(
        session.views[&source].location.memories["draft:draft"]["text"],
        "draft text"
    );
}

#[test]
fn rejecting_help_pinning_inside_a_fullscreen_layer_preserves_suspended_input() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "red blue\n").unwrap();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session
        .request(
            source,
            HostRequest::FileLayer {
                path: "note.txt".into(),
                line: 0,
            },
        )
        .unwrap();
    let layer = session.focus;
    session.input("/");
    session.input("b");
    session.command("help").unwrap();
    assert!(session.execute_command("help.pin", json!({})).is_err());
    assert!(session.help.is_some());
    session.execute_command("help.close", json!({})).unwrap();
    assert_eq!(session.focus, layer);
    assert_eq!(session.active_prompt().unwrap().text, "b");
    session.input("l");
    assert_eq!(session.active_prompt().unwrap().text, "bl");
    session.input("Esc");
    assert!(session.prompt.is_none());
    let buffer = session.views[&layer].location.buffer;
    let text = session.buffers.get_mut(&buffer).unwrap().text().unwrap();
    let selections = text.selections(layer);
    assert_eq!(selections.len(), 1);
    assert_eq!((selections[0].row, selections[0].column), (0, 0));
}

#[test]
fn source_updates_receive_the_query_while_help_owns_its_input() {
    let scripts = Fixture::new();
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session
        .scripts
        .as_mut()
        .unwrap()
        .load(&scripts.write(
            "query-aware.w",
            r#"
require "zess/buffer"
require "zess/ui"
register-buffer "query-aware"
  lambda : input
    buffer-result null null null
      buffer-output
        buffer-display (if (input-query input) "query present" "query missing") "" '() #f ""
        . #f '()
register-command "query-aware" "Open a query-aware buffer"
  lambda : argument
    request-open-buffer "query-aware" argument
"#,
        ))
        .unwrap();
    session.command("query-aware").unwrap();
    let source = session.focus;
    session.execute_command("data.filter", json!({})).unwrap();
    session.input("n");
    session.command("help").unwrap();
    session.dispatch(source, "render", Value::Null).unwrap();
    assert_eq!(
        session.views[&source].location.display.title,
        "query present"
    );
    session.execute_command("help.close", json!({})).unwrap();
    assert_eq!(session.active_prompt().unwrap().text, "n");
}

#[test]
fn failed_input_cancellation_does_not_create_a_split() {
    let fixture = Fixture::new();
    for input in ["filter", "minibuffer"] {
        let mut session = cancellable_session(&fixture);
        let source = session.focus;
        if input == "filter" {
            session.execute_command("data.filter", json!({})).unwrap();
        } else {
            session
                .request(
                    source,
                    HostRequest::MiniBuffer {
                        purpose: "retry".into(),
                        text: "draft".into(),
                        key_scope: None,
                    },
                )
                .unwrap();
        }
        let layout = panes(&session);
        let buffers = session.buffers.len();
        assert!(session.command("vsplit").is_err(), "{input}");
        assert_eq!(session.focus, source);
        assert_eq!(session.views.values().count(), 1);
        assert_eq!(panes(&session), layout);
        assert_eq!(session.buffers.len(), buffers);
        assert!(session.active_prompt().unwrap().error.is_some());
        session.command("vsplit").unwrap();
        assert_eq!(session.views.values().count(), 2);
        assert!(session.prompt.is_none());
        assert!(
            session
                .views
                .values()
                .all(|view| view.location.mode == ViewMode::Normal)
        );
    }
}

#[test]
fn failed_input_cancellation_does_not_create_a_layer_or_close_help() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "source\n").unwrap();
    for kind in ["draft", "file", "snapshot"] {
        let mut session = cancellable_session(&fixture);
        let source = session.focus;
        session
            .request(
                source,
                HostRequest::MiniBuffer {
                    purpose: "retry".into(),
                    text: "draft".into(),
                    key_scope: None,
                },
            )
            .unwrap();
        session.command("help").unwrap();
        let help = session.focus;
        let open = |session: &mut Session| match kind {
            "draft" => session.request(
                help,
                HostRequest::Layer {
                    purpose: "new".into(),
                    text: "new draft".into(),
                    inline: true,
                },
            ),
            "file" => session.request(
                help,
                HostRequest::FileLayer {
                    path: "note.txt".into(),
                    line: 0,
                },
            ),
            "snapshot" => session.open_snapshot(
                help,
                FileSnapshot {
                    id: SnapshotId {
                        repository: fixture.0.to_string_lossy().into_owned(),
                        commit: "historical".into(),
                        path: "note.txt".into(),
                        blob: "historical-blob".into(),
                    },
                    text: "historical source\n".into(),
                },
                SnapshotPresentation::Fullscreen,
            ),
            _ => unreachable!(),
        };
        let buffers = session.buffers.len();
        let layout = panes(&session);
        assert!(open(&mut session).is_err(), "{kind}");
        assert_eq!(session.focus, help);
        assert!(session.help.is_some());
        assert_eq!(session.views.values().count(), 2);
        assert_eq!(session.buffers.len(), buffers);
        assert_eq!(panes(&session), layout);
        assert_eq!(session.views.child(source), None);
        assert_eq!(session.views.child(help), None);
        assert_eq!(
            session
                .help
                .as_ref()
                .unwrap()
                .suspended
                .as_ref()
                .unwrap()
                .text,
            "draft"
        );
        open(&mut session).unwrap();
        let layer = session.focus;
        assert!(session.help.is_none());
        assert!(session.prompt.is_none());
        assert_eq!(session.views.values().count(), 2);
        assert_eq!(session.views.child(source), Some(layer));
        assert_eq!(session.views.parent(layer), Some(source));
    }
}
