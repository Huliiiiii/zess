use super::{Index, Resolution, Sorted};
use fst::{IntoStreamer, Streamer};
use std::cell::{Ref, RefCell};

// Byte indexes must preserve whole-token prefixes and Vec<String> ordering.
// 0 and 1 are escaped; [1, 1] terminates each token. The initial 1 represents
// the root, including an empty sequence. No output byte is 0 (yada requires it).
// All byte-index adapters pay for this encoding on every lookup and mutation.
fn encode(keys: &[String]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1 + keys.iter().map(|key| key.len() + 2).sum::<usize>());
    bytes.push(1);
    for key in keys {
        for byte in key.bytes() {
            if byte <= 1 {
                bytes.extend([1, byte + 2]);
            } else {
                bytes.push(byte);
            }
        }
        bytes.extend([1, 1]);
    }
    bytes
}

#[derive(Clone, Default)]
pub(crate) struct Art(blart::TreeMap<Box<[u8]>, usize>);

impl Index for Art {
    fn bind(&mut self, keys: &[String], binding: usize) {
        // ART natively replaces conflicting ancestors and descendants.
        self.0
            .prefix_insert(encode(keys).into_boxed_slice(), binding);
    }

    fn unmap(&mut self, keys: &[String]) {
        let prefix = encode(keys).into_boxed_slice();
        // Only replace an existing subtree. Without the prefix check,
        // prefix_entry could also erase a command on a shorter ancestor.
        if self.0.prefix(&prefix).next().is_some() {
            self.0.prefix_entry(prefix).insert_entry(0).remove();
        }
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        let keys = encode(keys);
        if let Some(binding) = self.0.get(keys.as_slice()) {
            Resolution::Command(*binding)
        } else if self.0.prefix(&keys).next().is_some() {
            Resolution::Prefix
        } else {
            Resolution::Unbound
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        for (_, binding) in self.0.prefix(&encode(prefix)) {
            visit(*binding);
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct QpTrie(qp_trie::Trie<Vec<u8>, usize>);

impl Index for QpTrie {
    fn bind(&mut self, keys: &[String], binding: usize) {
        let encoded = encode(keys);
        self.0.remove_prefix(encoded.as_slice());
        for length in 0..keys.len() {
            self.0.remove(encode(&keys[..length]).as_slice());
        }
        self.0.insert(encoded, binding);
    }

    fn unmap(&mut self, keys: &[String]) {
        self.0.remove_prefix(encode(keys).as_slice());
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        let keys = encode(keys);
        if let Some(binding) = self.0.get(keys.as_slice()) {
            Resolution::Command(*binding)
        } else if self.0.iter_prefix(keys.as_slice()).next().is_some() {
            Resolution::Prefix
        } else {
            Resolution::Unbound
        }
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        // qp-trie visits low nibbles before high nibbles, which is not byte
        // lexicographic order. Sorting and its temporary allocation are timed.
        let mut candidates = self
            .0
            .iter_prefix(encode(prefix).as_slice())
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|(keys, _)| keys.as_slice());
        for (_, binding) in candidates {
            visit(*binding);
        }
    }
}

pub(crate) trait Snapshot: Clone {
    fn build(entries: &Sorted) -> Self;
    fn resolve(&self, entries: &Sorted, keys: &[String]) -> Resolution;
    fn for_each_candidate(
        &self,
        entries: &Sorted,
        prefix: &[String],
        visit: &mut impl FnMut(usize),
    );
}

// Immutable indexes retain their editable, normalized entries as well as their
// built snapshot. Both allocations count as retained memory. A declaration
// batch builds once; each independently measured mutation rebuilds once.
#[derive(Clone)]
pub(crate) struct Static<S> {
    entries: Sorted,
    snapshot: RefCell<Option<S>>,
}

impl<S: Snapshot> Default for Static<S> {
    fn default() -> Self {
        Self {
            entries: Sorted::default(),
            snapshot: RefCell::new(None),
        }
    }
}

impl<S: Snapshot> Static<S> {
    fn snapshot(&self) -> Ref<'_, S> {
        if self.snapshot.borrow().is_none() {
            *self.snapshot.borrow_mut() = Some(S::build(&self.entries));
        }
        Ref::map(self.snapshot.borrow(), |snapshot| {
            snapshot.as_ref().unwrap()
        })
    }
}

impl<S: Snapshot> Index for Static<S> {
    fn bind(&mut self, keys: &[String], binding: usize) {
        self.entries.bind(keys, binding);
        *self.snapshot.get_mut() = None;
    }

    fn unmap(&mut self, keys: &[String]) {
        self.entries.unmap(keys);
        *self.snapshot.get_mut() = None;
    }

    fn resolve(&self, keys: &[String]) -> Resolution {
        self.snapshot().resolve(&self.entries, keys)
    }

    fn for_each_candidate(&self, prefix: &[String], visit: &mut impl FnMut(usize)) {
        self.snapshot()
            .for_each_candidate(&self.entries, prefix, visit);
    }

    fn finish(&mut self) {
        self.snapshot();
    }
}

#[derive(Clone)]
pub(crate) struct FstSnapshot(fst::Map<Vec<u8>>);

pub(crate) type FstIndex = Static<FstSnapshot>;

impl Snapshot for FstSnapshot {
    fn build(entries: &Sorted) -> Self {
        Self(
            fst::Map::from_iter(
                entries
                    .0
                    .iter()
                    .map(|(keys, binding)| (encode(keys), *binding as u64)),
            )
            .unwrap(),
        )
    }

    fn resolve(&self, _: &Sorted, keys: &[String]) -> Resolution {
        let keys = encode(keys);
        if let Some(binding) = self.0.get(&keys) {
            return Resolution::Command(binding as usize);
        }
        let mut stream = self.0.range().ge(&keys).into_stream();
        if stream.next().is_some_and(|(key, _)| key.starts_with(&keys)) {
            Resolution::Prefix
        } else {
            Resolution::Unbound
        }
    }

    fn for_each_candidate(&self, _: &Sorted, prefix: &[String], visit: &mut impl FnMut(usize)) {
        let prefix = encode(prefix);
        let mut stream = self.0.range().ge(&prefix).into_stream();
        while let Some((key, binding)) = stream.next() {
            if !key.starts_with(&prefix) {
                break;
            }
            visit(binding as usize);
        }
    }
}

#[derive(Clone)]
pub(crate) struct LoudsSnapshot(trie_rs::map::Trie<String, usize>);

pub(crate) type LoudsIndex = Static<LoudsSnapshot>;

struct NoReconstructedKeys;

impl FromIterator<String> for NoReconstructedKeys {
    fn from_iter<T: IntoIterator<Item = String>>(_: T) -> Self {
        // Candidates borrow their original keys using the returned binding ID.
        // The library's lazy key iterator need not clone those strings again.
        Self
    }
}

impl Snapshot for LoudsSnapshot {
    fn build(entries: &Sorted) -> Self {
        let mut builder = trie_rs::map::TrieBuilder::new();
        for (keys, binding) in &entries.0 {
            // A leading root label lets the library represent an empty sequence.
            builder.insert(
                std::iter::once(String::new()).chain(keys.iter().cloned()),
                *binding,
            );
        }
        Self(builder.build())
    }

    fn resolve(&self, _: &Sorted, keys: &[String]) -> Resolution {
        let mut search = self.0.inc_search();
        let Some(mut answer) = search.query(&String::new()) else {
            return Resolution::Unbound;
        };
        for key in keys {
            let Some(next) = search.query(key) else {
                return Resolution::Unbound;
            };
            answer = next;
        }
        if let Some(binding) = search.value() {
            Resolution::Command(*binding)
        } else if answer.is_prefix() {
            Resolution::Prefix
        } else {
            Resolution::Unbound
        }
    }

    fn for_each_candidate(&self, _: &Sorted, prefix: &[String], visit: &mut impl FnMut(usize)) {
        let prefix = std::iter::once(String::new())
            .chain(prefix.iter().cloned())
            .collect::<Vec<_>>();
        if let Some(binding) = self.0.exact_match(&prefix) {
            // Keymap commands cannot coexist with commands below them.
            visit(*binding);
            return;
        }
        for (_, binding) in self.0.postfix_search::<NoReconstructedKeys, _>(&prefix) {
            visit(*binding);
        }
    }
}

#[derive(Clone)]
pub(crate) struct DoubleArraySnapshot(Option<yada::DoubleArray<Vec<u8>>>);

pub(crate) type DoubleArrayIndex = Static<DoubleArraySnapshot>;

impl Snapshot for DoubleArraySnapshot {
    fn build(entries: &Sorted) -> Self {
        if entries.0.is_empty() {
            return Self(None);
        }
        let entries = entries
            .0
            .iter()
            .map(|(keys, binding)| (encode(keys), u32::try_from(*binding).unwrap()))
            .collect::<Vec<_>>();
        Self(Some(
            yada::DoubleArray::new(yada::builder::DoubleArrayBuilder::build(&entries).unwrap())
                .unwrap(),
        ))
    }

    fn resolve(&self, entries: &Sorted, keys: &[String]) -> Resolution {
        let Some(array) = &self.0 else {
            return Resolution::Unbound;
        };
        if let Some(binding) = array.exact_match_search(encode(keys)) {
            Resolution::Command(binding as usize)
        } else {
            // yada has common-prefix lookup but no predictive traversal.
            // Its retained, ordered mutation source supplies this capability.
            entries.resolve(keys)
        }
    }

    fn for_each_candidate(
        &self,
        entries: &Sorted,
        prefix: &[String],
        visit: &mut impl FnMut(usize),
    ) {
        entries.for_each_candidate(prefix, visit);
    }
}
