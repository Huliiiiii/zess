use super::*;
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use zess_plugin::PluginContext;
use zess_services::NativeServices;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zess-file-yank-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn plugin(&self) -> (Files, PluginInput) {
        let services = Rc::new(RefCell::new(NativeServices::new(self.0.clone()).unwrap()));
        let mut plugin = Files::new(services);
        let input = PluginInput {
            buffer_id: 1,
            view_id: 1,
            generation: 1,
            preview_debounce: Duration::ZERO,
            preview_area: None,
            event: "render".into(),
            argument: serde_json::Value::Null,
            query: None,
            options: serde_json::Value::Null,
            mode: "normal".into(),
            selection: ItemSelection::default(),
            register: None,
            context: PluginContext::default(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = plugin.update(&input).unwrap();
            if !output.display.unwrap().rows.is_empty() {
                return (plugin, input);
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn select(input: &mut PluginInput, paths: &[&str]) {
    input.selection = ItemSelection {
        primary: paths.first().map(|path| (*path).into()),
        anchor: paths.first().map(|path| (*path).into()),
        selected: paths.iter().map(|path| (*path).into()).collect(),
    };
}

fn values(output: PluginOutput) -> Vec<Vec<String>> {
    output
        .requests
        .into_iter()
        .filter_map(|request| {
            if let HostRequest::Yank { values, .. } = request {
                Some(values)
            } else {
                None
            }
        })
        .collect()
}

fn queue_content(plugin: &mut Files, input: &PluginInput, path: &str) -> PluginOutput {
    let mut output = PluginOutput::default();
    plugin
        .views
        .get_mut(&(input.buffer_id, input.view_id))
        .unwrap()
        .yanks
        .queue(
            &plugin.services,
            Event::YankContent,
            input,
            vec![Target {
                path: path.into(),
                name: path.into(),
                kind: TargetKind::File {
                    source: Source::Worktree,
                    original_path: None,
                },
            }],
            &mut output,
        )
        .unwrap();
    output
}

#[test]
fn path_yanks_preserve_each_selected_path() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join("a.txt"), "A").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["folder", "a.txt"]);
    for (event, expected) in [
        ("yank-path", vec!["folder".into(), "a.txt".into()]),
        ("yank-name", vec!["folder".into(), "a.txt".into()]),
        (
            "yank-absolute-path",
            vec![
                fixture.0.join("folder").display().to_string(),
                fixture.0.join("a.txt").display().to_string(),
            ],
        ),
    ] {
        input.event = event.into();
        assert_eq!(values(plugin.update(&input).unwrap()), [expected]);
    }
}

#[test]
fn queued_yanks_keep_their_original_selection_and_register() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "first\n").unwrap();
    fs::write(fixture.0.join("b.txt"), "second\n").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["b.txt", "a.txt"]);
    input.event = "yank-content".into();
    input.register = Some('+');
    let mut publications = plugin.update(&input).unwrap().requests;
    select(&mut input, &["a.txt"]);
    input.event = "yank-name".into();
    input.register = Some('a');
    publications.extend(plugin.update(&input).unwrap().requests);
    input.event = "render".into();
    input.register = Some('*');
    let deadline = Instant::now() + Duration::from_secs(5);
    while publications
        .iter()
        .filter(|request| matches!(request, HostRequest::Yank { .. }))
        .count()
        < 2
    {
        publications.extend(plugin.update(&input).unwrap().requests);
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let publications = publications
        .into_iter()
        .filter_map(|request| match request {
            HostRequest::Yank { register, values } => Some((register, values)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        publications,
        [
            (Some('+'), vec!["second\n".into(), "first\n".into()]),
            (Some('a'), vec!["a.txt".into()]),
        ]
    );
}

#[test]
fn an_invalid_file_prevents_a_partial_content_yank() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "valid").unwrap();
    fs::write(fixture.0.join("b.bin"), b"binary\0data").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["a.txt", "b.bin"]);
    input.event = "yank-content".into();
    input.context.operation_id = Some("binary-content".into());
    let mut output = plugin.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["binary-content"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            !output
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::Yank { .. }))
        );
        if output.requests.iter().any(
            |request| matches!(request, HostRequest::Message { text } if text.contains("binary")),
        ) {
            assert!(matches!(
                output.completed_operations.as_slice(),
                [zess_plugin::OperationCompletion { operation_id, result: zess_plugin::OperationResult::Failed { message } }]
                    if operation_id == "binary-content" && message.contains("binary")
            ));
            break;
        }
        assert!(Instant::now() < deadline);
        output = plugin.poll_operations().pop().unwrap_or_default();
        std::thread::yield_now();
    }
}

#[test]
fn content_yanks_complete_without_further_view_updates() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "copied contents\n").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["a.txt"]);
    input.event = "yank-content".into();
    input.context.operation_id = Some("read-content".into());
    let mut output = plugin.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["read-content"]);
    assert!(output.completed_operations.is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    while output.completed_operations.is_empty() {
        assert!(Instant::now() < deadline);
        output = plugin.poll_operations().pop().unwrap_or_default();
        std::thread::yield_now();
    }
    assert!(matches!(
        output.completed_operations.as_slice(),
        [zess_plugin::OperationCompletion { operation_id, result: zess_plugin::OperationResult::Completed }]
            if operation_id == "read-content"
    ));
    assert_eq!(values(output), [vec!["copied contents\n".to_owned()]]);
    assert!(plugin.poll_operations().is_empty());
}

#[test]
fn a_failed_yank_does_not_share_its_output_with_a_successful_yank() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "copied contents\n").unwrap();
    fs::write(fixture.0.join("b.bin"), b"binary\0data").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["b.bin"]);
    input.event = "yank-content".into();
    input.context.operation_id = Some("binary-content".into());
    let output = plugin.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["binary-content"]);
    assert!(output.completed_operations.is_empty());
    select(&mut input, &["a.txt"]);
    input.context.operation_id = Some("text-content".into());
    let output = plugin.update(&input).unwrap();
    assert_eq!(output.pending_operations, ["text-content"]);
    assert!(output.completed_operations.is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut outputs = Vec::new();
    while outputs.len() < 2 {
        assert!(Instant::now() < deadline);
        outputs.extend(plugin.poll_operations());
        std::thread::yield_now();
    }
    assert_eq!(outputs.len(), 2);
    let success = outputs.pop().unwrap();
    let failure = outputs.pop().unwrap();
    assert!(matches!(
        failure.completed_operations.as_slice(),
        [zess_plugin::OperationCompletion { operation_id, result: zess_plugin::OperationResult::Failed { message } }]
            if operation_id == "binary-content" && message.contains("binary")
    ));
    assert!(values(failure).is_empty());
    assert!(matches!(
        success.completed_operations.as_slice(),
        [zess_plugin::OperationCompletion { operation_id, result: zess_plugin::OperationResult::Completed }]
            if operation_id == "text-content"
    ));
    assert_eq!(values(success), [vec!["copied contents\n".to_owned()]]);
}

#[test]
fn removing_a_view_or_buffer_reports_its_pending_yanks_as_cancelled() {
    for remove in [
        Files::remove_buffer_view as fn(&mut Files, u64, u64),
        |plugin, _, view| plugin.remove_view(view),
        |plugin, buffer, _| plugin.remove_buffer(buffer),
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("a.txt"), "content").unwrap();
        let (mut plugin, mut input) = fixture.plugin();
        input.context.operation_id = Some("removed-content".into());
        let output = queue_content(&mut plugin, &input, "a.txt");
        assert_eq!(output.pending_operations, ["removed-content"]);
        remove(&mut plugin, input.buffer_id, input.view_id);
        let mut outputs = plugin.poll_operations();
        assert_eq!(outputs.len(), 1);
        let output = outputs.pop().unwrap();
        assert!(matches!(
            output.completed_operations.as_slice(),
            [zess_plugin::OperationCompletion { operation_id, result: zess_plugin::OperationResult::Cancelled }]
                if operation_id == "removed-content"
        ));
        assert!(values(output).is_empty());
        assert!(plugin.poll_operations().is_empty());
    }
}

#[test]
fn read_only_registers_cannot_queue_content_yanks() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "content").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["note.txt"]);
    input.event = "yank-content".into();
    for register in ['#', '.', '%'] {
        input.register = Some(register);
        let Err(error) = plugin.update(&input) else {
            panic!("read-only target was accepted");
        };
        assert_eq!(
            error.to_string(),
            format!("Register {register} does not support writing")
        );
        assert!(plugin.views[&(1, 1)].yanks.is_empty());
    }
}

#[test]
fn directories_are_rejected_before_content_queries_start() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join("a.txt"), "valid").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["a.txt", "folder"]);
    for event in ["yank-content", "yank-diff"] {
        input.event = event.into();
        assert!(plugin.update(&input).is_err());
        assert!(plugin.views[&(1, 1)].yanks.is_empty());
    }
}

#[test]
fn leaving_the_view_prevents_later_yank_publications() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "content").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["a.txt"]);
    input.context.operation_id = Some("leaving-content".into());
    queue_content(&mut plugin, &input, "a.txt");
    input.event = "view_leaving".into();
    let output = plugin.update(&input).unwrap();
    assert!(matches!(
        output.completed_operations.as_slice(),
        [zess_plugin::OperationCompletion { operation_id, result: zess_plugin::OperationResult::Cancelled }]
            if operation_id == "leaving-content"
    ));
    assert!(values(output).is_empty());
    assert!(plugin.views[&(1, 1)].yanks.is_empty());
    input.event = "view_entered".into();
    assert!(values(plugin.update(&input).unwrap()).is_empty());
}

#[test]
fn a_failed_batch_rename_preserves_all_sources_and_can_be_retried() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "first").unwrap();
    fs::write(fixture.0.join("b.txt"), "second").unwrap();
    fs::write(fixture.0.join("occupied.txt"), "existing").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["a.txt", "b.txt"]);
    input.event = "rename".into();
    plugin.update(&input).unwrap();
    input.event = "layer_result".into();
    input.argument = json!({"purpose": "rename", "text": "renamed-a.txt"});
    let first = plugin.update(&input).unwrap();
    assert_eq!(first.modified, Some(false));
    assert!(first.requests.iter().any(|request| matches!(request,
        HostRequest::MiniBuffer { purpose, text, .. } if purpose == "rename" && text == "b.txt"
    )));
    input.argument = json!({"purpose": "rename", "text": "occupied.txt"});
    let Err(error) = plugin.update(&input) else {
        panic!("the conflicting destination was accepted");
    };
    assert_eq!(
        error.to_string(),
        "destination already exists: occupied.txt"
    );
    input.event = "save".into();
    let output = plugin.update(&input).unwrap();
    assert_eq!(output.modified, Some(false));
    assert_eq!(
        fs::read_to_string(fixture.0.join("a.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("b.txt")).unwrap(),
        "second"
    );
    assert!(!fixture.0.join("renamed-a.txt").exists());
    input.event = "layer_result".into();
    input.argument = json!({"purpose": "rename", "text": "renamed-b.txt"});
    assert_eq!(plugin.update(&input).unwrap().modified, Some(true));
    input.event = "save".into();
    assert_eq!(plugin.update(&input).unwrap().modified, Some(false));
    assert!(!fixture.0.join("a.txt").exists());
    assert!(!fixture.0.join("b.txt").exists());
    assert_eq!(
        fs::read_to_string(fixture.0.join("renamed-a.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("renamed-b.txt")).unwrap(),
        "second"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("occupied.txt")).unwrap(),
        "existing"
    );
}

#[test]
fn cancelling_a_batch_rename_keeps_earlier_destinations_unapplied() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("a.txt"), "first").unwrap();
    fs::write(fixture.0.join("b.txt"), "second").unwrap();
    let (mut plugin, mut input) = fixture.plugin();
    select(&mut input, &["a.txt", "b.txt"]);
    input.event = "rename".into();
    plugin.update(&input).unwrap();
    input.event = "layer_result".into();
    input.argument = json!({"purpose": "rename", "text": "renamed-a.txt"});
    plugin.update(&input).unwrap();
    input.argument = json!({"purpose": "rename", "cancelled": true});
    assert_eq!(plugin.update(&input).unwrap().modified, Some(false));
    input.event = "save".into();
    plugin.update(&input).unwrap();
    assert!(!fixture.0.join("renamed-a.txt").exists());
    assert_eq!(
        fs::read_to_string(fixture.0.join("a.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("b.txt")).unwrap(),
        "second"
    );
}
