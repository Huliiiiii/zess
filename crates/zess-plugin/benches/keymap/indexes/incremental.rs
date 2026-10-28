use super::{Cursor, Index, Resolution, Sorted, Trie, TrieCursor};
use smallvec::SmallVec;
use std::{
    cell::{Ref, RefCell},
    collections::{BTreeMap, HashMap},
};

// Editable source and immutable CSR snapshot are both retained and measured.
// A declaration batch builds once; a subsequent edit invalidates the snapshot.
#[derive(Clone, Default)]
pub(crate) struct Packed {
    source: Trie,
    snapshot: RefCell<Option<PackedSnapshot>>,
}

#[derive(Clone, Default)]
struct PackedSnapshot {
    nodes: Vec<PackedNode>,
    edges: Vec<(String, usize)>,
}

#[derive(Clone)]
struct PackedNode {
    binding: Option<usize>,
    start: usize,
    length: usize,
}

impl PackedSnapshot {
    fn append(&mut self, trie: &Trie) -> usize {
        let position = self.nodes.len();
        let start = self.edges.len();
        self.nodes.push(PackedNode {
            binding: trie.binding,
            start,
            length: trie.children.len(),
        });
        self.edges
            .extend(trie.children.keys().map(|key| (key.clone(), 0)));
        for (offset, child) in trie.children.values().enumerate() {
            let child = self.append(child);
            self.edges[start + offset].1 = child;
        }
        position
    }

    fn child(&self, position: usize, key: &String) -> Option<usize> {
        let node = &self.nodes[position];
        let edges = &self.edges[node.start..node.start + node.length];
        edges
            .binary_search_by(|(token, _)| token.cmp(key))
            .ok()
            .map(|i| edges[i].1)
    }

    fn resolution(&self, position: Option<usize>) -> Resolution {
        match position.map(|position| &self.nodes[position]) {
            Some(node) if node.binding.is_some() => Resolution::Command(node.binding.unwrap()),
            Some(node) if node.length > 0 => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }
}

impl Packed {
    fn snapshot(&self) -> Ref<'_, PackedSnapshot> {
        if self.snapshot.borrow().is_none() {
            let mut snapshot = PackedSnapshot::default();
            snapshot.append(&self.source);
            *self.snapshot.borrow_mut() = Some(snapshot);
        }
        Ref::map(self.snapshot.borrow(), |snapshot| {
            snapshot.as_ref().unwrap()
        })
    }
}

struct PackedCursor<'a> {
    snapshot: Ref<'a, PackedSnapshot>,
    node: Option<usize>,
}

impl Cursor for PackedCursor<'_> {
    fn advance(&mut self, key: &String) -> Resolution {
        self.node = self.node.and_then(|node| self.snapshot.child(node, key));
        self.snapshot.resolution(self.node)
    }

    fn reset(&mut self) {
        self.node = Some(0);
    }
}

impl Index for Packed {
    fn bind(&mut self, keys: &[String], binding: usize) {
        self.source.bind(keys, binding);
        *self.snapshot.get_mut() = None;
    }

    fn unmap(&mut self, keys: &[String]) {
        self.source.unmap(keys);
        *self.snapshot.get_mut() = None;
    }

    fn finish(&mut self) {
        self.snapshot();
    }

    fn cursor(&self) -> impl Cursor {
        PackedCursor {
            snapshot: self.snapshot(),
            node: Some(0),
        }
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        let snapshot = self.snapshot();
        let mut node = Some(0);
        for key in keys {
            node = node.and_then(|node| snapshot.child(node, key));
        }
        snapshot.resolution(node)
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        // Enumeration traverses the same CSR snapshot, including its ordering.
        fn collect(snapshot: &PackedSnapshot, position: usize, visit: &mut impl FnMut(usize)) {
            let node = &snapshot.nodes[position];
            if let Some(binding) = node.binding {
                visit(binding);
            }
            for (_, child) in &snapshot.edges[node.start..node.start + node.length] {
                collect(snapshot, *child, visit);
            }
        }
        let snapshot = self.snapshot();
        let mut node = Some(0);
        for key in prefix {
            node = node.and_then(|node| snapshot.child(node, key));
        }
        if let Some(node) = node {
            collect(&snapshot, node, visit);
        }
    }
}

#[derive(Clone)]
enum Children {
    Small(SmallVec<[(String, Box<Adaptive>); 4]>),
    Large(BTreeMap<String, Box<Adaptive>>),
}

impl Default for Children {
    fn default() -> Self {
        Self::Small(SmallVec::new())
    }
}

impl Children {
    fn get(&self, key: &String) -> Option<&Adaptive> {
        match self {
            Self::Small(children) => children
                .binary_search_by(|(token, _)| token.cmp(key))
                .ok()
                .map(|position| children[position].1.as_ref()),
            Self::Large(children) => children.get(key).map(Box::as_ref),
        }
    }

    fn entry(&mut self, key: &String) -> &mut Adaptive {
        if matches!(self, Self::Small(children) if children.len() == 4 && children.binary_search_by(|(token, _)| token.cmp(key)).is_err())
        {
            let Self::Small(children) = std::mem::take(self) else {
                unreachable!()
            };
            *self = Self::Large(children.into_iter().collect());
        }
        match self {
            Self::Small(children) => {
                let position = match children.binary_search_by(|(token, _)| token.cmp(key)) {
                    Ok(position) => position,
                    Err(position) => {
                        children.insert(position, (key.clone(), Box::default()));
                        position
                    }
                };
                &mut children[position].1
            }
            Self::Large(children) => children.entry(key.clone()).or_default(),
        }
    }

    fn remove(&mut self, key: &String) {
        match self {
            Self::Small(children) => {
                if let Ok(position) = children.binary_search_by(|(token, _)| token.cmp(key)) {
                    children.remove(position);
                }
            }
            Self::Large(children) => {
                children.remove(key);
            }
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Small(children) => children.is_empty(),
            Self::Large(children) => children.is_empty(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Adaptive {
    binding: Option<usize>,
    children: Children,
}

impl Adaptive {
    fn collect(&self, visit: &mut impl FnMut(usize)) {
        if let Some(binding) = self.binding {
            visit(binding);
        }
        match &self.children {
            Children::Small(children) => {
                for (_, child) in children {
                    child.collect(visit);
                }
            }
            Children::Large(children) => {
                for child in children.values() {
                    child.collect(visit);
                }
            }
        }
    }
}

struct AdaptiveCursor<'a> {
    root: &'a Adaptive,
    node: Option<&'a Adaptive>,
}

impl Cursor for AdaptiveCursor<'_> {
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

impl Index for Adaptive {
    fn bind(&mut self, keys: &[String], binding: usize) {
        let mut node = self;
        for key in keys {
            node.binding = None;
            node = node.children.entry(key);
        }
        node.children = Children::default();
        node.binding = Some(binding);
    }

    fn unmap(&mut self, keys: &[String]) {
        let Some((key, rest)) = keys.split_first() else {
            *self = Self::default();
            return;
        };
        if self.children.get(key).is_some() {
            let child = self.children.entry(key);
            child.unmap(rest);
            if child.binding.is_none() && child.children.is_empty() {
                self.children.remove(key);
            }
        }
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        let mut cursor = self.cursor();
        let mut answer = match self.binding {
            Some(binding) => Resolution::Command(binding),
            None if !self.children.is_empty() => Resolution::Prefix,
            None => Resolution::Unbound,
        };
        for key in keys {
            answer = cursor.advance(key);
        }
        answer
    }

    fn cursor(&self) -> impl Cursor {
        AdaptiveCursor {
            root: self,
            node: Some(self),
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        let mut node = self;
        for key in prefix {
            let Some(child) = node.children.get(key) else {
                return;
            };
            node = child;
        }
        node.collect(visit);
    }
}

// Direct lookup is a Unicode-capable String hash table, not an ASCII array.
#[derive(Clone, Default)]
pub(crate) struct Hybrid {
    empty: Option<usize>,
    singles: HashMap<String, usize>,
    multi: Trie,
}

struct HybridCursor<'a> {
    root: &'a Hybrid,
    node: Option<&'a Trie>,
    started: bool,
}

impl Cursor for HybridCursor<'_> {
    fn advance(&mut self, key: &String) -> Resolution {
        if !self.started {
            self.started = true;
            if let Some(binding) = self.root.singles.get(key) {
                self.node = None;
                return Resolution::Command(*binding);
            }
        }
        self.node = self.node.and_then(|node| node.children.get(key));
        match self.node {
            Some(node) if node.binding.is_some() => Resolution::Command(node.binding.unwrap()),
            Some(node) if !node.children.is_empty() => Resolution::Prefix,
            _ => Resolution::Unbound,
        }
    }
    fn reset(&mut self) {
        self.started = false;
        self.node = Some(&self.root.multi);
    }
}

impl Index for Hybrid {
    fn bind(&mut self, keys: &[String], binding: usize) {
        match keys {
            [] => {
                *self = Self::default();
                self.empty = Some(binding);
            }
            [key] => {
                self.empty = None;
                self.multi.unmap(keys);
                self.singles.insert(key.clone(), binding);
            }
            [key, ..] => {
                self.empty = None;
                self.singles.remove(key);
                self.multi.bind(keys, binding);
            }
        }
    }
    fn unmap(&mut self, keys: &[String]) {
        match keys {
            [] => *self = Self::default(),
            [key] => {
                self.singles.remove(key);
                self.multi.unmap(keys);
            }
            _ => self.multi.unmap(keys),
        }
    }
    fn cursor(&self) -> impl Cursor {
        HybridCursor {
            root: self,
            node: Some(&self.multi),
            started: false,
        }
    }
    fn resolve(&self, keys: &[String]) -> Resolution {
        if keys.is_empty() {
            return self.empty.map(Resolution::Command).unwrap_or_else(|| {
                if self.singles.is_empty() {
                    self.multi.resolve(keys)
                } else {
                    Resolution::Prefix
                }
            });
        }
        let mut cursor = self.cursor();
        let mut answer = Resolution::Unbound;
        for key in keys {
            answer = cursor.advance(key);
        }
        answer
    }
    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        if !prefix.is_empty() {
            if prefix.len() == 1 {
                if let Some(binding) = self.singles.get(&prefix[0]) {
                    visit(*binding);
                    return;
                }
            }
            self.multi.for_each_candidate(prefix, visit);
            return;
        }
        if let Some(binding) = self.empty {
            visit(binding);
        }
        let mut singles = self.singles.iter().collect::<Vec<_>>();
        singles.sort_unstable_by_key(|(key, _)| *key);
        let mut singles = singles.into_iter().peekable();
        let mut multi = self.multi.children.iter().peekable();
        loop {
            match (singles.peek(), multi.peek()) {
                (Some((key, _)), Some((other, _))) if key > other => {
                    multi.next().unwrap().1.collect(visit);
                }
                (Some(_), _) => {
                    visit(*singles.next().unwrap().1);
                }
                (None, Some(_)) => {
                    multi.next().unwrap().1.collect(visit);
                }
                (None, None) => break,
            }
        }
    }
}

// This is a LOUDS base plus mutable delta and tombstones, not a dynamic LOUDS
// bitvector. At 64 post-build edits it merges the normalized source into a new
// base. Source, base metadata, delta, live subtree counts and merge all count.
#[derive(Clone, Default)]
pub(crate) struct LoudsDelta {
    entries: Sorted,
    base: Option<LoudsBase>,
    delta: Trie,
    edits: usize,
}

#[derive(Clone)]
struct LoudsRecord {
    keys: Vec<String>,
    path: Vec<usize>,
    active: bool,
}

#[derive(Clone)]
struct LoudsBase {
    trie: trie_rs::map::Trie<String, usize>,
    records: Vec<LoudsRecord>,
    live: Vec<usize>,
}

impl LoudsDelta {
    fn merge(&mut self) {
        let mut builder = trie_rs::map::TrieBuilder::new();
        for (keys, binding) in &self.entries.0 {
            builder.insert(
                std::iter::once(String::new()).chain(keys.iter().cloned()),
                *binding,
            );
        }
        let trie = builder.build();
        let mut live = Vec::new();
        let mut records = Vec::new();
        for (keys, _) in &self.entries.0 {
            let mut search = trie.inc_search();
            let mut path = Vec::new();
            for key in std::iter::once(&String::new()).chain(keys) {
                search.query(key).unwrap();
                let position = trie_rs::inc_search::Position::from(search.clone()).0 as usize;
                if live.len() <= position {
                    live.resize(position + 1, 0);
                }
                live[position] += 1;
                path.push(position);
            }
            records.push(LoudsRecord {
                keys: keys.clone(),
                path,
                active: true,
            });
        }
        self.base = Some(LoudsBase {
            trie,
            records,
            live,
        });
        self.delta = Trie::default();
        self.edits = 0;
    }

    fn tombstone(&mut self, keys: &[String], ancestors: bool) {
        let Some(base) = &mut self.base else {
            return;
        };
        for record in &mut base.records {
            if record.active
                && (record.keys.starts_with(keys) || (ancestors && keys.starts_with(&record.keys)))
            {
                record.active = false;
                for position in &record.path {
                    base.live[*position] -= 1;
                }
            }
        }
    }

    fn edited(&mut self) {
        if self.base.is_some() {
            self.edits += 1;
            if self.edits >= 64 {
                self.merge();
            }
        }
    }
}

enum LoudsCursor<'a> {
    Base {
        base: &'a LoudsBase,
        search: trie_rs::inc_search::IncSearch<'a, String, usize>,
        base_live: bool,
        delta: TrieCursor<'a>,
    },
    Unbuilt(super::Requery<'a, Sorted>),
}

impl Cursor for LoudsCursor<'_> {
    fn advance(&mut self, key: &String) -> Resolution {
        match self {
            Self::Unbuilt(cursor) => cursor.advance(key),
            Self::Base {
                base,
                search,
                base_live,
                delta,
            } => {
                let delta = delta.advance(key);
                if *base_live {
                    *base_live = search.query(key).is_some() && {
                        let position =
                            trie_rs::inc_search::Position::from(search.clone()).0 as usize;
                        base.live.get(position).copied().unwrap_or(0) > 0
                    };
                }
                if let Resolution::Command(_) = delta {
                    return delta;
                }
                if *base_live {
                    if let Some(binding) = search.value() {
                        return Resolution::Command(*binding);
                    }
                    return Resolution::Prefix;
                }
                delta
            }
        }
    }
    fn reset(&mut self) {
        match self {
            Self::Unbuilt(cursor) => cursor.reset(),
            Self::Base {
                search,
                base_live,
                delta,
                ..
            } => {
                search.reset();
                *base_live = search.query(&String::new()).is_some();
                delta.reset();
            }
        }
    }
}

impl Index for LoudsDelta {
    fn bind(&mut self, keys: &[String], binding: usize) {
        self.entries.bind(keys, binding);
        self.tombstone(keys, true);
        if self.base.is_some() {
            self.delta.bind(keys, binding);
        }
        self.edited();
    }
    fn unmap(&mut self, keys: &[String]) {
        self.entries.unmap(keys);
        self.tombstone(keys, false);
        self.delta.unmap(keys);
        self.edited();
    }
    fn finish(&mut self) {
        if self.base.is_none() {
            self.merge();
        }
    }
    fn cursor(&self) -> impl Cursor {
        match &self.base {
            None => LoudsCursor::Unbuilt(super::Requery {
                index: &self.entries,
                keys: Vec::new(),
            }),
            Some(base) => {
                let mut search = base.trie.inc_search();
                let base_live = search.query(&String::new()).is_some();
                LoudsCursor::Base {
                    base,
                    search,
                    base_live,
                    delta: TrieCursor {
                        root: &self.delta,
                        node: Some(&self.delta),
                    },
                }
            }
        }
    }
    fn resolve(&self, keys: &[String]) -> Resolution {
        if keys.is_empty() {
            return self.entries.resolve(keys);
        }
        let mut cursor = self.cursor();
        let mut answer = Resolution::Unbound;
        for key in keys {
            answer = cursor.advance(key);
        }
        answer
    }
    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        let Some(base) = &self.base else {
            self.entries.for_each_candidate(prefix, visit);
            return;
        };
        // Base traversal and delta ordering are included; normalized source is
        // only for updates/merge, not used to answer enumeration or lookups.
        let mut candidates = BTreeMap::new();
        for (keys, binding) in base.trie.predictive_search::<Vec<String>, _>(
            std::iter::once(String::new())
                .chain(prefix.iter().cloned())
                .collect::<Vec<_>>(),
        ) {
            let keys = &keys[1..];
            if base
                .records
                .binary_search_by(|record| record.keys.as_slice().cmp(keys))
                .is_ok_and(|position| base.records[position].active)
            {
                candidates.insert(keys.to_vec(), *binding);
            }
        }
        fn collect(node: &Trie, keys: &mut Vec<String>, out: &mut BTreeMap<Vec<String>, usize>) {
            if let Some(binding) = node.binding {
                out.insert(keys.clone(), binding);
            }
            for (key, child) in &node.children {
                keys.push(key.clone());
                collect(child, keys, out);
                keys.pop();
            }
        }
        if let Some(node) = self.delta.node(prefix) {
            collect(node, &mut prefix.to_vec(), &mut candidates);
        }
        for binding in candidates.values() {
            visit(*binding);
        }
    }
}
