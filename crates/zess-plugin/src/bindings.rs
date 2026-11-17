use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{CommandCatalog, HostRequest, Keymap, keymap::KeymapDefinition};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum BindingCommand {
    Native,
    Request(HostRequest),
    BufferEvent { kind: String, event: String },
    Script,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BindingCatalog {
    pub commands: CommandCatalog,
    pub status: crate::StatusConfig,
    pub(crate) implementations: BTreeMap<String, BindingCommand>,
    pub(crate) keymap: KeymapDefinition,
}

impl BindingCatalog {
    pub fn keymap(&self) -> Keymap {
        Keymap::from_definition(&self.keymap)
    }

    pub fn implementation(&self, name: &str) -> Option<&BindingCommand> {
        self.implementations.get(self.commands.resolve_name(name))
    }
}
