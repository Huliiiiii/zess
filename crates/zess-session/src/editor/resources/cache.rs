use std::{
    collections::{HashSet, VecDeque},
    hash::{BuildHasher, Hasher, RandomState},
    io,
};

use serde::Serialize;
use zess_plugin::{
    FileTree, FileTreeKey, FileTreeListing, HostRequest, PreviewContent, PreviewLayout, PreviewPane,
};
use zess_protocol::resource::{
    CachedDirectory, CachedPreview, CachedRows, PageCacheEntry, PageCacheKey, PageId,
    PreviewProjection, ResourceCacheInvalidation, ResourceUpdate,
};
use zess_services::Observation;

use super::Subscription;

#[cfg(test)]
mod tests;

const MAX_CACHE_ENTRIES: usize = 512;

struct Fingerprint<H>(H);

impl<H: Hasher> io::Write for Fingerprint<H> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ObservedEntry {
    entry: PageCacheEntry,
    fingerprint: u64,
    watches: Vec<Observation>,
    visible_directory: bool,
}

enum Publication {
    Projection,
    Directory { refreshing: bool },
}

#[derive(Default)]
pub(super) struct CachePublications {
    known: Option<HashSet<PageCacheEntry>>,
    observed: VecDeque<ObservedEntry>,
    invalidated: Vec<(ResourceCacheInvalidation, PageCacheEntry)>,
    revisions: RandomState,
    next_revision: u64,
}

impl CachePublications {
    pub(super) fn set_interest(&mut self, entries: Vec<PageCacheEntry>) {
        let mut known = entries
            .into_iter()
            .take(MAX_CACHE_ENTRIES)
            .collect::<HashSet<_>>();
        if let Some(previous) = &self.known {
            self.observed
                .retain(|item| !previous.contains(&item.entry) || known.contains(&item.entry));
        }
        known.retain(|entry| self.observed.iter().any(|item| item.entry == *entry));
        self.known = Some(known);
    }

    fn invalidate(&mut self, mut matches: impl FnMut(&mut ObservedEntry) -> bool) {
        self.observed.retain_mut(|item| {
            if !matches(item) {
                return true;
            }
            self.known.as_mut().unwrap().remove(&item.entry);
            self.invalidated
                .push((ResourceCacheInvalidation::Changed, item.entry.clone()));
            false
        });
    }

    pub(super) fn poll(&mut self) {
        self.invalidate(|item| {
            !item.visible_directory && item.watches.iter_mut().any(Observation::changed)
        });
    }

    pub(super) fn release_page(&mut self, page_id: PageId) {
        let Some(known) = &self.known else {
            return;
        };
        self.observed.retain_mut(|item| {
            if item.entry.page_id != page_id {
                return true;
            }
            item.visible_directory = false;
            if known.contains(&item.entry) {
                return true;
            }
            self.invalidated
                .push((ResourceCacheInvalidation::Evicted, item.entry.clone()));
            false
        });
        self.poll();
    }

    fn update_visible_directories(&mut self, page: PageId, tree: Option<&FileTree>) {
        for item in &mut self.observed {
            if item.entry.page_id == page
                && let PageCacheKey::FileTreeDirectory(key) = &item.entry.key
            {
                item.visible_directory = tree.is_some_and(|tree| {
                    tree.show_ignored == key.show_ignored
                        && tree
                            .directories
                            .get(&key.path)
                            .is_some_and(|listing| *listing != FileTreeListing::Closed)
                });
            }
        }
        self.poll();
    }

    pub(super) fn invalidate_page(&mut self, page_id: PageId) {
        self.invalidate(|item| item.entry.page_id == page_id);
    }

    pub(super) fn invalidate_all(&mut self) {
        self.invalidate(|_| true);
    }

    pub(super) fn take_invalidated(&mut self) -> Vec<(ResourceCacheInvalidation, PageCacheEntry)> {
        std::mem::take(&mut self.invalidated)
    }

    fn publish<T: Serialize>(
        &mut self,
        page_id: PageId,
        key: PageCacheKey,
        data: T,
        mut watches: Vec<Observation>,
        publication: Publication,
    ) -> (PageCacheEntry, Option<T>) {
        let mut fingerprint = Fingerprint(self.revisions.build_hasher());
        serde_json::to_writer(&mut fingerprint, &data).expect("page projection is serializable");
        let fingerprint = fingerprint.0.finish();
        let previous = self
            .observed
            .iter()
            .position(|item| item.entry.page_id == page_id && item.entry.key == key)
            .and_then(|index| self.observed.remove(index));
        let known = self.known.as_mut().unwrap();
        let revision =
            if let Some(previous) = previous.filter(|item| item.fingerprint == fingerprint) {
                if matches!(publication, Publication::Directory { refreshing: true }) {
                    watches = previous.watches;
                }
                previous.entry.revision
            } else {
                known.retain(|entry| entry.page_id != page_id || entry.key != key);
                self.next_revision += 1;
                self.next_revision
            };
        let entry = PageCacheEntry {
            page_id,
            key,
            revision,
        };
        self.observed.push_back(ObservedEntry {
            entry: entry.clone(),
            fingerprint,
            watches,
            visible_directory: matches!(publication, Publication::Directory { .. }),
        });
        if self.observed.len() > MAX_CACHE_ENTRIES {
            let oldest = self.observed.pop_front().unwrap().entry;
            known.remove(&oldest);
            self.invalidated
                .push((ResourceCacheInvalidation::Evicted, oldest));
        }
        let data = (!known.contains(&entry)).then_some(data);
        (entry, data)
    }

    fn preview(
        &mut self,
        page: &Subscription,
        update: &mut ResourceUpdate,
    ) -> Option<CachedPreview> {
        let primary = page.input.selection.primary.as_ref()?;
        let path = if matches!(page.source.kind.as_str(), "git" | "git-commit-files") {
            primary
                .strip_prefix("file:")
                .or_else(|| primary.strip_prefix("directory:"))
                .unwrap_or(primary)
        } else {
            primary
        };
        if path.is_empty() {
            return None;
        }
        let requests = &mut update.output.requests;
        let request_index = requests.iter().rposition(|request| {
            matches!(
                request,
                HostRequest::PreviewGroup { .. }
                    | HostRequest::Preview { .. }
                    | HostRequest::PreviewPending
            )
        })?;
        if page.source.kind == "git-history"
            && let HostRequest::PreviewGroup { panes, focus, .. } = &mut requests[request_index]
            && panes.iter().any(PreviewPane::loading)
            && panes.iter().any(|pane| match pane {
                PreviewPane::Surface { path, .. } | PreviewPane::View { path, .. } => {
                    !path.is_empty()
                }
            })
        {
            if *focus {
                for pane in panes {
                    let content = match pane {
                        PreviewPane::Surface { content, .. }
                        | PreviewPane::View { content, .. } => content,
                    };
                    if let PreviewContent::Rows { rows, .. } | PreviewContent::Items { rows, .. } =
                        content
                    {
                        rows.clear();
                    }
                }
            } else {
                requests[request_index] = HostRequest::PreviewPending;
            }
            return None;
        }
        let matches_path = match &requests[request_index] {
            HostRequest::Preview { path: shown, .. } => shown == path,
            HostRequest::PreviewGroup { panes, .. } => {
                !panes.is_empty()
                    && panes.iter().all(|pane| {
                        let (shown, loading) = match pane {
                            PreviewPane::Surface { path, loading, .. }
                            | PreviewPane::View { path, loading, .. } => (path, loading),
                        };
                        shown == path && !loading
                    })
            }
            _ => false,
        };
        if !matches_path {
            return None;
        }
        let (panes, layout, focus) = match requests.remove(request_index) {
            HostRequest::PreviewGroup {
                panes,
                layout,
                focus,
            } => (panes, layout, focus),
            HostRequest::Preview {
                path,
                content,
                focus,
            } => (
                vec![PreviewPane::View {
                    id: "main".into(),
                    path,
                    header: String::new(),
                    loading: false,
                    content,
                    key_scope: None,
                }],
                PreviewLayout::Rows,
                focus,
            ),
            _ => unreachable!(),
        };
        let git_changes_directory = page.source.kind == "git" && primary.starts_with("directory:");
        let watches = page
            .watches
            .iter()
            .filter_map(|(query, observation)| {
                let observes_content = matches!(
                    query.operation.as_str(),
                    "fs.preview" | "git.diff-files" | "git.commit-files"
                );
                let observes_directory_members =
                    git_changes_directory && query.operation == "git.status";
                (observes_content || observes_directory_members).then(|| observation.clone())
            })
            .collect();
        let (entry, data) = self.publish(
            page.page_id,
            PageCacheKey::Preview {
                primary: primary.clone(),
                width: page.input.preview_width,
                height: page.input.preview_height,
            },
            PreviewProjection { panes, layout },
            watches,
            Publication::Projection,
        );
        Some(CachedPreview {
            entry,
            data,
            focus,
            request_index,
        })
    }

    pub(super) fn prepare(&mut self, page: &Subscription, update: &mut ResourceUpdate) {
        if self.known.is_none()
            || !matches!(
                page.source.kind.as_str(),
                "files" | "git" | "git-history" | "git-commit-files"
            )
        {
            return;
        }
        if update.generation < page.input.generation {
            update.output.display = None;
            update.output.file_tree = None;
            update.output.selection = None;
            update.output.requests.retain(|request| {
                !matches!(
                    request,
                    HostRequest::PreviewGroup { .. }
                        | HostRequest::Preview { .. }
                        | HostRequest::PreviewPending
                        | HostRequest::ConfigurePreview { .. }
                )
            });
            return;
        }
        update.cache.preview = self.preview(page, update);
        let Some(display) = &mut update.output.display else {
            return;
        };
        if page.source.kind == "files" {
            self.update_visible_directories(page.page_id, update.output.file_tree.as_ref());
        }
        if update.output.file_tree.as_ref().is_some_and(FileTree::is_loading)
            && self.known.as_ref().unwrap().iter().any(|entry| entry.page_id == page.page_id
                && matches!(&entry.key, PageCacheKey::FileTreeDirectory(key) if key.path.is_empty()))
        {
            display.rows.clear();
            return;
        }
        let tree = update
            .output
            .file_tree
            .as_mut()
            .filter(|tree| !tree.is_loading());
        if let Some(tree) = tree {
            let directories = tree.directory_rows(&display.rows);
            let rows = tree.rows("", |path| {
                directories
                    .get(&FileTreeKey {
                        path: path.into(),
                        show_ignored: tree.show_ignored,
                    })
                    .cloned()
            });
            if !directories.is_empty() && rows.as_ref() == Some(&display.rows) {
                for (key, directory) in directories {
                    let watches = page
                        .watches
                        .iter()
                        .filter_map(|(query, observation)| {
                            let observes_directory = query.operation == "fs.list"
                                && query.argument["path"].as_str() == Some(&key.path);
                            (observes_directory || query.operation == "git.decorations")
                                .then(|| observation.clone())
                        })
                        .collect();
                    let (entry, data) = self.publish(
                        page.page_id,
                        PageCacheKey::FileTreeDirectory(key),
                        directory,
                        watches,
                        Publication::Directory {
                            refreshing: display.loading,
                        },
                    );
                    update
                        .cache
                        .directories
                        .push(CachedDirectory { entry, data });
                }
                tree.directories
                    .retain(|_, listing| *listing != FileTreeListing::Closed);
                display.rows.clear();
                return;
            }
        }
        let (entry, data) = self.publish(
            page.page_id,
            PageCacheKey::Rows,
            std::mem::take(&mut display.rows),
            Vec::new(),
            Publication::Projection,
        );
        update.cache.rows = Some(CachedRows { entry, data });
    }
}
