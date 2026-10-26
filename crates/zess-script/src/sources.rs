use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use steel::{
    parser::{parser::SourceId, span::Span},
    rerrs::SteelErr,
    steel_vm::engine::Engine,
};

pub(super) struct SourceOrigin {
    path: PathBuf,
    original: String,
    mapping: Option<zess_wisp::SourceMap>,
    prefix: usize,
}

impl SourceOrigin {
    pub(super) fn translate(source: &str, path: &Path) -> Result<(Self, String)> {
        let (scheme, mapping) = if path.extension().is_some_and(|extension| extension == "scm") {
            (source.to_owned(), None)
        } else {
            let translated = zess_wisp::read_with_source_map(source)
                .with_context(|| format!("read Wisp {}", path.display()))?;
            (translated.scheme, Some(translated.source_map))
        };
        Ok((
            Self {
                path: path.into(),
                original: source.into(),
                mapping,
                prefix: 0,
            },
            scheme,
        ))
    }

    pub(super) fn module(mut self) -> Self {
        self.prefix = steel::compiler::modules::PRELUDE_STRING.len();
        self
    }

    fn location(&self, offset: usize) -> Option<String> {
        let generated = offset.checked_sub(self.prefix)?;
        let original = match &self.mapping {
            Some(mapping) => mapping.original_offset(generated)?,
            None => generated,
        };
        location(&self.path, &self.original, original)
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
pub(super) enum SourceIdentity {
    ProgramPath(PathBuf),
    Module(String),
}

impl SourceIdentity {
    fn name(&self) -> &Path {
        match self {
            Self::ProgramPath(path) => path,
            Self::Module(name) => Path::new(name),
        }
    }
}

#[derive(Default)]
pub(super) struct Sources {
    pending: HashMap<SourceIdentity, SourceOrigin>,
    compiled: HashMap<SourceId, SourceOrigin>,
}

impl Sources {
    pub(super) fn register(&mut self, identity: SourceIdentity, source: SourceOrigin) {
        self.pending.insert(identity, source);
    }

    pub(super) fn bind_pending(&mut self, engine: &Engine) {
        let loaded = self
            .pending
            .keys()
            .filter_map(|identity| {
                engine
                    .get_source_id(identity.name())
                    .map(|id| (identity.clone(), id))
            })
            .collect::<Vec<_>>();
        for (identity, id) in loaded {
            self.compiled
                .insert(id, self.pending.remove(&identity).unwrap());
        }
    }

    pub(super) fn error(
        &self,
        engine: &Engine,
        error: SteelErr,
        fallback: Option<&Path>,
    ) -> anyhow::Error {
        let spans = error
            .span()
            .into_iter()
            .chain(error.stack_trace().iter().flat_map(|trace| {
                trace
                    .trace()
                    .iter()
                    .rev()
                    .filter_map(|context| *context.span())
            }));
        let mut locations = Vec::new();
        for span in spans {
            if let Some(location) = self.location(engine, span)
                && !locations.contains(&location)
            {
                locations.push(location);
            }
        }
        if locations.is_empty()
            && let Some(path) = fallback
        {
            locations.push(path.display().to_string());
        }
        let error = anyhow::Error::new(error);
        if locations.is_empty() {
            error
        } else {
            error.context(locations.join("\n  called from "))
        }
    }

    fn location(&self, engine: &Engine, span: Span) -> Option<String> {
        let id = span.source_id?;
        if let Some(source) = self.compiled.get(&id) {
            return source.location(span.start as usize);
        }
        let path = engine.get_path_for_source_id(&id)?;
        if let Some((_, source)) = self
            .pending
            .iter()
            .find(|(identity, _)| identity.name() == path)
        {
            return source.location(span.start as usize);
        }
        let source = engine.get_source(&id)?;
        location(&path, &source, span.start as usize)
    }
}

fn location(path: &Path, source: &str, offset: usize) -> Option<String> {
    let prefix = source.get(..offset)?;
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next()?.chars().count() + 1;
    Some(format!("{}:{line}:{column}", path.display()))
}
