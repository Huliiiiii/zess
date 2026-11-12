use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use super::*;

mod browser_layout;
mod count_input;
mod file_reload;
mod file_yank;
mod files_find;
mod filters;
mod git_navigation;
mod help;
mod input;
mod item_scroll;
mod language;
mod mode_labels;
mod mouse;
mod native_plugins;
mod outline;
mod preview_config;
mod preview_surfaces;
mod prompt_lifecycle;
mod regex_input;
mod simulation;
mod status;
mod sync_state;
mod text_input;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zess-session-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, source).unwrap();
        path
    }

    fn session(&self, files: &[PathBuf]) -> Session {
        let session = Session::new(self.0.clone(), files, None).unwrap();
        assert!(session.generation > 0, "{}", session.message);
        session
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn settle(session: &mut Session) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for view in session.views.values_mut() {
            if view.wake_at.is_some() {
                view.wake_at = Some(Instant::now());
            }
        }
        session.tick();
        if session.pending_diff_source.is_none()
            && session.views.values().all(|view| view.wake_at.is_none())
        {
            break;
        }
        assert!(Instant::now() < deadline, "{}", session.message);
        thread::yield_now();
    }
}

fn wait_until(session: &mut Session, condition: impl Fn(&mut Session) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.tick();
        if condition(session) {
            return;
        }
        if Instant::now() >= deadline {
            let frame = rendered(session);
            panic!(
                "timed out waiting for an update: {}\n{frame}",
                session.message
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn rendered(session: &mut Session) -> String {
    session
        .frame()
        .rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn selected_row(session: &Session, id: ViewId) -> &DisplayRow {
    let view = &session.views[&id];
    let primary = view.location.selection.primary.as_ref().unwrap();
    view.location
        .display
        .rows
        .iter()
        .find(|row| &row.id == primary)
        .unwrap()
}

fn preview_buffer(session: &Session, source: ViewId) -> BufferId {
    let group = session.views.page(source).previews.as_ref().unwrap();
    group
        .members
        .iter()
        .find(|slot| slot.id == group.active)
        .unwrap()
        .buffer()
}

fn preview_surface(session: &Session, source: ViewId) -> &pages::PreviewSurface {
    let RenderTarget::Surface { page, slot } = session.active_preview(source).unwrap() else {
        panic!("expected a passive preview")
    };
    session.views.surface(page, &slot).unwrap()
}

fn preview_text(session: &Session, source: ViewId) -> String {
    let buffer = preview_buffer(session, source);
    let Buffer::PreviewText { text, .. } = session.buffers.get(&buffer).unwrap() else {
        panic!("expected text preview for {source}")
    };
    text.text()
}

fn view_rows(session: &mut Session, view: ViewId) -> Vec<String> {
    let (_, _, _, width, height) = panes(session)
        .into_iter()
        .find(|(id, ..)| *id == view)
        .unwrap();
    session
        .render_view(view, width, height)
        .0
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn preview_groups(session: &Session) -> impl Iterator<Item = &pages::PreviewSlots> {
    session
        .views
        .pages()
        .filter_map(|page| page.previews.as_ref())
}

fn panes(session: &Session) -> Vec<(ViewId, usize, usize, usize, usize)> {
    let mut panes = Vec::new();
    let mut pages = Vec::new();
    session.layout.as_ref().unwrap().page_rectangles(
        Rect {
            x: 0,
            y: 0,
            width: session.width as usize,
            height: session.height.saturating_sub(1) as usize,
        },
        &mut pages,
    );
    for (page, area) in pages {
        session.views.rectangles(page, area, &mut panes);
    }
    panes
        .into_iter()
        .filter_map(|(target, rect)| match target {
            RenderTarget::View(id) => Some((id, rect.x, rect.y, rect.width, rect.height)),
            RenderTarget::Surface { .. } => None,
        })
        .collect()
}

#[test]
fn system_copy_uses_the_same_command_for_default_and_custom_bindings() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    let content = "中文 clipboard\nsecond line\n";
    fs::write(&path, content).unwrap();
    let config = fixture.0.join("config.w");
    fs::write(
        &config,
        "map-modal \"text\" \"y\" \"text.yank-to-clipboard\"\n",
    )
    .unwrap();
    for custom in [false, true] {
        let mut session = Session::new(
            fixture.0.clone(),
            std::slice::from_ref(&path),
            if custom { Some(config.clone()) } else { None },
        )
        .unwrap();
        assert!(session.generation > 0, "{}", session.message);
        session.input("%");
        if !custom {
            session.input("y");
            assert!(
                !session.flush_clipboard_writes(|_| panic!("ordinary yank copied to clipboard"))
            );
            session.input("space");
            let frame = session.frame();
            assert!(frame.rows.iter().flat_map(|row| &row.spans).any(|span| {
                span.style == "popup"
                    && span.text.split_whitespace().collect::<Vec<_>>().join(" ")
                        == "y Copy selections to the system clipboard"
            }));
            let (scopes, mode) = session.key_context();
            session.scripts.as_ref().unwrap().with_keymap(|keymap| {
                let binding = keymap
                    .effective_bindings(&scopes, &mode)
                    .into_iter()
                    .find(|binding| binding.keys == ["space", "y"])
                    .unwrap();
                assert_eq!(binding.command, "text.yank-to-clipboard");
            });
        }
        session.input("y");
        let mut copies = Vec::new();
        session.flush_clipboard_writes(|text| {
            copies.push(text);
            Ok(())
        });
        assert_eq!(copies, [content]);
        session.frame();
        session.frame();
        assert!(!session.flush_clipboard_writes(|_| panic!("redraw repeated clipboard write")));
        assert!(!session.modified());
    }
}

fn input_command(session: &mut Session, command: &str) {
    session.input(":");
    for ch in command.chars() {
        session.input(&ch.to_string());
    }
    session.input("Enter");
}

#[test]
fn file_reload_and_its_alias_restore_disk_content_with_undo() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "old").unwrap();
    let mut session = fixture.session(std::slice::from_ref(&path));
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    fs::write(&path, "new").unwrap();
    session.command("rl").unwrap();
    let buffer = session.views[&session.focus].location.buffer;
    let text = session.buffers.get_mut(&buffer).unwrap().text().unwrap();
    assert_eq!(text.text(), "new");
    assert!(!text.modified());
    session.input("u");
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "!old"
    );
    assert!(session.modified());
}

#[test]
fn command_aliases_resolve_to_one_completion_entry() {
    let fixture = Fixture::new();
    let session = fixture.session(&[]);
    let candidates = session.command_candidates("rl");
    assert_eq!(candidates[0].name, "reload");
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.name == "reload")
            .count(),
        1
    );
    let candidates = session.command_candidates("");
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.name == "rl" || candidate.name == "q")
    );
}

#[test]
fn clipboard_reply_is_applied_to_the_requested_buffer() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    for key in ["\"", "*", "p"] {
        session.input(key);
    }
    let (id, primary) = session.clipboard_request().unwrap();
    assert!(primary);
    session.complete_clipboard_read(id, Some("external".into()), None);
    let buffer = session.views[&session.focus].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "external"
    );
}

#[test]
fn later_input_cancels_an_outstanding_clipboard_reply() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    for key in ["\"", "+", "p"] {
        session.input(key);
    }
    let (id, _) = session.clipboard_request().unwrap();
    session.input("i");
    session.input("x");
    session.complete_clipboard_read(id, Some("stale".into()), None);
    let buffer = session.views[&session.focus].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "x"
    );
}

#[test]
fn a_failed_scratch_save_does_not_leave_an_error_on_later_close_commands() {
    let fixture = Fixture::new();
    for command in ["q", "q!"] {
        let mut session = fixture.session(&[]);
        input_command(&mut session, "new");
        input_command(&mut session, "w");
        assert_eq!(session.message, "error: buffer has no file name");
        input_command(&mut session, command);
        assert!(!session.exiting());
        assert!(session.message.is_empty());
        assert!(session.active_prompt().is_none());
    }
}

#[test]
fn closing_a_scratch_buffer_preserves_unsaved_hidden_buffers() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source").unwrap();
    let mut session = fixture.session(&[path]);
    let original = session.views[&session.focus].location.buffer;
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    input_command(&mut session, "new");
    let scratch = session.views[&session.focus].location.buffer;
    for id in session
        .client_actions
        .iter()
        .map(|action| action.id)
        .collect::<Vec<_>>()
    {
        assert_eq!(
            session.begin_operation(
                "workspace",
                &zess_protocol::CommandAction::AcknowledgeClientAction { action_id: id }
            ),
            Some(zess_protocol::CommandOutcome::Applied)
        );
    }
    input_command(&mut session, "q");
    let action = session
        .client_actions
        .iter()
        .find(|action| matches!(action.kind, zess_protocol::ClientActionKind::Close { .. }))
        .unwrap()
        .id;
    session
        .close_client_buffer(action, scratch, Some(original), false)
        .unwrap();
    assert_eq!(session.views[&session.focus].location.buffer, original);
    assert!(session.modified());
}

#[test]
fn rejected_buffer_switches_do_not_create_buffers_inside_file_layers() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "source").unwrap();
    fs::write(fixture.0.join("other.txt"), "other").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("e");
    let layer = session.focus;
    session.command("buffers").unwrap();
    let buffers = session.frame().status;
    for command in ["new", "edit other.txt"] {
        assert!(session.command(command).is_err());
        assert_eq!(session.focus, layer);
        session.command("buffers").unwrap();
        assert_eq!(session.frame().status, buffers);
    }
    assert!(
        session
            .request(
                layer,
                HostRequest::OpenFile {
                    path: "other.txt".into(),
                    line: 0,
                },
            )
            .is_err()
    );
    assert_eq!(session.focus, layer);
    session.command("buffers").unwrap();
    assert_eq!(session.frame().status, buffers);
}

#[test]
fn draft_layers_reject_plugin_buffer_switches() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
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
    session.command("buffers").unwrap();
    let buffers = session.frame().status;
    for command in ["files", "git", "outline"] {
        assert!(session.command(command).is_err());
        assert_eq!(session.focus, layer);
        session.command("buffers").unwrap();
        assert_eq!(session.frame().status, buffers);
    }
}

#[test]
fn closing_a_parent_finishes_its_draft_layer_and_keeps_the_parent_open() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
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
    let buffer = session.views[&layer].location.buffer;
    session.focus_view(source).unwrap();
    session.command("quit").unwrap();
    assert!(!session.exiting());
    assert_eq!(session.focus, source);
    assert_eq!(session.views.values().count(), 1);
    assert_eq!(session.views.child(source), None);
    assert_eq!(session.views.parent(layer), None);
    assert!(!session.buffers.contains_key(&buffer));
    assert_eq!(
        session.views[&source].location.memories["draft:draft"]["text"],
        "draft text"
    );
}

#[test]
fn refreshing_files_updates_the_preview_without_changing_the_layout_or_selection() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "before preview").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(preview_text(&session, source), "before preview");
    let target = session.active_preview(source).unwrap();
    let selection = serde_json::to_value(&session.views[&source].location.selection).unwrap();
    let rectangles = panes(&session);
    fs::write(fixture.0.join("note.txt"), "after preview").unwrap();
    fs::write(fixture.0.join("new.txt"), "new").unwrap();
    session.input("R");
    let text = preview_text(&session, source);
    assert!(["before preview", "after preview"].contains(&text.as_str()));
    assert_eq!(session.active_preview(source).unwrap(), target);
    settle(&mut session);
    assert_eq!(preview_text(&session, source), "after preview");
    assert_eq!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["new.txt", "note.txt"]
    );
    assert_eq!(session.focus, source);
    assert_eq!(session.active_preview(source).unwrap(), target);
    assert_eq!(
        serde_json::to_value(&session.views[&source].location.selection).unwrap(),
        selection
    );
    assert_eq!(panes(&session), rectangles);
}

#[test]
fn saving_text_invalidates_cached_previews_and_directory_metadata() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "old").unwrap();
    let mut session = fixture.session(&[path.clone()]);
    let preview = session
        .services
        .borrow_mut()
        .call("fs.preview", json!({"path":"note.txt"}));
    assert_eq!(preview["text"], "old");
    session
        .services
        .borrow_mut()
        .call("fs.list", json!({"path":""}));
    let editor = session.focus;
    session.command("vsplit").unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    session.focus_view(editor).unwrap();
    for key in ["i", "n", "e", "w", "Esc"] {
        session.input(key);
    }
    session.command("write").unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "newold");
    let preview = session
        .services
        .borrow_mut()
        .call("fs.preview", json!({"path":"note.txt"}));
    assert_eq!(preview["text"], "newold");
    let listing = session
        .services
        .borrow_mut()
        .call("fs.list", json!({"path":""}));
    assert_eq!(listing["entries"][0]["size"], 6);
    settle(&mut session);
    let group = preview_groups(&session).next().unwrap();
    let buffer = group
        .members
        .iter()
        .find(|slot| slot.id == group.active)
        .unwrap()
        .buffer();
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "newold"
    );
}

#[test]
fn closing_one_file_view_keeps_the_other_views_requests_alive() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    fs::write(fixture.0.join("folder/note.txt"), "visible child").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("l");
    session.command("vsplit").unwrap();
    let clone = session.focus;
    assert_ne!(clone, source);
    session.command("quit").unwrap();
    session.focus_view(source).unwrap();
    settle(&mut session);
    assert_eq!(
        session.views[&source]
            .location
            .display
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["folder", "folder/note.txt"]
    );
    assert_eq!(
        session
            .services
            .borrow_mut()
            .call("service.stats", json!({}))["jobs"],
        0
    );
}

#[test]
fn leaving_a_file_view_releases_its_pending_subscriptions() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "preview").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    assert!(
        session
            .services
            .borrow_mut()
            .call("service.stats", json!({}))["jobs"]
            .as_u64()
            .unwrap()
            > 0
    );
    session.command("new").unwrap();
    assert_eq!(
        session
            .services
            .borrow_mut()
            .call("service.stats", json!({}))["jobs"],
        0
    );
}

#[test]
fn refreshing_files_keeps_an_open_outline_unchanged() {
    let fixture = Fixture::new();
    let path = fixture.0.join("sample.rs");
    fs::write(&path, "fn alpha() {}\n").unwrap();
    let mut session = fixture.session(&[path]);
    session.resize(160, 30);
    session.command("outline").unwrap();
    let outline = session.focus;
    let display = serde_json::to_value(&session.views[&outline].location.display).unwrap();
    session.command("vsplit").unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    assert_eq!(
        session.views[&outline]
            .location
            .display
            .rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["  function_item alpha"]
    );
    let outline_rows = view_rows(&mut session, outline);
    session.input("R");
    settle(&mut session);
    assert_eq!(
        serde_json::to_value(&session.views[&outline].location.display).unwrap(),
        display
    );
    assert_eq!(view_rows(&mut session, outline), outline_rows);
}

#[test]
fn starting_with_a_directory_opens_its_file_tree() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source\n").unwrap();
    let mut session = fixture.session(&[PathBuf::from(".")]);
    settle(&mut session);
    let view = &session.views[&session.focus];
    assert!(matches!(
        &session.buffers[&view.location.buffer],
        Buffer::Data { kind, .. } if kind == "files"
    ));
    assert_eq!(selected_row(&session, session.focus).id, "note.txt");
    let source = session.focus;
    assert_eq!(preview_text(&session, source), "source\n");

    session.input("Enter");
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("{}", session.message)
    };
    assert_eq!(text.path(), Some(path.as_path()));
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "source\n"
    );
}

#[test]
fn attaching_with_a_directory_opens_the_tree_at_that_directory() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source\n").unwrap();
    let directory = fixture.0.join("folder");
    fs::create_dir(&directory).unwrap();
    let mut session = fixture.session(&[path.clone()]);

    session.open_paths(&[directory.to_string_lossy().into_owned()]);
    settle(&mut session);
    let view = &session.views[&session.focus];
    assert!(matches!(
        &session.buffers[&view.location.buffer],
        Buffer::Data { kind, .. } if kind == "files"
    ));
    assert_eq!(selected_row(&session, session.focus).id, "folder");
    session.command("quit").unwrap();
    let buffer = session.views[&session.focus].location.buffer;
    let Buffer::Text(text) = &session.buffers[&buffer] else {
        panic!("{}", session.message)
    };
    assert_eq!(text.path(), Some(path.as_path()));
}

#[test]
fn rejecting_an_outside_directory_keeps_the_open_file_layer() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source\n").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("e");
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    let focus = session.focus;
    let buffer = session.views[&focus].location.buffer;

    session.open_paths(&[outside.0.to_string_lossy().into_owned()]);

    assert_eq!(
        session.message,
        "error: directory must remain within the session workspace: prefix not found"
    );
    assert_eq!(session.focus, focus);
    assert_eq!(session.views[&focus].location.buffer, buffer);
    assert!(session.views.parent(focus).is_some());
    let Buffer::Text(text) = session.buffers.get_mut(&buffer).unwrap() else {
        panic!("expected a text file layer")
    };
    assert_eq!(text.path(), Some(path.as_path()));
    assert_eq!(text.text(), "!source\n");
    assert!(text.modified());
}

#[test]
fn opening_files_uses_the_same_action_for_default_and_custom_bindings() {
    let scripts = Fixture::new();
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source").unwrap();
    for keys in [vec!["Enter"], vec!["g", "f"], vec!["o"]] {
        let mut session = fixture.session(&[]);
        session.command("files").unwrap();
        settle(&mut session);
        let source = session.focus;
        session
            .scripts
            .as_mut()
            .unwrap()
            .load(&scripts.write("custom-key.w", "map-modal \"files\" \"o\" \"files.open\""))
            .unwrap();
        for key in keys {
            session.input(key);
        }
        assert_eq!(session.focus, source);
        assert_eq!(session.views.values().count(), 1, "{}", session.message);
        assert!(preview_groups(&session).next().is_none());
        let buffer = session.views[&source].location.buffer;
        let Buffer::Text(text) = &session.buffers[&buffer] else {
            panic!("{}", session.message)
        };
        assert_eq!(text.path(), Some(path.as_path()));
        assert_eq!(
            session
                .buffers
                .get_mut(&buffer)
                .unwrap()
                .text()
                .unwrap()
                .text(),
            "source"
        );
    }
}

#[test]
fn leaving_a_browser_closes_its_preview_and_preserves_other_editors() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source").unwrap();
    for command in ["quit", "new", "edit note.txt"] {
        let mut session = fixture.session(&[path.clone()]);
        let editor = session.focus;
        session.command("vsplit").unwrap();
        session.command("files").unwrap();
        settle(&mut session);
        assert_eq!(session.views.values().count(), 2);
        session.command(command).unwrap();
        assert!(preview_groups(&session).next().is_none());
        assert!(
            session
                .views
                .values()
                .all(|view| !session.buffers[&view.location.buffer].is_preview())
        );
        let buffer = session.views[&editor].location.buffer;
        assert_eq!(
            session
                .buffers
                .get_mut(&buffer)
                .unwrap()
                .text()
                .unwrap()
                .text(),
            "source"
        );
    }
}

fn preview_rows(session: &mut Session, source: ViewId) -> Vec<FrameRow> {
    let target = session.active_preview(source).unwrap();
    let (_, rect) = session
        .visible_rectangles()
        .into_iter()
        .find(|(current, _)| current == &target)
        .unwrap();
    match target {
        RenderTarget::View(id) => session.render_view(id, rect.width, rect.height).0,
        RenderTarget::Surface { page, slot } => session.render_surface(page, &slot, rect),
    }
}

fn preview_row(session: &mut Session) -> String {
    let source = session.focus;
    preview_rows(session, source)[0]
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect()
}

#[test]
fn preview_horizontal_scroll_stops_at_the_content_edges() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "short\n").unwrap();
    let configuration = Fixture::new();
    let config = configuration.0.join("config.w");
    fs::write(
        &config,
        "set-view-options! \"files\" : obj 'preview-wrap #f\n",
    )
    .unwrap();
    let mut session = Session::new(fixture.0.clone(), &[], Some(config)).unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    let initial = preview_row(&mut session);
    session.input("L");
    assert_eq!(preview_row(&mut session), initial);
    session.input("H");
    assert_eq!(preview_row(&mut session), initial);
    let text = format!("\t中文{}END\n", "0123456789".repeat(6));
    fs::write(fixture.0.join("note.txt"), &text).unwrap();
    session.input("R");
    settle(&mut session);
    let initial = preview_row(&mut session);
    session.input("L");
    assert_ne!(preview_row(&mut session), initial);
    for _ in 0..100 {
        session.input("L");
    }
    let end = preview_row(&mut session);
    assert!(end.ends_with("END"), "{end:?}");
    session.input("L");
    assert_eq!(preview_row(&mut session), end);
    session.resize(200, 24);
    assert_eq!(
        preview_row(&mut session).trim_end(),
        format!("    中文{}END", "0123456789".repeat(6))
    );
    session.resize(80, 24);
    session.frame();
    session.input("L");
    fs::write(fixture.0.join("note.txt"), "short\n").unwrap();
    session.input("R");
    settle(&mut session);
    assert_eq!(preview_row(&mut session).trim_end(), "short");
}

#[test]
fn directory_preview_horizontal_scroll_stops_at_the_content_edges() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let initial = preview_row(&mut session);
    session.input("L");
    assert_eq!(preview_row(&mut session), initial);
    let name = format!("{}END", "x".repeat(50));
    fs::write(fixture.0.join("folder").join(&name), "").unwrap();
    session.input("R");
    settle(&mut session);
    for _ in 0..100 {
        session.input("L");
    }
    let end = preview_row(&mut session);
    assert!(end.ends_with("END"), "{end:?}");
    session.input("L");
    assert_eq!(preview_row(&mut session), end);
    session.input("H");
    assert_ne!(preview_row(&mut session), end);
    session.resize(200, 24);
    assert_eq!(preview_row(&mut session).trim_end(), name);
}

#[test]
fn narrow_panes_keep_mode_labels_whole_across_buffer_types() {
    let fixture = Fixture::new();
    for kind in ["text", "files", "git"] {
        let mut session = fixture.session(&[]);
        let source = session.focus;
        if kind != "text" {
            session.command(kind).unwrap();
            settle(&mut session);
            session
                .close_preview(session.views.page_id(source))
                .unwrap();
        }
        session.command("vsplit").unwrap();
        session.focus_view(source).unwrap();
        for (width, pane_width, normal, select) in [
            (10, 4, "NOR", "SEL"),
            (11, 5, " NOR ", " SEL "),
            (80, 39, " NOR ", " SEL "),
        ] {
            session.resize(width, 24);
            assert_eq!(session.focus, source);
            assert_eq!(
                panes(&session)
                    .into_iter()
                    .find(|(id, ..)| *id == source)
                    .unwrap()
                    .3,
                pane_width
            );
            let frame = session.frame();
            let label = frame
                .rows
                .last()
                .unwrap()
                .spans
                .iter()
                .find(|span| span.style == "status.normal")
                .unwrap();
            assert_eq!(label.text, normal, "{kind}: width {width}");
            session.input("v");
            let frame = session.frame();
            let label = frame
                .rows
                .last()
                .unwrap()
                .spans
                .iter()
                .find(|span| span.style == "status.select")
                .unwrap();
            assert_eq!(label.text, select, "{kind}: width {width}");
            session.input("Esc");
        }
    }
}

#[test]
fn repeated_vertical_splits_keep_mode_labels_visible_across_buffer_types() {
    let fixture = Fixture::new();
    for kind in ["text", "files", "git"] {
        let mut session = fixture.session(&[]);
        if kind != "text" {
            session.command(kind).unwrap();
            settle(&mut session);
        }
        for _ in 0..5 {
            session.command("vsplit").unwrap();
        }
        for width in [80, 60] {
            session.resize(width, 24);
            let frame = session.frame();
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
fn previews_omit_filenames_and_readonly_labels_from_the_statusline() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    for (content, mode_label) in [
        (
            PreviewContent::Text {
                text: "preview content".into(),
            },
            " NOR ",
        ),
        (
            PreviewContent::Diff {
                text: "@@ -1 +1 @@\n-before\n+after\n".into(),
            },
            " HUN ",
        ),
    ] {
        session.focus_view(source).unwrap();
        session
            .show_preview(
                session.views.page_id(source),
                "selected-file.txt".into(),
                content,
                true,
            )
            .unwrap();
        session.frame();
        let target = session.focus;
        let (_, _, _, width, height) = panes(&session)
            .into_iter()
            .find(|(id, ..)| *id == target)
            .unwrap();
        let (rows, _) = session.render_view(target, width, height);
        let status = rows
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(rows.last().unwrap().spans[0].text, mode_label);
        assert!(!status.contains("selected-file.txt"), "{status:?}");
        assert!(!status.contains("readonly"), "{status:?}");
    }
}

#[test]
fn directory_previews_show_contents_without_browser_selection_or_status() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("folder");
    fs::create_dir(&directory).unwrap();
    for index in 0..30 {
        fs::write(directory.join(format!("{index:02}.txt")), "").unwrap();
    }
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(preview_row(&mut session).trim_end(), "00.txt");
    session.command("files.preview").unwrap();
    assert_eq!(session.focus, source);
    session.input("J");
    let rows = preview_rows(&mut session, source);
    assert_eq!(rows[0].spans[0].text.trim_end(), "01.txt");
    assert_eq!(rows.last().unwrap().spans[0].text.trim_end(), "23.txt");
    for row in rows {
        for span in row.spans {
            assert!(!span.style.starts_with("selection"));
            assert!(!span.style.starts_with("status"));
            assert!(!span.style.starts_with("linenr"));
        }
    }
    let (scopes, _) = session.key_context();
    assert!(scopes.contains(&"files"));
}

#[test]
fn diff_previews_show_one_source_line_number_per_row() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    session
        .show_preview(
            session.views.page_id(source),
            "note.txt".into(),
            PreviewContent::Diff {
                text: "@@ -5,2 +8,2 @@\n context\n-before\n+after\n".into(),
            },
            false,
        )
        .unwrap();
    let target = session.preview_target(source).unwrap();
    let (_, _, _, width, height) = panes(&session)
        .into_iter()
        .find(|(id, ..)| *id == target)
        .unwrap();
    let (rows, _) = session.render_view(target, width, height);
    let lines = rows
        .iter()
        .take(4)
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        ["  @@ -5,2 +8,2 @@", "8  context", "6 -before", "9 +after"]
    );
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .all(|span| { !span.style.starts_with("selection") && span.style != "linenr.current" })
    );
    session.focus_preview(session.focus).unwrap();
    let (rows, _) = session.render_view(target, width, height);
    assert!(
        rows[2]
            .spans
            .iter()
            .any(|span| span.style == "selection.git.deleted")
    );
    assert!(
        rows[3]
            .spans
            .iter()
            .any(|span| span.style == "selection.git.added")
    );
}

#[test]
fn cloned_directory_previews_keep_scroll_on_refresh_and_reset_for_a_new_path() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
    let directory = |name: &str, count| PreviewContent::Directory {
        column_alignment: Default::default(),
        rows: (0..count)
            .map(|index| DisplayRow {
                id: format!("{name}/{index:02}"),
                text: format!("{name}{index:02}"),
                ..DisplayRow::default()
            })
            .collect(),
    };
    session
        .show_preview(
            session.views.page_id(source),
            "A".into(),
            directory("A", 40),
            false,
        )
        .unwrap();
    let target = session.preview_target(source).unwrap();
    session.frame();
    for _ in 0..8 {
        session
            .execute_command("preview.move-down", serde_json::json!({}))
            .unwrap();
    }
    session.focus_view(target).unwrap();
    session.command("vsplit").unwrap();
    let clone = session.focus;
    let first_line = |session: &mut Session, id| {
        let (_, _, _, width, height) = panes(session)
            .into_iter()
            .find(|(view, ..)| *view == id)
            .unwrap();
        session.render_view(id, width, height).0[0].spans[0]
            .text
            .trim_end()
            .to_owned()
    };
    session
        .show_preview(
            session.views.page_id(source),
            "A".into(),
            directory("A", 40),
            false,
        )
        .unwrap();
    for view in [target, clone] {
        assert_eq!(first_line(&mut session, view), "A08");
    }
    session
        .show_preview(
            session.views.page_id(source),
            "B".into(),
            directory("B", 3),
            false,
        )
        .unwrap();
    for view in [target, clone] {
        assert_eq!(first_line(&mut session, view), "B00");
    }
}

#[test]
fn directory_preview_does_not_scroll_past_the_last_visible_row() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    session.resize(80, 8);
    let source = session.focus;
    let rows = (0..12)
        .map(|index| DisplayRow {
            id: index.to_string(),
            text: format!("row{index:02}"),
            ..DisplayRow::default()
        })
        .collect();
    session
        .show_preview(
            session.views.page_id(source),
            "dir".into(),
            PreviewContent::Directory {
                rows,
                column_alignment: Default::default(),
            },
            false,
        )
        .unwrap();
    session.frame();
    let target = session.preview_target(source).unwrap();
    for _ in 0..20 {
        session
            .execute_command("preview.move-down", serde_json::json!({}))
            .unwrap();
    }
    let view = &session.views[&target];
    assert_eq!(
        view.location.scroll,
        view.location.display.rows.len() - view.viewport_height
    );
}

#[test]
fn git_file_status_colors_survive_selection() {
    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
    };
    git(&["init", "--quiet"]);
    for name in ["modified.txt", "deleted.txt", "rename-before.txt"] {
        fs::write(fixture.0.join(name), "before\n").unwrap();
    }
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Zess",
        "-c",
        "user.email=zess@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "Initial files",
    ]);
    fs::write(fixture.0.join("modified.txt"), "after\n").unwrap();
    fs::remove_file(fixture.0.join("deleted.txt")).unwrap();
    fs::write(fixture.0.join("added.txt"), "added\n").unwrap();
    fs::write(fixture.0.join("untracked.txt"), "new\n").unwrap();
    git(&["add", "added.txt"]);
    git(&["mv", "rename-before.txt", "renamed.txt"]);
    let mut session = fixture.session(&[]);
    session.resize(160, 24);
    session.command("git").unwrap();
    settle(&mut session);
    session.input("%");
    let source = session.focus;
    let (_, _, _, width, height) = panes(&session)
        .into_iter()
        .find(|(id, ..)| *id == source)
        .unwrap();
    let (rows, _) = session.render_view(source, width, height);
    for (name, style) in [
        ("added.txt", "selection.git.added"),
        ("untracked.txt", "selection.git.added"),
        ("modified.txt", "selection.modified"),
        ("renamed.txt", "selection.modified"),
        ("deleted.txt", "selection.git.deleted"),
    ] {
        let index = session.views[&source]
            .location
            .display
            .rows
            .iter()
            .position(|row| row.id == format!("file:{name}"))
            .unwrap();
        let row = &session.views[&source].location.display.rows[index];
        assert_eq!(rows[index + 1].spans[0].text.trim_end(), row.text);
        assert_eq!(rows[index + 1].spans[0].style, style, "{name}");
    }
}

#[test]
fn saving_a_fullscreen_file_layer_keeps_it_open_until_explicitly_closed() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    let original = "0123456789".repeat(6);
    fs::write(&path, &original).unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let layout = panes(&session);
    let selection = serde_json::to_value(&session.views[&source].location.selection).unwrap();
    session.input("3");
    session.input("e");
    let layer = session.focus;
    assert_ne!(layer, source, "{}", session.message);
    assert_eq!(session.views.parent(layer), Some(source));
    assert_eq!(panes(&session), layout);
    let buffer = session.views[&layer].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        original
    );
    assert!(
        session
            .frame()
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .all(|span| span.style != "window")
    );
    session.input("C-w");
    session.input("l");
    assert_eq!(session.focus, layer);
    session.command("help").unwrap();
    assert!(
        session
            .execute_command("help.pin", serde_json::json!({}))
            .is_err()
    );
    assert!(session.command("edit note.txt").is_err());
    assert!(session.help.is_some());
    assert_eq!(session.views.child(source), Some(layer));
    assert_eq!(session.views.parent(layer), Some(source));
    session
        .execute_command("help.close", serde_json::json!({}))
        .unwrap();
    assert_eq!(session.focus, layer);
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    session.command("write").unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), format!("!{original}"));
    assert_eq!(session.focus, layer);
    assert_eq!(session.views.child(source), Some(layer));
    assert_eq!(panes(&session), layout);
    session.command("quit").unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(panes(&session), layout);
    assert_eq!(
        serde_json::to_value(&session.views[&source].location.selection).unwrap(),
        selection
    );
    assert!(!session.views.contains_key(&layer));
    assert!(session.views[&source].count.is_empty());
    settle(&mut session);
    assert_eq!(preview_text(&session, source), format!("!{original}"));
}

#[test]
fn closing_a_file_layer_discards_its_edits_without_changing_the_open_file() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source").unwrap();
    let mut session = fixture.session(&[path]);
    let editor = session.focus;
    let buffer = session.views[&editor].location.buffer;
    for key in ["i", "?", "Esc"] {
        session.input(key);
    }
    session.command("vsplit").unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.input("e");
    let layer = session.focus;
    let temporary = session.views[&layer].location.buffer;
    assert_ne!(temporary, buffer);
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    assert!(session.command("quit").is_err());
    assert_eq!(session.focus, layer);
    session.command("quit!").unwrap();
    assert_eq!(session.focus, source);
    let Buffer::Text(text) = &mut session.buffers.get_mut(&buffer).unwrap() else {
        panic!()
    };
    assert_eq!(text.text(), "?source");
    assert!(text.modified());
    assert!(!session.buffers.contains_key(&temporary));
    assert_eq!(session.views[&editor].location.buffer, buffer);
    session.input("e");
    let reopened = session.views[&session.focus].location.buffer;
    let text = session.buffers.get_mut(&reopened).unwrap().text().unwrap();
    assert_eq!(text.text(), "source");
    assert!(!text.modified());
    session.command("quit").unwrap();
    session.command("buffer-next").unwrap();
}

#[test]
fn a_failed_file_layer_save_keeps_the_editor_and_its_changes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("e");
    let layer = session.focus;
    for key in ["i", "!", "Esc"] {
        session.input(key);
    }
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(session.command("write").is_err());
    assert_eq!(session.focus, layer);
    let buffer = session.views[&layer].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "!source"
    );
    assert!(session.modified());
}

#[test]
fn saving_a_draft_layer_still_returns_its_text_to_parent_memory() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let source = session.focus;
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
    session.command("write").unwrap();
    assert_eq!(session.focus, source);
    assert_eq!(
        session.views[&source].location.memories["draft:draft"]["text"],
        "draft text"
    );
    assert_eq!(session.views.values().count(), 1);
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[test]
fn git_uses_the_shared_file_layer_action_and_closes_its_diff_with_the_browser() {
    let fixture = Fixture::new();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.0)
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(fixture.0.join("note.txt"), "source").unwrap();
    let mut session = fixture.session(&[]);
    session.command("vsplit").unwrap();
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    assert_eq!(selected_row(&session, source).id, "file:note.txt");
    session.input("e");
    assert_ne!(session.focus, source, "{}", session.message);
    let buffer = session.views[&session.focus].location.buffer;
    assert_eq!(
        session
            .buffers
            .get_mut(&buffer)
            .unwrap()
            .text()
            .unwrap()
            .text(),
        "source"
    );
    session.command("quit").unwrap();
    assert_eq!(session.focus, source);
    assert!(session.views.page(source).previews.as_ref().is_some());
    session.command("quit").unwrap();
    assert!(preview_groups(&session).next().is_none());
    assert!(
        session
            .views
            .values()
            .all(|view| !session.buffers[&view.location.buffer].is_preview())
    );
}

#[test]
fn opening_git_source_finishes_browsing_even_when_the_file_already_has_a_view() {
    let fixture = Fixture::new();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.0)
        .status()
        .unwrap();
    assert!(status.success());
    let path = fixture.0.join("note.txt");
    fs::write(&path, "source").unwrap();
    for (read_diff, keys) in [
        (false, vec!["g", "f"]),
        (true, vec!["g", "f"]),
        (false, vec!["g", "f"]),
    ] {
        let mut session = fixture.session(&[path.clone()]);
        let editor = session.focus;
        let buffer = session.views[&editor].location.buffer;
        session.command("vsplit").unwrap();
        session.command("git").unwrap();
        settle(&mut session);
        let browser = session.focus;
        if read_diff {
            session.command("git.preview").unwrap();
            assert_ne!(session.focus, browser);
        }
        for key in keys {
            session.input(key);
        }
        assert_eq!(session.focus, browser, "{}", session.message);
        assert_eq!(session.views[&editor].location.buffer, buffer);
        assert_eq!(session.views[&browser].location.buffer, buffer);
        assert_eq!(session.views.values().count(), 2);
        assert!(preview_groups(&session).next().is_none());
        assert!(
            session
                .views
                .values()
                .all(|view| !session.buffers[&view.location.buffer].is_preview())
        );
    }
}

#[test]
fn external_file_changes_refresh_browsing_without_disrupting_selection_or_layout() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("folder")).unwrap();
    let path = fixture.0.join("folder/note.txt");
    fs::write(&path, "before preview").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    session.input("l");
    settle(&mut session);
    session.input("j");
    settle(&mut session);
    let source = session.focus;
    assert_eq!(preview_text(&session, source), "before preview");
    let target = session.active_preview(source).unwrap();
    let selection = serde_json::to_value(&session.views[&source].location.selection).unwrap();
    assert_eq!(
        session.views[&source].location.selection.primary.as_deref(),
        Some("folder/note.txt")
    );
    let rectangles = panes(&session);

    fs::write(&path, "after! preview").unwrap();
    fs::write(fixture.0.join("folder/added.txt"), "added").unwrap();
    wait_until(&mut session, |session| {
        preview_text(session, source) == "after! preview"
            && session.views[&source]
                .location
                .display
                .rows
                .iter()
                .any(|row| row.id == "folder/added.txt")
    });

    assert_eq!(session.focus, source);
    assert_eq!(session.active_preview(source).unwrap(), target);
    assert_eq!(
        serde_json::to_value(&session.views[&source].location.selection).unwrap(),
        selection
    );
    assert_eq!(panes(&session), rectangles);
}

#[test]
fn an_external_commit_removes_committed_changes_and_clears_the_diff() {
    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Zess Tests",
                "-c",
                "user.email=zess@example.invalid",
            ])
            .args(args)
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    let path = fixture.0.join("note.txt");
    fs::write(&path, "before baseline\n").unwrap();
    git(&["add", "note.txt"]);
    git(&["commit", "--quiet", "-m", "baseline"]);
    fs::write(&path, "after commit\n").unwrap();
    git(&["add", "note.txt"]);
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    settle(&mut session);
    let source = session.focus;
    let target = session.preview_target(source).unwrap();
    let buffer = session.views[&target].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected staged diff")
    };
    assert_eq!(
        diff.raw.lines().skip(4).collect::<Vec<_>>(),
        ["@@ -1 +1 @@", "-before baseline", "+after commit"]
    );
    let target = session.preview_target(source).unwrap();
    let rectangles = panes(&session);
    assert_eq!(selected_row(&session, source).id, "file:note.txt");

    git(&["commit", "--quiet", "-m", "external commit"]);
    wait_until(&mut session, |session| {
        let target = session.preview_target(source).unwrap();
        let buffer = session.views[&target].location.buffer;
        session.views[&source].location.display.rows.is_empty()
            && matches!(&session.buffers[&buffer], Buffer::PreviewRows {
                kind: PreviewRowKind::Directory, path, display,
            } if path.is_empty() && display.rows.is_empty())
    });

    assert_eq!(fs::read_to_string(&path).unwrap(), "after commit\n");
    assert!(session.views[&source].location.selection.primary.is_none());
    assert!(
        session.views[&source]
            .location
            .selection
            .selected
            .is_empty()
    );
    assert_eq!(session.focus, source);
    assert_eq!(session.preview_target(source).unwrap(), target);
    assert_eq!(panes(&session), rectangles);
}

#[test]
fn file_browser_observations_are_shared_until_the_last_view_leaves() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("note.txt"), "preview").unwrap();
    let mut session = fixture.session(&[]);
    let stats = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 0);
    assert_eq!(stats["watched_paths"], 0);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    let active = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(active["observed_queries"], 3);
    assert!(active["watched_paths"].as_u64().unwrap() > 0);

    session.command("vsplit").unwrap();
    settle(&mut session);
    let cloned = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(cloned["observed_queries"], active["observed_queries"]);
    assert_eq!(cloned["watched_paths"], active["watched_paths"]);
    session.command("quit").unwrap();
    session.focus_view(source).unwrap();
    let remaining = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(remaining["observed_queries"], active["observed_queries"]);
    assert_eq!(remaining["watched_paths"], active["watched_paths"]);
    session.command("new").unwrap();
    let stopped = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(stopped["observed_queries"], 0);
    assert_eq!(stopped["watched_paths"], 0);
    assert_eq!(stopped["jobs"], 0);
}

#[test]
fn leaving_files_releases_browser_observations_and_reopening_reads_current_contents() {
    let fixture = Fixture::new();
    let path = fixture.0.join("note.txt");
    fs::write(&path, "old preview").unwrap();
    let mut session = fixture.session(&[]);
    session.command("files").unwrap();
    settle(&mut session);
    let source = session.focus;
    session.command("new").unwrap();
    assert!(!session.views.page(source).previews.as_ref().is_some());
    let stats = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 0);
    assert_eq!(stats["jobs"], 0);

    fs::write(&path, "new preview").unwrap();
    session.command("files").unwrap();
    settle(&mut session);
    session.command("files.preview").unwrap();
    settle(&mut session);
    assert_eq!(preview_text(&session, source), "new preview");
    assert_eq!(session.focus, source);
    assert!(session.views.page(source).previews.as_ref().is_some());
    let stats = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 3);
}

#[test]
fn closing_one_git_view_keeps_the_other_views_requests_and_observations_alive() {
    let fixture = Fixture::new();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.0)
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(fixture.0.join("note.txt"), "git preview").unwrap();
    let mut session = fixture.session(&[]);
    session.command("git").unwrap();
    let source = session.focus;
    session.command("vsplit").unwrap();
    session.command("quit").unwrap();
    session.focus_view(source).unwrap();
    settle(&mut session);
    let target = session.preview_target(source).unwrap();
    let buffer = session.views[&target].location.buffer;
    let Buffer::PreviewDiff { diff, .. } = &session.buffers[&buffer] else {
        panic!("expected untracked diff")
    };
    assert_eq!(
        diff.raw.lines().skip(5).collect::<Vec<_>>(),
        [
            "@@ -0,0 +1 @@",
            "+git preview",
            "\\ No newline at end of file"
        ]
    );
    assert_eq!(selected_row(&session, source).id, "file:note.txt");
    session.command("new").unwrap();
    let stats = session
        .services
        .borrow_mut()
        .call("service.stats", json!({}));
    assert_eq!(stats["observed_queries"], 0);
    assert_eq!(stats["watched_paths"], 0);
    assert_eq!(stats["jobs"], 0);
}
