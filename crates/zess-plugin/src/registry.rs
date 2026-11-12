use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::{
    BufferKindId, BufferPluginHandle, CommandCatalog, CommandImplementation, EditorOptions,
    HostRequest, IdentityRegistry, Keymap, ModeId, PluginRegistrar,
};

#[derive(Clone)]
pub enum BufferBackend<H> {
    External(H),
    Native(BufferPluginHandle),
}

#[derive(Clone)]
pub struct ModeRegistration {
    pub kind: BufferKindId,
    pub name: ModeId,
}

struct ContextRegistration {
    field: String,
    events: Vec<String>,
}

pub struct Registry<H> {
    identities: IdentityRegistry,
    buffers: BTreeMap<BufferKindId, BufferBackend<H>>,
    catalog: CommandCatalog,
    commands: BTreeMap<String, CommandImplementation<H>>,
    pub apis: BTreeMap<String, H>,
    pub events: BTreeMap<String, H>,
    contexts: BTreeMap<BufferKindId, Vec<ContextRegistration>>,
    pub statuslines: BTreeMap<String, H>,
    view_defaults: BTreeMap<BufferKindId, serde_json::Map<String, Value>>,
    view_options: BTreeMap<BufferKindId, Value>,
    pub editor_options: EditorOptions,
    pub on_client_exit: crate::OnClientExit,
    pub keymap: Keymap,
    modes: Vec<ModeRegistration>,
}

impl<H> Default for Registry<H> {
    fn default() -> Self {
        Self::with_identities(IdentityRegistry::default())
    }
}

impl<H> Registry<H> {
    pub fn with_identities(identities: IdentityRegistry) -> Self {
        Self {
            keymap: Keymap::with_identities(identities.clone()),
            identities,
            buffers: BTreeMap::new(),
            catalog: CommandCatalog::default(),
            commands: BTreeMap::new(),
            apis: BTreeMap::new(),
            events: BTreeMap::new(),
            contexts: BTreeMap::new(),
            statuslines: BTreeMap::new(),
            view_defaults: BTreeMap::new(),
            view_options: BTreeMap::new(),
            editor_options: EditorOptions::default(),
            on_client_exit: crate::OnClientExit::default(),
            modes: Vec::new(),
        }
    }

    pub fn identities(&self) -> &IdentityRegistry {
        &self.identities
    }

    pub fn register_buffer(&mut self, kind: &str, backend: BufferBackend<H>) {
        self.buffers
            .insert(self.identities.intern_buffer_kind(kind), backend);
    }

    pub fn buffer_backend(&self, kind: &str) -> Option<&BufferBackend<H>> {
        self.identities
            .buffer_kind(kind)
            .and_then(|kind| self.buffers.get(&kind))
    }

    pub fn buffer_by_id(&self, kind: BufferKindId) -> Option<&BufferBackend<H>> {
        self.buffers.get(&kind)
    }

    pub fn buffer_kinds(&self) -> impl Iterator<Item = BufferKindId> + '_ {
        self.buffers.keys().copied()
    }

    pub fn buffer_backends(&self) -> impl Iterator<Item = &BufferBackend<H>> {
        self.buffers.values()
    }

    pub fn set_view_options(&mut self, kind: &str, options: Value) {
        self.view_options
            .insert(self.identities.intern_buffer_kind(kind), options);
    }

    pub fn catalog(&self) -> &CommandCatalog {
        &self.catalog
    }

    pub fn register_command(
        &mut self,
        id: &str,
        description: &str,
        implementation: CommandImplementation<H>,
    ) {
        self.register_action(id, description, implementation);
        self.register_command_line(id, id);
    }

    pub fn register_action(
        &mut self,
        id: &str,
        description: &str,
        implementation: CommandImplementation<H>,
    ) {
        self.catalog.register_action(id, description);
        self.commands.insert(id.to_owned(), implementation);
    }

    pub fn register_command_line(&mut self, id: &str, name: &str) {
        self.catalog.register_command_line(id, name);
    }

    pub fn register_command_alias(&mut self, alias: &str, name: &str) {
        self.catalog.register_alias(alias, name);
    }

    pub fn lookup_command(&self, name: &str) -> Result<(&str, &CommandImplementation<H>)> {
        let name = self.catalog.resolve_name(name);
        self.commands
            .get_key_value(name)
            .map(|(name, implementation)| (name.as_str(), implementation))
            .ok_or_else(|| anyhow!("unknown command: {name}"))
    }

    pub fn register_context(&mut self, kind: &str, field: &str, events: Vec<String>) {
        let fields = self
            .contexts
            .entry(self.identities.intern_buffer_kind(kind))
            .or_default();
        if let Some(registration) = fields
            .iter_mut()
            .find(|registration| registration.field == field)
        {
            registration.events = events;
        } else {
            fields.push(ContextRegistration {
                field: field.to_owned(),
                events,
            });
        }
    }

    pub fn context_fields(&self, kind: &str, event: &str) -> Vec<String> {
        self.identities
            .buffer_kind(kind)
            .and_then(|kind| self.contexts.get(&kind))
            .into_iter()
            .flatten()
            .filter(|registration| {
                registration.events.is_empty()
                    || registration.events.iter().any(|name| name == event)
            })
            .map(|registration| registration.field.clone())
            .collect()
    }

    pub fn modes(&self) -> &[ModeRegistration] {
        &self.modes
    }

    pub fn view_options_with_overrides(
        &self,
        kind: &str,
        overrides: Option<&serde_json::Map<String, Value>>,
    ) -> Value {
        let mut options = serde_json::Map::new();
        if let Some(kind) = self.identities.buffer_kind(kind) {
            if let Some(defaults) = self.view_defaults.get(&kind) {
                options.clone_from(defaults);
            }
            if let Some(configured) = self.view_options.get(&kind).and_then(Value::as_object) {
                options.extend(configured.clone());
            }
        }
        if let Some(overrides) = overrides {
            options.extend(overrides.clone());
        }
        Value::Object(options)
    }

    pub fn view_options(&self, kind: &str) -> Value {
        self.view_options_with_overrides(kind, None)
    }

    pub fn validate_view_options_value(&self, kind: Option<&str>, options: &Value) -> Result<()> {
        if !options.is_object() {
            return Err(anyhow!("view options must be an object"));
        }
        if let Some(statusline) = options.get("statusline") {
            let statusline = statusline
                .as_str()
                .ok_or_else(|| anyhow!("statusline must be a string"))?;
            if !statusline.is_empty() && !self.statuslines.contains_key(statusline) {
                return Err(anyhow!("unknown statusline {statusline}"));
            }
        }
        if let Some(formats) = options.get("status-formats") {
            crate::validate_status_formats(formats)?;
        }
        if options
            .get("header-style")
            .is_some_and(|style| !style.is_string())
        {
            return Err(anyhow!("header-style must be a string"));
        }
        if options
            .get("gutter")
            .is_some_and(|gutter| !gutter.is_boolean())
        {
            return Err(anyhow!("gutter must be a boolean"));
        }
        if options
            .get("double-click-command")
            .is_some_and(|command| !command.is_string())
        {
            return Err(anyhow!("double-click-command must be a string"));
        }
        if let Some(register) = options.get("default-yank-register") {
            let register = register
                .as_str()
                .ok_or_else(|| anyhow!("default-yank-register must be a single character"))?
                .parse::<char>()
                .context("default-yank-register must be a single character")?;
            crate::validate_register_write(register)?;
        }
        if let Some(BufferBackend::Native(backend)) =
            kind.and_then(|kind| self.buffer_backend(kind))
        {
            backend.borrow().validate_view_options(options)?;
        }
        Ok(())
    }

    pub fn validate_view_options(&self) -> Result<()> {
        let kinds = self
            .view_defaults
            .keys()
            .chain(self.view_options.keys())
            .collect::<BTreeSet<_>>();
        for kind in kinds {
            let name = self.identities.buffer_kind_name(*kind).to_string();
            self.validate_view_options_value(Some(&name), &self.view_options(&name))
                .with_context(|| format!("view options for {name}"))?;
        }
        Ok(())
    }

    pub fn validate_bindings(&self) -> Result<()> {
        let scopes_and_modes = self.keymap.scopes_and_modes().collect::<BTreeSet<_>>();
        for (scope, mode) in scopes_and_modes {
            let scope = self.identities.scope_name(scope);
            let mode = self.identities.mode_name(mode);
            let scopes = if &*scope == "global" {
                vec!["global"]
            } else {
                vec!["global", &scope]
            };
            self.validate_bindings_for(&scopes, &mode)?;
        }
        Ok(())
    }

    pub fn validate_bindings_for(&self, scopes: &[&str], mode: &str) -> Result<()> {
        for binding in self.keymap.effective_bindings(scopes, mode) {
            if self.catalog.resolve(binding.command).is_none() {
                return Err(anyhow!(
                    "unknown command {} in key mapping {} ({}, mode {mode})",
                    binding.command,
                    binding.keys.join(" "),
                    scopes.join(" -> ")
                ));
            }
        }
        Ok(())
    }
}

impl<H> PluginRegistrar for Registry<H> {
    fn view_defaults(&mut self, kind: &str, options: serde_json::Map<String, Value>) {
        self.view_defaults
            .insert(self.identities.intern_buffer_kind(kind), options);
    }

    fn scope(&mut self, id: &str, label: &str) {
        self.keymap.register_scope(id, label);
    }

    fn buffer(&mut self, kind: &str, backend: BufferPluginHandle) {
        self.register_buffer(kind, BufferBackend::Native(backend));
    }

    fn command(&mut self, name: &str, description: &str, request: HostRequest) {
        self.register_command(name, description, CommandImplementation::Request(request));
    }

    fn buffer_command(&mut self, name: &str, description: &str, kind: &str, event: &str) {
        self.register_command(
            name,
            description,
            CommandImplementation::BufferEvent {
                kind: kind.into(),
                event: event.into(),
            },
        );
    }

    fn binding(&mut self, scope: &str, mode: &str, keys: &[&str], command: &str) {
        self.keymap.map(
            scope,
            mode,
            keys.iter().map(|key| (*key).to_owned()).collect(),
            Some(command.into()),
        );
    }

    fn mode(&mut self, kind: &str, name: &str) {
        self.modes.push(ModeRegistration {
            kind: self.identities.intern_buffer_kind(kind),
            name: self.identities.intern_mode(name),
        });
    }
}
