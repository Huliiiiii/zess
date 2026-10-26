//! SRFI-119 Wisp reader. Explicit Scheme datums are read before indentation,
//! so parentheses, strings, and comments never participate in layout.

use anyhow::{Result, bail, ensure};

#[cfg(test)]
mod tests;

enum Datum {
    Atom(Text),
    List(Vec<Datum>, usize),
    Prefix(Text),
    Prefixed(Text, Box<Datum>),
    Colon(Text),
    Dot(Text),
}

struct Line {
    indent: usize,
    number: usize,
    origin: usize,
    code: Vec<Datum>,
}

/// Read Wisp source and emit the equivalent Scheme source.
pub fn read(source: &str) -> Result<String> {
    Ok(read_with_source_map(source)?.scheme)
}

/// Generated Scheme and its byte offsets in the original Wisp source.
pub struct Translation {
    pub scheme: String,
    pub source_map: SourceMap,
}

pub struct SourceMap {
    offsets: Vec<usize>,
}

impl SourceMap {
    /// Map a generated UTF-8 byte offset to an original UTF-8 byte offset.
    pub fn original_offset(&self, generated: usize) -> Option<usize> {
        self.offsets.get(generated).copied()
    }
}

#[derive(Clone)]
struct Text {
    value: String,
    offsets: Vec<usize>,
}

impl Text {
    fn generated(value: &str, origin: usize) -> Self {
        Self {
            value: value.into(),
            offsets: vec![origin; value.len()],
        }
    }

    fn append(&mut self, other: Self) {
        self.value.push_str(&other.value);
        self.offsets.extend(other.offsets);
    }

    fn join(values: impl IntoIterator<Item = Self>, separator: &str) -> Self {
        let mut output = Self::generated("", 0);
        for value in values {
            if !output.value.is_empty() {
                output.append(Self::generated(
                    separator,
                    value.offsets.first().copied().unwrap_or(0),
                ));
            }
            output.append(value);
        }
        output
    }

    fn list(values: impl IntoIterator<Item = Self>, origin: usize) -> Self {
        let body = Self::join(values, " ");
        let start = body.offsets.first().copied().unwrap_or(origin);
        let end = body
            .value
            .char_indices()
            .last()
            .map_or(origin, |(offset, _)| body.offsets[offset]);
        let mut output = Self::generated("(", start);
        output.append(body);
        output.append(Self::generated(")", end));
        output
    }
}

/// Read Wisp while retaining exact source locations through layout expansion.
pub fn read_with_source_map(source: &str) -> Result<Translation> {
    let mut reader = Reader {
        chars: source.chars().collect(),
        byte_offsets: source
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([source.len()])
            .collect(),
        offset: 0,
        line: 1,
    };

    let mut lines = Vec::new();
    let mut empty_lines = 0;
    let mut block_ended = false;

    while reader.peek().is_some() {
        let number = reader.line;
        let mut indent = 0;

        if reader.peek() == Some('_') {
            while reader.peek() == Some('_') {
                reader.next();
                indent += 1;
            }

            ensure!(
                matches!(reader.peek(), Some(' ' | '\n' | '\r') | None),
                "Wisp line {number}: indentation underscores need following whitespace"
            );
        }

        while reader.peek() == Some(' ') {
            reader.next();
            indent += 1;
        }

        ensure!(
            reader.peek() != Some('\t'),
            "Wisp line {number}: tabs cannot express indentation"
        );

        let origin = reader.byte_offsets[reader.offset];
        let mut code = Vec::new();
        let mut comment = false;

        loop {
            reader.skip_horizontal();

            match reader.peek() {
                None | Some('\n') => break,
                Some(';') => {
                    comment = true;
                    reader.line_comment();
                }
                Some('#') if reader.starts("#|") => {
                    comment = true;
                    reader.block_comment()?;
                }
                Some('#') if reader.starts("#;") => {
                    comment = true;
                    reader.offset += 2;
                    reader.skip_space_and_comments()?;
                    reader.datum()?;
                }
                Some('#') if reader.starts("#!") && number == 1 => {
                    comment = true;
                    reader.line_comment();
                }
                _ => {
                    let datum = reader.datum()?;
                    let followed_by_space = reader.peek().is_none_or(char::is_whitespace);

                    code.push(match datum.value.as_str() {
                        ":" if followed_by_space => Datum::Colon(datum),
                        "." if followed_by_space => Datum::Dot(datum),
                        "'" | "`" | "," | ",@" | "#'" | "#`" | "#," | "#,@" | "#@" => {
                            Datum::Prefix(datum)
                        }
                        "\\:" => Datum::Atom(Text::generated(":", datum.offsets[1])),
                        _ if datum.value.starts_with("\\_") => Datum::Atom(Text {
                            value: datum.value[1..].into(),
                            offsets: datum.offsets[1..].into(),
                        }),
                        _ => Datum::Atom(datum),
                    });
                }
            }
        }

        if !code.is_empty() {
            ensure!(
                !block_ended || indent == 0,
                "Wisp line {number}: a block after two empty lines must start at zero indentation"
            );
            lines.push(Line {
                indent,
                number,
                origin,
                code,
            });
            empty_lines = 0;
            block_ended = false;
        } else if comment {
            empty_lines = 0;
        } else {
            empty_lines += 1;
            block_ended |= empty_lines >= 2;
        }

        if reader.peek() == Some('\n') {
            reader.next();
        }
    }

    ensure!(
        lines.first().is_none_or(|line| line.indent == 0),
        "Wisp source must start at zero indentation"
    );

    let mut lines = lines.into_iter().peekable();
    let forms = group(&mut lines, 0)?;

    let mut output = Text::join(
        forms.into_iter().map(render).collect::<Result<Vec<_>>>()?,
        "\n",
    );
    output.offsets.push(source.len());
    Ok(Translation {
        scheme: output.value,
        source_map: SourceMap {
            offsets: output.offsets,
        },
    })
}

fn group(
    lines: &mut std::iter::Peekable<std::vec::IntoIter<Line>>,
    indent: usize,
) -> Result<Vec<Datum>> {
    let mut forms = Vec::new();

    while let Some(line) = lines.peek() {
        if line.indent < indent {
            break;
        }

        ensure!(
            line.indent == indent,
            "Wisp line {}: dedentation does not match an enclosing indentation",
            line.number
        );

        let Line {
            mut code, origin, ..
        } = lines.next().unwrap();
        let continuation = matches!(code.first(), Some(Datum::Dot(_)));

        ensure!(
            !matches!(code.last(), Some(Datum::Dot(_))),
            "Wisp: a trailing or lone dot is reserved"
        );

        if continuation {
            code.remove(0);
        }

        if matches!(code.as_slice(), [Datum::Colon(_)]) {
            code.clear();
        }

        let mut code = colons(code.into_iter());

        if let Some(next) = lines.peek()
            && next.indent > indent
        {
            let child_indent = next.indent;

            code.extend(group(lines, child_indent)?);
        }

        if continuation {
            forms.extend(compose_prefixes(code)?);
        } else {
            forms.push(Datum::List(code, origin));
        }
    }

    Ok(forms)
}

fn compose_prefixes(code: Vec<Datum>) -> Result<Vec<Datum>> {
    let mut forms = Vec::new();
    let mut prefixes = Vec::new();

    for datum in code {
        if let Datum::Prefix(prefix) = datum {
            prefixes.push(prefix);
            continue;
        }

        let datum = prefixes.drain(..).rev().fold(datum, |datum, prefix| {
            Datum::Prefixed(prefix, Box::new(datum))
        });

        forms.push(datum);
    }

    ensure!(prefixes.is_empty(), "Wisp quotation has no datum");

    Ok(forms)
}

fn colons(mut code: std::vec::IntoIter<Datum>) -> Vec<Datum> {
    let mut output = Vec::new();

    while let Some(datum) = code.next() {
        if let Datum::Colon(colon) = datum {
            output.push(Datum::List(colons(code), colon.offsets[0]));
            break;
        }

        output.push(datum);
    }

    output
}

fn render(datum: Datum) -> Result<Text> {
    match datum {
        Datum::Atom(atom) => Ok(atom),
        Datum::Dot(dot) => Ok(dot),
        Datum::Colon(_) => unreachable!("colons are resolved before rendering"),
        Datum::Prefix(_) => bail!("Wisp quotation has no datum"),
        Datum::Prefixed(mut prefix, datum) => {
            prefix.append(render(*datum)?);
            Ok(prefix)
        }
        Datum::List(mut values, origin) => {
            if matches!(values.first(), Some(Datum::Prefix(_))) {
                let Datum::Prefix(prefix) = values.remove(0) else {
                    unreachable!()
                };

                let mut prefix = prefix;
                prefix.append(render(Datum::List(values, origin))?);
                return Ok(prefix);
            }

            let mut output = Vec::new();
            let mut values = values.into_iter().peekable();

            while let Some(value) = values.next() {
                if let Datum::Prefix(prefix) = value {
                    let next = values
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Wisp quotation has no datum"))?;

                    let mut prefix = prefix;
                    prefix.append(render(next)?);
                    output.push(prefix);
                } else {
                    if matches!(value, Datum::Dot(_)) {
                        ensure!(
                            !output.is_empty() && values.len() == 1,
                            "Wisp dot must precede the final list element"
                        );
                    }

                    output.push(render(value)?);
                }
            }

            Ok(Text::list(output, origin))
        }
    }
}

struct Reader {
    chars: Vec<char>,
    byte_offsets: Vec<usize>,
    offset: usize,
    line: usize,
}

impl Reader {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.offset).copied()
    }

    fn starts(&self, value: &str) -> bool {
        self.chars[self.offset..]
            .iter()
            .copied()
            .zip(value.chars())
            .all(|(a, b)| a == b)
            && self.chars.len() - self.offset >= value.chars().count()
    }

    fn next(&mut self) -> Option<char> {
        let value = self.peek()?;

        self.offset += 1;

        if value == '\n' {
            self.line += 1;
        }

        Some(value)
    }

    fn skip_horizontal(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.next();
        }
    }

    fn line_comment(&mut self) {
        while !matches!(self.peek(), None | Some('\n')) {
            self.next();
        }
    }

    fn block_comment(&mut self) -> Result<()> {
        self.offset += 2;

        let mut depth = 1;

        while self.peek().is_some() {
            if self.starts("#|") {
                self.offset += 2;
                depth += 1;
            } else if self.starts("|#") {
                self.offset += 2;
                depth -= 1;

                if depth == 0 {
                    return Ok(());
                }
            } else {
                self.next();
            }
        }

        bail!("Wisp line {}: unterminated block comment", self.line)
    }

    fn skip_space_and_comments(&mut self) -> Result<()> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.next();
            }

            if self.peek() == Some(';') {
                self.line_comment();
            } else if self.starts("#|") {
                self.block_comment()?;
            } else if self.starts("#;") {
                self.offset += 2;
                self.skip_space_and_comments()?;
                self.datum()?;
            } else {
                return Ok(());
            }
        }
    }

    fn datum(&mut self) -> Result<Text> {
        let start = self.offset;
        let first = self
            .next()
            .ok_or_else(|| anyhow::anyhow!("Wisp: expected datum"))?;

        match first {
            '(' | '[' | '{' => return self.delimited(first, start),
            '"' | '|' => self.quoted(first)?,
            ')' | ']' | '}' => bail!("Wisp line {}: unexpected closing delimiter", self.line),
            '\'' | '`' | ',' => {
                if first == ',' && self.peek() == Some('@') {
                    self.next();
                }

                if self.peek().is_some_and(|c| !c.is_whitespace() && c != ';') {
                    let mut prefix = self.text(start, self.offset);
                    prefix.append(self.datum()?);
                    return Ok(prefix);
                }
            }
            '#' if matches!(self.peek(), Some('\'' | '`' | ',')) => {
                let mut prefix = self.text(start, self.offset);
                prefix.append(self.datum()?);
                return Ok(prefix);
            }
            '#' if self.peek() == Some('(') => {
                self.next();

                let mut prefix = self.text(start, start + 1);
                prefix.append(self.delimited('(', start + 1)?);
                return Ok(prefix);
            }
            '#' if self.starts("u8(") => {
                self.offset += 3;

                let mut prefix = self.text(start, start + 3);
                prefix.append(self.delimited('(', start + 3)?);
                return Ok(prefix);
            }
            '#' if self.peek() == Some('\\') => {
                self.next();
                self.next()
                    .ok_or_else(|| anyhow::anyhow!("Wisp: incomplete character literal"))?;

                while self
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !"()[]{};".contains(c))
                {
                    self.next();
                }
            }
            _ => {
                while self
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !"()[]{};".contains(c))
                {
                    if self.next() == Some('\\') {
                        self.next();
                    }
                }
            }
        }

        Ok(self.text(start, self.offset))
    }

    fn quoted(&mut self, delimiter: char) -> Result<()> {
        while let Some(next) = self.next() {
            if next == '\\' {
                self.next();
            } else if next == delimiter {
                return Ok(());
            }
        }

        bail!("Wisp line {}: unterminated quoted datum", self.line)
    }

    fn text(&self, start: usize, end: usize) -> Text {
        let value = self.chars[start..end].iter().collect();
        Text {
            value,
            offsets: (self.byte_offsets[start]..self.byte_offsets[end]).collect(),
        }
    }

    fn delimited(&mut self, open: char, start: usize) -> Result<Text> {
        let close = match open {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            _ => unreachable!(),
        };
        let mut values = Vec::new();

        loop {
            self.skip_space_and_comments()?;

            if self.peek() == Some(close) {
                self.next();
                break;
            }

            ensure!(
                self.peek().is_some(),
                "Wisp line {}: unclosed delimiter {open}",
                self.line
            );

            values.push(self.datum()?);
        }

        if open == '{' {
            match values.len() {
                0 => return Ok(Text::generated("()", self.byte_offsets[start])),
                1 => return Ok(values.remove(0)),
                count
                    if count >= 3
                        && count % 2 == 1
                        && values
                            .iter()
                            .skip(1)
                            .step_by(2)
                            .all(|op| op.value == values[1].value) =>
                {
                    let operator = values[1].clone();
                    let operands = values.into_iter().step_by(2).collect::<Vec<_>>();

                    return Ok(Text::list(
                        std::iter::once(operator).chain(operands),
                        self.byte_offsets[start],
                    ));
                }
                _ => {
                    return Ok(Text::list(
                        std::iter::once(Text::generated("$nfx$", self.byte_offsets[start]))
                            .chain(values),
                        self.byte_offsets[start],
                    ));
                }
            }
        }

        Ok(Text::list(values, self.byte_offsets[start]))
    }
}
