use super::*;

#[test]
fn unspecified_colors_inherit_and_explicit_defaults_reset() {
    let base = TextStyle {
        foreground: Some(StyleColor::Indexed(15)),
        background: Some(StyleColor::Indexed(8)),
        ..TextStyle::default()
    };
    let inherited = base.clone().patch(&TextStyle {
        foreground: Some(StyleColor::Indexed(2)),
        ..TextStyle::default()
    });
    assert_eq!(inherited.foreground, Some(StyleColor::Indexed(2)));
    assert_eq!(inherited.background, Some(StyleColor::Indexed(8)));
    let reset = base.patch(&TextStyle {
        background: Some(StyleColor::Default),
        ..TextStyle::default()
    });
    assert_eq!(reset.foreground, Some(StyleColor::Indexed(15)));
    assert_eq!(reset.background, Some(StyleColor::Default));
}

#[test]
fn later_attribute_changes_override_earlier_changes() {
    let base = TextStyle {
        attributes: vec![StyleAttribute::Bold, StyleAttribute::Italic],
        ..TextStyle::default()
    };
    let remove_bold = TextStyle {
        removed_attributes: vec![StyleAttribute::Bold],
        ..TextStyle::default()
    };
    let add_bold = TextStyle {
        attributes: vec![StyleAttribute::Bold],
        ..TextStyle::default()
    };
    let removed = base.clone().patch(&remove_bold);
    assert_eq!(removed.attributes, [StyleAttribute::Italic]);
    assert_eq!(removed.removed_attributes, [StyleAttribute::Bold]);
    let restored = removed.patch(&add_bold);
    assert_eq!(
        restored.attributes,
        [StyleAttribute::Italic, StyleAttribute::Bold]
    );
    assert!(restored.removed_attributes.is_empty());
}

#[test]
fn older_frames_preserve_explicit_colors_and_omit_the_base_style() {
    let style: TextStyle = serde_json::from_value(serde_json::json!({
        "foreground": {"indexed":2}, "background":"default", "attributes":["bold"]
    }))
    .unwrap();
    assert_eq!(style.foreground, Some(StyleColor::Indexed(2)));
    assert_eq!(style.background, Some(StyleColor::Default));
    assert_eq!(style.attributes, [StyleAttribute::Bold]);
    assert!(style.removed_attributes.is_empty());
    let span: crate::Span =
        serde_json::from_value(serde_json::json!({"text":"+12", "style":"git.added"})).unwrap();
    assert_eq!(span.base_style, None);
}
