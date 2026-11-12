use serde_json::Value;
use zess_ui::Span;

use crate::{BufferId, ViewId};

pub const DEFAULT_STATUS_FORMATS: &[(&str, &str)] = &[
    ("modified", " [+]"),
    ("loading", " …"),
    ("pending", "{count} pending"),
    ("items", "{selected} selected / {items} items"),
    ("text", "{selections} sel  {position}  {encoding}"),
    ("git-loading", "Loading changes…"),
    ("git-failed", "Could not load changes"),
    ("git-refreshing", "Refreshing…  "),
    ("git-unavailable", "Line counts unavailable"),
    ("git-added", "+{count}"),
    ("git-deleted", "-{count}"),
    ("git-uncounted", "  {count} uncounted"),
];

pub fn default_status_text(name: &str, fields: &[(&str, String)]) -> anyhow::Result<String> {
    let mut text = DEFAULT_STATUS_FORMATS
        .iter()
        .find(|(key, _)| *key == name)
        .ok_or_else(|| anyhow::anyhow!("unknown status format {name}"))?
        .1
        .to_owned();
    for (key, value) in fields {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    Ok(text)
}

pub fn validate_status_formats(value: &Value) -> anyhow::Result<()> {
    let formats = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("status-formats must be an object"))?;
    for (name, value) in formats {
        let default = DEFAULT_STATUS_FORMATS
            .iter()
            .find(|(key, _)| key == name)
            .ok_or_else(|| anyhow::anyhow!("unknown status format {name}"))?
            .1;
        let template = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("status format {name} must be a string"))?;
        let mut rest = template;
        while let Some(index) = rest.find(['{', '}']) {
            rest = &rest[index..];
            let end = rest
                .find('}')
                .filter(|_| rest.starts_with('{'))
                .ok_or_else(|| anyhow::anyhow!("unmatched brace in status format {name}"))?;
            let field = &rest[..=end];
            if !default.contains(field) {
                anyhow::bail!("unknown field {field} in status format {name}");
            }
            rest = &rest[end + 1..];
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct StatusContext {
    pub kind: String,
    pub title: String,
    pub active: bool,
    pub modified: bool,
    pub loading: bool,
    pub view_id: ViewId,
    pub buffer_id: BufferId,
    pub width: usize,
    pub right_inset: usize,
    pub mode: Option<String>,
    pub options: Value,
    pub plugin_status: Value,
    pub plugin_status_data: Value,
    pub content: StatusContent,
}

#[derive(Default)]
pub enum StatusContent {
    #[default]
    None,
    Text {
        position: StatusPosition,
        selection_count: usize,
        encoding: String,
    },
    Diff {
        old_line: Option<usize>,
        new_line: Option<usize>,
        selection_count: usize,
    },
    Items {
        selection_count: usize,
        primary: Option<String>,
        item_count: usize,
    },
}

pub struct StatusPosition {
    pub row: usize,
    pub column: usize,
}

pub struct StatusLine {
    pub left: Vec<Span>,
    pub center: Vec<Span>,
    pub right: Vec<Span>,
}
