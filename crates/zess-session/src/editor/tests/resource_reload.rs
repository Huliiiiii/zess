use super::*;
use zess_protocol::resource::{
    FilterKind, PageCommandInput, PageFilter, PageId, PageInput, PageSource, ResourceEvent,
    ResourceRequest, ResourceUpdate,
};
use zess_protocol::{CommandAction, CommandOutcome, CommandTarget};

const DRAFT_PLUGIN: &str = r#"
require "zess/buffer"
register-buffer "draft"
  lambda : input
    let*
      : draft : if (equal? (input-event input) "edit") "unsaved" (buffer-input-state input)
        modified : string? draft
      buffer-result draft null null
        output-modified
          buffer-output
            buffer-display (if modified draft "clean") "" '() #f ""
            . #f '()
          . modified
register-buffer-command "draft.edit" "Edit draft" "draft" "edit"
"#;

fn subscribe(
    session: &mut Session,
    subscription_id: u64,
    sequence: u64,
    kind: &str,
    input: PageInput,
) -> Vec<ResourceUpdate> {
    session.handle_resource(ResourceRequest::Subscribe {
        subscription_id,
        page_id: PageId {
            context_epoch: 0,
            sequence,
        },
        clone_from: None,
        source: PageSource {
            kind: kind.into(),
            argument: Value::Null,
        },
        input,
    });
    session.take_resource_updates()
}

fn page_input() -> PageInput {
    PageInput {
        generation: 1,
        selection: Default::default(),
        register: None,
        mode: "normal".into(),
        filter: None,
        source_buffer: None,
        width: 40,
        active: true,
        preview_width: 39,
        preview_height: 22,
    }
}

fn invoke(
    session: &mut Session,
    subscription_id: u64,
    input: &PageInput,
    name: &str,
) -> Option<CommandOutcome> {
    session.begin_operation(
        name,
        &CommandAction::Invoke {
            target: CommandTarget::Page {
                subscription_id,
                input: PageCommandInput {
                    generation: input.generation,
                    selection: input.selection.clone(),
                    register: input.register,
                },
            },
            name: name.into(),
            argument: json!({}),
        },
    )
}

#[test]
fn configuration_reload_refuses_to_discard_live_or_retained_script_drafts() {
    let root = Fixture::new();
    let config = root.write("init.w", DRAFT_PLUGIN);
    let mut session = Session::new(root.0.clone(), &[], Some(config.clone())).unwrap();
    let input = page_input();
    subscribe(&mut session, 1, 1, "draft", input.clone());
    assert_eq!(
        invoke(&mut session, 1, &input, "draft.edit"),
        Some(CommandOutcome::Applied)
    );
    assert!(session.take_resource_updates().iter().any(|update| {
        update.output.modified == Some(true)
            && update.output.display.as_ref().unwrap().title == "unsaved"
    }));
    fs::write(
        &config,
        format!("{DRAFT_PLUGIN}\nset-editor-options! : obj 'mouse #f\n"),
    )
    .unwrap();
    assert!(matches!(
        invoke(&mut session, 1, &input, "config-reload"),
        Some(CommandOutcome::Failed { message }) if message.contains("save or undo")
    ));
    assert!(session.snapshot().editor_options.mouse);
    session.handle_resource(ResourceRequest::Release { subscription_id: 1 });
    let error = session.command("config-reload").unwrap_err();
    assert!(error.to_string().contains("save or undo"));
    assert!(session.snapshot().editor_options.mouse);
    let updates = subscribe(&mut session, 2, 1, "draft", input.clone());
    assert!(updates.iter().any(|update| {
        update.output.modified == Some(true)
            && update.output.display.as_ref().unwrap().title == "unsaved"
    }));
}

#[test]
fn removing_a_buffer_plugin_requires_closing_its_live_and_retained_pages() {
    let root = Fixture::new();
    let config = root.write("init.w", DRAFT_PLUGIN);
    let mut session = Session::new(root.0.clone(), &[], Some(config.clone())).unwrap();
    let input = page_input();
    for id in [1, 2] {
        assert!(
            subscribe(&mut session, id, id, "draft", input.clone())
                .iter()
                .any(|update| update.metadata.is_some())
        );
    }
    session.handle_resource(ResourceRequest::Release { subscription_id: 2 });
    fs::write(&config, "set-editor-options! : obj 'mouse #f\n").unwrap();
    for sequence in [1, 2] {
        let outcome = if sequence == 1 {
            invoke(&mut session, 1, &input, "config-reload")
        } else {
            session.begin_operation(
                "reload",
                &CommandAction::CommandLine {
                    text: "config-reload".into(),
                },
            )
        };
        assert!(
            matches!(outcome, Some(CommandOutcome::Failed { message }) if message.contains("removes the draft buffer plugin"))
        );
        assert!(session.snapshot().editor_options.mouse);
        session.handle_resource(ResourceRequest::Discard {
            page_id: PageId {
                context_epoch: 0,
                sequence,
            },
        });
    }
    session.command("config-reload").unwrap();
    assert!(!session.snapshot().editor_options.mouse);
}

#[test]
fn reload_reports_page_initialization_failure_after_installing_the_configuration() {
    let root = Fixture::new();
    let config = root.write("init.w", DRAFT_PLUGIN);
    let mut session = Session::new(root.0.clone(), &[], Some(config.clone())).unwrap();
    let input = page_input();
    assert!(
        subscribe(&mut session, 1, 1, "draft", input.clone())
            .iter()
            .any(|update| update.metadata.is_some())
    );

    fs::write(
        &config,
        "set-editor-options! : obj 'mouse #f\nregister-buffer \"draft\"\n  lambda : input\n    error \"draft initialization failed\"\n",
    )
    .unwrap();
    let Some(CommandOutcome::Failed { message }) = invoke(&mut session, 1, &input, "config-reload")
    else {
        panic!("the reload receipt must report the page error");
    };
    assert!(message.starts_with("could not refresh draft page after installing the configuration"));
    assert!(message.contains("draft initialization failed"));
    assert!(!session.snapshot().editor_options.mouse);
    assert!(session.take_resource_updates().iter().any(|update| {
        update.subscription_id == 1
            && update
                .output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::Message { text } if text == &message))
    }));
}

#[test]
fn native_file_drafts_survive_reload_and_prevent_changing_their_backend() {
    let root = Fixture::new();
    let config = root.write("init.w", "");
    let file = root.write("pending.txt", "draft contents");
    let mut session = Session::new(root.0.clone(), &[], Some(config.clone())).unwrap();
    let mut input = page_input();
    input.selection = ItemSelection {
        primary: Some("pending.txt".into()),
        selected: vec!["pending.txt".into()],
        anchor: Some("pending.txt".into()),
    };
    subscribe(&mut session, 1, 1, "files", input.clone());
    assert_eq!(
        invoke(&mut session, 1, &input, "files.delete"),
        Some(CommandOutcome::Applied)
    );
    assert!(
        session
            .take_resource_updates()
            .iter()
            .any(|update| { update.output.modified == Some(true) })
    );

    session.handle_resource(ResourceRequest::Release { subscription_id: 1 });
    fs::write(
        &config,
        format!(
            "{}\nset-editor-options! : obj 'mouse #f\n",
            DRAFT_PLUGIN.replace("\"draft\"", "\"files\"")
        ),
    )
    .unwrap();
    let error = session.command("config-reload").unwrap_err();
    assert!(format!("{error:#}").contains("save or undo pending files buffer changes"));
    assert!(session.snapshot().editor_options.mouse);

    fs::write(&config, "set-editor-options! : obj 'mouse #f\n").unwrap();
    session.command("config-reload").unwrap();
    assert!(!session.snapshot().editor_options.mouse);
    let updates = subscribe(&mut session, 2, 1, "files", input.clone());
    assert!(
        updates
            .iter()
            .any(|update| update.output.modified == Some(true))
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "draft contents");
    assert_eq!(
        invoke(&mut session, 2, &input, "write"),
        Some(CommandOutcome::Applied)
    );
    assert!(!file.exists());
    assert!(!session.snapshot().modified);
}

#[test]
fn outlining_keeps_the_filter_and_selection_across_configuration_reload() {
    let root = Fixture::new();
    let path = root.write(
        "outline.rs",
        "fn alpha() {}\nfn bravo() {}\nfn bravura() {}\n",
    );
    let config = root.write("init.w", "");
    let mut session = Session::new(root.0.clone(), &[path], Some(config.clone())).unwrap();
    let mut input = page_input();
    input.source_buffer = session.snapshot().workspace.current;
    input.filter = Some(PageFilter {
        kind: FilterKind::Plain,
        text: "brav".into(),
    });
    let updates = subscribe(&mut session, 1, 1, "outline", input.clone());
    let display = updates
        .iter()
        .rev()
        .find_map(|update| update.output.display.as_ref())
        .unwrap();
    let id = display
        .rows
        .iter()
        .find(|row| row.text.ends_with("bravura"))
        .unwrap()
        .id
        .clone();
    input.selection = ItemSelection {
        primary: Some(id.clone()),
        selected: vec![id.clone()],
        anchor: Some(id),
    };
    session.handle_resource(ResourceRequest::Update {
        subscription_id: 1,
        event: ResourceEvent::Select {
            navigation: ItemNavigation::Step,
        },
        input: input.clone(),
    });
    session.take_resource_updates();
    fs::write(&config, "set-editor-options! : obj 'mouse #f\n").unwrap();
    assert!(matches!(
        invoke(&mut session, 1, &input, "config-reload"),
        Some(CommandOutcome::Message { message }) if message == "user plugins reloaded"
    ));
    let live = session.take_resource_updates();
    session.handle_resource(ResourceRequest::Release { subscription_id: 1 });
    session.command("config-reload").unwrap();
    let retained = subscribe(&mut session, 2, 1, "outline", input.clone());
    for updates in [live, retained] {
        let display = updates
            .iter()
            .rev()
            .find_map(|update| update.output.display.as_ref())
            .unwrap();
        let actual: Vec<_> = display
            .rows
            .iter()
            .map(|row| row.text.split_whitespace().last().unwrap())
            .collect();
        assert_eq!(actual, ["bravo", "bravura"]);
        let selection = updates
            .iter()
            .rev()
            .find_map(|update| update.output.selection.as_ref())
            .unwrap();
        assert_eq!(selection, &input.selection);
    }
}
