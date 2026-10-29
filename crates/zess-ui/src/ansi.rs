use std::collections::HashMap;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::{FrameRow, Span, StyleAttribute, StyleColor, TextStyle};

#[cfg(test)]
mod tests;

/// Terminal text decoded into printable rows and styles, without terminal actions.
#[derive(Default)]
pub struct AnsiText {
    pub rows: Vec<FrameRow>,
    pub styles: HashMap<String, TextStyle>,
    pub diff_metadata: Vec<Vec<DiffLineMetadata>>,
    pub invalid_diff_metadata: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
    File,
    Hunk,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffLineMetadata {
    pub kind: DiffLineKind,
    pub new_line: Option<usize>,
    pub old_line: Option<usize>,
    pub path: String,
}

impl AnsiText {
    pub fn parse(text: &str) -> Self {
        let mut decoder = Decoder {
            output: Self::default(),
            row: FrameRow::default(),
            style: TextStyle::reset(),
            style_key: String::new(),
            column: 0,
            diff_metadata: Vec::new(),
        };
        vte::Parser::new().advance(&mut decoder, text.as_bytes());
        if !decoder.row.spans.is_empty() || !decoder.diff_metadata.is_empty() {
            decoder.finish_row();
        }
        decoder.output
    }
}

struct Decoder {
    output: AnsiText,
    row: FrameRow,
    style: TextStyle,
    style_key: String,
    column: usize,
    diff_metadata: Vec<DiffLineMetadata>,
}

impl Decoder {
    fn finish_row(&mut self) {
        self.output.rows.push(std::mem::take(&mut self.row));
        self.output
            .diff_metadata
            .push(std::mem::take(&mut self.diff_metadata));
        self.column = 0;
    }

    fn attribute(&mut self, attribute: StyleAttribute, enabled: bool) {
        self.style.attributes.retain(|value| value != &attribute);
        self.style
            .removed_attributes
            .retain(|value| value != &attribute);
        if enabled {
            self.style.attributes.push(attribute);
        } else {
            self.style.removed_attributes.push(attribute);
        }
    }
}

impl Perform for Decoder {
    fn print(&mut self, ch: char) {
        let ch = if ch.is_control() { '\u{fffd}' } else { ch };
        // The complete style is the key, so separately decoded files and buffers
        // can share a Frame without reusing an index for a different style.
        if self.style_key.is_empty() {
            self.style_key = format!("ansi.{}", serde_json::to_string(&self.style).unwrap());
            self.output
                .styles
                .entry(self.style_key.clone())
                .or_insert_with(|| self.style.clone());
        }
        if let Some(span) = self
            .row
            .spans
            .last_mut()
            .filter(|span| span.style == self.style_key)
        {
            span.text.push(ch);
        } else {
            self.row.spans.push(Span {
                base_style: None,
                text: ch.into(),
                style: self.style_key.clone(),
            });
        }
        self.column += ch.width().unwrap_or(0);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                self.finish_row();
            }
            b'\t' => {
                for _ in 0..8 - self.column % 8 {
                    self.print(' ');
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _: bool) {
        if params.first() != Some(&b"1717".as_slice()) {
            return;
        }
        if params == [b"1717".as_slice(), b"1".as_slice()] {
            return;
        }
        if params.get(1) == Some(&b"1".as_slice())
            && params
                .get(2)
                .is_some_and(|kind| !matches!(*kind, b"c" | b"a" | b"d" | b"f" | b"h"))
        {
            return;
        }
        let parse = || {
            if params.len() < 6 || params[1] != b"1" {
                return None;
            }
            let kind = match params[2] {
                b"c" => DiffLineKind::Context,
                b"a" => DiffLineKind::Addition,
                b"d" => DiffLineKind::Deletion,
                b"f" => DiffLineKind::File,
                b"h" => DiffLineKind::Hunk,
                _ => return None,
            };
            let number = |bytes: &[u8]| {
                if bytes.is_empty() {
                    Some(None)
                } else if bytes.iter().all(u8::is_ascii_digit) {
                    Some(Some(std::str::from_utf8(bytes).ok()?.parse().ok()?))
                } else {
                    None
                }
            };
            let new_line = number(params[3])?;
            let old_line = number(params[4])?;
            let path = String::from_utf8(params[5..].join(&b';')).ok()?;
            if path.is_empty()
                || path.chars().any(char::is_control)
                || match kind {
                    DiffLineKind::File => new_line.is_some() || old_line.is_some(),
                    DiffLineKind::Deletion => {
                        new_line.is_none() || old_line.is_none_or(|line| line == 0)
                    }
                    DiffLineKind::Context | DiffLineKind::Addition => {
                        new_line.is_none_or(|line| line == 0) || old_line.is_some()
                    }
                    DiffLineKind::Hunk => new_line.is_none() || old_line.is_some(),
                }
            {
                return None;
            }
            Some(DiffLineMetadata {
                kind,
                new_line,
                old_line,
                path,
            })
        };
        if let Some(metadata) = parse() {
            self.diff_metadata.push(metadata);
        } else {
            self.output.invalid_diff_metadata = true;
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() || action != 'm' {
            return;
        }
        self.style_key.clear();
        let mut params = params.into_iter();
        while let Some(param) = params.next() {
            match param[0] {
                0 => self.style = TextStyle::reset(),
                1 => self.attribute(StyleAttribute::Bold, true),
                2 => self.attribute(StyleAttribute::Dim, true),
                3 => self.attribute(StyleAttribute::Italic, true),
                4 => self.attribute(StyleAttribute::Underline, param.get(1) != Some(&0)),
                5 | 6 => self.attribute(StyleAttribute::Blink, true),
                7 => self.attribute(StyleAttribute::Reverse, true),
                9 => self.attribute(StyleAttribute::Strike, true),
                22 => {
                    self.attribute(StyleAttribute::Bold, false);
                    self.attribute(StyleAttribute::Dim, false);
                }
                23 => self.attribute(StyleAttribute::Italic, false),
                24 => self.attribute(StyleAttribute::Underline, false),
                25 => self.attribute(StyleAttribute::Blink, false),
                27 => self.attribute(StyleAttribute::Reverse, false),
                29 => self.attribute(StyleAttribute::Strike, false),
                30..=37 => self.style.foreground = Some(StyleColor::Indexed((param[0] - 30) as u8)),
                40..=47 => self.style.background = Some(StyleColor::Indexed((param[0] - 40) as u8)),
                90..=97 => {
                    self.style.foreground = Some(StyleColor::Indexed((param[0] - 90 + 8) as u8))
                }
                100..=107 => {
                    self.style.background = Some(StyleColor::Indexed((param[0] - 100 + 8) as u8))
                }
                39 => self.style.foreground = Some(StyleColor::Default),
                49 => self.style.background = Some(StyleColor::Default),
                38 | 48 => {
                    let color = if param.len() > 1 {
                        match &param[1..] {
                            [5, index] => u8::try_from(*index).ok().map(StyleColor::Indexed),
                            [2, red, green, blue] | [2, _, red, green, blue] => {
                                rgb(*red, *green, *blue)
                            }
                            _ => None,
                        }
                    } else {
                        match params.next().map(|param| param[0]) {
                            Some(5) => params
                                .next()
                                .and_then(|param| u8::try_from(param[0]).ok())
                                .map(StyleColor::Indexed),
                            Some(2) => match (params.next(), params.next(), params.next()) {
                                (Some(red), Some(green), Some(blue)) => {
                                    rgb(red[0], green[0], blue[0])
                                }
                                _ => None,
                            },
                            _ => None,
                        }
                    };
                    if let Some(color) = color {
                        if param[0] == 38 {
                            self.style.foreground = Some(color);
                        } else {
                            self.style.background = Some(color);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn rgb(red: u16, green: u16, blue: u16) -> Option<StyleColor> {
    Some(StyleColor::Rgb {
        red: red.try_into().ok()?,
        green: green.try_into().ok()?,
        blue: blue.try_into().ok()?,
    })
}
