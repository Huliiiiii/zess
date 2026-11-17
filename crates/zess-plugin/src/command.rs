use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::HostRequest;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandInfo {
    pub id: String,
    pub description: String,
    pub command_line: Option<CommandLineEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandLineEntry {
    pub name: String,
    pub aliases: Vec<String>,
}

#[derive(Clone)]
pub enum CommandImplementation<H> {
    Native,
    Request(HostRequest),
    External(H),
    BufferEvent { kind: String, event: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CommandCatalog {
    commands: BTreeMap<String, CommandInfo>,
    aliases: BTreeMap<String, String>,
}

impl CommandCatalog {
    pub fn iter(&self) -> impl Iterator<Item = &CommandInfo> {
        self.commands.values()
    }

    pub fn resolve(&self, name: &str) -> Option<&CommandInfo> {
        self.commands.get(self.resolve_name(name))
    }

    pub(super) fn resolve_name<'a>(&'a self, name: &'a str) -> &'a str {
        if self.commands.contains_key(name) {
            name
        } else {
            self.aliases.get(name).map_or(name, String::as_str)
        }
    }

    pub fn command_line_entries(&self) -> impl Iterator<Item = (&CommandInfo, &CommandLineEntry)> {
        self.commands
            .values()
            .filter_map(|command| Some((command, command.command_line.as_ref()?)))
    }

    pub fn resolve_command_line(&self, name: &str) -> Option<&CommandInfo> {
        if let Some(id) = self.aliases.get(name) {
            return self
                .commands
                .get(id)
                .filter(|command| command.command_line.is_some());
        }
        self.command_line_entries()
            .find(|(_, entry)| entry.name == name)
            .map(|(command, _)| command)
    }

    pub(super) fn register_action(&mut self, id: &str, description: &str) {
        self.commands.insert(
            id.to_owned(),
            CommandInfo {
                id: id.to_owned(),
                description: description.to_owned(),
                command_line: None,
            },
        );
    }

    pub(super) fn register_command_line(&mut self, id: &str, name: &str) {
        for command in self.commands.values_mut() {
            if command.id != id
                && command
                    .command_line
                    .as_ref()
                    .is_some_and(|entry| entry.name == name)
            {
                command.command_line = None;
            }
        }
        let aliases = self
            .aliases
            .iter()
            .filter_map(|(alias, target)| (target == id).then(|| alias.clone()))
            .collect();
        if let Some(command) = self.commands.get_mut(id) {
            command.command_line = Some(CommandLineEntry {
                name: name.to_owned(),
                aliases,
            });
        }
    }

    pub fn register_alias(&mut self, alias: &str, name: &str) {
        let id = self
            .resolve_command_line(name)
            .map_or_else(|| name.to_owned(), |command| command.id.clone());
        if let Some(previous) = self.aliases.insert(alias.to_owned(), id.clone())
            && let Some(entry) = self
                .commands
                .get_mut(&previous)
                .and_then(|command| command.command_line.as_mut())
        {
            entry.aliases.retain(|entry| entry != alias);
        }
        if let Some(entry) = self
            .commands
            .get_mut(&id)
            .and_then(|command| command.command_line.as_mut())
        {
            entry.aliases.push(alias.to_owned());
            entry.aliases.sort();
        }
    }
}
