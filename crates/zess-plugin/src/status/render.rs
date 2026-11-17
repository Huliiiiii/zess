use serde_json::Value;
use zess_ui::Span;

use super::{
    DEFAULT_STATUS_FORMATS, StatusConfig, StatusContent, StatusContext, StatusElement, StatusLine,
    format_mode_label,
};

fn span(text: impl Into<String>, style: &str) -> Span {
    Span {
        text: text.into(),
        style: style.into(),
        base_style: None,
    }
}

fn plain(text: impl Into<String>) -> Vec<Span> {
    vec![span(text, "")]
}

struct Formatter<'a> {
    config: &'a StatusConfig,
    context: &'a StatusContext,
}

impl Formatter<'_> {
    fn template(&self, key: &str, fields: &[(&str, String)]) -> String {
        let mut text = self.context.options["status-formats"]
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| self.config.status_formats.get(key).map(String::as_str))
            .or_else(|| {
                DEFAULT_STATUS_FORMATS
                    .iter()
                    .find(|(id, _)| *id == key)
                    .map(|(_, value)| *value)
            })
            .unwrap_or("")
            .to_owned();
        for (field, value) in fields {
            text = text.replace(&format!("{{{field}}}"), value);
        }
        text
    }

    fn mode(&self) -> Span {
        let mode = self.context.mode.as_deref().unwrap_or("normal");
        let label = self
            .config
            .mode_labels
            .get(mode)
            .cloned()
            .unwrap_or_else(|| mode.chars().take(3).collect::<String>().to_uppercase());
        let text = format_mode_label(&label, self.context.width, self.context.active);
        if self.context.active {
            span(text, &format!("status.{mode}"))
        } else {
            span(text, "status.inactive")
        }
    }

    fn position(&self) -> String {
        if let StatusContent::Text { position, .. } = &self.context.content {
            format!("{}:{}", position.row, position.column)
        } else {
            String::new()
        }
    }

    fn items(&self) -> String {
        let (selected, items) = match self.context.content {
            StatusContent::Items {
                selection_count,
                item_count,
                ..
            } => (selection_count, item_count),
            _ => (0, 0),
        };
        self.template(
            "items",
            &[
                ("selected", selected.to_string()),
                ("items", items.to_string()),
            ],
        )
    }

    fn pending(&self) -> String {
        let count = self.context.plugin_status_data["files"]["pending_count"]
            .as_u64()
            .unwrap_or(0);
        if count > 0 {
            self.template("pending", &[("count", count.to_string())])
        } else {
            String::new()
        }
    }

    fn plugin(&self) -> Option<Vec<Span>> {
        let value = &self.context.plugin_status;
        let value = if value.is_object() {
            value.get("text").unwrap_or(&Value::Null)
        } else {
            value
        };
        let parse = |value: &Value| match value {
            Value::String(text) => Some(span(text, "")),
            Value::Object(obj) => Some(span(
                obj.get("text")?.as_str()?,
                obj.get("style").and_then(Value::as_str).unwrap_or(""),
            )),
            _ => None,
        };
        match value {
            Value::Array(values) => values.iter().map(parse).collect(),
            Value::Null => Some(plain("")),
            Value::String(_) | Value::Object(_) => Some(vec![parse(value)?]),
            _ => None,
        }
    }

    fn git_pending(&self) -> bool {
        self.context.plugin_status["git"]["write_epoch"].is_u64()
            || (self.context.plugin_status["error"].is_null()
                || self.context.plugin_status["error"] == false)
                && matches!(
                    self.context.plugin_status["git"]["state"].as_str(),
                    Some("loading" | "refreshing")
                )
    }

    fn git(&self) -> Vec<Span> {
        let status = &self.context.plugin_status;
        let git = &status["git"];
        let writing = self.context.loading && git["write_epoch"].is_u64();
        if status
            .get("error")
            .is_some_and(|err| !err.is_null() && err != false)
            || !git.is_object()
        {
            return plain(if writing {
                self.template("git-writing", &[])
            } else {
                String::new()
            });
        }
        match git["state"].as_str() {
            Some("loading") => {
                return plain(if self.context.loading {
                    self.template(
                        if writing {
                            "git-writing"
                        } else {
                            "git-loading"
                        },
                        &[],
                    )
                } else {
                    String::new()
                });
            }
            Some("failed") => {
                return plain(
                    self.template(if writing { "git-writing" } else { "git-failed" }, &[]),
                );
            }
            _ => {}
        }
        let (Some(added), Some(deleted)) = (git["additions"].as_u64(), git["deletions"].as_u64())
        else {
            return plain(self.template(
                if writing {
                    "git-writing"
                } else {
                    "git-unavailable"
                },
                &[],
            ));
        };
        let uncounted = git["unknown_files"].as_u64().unwrap_or(0);
        vec![
            span(
                if writing {
                    self.template("git-writing", &[])
                } else if self.context.loading && git["state"] == "refreshing" {
                    self.template("git-refreshing", &[])
                } else {
                    String::new()
                },
                "",
            ),
            span(
                self.template("git-added", &[("count", added.to_string())]),
                "git.added",
            ),
            span("  ", ""),
            span(
                self.template("git-deleted", &[("count", deleted.to_string())]),
                "git.deleted",
            ),
            span(
                if uncounted > 0 {
                    self.template("git-uncounted", &[("count", uncounted.to_string())])
                } else {
                    String::new()
                },
                "",
            ),
        ]
    }

    fn component(&self, name: &str) -> Option<Vec<Span>> {
        let ctx = self.context;
        Some(match name {
            "mode" => vec![self.mode()],
            "title" => plain(&ctx.title),
            "modified" => plain(if ctx.modified {
                self.template("modified", &[])
            } else {
                String::new()
            }),
            "loading" => plain(if ctx.loading {
                self.template("loading", &[])
            } else {
                String::new()
            }),
            "pending" => plain(self.pending()),
            "items" => plain(if matches!(ctx.content, StatusContent::Items { .. }) {
                self.items()
            } else {
                String::new()
            }),
            "git" => self.git(),
            "position" => plain(self.position()),
            "encoding" => plain(match &ctx.content {
                StatusContent::Text { encoding, .. } => encoding.as_str(),
                _ => "",
            }),
            "source-position" => plain(match ctx.content {
                StatusContent::Diff {
                    new_line, old_line, ..
                } => new_line
                    .or(old_line)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            }),
            "text" => plain(match &ctx.content {
                StatusContent::Text {
                    selection_count,
                    encoding,
                    ..
                } => self.template(
                    "text",
                    &[
                        ("selections", selection_count.to_string()),
                        ("position", self.position()),
                        ("encoding", encoding.clone()),
                    ],
                ),
                _ => String::new(),
            }),
            "plugin" => self.plugin()?,
            _ => return None,
        })
    }

    fn elements(&self, elements: &[StatusElement], separator: &str) -> Option<Vec<Span>> {
        let mut result = Vec::new();
        for element in elements {
            let spans = match element {
                StatusElement::Literal(text) => plain(text),
                StatusElement::Component(name) if self.config.builtin_components.contains(name) => {
                    self.component(name)?
                }
                StatusElement::Component(_) => return None,
                StatusElement::Join {
                    elements,
                    separator,
                } => self.elements(elements, separator)?,
            };
            if spans.iter().any(|span| !span.text.is_empty()) {
                if !result.is_empty() && !separator.is_empty() {
                    result.push(span(separator, ""));
                }
                result.extend(spans);
            }
        }
        Some(result)
    }

    fn builtin(&self, name: &str) -> Option<StatusLine> {
        let ctx = self.context;
        let mut left = vec![self.mode()];
        let (center, right) = match name {
            "default" => {
                let show_git_totals = ctx.kind != "git-history";
                let files = ctx.plugin_status_data["files"].is_object();
                let pending = self.pending();
                left.push(span(" ", ""));
                left.push(span(&ctx.title, ""));
                if ctx.kind != "files" || pending.is_empty() {
                    left.extend(self.component("modified")?);
                }
                if !show_git_totals || !self.git_pending() {
                    left.extend(self.component("loading")?);
                }
                let center = if files { plain("") } else { self.plugin()? };
                let right = if show_git_totals
                    && (ctx.plugin_status["git"].is_object()
                        || ctx
                            .plugin_status
                            .get("error")
                            .is_some_and(|err| !err.is_null() && err != false))
                {
                    self.git()
                } else if matches!(ctx.content, StatusContent::Text { .. }) {
                    self.component("text")?
                } else {
                    plain(
                        [pending, self.items()]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · "),
                    )
                };
                (center, right)
            }
            "preview" | "diff" => {
                if name == "preview" && ctx.mode.is_none() {
                    left = plain("")
                } else {
                    left.extend(self.component("loading")?)
                }
                (
                    self.plugin()?,
                    self.component(if name == "diff" {
                        "source-position"
                    } else {
                        "text"
                    })?,
                )
            }
            _ => return None,
        };
        Some(StatusLine {
            left,
            center,
            right,
        })
    }
}

impl StatusConfig {
    /// Evaluates registered built-in components and declarative layouts. An opaque script requires its host runtime.
    pub fn format_local(&self, name: &str, context: &StatusContext) -> Option<StatusLine> {
        if !self.builtin_statuslines.contains(name) {
            return None;
        }
        let formatter = Formatter {
            config: self,
            context,
        };
        let layout = self.layouts.get(&context.kind);
        let mut line = formatter.builtin(name)?;
        if let Some(layout) = layout {
            for (region, elements) in [
                (&mut line.left, &layout.left),
                (&mut line.center, &layout.center),
                (&mut line.right, &layout.right),
            ] {
                if let Some(elements) = elements {
                    *region = formatter.elements(elements, "")?
                }
            }
        }
        Some(line)
    }
}
