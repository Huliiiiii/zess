use super::*;

#[test]
fn styled_regions_and_padding_inherit_the_current_status_background() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let id = session.focus;
    for active in [true, false] {
        let spans = session.statusline(
            id,
            "default",
            json!({
                "mode": "normal", "active": active,
                "plugin_status": {"git": {"additions": 123456, "deletions": 4}}
            }),
            24,
            active,
        );
        let base = if active { "status" } else { "status.inactive" };
        assert!(
            spans
                .iter()
                .all(|span| span.base_style.as_deref() == Some(base))
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "+123456" && span.style == "git.added")
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "-4" && span.style == "git.deleted")
        );
        assert_eq!(
            spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                .sum::<usize>(),
            24
        );
    }
}

#[test]
fn cached_status_spans_follow_focus_changes_when_the_formatter_is_unavailable() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let id = session.focus;
    session.statusline(
        id,
        "default",
        json!({"title":"cached", "active":true}),
        24,
        true,
    );
    let spans = session.statusline(id, "missing", json!({}), 12, false);
    assert_eq!(session.message, "statusline: unknown statusline: missing");
    assert!(
        spans
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status.inactive"))
    );
    assert!(
        spans
            .iter()
            .any(|span| span.text == "cached" && span.style.is_empty())
    );
    session.scripts = None;
    let spans = session.statusline(id, "default", json!({}), 8, false);
    assert!(
        spans
            .iter()
            .all(|span| span.base_style.as_deref() == Some("status.inactive"))
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum::<usize>(),
        8
    );
}

#[test]
fn explicit_status_roles_survive_focus_changes_in_cached_regions() {
    let fixture = Fixture::new();
    let mut session = fixture.session(&[]);
    let id = session.focus;
    session.views.get_mut(&id).unwrap().location.statusline = vec![Span {
        text: "explicit".into(),
        style: "status".into(),
        base_style: Some("status".into()),
    }];
    session.scripts = None;
    let spans = session.statusline(id, "default", json!({}), 8, false);
    assert_eq!(spans[0].text, "explicit");
    assert_eq!(spans[0].style, "status");
    assert_eq!(spans[0].base_style.as_deref(), Some("status.inactive"));
}
