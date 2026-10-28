#[path = "keymap/allocations.rs"]
mod allocations;
#[path = "keymap/batch.rs"]
mod batch;
#[path = "keymap/indexes.rs"]
mod indexes;

use indexes::{
    Adaptive, Art, Cursor, DoubleArrayIndex, FstIndex, HashIndex, HashTrie, Hybrid, Index,
    LoudsDelta, LoudsIndex, Packed, QpTrie, Radix, Resolution, Sorted, Trie,
};
use std::{
    cell::{Ref, RefCell},
    collections::BTreeMap,
    hint::black_box,
    time::{Duration, Instant},
};
use zess_plugin::{KeyResolution, KeyScopeId, KeyState, Keymap, ResolvedBinding};

#[derive(Clone)]
struct Mapping {
    scope: String,
    mode: String,
    keys: Vec<String>,
    command: Option<String>,
}

#[derive(Clone, Copy)]
struct Context {
    scopes: &'static [&'static str],
    mode: &'static str,
}

const SCOPES: &[&str] = &["global", "browser", "user"];
const SYNTHETIC_CONTEXTS: [Context; 3] = [
    Context {
        scopes: SCOPES,
        mode: "normal",
    },
    Context {
        scopes: SCOPES,
        mode: "select",
    },
    Context {
        scopes: &["global"],
        mode: "normal",
    },
];

struct Fixture {
    name: &'static str,
    size: usize,
    mappings: Vec<Mapping>,
    contexts: [Context; 3],
}

fn mapping(scope: &str, mode: &str, keys: &[&str], command: Option<&str>) -> Mapping {
    Mapping {
        scope: scope.into(),
        mode: mode.into(),
        keys: keys.iter().map(|s| (*s).into()).collect(),
        command: command.map(str::to_owned),
    }
}

fn fixture(size: usize, deep: bool) -> Fixture {
    let mut mappings = Vec::new();
    for position in 0..size {
        // A fixed permutation avoids giving ordered insertion to sorted Vec.
        let i = position * 37 % size;
        // Each String is one whole key event, not a character prefix.
        let tokens = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let token =
            |digit: usize| tokens[digit % tokens.len()..digit % tokens.len() + 1].to_owned();
        let mut keys = vec![token(i)];
        if deep {
            keys.extend(["C-w".into(), "space".into(), "Enter".into()]);
        }
        keys.extend([token(i / 62), token(i / (62 * 62))]);
        mappings.push(Mapping {
            scope: "global".into(),
            mode: "normal".into(),
            keys: keys.clone(),
            command: Some(format!("global-{i}")),
        });
        if i % 10 == 0 {
            mappings.push(Mapping {
                scope: "browser".into(),
                mode: "normal".into(),
                keys: keys.clone(),
                command: Some(format!("browser-{i}")),
            });
            mappings.push(Mapping {
                scope: "global".into(),
                mode: "select".into(),
                keys: keys.clone(),
                command: Some(format!("select-{i}")),
            });
        }
        if i % 20 == 1 {
            mappings.push(Mapping {
                scope: "browser".into(),
                mode: "normal".into(),
                keys: keys.clone(),
                command: None,
            });
        }
        if i % 50 == 2 {
            let mut child = keys.clone();
            child.push("Tab".into());
            mappings.push(Mapping {
                scope: "browser".into(),
                mode: "normal".into(),
                keys: child,
                command: Some(format!("child-{i}")),
            });
        }
        if i % 100 == 2 {
            mappings.push(Mapping {
                scope: "user".into(),
                mode: "normal".into(),
                keys,
                command: Some(format!("user-{i}")),
            });
        }
    }
    Fixture {
        name: if deep {
            "synthetic_deep"
        } else {
            "synthetic_shallow"
        },
        size,
        mappings,
        contexts: SYNTHETIC_CONTEXTS,
    }
}

// Parse only the literal map-modal/map-key declarations currently in defaults.w.
// This is the actual script distribution; NativePlugin registrations are excluded.
fn defaults() -> Vec<Mapping> {
    let mut mappings = Vec::new();
    for line in include_str!("../../../plugins/defaults.w").lines() {
        let modal = line.starts_with("map-modal ");
        if !modal && !line.starts_with("map-key ") {
            continue;
        }
        let mut strings = Vec::new();
        let mut rest = line;
        while let Some(start) = rest.find('"') {
            rest = &rest[start..];
            let mut escaped = false;
            let end = rest
                .char_indices()
                .skip(1)
                .find_map(|(index, c)| {
                    if escaped {
                        escaped = false;
                        None
                    } else if c == '\\' {
                        escaped = true;
                        None
                    } else if c == '"' {
                        Some(index)
                    } else {
                        None
                    }
                })
                .unwrap();
            strings.push(serde_json::from_str::<String>(&rest[..=end]).unwrap());
            rest = &rest[end + 1..];
        }
        let offset = if modal { 1 } else { 2 };
        assert!(
            strings.len() > offset + 1,
            "unsupported defaults declaration: {line}"
        );
        let command = strings.last().unwrap();
        let explicit_mode = [strings[1].as_str()];
        for mode in if modal {
            &["normal", "select"][..]
        } else {
            &explicit_mode
        } {
            mappings.push(Mapping {
                scope: strings[0].clone(),
                mode: (*mode).into(),
                keys: strings[offset..strings.len() - 1].to_vec(),
                command: Some(command.clone()),
            });
        }
    }
    mappings
}

fn fixtures() -> Vec<Fixture> {
    let mut fixtures = Vec::new();
    for (name, scopes) in [
        ("defaults_text", &["global", "text"][..]),
        ("defaults_data", &["global", "data"][..]),
        ("defaults_diff", &["global", "diff"][..]),
    ] {
        let mappings = defaults();
        let size = mappings.len();
        fixtures.push(Fixture {
            name,
            size,
            mappings,
            contexts: [
                Context {
                    scopes,
                    mode: "normal",
                },
                Context {
                    scopes,
                    mode: "select",
                },
                Context {
                    scopes: &["global", "text"],
                    mode: "insert",
                },
            ],
        });
    }
    let mut physical = Vec::new();
    for (position, key) in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .enumerate()
    {
        physical.push(mapping(
            "global",
            "normal",
            &[&key.to_string()],
            Some(&format!("single-{position}")),
        ));
    }
    for (prefix, tokens) in [
        ("space", "abcdefghijklmnopqrstuvwxyz"),
        ("C-w", "hjklwsq"),
        ("g", "gedhflrt"),
    ] {
        for (position, key) in tokens.chars().enumerate() {
            physical.push(mapping(
                "browser",
                "normal",
                &[prefix, &key.to_string()],
                Some(&format!("{prefix}-{position}")),
            ));
        }
    }
    for key in ["Enter", "Esc", "C-x", "S-Tab", "M-Enter", "é", "鍵"] {
        physical.push(mapping(
            "user",
            "normal",
            &[key],
            Some(&format!("special-{key}")),
        ));
    }
    for (scope, mode) in [
        ("global", "select"),
        ("browser", "select"),
        ("global", "insert"),
    ] {
        physical.push(mapping(scope, mode, &["Esc"], Some("leave-mode")));
    }
    fixtures.push(Fixture {
        name: "physical_mixed",
        size: physical.len(),
        mappings: physical,
        contexts: SYNTHETIC_CONTEXTS,
    });
    for size in [100, 1_000, 10_000] {
        fixtures.push(fixture(size, false));
        fixtures.push(fixture(size, true));
    }
    // Every table in this fixture is nonempty. Scope priority replaces shared
    // prefixes while keeping scope-local F-key sequences, so larger contexts
    // change both the declaration scan and the effective tree that is built.
    let scopes = context_shapes()[8].scopes;
    let mut layered = Vec::new();
    let tokens = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for mode in ["normal", "select", "insert", "prompt"] {
        for (layer, scope) in scopes.iter().enumerate() {
            for (position, token) in tokens.chars().enumerate() {
                let prefix = if position % 7 == 0 {
                    "space".to_owned()
                } else {
                    format!("F{}", layer + 1)
                };
                layered.push(mapping(
                    scope,
                    mode,
                    &[&prefix, &token.to_string()],
                    Some(&format!("{scope}-{mode}-{position}")),
                ));
            }
        }
    }
    fixtures.push(Fixture {
        name: "context_layered",
        size: layered.len(),
        mappings: layered,
        contexts: [
            Context {
                scopes,
                mode: "normal",
            },
            Context {
                scopes: SCOPES,
                mode: "select",
            },
            Context {
                scopes: &["global"],
                mode: "insert",
            },
        ],
    });
    fixtures
}

fn compile<I: Index>(mappings: &[Mapping], context: Context) -> I {
    let mut index = I::default();
    for scope in context.scopes {
        for (binding, mapping) in mappings.iter().enumerate() {
            if mapping.scope != *scope || mapping.mode != context.mode {
                continue;
            }
            if mapping.command.is_some() {
                index.bind(&mapping.keys, binding);
            } else {
                index.unmap(&mapping.keys);
            }
        }
    }
    index.finish();
    index
}

struct Compiled<I> {
    scopes: Vec<String>,
    mode: String,
    index: I,
}

struct Cached<I> {
    mappings: Vec<Mapping>,
    contexts: RefCell<Vec<Compiled<I>>>,
}

impl<I: Index> Cached<I> {
    fn new(mappings: &[Mapping]) -> Self {
        let mut declarations = Vec::new();
        for mapping in mappings {
            declarations.push(mapping.clone());
        }
        Self {
            mappings: declarations,
            contexts: RefCell::new(Vec::new()),
        }
    }

    fn map(&mut self, mapping: Mapping) {
        self.contexts.get_mut().clear();
        self.mappings.push(mapping);
    }

    fn context(&self, context: Context) -> Ref<'_, I> {
        let contexts = self.contexts.borrow();
        if let Some(position) = contexts.iter().position(|candidate| {
            candidate.scopes == context.scopes && candidate.mode == context.mode
        }) {
            return Ref::map(contexts, |contexts| &contexts[position].index);
        }
        drop(contexts);
        let index = compile(&self.mappings, context);
        let position = {
            let mut contexts = self.contexts.borrow_mut();
            let position = contexts.len();
            contexts.push(Compiled {
                scopes: context.scopes.iter().map(|scope| (*scope).into()).collect(),
                mode: context.mode.into(),
                index,
            });
            position
        };
        Ref::map(self.contexts.borrow(), |contexts| &contexts[position].index)
    }
}

fn production(mappings: &[Mapping]) -> Keymap {
    let mut keymap = Keymap::default();
    for mapping in mappings {
        keymap.map(
            &mapping.scope,
            &mapping.mode,
            mapping.keys.clone(),
            mapping.command.clone(),
        );
    }
    for context in context_shapes() {
        keymap.identities().intern_mode(context.mode);
        for scope in context.scopes {
            keymap.identities().intern_scope(scope);
        }
    }
    keymap
}

fn production_resolution(
    keymap: &Keymap,
    context: Context,
    query: &[String],
    mappings: &[Mapping],
) -> Resolution {
    match keymap.resolve(context.scopes, context.mode, query) {
        KeyResolution::Command(command) => Resolution::Command(
            mappings
                .iter()
                .rposition(|mapping| mapping.command.as_deref() == Some(command))
                .unwrap(),
        ),
        KeyResolution::Prefix => Resolution::Prefix,
        KeyResolution::Unbound => Resolution::Unbound,
    }
}

fn resolved_command(resolution: Resolution, mappings: &[Mapping]) -> KeyResolution<&str> {
    match resolution {
        Resolution::Command(binding) => {
            KeyResolution::Command(mappings[binding].command.as_deref().unwrap())
        }
        Resolution::Prefix => KeyResolution::Prefix,
        Resolution::Unbound => KeyResolution::Unbound,
    }
}

fn dispatch(cursor: &mut impl Cursor, key: &String) -> Resolution {
    let answer = cursor.advance(key);
    if !matches!(answer, Resolution::Prefix) {
        cursor.reset();
    }
    answer
}

fn context_shapes() -> [Context; 12] {
    let scopes: [&'static [&'static str]; 3] = [
        &["global"],
        &["global", "browser", "user"],
        &[
            "global", "browser", "user", "text", "data", "diff", "files", "git",
        ],
    ];
    std::array::from_fn(|position| Context {
        scopes: scopes[position / 4],
        mode: ["normal", "select", "insert", "prompt"][position % 4],
    })
}

fn candidate_bindings<'a, I: Index>(
    index: &I,
    mappings: &'a [Mapping],
    scopes: &[KeyScopeId],
    prefix: &[String],
) -> Vec<ResolvedBinding<'a>> {
    let mut bindings = Vec::new();
    index.for_each_candidate(prefix, &mut |binding| {
        let mapping = &mappings[binding];
        bindings.push(ResolvedBinding {
            keys: &mapping.keys,
            command: mapping.command.as_deref().unwrap(),
            scope: scopes[binding],
        });
    });
    bindings
}

fn conformance<I: Index>() {
    let context = SYNTHETIC_CONTEXTS[0];
    let mut maps = vec![
        mapping("global", "normal", &["g"], Some("old")),
        mapping("global", "normal", &["z", "a"], Some("removed-descendant")),
        mapping("global", "normal", &["g0"], Some("token-not-character")),
        mapping(
            "global",
            "normal",
            &["C-w", "space", "Enter", "j"],
            Some("deep-j"),
        ),
        mapping(
            "global",
            "normal",
            &["C-w", "space", "Enter", "k"],
            Some("deep-k"),
        ),
        mapping("browser", "normal", &["g", "g"], Some("new-descendant")),
        mapping("browser", "normal", &["z"], Some("new-ancestor")),
        mapping("browser", "normal", &["g0", "x"], None),
        mapping("browser", "select", &["g"], Some("other-mode")),
    ];
    let mut cached = Cached::<I>::new(&maps);
    let expected = vec![3, 4, 5, 2, 6];
    assert_eq!(
        cached.context(context).candidates(&[]),
        expected,
        "{}",
        std::any::type_name::<I>(),
    );
    let mut index = compile::<I>(&maps, context);
    for (keys, expected) in [
        (vec!["g"], Resolution::Prefix),
        (vec!["g", "g"], Resolution::Command(5)),
        (vec!["g0"], Resolution::Command(2)),
        (vec!["g0", "x"], Resolution::Unbound),
        (vec!["g00"], Resolution::Unbound),
        (vec!["z"], Resolution::Command(6)),
        (vec!["z", "a"], Resolution::Unbound),
        (vec!["C-w", "space"], Resolution::Prefix),
    ] {
        let query = keys.iter().map(|key| (*key).into()).collect::<Vec<_>>();
        let mut cursor = index.cursor();
        for length in 1..=query.len() {
            assert_eq!(
                cursor.advance(&query[length - 1]),
                index.resolve(&query[..length])
            );
        }
        cursor.reset();
        if let Some(first) = query.first() {
            assert_eq!(cursor.advance(first), index.resolve(&query[..1]));
            cursor.reset();
            assert_eq!(cursor.advance(first), index.resolve(&query[..1]));
        }
        drop(cursor);
        assert_eq!(index.resolve(&query), expected);
        assert_eq!(
            production_resolution(&production(&maps), context, &query, &maps),
            expected
        );
    }
    assert_eq!(
        index.candidates(&["C-w".into(), "space".into()]),
        vec![3, 4]
    );
    index.unmap(&["C-w".into(), "space".into()]);
    assert_eq!(index.candidates(&[]), vec![5, 2, 6]);
    index.bind(&["g".into()], 9);
    assert_eq!(index.candidates(&[]), vec![9, 2, 6]);
    index.bind(&["g".into(), "x".into()], 10);
    index.unmap(&["g".into(), "x".into()]);
    assert_eq!(index.resolve(&["g".into()]), Resolution::Unbound);
    index.bind(&[], 11);
    assert_eq!(index.candidates(&[]), vec![11]);
    index.unmap(&[]);
    assert_eq!(index.resolve(&[]), Resolution::Unbound);
    let long = mapping("global", "normal", &["a", "b", "c", "d"], Some("long"));
    let sibling = mapping("global", "normal", &["a", "b", "x", "y"], Some("sibling"));
    index.bind(&long.keys, 0);
    index.bind(&sibling.keys, 1);
    index.unmap(&["a".into(), "b".into(), "c".into()]);
    assert_eq!(index.candidates(&["a".into(), "b".into()]), vec![1]);
    assert_eq!(
        index.resolve(&["a".into(), "b".into(), "x".into()]),
        Resolution::Prefix
    );
    index.bind(&["a".into(), "b".into(), "z".into()], 2);
    assert_eq!(index.candidates(&[]), vec![1, 2]);
    index.unmap(&["a".into()]);
    assert_eq!(index.candidates(&[]), Vec::<usize>::new());
    // Native byte tries must keep token boundaries, empty tokens, byte escapes,
    // and Unicode in the same order as the production token trie.
    let boundary_keys = [
        vec![""],
        vec!["\0"],
        vec!["\u{1}"],
        vec!["a"],
        vec!["a\0"],
        vec!["a", "b"],
        vec!["ab"],
        vec!["é"],
        vec!["鍵"],
    ];
    // Binding the longer token path must remove the "a" command,
    // without removing the distinct single token "ab" or "a\\0".
    for (binding, keys) in boundary_keys.iter().enumerate() {
        index.bind(
            &keys.iter().map(|key| (*key).into()).collect::<Vec<_>>(),
            binding,
        );
    }
    assert_eq!(index.candidates(&[]), vec![0, 1, 2, 5, 4, 6, 7, 8]);
    assert_eq!(index.resolve(&["a".into()]), Resolution::Prefix);
    assert_eq!(
        index.resolve(&["a", "b"].map(String::from)),
        Resolution::Command(5)
    );
    assert_eq!(index.resolve(&["a\0".into()]), Resolution::Command(4));
    index.unmap(&["a".into()]);
    assert_eq!(index.candidates(&[]), vec![0, 1, 2, 4, 6, 7, 8]);
    // Cache every context before a mutation; every context must be rebuilt.
    for context in SYNTHETIC_CONTEXTS {
        cached.context(context);
    }
    let change = mapping("user", "normal", &["g"], Some("updated"));
    maps.push(change.clone());
    cached.map(change);
    assert_eq!(cached.context(context).candidates(&[]), vec![3, 4, 9, 2, 6]);
    assert_eq!(
        cached.context(SYNTHETIC_CONTEXTS[1]).candidates(&[]),
        vec![8]
    );
    assert_eq!(
        cached.context(SYNTHETIC_CONTEXTS[2]).candidates(&[]),
        vec![3, 4, 0, 2, 1]
    );
    // Repeated updates cross the LOUDS overlay merge threshold and invalidate
    // compiled snapshots; previously issued cursors are dropped before editing.
    for step in 0..70 {
        let keys = vec!["更新".into(), format!("F{}", step % 12 + 1)];
        index.bind(&keys, step);
        index.finish();
        let mut cursor = index.cursor();
        assert_eq!(cursor.advance(&keys[0]), Resolution::Prefix);
        assert_eq!(cursor.advance(&keys[1]), Resolution::Command(step));
        cursor.reset();
        assert_eq!(cursor.advance(&"__missing__".into()), Resolution::Unbound);
    }
    let reverse = Context {
        scopes: &["user", "browser", "global"],
        mode: "normal",
    };
    assert_eq!(
        compile::<I>(&maps, reverse).candidates(&[]),
        vec![3, 4, 0, 2, 1]
    );
}

struct Samples {
    count: usize,
    quantiles: [f64; 3],
}

fn distribution(mut values: Vec<f64>) -> Samples {
    values.sort_unstable_by(f64::total_cmp);
    let quantiles = [50, 95, 99]
        .map(|percentile| values[(values.len() * percentile).div_ceil(100).saturating_sub(1)]);
    Samples {
        count: values.len(),
        quantiles,
    }
}

fn samples(mut operation: impl FnMut()) -> Samples {
    let limit = std::env::var("KEYMAP_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(4096);
    assert!(limit > 0, "KEYMAP_BENCH_SAMPLES must be positive");
    let minimum = limit.min(128);
    let mut values = Vec::with_capacity(limit);
    let batch = Instant::now();
    while values.len() < limit {
        let start = Instant::now();
        operation();
        values.push(start.elapsed().as_nanos() as f64);
        if values.len() >= minimum && batch.elapsed() >= Duration::from_millis(30) {
            break;
        }
    }
    distribution(values)
}

fn samples_with_setup<T>(
    mut setup: impl FnMut() -> T,
    mut operation: impl FnMut(&mut T),
) -> Samples {
    let limit = std::env::var("KEYMAP_BENCH_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(4096);
    assert!(limit > 0, "KEYMAP_BENCH_SAMPLES must be positive");
    let mut values = Vec::with_capacity(limit);
    let batch = Instant::now();
    while values.len() < limit {
        let mut input = setup();
        let start = Instant::now();
        operation(&mut input);
        values.push(start.elapsed().as_nanos() as f64);
        if values.len() >= limit.min(128) && batch.elapsed() >= Duration::from_millis(30) {
            break;
        }
    }
    distribution(values)
}

fn mutation_samples<I: Index + Clone>(index: &I, mut operation: impl FnMut(&mut I)) -> Samples {
    // Restoration and destruction are inside every operation sample. These
    // rows describe independent edit cycles, not an in-place steady-state edit.
    samples(|| {
        let mut input = black_box(index).clone();
        operation(black_box(&mut input));
        input.finish();
        black_box(input);
    })
}

fn report(fixture: &Fixture, phase: &str, structure: &str, case: &str, samples: Samples) {
    println!(
        "{},{},{phase},{structure},{case},{},{:.1},{:.1},{:.1}",
        fixture.name,
        fixture.size,
        samples.count,
        samples.quantiles[0],
        samples.quantiles[1],
        samples.quantiles[2]
    );
}

fn fanout(fixture: &Fixture) {
    let index = compile::<Trie>(&fixture.mappings, fixture.contexts[0]);
    fn walk(node: &Trie, depth: usize, histogram: &mut BTreeMap<(usize, usize), usize>) {
        *histogram.entry((depth, node.degree())).or_default() += 1;
        for child in node.child_nodes() {
            walk(child, depth + 1, histogram);
        }
    }
    let mut histogram = BTreeMap::new();
    walk(&index, 0, &mut histogram);
    for ((depth, degree), count) in histogram {
        eprintln!(
            "fanout,{},{},depth={depth},token_degree={degree},nodes={count}",
            fixture.name, fixture.size
        );
    }
}

fn queries<I: Index>(index: &I, mappings: &[Mapping]) -> [Vec<Vec<String>>; 3] {
    let bindings = index.candidates(&[]);
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    let mut prefixes = Vec::new();
    for i in 0..64 {
        let key = mappings[bindings[i * bindings.len() / 64]].keys.clone();
        hits.push(key.clone());
        let mut miss = key.clone();
        match i % 3 {
            0 => miss[0] = "__missing__".into(),
            1 => *miss.last_mut().unwrap() = "__missing__".into(),
            _ => miss.push("__missing__".into()),
        }
        misses.push(miss);
        if key.len() > 1 {
            prefixes.push(key[..1 + i % (key.len() - 1)].to_vec());
        }
    }
    if prefixes.is_empty() {
        prefixes.push(Vec::new());
    }
    for query in &hits {
        assert!(matches!(index.resolve(query), Resolution::Command(_)));
    }
    for query in &misses {
        assert_eq!(index.resolve(query), Resolution::Unbound);
    }
    for query in &prefixes {
        assert_eq!(index.resolve(query), Resolution::Prefix);
    }
    [hits, misses, prefixes]
}

fn run<I: Index + Clone>(fixture: &Fixture, name: &str, memory: bool) {
    if std::env::var("KEYMAP_BENCH_STRUCTURE")
        .ok()
        .is_some_and(|filter| !name.contains(&filter))
    {
        return;
    }
    let context = fixture.contexts[0];
    let index = compile::<I>(&fixture.mappings, context);
    let queries = queries(&index, &fixture.mappings);
    let bindings = index.candidates(&[]);
    // Check every fixture against the public production entry, including ordering,
    // command identity, scope winner, misses, and token-prefix queries.
    let production = production(&fixture.mappings);
    // Match production's candidate payload and allocation size. Name-to-ID
    // conversion belongs to fixture setup, outside enumeration timing.
    let scope_ids = fixture
        .mappings
        .iter()
        .map(|mapping| production.identities().scope(&mapping.scope).unwrap())
        .collect::<Vec<_>>();
    let actual = production.effective_bindings(context.scopes, context.mode);
    assert_eq!(bindings.len(), actual.len());
    for (binding, actual) in bindings.iter().zip(&actual) {
        let mapping = &fixture.mappings[*binding];
        assert_eq!(
            (
                &mapping.keys[..],
                mapping.command.as_deref().unwrap(),
                mapping.scope.as_str()
            ),
            (
                actual.keys,
                actual.command,
                &*production.identities().scope_name(actual.scope),
            )
        );
    }
    for query in queries.iter().flatten() {
        let actual = match production.resolve(context.scopes, context.mode, query) {
            KeyResolution::Command(command) => {
                let Resolution::Command(binding) = index.resolve(query) else {
                    panic!("command mismatch")
                };
                assert_eq!(fixture.mappings[binding].command.as_deref(), Some(command));
                Resolution::Command(binding)
            }
            KeyResolution::Prefix => Resolution::Prefix,
            KeyResolution::Unbound => Resolution::Unbound,
        };
        assert_eq!(index.resolve(query), actual);
    }
    for query in queries.iter().flatten() {
        let mut cursor = index.cursor();
        for length in 1..=query.len() {
            assert_eq!(
                cursor.advance(&query[length - 1]),
                index.resolve(&query[..length]),
                "{name}"
            );
        }
    }
    if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_some() {
        return;
    }
    if memory {
        let mut cursor = index.cursor();
        let label = format!("{}:{name}:steady_advance_sequence_with_reset", fixture.name);
        allocations::report(fixture.size, &label, || {
            cursor.reset();
            for key in &queries[0][0] {
                black_box(dispatch(&mut cursor, key));
            }
        });
        drop(cursor);
        let label = format!("{}:{name}:index", fixture.name);
        allocations::report(fixture.size, &label, || {
            compile::<I>(&fixture.mappings, context)
        });
        let label = format!("{}:{name}:cache_fixture_3_contexts", fixture.name);
        allocations::report(fixture.size, &label, || {
            let cache = Cached::<I>::new(&fixture.mappings);
            for context in fixture.contexts {
                black_box(cache.context(context));
            }
            cache
        });
        for count in [1, 3, 12] {
            let label = format!("{}:{name}:cache_matrix_{count}_contexts", fixture.name);
            allocations::report(fixture.size, &label, || {
                let cache = Cached::<I>::new(&fixture.mappings);
                for context in &context_shapes()[..count] {
                    black_box(cache.context(*context));
                }
                cache
            });
        }
        let label = format!("{}:{name}:clone_and_one_edit", fixture.name);
        allocations::report(fixture.size, &label, || {
            let mut input = index.clone();
            input.bind(&["F12".into(), "F1".into()], 0);
            input.finish();
            input
        });
        let label = format!("{}:{name}:enumerate_all", fixture.name);
        allocations::report(fixture.size, &label, || {
            candidate_bindings(&index, &fixture.mappings, &scope_ids, &[])
        });
        return;
    }
    eprintln!(
        "dataset={} base={} declarations={} effective={} structure={name}",
        fixture.name,
        fixture.size,
        fixture.mappings.len(),
        bindings.len()
    );
    report(
        fixture,
        "compile",
        name,
        "layered_context_and_drop",
        samples(|| {
            black_box(compile::<I>(black_box(&fixture.mappings), context));
        }),
    );
    for (case, queries) in ["hit", "miss", "prefix"].iter().zip(&queries) {
        let mut cursor = 0;
        report(
            fixture,
            "resolve",
            name,
            case,
            samples(|| {
                let query = black_box(&queries[cursor % queries.len()]);
                cursor += 1;
                black_box(resolved_command(index.resolve(query), &fixture.mappings));
            }),
        );
    }
    for (case, queries) in [
        "hit_sequence_with_reset",
        "miss_sequence_with_reset",
        "prefix_sequence_with_reset",
    ]
    .iter()
    .zip(&queries)
    {
        let mut cursor = index.cursor();
        let mut position = 0;
        report(
            fixture,
            "advance",
            name,
            case,
            samples(|| {
                cursor.reset();
                let query = black_box(&queries[position % queries.len()]);
                position += 1;
                for key in query {
                    black_box(dispatch(&mut cursor, key));
                }
            }),
        );
        let mut cursor = index.cursor();
        let mut query_position = 0;
        let mut token_position = 0;
        report(
            fixture,
            "advance_token",
            name,
            case,
            samples(|| {
                let query = &queries[query_position % queries.len()];
                black_box(dispatch(&mut cursor, black_box(&query[token_position])));
                token_position += 1;
                if token_position == query.len() {
                    cursor.reset();
                    token_position = 0;
                    query_position += 1;
                }
            }),
        );
    }
    report(
        fixture,
        "enumerate",
        name,
        "all_ordered",
        samples(|| {
            black_box(candidate_bindings(
                &index,
                &fixture.mappings,
                &scope_ids,
                black_box(&[]),
            ));
        }),
    );
    let mut cursor = 0;
    report(
        fixture,
        "enumerate",
        name,
        "prefix_ordered",
        samples(|| {
            let query = black_box(&queries[2][cursor % queries[2].len()]);
            cursor += 1;
            black_box(candidate_bindings(
                &index,
                &fixture.mappings,
                &scope_ids,
                query,
            ));
        }),
    );
    let add = vec!["__new__".into(), "space".into(), "Enter".into()];
    let replacement = &queries[0][0];
    let subtree = &queries[2][0];
    report(
        fixture,
        "mutate_clone_edit_drop",
        name,
        "add",
        mutation_samples(&index, |index| index.bind(black_box(&add), 0)),
    );
    report(
        fixture,
        "mutate_clone_edit_drop",
        name,
        "replace",
        mutation_samples(&index, |index| index.bind(black_box(replacement), 0)),
    );
    report(
        fixture,
        "mutate_clone_edit_drop",
        name,
        "bind_ancestor",
        mutation_samples(&index, |index| index.bind(black_box(subtree), 0)),
    );
    let mut descendant = replacement.clone();
    descendant.push("__child__".into());
    report(
        fixture,
        "mutate_clone_edit_drop",
        name,
        "bind_descendant",
        mutation_samples(&index, |index| index.bind(black_box(&descendant), 0)),
    );
    report(
        fixture,
        "mutate_clone_edit_drop",
        name,
        "unmap_subtree",
        mutation_samples(&index, |index| index.unmap(black_box(subtree))),
    );
    let cached = Cached::<I>::new(&fixture.mappings);
    for context in fixture.contexts {
        cached.context(context);
    }
    report(
        fixture,
        "cache",
        name,
        "cold_context_compile_setup_drop_excluded",
        samples_with_setup(
            || Cached::<I>::new(&fixture.mappings),
            |cache| {
                black_box(cache.context(context));
            },
        ),
    );
    report(
        fixture,
        "cache",
        name,
        "warm_context_acquire",
        samples(|| {
            black_box(cached.context(context));
        }),
    );
    let mut switch = 0;
    report(
        fixture,
        "cache",
        name,
        "warm_context_switch_3",
        samples(|| {
            switch = (switch + 1) % fixture.contexts.len();
            black_box(cached.context(fixture.contexts[switch]));
        }),
    );
    let mut cursor = 0;
    report(
        fixture,
        "cache",
        name,
        "hit",
        samples(|| {
            let query = black_box(&queries[0][cursor % queries[0].len()]);
            cursor += 1;
            black_box(resolved_command(
                cached.context(context).resolve(query),
                &cached.mappings,
            ));
        }),
    );
    let change = mapping(
        context.scopes.last().unwrap(),
        context.mode,
        &["F12"],
        Some("new-command"),
    );
    let rebuild = samples_with_setup(
        || {
            let input = Cached::<I>::new(&fixture.mappings);
            for context in fixture.contexts {
                black_box(input.context(context));
            }
            input
        },
        |input| {
            input.map(black_box(change.clone()));
            for context in fixture.contexts {
                black_box(input.context(context));
            }
        },
    );
    report(
        fixture,
        "cache",
        name,
        "map_invalidate_rebuild_3_setup_drop_excluded",
        rebuild,
    );
    for scope_count in [1, 3, 8] {
        let scopes: &[&'static [&'static str]] = &[
            &["global"],
            &["global", "browser", "user"],
            &[
                "global", "browser", "user", "text", "data", "diff", "files", "git",
            ],
        ];
        let scopes = scopes[match scope_count {
            1 => 0,
            3 => 1,
            _ => 2,
        }];
        let contexts =
            ["normal", "select", "insert", "prompt"].map(|mode| Context { scopes, mode });
        let cache = Cached::<I>::new(&fixture.mappings);
        for context in contexts {
            black_box(cache.context(context));
        }
        let mut position = 0;
        report(
            fixture,
            "cache",
            name,
            &format!("warm_switch_4_modes_{scope_count}_scopes"),
            samples(|| {
                position = (position + 1) % contexts.len();
                let index = cache.context(contexts[position]);
                let mut cursor = index.cursor();
                black_box(dispatch(&mut cursor, &queries[0][0][0]));
            }),
        );
        report(
            fixture,
            "cache",
            name,
            &format!("compile_4_modes_{scope_count}_scopes_setup_drop_excluded"),
            samples_with_setup(
                || Cached::<I>::new(&fixture.mappings),
                |cache| {
                    for context in contexts {
                        black_box(cache.context(context));
                    }
                },
            ),
        );
    }
    let shapes = context_shapes();
    let matrix = Cached::<I>::new(&fixture.mappings);
    for context in shapes {
        black_box(matrix.context(context));
    }
    let mut position = 0;
    report(
        fixture,
        "cache",
        name,
        "warm_switch_12_contexts",
        samples(|| {
            position = (position + 1) % shapes.len();
            let index = matrix.context(shapes[position]);
            let mut cursor = index.cursor();
            black_box(dispatch(&mut cursor, &queries[0][0][0]));
        }),
    );
    if name == "louds_base_delta_merge64" {
        report(
            fixture,
            "overlay",
            name,
            "delta_bind_setup_drop_excluded",
            samples_with_setup(
                || index.clone(),
                |input| {
                    input.bind(&["F12".into(), "F1".into()], 0);
                    input.finish();
                },
            ),
        );
        report(
            fixture,
            "overlay",
            name,
            "delta_unmap_setup_drop_excluded",
            samples_with_setup(
                || index.clone(),
                |input| {
                    input.unmap(&queries[0][0]);
                    input.finish();
                },
            ),
        );
        report(
            fixture,
            "overlay",
            name,
            "threshold_64th_edit_merge_setup_drop_excluded",
            samples_with_setup(
                || {
                    let mut input = index.clone();
                    for _ in 0..63 {
                        input.bind(&["F12".into(), "F1".into()], 0);
                    }
                    input
                },
                |input| {
                    input.bind(&["F12".into(), "F2".into()], 0);
                    input.finish();
                },
            ),
        );
        for count in [1, 63, 64] {
            report(
                fixture,
                "overlay",
                name,
                &format!("{count}_edits_with_clone_drop"),
                mutation_samples(&index, |index| {
                    for edit in 0..count {
                        index.bind(&["F12".into(), format!("F{}", edit % 12 + 1)], 0);
                    }
                }),
            );
        }
        let mut delta = index.clone();
        let mut delta_mappings = fixture.mappings.clone();
        let new_binding = delta_mappings.len();
        delta_mappings.push(mapping(
            context.scopes.last().unwrap(),
            context.mode,
            &["F12", "F1"],
            Some("delta-command"),
        ));
        let mut delta_scopes = scope_ids.clone();
        delta_scopes.push(
            production
                .identities()
                .scope(context.scopes.last().unwrap())
                .unwrap(),
        );
        delta.bind(&delta_mappings[new_binding].keys, new_binding);
        let mut cursor = delta.cursor();
        let first = String::from("F12");
        let second = String::from("F1");
        report(
            fixture,
            "overlay",
            name,
            "delta_hit_sequence_reset",
            samples(|| {
                cursor.reset();
                black_box(dispatch(&mut cursor, &first));
                black_box(dispatch(&mut cursor, &second));
            }),
        );
        report(
            fixture,
            "overlay",
            name,
            "enumerate_after_delta",
            samples(|| {
                black_box(candidate_bindings(
                    &delta,
                    &delta_mappings,
                    &delta_scopes,
                    &[],
                ));
            }),
        );
    }
}

fn run_production(fixture: &Fixture, memory: bool) {
    let name = "production_flat_btree_dispatch";
    if std::env::var("KEYMAP_BENCH_STRUCTURE")
        .ok()
        .is_some_and(|filter| !name.contains(&filter))
    {
        return;
    }
    let context = fixture.contexts[0];
    let keymap = production(&fixture.mappings);
    let index = compile::<Trie>(&fixture.mappings, context);
    let queries = queries(&index, &fixture.mappings);
    let scopes = context
        .scopes
        .iter()
        .map(|scope| keymap.identities().scope(scope).unwrap())
        .collect::<Vec<_>>();
    let mode = keymap.identities().mode(context.mode).unwrap();
    // Whole-query search and dispatch have different terminal lifecycles. This
    // oracle resets its pending input after Command/Unbound, just as production.
    for query in queries.iter().flatten() {
        let mut state = KeyState::default();
        let mut pending = Vec::new();
        for key in query {
            pending.push(key.clone());
            let expected = index.resolve(&pending);
            match (
                keymap.advance_ids(&mut state, &scopes, mode, key),
                &expected,
            ) {
                (KeyResolution::Command(command), Resolution::Command(binding)) => {
                    assert_eq!(Some(command), fixture.mappings[*binding].command.as_deref())
                }
                (KeyResolution::Prefix, Resolution::Prefix)
                | (KeyResolution::Unbound, Resolution::Unbound) => (),
                _ => panic!("production dispatch mismatch"),
            }
            if !matches!(expected, Resolution::Prefix) {
                pending.clear();
            }
        }
    }
    if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_some() {
        return;
    }
    let mut state = KeyState::default();
    black_box(keymap.advance_ids(&mut state, &scopes, mode, &queries[0][0][0]));
    state.reset();
    if memory {
        for count in [1, 3, 12] {
            let label = format!("{}:{name}:cache_matrix_{count}_contexts", fixture.name);
            allocations::report(fixture.size, &label, || {
                let keymap = production(&fixture.mappings);
                for context in &context_shapes()[..count] {
                    black_box(keymap.resolve(context.scopes, context.mode, &[]));
                }
                keymap
            });
        }
        let label = format!("{}:{name}:steady_advance_sequence_with_reset", fixture.name);
        allocations::report(fixture.size, &label, || {
            state.reset();
            for key in &queries[0][0] {
                black_box(keymap.advance_ids(&mut state, &scopes, mode, key));
            }
        });
        let label = format!("{}:{name}:enumerate_all", fixture.name);
        allocations::report(fixture.size, &label, || {
            keymap.effective_bindings_ids(&scopes, mode)
        });
        let label = format!(
            "{}:{name}:build_warm_map_invalidate_rebuild_3",
            fixture.name
        );
        allocations::report(fixture.size, &label, || {
            let mut input = production(&fixture.mappings);
            for context in fixture.contexts {
                black_box(input.resolve(context.scopes, context.mode, &[]));
            }
            input.map(
                context.scopes.last().unwrap(),
                context.mode,
                vec!["F12".into()],
                Some("new-command".into()),
            );
            for context in fixture.contexts {
                black_box(input.resolve(context.scopes, context.mode, &[]));
            }
            input
        });
        return;
    }
    for (case, queries) in ["hit", "miss", "prefix"].iter().zip(&queries) {
        let mut position = 0;
        report(
            fixture,
            "resolve",
            name,
            case,
            samples(|| {
                let query = black_box(&queries[position % queries.len()]);
                position += 1;
                black_box(keymap.resolve_ids(&scopes, mode, query));
            }),
        );
        let mut position = 0;
        report(
            fixture,
            "advance",
            name,
            &format!("{case}_sequence_with_reset"),
            samples(|| {
                state.reset();
                let query = black_box(&queries[position % queries.len()]);
                position += 1;
                for key in query {
                    black_box(keymap.advance_ids(&mut state, &scopes, mode, key));
                }
            }),
        );
        let mut position = 0;
        let mut token = 0;
        state.reset();
        report(
            fixture,
            "advance_token",
            name,
            &format!("{case}_sequence_with_reset"),
            samples(|| {
                let query = &queries[position % queries.len()];
                black_box(keymap.advance_ids(&mut state, &scopes, mode, black_box(&query[token])));
                token += 1;
                if token == query.len() {
                    state.reset();
                    token = 0;
                    position += 1;
                }
            }),
        );
    }
    report(
        fixture,
        "enumerate",
        name,
        "all_ordered",
        samples(|| {
            black_box(keymap.effective_bindings_ids(&scopes, mode));
        }),
    );
    let mut position = 0;
    report(
        fixture,
        "enumerate",
        name,
        "prefix_ordered",
        samples(|| {
            let prefix = &queries[2][position % queries[2].len()];
            position += 1;
            // The public production API currently enumerates a context and then the
            // consumer filters by prefix. Include both allocation and filtering.
            let mut bindings = keymap.effective_bindings_ids(&scopes, mode);
            bindings.retain(|binding| binding.keys.starts_with(prefix));
            black_box(bindings);
        }),
    );
    report(
        fixture,
        "cache",
        name,
        "cold_context_compile_setup_drop_excluded",
        samples_with_setup(
            || production(&fixture.mappings),
            |keymap| {
                black_box(keymap.resolve(context.scopes, context.mode, &[]));
            },
        ),
    );
    for scope_count in [1, 3, 8] {
        let contexts = &context_shapes()[match scope_count {
            1 => 0..4,
            3 => 4..8,
            _ => 8..12,
        }];
        for context in contexts {
            black_box(keymap.resolve(context.scopes, context.mode, &[]));
        }
        let typed = contexts
            .iter()
            .map(|context| {
                (
                    context
                        .scopes
                        .iter()
                        .map(|scope| keymap.identities().scope(scope).unwrap())
                        .collect::<Vec<_>>(),
                    keymap.identities().mode(context.mode).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let mut position = 0;
        report(
            fixture,
            "cache",
            name,
            &format!("warm_switch_4_modes_{scope_count}_scopes"),
            samples(|| {
                position = (position + 1) % typed.len();
                black_box(keymap.advance_ids(
                    &mut state,
                    &typed[position].0,
                    typed[position].1,
                    &queries[0][0][0],
                ));
            }),
        );
        report(
            fixture,
            "cache",
            name,
            &format!("compile_4_modes_{scope_count}_scopes_setup_drop_excluded"),
            samples_with_setup(
                || production(&fixture.mappings),
                |keymap| {
                    for context in contexts {
                        black_box(keymap.resolve(context.scopes, context.mode, &[]));
                    }
                },
            ),
        );
    }
    let shapes = context_shapes();
    for context in shapes {
        black_box(keymap.resolve(context.scopes, context.mode, &[]));
    }
    let typed = shapes
        .iter()
        .map(|context| {
            (
                context
                    .scopes
                    .iter()
                    .map(|scope| keymap.identities().scope(scope).unwrap())
                    .collect::<Vec<_>>(),
                keymap.identities().mode(context.mode).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut position = 0;
    report(
        fixture,
        "cache",
        name,
        "warm_switch_12_contexts",
        samples(|| {
            position = (position + 1) % typed.len();
            black_box(keymap.advance_ids(
                &mut state,
                &typed[position].0,
                typed[position].1,
                &queries[0][0][0],
            ));
        }),
    );
    let rebuild = samples_with_setup(
        || {
            let keymap = production(&fixture.mappings);
            for context in fixture.contexts {
                black_box(keymap.resolve(context.scopes, context.mode, &[]));
            }
            keymap
        },
        |keymap| {
            keymap.map(
                context.scopes.last().unwrap(),
                context.mode,
                vec!["F12".into()],
                Some("new-command".into()),
            );
            for context in fixture.contexts {
                black_box(keymap.resolve(context.scopes, context.mode, &[]));
            }
        },
    );
    report(
        fixture,
        "cache",
        name,
        "map_invalidate_rebuild_3_setup_drop_excluded",
        rebuild,
    );
}

fn main() {
    conformance::<Trie>();
    conformance::<HashIndex>();
    conformance::<HashTrie>();
    conformance::<Sorted>();
    conformance::<Radix>();
    conformance::<Art>();
    conformance::<FstIndex>();
    conformance::<LoudsIndex>();
    conformance::<DoubleArrayIndex>();
    conformance::<QpTrie>();
    conformance::<Packed>();
    conformance::<Adaptive>();
    conformance::<Hybrid>();
    conformance::<LoudsDelta>();
    conformance::<indexes::NodeStorage<indexes::BTreeNode>>();
    conformance::<indexes::NodeStorage<indexes::LeafNode>>();
    conformance::<indexes::NodeStorage<indexes::InlineNode>>();
    if std::env::var_os("KEYMAP_BENCH_BATCH").is_some() {
        batch::run();
        return;
    }
    let memory = std::env::var_os("KEYMAP_BENCH_MEMORY").is_some();
    if !memory && std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_none() {
        let overhead = samples(|| {
            black_box(());
        });
        eprintln!(
            "method=individual_operation_ns; timer_empty_p50_ns={:.1};timer_empty_p95_ns={:.1};timer_empty_p99_ns={:.1}; reported_values_include_timer_overhead; mutations_include_clone_drop; dispatch_only_no_action_execution;defaults_literal_maps_exclude_native_plugin_registration",
            overhead.quantiles[0], overhead.quantiles[1], overhead.quantiles[2]
        );
        eprintln!(
            "excluded_from_regular_timing=sorted_vec,byte_fst,byte_double_array,static_louds,byte_qp,louds_base_delta_merge64; retained_for_conformance; louds_experiment_select_with_KEYMAP_BENCH_STRUCTURE=louds"
        );
    }
    if memory {
        println!(
            "base_bindings,dataset_structure_phase,allocations,reallocations,allocated_bytes,retained_bytes,peak_bytes"
        );
    } else {
        println!(
            "dataset,base_bindings,phase,structure,case,operation_samples,p50_ns,p95_ns,p99_ns"
        );
    }
    let filter = std::env::var("KEYMAP_BENCH_FILTER").ok();
    for fixture in fixtures() {
        if filter
            .as_ref()
            .is_some_and(|filter| !fixture.name.contains(filter))
        {
            continue;
        }
        fanout(&fixture);
        run::<Trie>(&fixture, "sequence_btree_trie", memory);
        run::<indexes::NodeStorage<indexes::BTreeNode>>(&fixture, "flat_btree_storage", memory);
        run::<indexes::NodeStorage<indexes::LeafNode>>(&fixture, "flat_leaf_enum", memory);
        run::<indexes::NodeStorage<indexes::InlineNode>>(&fixture, "flat_inline_one_enum", memory);
        run::<Packed>(&fixture, "packed_csr_with_source", memory);
        run::<Adaptive>(&fixture, "adaptive_smallvec_btree", memory);
        run::<Hybrid>(&fixture, "hybrid_direct_multi_trie", memory);
        if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_some()
            || std::env::var("KEYMAP_BENCH_STRUCTURE")
                .ok()
                .is_some_and(|filter| "louds_base_delta_merge64".contains(&filter))
        {
            run::<LoudsDelta>(&fixture, "louds_base_delta_merge64", memory);
        }
        run::<HashIndex>(&fixture, "hash_ordered_requery", memory);
        run::<HashTrie>(&fixture, "hash_children_trie", memory);

        run::<Radix>(&fixture, "token_radix", memory);
        run::<Art>(&fixture, "byte_art_requery", memory);

        run_production(&fixture, memory);
    }
}
