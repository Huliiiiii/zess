use super::{Cursor, Index, Resolution, Trie};
use std::collections::BTreeMap;

// These snapshots preserve every token node and BTree ordering. One is one
// inline edge, not radix/path compression. The editable Trie is consumed after
// construction, so retained memory includes only the flat snapshot.
pub(crate) trait StorageNode: Clone {
    fn empty() -> Self;
    fn leaf(binding: usize) -> Self;
    fn branch(children: impl ExactSizeIterator<Item = (String, usize)>) -> Self;
    fn child(&self, key: &String) -> Option<usize>;
    fn resolution(&self) -> Resolution;
    fn edges(&self) -> impl Iterator<Item = (&String, usize)>;
}

#[derive(Clone)]
pub(crate) struct BTreeNode {
    binding: Option<usize>,
    children: BTreeMap<String, usize>,
}
impl StorageNode for BTreeNode {
    fn empty() -> Self {
        Self {
            binding: None,
            children: BTreeMap::new(),
        }
    }
    fn leaf(binding: usize) -> Self {
        Self {
            binding: Some(binding),
            children: BTreeMap::new(),
        }
    }
    fn branch(children: impl ExactSizeIterator<Item = (String, usize)>) -> Self {
        Self {
            binding: None,
            children: children.collect(),
        }
    }
    fn child(&self, key: &String) -> Option<usize> {
        self.children.get(key).copied()
    }
    fn resolution(&self) -> Resolution {
        match self.binding {
            Some(binding) => Resolution::Command(binding),
            None if !self.children.is_empty() => Resolution::Prefix,
            None => Resolution::Unbound,
        }
    }
    fn edges(&self) -> impl Iterator<Item = (&String, usize)> {
        self.children.iter().map(|(key, child)| (key, *child))
    }
}

macro_rules! node {
    ($name:ident $(, $one:ident)?) => {
        #[derive(Clone)]
        pub(crate) enum $name {
            Empty,
            Leaf(usize),
            $($one(String, usize),)?
            Branch(BTreeMap<String, usize>),
        }
        impl StorageNode for $name {
            fn empty() -> Self { Self::Empty }
            fn leaf(binding: usize) -> Self { Self::Leaf(binding) }
            fn branch(mut children: impl ExactSizeIterator<Item = (String, usize)>) -> Self {
                $(if children.len() == 1 {
                    let (key, child) = children.next().unwrap();
                    return Self::$one(key, child);
                })?
                Self::Branch(children.by_ref().collect())
            }
            fn child(&self, key: &String) -> Option<usize> {
                match self {
                    $(Self::$one(token, child) if token == key => Some(*child),)?
                    Self::Branch(children) => children.get(key).copied(),
                    _ => None,
                }
            }
            fn resolution(&self) -> Resolution {
                match self {
                    Self::Empty => Resolution::Unbound,
                    Self::Leaf(binding) => Resolution::Command(*binding),
                    $(Self::$one(..) => Resolution::Prefix,)?
                    Self::Branch(_) => Resolution::Prefix,
                }
            }
            fn edges(&self) -> impl Iterator<Item = (&String, usize)> {
                let single = match self {
                    $(Self::$one(key, child) => Some((key, *child)),)?
                    _ => None,
                };
                let branches = match self {
                    Self::Branch(children) => Some(children),
                    _ => None,
                };
                single.into_iter().chain(branches.into_iter().flat_map(|children| children.iter().map(|(key, child)| (key, *child))))
            }
        }
    };
}
node!(LeafNode);
node!(InlineNode, One);

#[derive(Clone)]
pub(crate) struct NodeStorage<N: StorageNode> {
    editing: Option<Trie>,
    nodes: Vec<N>,
}

impl<N: StorageNode> Default for NodeStorage<N> {
    fn default() -> Self {
        Self {
            editing: None,
            nodes: Vec::new(),
        }
    }
}

impl<N: StorageNode> NodeStorage<N> {
    fn append(nodes: &mut Vec<N>, trie: Trie) -> usize {
        let position = nodes.len();
        nodes.push(N::empty());
        nodes[position] = if let Some(binding) = trie.binding {
            N::leaf(binding)
        } else if trie.children.is_empty() {
            N::empty()
        } else {
            N::branch(
                trie.children
                    .into_iter()
                    .map(|(key, child)| (key, Self::append(nodes, child))),
            )
        };
        position
    }
    fn restore(&self, position: usize) -> Trie {
        let mut trie = Trie::default();
        if let Resolution::Command(binding) = self.nodes[position].resolution() {
            trie.binding = Some(binding);
        }
        for (key, child) in self.nodes[position].edges() {
            trie.children.insert(key.clone(), self.restore(child));
        }
        trie
    }
    fn editing(&mut self) -> &mut Trie {
        if self.editing.is_none() {
            self.editing = Some(if self.nodes.is_empty() {
                Trie::default()
            } else {
                self.restore(0)
            });
            self.nodes.clear();
        }
        self.editing.as_mut().unwrap()
    }
    fn collect(&self, position: usize, visit: &mut impl FnMut(usize)) {
        if let Resolution::Command(binding) = self.nodes[position].resolution() {
            visit(binding);
        }
        for (_, child) in self.nodes[position].edges() {
            self.collect(child, visit);
        }
    }
    fn node(&self, keys: &[String]) -> Option<usize> {
        let mut position = (!self.nodes.is_empty()).then_some(0)?;
        for key in keys {
            position = self.nodes[position].child(key)?;
        }
        Some(position)
    }
}

struct StorageCursor<'a, N: StorageNode> {
    index: &'a NodeStorage<N>,
    node: Option<usize>,
    editable: super::TrieCursor<'a>,
}

impl<N: StorageNode> Cursor for StorageCursor<'_, N> {
    fn advance(&mut self, key: &String) -> Resolution {
        if self.index.editing.is_some() {
            return self.editable.advance(key);
        }
        self.node = self.node.and_then(|node| self.index.nodes[node].child(key));
        self.node
            .map(|node| self.index.nodes[node].resolution())
            .unwrap_or(Resolution::Unbound)
    }
    fn reset(&mut self) {
        self.node = (!self.index.nodes.is_empty()).then_some(0);
        self.editable.reset();
    }
}

impl<N: StorageNode> Index for NodeStorage<N> {
    fn bind(&mut self, keys: &[String], binding: usize) {
        self.editing().bind(keys, binding);
    }
    fn unmap(&mut self, keys: &[String]) {
        self.editing().unmap(keys);
    }
    fn finish(&mut self) {
        if let Some(trie) = self.editing.take() {
            self.nodes.clear();
            Self::append(&mut self.nodes, trie);
        }
    }
    fn resolve(&self, keys: &[String]) -> Resolution {
        if let Some(trie) = &self.editing {
            return trie.resolve(keys);
        }
        self.node(keys)
            .map(|node| self.nodes[node].resolution())
            .unwrap_or(Resolution::Unbound)
    }
    fn cursor(&self) -> impl Cursor {
        // The unused editable cursor borrows an empty static root.
        static EMPTY: std::sync::LazyLock<Trie> = std::sync::LazyLock::new(Trie::default);
        let root = self.editing.as_ref().unwrap_or(&EMPTY);
        StorageCursor {
            index: self,
            node: (!self.nodes.is_empty()).then_some(0),
            editable: super::TrieCursor {
                root,
                node: Some(root),
            },
        }
    }
    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        if let Some(trie) = &self.editing {
            trie.for_each_candidate(prefix, visit);
        } else if let Some(node) = self.node(prefix) {
            self.collect(node, visit);
        }
    }
}
