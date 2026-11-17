use std::cell::Cell;

use zess_plugin::{ItemNavigation, PreviewContent, PreviewPane};
use zess_protocol::resource::{
    PageCacheEntry, PageCacheKey, ResourceCacheInvalidation, ResourceEvent, ResourceUpdate,
};

use super::*;

fn selection(path: &str) -> ItemSelection {
    ItemSelection {
        primary: Some(path.into()),
        anchor: Some(path.into()),
        selected: vec![path.into()],
    }
}

fn page_input(generation: u64, path: &str) -> PageInput {
    PageInput {
        generation,
        selection: selection(path),
        register: None,
        mode: "normal".into(),
        filter: None,
        source_buffer: None,
        width: 80,
        active: true,
        preview_width: 39,
        preview_height: 22,
    }
}

fn git(server: &Server, arguments: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(arguments)
        .current_dir(&server.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn select(generation: u64, primary: &str) -> ClientMessage {
    ClientMessage::Resource {
        request: ResourceRequest::Update {
            subscription_id: 1,
            input: page_input(generation, primary),
            event: ResourceEvent::Select {
                navigation: ItemNavigation::Jump,
            },
        },
    }
}

fn settled_preview(update: &ResourceUpdate) -> bool {
    update.cache.preview.is_some()
        && update
            .output
            .display
            .as_ref()
            .is_some_and(|display| !display.loading)
}

fn directory(entry: &PageCacheEntry, path: &str) -> bool {
    matches!(&entry.key, PageCacheKey::FileTreeDirectory(key) if key.path == path)
}

fn file_directory_command(name: &str, generation: u64, path: &str) -> CommandAction {
    CommandAction::Invoke {
        target: CommandTarget::Page {
            subscription_id: 1,
            input: PageCommandInput {
                selection: selection(path),
                generation,
                register: None,
            },
        },
        name: name.into(),
        argument: json!({}),
    }
}

impl Client {
    fn cache_interest(&mut self, entries: Vec<PageCacheEntry>) {
        self.send(ClientMessage::Resource {
            request: ResourceRequest::CacheInterest { entries },
        });
    }

    fn open_cached_page(
        &mut self,
        kind: &str,
        page_id: PageId,
        primary: &str,
        argument: serde_json::Value,
    ) {
        self.send(ClientMessage::Resource {
            request: ResourceRequest::Subscribe {
                subscription_id: 1,
                page_id,
                clone_from: None,
                source: PageSource {
                    kind: kind.into(),
                    argument,
                },
                input: page_input(0, primary),
            },
        });
    }

    fn select_cached_page(&mut self, generation: u64, primary: &str) {
        self.send(select(generation, primary));
    }

    fn cached_projection(
        &mut self,
        generation: u64,
        matches: impl Fn(&ResourceUpdate) -> bool,
    ) -> ResourceUpdate {
        loop {
            if let ServerMessage::Resource { update } = receive(&mut self.reader)
                && update.subscription_id == 1
                && update.generation == generation
                && matches(&update)
            {
                return update;
            }
        }
    }

    fn wait_for_cache_change(&mut self, entry: &PageCacheEntry) {
        loop {
            if let ServerMessage::ResourceCacheInvalidated {
                entries,
                reason: ResourceCacheInvalidation::Changed,
            } = receive(&mut self.reader)
                && entries.contains(entry)
            {
                return;
            }
        }
    }
}

#[test]
fn git_preview_reuse_ends_when_its_file_changes_off_selection() {
    let server = Server::start("keep");
    git(&server, &["init", "--quiet"]);
    let alpha = server.root.join("alpha.txt");
    fs::write(&alpha, "original alpha\n").unwrap();
    fs::write(server.root.join("beta.txt"), "original beta\n").unwrap();
    let (mut client, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: client.epoch,
        sequence: 1,
    };
    client.cache_interest(Vec::new());
    client.open_cached_page("git", page_id, "file:alpha.txt", json!({}));
    let projection = client.cached_projection(0, settled_preview);
    let original = projection.cache.preview.unwrap();
    assert!(original.data.is_some());
    let rows = projection.cache.rows.unwrap();
    assert!(rows.data.is_some());
    assert_eq!(
        original.entry.key,
        PageCacheKey::Preview {
            primary: "file:alpha.txt".into(),
            width: 39,
            height: 22,
        }
    );
    client.cache_interest(vec![original.entry.clone(), rows.entry.clone()]);

    client.select_cached_page(1, "file:beta.txt");
    let projection = client.cached_projection(1, settled_preview);
    let beta = projection.cache.preview.unwrap();
    assert!(beta.data.is_some());
    assert!(projection.cache.rows.unwrap().data.is_none());
    client.cache_interest(vec![original.entry.clone(), beta.entry.clone(), rows.entry]);

    client.select_cached_page(2, "file:alpha.txt");
    let reused = client
        .cached_projection(2, settled_preview)
        .cache
        .preview
        .unwrap();
    assert_eq!(reused.entry, original.entry);
    assert!(reused.data.is_none());
    client.select_cached_page(3, "file:beta.txt");
    let reused = client
        .cached_projection(3, settled_preview)
        .cache
        .preview
        .unwrap();
    assert_eq!(reused.entry, beta.entry);
    assert!(reused.data.is_none());

    fs::write(&alpha, "changed alpha\n").unwrap();
    loop {
        if let ServerMessage::ResourceCacheInvalidated {
            entries,
            reason: ResourceCacheInvalidation::Changed,
        } = receive(&mut client.reader)
        {
            assert!(!entries.contains(&beta.entry));
            if entries.contains(&original.entry) {
                break;
            }
        }
    }
    client.cache_interest(vec![beta.entry]);
    client.select_cached_page(4, "file:alpha.txt");
    let changed = client
        .cached_projection(4, settled_preview)
        .cache
        .preview
        .unwrap();
    assert_ne!(changed.entry.revision, original.entry.revision);
    assert!(changed.data.unwrap().panes.iter().any(|pane| {
        matches!(pane, PreviewPane::View { content: PreviewContent::GitDiff { diff }, .. }
            if diff.files.iter().any(|file| file.raw.contains("+changed alpha")))
    }));
}

#[test]
fn a_cached_git_directory_preview_tracks_new_members() {
    let server = Server::start("keep");
    git(&server, &["init", "--quiet"]);
    let directory_path = server.root.join("group");
    fs::create_dir(&directory_path).unwrap();
    fs::write(directory_path.join("existing.txt"), "existing change\n").unwrap();
    fs::write(server.root.join("other.txt"), "other change\n").unwrap();
    let (mut client, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: client.epoch,
        sequence: 1,
    };

    client.cache_interest(Vec::new());
    client.open_cached_page("git", page_id, "file:other.txt", json!({}));
    client.cached_projection(0, settled_preview);
    client.select_cached_page(1, "directory:group");
    let old = client
        .cached_projection(1, settled_preview)
        .cache
        .preview
        .unwrap();
    assert!(old.data.is_some());
    client.cache_interest(vec![old.entry.clone()]);
    client.select_cached_page(2, "file:other.txt");
    client.cached_projection(2, settled_preview);

    fs::write(directory_path.join("new.txt"), "new change inside group\n").unwrap();
    client.wait_for_cache_change(&old.entry);
    client.cache_interest(Vec::new());
    client.select_cached_page(3, "directory:group");
    let current = client
        .cached_projection(3, settled_preview)
        .cache
        .preview
        .unwrap();
    assert_ne!(current.entry.revision, old.entry.revision);
    assert!(current.data.unwrap().panes.iter().any(|pane| {
        matches!(pane, PreviewPane::View { content: PreviewContent::GitDiff { diff }, .. }
            if diff.files.iter().any(|file| file.target.path == "group/new.txt"))
    }));
}

#[test]
fn commit_projections_reuse_the_previously_selected_preview() {
    let server = Server::start("keep");
    git(&server, &["init", "--quiet"]);
    fs::write(server.root.join("first.txt"), "first commit file\n").unwrap();
    git(&server, &["add", "first.txt"]);
    git(&server, &["commit", "--quiet", "-m", "First commit"]);
    let first = git(&server, &["rev-parse", "HEAD"]);
    fs::write(server.root.join("first.txt"), "first file edited\n").unwrap();
    fs::write(server.root.join("second.txt"), "second file added\n").unwrap();
    git(&server, &["add", "first.txt", "second.txt"]);
    git(&server, &["commit", "--quiet", "-m", "Second commit"]);
    let second = git(&server, &["rev-parse", "HEAD"]);
    let (mut client, _) = Client::attach(&server, "client");

    for (index, (kind, argument, primary, other)) in [
        ("git-history", json!({}), first.as_str(), second.as_str()),
        (
            "git-commit-files",
            json!({"commit": second}),
            "file:first.txt",
            "file:second.txt",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let page_id = PageId {
            context_epoch: client.epoch,
            sequence: index as u64 + 1,
        };
        let focus_requested = Cell::new(false);
        let preview_for = |update: &ResourceUpdate| {
            if kind == "git-history" {
                for request in &update.output.requests {
                    if let HostRequest::PreviewGroup { panes, focus, .. } = request {
                        if *focus {
                            focus_requested.set(true);
                        }
                        if panes.iter().any(PreviewPane::loading) {
                            assert!(panes.iter().all(|pane| {
                                let content = match pane {
                                    PreviewPane::Surface { content, .. } | PreviewPane::View { content, .. } => content,
                                };
                                matches!(content, PreviewContent::Rows { rows, .. } | PreviewContent::Items { rows, .. } if rows.is_empty())
                            }));
                        }
                    }
                }
                if update
                    .cache
                    .preview
                    .as_ref()
                    .is_some_and(|preview| preview.focus)
                {
                    focus_requested.set(true);
                }
            }
            update
                .cache
                .preview
                .as_ref()
                .is_some_and(|preview| preview.entry.page_id == page_id)
                && settled_preview(update)
        };
        client.cache_interest(Vec::new());
        client.open_cached_page(kind, page_id, primary, argument);
        let selected = client
            .cached_projection(0, preview_for)
            .cache
            .preview
            .unwrap();
        assert!(selected.data.is_some());
        assert!(
            matches!(&selected.entry.key, PageCacheKey::Preview { primary: key, .. } if key == primary)
        );
        client.cache_interest(vec![selected.entry.clone()]);
        if kind == "git-history" {
            let mut requests = framing::encode(&select(1, other)).unwrap();
            requests.extend(
                framing::encode(&ClientMessage::Command {
                    request_id: RequestId(1),
                    server_instance: client.instance.clone(),
                    action: CommandAction::Invoke {
                        target: CommandTarget::Page {
                            subscription_id: 1,
                            input: PageCommandInput {
                                generation: 1,
                                selection: selection(other),
                                register: None,
                            },
                        },
                        name: "git-history.preview".into(),
                        argument: json!({}),
                    },
                })
                .unwrap(),
            );
            client.stream.write_all(&requests).unwrap();
        } else {
            client.select_cached_page(1, other);
        }
        assert!(
            client
                .cached_projection(1, preview_for)
                .cache
                .preview
                .unwrap()
                .data
                .is_some()
        );
        if kind == "git-history" {
            assert!(focus_requested.get());
        }
        client.select_cached_page(2, primary);
        let reused = client
            .cached_projection(2, preview_for)
            .cache
            .preview
            .unwrap();
        assert_eq!(selected.entry, reused.entry);
        assert!(reused.data.is_none());
        client.send(ClientMessage::Resource {
            request: ResourceRequest::Discard { page_id },
        });
    }
}

#[test]
fn an_obsolete_selection_does_not_send_display_rows() {
    let server = Server::start("keep");
    fs::write(server.root.join("alpha.txt"), "alpha\n").unwrap();
    fs::write(server.root.join("beta.txt"), "beta\n").unwrap();
    let (mut client, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: client.epoch,
        sequence: 1,
    };
    client.cache_interest(Vec::new());
    client.open_cached_page("files", page_id, "alpha.txt", json!({}));
    client.cached_projection(0, settled_preview);

    let mut requests = framing::encode(&select(1, "beta.txt")).unwrap();
    requests.extend(framing::encode(&select(2, "alpha.txt")).unwrap());
    client.stream.write_all(&requests).unwrap();
    let obsolete = client.cached_projection(1, |_| true);
    assert!(obsolete.output.display.is_none());
    assert!(obsolete.output.file_tree.is_none());
    assert!(obsolete.cache.preview.is_none());
    assert!(obsolete.output.requests.iter().all(|request| !matches!(
        request,
        HostRequest::PreviewGroup { .. } | HostRequest::Preview { .. }
    )));
    client.cached_projection(2, |update| update.output.display.is_some());
}

#[test]
fn releasing_a_page_retains_only_observations_with_client_interest() {
    let server = Server::start("keep");
    let alpha = server.root.join("alpha.txt");
    fs::write(&alpha, "alpha\n").unwrap();
    fs::write(server.root.join("beta.txt"), "beta\n").unwrap();
    let (mut client, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: client.epoch,
        sequence: 1,
    };
    client.cache_interest(Vec::new());
    client.open_cached_page("files", page_id, "alpha.txt", json!({}));
    let retained = client
        .cached_projection(0, settled_preview)
        .cache
        .preview
        .unwrap()
        .entry;
    client.cache_interest(vec![retained.clone()]);
    client.select_cached_page(1, "beta.txt");
    let unacknowledged = client
        .cached_projection(1, settled_preview)
        .cache
        .preview
        .unwrap()
        .entry;

    client.send(ClientMessage::Resource {
        request: ResourceRequest::Release { subscription_id: 1 },
    });
    loop {
        if let ServerMessage::ResourceCacheInvalidated {
            entries,
            reason: ResourceCacheInvalidation::Evicted,
        } = receive(&mut client.reader)
            && entries.contains(&unacknowledged)
        {
            assert!(!entries.contains(&retained));
            break;
        }
    }
    fs::write(alpha, "changed after page release\n").unwrap();
    client.wait_for_cache_change(&retained);
}

#[test]
fn files_directory_reuse_is_scoped_to_the_connection() {
    let server = Server::start("keep");
    let folder = server.root.join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("original.txt"), "original\n").unwrap();
    let (mut client, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: client.epoch,
        sequence: 1,
    };
    client.cache_interest(Vec::new());
    client.open_cached_page("files", page_id, "folder", json!({}));
    let root = client.cached_projection(0, |update| !update.cache.directories.is_empty());
    let root = root
        .cache
        .directories
        .into_iter()
        .find(|entry| directory(&entry.entry, ""))
        .unwrap();
    assert!(root.data.is_some());
    client.cache_interest(vec![root.entry.clone()]);
    client.command(1, file_directory_command("files.right", 1, "folder"));
    let opened = client.cached_projection(1, |update| update.cache.directories.len() == 2);
    let child = opened
        .cache
        .directories
        .into_iter()
        .find(|entry| directory(&entry.entry, "folder"))
        .unwrap();
    assert!(
        child
            .data
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|row| row.row.id == "folder/original.txt")
    );
    client.cache_interest(vec![root.entry.clone(), child.entry.clone()]);
    client.command(2, file_directory_command("files.left", 2, "folder"));
    let closed = client.cached_projection(2, |update| update.cache.directories.len() == 1);
    assert!(closed.cache.directories[0].data.is_none());
    client.command(3, file_directory_command("files.right", 3, "folder"));
    let reopened = client.cached_projection(3, |update| update.cache.directories.len() == 2);
    assert!(
        reopened
            .cache
            .directories
            .iter()
            .all(|entry| entry.data.is_none())
    );

    client.detach();
    fs::write(folder.join("offline.txt"), "offline\n").unwrap();
    let (mut reconnected, _) = Client::attach(&server, "client");
    reconnected.cache_interest(vec![root.entry, child.entry]);
    reconnected.open_cached_page("files", page_id, "folder", json!({}));
    let update = reconnected.cached_projection(0, |update| !update.cache.directories.is_empty());
    assert!(
        update
            .cache
            .directories
            .iter()
            .all(|entry| entry.data.is_some())
    );
    reconnected.command(4, file_directory_command("files.right", 1, "folder"));
    let update = reconnected.cached_projection(1, |update| update.cache.directories.len() == 2);
    assert!(update.cache.directories.iter().any(|entry| {
        directory(&entry.entry, "folder")
            && entry
                .data
                .as_ref()
                .unwrap()
                .rows
                .iter()
                .any(|row| row.row.id == "folder/offline.txt")
    }));
}

#[test]
fn a_worktree_edit_republishes_only_file_directories_with_changed_decorations() {
    let server = Server::start("keep");
    git(&server, &["init", "--quiet"]);
    for name in ["folder", "other"] {
        fs::create_dir(server.root.join(name)).unwrap();
        fs::write(server.root.join(name).join("tracked.txt"), "clean\n").unwrap();
    }
    git(&server, &["add", "folder", "other"]);
    git(&server, &["commit", "--quiet", "-m", "Clean directories"]);
    let (mut client, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: client.epoch,
        sequence: 1,
    };
    client.cache_interest(Vec::new());
    client.open_cached_page("files", page_id, "folder", json!({}));
    client.cached_projection(0, |update| !update.cache.directories.is_empty());
    client.command(1, file_directory_command("files.right", 1, "folder"));
    client.cached_projection(1, |update| update.cache.directories.len() == 2);
    client.command(2, file_directory_command("files.right", 2, "other"));
    let baseline = client.cached_projection(2, |update| {
        update.cache.directories.len() == 3
            && update
                .output
                .display
                .as_ref()
                .is_some_and(|display| !display.loading)
    });
    let sibling = baseline
        .cache
        .directories
        .iter()
        .find(|entry| directory(&entry.entry, "other"))
        .unwrap()
        .entry
        .clone();
    client.cache_interest(
        baseline
            .cache
            .directories
            .into_iter()
            .map(|entry| entry.entry)
            .collect(),
    );

    fs::write(server.root.join("folder/tracked.txt"), "edited\n").unwrap();
    let modified = |entry: &zess_protocol::resource::CachedDirectory, path| {
        entry.data.as_ref().is_some_and(|data| {
            data.rows.iter().any(|row| {
                row.row.id == path
                    && row
                        .row
                        .cells
                        .iter()
                        .flat_map(|cell| &cell.spans)
                        .any(|span| span.text == "M")
            })
        })
    };
    let update = client.cached_projection(2, |update| {
        update
            .cache
            .directories
            .iter()
            .any(|entry| directory(&entry.entry, "") && modified(entry, "folder"))
    });
    assert!(update.cache.directories.iter().any(|entry| directory(&entry.entry, "folder") && modified(entry, "folder/tracked.txt")));
    let unchanged = update
        .cache
        .directories
        .iter()
        .find(|entry| directory(&entry.entry, "other"))
        .unwrap();
    assert_eq!(unchanged.entry, sibling);
    assert!(unchanged.data.is_none());
}

#[test]
fn an_index_change_invalidates_a_closed_file_directory() {
    let server = Server::start("keep");
    git(&server, &["init", "--quiet"]);
    let folder = server.root.join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("new.txt"), "new file\n").unwrap();
    let (mut client, _) = Client::attach(&server, "client");
    let page_id = PageId {
        context_epoch: client.epoch,
        sequence: 1,
    };
    let settled_directories = |update: &ResourceUpdate| {
        update.cache.directories.len() == 2
            && update
                .output
                .display
                .as_ref()
                .is_some_and(|display| !display.loading)
    };
    client.cache_interest(Vec::new());
    client.open_cached_page("files", page_id, "folder", json!({}));
    client.cached_projection(0, |update| !update.cache.directories.is_empty());
    client.command(1, file_directory_command("files.right", 1, "folder"));
    let child = client
        .cached_projection(1, settled_directories)
        .cache
        .directories
        .into_iter()
        .find(|entry| directory(&entry.entry, "folder"))
        .unwrap();
    let entry = child.entry;
    assert!(
        child
            .data
            .unwrap()
            .rows
            .iter()
            .all(|row| row.row.cells.is_empty())
    );
    client.cache_interest(vec![entry.clone()]);
    client.command(2, file_directory_command("files.left", 2, "folder"));
    client.cached_projection(2, |update| update.cache.directories.len() == 1);

    git(&server, &["add", "folder/new.txt"]);
    client.wait_for_cache_change(&entry);
    client.cache_interest(Vec::new());
    client.command(3, file_directory_command("files.right", 3, "folder"));
    let changed = client
        .cached_projection(3, settled_directories)
        .cache
        .directories
        .into_iter()
        .find(|published| directory(&published.entry, "folder"))
        .unwrap();
    assert_ne!(entry.revision, changed.entry.revision);
    assert!(changed.data.unwrap().rows.iter().any(|row| {
        row.row.id == "folder/new.txt"
            && row
                .row
                .cells
                .iter()
                .flat_map(|cell| &cell.spans)
                .any(|span| span.text == "A")
    }));
}
