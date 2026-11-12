use super::*;
use std::fs;

struct Directory(PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zess-watch-{name}-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn listeners(query: &Query) -> Vec<u64> {
    query
        .binding
        .lock()
        .unwrap()
        .watches
        .iter()
        .map(|watch| watch.listener)
        .collect()
}

#[cfg(unix)]
#[test]
fn rescans_recover_missed_symbolic_link_changes_and_observe_the_new_target() {
    let directory = Directory::new("rescan-link");
    for (name, text) in [("first", "old"), ("second", "new")] {
        fs::create_dir(directory.0.join(name)).unwrap();
        fs::write(directory.0.join(name).join("note.txt"), text).unwrap();
    }
    std::os::unix::fs::symlink("first", directory.0.join("alias")).unwrap();
    let mut services = crate::NativeServices::new(directory.0.clone()).unwrap();
    let request = crate::QueryRequest::Preview {
        path: "alias/note.txt".into(),
        show_ignored: false,
    };
    let query = services.acquire_query(&request).unwrap();
    services.wait_for_registration(&query).unwrap();
    let mut observation = Observation::new(query.clone());
    let root_watch = services.queries.registry.lock().unwrap().entries[&WatchSpec {
        path: directory.0.clone(),
        recursive: false,
    }]
        .clone();

    // Keep the physical watches alive but discard their deliveries, as if the
    // backend had lost the symlink replacement events before reporting Rescan.
    let detached = query
        .binding
        .lock()
        .unwrap()
        .watches
        .iter()
        .map(|watch| {
            watch
                .owner
                .listeners
                .lock()
                .unwrap()
                .remove(&watch.listener)
                .unwrap()
        })
        .collect::<Vec<_>>();
    std::os::unix::fs::symlink("second", directory.0.join("next-alias")).unwrap();
    fs::rename(directory.0.join("next-alias"), directory.0.join("alias")).unwrap();
    let rescan = WatchEvent {
        paths: Vec::new(),
        kind: WatchEventKind::Other,
        rescan: true,
    };
    for listener in detached {
        listener.notify(&rescan);
    }
    services.poll_changes().unwrap();
    services.wait_for_registration(&query).unwrap();
    assert!(observation.changed());
    observation.acknowledge();
    assert_eq!(
        services.call("fs.preview", serde_json::json!({"path":"alias/note.txt"}))["text"],
        "new"
    );
    let root_owner = root_watch.upgrade().unwrap();
    let current = services.queries.registry.lock().unwrap().entries[&WatchSpec {
        path: directory.0.clone(),
        recursive: false,
    }]
        .upgrade()
        .unwrap();
    assert!(Arc::ptr_eq(&root_owner, &current));
    assert!(!current.stale.load(Ordering::Relaxed));

    fs::write(directory.0.join("second/note.txt"), "updated").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !observation.changed() {
        services.poll_changes().unwrap();
        assert!(
            Instant::now() < deadline,
            "new symlink target was not watched after Rescan"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        services.call("fs.preview", serde_json::json!({"path":"alias/note.txt"}))["text"],
        "updated"
    );
}

#[test]
fn replacement_watches_are_installed_before_previous_leases_are_released() {
    let directory = Directory::new("replacement");
    fs::write(directory.0.join("note.txt"), "text").unwrap();
    let mut services = crate::NativeServices::new(directory.0.clone()).unwrap();
    let query = services
        .acquire_key(QueryKey::Filesystem("note.txt".into()))
        .unwrap();
    services.wait_for_registration(&query).unwrap();
    let previous = listeners(&query);
    assert!(!previous.is_empty());
    let watched = services.queries.watched_paths();
    let registry = services.queries.registry.clone();
    let gate = registry.lock().unwrap();

    query.signal.invalidate();
    services.poll_changes().unwrap();
    let registration = query.registration();
    assert_eq!(registration.ready(), Ok(false));
    assert_eq!(listeners(&query), previous);
    assert_eq!(services.queries.watched_paths(), watched);

    drop(gate);
    services.wait_for_registration(&query).unwrap();
    assert_eq!(registration.ready(), Ok(true));
    assert_ne!(listeners(&query), previous);
    assert_eq!(services.queries.watched_paths(), watched);
}
