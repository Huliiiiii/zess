use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result, ensure};
use helix_loader::grammar::{GrammarConfiguration, GrammarSelection, GrammarSource};
use serde::Deserialize;
use zess_platform::{FileSystem, NativeFileSystem, NativePlatform, Platform};

#[cfg(test)]
mod tests;

const QUERIES: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/queries.rs"));

static DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Fetches and builds grammars using a host toolchain and its native resource APIs.
pub trait GrammarToolchain: Send + Sync {
    fn fetch(
        &self,
        directory: Option<&Path>,
        names: &[String],
        progress: &(dyn Fn(&str, &str) + Sync),
    ) -> Result<PathBuf>;
    fn build(
        &self,
        directory: Option<&Path>,
        names: &[String],
        progress: &(dyn Fn(&str, &str) + Sync),
    ) -> Result<PathBuf>;
}

/// Uses Helix's native Git, compiler, library, and resource handling.
/// The supplied filesystem handles Zess resource preparation; Helix's toolchain
/// reads and writes its source cache and libraries through native host APIs.
pub struct NativeGrammarToolchain {
    platform: Arc<dyn Platform>,
    filesystem: Arc<dyn FileSystem>,
}

impl NativeGrammarToolchain {
    pub fn new(platform: Arc<dyn Platform>, filesystem: Arc<dyn FileSystem>) -> Self {
        Self {
            platform,
            filesystem,
        }
    }
}

impl GrammarToolchain for NativeGrammarToolchain {
    fn fetch(
        &self,
        directory: Option<&Path>,
        names: &[String],
        progress: &(dyn Fn(&str, &str) + Sync),
    ) -> Result<PathBuf> {
        native_fetch(
            directory,
            names,
            self.platform.as_ref(),
            self.filesystem.as_ref(),
            progress,
        )
    }

    fn build(
        &self,
        directory: Option<&Path>,
        names: &[String],
        progress: &(dyn Fn(&str, &str) + Sync),
    ) -> Result<PathBuf> {
        native_build(
            directory,
            names,
            self.platform.as_ref(),
            self.filesystem.as_ref(),
            progress,
        )
    }
}

/// Selects Zess resources before the first language is loaded.
pub fn initialize(directory: Option<&Path>) -> Result<PathBuf> {
    initialize_with_filesystem(directory, &NativeFileSystem)
}

pub fn initialize_with_filesystem(
    directory: Option<&Path>,
    filesystem: &dyn FileSystem,
) -> Result<PathBuf> {
    initialize_with_platform(directory, &NativePlatform, filesystem)
}

/// Selects the process-wide resource directory for the native Helix provider.
pub fn initialize_with_platform(
    directory: Option<&Path>,
    platform: &dyn Platform,
    filesystem: &dyn FileSystem,
) -> Result<PathBuf> {
    let mut configured = DIRECTORY.lock().unwrap();
    if let Some(current) = configured.as_ref() {
        if let Some(directory) = directory {
            ensure!(
                absolute(directory, platform)? == *current,
                "syntax runtime is already initialized at {}",
                current.display()
            );
        }
        return Ok(current.clone());
    }

    let directory = directory
        .map(|path| absolute(path, platform))
        .unwrap_or_else(|| default_directory_with_platform(platform, filesystem))?;
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
    default_directory_with_filesystem(&NativeFileSystem)
}

pub fn default_directory_with_filesystem(filesystem: &dyn FileSystem) -> Result<PathBuf> {
    default_directory_with_platform(&NativePlatform, filesystem)
}

/// Resolves the native Helix resource directory using the supplied host context.
pub fn default_directory_with_platform(
    platform: &dyn Platform,
    filesystem: &dyn FileSystem,
) -> Result<PathBuf> {
    if let Some(directory) = platform.environment(std::ffi::OsStr::new("ZESS_RUNTIME")) {
        ensure!(!directory.is_empty(), "ZESS_RUNTIME must name a directory");
        return absolute(Path::new(&directory), platform);
    }
    let executable = platform.current_exe().context("locate Zess executable")?;
    let executable = match filesystem.canonicalize(&executable) {
        Ok(executable) => executable,
        // A running daemon can outlive an executable replaced during installation.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => executable,
        Err(error) => return Err(error).context("resolve Zess executable"),
    };
    let sibling = executable
        .parent()
        .context("executable has no parent directory")?
        .join("runtime");
    if filesystem
        .metadata(&sibling)
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Ok(sibling);
    }
    Ok(platform
        .user_dirs()
        .context("locate Zess cache directory")?
        .cache
        .join("zess/runtime"))
}

/// Fetches selected pinned grammar sources; an empty selection uses Helix's defaults.
pub fn fetch_grammars(directory: Option<&Path>, names: &[String]) -> Result<PathBuf> {
    fetch_grammars_with_filesystem(directory, names, &NativeFileSystem)
}

pub fn fetch_grammars_with_filesystem(
    directory: Option<&Path>,
    names: &[String],
    filesystem: &dyn FileSystem,
) -> Result<PathBuf> {
    native_fetch(directory, names, &NativePlatform, filesystem, &|_, _| {})
}

fn native_fetch(
    directory: Option<&Path>,
    names: &[String],
    platform: &dyn Platform,
    filesystem: &dyn FileSystem,
    progress: &(dyn Fn(&str, &str) + Sync),
) -> Result<PathBuf> {
    let directory = directory
        .map(|path| absolute(path, platform))
        .unwrap_or_else(|| default_directory_with_platform(platform, filesystem))?;
    let grammars = grammar_configs(&directory, names)?;
    let directory = initialize_with_platform(Some(&directory), platform, filesystem)?;
    run_grammars(grammars, "fetch", 8, progress, |grammar| {
        if grammar.grammar_id == "wisp" {
            extract_wisp(&directory, filesystem)
        } else {
            helix_loader::grammar::fetch_grammar_source(grammar)
        }
    })?;
    Ok(directory)
}

/// Builds selected cached grammars and installs bundled queries without networking.
pub fn build_grammars(directory: Option<&Path>, names: &[String]) -> Result<PathBuf> {
    build_grammars_with_filesystem(directory, names, &NativeFileSystem)
}

pub fn build_grammars_with_filesystem(
    directory: Option<&Path>,
    names: &[String],
    filesystem: &dyn FileSystem,
) -> Result<PathBuf> {
    native_build(directory, names, &NativePlatform, filesystem, &|_, _| {})
}

fn native_build(
    directory: Option<&Path>,
    names: &[String],
    platform: &dyn Platform,
    filesystem: &dyn FileSystem,
    progress: &(dyn Fn(&str, &str) + Sync),
) -> Result<PathBuf> {
    let directory = directory
        .map(|path| absolute(path, platform))
        .unwrap_or_else(|| default_directory_with_platform(platform, filesystem))?;
    let grammars = grammar_configs(&directory, names)?;
    let directory = initialize_with_platform(Some(&directory), platform, filesystem)?;
    filesystem
        .create_dir_all(&directory.join("grammars"))
        .context("create grammar directory")?;
    for (file, contents) in QUERIES {
        write_resource(&directory.join(file), contents, filesystem)?;
    }
    run_grammars(grammars, "build", 4, progress, |grammar| {
        if grammar.grammar_id == "wisp" {
            extract_wisp(&directory, filesystem)?;
            write_resource(
                &directory.join("grammars/LICENSE-wisp"),
                include_bytes!("../../../vendor/tree-sitter-wisp/LICENSE"),
                filesystem,
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
            filesystem
                .metadata(&source)
                .is_ok_and(|metadata| metadata.is_dir()),
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
    progress: &(dyn Fn(&str, &str) + Sync),
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
                    progress(action, &name);
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

fn extract_wisp(directory: &Path, filesystem: &dyn FileSystem) -> Result<()> {
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
        write_resource(&source.join(file), contents, filesystem)?;
    }
    Ok(())
}

fn write_resource(path: &Path, contents: &[u8], filesystem: &dyn FileSystem) -> Result<()> {
    if filesystem.read(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    filesystem
        .create_dir_all(path.parent().context("resource path has no parent")?)
        .with_context(|| format!("create parent of {}", path.display()))?;
    filesystem
        .write(path, contents)
        .with_context(|| format!("write {}", path.display()))
}

fn absolute(path: &Path, platform: &dyn Platform) -> Result<PathBuf> {
    Ok(helix_stdx::path::normalize(platform.absolute_path(path)?))
}
