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
    let mut output = plugin.update(&input).unwrap();
    input.event = "render".into();
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
            break;
        }
        assert!(Instant::now() < deadline);
        output = plugin.update(&input).unwrap();
        std::thread::yield_now();
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
    input.event = "yank-content".into();
    plugin.update(&input).unwrap();
    input.event = "view_leaving".into();
    assert!(values(plugin.update(&input).unwrap()).is_empty());
    assert!(plugin.views[&(1, 1)].yanks.is_empty());
    input.event = "view_entered".into();
    assert!(values(plugin.update(&input).unwrap()).is_empty());
}
