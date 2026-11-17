mod pages;

use zess_plugin::{CommandImplementation, Registry};
use zess_protocol::{
    BufferSummary, ClientWorkspace, TextMetrics, TextRange, ViewContent, document::DocumentSnapshot,
};
use zess_ui::{Frame, FrameRow, MouseKind, fitting_prefix};

use super::*;

struct Terminal {
    workbench: Workbench,
    state: SessionSnapshot,
    width: u16,
    height: u16,
}

impl Terminal {
    fn document(text: &str) -> Self {
        let state = SessionSnapshot {
            workspace: ClientWorkspace {
                current: Some(7),
                buffers: vec![7],
            },
            focused_view: Some(1),
            views: vec![ViewSnapshot {
                view_id: 1,
                buffer_id: 7,
                kind: "text".into(),
                title: "note.txt".into(),
                header: String::new(),
                tabs: vec![],
                loading: false,
                column_alignment: Default::default(),
                version: Some(1),
                mode: "normal".into(),
                content: ViewContent::Text { lines: vec![] },
                selection: ViewSelection::Text {
                    ranges: vec![TextRange { anchor: 0, head: 1 }],
                    primary: 0,
                },
                scroll: 0,
                horizontal_scroll: 0,
                text_metrics: Some(TextMetrics {
                    line_count: text.lines().count() + 1,
                    tab_width: 4,
                }),
                statusline: None,
                options: None,
                key_scopes: vec!["global".into(), "text".into()],
            }],
            buffers: vec![BufferSummary {
                id: 7,
                kind: BufferKind::Text { path: None },
                modified: false,
            }],
            ..Default::default()
        };
        let mut workbench = Workbench::default();
        workbench.synchronize(&state);
        workbench
            .apply_document(
                DocumentEvent::Snapshot(DocumentSnapshot {
                    buffer_id: 7,
                    version: 1,
                    text: text.into(),
                    path: None,
                    language: None,
                    modified: false,
                    readonly: false,
                    history: Default::default(),
                }),
                &state,
            )
            .unwrap();
        Self {
            workbench,
            state,
            width: 80,
            height: 24,
        }
    }

    fn render(&mut self) -> Frame {
        self.workbench
            .render(&self.state, self.width, self.height)
            .unwrap()
    }

    fn mouse(&mut self, kind: MouseKind, (column, row): (u16, u16)) -> Vec<Effect> {
        self.workbench
            .mouse(
                &MouseEvent { kind, column, row },
                &self.state,
                self.width,
                self.height,
            )
            .unwrap()
    }

    fn key(&mut self, key: &str) -> Vec<Effect> {
        self.workbench.key(key, &self.state, self.width).unwrap()
    }

    fn native(&mut self, name: &str) {
        self.workbench
            .native(name, json!({}), &self.state, &mut Vec::new())
            .unwrap();
    }

    fn bindings(&mut self, bindings: &[(&str, &str, &[&str], &str)]) {
        let mut registry = Registry::<()>::default();
        for &(scope, mode, keys, command) in bindings {
            registry.register_action(command, command, CommandImplementation::Native);
            registry.keymap.map(
                scope,
                mode,
                keys.iter().map(|key| (*key).into()).collect(),
                Some(command.into()),
            );
        }
        self.workbench
            .editor
            .set_catalog(registry.binding_catalog());
    }

    fn active_view(&self) -> u64 {
        self.workbench.active_pane().unwrap().view_id
    }

    fn selection(&self) -> (usize, usize) {
        let cursor = &self
            .workbench
            .documents
            .text(7)
            .unwrap()
            .selections(self.active_view())[0];
        (cursor.anchor_column, cursor.column)
    }
}

fn row_text(row: &FrameRow) -> String {
    row.spans.iter().map(|span| span.text.as_str()).collect()
}

fn cell(frame: &Frame, text: &str) -> (u16, u16) {
    for (row, line) in frame.rows.iter().enumerate() {
        let line = row_text(line);
        if let Some(offset) = line.find(text) {
            let (_, width) = fitting_prefix(std::iter::once(&line[..offset]), usize::MAX);
            return (width as u16, row as u16);
        }
    }
    panic!("no rendered cell for {text:?}");
}

#[test]
fn clicks_follow_rendered_graphemes_after_horizontal_scrolling() {
    let mut terminal = Terminal::document("\t界a\u{301}🙂Z0123456789\n");
    terminal.width = 10;
    terminal.height = 8;
    let frame = terminal.render();
    let (x, y) = cell(&frame, "界");
    terminal.mouse(MouseKind::Press, (x + 1, y));
    assert_eq!(terminal.selection(), (1, 1));

    let frame = terminal.render();
    let (x, y) = cell(&frame, "a\u{301}");
    terminal.mouse(MouseKind::Press, (x, y));
    assert_eq!(terminal.selection(), (2, 2));

    let view = terminal.active_view();
    let mut saved = terminal.workbench.workspace_view(&terminal.state);
    saved.documents[0].state.horizontal_scroll = 4;
    terminal.workbench.restore(&saved, &terminal.state).unwrap();
    terminal
        .workbench
        .apply_document(
            DocumentEvent::Snapshot(DocumentSnapshot {
                buffer_id: 7,
                version: 1,
                text: "\t界a\u{301}🙂Z0123456789\n".into(),
                path: None,
                language: None,
                modified: false,
                readonly: false,
                history: Default::default(),
            }),
            &terminal.state,
        )
        .unwrap();
    let frame = terminal.render();
    assert_eq!(cell(&frame, "界").0, 3);
    terminal.mouse(MouseKind::Press, cell(&frame, "界"));
    assert_eq!(terminal.active_view(), view);
    assert_eq!(terminal.selection(), (1, 1));
}

#[test]
fn pressing_or_double_clicking_text_starts_a_drag_that_ends_on_release() {
    for press in [MouseKind::Press, MouseKind::DoubleClick] {
        let mut terminal = Terminal::document("abcdef\n");
        let frame = terminal.render();
        terminal.mouse(press, cell(&frame, "b"));
        let frame = terminal.render();
        terminal.mouse(MouseKind::Drag, cell(&frame, "e"));
        assert_eq!(terminal.selection(), (1, 4));

        let frame = terminal.render();
        terminal.mouse(MouseKind::Release, cell(&frame, "e"));
        terminal.mouse(MouseKind::Drag, cell(&frame, "f"));
        assert_eq!(terminal.selection(), (1, 4));
    }
}

#[test]
fn wheel_scrolls_the_hovered_document_without_moving_focus() {
    let text = (0..50)
        .map(|line| format!("row {line:02}\n"))
        .collect::<String>();
    let mut terminal = Terminal::document(&text);
    let hovered = terminal.active_view();
    terminal.native("vsplit");
    let focused = terminal.active_view();
    assert_ne!(focused, hovered);
    let frame = terminal.render();
    let target = cell(&frame, "row 00");

    for visible in ["row 03", "row 06"] {
        terminal.mouse(MouseKind::ScrollDown, target);
        let frame = terminal.render();
        assert_eq!(cell(&frame, visible), target);
        assert_eq!(terminal.active_view(), focused);
    }
    terminal.mouse(MouseKind::Press, target);
    assert_eq!(terminal.active_view(), hovered);
    let cursor = &terminal
        .workbench
        .documents
        .text(7)
        .unwrap()
        .selections(hovered)[0];
    assert_eq!(cursor.row, 6);
}

#[test]
fn mouse_settings_from_the_host_update_the_existing_document_view() {
    let text = (0..30)
        .map(|line| format!("row {line:02}\n"))
        .collect::<String>();
    let mut terminal = Terminal::document(&text);
    let view = terminal.active_view();
    let default = terminal.render();
    assert!(default.mouse_enabled);
    let target = cell(&default, "row 00");
    terminal.mouse(MouseKind::Press, (target.0 + 2, target.1));
    assert_eq!(terminal.selection(), (2, 2));

    terminal.state.editor_options.mouse = false;
    assert!(!terminal.render().mouse_enabled);
    terminal.mouse(MouseKind::Press, (target.0 + 3, target.1));
    assert_eq!(terminal.selection(), (2, 2));

    let mut reloaded = terminal.state.clone();
    reloaded.editor_options.mouse = true;
    reloaded.editor_options.scroll_lines = 5;
    zess_protocol::SessionDelta::between(&terminal.state, &reloaded).apply(&mut terminal.state);
    assert!(terminal.render().mouse_enabled);
    terminal.mouse(MouseKind::ScrollDown, target);
    assert_eq!(cell(&terminal.render(), "row 05"), target);
    assert_eq!(terminal.active_view(), view);
    assert_eq!(terminal.selection(), (2, 2));
}

#[test]
fn clicks_in_insert_mode_place_text_at_the_clicked_character() {
    let mut terminal = Terminal::document("abcdef\n");
    terminal.bindings(&[("text", "normal", &["i"], "text.insert")]);
    terminal.key("i");
    let frame = terminal.render();
    terminal.mouse(MouseKind::Press, cell(&frame, "c"));
    terminal.key("X");
    assert_eq!(
        terminal.workbench.documents.text(7).unwrap().text(),
        "abXcdef\n"
    );
}

#[test]
fn headers_gutters_and_status_lines_do_not_move_the_text_selection() {
    let mut terminal = Terminal::document("abcdef\n");
    terminal.state.views[0].header = "Header".into();
    let frame = terminal.render();
    let (x, y) = cell(&frame, "abcdef");
    let before = terminal.selection();
    for target in [cell(&frame, "Header"), (x - 1, y), (x, frame.height - 2)] {
        terminal.mouse(MouseKind::Press, target);
        assert_eq!(terminal.selection(), before);
    }
    terminal.mouse(MouseKind::Press, (x + 2, y));
    assert_eq!(terminal.selection(), (2, 2));
}

#[test]
fn double_clicks_run_the_clicked_document_views_command() {
    let mut terminal = Terminal::document("abcdef\n");
    terminal.bindings(&[("text", "normal", &["l"], "text.move-right")]);
    terminal.state.views[0].options = Some(json!({"double-click-command": "text.move-right"}));
    let clicked = terminal.active_view();
    terminal.native("vsplit");
    assert_ne!(terminal.active_view(), clicked);
    let frame = terminal.render();
    terminal.mouse(MouseKind::DoubleClick, cell(&frame, "abcdef"));
    assert_eq!(terminal.active_view(), clicked);
    assert_eq!(terminal.selection(), (1, 1));
}

fn prompt_terminal() -> Terminal {
    let mut terminal = Terminal::document("abcdef\n");
    let mut registry = Registry::<()>::default();
    for command in [
        "command-prompt",
        "prompt.submit",
        "prompt.complete-next",
        "text.search-forward",
    ] {
        registry.register_action(command, command, CommandImplementation::Native);
    }
    for (scope, mode, key, command) in [
        ("global", "normal", ":", "command-prompt"),
        ("global", "prompt", "Enter", "prompt.submit"),
        ("global", "prompt", "Tab", "prompt.complete-next"),
        ("text", "normal", "/", "text.search-forward"),
    ] {
        registry
            .keymap
            .map(scope, mode, vec![key.into()], Some(command.into()));
    }
    for command in [
        "alfa",
        "bravo",
        "charlie",
        "delta",
        "echo",
        "foxtrot",
        "golf",
        "help",
        "hotel",
        "set-language",
    ] {
        let id = format!("fixture.{command}");
        registry.register_action(&id, command, CommandImplementation::External(()));
        registry.register_command_line(&id, command);
    }
    registry.register_command_alias("lang", "set-language");
    registry.register_command_alias("语言", "set-language");
    terminal
        .workbench
        .editor
        .set_catalog(registry.binding_catalog());
    terminal
}

fn type_prompt(terminal: &mut Terminal, text: &str) -> Frame {
    terminal.key(":");
    for key in text.chars() {
        terminal.key(&key.to_string());
    }
    let frame = terminal.render();
    assert_eq!(frame.prompt, format!(":{text}"));
    frame
}

#[test]
fn search_prompts_block_clicks_on_the_underlying_document() {
    let mut terminal = prompt_terminal();
    let before = terminal.selection();
    terminal.key("/");
    let frame = terminal.render();
    assert!(frame.prompt.contains("search"));
    terminal.mouse(MouseKind::Press, cell(&frame, "d"));
    assert_eq!(terminal.selection(), before);
    assert_eq!(terminal.render().prompt, frame.prompt);
}

#[test]
fn clicking_a_scrolled_command_candidate_selects_it_until_enter() {
    let mut terminal = prompt_terminal();
    terminal.width = 12;
    terminal.height = 8;
    let initial = type_prompt(&mut terminal, "");
    assert!(
        !initial
            .rows
            .iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.text.contains("help"))
    );
    for _ in 0..8 {
        terminal.key("Tab");
    }
    let frame = terminal.render();
    assert_ne!(frame.prompt, ":help");
    assert!(
        terminal
            .mouse(MouseKind::Press, cell(&frame, "help"))
            .is_empty()
    );
    assert_eq!(terminal.render().prompt, ":help");
    assert!(matches!(
        terminal.key("Enter").as_slice(),
        [Effect::Command(CommandAction::Invoke { name, .. })] if name == "fixture.help"
    ));
}

#[test]
fn clicking_outside_command_candidates_dismisses_the_prompt_and_selects_text() {
    for input in ["", "zzzzzzzz"] {
        let mut terminal = prompt_terminal();
        let frame = type_prompt(&mut terminal, input);
        terminal.mouse(MouseKind::Press, cell(&frame, "c"));
        assert!(terminal.render().prompt.is_empty());
        assert_eq!(terminal.selection(), (2, 2));
    }
}

#[test]
fn clicks_on_command_popup_padding_and_alias_documentation_keep_the_prompt_open() {
    let mut terminal = prompt_terminal();
    let frame = type_prompt(&mut terminal, "he");
    let (_, row) = cell(&frame, "help");
    terminal.mouse(MouseKind::Press, (frame.width - 1, row));
    assert_eq!(terminal.render().prompt, ":he");

    let mut terminal = prompt_terminal();
    let frame = type_prompt(&mut terminal, "语");
    terminal.mouse(MouseKind::Press, cell(&frame, "Aliases: lang, 语言"));
    assert_eq!(terminal.render().prompt, ":语");
}

#[test]
fn clicking_the_command_input_keeps_its_text() {
    for input in ["help", "zzzzzzzz", "help argument"] {
        let mut terminal = prompt_terminal();
        let frame = type_prompt(&mut terminal, input);
        terminal.mouse(MouseKind::Press, (1, frame.height - 1));
        assert_eq!(terminal.render().prompt, format!(":{input}"));
    }
}
