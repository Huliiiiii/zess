use super::*;

#[test]
fn selected_rows_keep_semantic_foregrounds_and_attributes() {
    use std::collections::HashMap;
    use zess_ui::{FrameRow, Span};

    crossterm::style::force_color_output(true);
    let styles = HashMap::from([
        (
            "custom".into(),
            TextStyle {
                foreground: Some(StyleColor::Indexed(6)),
                background: Some(StyleColor::Indexed(52)),
                attributes: vec![StyleAttribute::Underline],
                removed_attributes: vec![StyleAttribute::Italic],
            },
        ),
        (
            "base".into(),
            TextStyle {
                foreground: Some(StyleColor::Indexed(9)),
                attributes: vec![StyleAttribute::Bold, StyleAttribute::Italic],
                ..TextStyle::default()
            },
        ),
    ]);
    let row = FrameRow {
        spans: vec![
            Span {
                text: "dir".into(),
                style: "selection.directory".into(),
                base_style: None,
            },
            Span {
                text: "cell".into(),
                style: "selection.custom".into(),
                base_style: Some("base".into()),
            },
            Span {
                text: "plain".into(),
                style: "selection.".into(),
                base_style: Some("base".into()),
            },
        ],
    };
    let mut output = Vec::new();
    super::super::render_row(&mut output, &row, 12, &styles).unwrap();
    assert_eq!(
        output,
        concat!(
            "\x1b[0m\x1b[38;5;12m\x1b[48;5;8m\x1b[1mdir",
            "\x1b[0m\x1b[38;5;6m\x1b[48;5;8m\x1b[1m\x1b[4mcell",
            "\x1b[0m\x1b[38;5;9m\x1b[48;5;8m\x1b[1m\x1b[3mplain\x1b[0m\x1b[0m"
        )
        .as_bytes()
    );
    let custom = resolve_in("custom", &styles);
    assert_eq!(custom.background, Some(StyleColor::Indexed(52)));
    assert_eq!(custom.attributes, [StyleAttribute::Underline]);
}

#[test]
fn explicit_selected_styles_override_the_composed_default() {
    let styles = std::collections::HashMap::from([
        (
            "git.added".into(),
            TextStyle {
                foreground: Some(StyleColor::Indexed(2)),
                ..TextStyle::default()
            },
        ),
        (
            "selection.git.added".into(),
            TextStyle {
                foreground: Some(StyleColor::Indexed(3)),
                background: Some(StyleColor::Indexed(4)),
                attributes: vec![StyleAttribute::Italic],
                ..TextStyle::default()
            },
        ),
    ]);
    assert_eq!(
        resolve_in("selection.git.added", &styles),
        styles["selection.git.added"]
    );
}

#[test]
fn headings_use_readable_upright_text() {
    let style = resolve("ui.heading");
    assert_eq!(style.foreground, Some(StyleColor::Indexed(15)));
    assert_eq!(style.attributes, [StyleAttribute::Bold]);
}

#[test]
fn configured_attributes_do_not_leak_into_the_next_span() {
    crossterm::style::force_color_output(true);
    let mut output = Vec::new();
    apply(
        &mut output,
        &TextStyle {
            foreground: Some(StyleColor::Rgb {
                red: 170,
                green: 187,
                blue: 204,
            }),
            background: Some(StyleColor::Indexed(52)),
            attributes: vec![StyleAttribute::Bold, StyleAttribute::Reverse],
            ..TextStyle::default()
        },
    )
    .unwrap();
    apply(&mut output, &resolve("")).unwrap();
    assert_eq!(
        output,
        b"\x1b[0m\x1b[38;2;170;187;204m\x1b[48;5;52m\x1b[1m\x1b[7m\x1b[0m\x1b[39m\x1b[49m"
    );
}

#[test]
fn spans_inherit_the_status_background_until_their_role_overrides_it() {
    use std::collections::HashMap;
    use zess_ui::{FrameRow, Span};

    crossterm::style::force_color_output(true);
    for (base, background) in [("status", "62;66;78"), ("status.inactive", "37;40;48")] {
        let styles = HashMap::from([
            (
                "git.added".into(),
                TextStyle {
                    foreground: Some(StyleColor::Indexed(2)),
                    ..TextStyle::default()
                },
            ),
            (
                "git.deleted".into(),
                TextStyle {
                    foreground: Some(StyleColor::Indexed(1)),
                    background: Some(StyleColor::Default),
                    ..TextStyle::default()
                },
            ),
        ]);
        let row = FrameRow {
            spans: vec![
                Span {
                    text: "+12".into(),
                    style: "git.added".into(),
                    base_style: Some(base.into()),
                },
                Span {
                    text: "-3".into(),
                    style: "git.deleted".into(),
                    base_style: Some(base.into()),
                },
                Span {
                    text: " NOR ".into(),
                    style: "status.normal".into(),
                    base_style: Some(base.into()),
                },
                Span {
                    text: "next".into(),
                    style: "".into(),
                    base_style: None,
                },
            ],
        };
        let mut output = Vec::new();
        super::super::render_row(&mut output, &row, 30, &styles).unwrap();
        assert_eq!(
            output,
            format!(
                concat!(
                    "\x1b[0m\x1b[38;5;2m\x1b[48;2;{background}m+12",
                    "\x1b[0m\x1b[38;5;1m\x1b[49m-3",
                    "\x1b[0m\x1b[38;5;0m\x1b[48;5;14m\x1b[1m NOR ",
                    "\x1b[0m\x1b[39m\x1b[49mnext\x1b[0m\x1b[0m",
                ),
                background = background
            )
            .as_bytes()
        );
    }
}

#[test]
fn popup_secondary_text_and_selected_matches_keep_their_backgrounds() {
    let popup = resolve("popup");
    let secondary = resolve("popup.secondary");
    let selection = resolve("selection");
    assert_eq!(selection.foreground, Some(StyleColor::Indexed(15)));
    let selected_match = resolve("selection.match");
    assert_eq!(secondary.background, popup.background);
    assert_ne!(secondary.foreground, popup.foreground);
    assert_eq!(selected_match.background, selection.background);
    assert_ne!(selected_match.foreground, selection.foreground);
}

#[test]
fn selected_rendered_diff_lines_keep_the_renderers_colors() {
    use std::collections::HashMap;
    use zess_ui::{FrameRow, Span};

    crossterm::style::force_color_output(true);
    let styles = HashMap::from([(
        "ansi.diff".into(),
        TextStyle {
            foreground: Some(StyleColor::Indexed(2)),
            background: Some(StyleColor::Indexed(52)),
            attributes: vec![StyleAttribute::Bold],
            ..TextStyle::default()
        },
    )]);
    let row = FrameRow {
        spans: vec![Span {
            text: "change".into(),
            style: "diff.selection".into(),
            base_style: Some("ansi.diff".into()),
        }],
    };
    let mut output = Vec::new();
    super::super::render_row(&mut output, &row, 6, &styles).unwrap();
    assert_eq!(
        output,
        b"\x1b[0m\x1b[38;5;2m\x1b[48;5;52m\x1b[1m\x1b[4mchange\x1b[0m\x1b[0m"
    );
}
