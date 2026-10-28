use std::{error::Error, fmt};

use globset::{GlobBuilder, GlobMatcher};
use regex::{Regex, RegexBuilder};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub enum Case {
    Sensitive,
    Insensitive,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum QueryKind {
    #[default]
    Plain,
    Glob,
    Regex,
}

#[derive(Clone, Default, PartialEq)]
pub struct Query {
    pub kind: QueryKind,
    pub text: String,
}

#[derive(Clone)]
pub struct Filter {
    query: Query,
    matcher: Matcher,
}

#[derive(Clone)]
enum Matcher {
    Substring { text: String, case: Case },
    Glob { matcher: GlobMatcher, case: Case },
    Regex(Regex),
}

impl Filter {
    pub fn compile(query: &Query, case: Case) -> Result<Self, CompileError> {
        let pattern = query.text.as_str();
        let matcher = match query.kind {
            QueryKind::Glob => {
                let pattern = match case {
                    Case::Sensitive => pattern.to_owned(),
                    Case::Insensitive => pattern.to_lowercase(),
                };
                Matcher::Glob {
                    matcher: GlobBuilder::new(&pattern)
                        .literal_separator(true)
                        .build()
                        .map_err(|error| CompileError(format!("Invalid glob: {error}")))?
                        .compile_matcher(),
                    case,
                }
            }
            QueryKind::Regex => Matcher::Regex(
                RegexBuilder::new(pattern)
                    .case_insensitive(matches!(case, Case::Insensitive))
                    .build()
                    .map_err(|error| {
                        let message = error.to_string();
                        let reason = message.lines().last().unwrap_or(&message);
                        CompileError(format!(
                            "Invalid regex: {}",
                            reason.trim_start_matches("error: ")
                        ))
                    })?,
            ),
            QueryKind::Plain => Matcher::Substring {
                text: match case {
                    Case::Sensitive => pattern.to_owned(),
                    Case::Insensitive => pattern.to_lowercase(),
                },
                case,
            },
        };
        Ok(Self {
            query: query.clone(),
            matcher,
        })
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn source(&self) -> &str {
        &self.query.text
    }

    pub fn is_empty(&self) -> bool {
        self.query.text.is_empty()
    }

    pub fn is_match(&self, candidate: &str) -> bool {
        match &self.matcher {
            Matcher::Substring {
                text,
                case: Case::Sensitive,
            } => candidate.contains(text),
            Matcher::Substring {
                text,
                case: Case::Insensitive,
            } => candidate.to_lowercase().contains(text),
            Matcher::Glob {
                matcher,
                case: Case::Sensitive,
            } => matcher.is_match(candidate),
            Matcher::Glob {
                matcher,
                case: Case::Insensitive,
            } => matcher.is_match(candidate.to_lowercase()),
            Matcher::Regex(regex) => regex.is_match(candidate),
        }
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            query: Query::default(),
            matcher: Matcher::Substring {
                text: String::new(),
                case: Case::Sensitive,
            },
        }
    }
}

#[derive(Debug)]
pub struct CompileError(String);

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CompileError {}
