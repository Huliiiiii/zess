use serde_json::Value;

pub(crate) fn header_style(options: Option<&Value>) -> &str {
    options
        .and_then(|options| options.get("header-style"))
        .and_then(Value::as_str)
        .unwrap_or("ui.heading")
}

pub(crate) fn gutter_width(options: Option<&Value>, digits: usize, width: usize) -> usize {
    if options.and_then(|options| options.get("gutter")) == Some(&Value::Bool(false)) {
        return 0;
    }
    let padding = options
        .and_then(|options| options.get("gutter-left-padding"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .min(width as u64) as usize;
    (digits + padding + 1).min(width.saturating_sub(1))
}
