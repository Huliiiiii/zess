use super::*;

#[test]
fn script_displays_keep_structured_status_separate_from_legacy_text() {
    let mut engine = Engine::new();
    register(&mut engine);
    let values = engine
        .run(
            r#"
        (require-builtin zess/native)
        (buffer-output
          (with-status-data
            (buffer-display "Files" "" '() #f "legacy")
            (hash 'pending_count 2))
          #f '())
    "#,
        )
        .unwrap();
    let display = output(values.last().unwrap()).unwrap().display.unwrap();
    assert_eq!(display.status, "legacy");
    assert_eq!(display.status_data, serde_json::json!({"pending_count": 2}));
    let serialized = serde_json::to_value(&display).unwrap();
    let restored: Display = serde_json::from_value(serialized).unwrap();
    assert_eq!(restored.status_data, display.status_data);
    let legacy: Display = serde_json::from_value(serde_json::json!({"status": "legacy"})).unwrap();
    assert_eq!(legacy.status, "legacy");
    assert_eq!(legacy.status_data, serde_json::Value::Null);
}

#[test]
fn script_cells_preserve_each_styled_span() {
    let mut engine = Engine::new();
    register(&mut engine);
    let values = engine
        .run(
            r#"
        (require-builtin zess/native)
        (define row
            (with-row-cells (display-row "file" "name" "" 0)
                (list (cell-append-span (display-cell "+" "git.added") "12" "number"))))
        (buffer-output
            (buffer-display "Files" "" (list (with-row-cells row (row-cells row))) #f "")
            #f '())
    "#,
        )
        .unwrap();
    let display = output(values.last().unwrap()).unwrap().display.unwrap();
    let spans = &display.rows[0].cells[0].spans;

    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].text, "+");
    assert_eq!(spans[0].style, "git.added");
    assert_eq!(spans[1].text, "12");
    assert_eq!(spans[1].style, "number");
}

#[test]
fn script_queries_keep_the_kind_separate_from_literal_text() {
    let mut engine = Engine::new();
    register(&mut engine);
    for (kind, text, candidate, expected) in [
        ("plain", "re:foo", "foo", false),
        ("plain", "re:foo", "prefix re:foo suffix", true),
        ("regex", "foo", "prefix foo suffix", true),
        ("regex", "^foo$", "prefix foo suffix", false),
        ("glob", "*.rs", "main.rs", true),
    ] {
        let query = zess_filter::Query {
            kind: match kind {
                "plain" => zess_filter::QueryKind::Plain,
                "glob" => zess_filter::QueryKind::Glob,
                "regex" => zess_filter::QueryKind::Regex,
                _ => unreachable!(),
            },
            text: text.into(),
        };
        let input = PluginInput {
            buffer_id: 1,
            view_id: 1,
            generation: 1,
            preview_debounce: zess_plugin::DEFAULT_PREVIEW_DEBOUNCE,
            preview_area: None,
            event: "filter_changed".into(),
            argument: serde_json::Value::Null,
            query: Some(query),
            options: serde_json::Value::Null,
            mode: "filter".into(),
            selection: ItemSelection::default(),
            register: None,
            context: zess_plugin::PluginContext::default(),
        };
        engine.register_value("input", super::input(&input).unwrap());
        engine.register_value("expected-kind", kind.into_steelval().unwrap());
        engine.register_value("expected-text", text.into_steelval().unwrap());
        engine.register_value("candidate", candidate.into_steelval().unwrap());
        let values = engine
            .run(
                r#"
            (require-builtin zess/native)
            (require-builtin zess/filter)
            (define input-query (%input-query input))
            (query-kind input-query)
            (query-text input-query)
            (filter-matches? (filter-compile input-query) candidate)
        "#,
            )
            .unwrap();
        let results = &values[values.len() - 3..];
        assert_eq!(String::from_steelval(&results[0]).unwrap(), kind);
        assert_eq!(String::from_steelval(&results[1]).unwrap(), text);
        assert_eq!(bool::from_steelval(&results[2]).unwrap(), expected);
    }
}
