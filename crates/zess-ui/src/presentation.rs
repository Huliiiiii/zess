use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{CursorShape, Frame, FrameRow, MouseContext, Span, TextStyle};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderPlan {
    pub width: u16,
    pub height: u16,
    pub styles: HashMap<String, TextStyle>,
    pub mouse_context: MouseContext,
    pub mouse_enabled: bool,
    pub rows: Vec<RenderRow>,
    pub cursor: Option<(u16, u16)>,
    pub cursor_shape: CursorShape,
    pub status: String,
    pub prompt: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderRow {
    pub spans: Vec<RenderSpan>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderSpan {
    pub base_style: Option<String>,
    pub style: String,
    pub content: RenderContent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderContent {
    Literal {
        text: String,
    },
    Text {
        buffer_id: u64,
        start_char: usize,
        end_char: usize,
    },
    Item {
        source: ContentSource,
        row_id: String,
        cell: Option<usize>,
        start_char: usize,
        end_char: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentSource {
    View { view_id: u64 },
    Surface { page_id: u64, slot_id: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderError {
    pub source: RenderContent,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "render source is absent from the cached state: {:?}",
            self.source
        )
    }
}

impl std::error::Error for RenderError {}

pub fn render_plan(
    plan: &RenderPlan,
    mut resolve: impl FnMut(&RenderContent) -> Option<String>,
) -> Result<Frame, RenderError> {
    let rows = plan
        .rows
        .iter()
        .map(|row| {
            let spans = row
                .spans
                .iter()
                .map(|span| {
                    let text = match &span.content {
                        RenderContent::Literal { text } => text.clone(),
                        source => resolve(source).ok_or_else(|| RenderError {
                            source: source.clone(),
                        })?,
                    };
                    Ok(Span {
                        base_style: span.base_style.clone(),
                        style: span.style.clone(),
                        text,
                    })
                })
                .collect::<Result<Vec<_>, RenderError>>()?;
            Ok(FrameRow { spans })
        })
        .collect::<Result<Vec<_>, RenderError>>()?;
    Ok(Frame {
        width: plan.width,
        height: plan.height,
        styles: plan.styles.clone(),
        mouse_context: plan.mouse_context,
        mouse_enabled: plan.mouse_enabled,
        rows,
        cursor: plan.cursor,
        cursor_shape: plan.cursor_shape.clone(),
        status: plan.status.clone(),
        prompt: plan.prompt.clone(),
    })
}
