use super::*;

#[test]
fn protocol_compatibility_depends_only_on_the_protocol_version() {
    let local = VersionInfo::new("local-build");
    local
        .check_compatible(&VersionInfo::new("different-build"))
        .unwrap();

    let incompatible = VersionInfo {
        protocol_version: local.protocol_version + 1,
        build_id: local.build_id.clone(),
    };
    let error = local.check_compatible(&incompatible).unwrap_err();
    assert_eq!(error.expected, local);
    assert_eq!(error.actual, incompatible);
}

#[test]
fn hello_advertises_protocol_version_and_build_id() {
    let version = VersionInfo::new("current-build");
    let expected = serde_json::json!({
        "type": "hello",
        "protocol_version": PROTOCOL_VERSION,
        "build_id": "current-build",
    });
    assert_eq!(
        serde_json::to_value(ClientMessage::Hello {
            version: version.clone(),
        })
        .unwrap(),
        expected,
    );
    assert_eq!(
        serde_json::to_value(ServerMessage::Hello { version }).unwrap(),
        expected,
    );
}

#[test]
fn messages_from_legacy_peers_preserve_the_build_id_for_a_protocol_mismatch() {
    let hello = r#"{"type":"hello","build_id":"legacy-build"}"#;
    let ClientMessage::Hello { version: client } = serde_json::from_str(hello).unwrap() else {
        panic!("expected client hello")
    };
    let ServerMessage::Hello { version: server } = serde_json::from_str(hello).unwrap() else {
        panic!("expected server hello")
    };
    let ServerMessage::Status {
        version: status, ..
    } = serde_json::from_str(
        r#"{"type":"status","build_id":"legacy-build","session_id":"id","name":null,
            "server_instance":"server","state_revision":0,"pid":42,"workspace":"/work",
            "attached":false,"modified":false}"#,
    )
    .unwrap()
    else {
        panic!("expected server status")
    };

    assert!(client.check_compatible(&server).is_err());

    let local = VersionInfo::new("current-build");
    for peer in [client, server, status] {
        let error = local.check_compatible(&peer).unwrap_err();
        assert_eq!(error.actual.protocol_version, 0);
        assert_eq!(error.actual.build_id, "legacy-build");
    }
}

#[test]
fn applying_a_delta_updates_text_and_selection_together() {
    fn snapshot(text: &str, cursor: usize) -> SessionSnapshot {
        SessionSnapshot {
            views: vec![ViewSnapshot {
                text_metrics: None,
                statusline: None,
                options: None,
                key_scopes: vec!["global".into(), "text".into()],
                view_id: 1,
                buffer_id: 2,
                kind: "text".into(),
                title: "note".into(),
                header: String::new(),
                tabs: Vec::new(),
                loading: false,
                column_alignment: Default::default(),
                version: Some(1),
                mode: "normal".into(),
                content: ViewContent::Text {
                    lines: vec![VisibleTextLine {
                        line: 0,
                        first_char: 0,
                        first_column: 0,
                        end_of_line: true,
                        highlights: vec![],
                        text: text.into(),
                    }],
                },
                selection: ViewSelection::Text {
                    ranges: vec![TextRange {
                        anchor: cursor,
                        head: cursor,
                    }],
                    primary: 0,
                },
                scroll: 0,
                horizontal_scroll: 0,
            }],
            ..SessionSnapshot::default()
        }
    }
    let mut state = snapshot("old", 2);
    let next = snapshot("updated", 6);
    SessionDelta::between(&state, &next).apply(&mut state);
    let ViewContent::Text { lines } = &state.views[0].content else {
        panic!("expected document text")
    };
    assert_eq!(lines[0].text, "updated");
    assert_eq!(
        state.views[0].selection,
        ViewSelection::Text {
            ranges: vec![TextRange { anchor: 6, head: 6 }],
            primary: 0
        }
    );
}
