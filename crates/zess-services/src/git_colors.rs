use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use zess_ui::{StyleAttribute, StyleColor, TextStyle};

use crate::{jobs::Cancellation, workspace::Workspace};

#[cfg(test)]
mod tests;

pub(crate) struct GitColors {
    added: TextStyle,
    deleted: TextStyle,
}

impl Default for GitColors {
    fn default() -> Self {
        Self {
            added: TextStyle {
                foreground: Some(StyleColor::Indexed(2)),
                ..TextStyle::default()
            },
            deleted: TextStyle {
                foreground: Some(StyleColor::Indexed(1)),
                ..TextStyle::default()
            },
        }
    }
}

impl GitColors {
    pub(crate) fn palette(&self) -> HashMap<String, TextStyle> {
        let mut styles = HashMap::new();
        for (role, style) in [("git.added", &self.added), ("git.deleted", &self.deleted)] {
            styles.insert(role.into(), style.clone());
            styles.insert(
                format!("selection.{role}"),
                TextStyle {
                    background: Some(StyleColor::Indexed(8)),
                    ..style.clone()
                },
            );
        }
        styles
    }
}

impl Workspace {
    pub(crate) fn refresh_git_colors(&self, cancellation: &Cancellation) -> Result<()> {
        let configured = zess_git::Repository::open(&self.root)?.diff_colors(cancellation)?;
        let mut colors = GitColors::default();
        if let Some(added) = configured.added {
            colors.added = parse_style(&added)?;
        }
        if let Some(deleted) = configured.deleted {
            colors.deleted = parse_style(&deleted)?;
        }
        cancellation.check()?;
        *self.git_colors.lock().unwrap() = colors;
        Ok(())
    }
}

fn parse_style(bytes: &[u8]) -> Result<TextStyle> {
    let mut style = TextStyle::default();
    if bytes.is_empty() {
        return Ok(style);
    }
    let parameters = std::str::from_utf8(bytes)?
        .strip_prefix("\x1b[")
        .and_then(|text| text.strip_suffix('m'))
        .context("invalid Git color sequence")?;
    let mut parameters = parameters.split(';').map(|parameter| {
        if parameter.is_empty() {
            Ok(0)
        } else {
            parameter
                .parse::<u8>()
                .context("invalid Git color parameter")
        }
    });
    while let Some(parameter) = parameters.next() {
        let parameter = parameter?;
        let attribute = match parameter {
            0 => {
                style = TextStyle::reset();
                continue;
            }
            1 => Some(StyleAttribute::Bold),
            2 => Some(StyleAttribute::Dim),
            3 => Some(StyleAttribute::Italic),
            4 => Some(StyleAttribute::Underline),
            5 => Some(StyleAttribute::Blink),
            7 => Some(StyleAttribute::Reverse),
            9 => Some(StyleAttribute::Strike),
            22..=25 | 27 | 29 => {
                let attributes: &[StyleAttribute] = match parameter {
                    22 => &[StyleAttribute::Bold, StyleAttribute::Dim],
                    23 => &[StyleAttribute::Italic],
                    24 => &[StyleAttribute::Underline],
                    25 => &[StyleAttribute::Blink],
                    27 => &[StyleAttribute::Reverse],
                    29 => &[StyleAttribute::Strike],
                    _ => unreachable!(),
                };
                style
                    .attributes
                    .retain(|attribute| !attributes.contains(attribute));
                for attribute in attributes {
                    if !style.removed_attributes.contains(attribute) {
                        style.removed_attributes.push(attribute.clone());
                    }
                }
                continue;
            }
            30..=37 => {
                style.foreground = Some(StyleColor::Indexed(parameter - 30));
                continue;
            }
            40..=47 => {
                style.background = Some(StyleColor::Indexed(parameter - 40));
                continue;
            }
            90..=97 => {
                style.foreground = Some(StyleColor::Indexed(parameter - 90 + 8));
                continue;
            }
            100..=107 => {
                style.background = Some(StyleColor::Indexed(parameter - 100 + 8));
                continue;
            }
            39 => {
                style.foreground = Some(StyleColor::Default);
                continue;
            }
            49 => {
                style.background = Some(StyleColor::Default);
                continue;
            }
            38 | 48 => {
                let mut next = || parameters.next().context("incomplete Git color")?;
                let color = match next()? {
                    5 => StyleColor::Indexed(next()?),
                    2 => StyleColor::Rgb {
                        red: next()?,
                        green: next()?,
                        blue: next()?,
                    },
                    _ => bail!("unsupported Git color mode"),
                };
                if parameter == 38 {
                    style.foreground = Some(color);
                } else {
                    style.background = Some(color);
                }
                continue;
            }
            _ => bail!("unsupported Git color parameter {parameter}"),
        };
        if let Some(attribute) = attribute {
            style
                .removed_attributes
                .retain(|removed| *removed != attribute);
            if !style.attributes.contains(&attribute) {
                style.attributes.push(attribute);
            }
        }
    }
    Ok(style)
}
