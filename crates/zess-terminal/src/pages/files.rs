use std::collections::BTreeMap;

use zess_plugin::{
    FileTree, FileTreeDirectory, FileTreeKey, FileTreeListing, ItemNavigation, ItemSelection,
    set_file_tree_expanded,
};
use zess_protocol::resource::{
    CachedDirectory, PageCacheEntry, PageCacheKey, PageId, ResourceUpdate,
};
use zess_ui::DisplayRow;

use super::{
    Page,
    cache::{self, Cache},
};

#[derive(Default)]
pub(super) struct DirectoryCache(Cache<PageCacheEntry, FileTreeDirectory>);

impl DirectoryCache {
    pub(super) fn entries(&self) -> Vec<PageCacheEntry> {
        self.0.iter().map(|(entry, _)| entry.clone()).collect()
    }

    pub(super) fn invalidate(&mut self, entries: &[PageCacheEntry]) {
        self.0.retain(|entry| !entries.contains(entry));
    }

    pub(super) fn forget_page(&mut self, page_id: PageId) {
        self.0.retain(|entry| entry.page_id != page_id);
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    fn directory(&mut self, page_id: PageId, key: FileTreeKey) -> Option<FileTreeDirectory> {
        let key = PageCacheKey::FileTreeDirectory(key);
        let entry = self.0.iter().find_map(|(entry, _)| {
            (entry.page_id == page_id && entry.key == key).then(|| entry.clone())
        })?;
        self.0.get(&entry).cloned()
    }

    fn reconstruct(
        &mut self,
        page_id: PageId,
        tree: &mut FileTree,
        transfers: Vec<CachedDirectory>,
    ) -> Option<Vec<DisplayRow>> {
        let directories = transfers
            .iter()
            .map(|transfer| {
                let PageCacheKey::FileTreeDirectory(key) = &transfer.entry.key else {
                    return None;
                };
                (transfer.entry.page_id == page_id && key.show_ignored == tree.show_ignored)
                    .then_some((key.path.as_str(), transfer))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        // Resolve all references before admitting these payloads, which can evict previous entries.
        let mut closed = Vec::new();
        let reconstructed = tree.rows("", |path| {
            let transfer = directories.get(path)?;
            let directory = transfer
                .data
                .as_ref()
                .or_else(|| self.0.get(&transfer.entry))?;
            closed.extend(
                directory
                    .rows
                    .iter()
                    .filter(|row| row.directory && !tree.expanded.contains(&row.row.id))
                    .map(|row| (row.row.id.clone(), FileTreeListing::Closed)),
            );
            Some(directory.clone())
        });
        tree.directories.extend(closed);
        for transfer in transfers {
            if let Some(data) = transfer.data {
                self.0.retain(|entry| {
                    entry.page_id != transfer.entry.page_id || entry.key != transfer.entry.key
                });
                if let Some(bytes) =
                    cache::payload_size(|writer| serde_json::to_writer(writer, &data))
                {
                    self.0.insert(transfer.entry, data, bytes);
                }
            }
        }
        reconstructed
    }
}

pub(super) struct FileNavigation {
    path: String,
    expanded: bool,
}

pub(super) struct PendingFileRows {
    navigation: Option<FileNavigation>,
    published_selection: ItemSelection,
    selection_change: Option<ItemNavigation>,
}

#[derive(Default)]
pub(super) enum FileRows {
    #[default]
    Untracked,
    Verifying(PendingFileRows),
    Confirmed {
        tree: FileTree,
        entries: Vec<PageCacheEntry>,
    },
}

pub(super) enum FileRowsUpdate {
    None,
    Missing,
    Selection(ItemNavigation),
}

impl Page {
    pub(super) fn verifying_file_rows(&self) -> bool {
        matches!(self.file_rows, FileRows::Verifying(_))
    }

    fn begin_file_validation(&mut self) -> &mut PendingFileRows {
        if !self.verifying_file_rows() {
            self.file_rows = FileRows::Verifying(PendingFileRows {
                navigation: None,
                published_selection: self.position.selection.clone(),
                selection_change: None,
            });
            self.display.loading = true;
            self.jump = None;
            self.dragging = false;
        }
        let FileRows::Verifying(pending) = &mut self.file_rows else {
            unreachable!()
        };
        pending
    }

    pub(super) fn revalidate_file_rows(&mut self) {
        if !matches!(self.file_rows, FileRows::Untracked)
            || (self.position.source.kind == "files" && !self.display.rows.is_empty())
        {
            self.begin_file_validation().navigation = None;
        }
    }

    pub(super) fn verified_file_selection(&self) -> &ItemSelection {
        match &self.file_rows {
            FileRows::Verifying(pending) => &pending.published_selection,
            _ => &self.position.selection,
        }
    }

    pub(super) fn defer_file_selection(&mut self, navigation: ItemNavigation) -> bool {
        let FileRows::Verifying(pending) = &mut self.file_rows else {
            return false;
        };
        pending.selection_change = Some(navigation);
        true
    }

    pub(super) fn has_deferred_file_selection(&self) -> bool {
        matches!(&self.file_rows, FileRows::Verifying(pending) if pending.selection_change.is_some())
    }

    pub(super) fn file_cache_invalidated(&mut self, entries: &[PageCacheEntry]) {
        let FileRows::Confirmed {
            entries: active, ..
        } = &self.file_rows
        else {
            return;
        };
        if entries.iter().any(|entry| active.contains(entry)) {
            self.revalidate_file_rows();
        }
    }

    pub(super) fn receive_file_rows(
        &mut self,
        update: &mut ResourceUpdate,
        cache: &mut DirectoryCache,
    ) -> FileRowsUpdate {
        let Some(display) = update.output.display.as_mut() else {
            return FileRowsUpdate::None;
        };
        let Some(mut tree) = update.output.file_tree.take() else {
            if self.verifying_file_rows() && display.loading && !self.display.rows.is_empty() {
                display.rows.clear();
                update.output.selection = None;
                return FileRowsUpdate::None;
            }
            return self.confirm_file_rows(update, FileRows::Untracked);
        };
        if tree.is_loading() {
            self.begin_file_validation();
            if !self.display.rows.is_empty() {
                display.loading = true;
                display.rows.clear();
                update.output.selection = None;
            }
            return FileRowsUpdate::None;
        }
        let entries = if update.cache.directories.is_empty() {
            Vec::new()
        } else {
            let transfers = std::mem::take(&mut update.cache.directories);
            let entries = transfers
                .iter()
                .map(|transfer| transfer.entry.clone())
                .collect();
            let Some(rows) = cache.reconstruct(self.position.page_id, &mut tree, transfers) else {
                self.begin_file_validation();
                display.loading = true;
                display.rows.clear();
                update.output.selection = None;
                return FileRowsUpdate::Missing;
            };
            display.rows = rows;
            entries
        };
        if let FileRows::Verifying(PendingFileRows {
            navigation: Some(navigation),
            ..
        }) = &self.file_rows
            && tree.directories.contains_key(&navigation.path)
            && tree.expanded.contains(&navigation.path) != navigation.expanded
        {
            display.loading = true;
            display.rows.clear();
            update.output.selection = None;
            return FileRowsUpdate::None;
        }
        self.confirm_file_rows(update, FileRows::Confirmed { tree, entries })
    }

    fn confirm_file_rows(
        &mut self,
        update: &mut ResourceUpdate,
        state: FileRows,
    ) -> FileRowsUpdate {
        let previous = std::mem::replace(&mut self.file_rows, state);
        let FileRows::Verifying(PendingFileRows {
            selection_change: Some(navigation),
            ..
        }) = previous
        else {
            return FileRowsUpdate::None;
        };
        let mut selection = self.position.selection.clone();
        selection.reconcile(&update.output.display.as_ref().unwrap().rows);
        update.output.selection = Some(selection);
        FileRowsUpdate::Selection(navigation)
    }

    pub(super) fn navigate_file_rows(&mut self, event: &str, cache: &mut DirectoryCache) -> bool {
        let FileRows::Confirmed { tree, .. } = &self.file_rows else {
            return false;
        };
        if self.position.mode != "normal" || self.position.filter.is_some() {
            return false;
        }
        let Some(path) = self.position.selection.primary.as_deref() else {
            return false;
        };
        if !tree.directories.contains_key(path) {
            return false;
        }
        let open = match (event, tree.expanded.contains(path)) {
            ("left" | "toggle", true) => false,
            ("right" | "toggle", false) => true,
            _ => return false,
        };
        let Some(index) = self.display.rows.iter().position(|row| row.id == path) else {
            return false;
        };
        let navigation = FileNavigation {
            path: path.into(),
            expanded: open,
        };
        let children = if open {
            tree.rows(path, |path| {
                cache.directory(
                    self.position.page_id,
                    FileTreeKey {
                        path: path.into(),
                        show_ignored: tree.show_ignored,
                    },
                )
            })
        } else {
            Some(Vec::new())
        };
        if let Some(children) = children {
            let row = &mut self.display.rows[index];
            set_file_tree_expanded(row, open);
            let depth = row.depth;
            let start = index + 1;
            let end = start
                + self.display.rows[start..]
                    .iter()
                    .take_while(|row| row.depth > depth)
                    .count();
            self.display.rows.splice(start..end, children);
            self.display.rows_revision = None;
            self.visible_rows = None;
            self.measure_display();
        }
        self.begin_file_validation().navigation = Some(navigation);
        true
    }
}
