pub(super) use zess_text::diff::*;

pub(super) struct Search {
    pub forward: bool,
    pub selection: Selection,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub scroll_mode: super::ScrollMode,
}

impl Search {
    pub fn restore(&self, view: &mut super::View) {
        view.location.diff_selection = self.selection.clone();
        view.location.scroll = self.scroll;
        view.location.horizontal_scroll = self.horizontal_scroll;
        view.location.scroll_mode = self.scroll_mode;
    }
}
