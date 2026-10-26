use super::*;

fn region(value: &Value) -> Result<Vec<Span>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(text) = value.as_str() {
        return Ok(vec![Span {
            base_style: None,
            text: text.into(),
            style: String::new(),
        }]);
    }
    let spans = value
        .as_array()
        .context("statusline region must be text or spans")?;
    spans
        .iter()
        .map(|value| {
            if let Some(text) = value.as_str() {
                return Ok(Span {
                    base_style: None,
                    text: text.into(),
                    style: String::new(),
                });
            }
            Ok(Span {
                base_style: None,
                text: value
                    .get("text")
                    .and_then(Value::as_str)
                    .context("statusline span requires text")?
                    .into(),
                style: value
                    .get("style")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            })
        })
        .collect()
}

impl Session {
    pub(super) fn statusline(
        &mut self,
        id: ViewId,
        name: &str,
        context: Value,
        width: usize,
        active: bool,
    ) -> Vec<Span> {
        let style = if active { "status" } else { "status.inactive" };
        if self.scripts.is_none() {
            let mut spans = clip_spans(self.views[&id].location.statusline.clone(), width);
            for span in &mut spans {
                span.base_style = Some(style.into());
            }
            return spans;
        }
        let result = (|| -> Result<Vec<Span>> {
            let formatted = self
                .scripts
                .as_mut()
                .context("statusline runtime unavailable")?
                .format_statusline(name, &context)?;
            formatted
                .as_object()
                .context("statusline formatter must return left, center and right regions")?;
            let left = region(&formatted["left"])?;
            let center = region(&formatted["center"])?;
            let right = region(&formatted["right"])?;
            let length = |spans: &[Span]| {
                spans
                    .iter()
                    .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                    .sum::<usize>()
            };
            let left_width = length(&left).min(width);
            let right_inset = context
                .get("right_inset")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(width.saturating_sub(left_width) as u64)
                as usize;
            let width = width - right_inset;
            let right_width = length(&right).min(width.saturating_sub(left_width));
            let center_width = length(&center).min(width.saturating_sub(left_width + right_width));
            let center_start = ((width.saturating_sub(center_width)) / 2)
                .max(left_width)
                .min(width.saturating_sub(right_width + center_width));
            let mut spans = clip_spans(left, left_width);
            spans.push(Span {
                base_style: None,
                text: " ".repeat(center_start - left_width),
                style: String::new(),
            });
            spans.extend(clip_spans(center, center_width));
            spans.push(Span {
                base_style: None,
                text: " ".repeat(width - right_width - center_start - center_width),
                style: String::new(),
            });
            spans.extend(clip_spans(right, right_width));
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
            Ok(spans)
        })();
        match result {
            Ok(spans) => {
                self.views.get_mut(&id).unwrap().location.statusline = spans.clone();
                spans
            }
            Err(error) => {
                self.message = format!("statusline: {error:#}");
                let mut spans = clip_spans(self.views[&id].location.statusline.clone(), width);
                for span in &mut spans {
                    span.base_style = Some(style.into());
                }
                spans
            }
        }
    }
}
