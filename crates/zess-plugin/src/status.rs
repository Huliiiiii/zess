use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zess_ui::{Span, clip_spans, spans_width};

use crate::{BufferId, ViewId};

mod layout;
mod render;
pub use layout::{StatusConfig, StatusElement, StatusLayout};

pub fn format_mode_label(label: &str, width: usize, active: bool) -> String {
    let cells = label.width();
    if width.saturating_sub(cells) >= 2 {
        return if active {
            format!(" {label} ")
        } else {
            " ".repeat(cells + 2)
        };
    }
    let label = if cells > width {
        label
            .graphemes(true)
            .next()
            .filter(|first| first.width() <= width)
            .unwrap_or_default()
    } else {
        label
    };
    if active {
        label.into()
    } else {
        " ".repeat(label.width())
    }
}

pub const DEFAULT_STATUS_FORMATS: &[(&str, &str)] = &[
    ("modified", " [+]"),
    ("loading", " …"),
    ("pending", "{count} pending"),
    ("items", "{selected} selected / {items} items"),
    ("text", "{selections} sel  {position}  {encoding}"),
    ("git-loading", "Loading changes…"),
    ("git-failed", "Could not load changes"),
    ("git-refreshing", "Refreshing…  "),
    ("git-writing", "Git file operation is running…  "),
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusPosition {
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusLine {
    pub left: Vec<Span>,
    pub center: Vec<Span>,
    pub right: Vec<Span>,
}

impl StatusLine {
    pub fn render(&self, width: usize, right_inset: usize, active: bool) -> Vec<Span> {
        let style = if active { "status" } else { "status.inactive" };
        let left_width = spans_width(&self.left).min(width);
        let right_inset = right_inset.min(width.saturating_sub(left_width));
        let width = width - right_inset;
        let right_width = spans_width(&self.right).min(width.saturating_sub(left_width));
        let center_width =
            spans_width(&self.center).min(width.saturating_sub(left_width + right_width));
        let center_start = ((width.saturating_sub(center_width)) / 2)
            .max(left_width)
            .min(width.saturating_sub(right_width + center_width));
        let mut spans = clip_spans(self.left.clone(), left_width);
        spans.push(Span {
            base_style: None,
            text: " ".repeat(center_start - left_width),
            style: String::new(),
        });
        spans.extend(clip_spans(self.center.clone(), center_width));
        spans.push(Span {
            base_style: None,
            text: " ".repeat(width - right_width - center_start - center_width),
            style: String::new(),
        });
        spans.extend(clip_spans(self.right.clone(), right_width));
        if right_inset > 0 {
            spans.push(Span {
                base_style: None,
                text: " ".repeat(right_inset),
                style: String::new(),
            });
        }
        for span in &mut spans {
            span.base_style = Some(style.into());
        }
        spans
    }
}
