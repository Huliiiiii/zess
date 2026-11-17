use zess_protocol::bindings::BindingCatalog;

use super::Session;

impl Session {
    pub fn binding_revision(&self) -> (u64, u64) {
        (
            self.generation,
            self.scripts
                .as_ref()
                .map_or(0, |scripts| scripts.binding_revision()),
        )
    }

    pub fn binding_catalog(&self) -> BindingCatalog {
        self.scripts
            .as_ref()
            .map_or_else(BindingCatalog::default, |scripts| scripts.binding_catalog())
    }
}
