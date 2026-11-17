use super::*;
use zess_git::{DiffSide, DiffSource, DiffTarget, PatchRequest};
use zess_protocol::resource::PagePatch;

#[derive(Default)]
pub(super) struct PublishedPatches {
    files: Vec<(DiffSide, DiffTarget, String)>,
    revision: u64,
    ready: bool,
}

impl PublishedPatches {
    pub(super) fn update(&mut self, requests: &[HostRequest]) -> Option<u64> {
        for request in requests.iter().rev() {
            match request {
                HostRequest::PreviewGroup { panes, .. } => return self.publish(panes),
                HostRequest::Preview { .. } | HostRequest::PreviewPending => {
                    self.ready = false;
                    return None;
                }
                _ => {}
            }
        }
        None
    }

    fn publish(&mut self, panes: &[PreviewPane]) -> Option<u64> {
        if panes.iter().any(PreviewPane::loading) {
            self.ready = false;
            return None;
        }
        let files = panes
            .iter()
            .filter_map(|pane| {
                let PreviewPane::View {
                    content: PreviewContent::GitDiff { diff },
                    ..
                } = pane
                else {
                    return None;
                };
                let DiffSource::Changes { side } = diff.source else {
                    return None;
                };
                Some(
                    diff.files
                        .iter()
                        .map(move |file| (side, &file.target, &file.raw)),
                )
            })
            .flatten();
        if !files.clone().eq(self
            .files
            .iter()
            .map(|(side, target, raw)| (*side, target, raw)))
        {
            self.files = files
                .map(|(side, target, raw)| (side, target.clone(), raw.clone()))
                .collect();
            self.revision += 1;
        }
        self.ready = !self.files.is_empty();
        self.ready.then_some(self.revision)
    }

    fn resolve(&self, selection: &PagePatch) -> Result<PatchRequest> {
        anyhow::ensure!(
            self.ready && self.revision == selection.revision,
            "Git preview changed; select the changes again"
        );
        let (_, target, raw) = self
            .files
            .iter()
            .find(|(side, target, _)| *side == selection.side && *target == selection.target)
            .context("select the changes from the current Git preview")?;
        Ok(PatchRequest {
            side: selection.side,
            path: target.path.clone(),
            original_path: target.original_path.clone(),
            raw: raw.clone(),
            rows: selection.rows.clone(),
        })
    }
}

impl Session {
    pub(in crate::editor) fn refresh_patch_page(&mut self, buffer: BufferId) -> Result<()> {
        if let Some((&id, _)) = self
            .resources
            .active
            .iter()
            .find(|(_, page)| page.buffer == buffer)
        {
            self.dispatch_page(id, "refresh", Value::Null)?;
        }
        Ok(())
    }

    pub(in crate::editor) fn apply_page_patch(
        &mut self,
        id: u64,
        selection: &PagePatch,
    ) -> Result<()> {
        let page = self
            .resources
            .active
            .get(&id)
            .context("page subscription is closed")?;
        let patch = page.patches.resolve(selection)?;
        self.start_git_patch(
            preview::PatchTarget::Subscription {
                buffer: page.buffer,
            },
            patch,
        )
    }
}
