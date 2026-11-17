use super::*;
use zess_plugin::{
    BindingCatalog, ItemSelection, PluginOutput, PreviewContent, StatusConfig, StatusElement,
    StatusLayout, StatusLine,
};
use zess_protocol::{
    BufferSummary, ClientMessage, ClientWorkspace,
    document::DocumentSnapshot,
    resource::{PageMetadata, ResourceEvent, ResourceRequest},
};
use zess_ui::{Display, DisplayRow, Span, spans_width};

fn document() -> (Workbench, SessionSnapshot) {
    let state = SessionSnapshot {
        workspace: ClientWorkspace {
            current: Some(7),
            buffers: vec![7],
        },
        buffers: vec![BufferSummary {
            id: 7,
            kind: BufferKind::Text { path: None },
            modified: false,
        }],
        ..Default::default()
    };
    let mut workbench = Workbench {
        panes: Some(DocumentPanes::new(1, 7)),
        ..Default::default()
    };
    workbench.synchronize(&state);
    workbench
        .apply_document(
            DocumentEvent::Snapshot(DocumentSnapshot {
                buffer_id: 7,
                text: "line of text".into(),
                version: 1,
                modified: false,
                readonly: false,
                path: None,
                language: None,
                history: Default::default(),
            }),
            &state,
        )
        .unwrap();
    (workbench, state)
}

fn status_config() -> StatusConfig {
    StatusConfig {
        builtin_statuslines: ["default".into(), "preview".into(), "diff".into()].into(),
        mode_labels: [
            ("normal".into(), "NOR".into()),
            ("select".into(), "SEL".into()),
        ]
        .into(),
        ..Default::default()
    }
}

fn set_status_config(workbench: &mut Workbench, status: StatusConfig) {
    let mut catalog = BindingCatalog::default();
    catalog.status = status;
    workbench.editor.set_catalog(catalog);
}

fn publish_page(
    workbench: &mut Workbench,
    state: &SessionSnapshot,
    status: Value,
    preview: Option<PreviewContent>,
) {
    let subscription_id = workbench.pages.subscription().unwrap();
    workbench
        .apply_resource(
            ResourceUpdate {
                cache: Default::default(),
                kind: Default::default(),
                subscription_id,
                generation: 0,
                document_scoped: false,
                metadata: Some(PageMetadata {
                    view_id: 1000 + subscription_id,
                    buffer_id: 2000 + subscription_id,
                    options: Value::Null,
                }),
                document: None,
                patch_revision: None,
                output: PluginOutput {
                    display: Some(Display {
                        rows: vec![DisplayRow {
                            id: "file".into(),
                            text: "file".into(),
                            ..Default::default()
                        }],
                        status,
                        ..Default::default()
                    }),
                    selection: Some(ItemSelection {
                        primary: Some("file".into()),
                        selected: vec!["file".into()],
                        anchor: Some("file".into()),
                    }),
                    requests: preview
                        .map(|content| HostRequest::Preview {
                            path: "selected-file.txt".into(),
                            content,
                            focus: true,
                        })
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
            },
            state,
        )
        .unwrap();
}

#[test]
fn document_status_uses_the_local_cursor_position_after_resize() {
    let (mut workbench, state) = document();
    set_status_config(&mut workbench, status_config());
    for width in [1, 2, 3, 80] {
        let frame = workbench.render(&state, width, 10).unwrap();
        let status = &frame.rows.last().unwrap().spans;
        assert_eq!(spans_width(status), usize::from(width));
        if width == 80 {
            assert!(status.iter().any(|span| span.text == "1 sel  1:1  UTF-8"));
        }
    }

    workbench
        .native("text.move-right", Value::Null, &state, &mut Vec::new())
        .unwrap();
    let frame = workbench.render(&state, 80, 10).unwrap();
    assert!(
        frame
            .rows
            .last()
            .unwrap()
            .spans
            .iter()
            .any(|span| span.text == "1 sel  1:2  UTF-8")
    );
}

#[test]
fn a_configured_document_statusline_fits_the_pane_and_can_be_hidden() {
    let (mut workbench, mut state) = document();
    let mut config = status_config();
    config.builtin_components = ["position".into(), "encoding".into()].into();
    config.layouts.insert(
        "text".into(),
        StatusLayout {
            center: Some(vec![StatusElement::Literal("中心".into())]),
            right: Some(vec![StatusElement::Join {
                separator: " · ".into(),
                elements: vec![
                    StatusElement::Component("position".into()),
                    StatusElement::Component("encoding".into()),
                ],
            }]),
            ..Default::default()
        },
    );
    set_status_config(&mut workbench, config);
    for width in [1, 2, 3, 12, 80] {
        let frame = workbench.render(&state, width, 10).unwrap();
        let status = &frame.rows.last().unwrap().spans;
        assert_eq!(spans_width(status), usize::from(width));
        if width == 80 {
            let text = status
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            assert_eq!(text.find("中心"), Some(38));
            assert!(text.ends_with("1:1 · UTF-8"), "{text:?}");
        }
    }

    state.text_options = json!({"statusline": ""});
    let frame = workbench.render(&state, 80, 10).unwrap();
    assert!(frame.rows.last().unwrap().spans.iter().all(|span| {
        span.text.trim().is_empty() && span.style.is_empty() && span.base_style.is_none()
    }));
    assert!(workbench.statuslines.requests().is_empty());
}

#[test]
fn a_failed_status_formatter_keeps_the_last_line_with_the_current_pane_background() {
    let (mut workbench, mut state) = document();
    state.text_options = json!({"statusline": "custom"});
    workbench.editor.set_catalog(BindingCatalog::default());
    workbench.render(&state, 80, 10).unwrap();
    let requests = workbench.statuslines.requests();
    let [ClientMessage::FormatStatusline { id, name, context }] = requests.as_slice() else {
        panic!("expected the document's status request");
    };
    assert_eq!(name, "custom");
    assert!(context.active);
    workbench
        .statuslines
        .receive(
            *id,
            Ok(StatusLine {
                left: vec![
                    Span {
                        text: "explicit".into(),
                        style: "status".into(),
                        base_style: None,
                    },
                    Span {
                        text: " cached".into(),
                        style: String::new(),
                        base_style: None,
                    },
                ],
                center: Vec::new(),
                right: Vec::new(),
            }),
        )
        .unwrap();
    let frame = workbench.render(&state, 80, 10).unwrap();
    let status = &frame.rows.last().unwrap().spans;
    assert_eq!(spans_width(status), 80);
    assert!(
        status
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status"))
    );

    workbench
        .native("vsplit", Value::Null, &state, &mut Vec::new())
        .unwrap();
    workbench.render(&state, 24, 10).unwrap();
    let requests = workbench.statuslines.requests();
    let id = requests
        .iter()
        .find_map(|request| match request {
            ClientMessage::FormatStatusline { id, context, .. } if !context.active => Some(*id),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        workbench
            .statuslines
            .receive(id, Err("formatter failed".into())),
        Err("formatter failed".into())
    );
    let frame = workbench.render(&state, 24, 10).unwrap();
    let status = &frame.rows.last().unwrap().spans;
    let divider = status
        .iter()
        .position(|span| span.style == "window")
        .unwrap();
    let status = &status[..divider];
    assert_eq!(spans_width(status), 11);
    assert!(
        status
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status.inactive"))
    );
    assert_eq!(
        (status[0].text.as_str(), status[0].style.as_str()),
        ("explicit", "status")
    );
    assert_eq!(status[1].text, " ca");
    assert!(status[1].style.is_empty());
}

#[test]
fn git_status_counts_and_padding_follow_the_pane_background() {
    let (mut workbench, state) = document();
    set_status_config(&mut workbench, status_config());
    workbench.pages.open("git", json!({}));
    let git_status = json!({"git": {"additions": 123456, "deletions": 4}});
    publish_page(&mut workbench, &state, git_status, None);

    let frame = workbench.render(&state, 80, 10).unwrap();
    let active = &frame.rows.last().unwrap().spans;
    assert_eq!(spans_width(active), 80);
    let title = active
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(title.starts_with(" NOR  git"), "{title:?}");
    assert!(
        active
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status"))
    );

    workbench
        .native("vsplit", Value::Null, &state, &mut Vec::new())
        .unwrap();
    let frame = workbench.render(&state, 80, 10).unwrap();
    let status = &frame.rows.last().unwrap().spans;
    let divider = status
        .iter()
        .position(|span| span.style == "window")
        .unwrap();
    let inactive = &status[..divider];
    assert_eq!(spans_width(inactive), 39);
    assert!(
        inactive
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status.inactive"))
    );

    for spans in [active.as_slice(), inactive] {
        let text = spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(text.ends_with("+123456  -4 "), "{text:?}");
        assert!(
            spans
                .iter()
                .any(|span| span.text == "+123456" && span.style == "git.added")
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "-4" && span.style == "git.deleted")
        );
    }
}

#[test]
fn split_panes_keep_the_active_mode_label_whole() {
    for (kind, select, escape) in [
        ("text", "text.select-mode", "text.escape"),
        ("files", "data.select-mode", "data.escape"),
        ("git", "data.select-mode", "data.escape"),
    ] {
        let (mut workbench, state) = document();
        set_status_config(&mut workbench, status_config());
        if kind != "text" {
            workbench.pages.open(kind, json!({}));
            publish_page(&mut workbench, &state, Value::Null, None);
        }
        let source = workbench.active_pane().unwrap().view_id;
        workbench
            .native("vsplit", Value::Null, &state, &mut Vec::new())
            .unwrap();
        workbench.focus_pane(source);

        for (width, normal, selected) in [
            (10, "NOR", "SEL"),
            (11, " NOR ", " SEL "),
            (80, " NOR ", " SEL "),
        ] {
            let frame = workbench.render(&state, width, 24).unwrap();
            let label = &frame.rows.last().unwrap().spans[0];
            assert_eq!(
                (label.text.as_str(), label.style.as_str()),
                (normal, "status.normal"),
                "{kind}: width {width}"
            );

            workbench
                .native(select, Value::Null, &state, &mut Vec::new())
                .unwrap();
            let frame = workbench.render(&state, width, 24).unwrap();
            let label = &frame.rows.last().unwrap().spans[0];
            assert_eq!(
                (label.text.as_str(), label.style.as_str()),
                (selected, "status.select"),
                "{kind}: width {width}"
            );
            workbench
                .native(escape, Value::Null, &state, &mut Vec::new())
                .unwrap();
        }

        for _ in 0..4 {
            workbench
                .native("vsplit", Value::Null, &state, &mut Vec::new())
                .unwrap();
        }
        if kind != "text" {
            publish_page(&mut workbench, &state, Value::Null, None);
        }
        for width in [80, 60] {
            let frame = workbench.render(&state, width, 24).unwrap();
            let label = frame
                .rows
                .last()
                .unwrap()
                .spans
                .iter()
                .find(|span| span.style == "status.normal")
                .unwrap();
            assert_eq!(label.text.trim(), "NOR", "{kind}: width {width}");
        }
    }
}

#[test]
fn interactive_previews_use_their_mode_without_the_filename_in_the_statusline() {
    for (content, formatter, style, label) in [
        (
            PreviewContent::Text {
                text: "preview content".into(),
            },
            "preview",
            "status.normal",
            " NOR ",
        ),
        (
            PreviewContent::Diff {
                text: "@@ -1 +1 @@\n-before\n+after\n".into(),
            },
            "diff",
            "status.hunk",
            " HUN ",
        ),
    ] {
        let (mut workbench, state) = document();
        let mut config = status_config();
        config
            .view_options
            .insert(formatter.into(), json!({"statusline": formatter}));
        set_status_config(&mut workbench, config);
        workbench.pages.open("files", json!({}));
        publish_page(&mut workbench, &state, Value::Null, Some(content));
        let frame = workbench.render(&state, 100, 24).unwrap();

        let status = frame
            .rows
            .iter()
            .find(|row| row.spans.iter().any(|span| span.style == style))
            .unwrap();
        let mode = status
            .spans
            .iter()
            .find(|span| span.style == style)
            .unwrap();
        assert_eq!(mode.text, label);
        assert!(status.spans.iter().all(|span| {
            !span.text.contains("selected-file.txt") && !span.text.contains("readonly")
        }));
    }
}

#[test]
fn the_document_register_prompt_stays_visible_until_cancelled() {
    let (mut workbench, state) = document();
    workbench
        .native("text.select-register", Value::Null, &state, &mut Vec::new())
        .unwrap();
    let frame = workbench.render(&state, 80, 10).unwrap();
    assert_eq!(frame.prompt, "Register: type a name · Esc to cancel");

    workbench.key("a", &state, 80).unwrap();
    let frame = workbench.render(&state, 80, 10).unwrap();
    assert_eq!(frame.prompt, "Register \"a");

    workbench.key("Esc", &state, 80).unwrap();
    assert!(workbench.render(&state, 80, 10).unwrap().prompt.is_empty());
}

#[test]
fn a_file_page_prompts_for_the_register_until_a_name_or_cancellation() {
    let (mut workbench, state) = document();
    workbench.pages.open("files", json!({}));
    publish_page(&mut workbench, &state, Value::Null, None);
    for (key, expected) in [("a", Some('a')), ("Esc", None)] {
        workbench
            .native("data.select-register", Value::Null, &state, &mut Vec::new())
            .unwrap();
        assert_eq!(
            workbench.render(&state, 80, 10).unwrap().prompt,
            "Select register"
        );
        workbench.key(key, &state, 80).unwrap();
        assert!(workbench.render(&state, 80, 10).unwrap().prompt.is_empty());
        let requests = workbench.pages.take_requests();
        let register = requests.iter().find_map(|request| match request {
            ClientMessage::Resource {
                request:
                    ResourceRequest::Update {
                        event: ResourceEvent::Register,
                        input,
                        ..
                    },
            } => Some(input.register),
            _ => None,
        });
        assert_eq!(register, Some(expected));
    }
}
