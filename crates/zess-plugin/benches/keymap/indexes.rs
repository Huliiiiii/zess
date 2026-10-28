use std::{
    collections::{BTreeMap, HashMap},
    ops::Bound,
};

#[path = "indexes/families.rs"]
mod families;
pub(super) use families::{Art, DoubleArrayIndex, FstIndex, LoudsIndex, QpTrie};

#[path = "indexes/incremental.rs"]
mod incremental;
pub(super) use incremental::{Adaptive, Hybrid, LoudsDelta, Packed};

#[path = "indexes/node_storage.rs"]
mod node_storage;
pub(super) use node_storage::{BTreeNode, InlineNode, LeafNode, NodeStorage};

#[derive(Debug, PartialEq)]
pub(super) enum Resolution {
    Command(usize),
    Prefix,
    Unbound,
}

pub(super) trait Cursor {
    fn advance(&mut self, key: &String) -> Resolution;
    fn reset(&mut self);
}

struct Requery<'a, I> {
    index: &'a I,
    keys: Vec<String>,
}

impl<I: Index> Cursor for Requery<'_, I> {
    fn advance(&mut self, key: &String) -> Resolution {
        self.keys.push(key.clone());
        self.index.resolve(&self.keys)
    }
    fn reset(&mut self) {
        self.keys.clear();
    }
}

pub(super) trait Index: Default {
    // Only whole-sequence baselines use this default; token tries override it.
    fn cursor(&self) -> impl Cursor {
        Requery {
            index: self,
            keys: Vec::new(),
        }
    }
    fn bind(&mut self, keys: &[String], binding: usize);
    fn unmap(&mut self, keys: &[String]);
    fn resolve(&self, keys: &[String]) -> Resolution;
    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize));

    // Static indexes build one snapshot after a declaration batch. Mutation
    // samples call this inside their timer, so rebuilding is never omitted.
    fn finish(&mut self) {}

    fn candidates(&self, prefix: &[String]) -> Vec<usize> {
        let mut bindings = Vec::new();
        self.for_each_candidate(prefix, &mut |binding| bindings.push(binding));
        bindings
    }
}

#[derive(Clone, Default)]
pub(super) struct Trie {
    binding: Option<usize>,
    children: BTreeMap<String, Trie>,
}

impl Trie {
    pub(super) fn degree(&self) -> usize {
        self.children.len()
    }
    pub(super) fn child_nodes(&self) -> impl Iterator<Item = &Self> {
        self.children.values()
    }
    fn node(&self, keys: &[String]) -> Option<&Self> {
        let mut node = self;
        for key in keys {
            node = node.children.get(key)?;
        }
        Some(node)
    }

    fn collect(&self, visit: &mut impl FnMut(usize)) {
        if let Some(binding) = self.binding {
            visit(binding);
        }
        for child in self.children.values() {
            child.collect(visit);
        }
    }
}

struct TrieCursor<'a> {
    root: &'a Trie,
    node: Option<&'a Trie>,
}
impl Cursor for TrieCursor<'_> {
    fn advance(&mut self, key: &String) -> Resolution {
        self.node = self.node.and_then(|node| node.children.get(key));
        match self.node {
            Some(node) if node.binding.is_some() => Resolution::Command(node.binding.unwrap()),
            Some(node) if !node.children.is_empty() => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }
    fn reset(&mut self) {
        self.node = Some(self.root);
    }
}

impl Index for Trie {
    fn cursor(&self) -> impl Cursor {
        TrieCursor {
            root: self,
            node: Some(self),
        }
    }
    fn bind(&mut self, keys: &[String], binding: usize) {
        let mut node = self;
        for key in keys {
            node.binding = None;
            node = node.children.entry(key.clone()).or_default();
        }
        node.children.clear();
        node.binding = Some(binding);
    }

    fn unmap(&mut self, keys: &[String]) {
        let Some((first, rest)) = keys.split_first() else {
            self.binding = None;
            self.children.clear();
            return;
        };
        if let Some(child) = self.children.get_mut(first) {
            child.unmap(rest);
            if child.binding.is_none() && child.children.is_empty() {
                self.children.remove(first);
            }
        }
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        match self.node(keys) {
            Some(Self {
                binding: Some(binding),
                ..
            }) => Resolution::Command(*binding),
            Some(node) if !node.children.is_empty() => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        if let Some(node) = self.node(prefix) {
            node.collect(visit);
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct HashTrie {
    binding: Option<usize>,
    children: HashMap<String, HashTrie>,
}

impl HashTrie {
    fn node(&self, keys: &[String]) -> Option<&Self> {
        let mut node = self;
        for key in keys {
            node = node.children.get(key)?;
        }
        Some(node)
    }

    fn collect(&self, visit: &mut impl FnMut(usize)) {
        if let Some(binding) = self.binding {
            visit(binding);
        }
        if self.children.len() <= 1 {
            for child in self.children.values() {
                child.collect(visit);
            }
            return;
        }
        let mut children = self.children.iter().collect::<Vec<_>>();
        children.sort_unstable_by_key(|(token, _)| *token);
        for (_, child) in children {
            child.collect(visit);
        }
    }
}

struct HashCursor<'a> {
    root: &'a HashTrie,
    node: Option<&'a HashTrie>,
}
impl Cursor for HashCursor<'_> {
    fn advance(&mut self, key: &String) -> Resolution {
        self.node = self.node.and_then(|node| node.children.get(key));
        match self.node {
            Some(node) if node.binding.is_some() => Resolution::Command(node.binding.unwrap()),
            Some(node) if !node.children.is_empty() => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }
    fn reset(&mut self) {
        self.node = Some(self.root);
    }
}

impl Index for HashTrie {
    fn cursor(&self) -> impl Cursor {
        HashCursor {
            root: self,
            node: Some(self),
        }
    }
    fn bind(&mut self, keys: &[String], binding: usize) {
        let mut node = self;
        for key in keys {
            node.binding = None;
            node = node.children.entry(key.clone()).or_default();
        }
        node.children.clear();
        node.binding = Some(binding);
    }

    fn unmap(&mut self, keys: &[String]) {
        let Some((first, rest)) = keys.split_first() else {
            self.binding = None;
            self.children.clear();
            return;
        };
        if let Some(child) = self.children.get_mut(first) {
            child.unmap(rest);
            if child.binding.is_none() && child.children.is_empty() {
                self.children.remove(first);
            }
        }
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        match self.node(keys) {
            Some(Self {
                binding: Some(binding),
                ..
            }) => Resolution::Command(*binding),
            Some(node) if !node.children.is_empty() => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        if let Some(node) = self.node(prefix) {
            node.collect(visit);
        }
    }
}

// Full-sequence hashing handles exact lookup. The second, maintained index is
// necessary for ordered prefix enumeration and efficient subtree removal.
#[derive(Clone, Default)]
pub(super) struct HashIndex {
    exact: HashMap<Vec<String>, usize>,
    ordered: BTreeMap<Vec<String>, usize>,
}

impl Index for HashIndex {
    fn bind(&mut self, keys: &[String], binding: usize) {
        self.unmap(keys);
        for length in 0..keys.len() {
            let ancestor = &keys[..length];
            self.exact.remove(ancestor);
            self.ordered.remove(ancestor);
        }
        self.exact.insert(keys.to_vec(), binding);
        self.ordered.insert(keys.to_vec(), binding);
    }

    fn unmap(&mut self, keys: &[String]) {
        while let Some((candidate, _)) = self
            .ordered
            .range::<[String], _>((Bound::Included(keys), Bound::Unbounded))
            .next()
        {
            if !candidate.starts_with(keys) {
                break;
            }
            let candidate = candidate.clone();
            self.ordered.remove(&candidate);
            self.exact.remove(&candidate);
        }
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        if let Some(binding) = self.exact.get(keys) {
            return Resolution::Command(*binding);
        }
        if self
            .ordered
            .range::<[String], _>((Bound::Included(keys), Bound::Unbounded))
            .next()
            .is_some_and(|(candidate, _)| candidate.starts_with(keys))
        {
            Resolution::Prefix
        } else {
            Resolution::Unbound
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        for (_, binding) in self
            .ordered
            .range::<[String], _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(|(candidate, _)| candidate.starts_with(prefix))
        {
            visit(*binding);
        }
    }
}

#[derive(Default)]
pub(super) struct Sorted(Vec<(Vec<String>, usize)>);

impl Clone for Sorted {
    fn clone(&self) -> Self {
        // Input restoration must preserve spare capacity for insertion samples.
        let mut entries = Vec::with_capacity(self.0.capacity());
        entries.extend(self.0.iter().cloned());
        Self(entries)
    }
}

impl Sorted {
    fn start(&self, keys: &[String]) -> usize {
        self.0
            .partition_point(|(candidate, _)| candidate.as_slice() < keys)
    }
}

impl Index for Sorted {
    fn bind(&mut self, keys: &[String], binding: usize) {
        self.unmap(keys);
        for length in 0..keys.len() {
            if let Ok(position) = self
                .0
                .binary_search_by(|(candidate, _)| candidate.as_slice().cmp(&keys[..length]))
            {
                self.0.remove(position);
            }
        }
        let position = self.start(keys);
        self.0.insert(position, (keys.to_vec(), binding));
    }

    fn unmap(&mut self, keys: &[String]) {
        let start = self.start(keys);
        let length = self.0[start..]
            .iter()
            .take_while(|(candidate, _)| candidate.starts_with(keys))
            .count();
        self.0.drain(start..start + length);
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        match self.0.get(self.start(keys)) {
            Some((candidate, binding)) if candidate == keys => Resolution::Command(*binding),
            Some((candidate, _)) if candidate.starts_with(keys) => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        for (_, binding) in self.0[self.start(prefix)..]
            .iter()
            .take_while(|(candidate, _)| candidate.starts_with(prefix))
        {
            visit(*binding);
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Radix {
    binding: Option<usize>,
    children: BTreeMap<String, Edge>,
}

#[derive(Clone)]
struct Edge {
    // The first token is the parent BTreeMap key. This is the remaining path.
    suffix: Vec<String>,
    node: Radix,
}

impl Radix {
    fn collect(&self, visit: &mut impl FnMut(usize)) {
        if let Some(binding) = self.binding {
            visit(binding);
        }
        for edge in self.children.values() {
            edge.node.collect(visit);
        }
    }

    // Queries may end inside an edge; every binding below it is a candidate.
    fn prefix_node(&self, keys: &[String]) -> Option<(&Self, bool)> {
        let Some((first, rest)) = keys.split_first() else {
            return Some((self, false));
        };
        let edge = self.children.get(first)?;
        if rest.starts_with(&edge.suffix) {
            edge.node.prefix_node(&rest[edge.suffix.len()..])
        } else if edge.suffix.starts_with(rest) {
            Some((&edge.node, true))
        } else {
            None
        }
    }
}

enum RadixPosition<'a> {
    Node(&'a Radix),
    Edge(&'a Edge, usize),
    Dead,
}
struct RadixCursor<'a> {
    root: &'a Radix,
    position: RadixPosition<'a>,
}
impl Cursor for RadixCursor<'_> {
    fn advance(&mut self, key: &String) -> Resolution {
        self.position = match self.position {
            RadixPosition::Node(node) => match node.children.get(key) {
                Some(edge) if edge.suffix.is_empty() => RadixPosition::Node(&edge.node),
                Some(edge) => RadixPosition::Edge(edge, 0),
                None => RadixPosition::Dead,
            },
            RadixPosition::Edge(edge, offset) if edge.suffix[offset] == *key => {
                if offset + 1 == edge.suffix.len() {
                    RadixPosition::Node(&edge.node)
                } else {
                    RadixPosition::Edge(edge, offset + 1)
                }
            }
            _ => RadixPosition::Dead,
        };
        match self.position {
            RadixPosition::Node(node) if node.binding.is_some() => {
                Resolution::Command(node.binding.unwrap())
            }
            RadixPosition::Node(node) if node.children.is_empty() => Resolution::Unbound,
            RadixPosition::Node(_) | RadixPosition::Edge(..) => Resolution::Prefix,
            RadixPosition::Dead => Resolution::Unbound,
        }
    }
    fn reset(&mut self) {
        self.position = RadixPosition::Node(self.root);
    }
}

impl Index for Radix {
    fn cursor(&self) -> impl Cursor {
        RadixCursor {
            root: self,
            position: RadixPosition::Node(self),
        }
    }
    fn bind(&mut self, keys: &[String], binding: usize) {
        let Some((first, rest)) = keys.split_first() else {
            self.binding = Some(binding);
            self.children.clear();
            return;
        };
        self.binding = None;
        match self.children.entry(first.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Edge {
                    suffix: rest.to_vec(),
                    node: Self {
                        binding: Some(binding),
                        children: BTreeMap::new(),
                    },
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let edge = entry.get_mut();
                let common = edge
                    .suffix
                    .iter()
                    .zip(rest)
                    .take_while(|(left, right)| left == right)
                    .count();
                if common < edge.suffix.len() {
                    let suffix = edge.suffix.split_off(common + 1);
                    let token = edge.suffix.pop().unwrap();
                    let node = std::mem::take(&mut edge.node);
                    edge.node.children.insert(token, Edge { suffix, node });
                }
                edge.node.bind(&rest[common..], binding);
            }
        }
    }

    fn unmap(&mut self, keys: &[String]) {
        let Some((first, rest)) = keys.split_first() else {
            self.binding = None;
            self.children.clear();
            return;
        };
        let Some(edge) = self.children.get_mut(first) else {
            return;
        };
        if rest.starts_with(&edge.suffix) {
            edge.node.unmap(&rest[edge.suffix.len()..]);
            if edge.node.binding.is_none() && edge.node.children.is_empty() {
                self.children.remove(first);
            } else if edge.node.binding.is_none() && edge.node.children.len() == 1 {
                let (token, child) = edge.node.children.pop_first().unwrap();
                edge.suffix.push(token);
                edge.suffix.extend(child.suffix);
                edge.node = child.node;
            }
        } else if edge.suffix.starts_with(rest) {
            self.children.remove(first);
        }
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        match self.prefix_node(keys) {
            Some((node, false)) if node.binding.is_some() => {
                Resolution::Command(node.binding.unwrap())
            }
            Some((node, inside)) if inside || !node.children.is_empty() => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        if let Some((node, _)) = self.prefix_node(prefix) {
            node.collect(visit);
        }
    }
}
