use anyhow::{Context, Result};
use steel::{HashMap, SteelVal, gc::Gc, rvals::IntoSteelVal};
use zess_plugin::{StatusContent, StatusContext, StatusLine};
use zess_ui::Span;

use super::json::Json;

pub(crate) fn context(context: &StatusContext) -> steel::rvals::Result<SteelVal> {
    let mut fields = HashMap::new();
    for (name, value) in [
        ("kind", context.kind.clone().into_steelval()?),
        ("title", context.title.clone().into_steelval()?),
        ("active", context.active.into_steelval()?),
        ("modified", context.modified.into_steelval()?),
        ("loading", context.loading.into_steelval()?),
        ("view_id", SteelVal::from(u128::from(context.view_id))),
        ("buffer_id", SteelVal::from(u128::from(context.buffer_id))),
        ("width", context.width.into_steelval()?),
        ("right_inset", context.right_inset.into_steelval()?),
        ("options", Json(context.options.clone()).into_steelval()?),
        (
            "plugin_status",
            Json(context.plugin_status.clone()).into_steelval()?,
        ),
        (
            "plugin_status_data",
            Json(context.plugin_status_data.clone()).into_steelval()?,
        ),
    ] {
        fields.insert(SteelVal::SymbolV(name.into()), value);
    }
    if let Some(mode) = &context.mode {
        fields.insert(
            SteelVal::SymbolV("mode".into()),
            mode.clone().into_steelval()?,
        );
    }
    match &context.content {
        StatusContent::None => {}
        StatusContent::Text {
            position,
            selection_count,
            encoding,
        } => {
            fields.insert(
                SteelVal::SymbolV("position".into()),
                hash([
                    ("row", position.row.into_steelval()?),
                    ("column", position.column.into_steelval()?),
                ]),
            );
            fields.insert(
                SteelVal::SymbolV("selection_count".into()),
                selection_count.into_steelval()?,
            );
            fields.insert(
                SteelVal::SymbolV("encoding".into()),
                encoding.clone().into_steelval()?,
            );
        }
        StatusContent::Diff {
            old_line,
            new_line,
            selection_count,
        } => {
            let old_line = old_line.map(SteelVal::from).unwrap_or(SteelVal::Void);
            let new_line = new_line.map(SteelVal::from).unwrap_or(SteelVal::Void);
            fields.insert(
                SteelVal::SymbolV("source_position".into()),
                hash([
                    ("old_line", old_line.clone()),
                    ("new_line", new_line.clone()),
                ]),
            );
            fields.insert(SteelVal::SymbolV("old_line".into()), old_line);
            fields.insert(SteelVal::SymbolV("new_line".into()), new_line);
            fields.insert(
                SteelVal::SymbolV("selection_count".into()),
                selection_count.into_steelval()?,
            );
        }
        StatusContent::Items {
            selection_count,
            primary,
            item_count,
        } => {
            fields.insert(
                SteelVal::SymbolV("selection_count".into()),
                selection_count.into_steelval()?,
            );
            fields.insert(
                SteelVal::SymbolV("selected_count".into()),
                selection_count.into_steelval()?,
            );
            fields.insert(
                SteelVal::SymbolV("primary".into()),
                match primary {
                    Some(primary) => primary.clone().into_steelval()?,
                    None => SteelVal::Void,
                },
            );
            fields.insert(
                SteelVal::SymbolV("item_count".into()),
                item_count.into_steelval()?,
            );
        }
    }
    Ok(SteelVal::HashMapV(Gc::new(fields).into()))
}

fn hash<const N: usize>(fields: [(&str, SteelVal); N]) -> SteelVal {
    SteelVal::HashMapV(
        Gc::new(
            fields
                .into_iter()
                .map(|(name, value)| (SteelVal::SymbolV(name.into()), value))
                .collect::<HashMap<_, _>>(),
        )
        .into(),
    )
}

pub(super) fn spans(value: SteelVal) -> steel::rvals::Result<SteelVal> {
    let spans = if matches!(value, SteelVal::HashMapV(_)) {
        span(&value).map(|span| vec![span])
    } else {
        region(Some(&value))
    }
    .map_err(|error| {
        steel::rerrs::SteelErr::new(steel::rerrs::ErrorKind::TypeMismatch, error.to_string())
    })?;
    let spans = spans
        .into_iter()
        .map(|span| {
            Ok(hash([
                ("text", span.text.into_steelval()?),
                ("style", span.style.into_steelval()?),
            ]))
        })
        .collect::<steel::rvals::Result<Vec<_>>>()?;
    Ok(SteelVal::ListV(spans.into_iter().collect()))
}

pub(crate) fn line(value: &SteelVal) -> Result<StatusLine> {
    let SteelVal::HashMapV(fields) = value else {
        anyhow::bail!("statusline formatter must return left, center and right regions");
    };
    Ok(StatusLine {
        left: region(field(fields, "left"))?,
        center: region(field(fields, "center"))?,
        right: region(field(fields, "right"))?,
    })
}

fn field<'a>(fields: &'a steel::rvals::SteelHashMap, name: &str) -> Option<&'a SteelVal> {
    fields
        .get(&SteelVal::StringV(name.into()))
        .or_else(|| fields.get(&SteelVal::SymbolV(name.into())))
}

pub(crate) fn region(value: Option<&SteelVal>) -> Result<Vec<Span>> {
    match value {
        None | Some(SteelVal::Void) => Ok(Vec::new()),
        Some(value @ (SteelVal::StringV(_) | SteelVal::SymbolV(_) | SteelVal::CharV(_))) => {
            Ok(vec![span(value)?])
        }
        Some(value @ (SteelVal::ListV(_) | SteelVal::VectorV(_) | SteelVal::HashSetV(_))) => {
            let values = match value {
                SteelVal::HashSetV(values) => values.iter().collect::<Vec<_>>(),
                SteelVal::ListV(values) => values.iter().collect(),
                SteelVal::VectorV(values) => values.iter().collect(),
                _ => unreachable!(),
            };
            values.into_iter().map(span).collect()
        }
        _ => anyhow::bail!("statusline region must be text or spans"),
    }
}

fn span(value: &SteelVal) -> Result<Span> {
    let text = |value: &SteelVal| match value {
        SteelVal::StringV(value) | SteelVal::SymbolV(value) => Some(value.to_string()),
        SteelVal::CharV(value) => Some(value.to_string()),
        _ => None,
    };
    if let Some(text) = text(value) {
        return Ok(Span {
            text,
            style: String::new(),
            base_style: None,
        });
    }
    let SteelVal::HashMapV(fields) = value else {
        anyhow::bail!("statusline span requires text");
    };
    Ok(Span {
        text: field(fields, "text")
            .and_then(text)
            .context("statusline span requires text")?,
        style: field(fields, "style").and_then(text).unwrap_or_default(),
        base_style: None,
    })
}
