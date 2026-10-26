use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use steel::{
    SteelVal,
    rerrs::{ErrorKind, SteelErr},
    rvals::{FromSteelVal, IntoSteelVal},
    steel_vm::engine::Engine,
};
use zess_plugin::{
    BufferBackend, BufferPluginHandle, CommandCatalog, CommandImplementation, Keymap,
    PluginRegistrar, Registry,
};
use zess_plugin::{BufferId, EditorOptions, HostRequest, PluginInput, PluginOutput, ViewId};
pub use zess_plugin::{
    BufferKindId, IdentityRegistry, KeyResolution, KeyScopeId, ModeId, ModeRegistration,
};

mod bridge;
mod registration;
mod sources;

const STANDARD_MODULES: [(&str, &str); 5] = [
    (
        "zess/buffer",
        include_str!("../../../plugins/zess/buffer.w"),
    ),
    ("zess/keys", include_str!("../../../plugins/zess/keys.w")),
    ("zess/state", include_str!("../../../plugins/zess/state.w")),
    ("zess/ui", include_str!("../../../plugins/zess/ui.w")),
    (
        "zess/status",
        include_str!("../../../plugins/zess/status.w"),
    ),
];

#[derive(Clone)]
struct Handler {
    function: SteelVal,
}

pub enum CommandOutput {
    Request(HostRequest),
    Value(Value),
}

struct ScriptView {
    value: steel::RootedSteelVal,
    memories: BTreeMap<String, steel::RootedSteelVal>,
}

struct ScriptState {
    value: steel::RootedSteelVal,
    views: BTreeMap<ViewId, ScriptView>,
}

struct ScriptRegistry {
    plugins: Registry<Handler>,
    host: Box<dyn Fn(String, Value) -> Value>,
}

struct ModuleSource {
    path: PathBuf,
    source: String,
}

pub struct ScriptRuntime {
    engine: Engine,
    states: BTreeMap<BufferId, ScriptState>,
    registry: Rc<RefCell<ScriptRegistry>>,
    id: usize,
    modules: BTreeMap<String, ModuleSource>,
    sources: sources::Sources,
}

impl ScriptRuntime {
    pub fn new(host: impl Fn(String, Value) -> Value + 'static) -> Result<Self> {
        Self::with_identities(host, IdentityRegistry::default())
    }

    pub fn with_identities(
        host: impl Fn(String, Value) -> Value + 'static,
        identities: IdentityRegistry,
    ) -> Result<Self> {
        let mut engine = Engine::new_sandboxed();
        bridge::register(&mut engine);
        let registry = Rc::new(RefCell::new(ScriptRegistry {
            plugins: Registry::with_identities(identities),
            host: Box::new(host),
        }));
        let id = registration::register(&mut engine, &registry);

        engine.register_value("null", SteelVal::Void);
        let mut runtime = Self {
            engine,
            states: BTreeMap::new(),
            registry,
            id,
            modules: BTreeMap::new(),
            sources: sources::Sources::default(),
        };

        runtime
            .engine
            .run(HELPERS)
            .map_err(script_error)
            .context("initialize script helpers")?;

        for (name, source) in STANDARD_MODULES {
            runtime.register_module_source(name, source, &PathBuf::from(format!("{name}.w")))?;
        }

        let result = runtime.engine.run(
            r#"(require "zess/buffer")
                   (require (prefix-in standard: "zess/status"))
                   (register-statusline "default" standard:default-statusline)"#,
        );
        runtime.sources.bind_pending(&runtime.engine);
        result
            .map_err(|error| runtime.source_error(error, None))
            .context("initialize default statusline")?;

        Ok(runtime)
    }

    pub fn load_builtin_plugins(&mut self) -> Result<()> {
        for (name, path, source, imports) in [
            (
                "#%private/zess/defaults",
                "defaults.w",
                include_str!("../../../plugins/defaults.w"),
                r##"(require "zess/keys")
                   (require "zess/status")
                   (require "#%private/zess/defaults")"##,
            ),
            (
                "#%private/zess/outline",
                "outline.w",
                include_str!("../../../plugins/outline.w"),
                r##"(require-builtin zess/filter)
                   (require "zess/keys")
                   (require "zess/buffer")
                   (require "zess/ui")
                   (require "#%private/zess/outline")"##,
            ),
        ] {
            self.register_module_source(name, source, Path::new(path))?;
            let result = self.engine.run(imports);
            self.sources.bind_pending(&self.engine);
            result
                .map_err(|error| self.source_error(error, Some(Path::new(path))))
                .with_context(|| format!("load builtin plugin {path}"))?;
        }
        Ok(())
    }

    pub fn load(&mut self, path: &Path) -> Result<()> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read plugin {}", path.display()))?;

        let path = std::fs::canonicalize(path)
            .with_context(|| format!("resolve plugin {}", path.display()))?;
        let (origin, scheme) = sources::SourceOrigin::translate(&source, &path)?;
        self.sources
            .register(sources::SourceIdentity::ProgramPath(path.clone()), origin);
        let result = self
            .engine
            .compile_and_run_raw_program_with_path(scheme, path.clone());
        self.sources.bind_pending(&self.engine);
        result
            .map_err(|error| self.source_error(error, Some(&path)))
            .with_context(|| format!("load plugin {}", path.display()))?;
        Ok(())
    }

    pub fn load_source(&mut self, source: &str, path: &Path) -> Result<()> {
        let (origin, source) = sources::SourceOrigin::translate(source, path)?;
        self.sources.register(
            sources::SourceIdentity::ProgramPath(path.to_path_buf()),
            origin,
        );
        let result = self
            .engine
            .compile_and_run_raw_program_with_path(source, path.to_path_buf());

        self.sources.bind_pending(&self.engine);
        result
            .map_err(|error| self.source_error(error, Some(path)))
            .with_context(|| format!("load plugin {}", path.display()))?;

        Ok(())
    }

    /// Preload importable source modules from an explicit host directory.
    /// Modules can be imported as `(require "helpers/path")`.
    /// Symlinks are not followed while preloading.
    pub fn preload_modules(&mut self, directory: &Path) -> Result<()> {
        self.preload_directory(directory, directory)
    }

    fn preload_directory(&mut self, root: &Path, directory: &Path) -> Result<()> {
        let mut entries = std::fs::read_dir(directory)
            .with_context(|| format!("read module directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;

        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let file_type = entry.file_type()?;
            let path = entry.path();

            if file_type.is_dir() {
                self.preload_directory(root, &path)?;
            } else if file_type.is_file()
                && path.extension().is_some_and(|extension| {
                    extension == "wisp" || extension == "w" || extension == "scm"
                })
            {
                let name = path
                    .strip_prefix(root)?
                    .with_extension("")
                    .to_string_lossy()
                    .replace('\\', "/");
                let source = std::fs::read_to_string(&path)
                    .with_context(|| format!("read module {}", path.display()))?;

                self.register_module_source(&name, &source, &path)?;
            }
        }

        Ok(())
    }

    /// Register a host-supplied module for imports by name.
    pub fn register_module_source(&mut self, name: &str, source: &str, path: &Path) -> Result<()> {
        if let Some(previous) = self.modules.get(name) {
            if previous.source == source {
                return Ok(());
            }

            return Err(anyhow!(
                "duplicate module {name}: {} conflicts with {}",
                path.display(),
                previous.path.display()
            ));
        }

        let (origin, scheme) = sources::SourceOrigin::translate(source, path)?;
        self.sources.register(
            sources::SourceIdentity::Module(name.to_owned()),
            origin.module(),
        );
        self.engine.register_steel_module(name.to_owned(), scheme);
        self.modules.insert(
            name.to_owned(),
            ModuleSource {
                path: path.to_path_buf(),
                source: source.to_owned(),
            },
        );

        Ok(())
    }

    pub fn register_native_module(&mut self, module: steel::steel_vm::builtin::BuiltInModule) {
        self.engine.register_module(module);
    }

    pub fn api<A: IntoSteelVal, R: FromSteelVal>(&mut self, name: &str, argument: A) -> Result<R> {
        let handler = self
            .registry
            .borrow()
            .plugins
            .apis
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown script API: {name}"))?;

        let argument = argument.into_steelval().map_err(script_error)?;
        let result = self
            .engine
            .call_function_with_args(handler.function, vec![argument])
            .map_err(|error| self.source_error(error, None))?;
        R::from_steelval(&result)
            .map_err(script_error)
            .with_context(|| format!("script API {name}"))
    }

    fn source_error(&self, error: SteelErr, fallback: Option<&Path>) -> anyhow::Error {
        self.sources.error(&self.engine, error, fallback)
    }
}

impl Drop for ScriptRuntime {
    fn drop(&mut self) {
        registration::remove(self.id);
    }
}

fn script_error(error: SteelErr) -> anyhow::Error {
    anyhow!(error.to_string())
}

const HELPERS: &str = r#"
(define (obj . entries) (apply hash entries))
(define (array . entries) (list->vector entries))
(define (register-command name description-or-handler . handler)
  (if (null? handler)
      (%register-command name "" description-or-handler)
      (%register-command name description-or-handler (car handler))))
(define (register-action name description-or-handler . handler)
  (if (null? handler)
      (%register-action name "" description-or-handler)
      (%register-action name description-or-handler (car handler))))
(define (register-context kind field . events)
  (%register-context kind field (if (null? events) (array) (car events))))
(define (get object key . fallback)
  (if (and (hash? object) (hash-contains? object key))
      (hash-ref object key)
      (if (null? fallback) null (car fallback))))
(define %script-apis (hash))
(define %script-events (hash))
(define (%script-name name) (if (string? name) (string->symbol name) name))
(define (register-api name handler)
  (%register-api name handler)
  (set! %script-apis (hash-insert %script-apis (%script-name name) handler)))
(define (api-call name argument)
  ((hash-ref %script-apis (%script-name name)) argument))
(define (register-event name handler)
  (%register-event name handler)
  (set! %script-events (hash-insert %script-events (%script-name name) handler)))
(define (event-emit name argument)
  ((hash-ref %script-events (%script-name name)) argument))
"#;

impl PluginRegistrar for ScriptRuntime {
    fn view_defaults(&mut self, kind: &str, options: serde_json::Map<String, Value>) {
        self.registry
            .borrow_mut()
            .plugins
            .view_defaults(kind, options);
    }

    fn scope(&mut self, id: &str, label: &str) {
        self.registry.borrow_mut().plugins.scope(id, label);
    }

    fn buffer(&mut self, kind: &str, backend: BufferPluginHandle) {
        self.registry.borrow_mut().plugins.buffer(kind, backend);
    }

    fn command(&mut self, name: &str, description: &str, request: HostRequest) {
        self.registry
            .borrow_mut()
            .plugins
            .command(name, description, request);
    }

    fn buffer_command(&mut self, name: &str, description: &str, kind: &str, event: &str) {
        self.registry
            .borrow_mut()
            .plugins
            .buffer_command(name, description, kind, event);
    }

    fn binding(&mut self, scope: &str, mode: &str, keys: &[&str], command: &str) {
        self.registry
            .borrow_mut()
            .plugins
            .binding(scope, mode, keys, command);
    }

    fn mode(&mut self, kind: &str, name: &str) {
        self.registry.borrow_mut().plugins.mode(kind, name);
    }
}
