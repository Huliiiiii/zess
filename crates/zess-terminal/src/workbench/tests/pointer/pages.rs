mod files_cache;

use zess_plugin::{
    ItemNavigation, ItemSelection, PluginOutput, PluginRegistrar, PreviewContent, PreviewLayout,
    PreviewPane,
};
use zess_protocol::{
    ClientMessage,
    resource::{PageInput, PageMetadata, ResourceEvent, ResourceRequest},
    workspace_view::pages::PagePosition,
};
use zess_ui::{Display, DisplayRow};

use super::*;

fn item(id: &str) -> DisplayRow {
    DisplayRow {
        id: id.into(),
        text: id.into(),
        ..Default::default()
    }
}

fn single_selection(id: &str) -> ItemSelection {
    ItemSelection {
        primary: Some(id.into()),
        selected: vec![id.into()],
        anchor: Some(id.into()),
    }
}

fn preview(content: PreviewContent) -> HostRequest {
    HostRequest::PreviewGroup {
        panes: vec![PreviewPane::Surface {
            id: "main".into(),
            path: "preview.txt".into(),
            header: String::new(),
            loading: false,
            content,
        }],
        layout: PreviewLayout::Rows,
        focus: false,
    }
}

fn text_preview(text: String) -> HostRequest {
    preview(PreviewContent::Text { text })
}

impl Terminal {
    fn open_files(&mut self, rows: Vec<DisplayRow>, requests: Vec<HostRequest>) -> u64 {
        self.open_files_with_options(
            rows,
            requests,
            json!({"double-click-command": "files.toggle"}),
        )
    }

    fn open_files_with_options(
        &mut self,
        rows: Vec<DisplayRow>,
        requests: Vec<HostRequest>,
        options: Value,
    ) -> u64 {
        self.workbench
            .apply_request(
                HostRequest::OpenBuffer {
                    kind: "files".into(),
                    argument: json!({}),
                },
                &self.state,
            )
            .unwrap();
        let subscription = self.workbench.pages.subscription().unwrap();
        let selection = single_selection(&rows[0].id);
        self.publish_with_options(
            subscription,
            0,
            PluginOutput {
                display: Some(Display {
                    rows,
                    ..Default::default()
                }),
                selection: Some(selection),
                requests,
                ..Default::default()
            },
            options,
        );
        subscription
    }

    fn publish(&mut self, subscription_id: u64, generation: u64, output: PluginOutput) {
        self.publish_with_options(
            subscription_id,
            generation,
            output,
            json!({"double-click-command": "files.toggle"}),
        );
    }

    fn publish_with_options(
        &mut self,
        subscription_id: u64,
        generation: u64,
        output: PluginOutput,
        options: Value,
    ) {
        self.workbench
            .apply_resource(
                ResourceUpdate {
                    cache: Default::default(),
                    kind: Default::default(),
                    subscription_id,
                    generation,
                    document_scoped: false,
                    document: None,
                    patch_revision: None,
                    metadata: Some(PageMetadata {
                        view_id: 2,
                        buffer_id: 8,
                        options,
                    }),
                    output,
                },
                &self.state,
            )
            .unwrap();
    }

    fn page(&self, view_id: u64) -> PagePosition {
        let pane = self
            .workbench
            .workspace_view(&self.state)
            .pages
            .into_iter()
            .find(|pane| pane.view_id == view_id)
            .unwrap();
        pane.pages.stack.into_iter().last().unwrap()
    }

    fn active_page(&self) -> PagePosition {
        self.page(self.active_view())
    }

    fn updates(&mut self, subscription: u64) -> Vec<(ResourceEvent, PageInput)> {
        self.workbench
            .pages
            .take_requests()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Resource {
                    request:
                        ResourceRequest::Update {
                            subscription_id,
                            input,
                            event,
                        },
                } if subscription_id == subscription => Some((event, input)),
                _ => None,
            })
            .collect()
    }

    fn last_update(&mut self, subscription: u64) -> (ResourceEvent, PageInput) {
        self.updates(subscription).pop().unwrap()
    }
}

#[test]
fn dragging_file_rows_selects_their_ids_across_the_row_width() {
    let mut terminal = Terminal::document("source\n");
    let subscription = terminal.open_files(
        ["a.txt", "b.txt", "c.txt"].into_iter().map(item).collect(),
        vec![],
    );
    let frame = terminal.render();
    let (x, y) = cell(&frame, "a.txt");
    terminal.mouse(MouseKind::Press, (x + 20, y));
    let frame = terminal.render();
    terminal.mouse(MouseKind::Drag, cell(&frame, "c.txt"));

    let (event, input) = terminal.last_update(subscription);
    assert!(matches!(
        event,
        ResourceEvent::Select {
            navigation: ItemNavigation::Jump
        }
    ));
    assert_eq!(
        input.selection,
        ItemSelection {
            primary: Some("c.txt".into()),
            selected: ["a.txt", "b.txt", "c.txt"].map(String::from).to_vec(),
            anchor: Some("a.txt".into()),
        }
    );
}

#[test]
fn double_clicking_file_rows_sends_the_toggle_command_for_the_clicked_item() {
    let mut terminal = Terminal::document("source\n");
    let mut registry = Registry::<()>::default();
    registry.buffer_command("files.toggle", "Toggle folder", "files", "toggle");
    terminal
        .workbench
        .editor
        .set_catalog(registry.binding_catalog());
    let subscription = terminal.open_files(
        ["other.txt", "folder", "note.txt"]
            .into_iter()
            .map(item)
            .collect(),
        vec![],
    );
    for target in ["folder", "note.txt"] {
        let frame = terminal.render();
        let position = cell(&frame, target);
        assert!(terminal.mouse(MouseKind::Press, position).is_empty());
        terminal.render();
        let effects = terminal.mouse(MouseKind::DoubleClick, position);
        assert!(matches!(
            effects.as_slice(),
            [Effect::Command(CommandAction::Invoke {
                target: CommandTarget::Page { subscription_id, input },
                name,
                ..
            })] if *subscription_id == subscription && name == "files.toggle"
                && input.selection == single_selection(target)
        ));
        let (_, input) = terminal.last_update(subscription);
        assert_eq!(input.selection, single_selection(target));
    }
}

#[test]
fn double_clicking_a_file_moves_down_in_the_clicked_pane() {
    let mut terminal = Terminal::document("source\n");
    terminal.bindings(&[("files", "normal", &["j"], "data.move-down")]);
    terminal.open_files_with_options(
        ["a.txt", "b.txt", "c.txt"].into_iter().map(item).collect(),
        vec![],
        json!({"double-click-command": "data.move-down"}),
    );
    let first = terminal.active_view();
    terminal.native("vsplit");
    let second = terminal.active_view();
    let frame = terminal.render();

    assert!(
        terminal
            .mouse(MouseKind::DoubleClick, cell(&frame, "b.txt"))
            .is_empty()
    );
    assert_eq!(terminal.active_view(), first);
    assert_eq!(terminal.active_page().selection, single_selection("c.txt"));
    assert_eq!(terminal.page(second).selection, single_selection("a.txt"));
}

#[test]
fn double_clicking_a_file_can_open_the_page_filter() {
    let mut terminal = Terminal::document("source\n");
    terminal.bindings(&[("files", "normal", &["/"], "data.filter")]);
    terminal.open_files_with_options(
        ["alpha.txt", "beta.txt"].into_iter().map(item).collect(),
        vec![],
        json!({"double-click-command": "data.filter"}),
    );
    let frame = terminal.render();

    assert!(
        terminal
            .mouse(MouseKind::DoubleClick, cell(&frame, "beta.txt"))
            .is_empty()
    );
    assert!(terminal.workbench.editor.external_prompt());
    assert_eq!(
        terminal.active_page().selection,
        single_selection("beta.txt")
    );
}

#[test]
fn clicking_a_file_requests_its_preview_as_a_jump() {
    let mut terminal = Terminal::document("source\n");
    let subscription = terminal.open_files(
        ["a.txt", "b.txt", "c.txt"].into_iter().map(item).collect(),
        vec![text_preview("alpha preview".into())],
    );
    let frame = terminal.render();
    cell(&frame, "alpha preview");
    terminal.mouse(MouseKind::Press, cell(&frame, "c.txt"));
    let (event, input) = terminal.last_update(subscription);
    assert!(matches!(
        event,
        ResourceEvent::Select {
            navigation: ItemNavigation::Jump
        }
    ));
    assert_eq!(input.selection.primary.as_deref(), Some("c.txt"));
    terminal.publish(
        subscription,
        input.generation,
        PluginOutput {
            requests: vec![text_preview("charlie preview".into())],
            ..Default::default()
        },
    );
    cell(&terminal.render(), "charlie preview");
}

fn file_rows() -> Vec<DisplayRow> {
    (0..40)
        .map(|line| item(&format!("file-{line:02}.txt")))
        .collect()
}

fn file_list() -> Terminal {
    let mut terminal = Terminal::document("source\n");
    terminal.height = 22;
    terminal.open_files(file_rows(), vec![]);
    terminal
}

#[test]
fn repeated_file_list_wheel_ticks_preserve_selection_across_refreshes() {
    let mut terminal = file_list();
    let active = terminal.active_view();
    let frame = terminal.render();
    let target = cell(&frame, "file-00.txt");
    let subscription = terminal.workbench.pages.subscription().unwrap();
    for visible in ["file-03.txt", "file-06.txt"] {
        terminal.mouse(MouseKind::ScrollDown, target);
        terminal.native("data.refresh");
        let (event, input) = terminal.last_update(subscription);
        assert!(matches!(event, ResourceEvent::Refresh));
        terminal.publish(
            subscription,
            input.generation,
            PluginOutput {
                display: Some(Display {
                    rows: file_rows(),
                    ..Default::default()
                }),
                selection: Some(single_selection("file-00.txt")),
                ..Default::default()
            },
        );
        assert_eq!(cell(&terminal.render(), visible), target);
        assert_eq!(
            terminal.active_page().selection,
            single_selection("file-00.txt")
        );
        assert_eq!(terminal.active_view(), active);
    }
}

#[test]
fn wheel_scrolling_at_the_end_respects_the_file_list_limit() {
    let mut terminal = file_list();
    terminal.render();
    for _ in 0..4 {
        terminal.native("data.page-half-down");
    }
    assert_eq!(
        terminal.active_page().selection.primary.as_deref(),
        Some("file-39.txt")
    );
    let frame = terminal.render();
    let target = cell(&frame, "file-34.txt");
    assert_eq!(target.1, 0);
    for (kind, visible) in [
        (MouseKind::ScrollDown, "file-34.txt"),
        (MouseKind::ScrollUp, "file-31.txt"),
        (MouseKind::ScrollDown, "file-34.txt"),
    ] {
        terminal.mouse(kind, target);
        assert_eq!(cell(&terminal.render(), visible), target);
        assert_eq!(
            terminal.active_page().selection,
            single_selection("file-39.txt")
        );
    }
}

#[test]
fn filtering_after_a_wheel_tick_keeps_the_matching_file_visible() {
    let mut terminal = file_list();
    terminal.bindings(&[
        ("files", "normal", &["/"], "data.filter"),
        ("global", "prompt", &["Enter"], "prompt.submit"),
    ]);
    let subscription = terminal.workbench.pages.subscription().unwrap();
    let frame = terminal.render();
    let target = cell(&frame, "file-00.txt");
    terminal.mouse(MouseKind::ScrollDown, target);
    assert_eq!(cell(&terminal.render(), "file-03.txt"), target);

    for key in ["/", "3", "9", "Enter"] {
        terminal.key(key);
    }
    let (_, input) = terminal.last_update(subscription);
    assert_eq!(input.filter.unwrap().text, "39");
    terminal.publish(
        subscription,
        input.generation,
        PluginOutput {
            display: Some(Display {
                rows: vec![item("file-39.txt")],
                ..Default::default()
            }),
            selection: Some(single_selection("file-39.txt")),
            ..Default::default()
        },
    );
    let frame = terminal.render();
    let (_, row) = cell(&frame, "file-39.txt");
    assert!(
        frame.rows[usize::from(row)]
            .spans
            .iter()
            .any(|span| span.text.trim() == "file-39.txt" && span.style.starts_with("selection"))
    );
}

#[test]
fn preview_gestures_are_owned_by_the_page_under_the_pointer() {
    let mut terminal = Terminal::document("source\n");
    terminal.width = 160;
    terminal.height = 16;
    let preview = (0..60)
        .map(|line| format!("preview row {line:02}\n"))
        .collect::<String>();
    let left_subscription = terminal.open_files(
        ["a.txt", "b.txt"].into_iter().map(item).collect(),
        vec![text_preview(preview)],
    );
    terminal.render();
    let left = terminal.active_view();
    terminal.native("vsplit");
    let right = terminal.active_view();
    assert_ne!(left, right);
    let frame = terminal.render();
    let surface = cell(&frame, "preview row 00");
    let selection = terminal.page(left).selection;

    for scroll in [3, 6, 9] {
        terminal.mouse(MouseKind::ScrollDown, surface);
        let frame = terminal.render();
        assert_eq!(cell(&frame, &format!("preview row {scroll:02}")), surface);
        assert_eq!(terminal.active_view(), right);
        assert_eq!(terminal.page(left).selection, selection);
        assert_eq!(terminal.page(right).preview.unwrap().scroll, 0);
    }
    terminal.mouse(MouseKind::Press, (surface.0, surface.1 + 1));
    terminal.render();
    assert_eq!(terminal.active_view(), left);
    terminal.mouse(MouseKind::Drag, (surface.0, surface.1 + 4));
    terminal.render();
    assert!(
        terminal
            .mouse(MouseKind::DoubleClick, (surface.0, surface.1 + 2))
            .is_empty()
    );
    terminal.render();
    terminal.mouse(MouseKind::Press, (u16::MAX, u16::MAX));
    let frame = terminal.render();
    assert_eq!(terminal.active_view(), left);
    assert_eq!(terminal.page(left).selection, selection);
    assert!(!terminal.page(left).preview.unwrap().focused);
    assert!(
        terminal
            .workbench
            .workspace_view(&terminal.state)
            .page_document
            .is_none()
    );

    terminal.mouse(MouseKind::Press, cell(&frame, "b.txt"));
    let (event, input) = terminal.last_update(left_subscription);
    assert!(matches!(
        event,
        ResourceEvent::Select {
            navigation: ItemNavigation::Jump
        }
    ));
    assert_eq!(input.selection, single_selection("b.txt"));
    terminal.publish(
        left_subscription,
        input.generation,
        PluginOutput {
            requests: vec![text_preview("replacement preview".into())],
            ..Default::default()
        },
    );
    let frame = terminal.render();
    let replacement = cell(&frame, "replacement preview");
    let original = cell(&frame, "preview row 00");
    assert!(replacement.0 < original.0);
    assert_eq!(terminal.page(right).selection, single_selection("a.txt"));
}

fn browser_with_bindings() -> (Terminal, u64) {
    let mut terminal = Terminal::document("source\n");
    let subscription = terminal.open_files(
        ["a.txt", "b.txt", "c.txt", "d.txt"]
            .into_iter()
            .map(item)
            .collect(),
        vec![text_preview("preview content".into())],
    );
    terminal.bindings(&[
        ("files", "normal", &["C-w", "l"], "focus-right"),
        ("files", "normal", &["\""], "data.select-register"),
        ("files", "normal", &["g", "w"], "data.goto-word"),
        ("files", "normal", &["j"], "data.move-down"),
    ]);
    (terminal, subscription)
}

#[test]
fn clicking_a_preview_clears_pending_browser_input_before_the_next_key() {
    for press in [MouseKind::Press, MouseKind::DoubleClick] {
        for prefix in [&["C-w"][..], &["3"], &["\""], &["g", "w"]] {
            let (mut terminal, _) = browser_with_bindings();
            terminal.render();
            for key in prefix {
                terminal.key(key);
            }
            let frame = terminal.render();
            assert!(!frame.prompt.is_empty(), "{prefix:?}");
            terminal.mouse(press, cell(&frame, "preview content"));
            assert_eq!(terminal.active_page().selection, single_selection("a.txt"));
            terminal.key("j");
            assert_eq!(
                terminal.active_page().selection,
                single_selection("b.txt"),
                "{press:?}, {prefix:?}"
            );
        }
    }
}

#[test]
fn clicking_a_file_row_discards_pending_and_selected_registers() {
    for (keys, register) in [(&["3", "\""][..], None), (&["3", "\"", "*"], Some('*'))] {
        let (mut terminal, subscription) = browser_with_bindings();
        terminal.render();
        terminal.updates(subscription);
        for key in keys {
            terminal.key(key);
        }
        if register.is_some() {
            let (event, input) = terminal.last_update(subscription);
            assert!(matches!(event, ResourceEvent::Register));
            assert_eq!(input.register, register);
        }
        let frame = terminal.render();
        terminal.mouse(MouseKind::Press, cell(&frame, "a.txt"));
        let (event, input) = terminal.last_update(subscription);
        assert!(matches!(event, ResourceEvent::Select { .. }));
        assert_eq!(input.register, None);
        terminal.key("j");
        assert_eq!(terminal.active_page().selection, single_selection("b.txt"));
    }
}

#[test]
fn narrow_preview_rows_keep_joined_emoji_whole() {
    let mut terminal = Terminal::document("source\n");
    terminal.width = 15;
    terminal.open_files(
        vec![item("folder")],
        vec![preview(PreviewContent::Directory {
            column_alignment: Default::default(),
            rows: vec![item("a👩‍👩‍👧‍👦bcdef")],
        })],
    );
    let frame = terminal.render();
    let (_, row) = cell(&frame, "a👩‍👩‍👧‍👦bcde");
    assert!(row_text(&frame.rows[usize::from(row)]).ends_with("a👩‍👩‍👧‍👦bcde"));
}
