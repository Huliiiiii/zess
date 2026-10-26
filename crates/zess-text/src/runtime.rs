use std::{
    collections::VecDeque,
    env, fs,
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
};

use anyhow::{Context, Result, ensure};
use etcetera::base_strategy::{BaseStrategy, choose_base_strategy};
use helix_loader::grammar::{GrammarConfiguration, GrammarSelection, GrammarSource};
use serde::Deserialize;

#[cfg(test)]
mod tests;

const QUERIES: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/queries.rs"));

static DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Selects Zess resources before the first language is loaded.
pub fn initialize(directory: Option<&Path>) -> Result<PathBuf> {
    let mut configured = DIRECTORY.lock().unwrap();
    if let Some(current) = configured.as_ref() {
        if let Some(directory) = directory {
            ensure!(
                absolute(directory)? == *current,
                "syntax runtime is already initialized at {}",
                current.display()
            );
        }
        return Ok(current.clone());
    }

    let directory = directory.map(absolute).unwrap_or_else(default_directory)?;
    helix_loader::initialize_runtime_dir(directory.clone()).map_err(|_| {
        anyhow::anyhow!(
            "Helix runtime lookup was initialized before Zess selected {}",
            directory.display()
        )
    })?;
    *configured = Some(directory.clone());
    Ok(directory)
}

/// Uses an explicit Zess directory, a packaged sibling runtime, or the Zess cache.
pub fn default_directory() -> Result<PathBuf> {
    if let Some(directory) = env::var_os("ZESS_RUNTIME") {
        ensure!(!directory.is_empty(), "ZESS_RUNTIME must name a directory");
        return absolute(Path::new(&directory));
    }
    let executable = env::current_exe().context("locate Zess executable")?;
    let executable = executable
        .canonicalize()
        .context("resolve Zess executable")?;
    let sibling = executable
        .parent()
        .context("executable has no parent directory")?
        .join("runtime");
    if sibling.is_dir() {
        return Ok(sibling);
    }
    let strategy = choose_base_strategy().context("locate Zess cache directory")?;
    Ok(strategy.cache_dir().join("zess/runtime"))
}

/// Fetches selected pinned grammar sources; an empty selection uses Helix's defaults.
pub fn fetch_grammars(directory: Option<&Path>, names: &[String]) -> Result<PathBuf> {
    let directory = directory.map(absolute).unwrap_or_else(default_directory)?;
    let grammars = grammar_configs(&directory, names)?;
    let directory = initialize(Some(&directory))?;
    run_grammars(grammars, "fetch", 8, |grammar| {
        if grammar.grammar_id == "wisp" {
            extract_wisp(&directory)
        } else {
            helix_loader::grammar::fetch_grammar_source(grammar)
        }
    })?;
    Ok(directory)
}

/// Builds selected cached grammars and installs bundled queries without networking.
pub fn build_grammars(directory: Option<&Path>, names: &[String]) -> Result<PathBuf> {
    let directory = directory.map(absolute).unwrap_or_else(default_directory)?;
    let grammars = grammar_configs(&directory, names)?;
    let directory = initialize(Some(&directory))?;
    fs::create_dir_all(directory.join("grammars")).context("create grammar directory")?;
    for (file, contents) in QUERIES {
        write_resource(&directory.join(file), contents)?;
    }
    run_grammars(grammars, "build", 4, |grammar| {
        if grammar.grammar_id == "wisp" {
            extract_wisp(&directory)?;
            write_resource(
                &directory.join("grammars/LICENSE-wisp"),
                include_bytes!("../../../vendor/tree-sitter-wisp/LICENSE"),
            )?;
        }
        let source = match &grammar.source {
            GrammarSource::Local { path } => PathBuf::from(path),
            GrammarSource::Git { subpath, .. } => {
                let source = directory.join("grammars/sources").join(&grammar.grammar_id);
                if let Some(path) = subpath {
                    source.join(path)
                } else {
                    source
                }
            }
        }
        .join("src");
        ensure!(
            source.is_dir(),
            "grammar source is missing at {}; run zess --grammar fetch {}",
            source.display(),
            grammar.grammar_id
        );
        helix_loader::grammar::build_grammar_library(grammar, None)
    })?;
    Ok(directory)
}

fn run_grammars(
    grammars: Vec<GrammarConfiguration>,
    action: &str,
    limit: usize,
    run: impl Fn(GrammarConfiguration) -> Result<()> + Sync,
) -> Result<()> {
    let workers = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(limit)
        .min(grammars.len());
    let grammars = Mutex::new(VecDeque::from(grammars));
    let errors = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some(grammar) = grammars.lock().unwrap().pop_front() else {
                        break;
                    };
                    let name = grammar.grammar_id.clone();
                    println!("Grammar {action}: {name}");
                    if let Err(error) = run(grammar) {
                        errors.lock().unwrap().push(format!("{name}: {error:#}"));
                    }
                }
            });
        }
    });
    let mut errors = errors.into_inner().unwrap();
    errors.sort();
    ensure!(
        errors.is_empty(),
        "grammar {action} failed:\n{}",
        errors.join("\n")
    );
    Ok(())
}

fn grammar_configs(directory: &Path, names: &[String]) -> Result<Vec<GrammarConfiguration>> {
    #[derive(Deserialize)]
    struct Configuration {
        #[serde(rename = "use-grammars")]
        selection: Option<GrammarSelection>,
        grammar: Vec<GrammarConfiguration>,
    }

    let Configuration {
        selection,
        mut grammar,
    } = helix_loader::config::default_lang_config()
        .try_into()
        .context("parse bundled grammar configuration")?;
    grammar.push(GrammarConfiguration {
        grammar_id: "wisp".into(),
        source: GrammarSource::Local {
            path: directory
                .join("grammars/sources/wisp")
                .to_str()
                .context("Wisp grammar source path must be UTF-8")?
                .to_owned(),
        },
    });
    for name in names {
        ensure!(
            grammar.iter().any(|grammar| grammar.grammar_id == *name),
            "unknown grammar: {name}"
        );
    }
    grammar.retain(|grammar| {
        if !names.is_empty() {
            return names.contains(&grammar.grammar_id);
        }
        match &selection {
            Some(GrammarSelection::Only { only }) => only.contains(&grammar.grammar_id),
            Some(GrammarSelection::Except { except }) => !except.contains(&grammar.grammar_id),
            None => true,
        }
    });
    Ok(grammar)
}

fn extract_wisp(directory: &Path) -> Result<()> {
    let source = directory.join("grammars/sources/wisp");
    for (file, contents) in [
        (
            "src/parser.c",
            include_bytes!("../../../vendor/tree-sitter-wisp/src/parser.c").as_slice(),
        ),
        (
            "src/scanner.c",
            include_bytes!("../../../vendor/tree-sitter-wisp/src/scanner.c").as_slice(),
        ),
        (
            "src/tree_sitter/parser.h",
            include_bytes!("../../../vendor/tree-sitter-wisp/src/tree_sitter/parser.h").as_slice(),
        ),
        (
            "src/tree_sitter/alloc.h",
            include_bytes!("../../../vendor/tree-sitter-wisp/src/tree_sitter/alloc.h").as_slice(),
        ),
        (
            "src/tree_sitter/array.h",
            include_bytes!("../../../vendor/tree-sitter-wisp/src/tree_sitter/array.h").as_slice(),
        ),
        (
            "LICENSE",
            include_bytes!("../../../vendor/tree-sitter-wisp/LICENSE").as_slice(),
        ),
    ] {
        write_resource(&source.join(file), contents)?;
    }
    Ok(())
}

fn write_resource(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().context("resource path has no parent")?)
        .with_context(|| format!("create parent of {}", path.display()))?;
    fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(helix_stdx::path::normalize(std::path::absolute(path)?))
}
