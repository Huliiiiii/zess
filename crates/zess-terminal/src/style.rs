use std::io::Write;

use anyhow::Result;
use crossterm::{
    queue,
    style::{Attribute, Color, SetAttribute, SetBackgroundColor, SetForegroundColor},
};
use zess_ui::{StyleAttribute, StyleColor, TextStyle};

#[cfg(test)]
mod tests;

pub(super) fn apply(stdout: &mut impl Write, style: &TextStyle) -> Result<()> {
    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(color(style.foreground.unwrap_or_default())),
        SetBackgroundColor(color(style.background.unwrap_or_default()))
    )?;
    for attribute in &style.attributes {
        let attribute = match attribute {
            StyleAttribute::Bold => Attribute::Bold,
            StyleAttribute::Dim => Attribute::Dim,
            StyleAttribute::Italic => Attribute::Italic,
            StyleAttribute::Underline => Attribute::Underlined,
            StyleAttribute::Blink => Attribute::SlowBlink,
            StyleAttribute::Reverse => Attribute::Reverse,
            StyleAttribute::Strike => Attribute::CrossedOut,
        };
        queue!(stdout, SetAttribute(attribute))?;
    }
    Ok(())
}

pub(super) fn resolve(role: &str) -> TextStyle {
    if role == "diff.selection" {
        return TextStyle {
            attributes: vec![StyleAttribute::Underline],
            ..TextStyle::default()
        };
    }
    let popup_background = Some(StyleColor::Rgb {
        red: 37,
        green: 40,
        blue: 48,
    });
    let (foreground, background, bold, italic) = match role {
        "ui.heading" => (Some(15), None, true, false),
        "status.normal" => (Some(0), Some(StyleColor::Indexed(14)), true, false),
        "status.insert" => (Some(0), Some(StyleColor::Indexed(10)), true, false),
        "status.select" => (Some(0), Some(StyleColor::Indexed(13)), true, false),
        "status.inactive" => (Some(7), popup_background, false, false),
        "linenr.current" => (Some(15), None, false, false),
        "selection.added" => (Some(10), Some(StyleColor::Indexed(8)), false, false),
        "selection.modified" => (Some(11), Some(StyleColor::Indexed(8)), false, false),
        "selection.deleted" => (Some(9), Some(StyleColor::Indexed(8)), false, false),
        "selection.match" => (Some(11), Some(StyleColor::Indexed(8)), true, false),
        "popup.secondary" => (Some(7), popup_background, false, false),
        "popup.match" => (Some(11), popup_background, true, false),
        "popup" => (Some(15), popup_background, false, false),
        _ => match role.split('.').next().unwrap_or(role) {
            "selection" => (Some(15), Some(StyleColor::Indexed(8)), false, false),
            "primary" => (Some(0), Some(StyleColor::Indexed(11)), true, false),
            "cursor" => (Some(0), Some(StyleColor::Indexed(15)), true, false),
            "comment" => (Some(8), None, false, true),
            "string" | "added" => (Some(10), None, false, false),
            "keyword" => (Some(14), None, true, false),
            "type" | "modified" => (Some(11), None, false, false),
            "function" | "directory" => (Some(12), None, true, false),
            "constant" | "symbol" => (Some(13), None, false, false),
            "operator" => (Some(14), None, false, false),
            "deleted" => (Some(9), None, false, false),
            "warn" | "warning" | "pending" => (Some(11), None, true, false),
            "linenr" | "window" => (Some(8), None, false, false),
            "status" => (
                Some(15),
                Some(StyleColor::Rgb {
                    red: 62,
                    green: 66,
                    blue: 78,
                }),
                false,
                false,
            ),
            "error" => (Some(9), None, true, false),
            _ => (None, None, false, false),
        },
    };
    let mut attributes = Vec::new();
    if bold {
        attributes.push(StyleAttribute::Bold);
    }
    if italic {
        attributes.push(StyleAttribute::Italic);
    }
    TextStyle {
        foreground: foreground.map(StyleColor::Indexed),
        background,
        attributes,
        ..TextStyle::default()
    }
}

fn color(color: StyleColor) -> Color {
    match color {
        StyleColor::Default => Color::Reset,
        StyleColor::Indexed(index) => Color::AnsiValue(index),
        StyleColor::Rgb { red, green, blue } => Color::Rgb {
            r: red,
            g: green,
            b: blue,
        },
    }
}
