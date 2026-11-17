use steel::{
    SteelVal,
    rvals::{Custom, FromSteelVal, IntoSteelVal},
    steel_vm::{builtin::BuiltInModule, engine::Engine, register_fn::RegisterFn},
};
use zess_plugin::{
    HostRequest, ItemSelection, PluginInput, PluginOutput, PreviewContent, PreviewLayout,
    PreviewPane, QueryWatch,
};
use zess_ui::{Display, DisplayRow};

#[cfg(test)]
mod tests;

pub(crate) mod json;
pub(super) mod status;

use json::Json;

pub(super) struct Input(pub PluginInput);
impl Custom for Input {}
#[derive(Clone)]
pub(super) struct Row(pub DisplayRow);
impl Custom for Row {}
#[derive(Clone)]
struct Cell(zess_ui::DisplayCell);
impl Custom for Cell {}
#[derive(Clone)]
pub(super) struct Content(pub Display);
impl Custom for Content {}
#[derive(Clone)]
pub(super) struct Selection(pub ItemSelection);
impl Custom for Selection {}
#[derive(Clone)]
pub(super) struct Request(pub HostRequest);
impl Custom for Request {}
#[derive(Clone)]
pub(super) struct Output(pub PluginOutput);
impl Custom for Output {}
#[derive(Clone)]
struct Watch(QueryWatch);
impl Custom for Watch {}
#[derive(Clone)]
struct Preview(PreviewContent);
impl Custom for Preview {}
#[derive(Clone)]
struct Pane(PreviewPane);
impl Custom for Pane {}
#[derive(Clone)]
struct Layout(PreviewLayout);
impl Custom for Layout {}

struct Query(zess_filter::Query);
impl Custom for Query {}

struct CompiledFilter(zess_filter::Filter);
impl Custom for CompiledFilter {}

struct Symbol(zess_plugin::OutlineSymbol);
impl Custom for Symbol {}
struct TextContext(zess_plugin::TextContext);
impl Custom for TextContext {}
struct Context(zess_plugin::PluginContext);
impl Custom for Context {}

pub(super) fn register(engine: &mut Engine) {
    let mut module = BuiltInModule::new("zess/native");
    module.register_fn("%status-spans", status::spans);
    module.register_fn(
        "%format-mode-label",
        |label: String, width: Option<usize>, active: bool| {
            zess_plugin::format_mode_label(&label, width.unwrap_or(usize::MAX), active)
        },
    );
    module.register_fn("%status-format-defaults", || {
        Json(
            zess_plugin::DEFAULT_STATUS_FORMATS
                .iter()
                .map(|(name, template)| ((*name).to_owned(), serde_json::json!(template)))
                .collect::<serde_json::Map<_, _>>()
                .into(),
        )
    });
    module.register_fn(
        "%validate-status-formats",
        |formats: Json| -> steel::rvals::Result<Json> {
            zess_plugin::validate_status_formats(&formats.0).map_err(|error| {
                steel::rerrs::SteelErr::new(
                    steel::rerrs::ErrorKind::TypeMismatch,
                    error.to_string(),
                )
            })?;
            Ok(formats)
        },
    );
    module.register_fn("%input-text", |input: &Input| {
        TextContext(input.0.context.text.clone().unwrap_or_default())
    });
    module.register_fn("text-context-id", |context: &TextContext| {
        context.0.buffer_id
    });
    module.register_fn("text-context-path", |context: &TextContext| {
        context.0.path.clone()
    });
    module.register_fn("text-context-version", |context: &TextContext| {
        context.0.version
    });
    module.register_fn("text-context-symbols", |context: &TextContext| {
        context
            .0
            .symbols
            .iter()
            .cloned()
            .map(Symbol)
            .collect::<Vec<_>>()
    });
    module.register_fn("symbol-id", |symbol: &Symbol| symbol.0.id.clone());
    module.register_fn("symbol-name", |symbol: &Symbol| symbol.0.name.clone());
    module.register_fn("symbol-kind", |symbol: &Symbol| symbol.0.kind.clone());
    module.register_fn("symbol-line", |symbol: &Symbol| symbol.0.line);
    module.register_fn("symbol-column", |symbol: &Symbol| symbol.0.column);
    module.register_fn("symbol-depth", |symbol: &Symbol| symbol.0.depth);
    module.register_fn("context-workspace", |context: &Context| {
        context.0.workspace.clone()
    });
    module.register_fn("context-text", |context: &Context| {
        context.0.text.clone().map(TextContext)
    });
    module.register_fn("context-filesystem-revision", |context: &Context| {
        context.0.filesystem_revision
    });
    module.register_fn("%input-event", |input: &Input| input.0.event.clone());
    module.register_fn("%input-argument", |input: &Input| {
        Json(input.0.argument.clone())
    });
    module.register_fn("%input-query", |input: &Input| {
        input.0.query.clone().map(Query)
    });
    module.register_fn("%input-options", |input: &Input| {
        Json(input.0.options.clone())
    });
    module.register_fn("%input-context", |input: &Input| {
        Context(input.0.context.clone())
    });
    module.register_fn("%input-mode", |input: &Input| input.0.mode.clone());
    module.register_fn("%input-buffer-id", |input: &Input| input.0.buffer_id);
    module.register_fn("%input-view-id", |input: &Input| input.0.view_id);
    module.register_fn("%input-generation", |input: &Input| input.0.generation);
    module.register_fn("%input-register", |input: &Input| input.0.register);
    module.register_fn("%input-selection", |input: &Input| {
        Selection(input.0.selection.clone())
    });
    module.register_fn(
        "item-selection",
        |primary: Option<String>, selected: Vec<String>, anchor: Option<String>| {
            Selection(ItemSelection {
                primary,
                selected,
                anchor,
            })
        },
    );
    module.register_fn("selection-primary", |selection: &Selection| {
        selection.0.primary.clone()
    });
    module.register_fn("selection-selected", |selection: &Selection| {
        selection.0.selected.clone()
    });
    module.register_fn("selection-anchor", |selection: &Selection| {
        selection.0.anchor.clone()
    });
    module.register_fn(
        "display-row",
        |id: String, text: String, style: String, depth: usize| {
            Row(DisplayRow {
                id,
                text,
                style,
                depth,
                ..DisplayRow::default()
            })
        },
    );
    module.register_fn("row-id", |row: &Row| row.0.id.clone());
    module.register_fn("row-text", |row: &Row| row.0.text.clone());
    module.register_fn("row-depth", |row: &Row| row.0.depth);
    module.register_fn("row-style", |row: &Row| row.0.style.clone());
    module.register_fn("display-cell", |text: String, style: String| {
        Cell(zess_ui::DisplayCell {
            spans: vec![zess_ui::Span {
                text,
                style,
                base_style: None,
            }],
        })
    });
    module.register_fn(
        "cell-append-span",
        |mut cell: Cell, text: String, style: String| {
            cell.0.spans.push(zess_ui::Span {
                text,
                style,
                base_style: None,
            });
            cell
        },
    );
    module.register_fn("row-cells", |row: &Row| {
        row.0.cells.iter().cloned().map(Cell).collect::<Vec<_>>()
    });
    module.register_fn("with-row-cells", |mut row: Row, cells: Vec<Cell>| {
        row.0.cells = cells.into_iter().map(|cell| cell.0).collect();
        row
    });
    module.register_fn(
        "buffer-display",
        |title: String, header: String, rows: Vec<Row>, loading: bool, status: Json| {
            Content(Display {
                column_alignment: Default::default(),
                tabs: Vec::new(),
                title,
                header,
                rows: rows.into_iter().map(|row| row.0).collect(),
                rows_revision: None,
                loading,
                status: status.0,
                status_data: Default::default(),
            })
        },
    );
    module.register_fn("with-status-data", |mut display: Content, data: Json| {
        display.0.status_data = data.0;
        display
    });
    module.register_fn(
        "buffer-output",
        |display: Option<Content>, selection: Option<Selection>, requests: Vec<Request>| {
            Output(PluginOutput {
                display: display.map(|d| d.0),
                selection: selection.map(|s| s.0),
                requests: requests.into_iter().map(|r| r.0).collect(),
                ..PluginOutput::default()
            })
        },
    );
    module.register_fn(
        "output-watches",
        |mut output: Output, watches: Vec<Watch>| {
            output.0.watches = Some(watches.into_iter().map(|watch| watch.0).collect());
            output
        },
    );
    module.register_fn("watch-query", |operation: String, argument: Json| {
        Watch(QueryWatch {
            operation,
            argument: argument.0,
        })
    });
    module.register_fn("preview-text", |text: String| {
        Preview(PreviewContent::Text { text })
    });
    module.register_fn("preview-diff", |text: String| {
        Preview(PreviewContent::Diff { text })
    });
    module.register_fn("preview-directory", |rows: Vec<Row>| {
        Preview(PreviewContent::Directory {
            rows: rows.into_iter().map(|row| row.0).collect(),
            column_alignment: Default::default(),
        })
    });
    module.register_fn("preview-unavailable", |message: String| {
        Preview(PreviewContent::Unsupported { message })
    });
    module.register_fn(
        "%preview-pane",
        |id: String, path: String, content: Preview| {
            Pane(PreviewPane::View {
                id,
                path,
                header: String::new(),
                key_scope: None,
                loading: false,
                content: content.0,
            })
        },
    );
    module.register_fn(
        "%preview-surface",
        |id: String, path: String, content: Preview| {
            Pane(PreviewPane::Surface {
                id,
                path,
                header: String::new(),
                loading: false,
                content: content.0,
            })
        },
    );
    module.register_fn("%preview-pane-header", |mut pane: Pane, header: String| {
        match &mut pane.0 {
            PreviewPane::Surface { header: value, .. }
            | PreviewPane::View { header: value, .. } => *value = header,
        }
        pane
    });
    module.register_fn(
        "%preview-pane-scope",
        |mut pane: Pane, scope: String| -> steel::rvals::Result<Pane> {
            let PreviewPane::View { key_scope, .. } = &mut pane.0 else {
                return Err(steel::rerrs::SteelErr::new(
                    steel::rerrs::ErrorKind::TypeMismatch,
                    "only preview views have a key scope".into(),
                ));
            };
            *key_scope = Some(scope);
            Ok(pane)
        },
    );
    module.register_fn("%preview-pane-loading", |mut pane: Pane, loading: bool| {
        match &mut pane.0 {
            PreviewPane::Surface { loading: value, .. }
            | PreviewPane::View { loading: value, .. } => *value = loading,
        }
        pane
    });
    module.register_fn("%preview-layout-rows", || Layout(PreviewLayout::Rows));
    module.register_fn("%preview-layout-columns", || Layout(PreviewLayout::Columns));
    module.register_fn("%preview-layout-responsive", |min_pane_width: usize| {
        Layout(PreviewLayout::Responsive { min_pane_width })
    });
    module.register_fn(
        "%request-preview-group",
        |panes: Vec<Pane>, layout: Layout, focus: bool| {
            Request(HostRequest::PreviewGroup {
                panes: panes.into_iter().map(|pane| pane.0).collect(),
                layout: layout.0,
                focus,
            })
        },
    );
    module.register_fn("output-modified", |mut output: Output, modified: bool| {
        output.0.modified = Some(modified);
        output
    });
    module.register_fn("%request-action", |name: String, argument: Json| {
        Request(HostRequest::Action {
            name,
            argument: argument.0,
        })
    });
    module.register_fn("%request-open-file", |path: String, line: usize| {
        Request(HostRequest::OpenFile { path, line })
    });
    module.register_fn("%request-open-buffer", |kind: String, argument: Json| {
        Request(HostRequest::OpenBuffer {
            kind,
            argument: argument.0,
        })
    });
    module.register_fn("%request-focus-text", |buffer_id: u64, line: usize| {
        Request(HostRequest::FocusText { buffer_id, line })
    });
    module.register_fn(
        "%request-layer",
        |purpose: String, text: String, inline: bool| {
            Request(HostRequest::Layer {
                purpose,
                text,
                inline,
            })
        },
    );
    module.register_fn("%request-file-layer", |path: String, line: usize| {
        Request(HostRequest::FileLayer { path, line })
    });
    module.register_fn(
        "%request-mini-buffer",
        |purpose: String, text: String, key_scope: Option<String>| {
            Request(HostRequest::MiniBuffer {
                purpose,
                text,
                key_scope,
                confirmation: None,
            })
        },
    );
    module.register_fn("%request-mode", |mode: String| {
        Request(HostRequest::SetMode { mode })
    });
    module.register_fn("%request-message", |text: String| {
        Request(HostRequest::Message { text })
    });
    module.register_fn(
        "%request-yank",
        |values: Vec<String>, register: Option<char>| {
            Request(HostRequest::Yank { values, register })
        },
    );
    module.register_fn("%request-schedule", |after_ms: u64| {
        Request(HostRequest::Schedule { after_ms })
    });
    module.register_fn("%request-close", || Request(HostRequest::Close));
    module.register_fn("%configure-view", |options: Json| {
        Request(HostRequest::ConfigureView { options: options.0 })
    });
    module.register_fn(
        "%request-preview",
        |path: String, content: Preview, focus: bool| {
            Request(HostRequest::Preview {
                path,
                content: content.0,
                focus,
            })
        },
    );
    engine.register_module(module);
    let mut module = BuiltInModule::new("zess/filter");
    module.register_fn(
        "query",
        |kind: String, text: String| -> steel::rvals::Result<Query> {
            let kind = match kind.as_str() {
                "plain" => zess_filter::QueryKind::Plain,
                "glob" => zess_filter::QueryKind::Glob,
                "regex" => zess_filter::QueryKind::Regex,
                _ => {
                    return Err(steel::rerrs::SteelErr::new(
                        steel::rerrs::ErrorKind::Generic,
                        format!("Unknown query kind: {kind}"),
                    ));
                }
            };
            Ok(Query(zess_filter::Query { kind, text }))
        },
    );
    module.register_fn("query-kind", |query: &Query| match query.0.kind {
        zess_filter::QueryKind::Plain => "plain".to_owned(),
        zess_filter::QueryKind::Glob => "glob".to_owned(),
        zess_filter::QueryKind::Regex => "regex".to_owned(),
    });
    module.register_fn("query-text", |query: &Query| query.0.text.clone());
    module.register_fn(
        "filter-compile",
        |query: &Query| -> steel::rvals::Result<CompiledFilter> {
            zess_filter::Filter::compile(&query.0, zess_filter::Case::Sensitive)
                .map(CompiledFilter)
                .map_err(|error| {
                    steel::rerrs::SteelErr::new(steel::rerrs::ErrorKind::Generic, error.to_string())
                })
        },
    );
    module.register_fn(
        "filter-matches?",
        |filter: &CompiledFilter, candidate: String| filter.0.is_match(&candidate),
    );
    module.register_fn("filter-source", |filter: &CompiledFilter| {
        filter.0.source().to_owned()
    });
    engine.register_module(module);
}

pub(super) fn output(value: &SteelVal) -> steel::rvals::Result<PluginOutput> {
    Ok(Output::from_steelval(value)?.0)
}
pub(super) fn input(input: &PluginInput) -> steel::rvals::Result<SteelVal> {
    Input(input.clone()).into_steelval()
}
