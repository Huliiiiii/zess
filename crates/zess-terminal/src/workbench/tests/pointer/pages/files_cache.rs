use zess_plugin::{FileTree, FileTreeListing, file_tree_disclosure};
use zess_protocol::resource::{
    CachedDirectory, PageCacheEntry, PageCacheKey, ResourceCacheInvalidation, ResourceCacheUpdate,
};

use super::*;

fn install_files_commands(terminal: &mut Terminal) {
    let mut commands = Registry::<()>::default();
    commands.command(
        "tree.open",
        "Open Files",
        HostRequest::OpenBuffer {
            kind: "files".into(),
            argument: json!({}),
        },
    );
    for (name, event) in [
        ("tree.expand", "right"),
        ("tree.collapse", "left"),
        ("tree.delete", "delete"),
    ] {
        commands.buffer_command(name, name, "files", event);
    }
    terminal
        .workbench
        .editor
        .set_catalog(commands.binding_catalog());
}

fn publish_tree(
    terminal: &mut Terminal,
    generation: u64,
    a_child: Option<&str>,
    b_child: Option<&str>,
) {
    let mut tree = FileTree::default();
    tree.directories
        .insert(String::new(), FileTreeListing::Ready);
    let mut rows = Vec::new();
    for (path, child) in [("A", a_child), ("B", b_child)] {
        let row = DisplayRow {
            id: path.into(),
            text: format!("{}{path}", file_tree_disclosure(child.is_some())),
            ..Default::default()
        };
        rows.push(row);
        let listing = if let Some(child) = child {
            tree.expanded.insert(path.into());
            rows.push(DisplayRow {
                id: format!("{path}/{child}"),
                text: child.into(),
                depth: 1,
                ..Default::default()
            });
            FileTreeListing::Ready
        } else {
            FileTreeListing::Closed
        };
        tree.directories.insert(path.into(), listing);
    }
    let page_id = terminal.active_page().page_id;
    let subscription_id = terminal.workbench.pages.subscription().unwrap();
    let directories = tree
        .directory_rows(&rows)
        .into_iter()
        .map(|(key, data)| CachedDirectory {
            entry: PageCacheEntry {
                page_id,
                key: PageCacheKey::FileTreeDirectory(key),
                revision: generation + 1,
            },
            data: Some(data),
        })
        .collect();
    tree.directories
        .retain(|_, listing| *listing != FileTreeListing::Closed);
    terminal
        .workbench
        .apply_resource(
            ResourceUpdate {
                cache: ResourceCacheUpdate {
                    directories,
                    ..Default::default()
                },
                kind: Default::default(),
                subscription_id,
                generation,
                document_scoped: false,
                document: None,
                patch_revision: None,
                metadata: Some(PageMetadata {
                    view_id: 2,
                    buffer_id: 8,
                    options: json!({}),
                }),
                output: PluginOutput {
                    display: Some(Display {
                        title: "Files".into(),
                        ..Default::default()
                    }),
                    selection: Some(single_selection("A")),
                    file_tree: Some(tree),
                    ..Default::default()
                },
            },
            &terminal.state,
        )
        .unwrap();
}

fn command(terminal: &mut Terminal, name: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    terminal
        .workbench
        .command(name.into(), Value::Null, &terminal.state, &mut effects)
        .unwrap();
    effects
}

fn generation(effects: &[Effect]) -> u64 {
    let [
        Effect::Command(CommandAction::Invoke {
            target: CommandTarget::Page { input, .. },
            ..
        }),
    ] = effects
    else {
        panic!("expected a command targeting the Files page");
    };
    input.generation
}

fn shows(terminal: &mut Terminal, text: &str) -> bool {
    terminal
        .render()
        .rows
        .iter()
        .any(|row| row_text(row).contains(text))
}

#[test]
fn reopening_a_directory_shows_its_cached_children_during_verification() {
    let mut terminal = Terminal::document("source\n");
    install_files_commands(&mut terminal);
    terminal.render();
    let subscription = terminal.open_files(vec![item("A"), item("B")], vec![]);
    publish_tree(&mut terminal, 0, Some("cached-child"), None);
    let collapsed = generation(&command(&mut terminal, "tree.collapse"));
    publish_tree(
        &mut terminal,
        collapsed,
        None,
        Some("other-directory-child"),
    );

    let pending = generation(&command(&mut terminal, "tree.expand"));
    assert!(shows(&mut terminal, "cached-child"));
    assert!(shows(&mut terminal, "other-directory-child"));
    assert_eq!(terminal.active_page().selection, single_selection("A"));
    terminal.native("data.move-down");
    assert_eq!(
        terminal.active_page().selection,
        single_selection("A/cached-child")
    );
    assert!(terminal.updates(subscription).is_empty());

    let mut effects = Vec::new();
    let error = terminal
        .workbench
        .command(
            "tree.delete".into(),
            Value::Null,
            &terminal.state,
            &mut effects,
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "wait for Files to finish loading");
    assert!(effects.is_empty());

    publish_tree(
        &mut terminal,
        collapsed,
        None,
        Some("other-directory-child"),
    );
    assert!(shows(&mut terminal, "cached-child"));

    terminal.width -= 1;
    terminal.render();
    let (event, input) = terminal.last_update(subscription);
    assert!(matches!(event, ResourceEvent::Resize));
    assert!(input.generation > pending);
    assert_eq!(input.selection, single_selection("A"));
    publish_tree(
        &mut terminal,
        input.generation,
        None,
        Some("other-directory-child"),
    );
    assert!(shows(&mut terminal, "cached-child"));

    publish_tree(
        &mut terminal,
        input.generation,
        Some("revalidated-child"),
        Some("other-directory-child"),
    );
    assert!(shows(&mut terminal, "revalidated-child"));
    assert!(!shows(&mut terminal, "cached-child"));
    let (event, selection_update) = terminal.last_update(subscription);
    assert!(matches!(event, ResourceEvent::Select { .. }));
    assert_eq!(selection_update.selection, single_selection("A"));
    assert_eq!(
        generation(&command(&mut terminal, "tree.delete")),
        selection_update.generation
    );
}

#[test]
fn an_invalidated_closed_directory_waits_for_its_fresh_listing() {
    let mut terminal = Terminal::document("source\n");
    install_files_commands(&mut terminal);
    terminal.render();
    terminal.open_files(vec![item("A"), item("B")], vec![]);
    publish_tree(&mut terminal, 0, Some("invalidated-child"), None);
    let collapsed = generation(&command(&mut terminal, "tree.collapse"));
    publish_tree(&mut terminal, collapsed, None, None);
    terminal.workbench.pages.invalidate_cache(
        &[PageCacheEntry {
            page_id: terminal.active_page().page_id,
            key: PageCacheKey::FileTreeDirectory(zess_plugin::FileTreeKey {
                path: "A".into(),
                show_ignored: false,
            }),
            revision: 1,
        }],
        ResourceCacheInvalidation::Changed,
    );

    let pending = generation(&command(&mut terminal, "tree.expand"));
    assert!(!shows(&mut terminal, "invalidated-child"));
    publish_tree(&mut terminal, pending, Some("new-child"), None);
    assert!(shows(&mut terminal, "new-child"));
}

#[test]
fn returning_to_files_keeps_its_visible_tree_and_selection_while_resubscribing() {
    let mut terminal = Terminal::document("source\n");
    install_files_commands(&mut terminal);
    terminal.render();
    let first = terminal.open_files(vec![item("A"), item("B")], vec![]);
    publish_tree(&mut terminal, 0, Some("previously-shown-file"), None);
    terminal.native("data.move-down");
    let selection = single_selection("A/previously-shown-file");
    assert_eq!(terminal.active_page().selection, selection);
    let page = terminal.active_page().page_id;

    terminal.native("quit");
    command(&mut terminal, "tree.open");

    assert_eq!(terminal.active_page().page_id, page);
    assert_ne!(terminal.workbench.pages.subscription(), Some(first));
    assert!(shows(&mut terminal, "previously-shown-file"));
    assert_eq!(terminal.active_page().selection, selection);
}
