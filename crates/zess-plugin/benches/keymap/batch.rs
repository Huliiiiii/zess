use super::*;

const OPERATIONS: usize = 4096;
const REPEATS: usize = 64;

fn code(answer: KeyResolution<&str>) -> usize {
    match answer {
        KeyResolution::Command(command) => command.len() + 3,
        KeyResolution::Prefix => 1,
        KeyResolution::Unbound => 2,
    }
}

fn row(
    fixture: &Fixture,
    structure: &str,
    case: &str,
    method: &str,
    repeat: usize,
    operations: usize,
    tokens: usize,
    completed: usize,
    elapsed: u128,
) {
    println!(
        "{},{},{structure},{case},{method},{repeat},{operations},{tokens},{completed},{elapsed},{:.4},{:.4}",
        fixture.name,
        fixture.size,
        elapsed as f64 / operations as f64,
        elapsed as f64 / tokens.max(1) as f64,
    );
}

fn measure(
    fixture: &Fixture,
    structure: &str,
    case: &str,
    queries: &[Vec<String>],
    mut operation: impl FnMut(&[String]) -> usize,
) {
    if queries.is_empty() {
        return;
    }
    let mut position = 0;
    // Every query is visited in order. Each token lookup depends on the prior
    // cursor state; input, token and result barriers prevent constant folding.
    // Do not use a result-derived stride: it can cycle over a small query subset.
    for _ in 0..OPERATIONS * 2 {
        black_box(operation(black_box(&queries[black_box(position)])));
        position = (position + 1) % queries.len();
    }
    let mut batches = Vec::with_capacity(REPEATS);
    for _ in 0..REPEATS {
        let mut tokens = 0;
        let mut completed_commands = 0;
        let start = Instant::now();
        for _ in 0..OPERATIONS {
            let query = black_box(&queries[black_box(position)]);
            tokens += query.len();
            let answer = black_box(operation(query));
            completed_commands += usize::from(answer > 2);
            position = (position + 1) % queries.len();
        }
        let elapsed = start.elapsed().as_nanos();
        batches.push((tokens, completed_commands, elapsed));
    }
    for (repeat, (tokens, completed, elapsed)) in batches.into_iter().enumerate() {
        row(
            fixture, structure, case, "batch", repeat, OPERATIONS, tokens, completed, elapsed,
        );
    }
    // Individual samples retain operation-level latency/timer noise. They are
    // separate from batch-average distributions and never timer-subtracted.
    for repeat in 0..512 {
        let query = black_box(&queries[black_box(position)]);
        let start = Instant::now();
        let answer = black_box(operation(query));
        let elapsed = start.elapsed().as_nanos();
        position = (position + 1) % queries.len();
        row(
            fixture,
            structure,
            case,
            "individual",
            repeat,
            1,
            query.len(),
            usize::from(answer > 2),
            elapsed,
        );
    }
}

fn classes(fixture: &Fixture) -> Vec<(&'static str, Vec<Vec<String>>)> {
    let index = compile::<Trie>(&fixture.mappings, fixture.contexts[0]);
    let mut single = Vec::new();
    let mut multi = Vec::new();
    for binding in index.candidates(&[]) {
        let keys = fixture.mappings[binding].keys.clone();
        if keys.len() == 1 {
            single.push(keys);
        } else {
            multi.push(keys);
        }
    }
    let [_, misses, prefixes] = queries(&index, &fixture.mappings);
    let mut classes = vec![
        ("single_hit", single),
        ("multi_hit", multi),
        ("prefix", prefixes),
        ("miss", misses),
        ("root_miss", vec![vec!["__missing__".into()]]),
    ];
    if fixture.name == "defaults_text" {
        // Authored query traces, not recorded human input. All run in the same
        // global+text/normal context; actions (and their mode changes) do not run.
        for (name, trace) in [
            (
                "flow_navigation",
                vec![
                    vec!["j"],
                    vec!["j"],
                    vec!["l"],
                    vec!["w"],
                    vec!["b"],
                    vec!["e"],
                    vec!["h"],
                    vec!["k"],
                    vec!["g", "g"],
                    vec!["j"],
                    vec!["j"],
                    vec!["C-d"],
                    vec!["n"],
                    vec!["n"],
                    vec!["g", "e"],
                    vec!["C-w", "w"],
                ],
            ),
            (
                "flow_edit",
                vec![
                    vec!["w"],
                    vec!["v"],
                    vec!["e"],
                    vec!["d"],
                    vec!["p"],
                    vec!["u"],
                    vec!["U"],
                    vec!["x"],
                    vec!["y"],
                    vec!["p"],
                    vec!["m", "i"],
                    vec!["space", "c"],
                    vec!["space", "y"],
                    vec!["C-w", "v"],
                    vec!["space"],
                    vec!["__missing__"],
                ],
            ),
            (
                "flow_windows",
                vec![
                    vec!["C-w", "v"],
                    vec!["C-w", "w"],
                    vec!["C-w", "h"],
                    vec!["C-w", "l"],
                    vec!["C-w", "s"],
                    vec!["j"],
                    vec!["k"],
                    vec!["C-w", "q"],
                    vec!["space", "e"],
                    vec!["space", "g"],
                    vec!["space"],
                    vec!["__missing__"],
                ],
            ),
        ] {
            let trace = trace
                .into_iter()
                .map(|keys| keys.into_iter().map(String::from).collect::<Vec<_>>())
                .collect::<Vec<_>>();
            let mut counts = [0; 4];
            for keys in &trace {
                match index.resolve(keys) {
                    Resolution::Command(_) if keys.len() == 1 => counts[0] += 1,
                    Resolution::Command(_) => counts[1] += 1,
                    Resolution::Prefix => counts[2] += 1,
                    Resolution::Unbound => counts[3] += 1,
                }
            }
            eprintln!(
                "profile={name};source=authored_scenario_not_human_log;scopes=global+text;mode=normal;single={};multi={};prefix={};miss={};query_sequences={trace:?}",
                counts[0], counts[1], counts[2], counts[3]
            );
            let labels = match name {
                "flow_navigation" => [
                    "navigation_single_hit",
                    "navigation_multi_hit",
                    "navigation_prefix",
                    "navigation_root_miss",
                ],
                "flow_edit" => [
                    "edit_single_hit",
                    "edit_multi_hit",
                    "edit_prefix",
                    "edit_root_miss",
                ],
                "flow_windows" => [
                    "windows_single_hit",
                    "windows_multi_hit",
                    "windows_prefix",
                    "windows_root_miss",
                ],
                _ => unreachable!(),
            };
            for (category, label) in labels.into_iter().enumerate() {
                let queries = trace
                    .iter()
                    .filter(|keys| {
                        let actual = match index.resolve(keys) {
                            Resolution::Command(_) if keys.len() == 1 => 0,
                            Resolution::Command(_) => 1,
                            Resolution::Prefix => 2,
                            Resolution::Unbound => 3,
                        };
                        actual == category
                    })
                    .cloned()
                    .collect();
                classes.push((label, queries));
            }
            classes.push((name, trace));
        }
    }
    classes
}

fn auxiliary<I>(
    fixture: &Fixture,
    structure: &str,
    mut build: impl FnMut() -> I,
    mut enumerate: impl FnMut(&I) -> usize,
) {
    let index = build();
    for repeat in 0..32 {
        let start = Instant::now();
        black_box(enumerate(black_box(&index)));
        row(
            fixture,
            structure,
            "all_ordered",
            "individual",
            repeat,
            1,
            0,
            0,
            start.elapsed().as_nanos(),
        );
        // Source declarations already exist; final index drop is excluded.
        let start = Instant::now();
        let input = black_box(build());
        let elapsed = start.elapsed().as_nanos();
        row(
            fixture,
            structure,
            "cold_context_compile_drop_excluded",
            "individual",
            repeat,
            1,
            0,
            0,
            elapsed,
        );
        black_box(input);
    }
}

fn rebuild_candidate<I: Index>(fixture: &Fixture, structure: &str) {
    for repeat in 0..32 {
        let mut cache = Cached::<I>::new(&fixture.mappings);
        for context in fixture.contexts {
            black_box(cache.context(context));
        }
        let context = fixture.contexts[0];
        let change = mapping(
            context.scopes.last().unwrap(),
            context.mode,
            &["F12"],
            Some("new-command"),
        );
        let start = Instant::now();
        cache.map(black_box(change));
        for context in fixture.contexts {
            black_box(cache.context(context));
        }
        row(
            fixture,
            structure,
            "map_invalidate_rebuild_3_drop_excluded",
            "individual",
            repeat,
            1,
            0,
            0,
            start.elapsed().as_nanos(),
        );
    }
}

fn candidate<I: Index + Clone>(fixture: &Fixture, structure: &str) {
    let context = fixture.contexts[0];
    let index = compile::<I>(&fixture.mappings, context);
    let oracle = compile::<Trie>(&fixture.mappings, context);
    assert_eq!(index.candidates(&[]), oracle.candidates(&[]));
    let keymap = production(&fixture.mappings);
    let scope_ids = fixture
        .mappings
        .iter()
        .map(|mapping| keymap.identities().scope(&mapping.scope).unwrap())
        .collect::<Vec<_>>();
    for (case, queries) in classes(fixture) {
        let mut cursor = index.cursor();
        for query in &queries {
            assert_eq!(index.resolve(query), oracle.resolve(query));
            cursor.reset();
            let mut pending = Vec::new();
            for key in query {
                pending.push(key.clone());
                let actual = dispatch(&mut cursor, key);
                let expected = oracle.resolve(&pending);
                assert_eq!(actual, expected);
                if !matches!(expected, Resolution::Prefix) {
                    pending.clear();
                }
            }
        }
        if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_some() {
            continue;
        }
        measure(fixture, structure, case, &queries, |query| {
            cursor.reset();
            let mut answer = Resolution::Unbound;
            for key in query {
                answer = black_box(dispatch(&mut cursor, black_box(key)));
            }
            code(resolved_command(answer, &fixture.mappings))
        });
    }
    if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_some() {
        return;
    }
    auxiliary(
        fixture,
        structure,
        || compile::<I>(&fixture.mappings, context),
        |index| candidate_bindings(index, &fixture.mappings, &scope_ids, &[]).len(),
    );
    rebuild_candidate::<I>(fixture, structure);
}

fn current(fixture: &Fixture) {
    let structure = "production_flat_btree_dispatch";
    let context = fixture.contexts[0];
    let keymap = production(&fixture.mappings);
    let scopes = context
        .scopes
        .iter()
        .map(|scope| keymap.identities().scope(scope).unwrap())
        .collect::<Vec<_>>();
    let mode = keymap.identities().mode(context.mode).unwrap();
    let mut state = KeyState::default();
    black_box(keymap.resolve_ids(&scopes, mode, &[]));
    let oracle = compile::<Trie>(&fixture.mappings, context);
    for (case, queries) in classes(fixture) {
        for query in &queries {
            state.reset();
            let mut pending = Vec::new();
            for key in query {
                pending.push(key.clone());
                let expected = oracle.resolve(&pending);
                match (
                    keymap.advance_ids(&mut state, &scopes, mode, key),
                    &expected,
                ) {
                    (KeyResolution::Command(command), Resolution::Command(binding)) => {
                        assert_eq!(Some(command), fixture.mappings[*binding].command.as_deref())
                    }
                    (KeyResolution::Prefix, Resolution::Prefix)
                    | (KeyResolution::Unbound, Resolution::Unbound) => (),
                    _ => panic!("production batch dispatch mismatch"),
                }
                if !matches!(oracle.resolve(&pending), Resolution::Prefix) {
                    pending.clear();
                }
            }
        }
        if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_some() {
            continue;
        }
        measure(fixture, structure, case, &queries, |query| {
            state.reset();
            let mut answer = KeyResolution::Unbound;
            for key in query {
                answer = black_box(keymap.advance_ids(&mut state, &scopes, mode, black_box(key)));
            }
            code(answer)
        });
    }
    if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_some() {
        return;
    }
    // Declarations/identity registration are setup, matching candidate compile.
    for repeat in 0..32 {
        let input = production(&fixture.mappings);
        let start = Instant::now();
        black_box(input.resolve_ids(&scopes, mode, &[]));
        let elapsed = start.elapsed().as_nanos();
        row(
            fixture,
            structure,
            "cold_context_compile_drop_excluded",
            "individual",
            repeat,
            1,
            0,
            0,
            elapsed,
        );
        drop(input);
        let start = Instant::now();
        black_box(keymap.effective_bindings_ids(&scopes, mode));
        row(
            fixture,
            structure,
            "all_ordered",
            "individual",
            repeat,
            1,
            0,
            0,
            start.elapsed().as_nanos(),
        );
    }
    for repeat in 0..32 {
        let mut input = production(&fixture.mappings);
        for context in fixture.contexts {
            black_box(input.resolve(context.scopes, context.mode, &[]));
        }
        let keys = vec!["F12".into()];
        let command = Some("new-command".into());
        let start = Instant::now();
        input.map(context.scopes.last().unwrap(), context.mode, keys, command);
        for context in fixture.contexts {
            black_box(input.resolve(context.scopes, context.mode, &[]));
        }
        row(
            fixture,
            structure,
            "map_invalidate_rebuild_3_drop_excluded",
            "individual",
            repeat,
            1,
            0,
            0,
            start.elapsed().as_nanos(),
        );
    }
}

pub(super) fn run() {
    println!(
        "dataset,base_bindings,structure,case,method,repeat,operations,tokens,completed_commands_at_query_end,elapsed_ns,ns_per_operation,ns_per_token"
    );
    eprintln!(
        "batch_operations={OPERATIONS}; repeats={REPEATS}; warmup_operations={}; individual_samples=512; dispatch_only_no_action_execution; all_queries_visited_in_order;token_lookup_depends_on_prior_cursor_state; cold_compile_includes_temporary_trie_and_snapshot_construction; final_index_drop_excluded; declaration_and_identity_setup_excluded; source_defaults_literal_maps_exclude_native_registration",
        OPERATIONS * 2
    );
    if std::env::var_os("KEYMAP_BENCH_CHECK_ONLY").is_none() {
        let timer = samples(|| {
            black_box(());
        });
        eprintln!(
            "empty_timer_individual_p50_ns={};p95_ns={};p99_ns={};no_overhead_subtraction",
            timer.quantiles[0], timer.quantiles[1], timer.quantiles[2]
        );
    }
    eprintln!(
        "node_sizes; leaf_enum={}; inline_one_enum={}; recursive_trie={}; flat_btree={}",
        std::mem::size_of::<indexes::LeafNode>(),
        std::mem::size_of::<indexes::InlineNode>(),
        std::mem::size_of::<Trie>(),
        std::mem::size_of::<indexes::BTreeNode>()
    );
    let filter = std::env::var("KEYMAP_BENCH_FILTER").ok();
    for fixture in fixtures() {
        if filter
            .as_ref()
            .is_some_and(|filter| !fixture.name.contains(filter))
        {
            continue;
        }
        fanout(&fixture);
        let index = compile::<Trie>(&fixture.mappings, fixture.contexts[0]);
        eprintln!(
            "dataset={}; base={}; declarations={}; effective={}; scopes={:?}; mode={}",
            fixture.name,
            fixture.size,
            fixture.mappings.len(),
            index.candidates(&[]).len(),
            fixture.contexts[0].scopes,
            fixture.contexts[0].mode
        );
        current(&fixture);
        candidate::<Trie>(&fixture, "sequence_btree_trie");
        candidate::<indexes::NodeStorage<indexes::BTreeNode>>(&fixture, "flat_btree_storage");
        candidate::<Hybrid>(&fixture, "hybrid_direct_multi_trie");
        candidate::<indexes::NodeStorage<indexes::LeafNode>>(&fixture, "flat_leaf_enum");
        candidate::<indexes::NodeStorage<indexes::InlineNode>>(&fixture, "flat_inline_one_enum");
    }
}
