use super::*;
use zess_plugin::StatusContext;

impl Session {
    pub(crate) fn format_client_statusline(
        &mut self,
        name: &str,
        context: &StatusContext,
    ) -> Result<zess_plugin::StatusLine> {
        self.scripts
            .as_mut()
            .context("statusline runtime unavailable")?
            .format_statusline(name, context)
    }
}
