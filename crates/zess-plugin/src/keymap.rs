use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use crate::{IdentityRegistry, KeyScopeId, ModeId};
use smallvec::SmallVec;

#[cfg(test)]
mod tests;

pub struct ResolvedBinding<'a> {
    pub keys: &'a [String],
    pub command: &'a str,
    pub scope: KeyScopeId,
}

pub enum KeyResolution<C = String> {
    Command(C),
    Prefix,
    Unbound,
}

#[derive(Clone, Copy, Default)]
struct NodeId(usize);

#[derive(Clone, Default)]
pub struct KeyState {
    context: Option<Rc<CompiledContext>>,
    node: NodeId,
}

impl KeyState {
    pub fn reset(&mut self) {
        self.node = NodeId(0);
    }

    pub fn shares_context(&self, other: &Self) -> bool {
        match (&self.context, &other.context) {
            (Some(current), Some(previous)) => Rc::ptr_eq(current, previous),
            (None, None) => true,
            _ => false,
        }
    }
}

struct KeyMapping {
    scope: KeyScopeId,
    mode: ModeId,
    keys: Vec<String>,
    command: Option<String>,
}

pub struct Keymap {
    identities: IdentityRegistry,
    scopes: BTreeMap<KeyScopeId, String>,
    bindings: Vec<KeyMapping>,
    revision: Rc<Cell<u64>>,
    compiled: RefCell<Vec<Rc<CompiledContext>>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::with_identities(IdentityRegistry::default())
    }
}

impl Keymap {
    pub fn with_identities(identities: IdentityRegistry) -> Self {
        Self {
            identities,
            scopes: BTreeMap::new(),
            bindings: Vec::new(),
            revision: Rc::new(Cell::new(0)),
            compiled: RefCell::new(Vec::new()),
        }
    }

    pub fn identities(&self) -> &IdentityRegistry {
        &self.identities
    }

    pub fn register_scope(&mut self, scope: &str, label: &str) {
        self.scopes
            .insert(self.identities.intern_scope(scope), label.to_owned());
    }

    pub fn scope_label<'a>(&'a self, scope: &'a str) -> &'a str {
        self.identities
            .scope(scope)
            .and_then(|id| self.scopes.get(&id))
            .map_or(scope, String::as_str)
    }

    pub fn scope_label_id(&self, scope: KeyScopeId) -> Option<&str> {
        self.scopes.get(&scope).map(String::as_str)
    }

    pub fn map(&mut self, scope: &str, mode: &str, keys: Vec<String>, command: Option<String>) {
        self.compiled.get_mut().clear();
        self.revision.set(self.revision.get() + 1);
        self.bindings.push(KeyMapping {
            scope: self.identities.intern_scope(scope),
            mode: self.identities.intern_mode(mode),
            keys,
            command,
        });
    }

    pub fn effective_bindings<'a>(
        &'a self,
        scopes: &[&str],
        mode: &str,
    ) -> Vec<ResolvedBinding<'a>> {
        let scopes = scopes
            .iter()
            .filter_map(|scope| self.identities.scope(scope))
            .collect::<SmallVec<[_; 4]>>();
        let Some(mode) = self.identities.mode(mode) else {
            return Vec::new();
        };
        self.effective_bindings_ids(&scopes, mode)
    }

    pub fn effective_bindings_ids(
        &self,
        scopes: &[KeyScopeId],
        mode: ModeId,
    ) -> Vec<ResolvedBinding<'_>> {
        let compiled = self.compiled_context(scopes, mode);
        let mut bindings = Vec::new();
        compiled.for_each_binding(NodeId(0), &mut |index| {
            let mapping = &self.bindings[index];
            bindings.push(ResolvedBinding {
                keys: &mapping.keys,
                command: mapping.command.as_deref().unwrap(),
                scope: mapping.scope,
            });
        });
        bindings
    }

    pub fn resolve<'a>(
        &'a self,
        scopes: &[&str],
        mode: &str,
        keys: &[String],
    ) -> KeyResolution<&'a str> {
        let scopes = scopes
            .iter()
            .filter_map(|scope| self.identities.scope(scope))
            .collect::<SmallVec<[_; 4]>>();
        let Some(mode) = self.identities.mode(mode) else {
            return KeyResolution::Unbound;
        };
        self.resolve_ids(&scopes, mode, keys)
    }

    pub fn resolve_ids<'a>(
        &'a self,
        scopes: &[KeyScopeId],
        mode: ModeId,
        keys: &[String],
    ) -> KeyResolution<&'a str> {
        let compiled = self.compiled_context(scopes, mode);
        let mut node = &compiled.nodes[0];
        for key in keys {
            let Some(child) = node.children.get(key) else {
                return KeyResolution::Unbound;
            };
            node = &compiled.nodes[child.0];
        }
        if let Some(index) = node.binding {
            KeyResolution::Command(self.bindings[index].command.as_deref().unwrap())
        } else if !node.children.is_empty() {
            KeyResolution::Prefix
        } else {
            KeyResolution::Unbound
        }
    }

    pub fn advance<'a>(
        &'a self,
        state: &mut KeyState,
        scopes: &[&str],
        mode: &str,
        key: &str,
    ) -> KeyResolution<&'a str> {
        let scopes = scopes
            .iter()
            .filter_map(|scope| self.identities.scope(scope))
            .collect::<SmallVec<[_; 4]>>();
        let Some(mode) = self.identities.mode(mode) else {
            state.context = None;
            state.reset();
            return KeyResolution::Unbound;
        };
        self.advance_ids(state, &scopes, mode, key)
    }

    pub fn advance_ids<'a>(
        &'a self,
        state: &mut KeyState,
        scopes: &[KeyScopeId],
        mode: ModeId,
        key: &str,
    ) -> KeyResolution<&'a str> {
        let current = state.context.as_ref().is_some_and(|context| {
            Rc::ptr_eq(&context.revision, &self.revision)
                && context.built_at == self.revision.get()
                && context.mode == mode
                && context.scopes == scopes
        });
        if !current {
            state.context = Some(self.compiled_context(scopes, mode));
            state.reset();
        }
        let context = state.context.as_ref().unwrap();
        let Some(next) = context.nodes[state.node.0].children.get(key).copied() else {
            state.reset();
            return KeyResolution::Unbound;
        };
        let node = &context.nodes[next.0];
        if let Some(binding) = node.binding {
            state.reset();
            KeyResolution::Command(self.bindings[binding].command.as_deref().unwrap())
        } else {
            state.node = next;
            KeyResolution::Prefix
        }
    }

    fn compiled_context(&self, scopes: &[KeyScopeId], mode: ModeId) -> Rc<CompiledContext> {
        let compiled = self.compiled.borrow();
        if let Some(context) = compiled
            .iter()
            .find(|context| context.mode == mode && context.scopes == scopes)
        {
            return context.clone();
        }
        drop(compiled);

        let mut root = KeyTrie::default();
        for scope in scopes {
            for (index, mapping) in self.bindings.iter().enumerate() {
                if mapping.scope != *scope || mapping.mode != mode {
                    continue;
                }
                if mapping.command.is_some() {
                    root.insert(&mapping.keys, index);
                } else {
                    root.remove(&mapping.keys);
                }
            }
        }
        let mut nodes = Vec::new();
        root.flatten(&mut nodes);
        let context = Rc::new(CompiledContext {
            scopes: scopes.to_owned(),
            mode,
            revision: self.revision.clone(),
            built_at: self.revision.get(),
            nodes,
        });
        self.compiled.borrow_mut().push(context.clone());
        context
    }

    pub(super) fn scopes_and_modes(&self) -> impl Iterator<Item = (KeyScopeId, ModeId)> {
        self.bindings
            .iter()
            .map(|mapping| (mapping.scope, mapping.mode))
    }
}

struct CompiledContext {
    scopes: Vec<KeyScopeId>,
    mode: ModeId,
    revision: Rc<Cell<u64>>,
    built_at: u64,
    nodes: Vec<KeyNode>,
}

impl CompiledContext {
    fn for_each_binding(&self, node: NodeId, visit: &mut impl FnMut(usize)) {
        let node = &self.nodes[node.0];
        if let Some(binding) = node.binding {
            visit(binding);
        }
        for child in node.children.values() {
            self.for_each_binding(*child, visit);
        }
    }
}

struct KeyNode {
    binding: Option<usize>,
    children: BTreeMap<String, NodeId>,
}

#[derive(Default)]
struct KeyTrie {
    binding: Option<usize>,
    children: BTreeMap<String, KeyTrie>,
}

impl KeyTrie {
    fn insert(&mut self, keys: &[String], binding: usize) {
        let mut node = self;
        for key in keys {
            node.binding = None;
            node = node.children.entry(key.clone()).or_default();
        }
        node.children.clear();
        node.binding = Some(binding);
    }

    fn remove(&mut self, keys: &[String]) {
        let Some((first, rest)) = keys.split_first() else {
            self.binding = None;
            self.children.clear();
            return;
        };
        if let Some(child) = self.children.get_mut(first) {
            child.remove(rest);
            if child.binding.is_none() && child.children.is_empty() {
                self.children.remove(first);
            }
        }
    }

    fn flatten(self, nodes: &mut Vec<KeyNode>) -> NodeId {
        let id = NodeId(nodes.len());
        nodes.push(KeyNode {
            binding: self.binding,
            children: BTreeMap::new(),
        });
        for (key, child) in self.children {
            let child = child.flatten(nodes);
            nodes[id.0].children.insert(key, child);
        }
        id
    }
}
