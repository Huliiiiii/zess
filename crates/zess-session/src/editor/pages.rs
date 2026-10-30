use std::ops::Index;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PageId(ViewId);

#[derive(Clone, Copy)]
pub(super) struct FocusTarget {
    page: PageId,
    preferred: ViewId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RenderTarget {
    View(ViewId),
    Surface { page: PageId, slot: String },
}

pub(super) struct PreviewSurface {
    pub buffer: BufferId,
    pub header: String,
    pub loading: bool,
    pub viewport: zess_text::PreviewViewport,
}

pub(super) enum SlotContent {
    Surface(PreviewSurface),
    View { view: View },
}

pub(super) struct Slot {
    pub id: String,
    pub content: SlotContent,
}

impl Slot {
    pub fn view(&self) -> Option<&View> {
        match &self.content {
            SlotContent::View { view, .. } => Some(view),
            _ => None,
        }
    }
    pub fn view_mut(&mut self) -> Option<&mut View> {
        match &mut self.content {
            SlotContent::View { view, .. } => Some(view),
            _ => None,
        }
    }
    pub fn buffer(&self) -> BufferId {
        match &self.content {
            SlotContent::Surface(surface) => surface.buffer,
            SlotContent::View { view, .. } => view.location.buffer,
        }
    }
    pub fn target(&self, page: PageId) -> RenderTarget {
        match &self.content {
            SlotContent::Surface(_) => RenderTarget::Surface {
                page,
                slot: self.id.clone(),
            },
            SlotContent::View { view, .. } => RenderTarget::View(view.id),
        }
    }
}

#[derive(PartialEq)]
pub(super) enum PreviewPublication {
    Pending,
    Ready,
}

pub(super) struct PreviewSlots {
    pub publication: PreviewPublication,
    pub members: Vec<Slot>,
    pub active: String,
    pub layout: PreviewLayout,
}

impl PreviewSlots {
    pub(super) fn contains(&self, view: ViewId) -> bool {
        self.members
            .iter()
            .any(|slot| slot.view().is_some_and(|current| current.id == view))
    }

    fn rectangles(&self, page: PageId, rect: Rect, out: &mut Vec<(RenderTarget, Rect)>) {
        for (slot, area) in self
            .members
            .iter()
            .zip(self.layout.rectangles(rect, self.members.len()))
        {
            out.push((slot.target(page), area));
        }
    }
}

pub(super) struct Page {
    pub main: View,
    pub previews: Option<PreviewSlots>,
    pub navigation: navigation::History,
}

impl Page {
    fn views(&self) -> impl Iterator<Item = (&ViewId, &View)> {
        std::iter::once((&self.main.id, &self.main)).chain(
            self.previews
                .iter()
                .flat_map(|previews| &previews.members)
                .filter_map(|slot| slot.view().map(|view| (&view.id, view))),
        )
    }

    fn views_mut(&mut self) -> impl Iterator<Item = (ViewId, &mut View)> {
        std::iter::once((self.main.id, &mut self.main)).chain(
            self.previews
                .iter_mut()
                .flat_map(|previews| &mut previews.members)
                .filter_map(|slot| slot.view_mut().map(|view| (view.id, view))),
        )
    }

    pub(super) fn references_buffer(&self, buffer: BufferId) -> bool {
        self.views().any(|(_, view)| view.location.buffer == buffer)
            || self
                .previews
                .iter()
                .flat_map(|previews| &previews.members)
                .any(|slot| slot.buffer() == buffer)
            || self.navigation.references_buffer(buffer)
    }

    pub(super) fn rectangles(&self, rect: Rect, out: &mut Vec<(RenderTarget, Rect)>) {
        if let Some(previews) = &self.previews {
            let (main, preview) = rect.split_preview();
            out.push((RenderTarget::View(self.main.id), main));
            previews.rectangles(PageId(self.main.id), preview, out);
        } else {
            out.push((RenderTarget::View(self.main.id), rect));
        }
    }

    fn into_views(self) -> impl Iterator<Item = (ViewId, View)> {
        std::iter::once((self.main.id, self.main)).chain(
            self.previews
                .into_iter()
                .flat_map(|previews| previews.members)
                .filter_map(|slot| match slot.content {
                    SlotContent::View { view, .. } => Some((view.id, view)),
                    _ => None,
                }),
        )
    }
}

#[derive(Default)]
pub(super) struct Store {
    pages: BTreeMap<PageId, Page>,
    locations: HashMap<ViewId, PageId>,
}

impl Store {
    pub(super) fn create_page(&mut self, view: View) -> PageId {
        let id = view.id;
        let page = PageId(id);
        self.pages.insert(
            page,
            Page {
                main: view,
                previews: None,
                navigation: navigation::History::default(),
            },
        );
        self.locations.insert(id, page);
        page
    }

    pub(super) fn page_id(&self, view: ViewId) -> PageId {
        self.locations[&view]
    }

    pub(super) fn focus_target(&self, view: ViewId) -> FocusTarget {
        FocusTarget {
            page: self.page_id(view),
            preferred: view,
        }
    }

    pub(super) fn resolve_focus(&self, target: FocusTarget) -> Option<ViewId> {
        let page = self.pages.get(&target.page)?;
        Some(
            if self.locations.get(&target.preferred) == Some(&target.page) {
                target.preferred
            } else {
                page.main.id
            },
        )
    }

    pub(super) fn page(&self, view: ViewId) -> &Page {
        &self.pages[&self.page_id(view)]
    }

    fn page_mut(&mut self, view: ViewId) -> &mut Page {
        let page = self.page_id(view);
        self.pages.get_mut(&page).unwrap()
    }

    pub(super) fn navigation_mut(&mut self, view: ViewId) -> &mut navigation::History {
        &mut self.page_mut(view).navigation
    }

    pub(super) fn activate_preview(&mut self, view: ViewId) {
        if let Some(previews) = &mut self.page_mut(view).previews
            && previews.contains(view)
        {
            previews.active = previews
                .members
                .iter()
                .find(|slot| slot.view().is_some_and(|current| current.id == view))
                .unwrap()
                .id
                .clone();
        }
    }

    pub(super) fn main(&self, page: PageId) -> ViewId {
        self.pages[&page].main.id
    }

    pub(super) fn pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.values()
    }

    pub(super) fn previews(&self, page: PageId) -> Option<&PreviewSlots> {
        self.pages.get(&page)?.previews.as_ref()
    }

    pub(super) fn previews_mut(&mut self, page: PageId) -> Option<&mut PreviewSlots> {
        self.pages.get_mut(&page)?.previews.as_mut()
    }

    pub(super) fn create_preview(&mut self, page: PageId, slot: &str, view: View) {
        let id = view.id;
        let previews = self
            .pages
            .get_mut(&page)
            .unwrap()
            .previews
            .get_or_insert_with(|| PreviewSlots {
                publication: PreviewPublication::Ready,
                members: Vec::new(),
                active: slot.into(),
                layout: PreviewLayout::Rows,
            });
        previews.members.push(Slot {
            id: slot.into(),
            content: SlotContent::View { view },
        });
        self.locations.insert(id, page);
    }

    pub(super) fn surface(&self, page: PageId, slot: &str) -> Option<&PreviewSurface> {
        match &self
            .previews(page)?
            .members
            .iter()
            .find(|member| member.id == slot)?
            .content
        {
            SlotContent::Surface(surface) => Some(surface),
            _ => None,
        }
    }

    pub(super) fn surface_mut(&mut self, page: PageId, slot: &str) -> Option<&mut PreviewSurface> {
        match &mut self
            .pages
            .get_mut(&page)?
            .previews
            .as_mut()?
            .members
            .iter_mut()
            .find(|member| member.id == slot)?
            .content
        {
            SlotContent::Surface(surface) => Some(surface),
            _ => None,
        }
    }

    pub(super) fn take_previews(&mut self, page: PageId) -> Vec<Slot> {
        let Some(previews) = self.pages.get_mut(&page).unwrap().previews.take() else {
            return Vec::new();
        };
        for slot in &previews.members {
            if let Some(view) = slot.view() {
                self.locations.remove(&view.id);
            }
        }
        previews.members
    }

    pub(super) fn set_previews(&mut self, page: PageId, previews: PreviewSlots) {
        for slot in &previews.members {
            if let Some(view) = slot.view() {
                self.locations.insert(view.id, page);
            }
        }
        self.pages.get_mut(&page).unwrap().previews = Some(previews);
    }

    pub(super) fn take_page(&mut self, id: PageId) -> Page {
        let page = self.pages.remove(&id).unwrap();
        for (id, _) in page.views() {
            self.locations.remove(id);
        }
        page
    }

    pub(super) fn get(&self, id: &ViewId) -> Option<&View> {
        let page = self
            .locations
            .get(id)
            .and_then(|page| self.pages.get(page))?;
        page.views()
            .find_map(|(current, view)| (current == id).then_some(view))
    }

    pub(super) fn get_mut(&mut self, id: &ViewId) -> Option<&mut View> {
        let page = self
            .locations
            .get(id)
            .and_then(|page| self.pages.get_mut(page))?;
        page.views_mut()
            .find_map(|(current, view)| (current == *id).then_some(view))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&ViewId, &View)> {
        self.pages.values().flat_map(Page::views)
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = (ViewId, &mut View)> {
        self.pages.values_mut().flat_map(Page::views_mut)
    }

    pub(super) fn values(&self) -> impl Iterator<Item = &View> {
        self.iter().map(|(_, view)| view)
    }

    pub(super) fn values_mut(&mut self) -> impl Iterator<Item = &mut View> {
        self.iter_mut().map(|(_, view)| view)
    }

    pub(super) fn contains_key(&self, view: &ViewId) -> bool {
        self.locations.contains_key(view)
    }

    pub(super) fn references_buffer(&self, buffer: BufferId) -> bool {
        self.pages
            .values()
            .any(|page| page.references_buffer(buffer))
    }

    pub(super) fn rectangles(&self, page: PageId, rect: Rect, out: &mut Vec<(RenderTarget, Rect)>) {
        self.pages[&page].rectangles(rect, out);
    }
}

impl Index<&ViewId> for Store {
    type Output = View;

    fn index(&self, id: &ViewId) -> &Self::Output {
        self.get(id).unwrap()
    }
}

impl Session {
    fn new_view(&mut self, buffer: BufferId) -> View {
        let id = self.id();
        if let Some(text) = self.buffers.get_mut(&buffer).and_then(Buffer::text) {
            text.ensure_view(id);
        }
        let mut view = View::new(id, buffer);
        if let Buffer::Items { display, .. } = &self.buffers[&buffer] {
            view.project_items(display);
        } else if let Buffer::PreviewRows { display, .. } = &self.buffers[&buffer] {
            view.location.set_display(display.clone());
        } else if let Buffer::PreviewDiff { diff, .. } = &self.buffers[&buffer] {
            if diff.mapped_rendering() {
                view.location.diff_presentation = diff::Presentation::Rendered;
            }
            view.location.diff_selection = diff
                .index(view.location.diff_presentation)
                .initial_selection();
        }
        view
    }

    pub(super) fn create_page(&mut self, buffer: BufferId) -> ViewId {
        let view = self.new_view(buffer);
        let id = view.id;
        self.views.create_page(view);
        id
    }

    pub(super) fn create_preview(&mut self, page: PageId, slot: &str, buffer: BufferId) -> ViewId {
        let view = self.new_view(buffer);
        let id = view.id;
        self.views.create_preview(page, slot, view);
        id
    }

    pub(super) fn retire_view(&mut self, id: ViewId, view: View) {
        self.cancel_diff_source(Some(id));
        for buffer in self.buffers.values_mut() {
            if let Some(text) = buffer.text() {
                text.remove_view(id);
            }
        }
        if let Some(runtime) = &mut self.scripts {
            runtime.remove_view(id);
        }
        self.remove_unused_preview_buffer(view.location.buffer);
    }

    pub(super) fn first_layout_view(&self) -> ViewId {
        let mut pages = Vec::new();
        self.layout.as_ref().unwrap().leaves(&mut pages);
        let mut view = self.views.main(pages[0]);
        while let Some(layer) = self.views[&view].layer {
            view = layer;
        }
        view
    }

    pub(super) fn leave_page(&mut self, page: PageId) -> Result<()> {
        self.cancel_input_context()?;
        let main = self.views.main(page);
        self.dispatch(main, "view_leaving", Value::Null)?;
        self.close_preview(page)
    }

    pub(super) fn retire_page(&mut self, page: PageId) {
        let page = self.views.take_page(page);
        let buffers = page
            .previews
            .iter()
            .flat_map(|previews| &previews.members)
            .map(Slot::buffer)
            .collect::<Vec<_>>();
        for (id, view) in page.into_views() {
            self.retire_view(id, view);
        }
        for buffer in buffers {
            self.remove_unused_preview_buffer(buffer);
        }
    }
}
