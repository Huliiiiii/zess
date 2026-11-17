use zess_protocol::resource::{
    CachedPreview, CachedRows, PageCacheEntry, PageCacheKey, PageId, PreviewProjection,
};
use zess_ui::{Display, DisplayRow};

use super::{
    Page,
    cache::{Cache, payload_size},
};

pub(super) enum RowsProjection {
    Received,
    Unchanged,
    Missing,
}

#[derive(Default)]
pub(super) struct RenderCache {
    previews: Cache<PageCacheEntry, PreviewProjection>,
    rows: Cache<PageCacheEntry, Vec<DisplayRow>>,
}

impl RenderCache {
    pub(super) fn entries(&self) -> impl Iterator<Item = &PageCacheEntry> {
        self.previews
            .iter()
            .map(|(entry, _)| entry)
            .chain(self.rows.iter().map(|(entry, _)| entry))
    }

    pub(super) fn forget_page(&mut self, page_id: PageId) {
        self.previews.retain(|entry| entry.page_id != page_id);
        self.rows.retain(|entry| entry.page_id != page_id);
    }

    pub(super) fn invalidate(&mut self, invalid: &[PageCacheEntry]) {
        self.previews.retain(|entry| !invalid.contains(entry));
        self.rows.retain(|entry| !invalid.contains(entry));
    }

    pub(super) fn clear(&mut self) {
        self.previews.clear();
        self.rows.clear();
    }

    pub(super) fn show_previous_preview(&mut self, page: &mut Page, width: u16, height: u16) {
        let Some(key) = page.preview_key(width, height) else {
            return;
        };
        let entry = self
            .previews
            .iter()
            .map(|(entry, _)| entry)
            .find(|entry| entry.page_id == page.position.page_id && entry.key == key)
            .cloned();
        let Some(entry) = entry else { return };
        let projection = self.previews.get(&entry).unwrap();
        if page.visible_preview.as_ref() != Some(&entry) {
            let projection = projection.clone();
            page.previews
                .replace(projection.panes, projection.layout, false);
            page.visible_preview = Some(entry);
        }
    }

    pub(super) fn confirm_preview(
        &mut self,
        page: &mut Page,
        preview: CachedPreview,
        width: u16,
        height: u16,
        generation: u64,
    ) -> bool {
        if preview.entry.page_id != page.position.page_id
            || page.preview_key(width, height).as_ref() != Some(&preview.entry.key)
        {
            return false;
        }
        let focus = preview.focus && page.can_focus_preview(generation);
        if let Some(projection) = preview.data {
            self.previews.retain(|existing| {
                existing.page_id != preview.entry.page_id || existing.key != preview.entry.key
            });
            if let Some(bytes) = payload_size(|writer| serde_json::to_writer(writer, &projection)) {
                self.previews
                    .insert(preview.entry.clone(), projection.clone(), bytes);
            }
            page.previews
                .replace(projection.panes, projection.layout, focus);
        } else if page.visible_preview.as_ref() == Some(&preview.entry) {
            page.previews.confirm(focus);
        } else if let Some(projection) = self.previews.get(&preview.entry).cloned() {
            page.previews
                .replace(projection.panes, projection.layout, focus);
        } else {
            return false;
        }
        page.visible_preview = Some(preview.entry);
        true
    }

    pub(super) fn fill_rows(
        &mut self,
        page: &mut Page,
        display: &mut Display,
        rows: CachedRows,
    ) -> RowsProjection {
        if rows.entry.page_id != page.position.page_id || rows.entry.key != PageCacheKey::Rows {
            return RowsProjection::Missing;
        }
        if let Some(data) = rows.data {
            self.rows
                .retain(|entry| entry.page_id != rows.entry.page_id);
            if let Some(bytes) = payload_size(|writer| serde_json::to_writer(writer, &data)) {
                self.rows.insert(rows.entry.clone(), data.clone(), bytes);
            }
            display.rows = data;
        } else if page.visible_rows.as_ref() == Some(&rows.entry) {
            if page.position.source.kind != "files" {
                return RowsProjection::Unchanged;
            }
            display.rows.clone_from(&page.display.rows);
        } else if let Some(data) = self.rows.get(&rows.entry) {
            display.rows.clone_from(data);
        } else {
            return RowsProjection::Missing;
        }
        if !display.loading || !display.rows.is_empty() {
            page.visible_rows = Some(rows.entry);
        }
        RowsProjection::Received
    }
}
