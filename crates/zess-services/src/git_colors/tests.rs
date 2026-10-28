use super::*;

#[test]
fn configured_colors_preserve_terminal_indices_and_attributes() {
    let style = parse_style(b"\x1b[1;2;3;4;5;7;9;38;2;170;187;204;48;5;52m").unwrap();
    assert_eq!(
        style.foreground,
        Some(StyleColor::Rgb {
            red: 170,
            green: 187,
            blue: 204
        })
    );
    assert_eq!(style.background, Some(StyleColor::Indexed(52)));
    assert_eq!(
        style.attributes,
        vec![
            StyleAttribute::Bold,
            StyleAttribute::Dim,
            StyleAttribute::Italic,
            StyleAttribute::Underline,
            StyleAttribute::Blink,
            StyleAttribute::Reverse,
            StyleAttribute::Strike
        ]
    );
    assert_eq!(
        parse_style(b"\x1b[32m").unwrap().foreground,
        Some(StyleColor::Indexed(2))
    );
    assert_eq!(
        parse_style(b"\x1b[92m").unwrap().foreground,
        Some(StyleColor::Indexed(10))
    );
}

#[test]
fn explicit_attribute_resets_remove_the_corresponding_effects() {
    assert_eq!(
        parse_style(b"\x1b[1;2;3;4;5;7;9;22;23;24;25;27;29m").unwrap(),
        TextStyle {
            removed_attributes: vec![
                StyleAttribute::Bold,
                StyleAttribute::Dim,
                StyleAttribute::Italic,
                StyleAttribute::Underline,
                StyleAttribute::Blink,
                StyleAttribute::Reverse,
                StyleAttribute::Strike,
            ],
            ..TextStyle::default()
        }
    );
    assert_eq!(parse_style(b"\x1b[m").unwrap(), TextStyle::reset());
    assert_eq!(parse_style(b"").unwrap(), TextStyle::default());
}

#[test]
fn non_style_terminal_sequences_are_rejected() {
    assert!(parse_style(b"\x1b[31m\x1b[2J").is_err());
}

#[test]
fn omitted_colors_inherit_but_explicit_color_resets_override_the_base() {
    let green = parse_style(b"\x1b[32m").unwrap();
    assert_eq!(green.background, None);
    let green_default = parse_style(b"\x1b[32;49m").unwrap();
    assert_eq!(green_default.foreground, Some(StyleColor::Indexed(2)));
    assert_eq!(green_default.background, Some(StyleColor::Default));
    let reset = parse_style(b"\x1b[39;49m").unwrap();
    assert_eq!(reset.foreground, Some(StyleColor::Default));
    assert_eq!(reset.background, Some(StyleColor::Default));
    let reset = parse_style(b"\x1b[0;32m").unwrap();
    assert_eq!(reset.foreground, Some(StyleColor::Indexed(2)));
    assert_eq!(reset.background, Some(StyleColor::Default));
    assert!(reset.attributes.is_empty());
    assert_eq!(reset.removed_attributes.len(), 7);
}

#[test]
fn attribute_switches_can_remove_and_restore_inherited_effects() {
    let base = TextStyle {
        attributes: vec![StyleAttribute::Bold, StyleAttribute::Italic],
        ..TextStyle::default()
    };
    let removed = base.clone().patch(&parse_style(b"\x1b[22m").unwrap());
    assert_eq!(removed.attributes, [StyleAttribute::Italic]);
    let restored = base.patch(&parse_style(b"\x1b[22;1m").unwrap());
    assert_eq!(
        restored.attributes,
        [StyleAttribute::Bold, StyleAttribute::Italic]
    );
    assert!(!restored.removed_attributes.contains(&StyleAttribute::Bold));
}
