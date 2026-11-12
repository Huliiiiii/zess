use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use bridge::json::Json;
use serde_json::Value;
use steel::{
    SteelVal,
    rerrs::{ErrorKind, SteelErr},
    rvals::{FromSteelVal, IntoSteelVal},
    steel_vm::engine::Engine,
};
use zess_platform::{FileSystem, NativeFileSystem};
use zess_plugin::{
    BufferBackend, BufferPluginHandle, CommandCatalog, CommandImplementation, Keymap,
    PluginRegistrar, Registry,
};
use zess_plugin::{BufferId, EditorOptions, HostRequest, PluginInput, PluginOutput, ViewId};
pub use zess_plugin::{
    BufferKindId, IdentityRegistry, KeyResolution, KeyScopeId, KeyState, ModeId, ModeRegistration,
};

mod bridge;
mod compiler;
mod registration;
mod sources;

pub use compiler::{NativeSteelCompiler, ScriptCompiler};

#[cfg(test)]
mod tests;

const STANDARD_MODULES: [(&str, &str); 7] = [
    (
        "zess/buffer",
        include_str!("../../../plugins/zess/buffer.w"),
    ),
    ("zess/keys", include_str!("../../../plugins/zess/keys.w")),
    ("zess/state", include_str!("../../../plugins/zess/state.w")),
    ("zess/ui", include_str!("../../../plugins/zess/ui.w")),
    (
        "zess/config",
        include_str!("../../../plugins/zess/config.w"),
    ),
    (
        "zess/status-layout",
        include_str!("../../../plugins/zess/status-layout.w"),
    ),
    (
        "zess/status",
        include_str!("../../../plugins/zess/status.w"),
    ),
];

#[derive(Clone)]
struct Handler {
    function: SteelVal,
}

#[derive(Clone)]
struct StatuslineRegion {
    handler: Handler,
    components: Vec<String>,
}

impl steel::rvals::Custom for StatuslineRegion {}

#[derive(Clone, Default)]
struct StatuslineLayout {
    left: Option<StatuslineRegion>,
    center: Option<StatuslineRegion>,
    right: Option<StatuslineRegion>,
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
    builtin_statuslines: BTreeSet<String>,
    statusline_components: BTreeMap<String, Handler>,
    statusline_layouts: BTreeMap<String, StatuslineLayout>,
    host: Box<dyn Fn(String, Value) -> Value>,
}

struct ModuleSource {
    path: PathBuf,
    source: String,
}

pub struct ScriptRuntime {
    filesystem: Arc<dyn FileSystem>,
    compiler: Arc<dyn ScriptCompiler>,
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
        Self::with_identities_and_filesystem(host, identities, Arc::new(NativeFileSystem))
    }

    pub fn with_identities_and_filesystem(
        host: impl Fn(String, Value) -> Value + 'static,
        identities: IdentityRegistry,
        filesystem: Arc<dyn FileSystem>,
    ) -> Result<Self> {
        Self::with_capabilities(host, identities, filesystem, Arc::new(NativeSteelCompiler))
    }

    /// Uses `filesystem` for configuration reads and module preloading.
    /// The compiler owns engine initialization and imported-module resolution.
    pub fn with_capabilities(
        host: impl Fn(String, Value) -> Value + 'static,
        identities: IdentityRegistry,
        filesystem: Arc<dyn FileSystem>,
        compiler: Arc<dyn ScriptCompiler>,
    ) -> Result<Self> {
        let mut engine = compiler.create_engine();
        bridge::register(&mut engine);
        let registry = Rc::new(RefCell::new(ScriptRegistry {
            plugins: Registry::with_identities(identities),
            builtin_statuslines: BTreeSet::new(),
            statusline_components: BTreeMap::new(),
            statusline_layouts: BTreeMap::new(),
            host: Box::new(host),
        }));
        let id = registration::register(&mut engine, &registry);

        engine.register_value("null", SteelVal::Void);
        let mut runtime = Self {
            filesystem,
            compiler,
            engine,
            states: BTreeMap::new(),
            registry,
            id,
            modules: BTreeMap::new(),
            sources: sources::Sources::default(),
        };

        runtime
            .compiler
            .compile_and_run(&mut runtime.engine, HELPERS.to_owned(), None)
            .map_err(script_error)
            .context("initialize script helpers")?;

        for (name, source) in STANDARD_MODULES {
            runtime.register_module_source(name, source, &PathBuf::from(format!("{name}.w")))?;
        }

        let result = runtime.compiler.compile_and_run(
            &mut runtime.engine,
            r#"(require "zess/buffer")
                   (require (prefix-in standard: "zess/status"))
                   (%register-builtin-statusline "default" standard:default-statusline)"#
                .to_owned(),
            None,
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
                   (require "zess/config")
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
            let result = self
                .compiler
                .compile_and_run(&mut self.engine, imports.to_owned(), None);
            self.sources.bind_pending(&self.engine);
            result
                .map_err(|error| self.source_error(error, Some(Path::new(path))))
                .with_context(|| format!("load builtin plugin {path}"))?;
        }
        Ok(())
    }

    pub fn load(&mut self, path: &Path) -> Result<()> {
        let source = self
            .filesystem
            .read_to_string(path)
            .with_context(|| format!("read plugin {}", path.display()))?;

        let path = self
            .filesystem
            .canonicalize(path)
            .with_context(|| format!("resolve plugin {}", path.display()))?;
        let (origin, scheme) = sources::SourceOrigin::translate(&source, &path)?;
        self.sources
            .register(sources::SourceIdentity::ProgramPath(path.clone()), origin);
        let result = self
            .compiler
            .compile_and_run(&mut self.engine, scheme, Some(path.clone()));
        self.sources.bind_pending(&self.engine);
        result
            .map_err(|error| self.source_error(error, Some(&path)))
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
        let mut entries = self
            .filesystem
            .read_dir(directory)
            .with_context(|| format!("read module directory {}", directory.display()))?;

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
                let source = self
                    .filesystem
                    .read_to_string(&path)
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

    pub fn dispatch(&mut self, kind: &str, input: &PluginInput) -> Result<PluginOutput> {
        let kind = self
            .identities()
            .buffer_kind(kind)
            .ok_or_else(|| anyhow!("unknown buffer kind: {kind}"))?;
        self.dispatch_by_id(kind, input)
    }

    pub fn dispatch_by_id(
        &mut self,
        kind: BufferKindId,
        input: &PluginInput,
    ) -> Result<PluginOutput> {
        let backend = self
            .registry
            .borrow()
            .plugins
            .buffer_by_id(kind)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "unknown buffer kind: {}",
                    self.identities().buffer_kind_name(kind)
                )
            })?;
        let handler = match backend {
            BufferBackend::External(handler) => handler,
            BufferBackend::Native(backend) => return backend.borrow_mut().update(input),
        };
        let state = self
            .states
            .entry(input.buffer_id)
            .or_insert_with(|| ScriptState {
                value: SteelVal::Void.as_rooted(),
                views: BTreeMap::new(),
            });
        if input.event == "view_cloned"
            && let Some(source) = input.argument["source_view"].as_u64()
            && let Some(source) = state.views.get(&source)
        {
            state.views.insert(
                input.view_id,
                ScriptView {
                    value: source.value.value().as_rooted(),
                    memories: source
                        .memories
                        .iter()
                        .map(|(mode, value)| (mode.clone(), value.value().as_rooted()))
                        .collect(),
                },
            );
        }
        let view = state
            .views
            .entry(input.view_id)
            .or_insert_with(|| ScriptView {
                value: SteelVal::Void.as_rooted(),
                memories: BTreeMap::new(),
            });
        let arguments = vec![
            bridge::input(input).map_err(script_error)?,
            state.value.value().clone(),
            view.value.value().clone(),
            view.memories
                .get(&input.mode)
                .map(|v| v.value().clone())
                .unwrap_or(SteelVal::Void),
        ];
        let input_value = self
            .engine
            .call_function_by_name_with_args("buffer-input", arguments)
            .map_err(|e| self.source_error(e, None))?;
        let result = self
            .engine
            .call_function_with_args(handler.function, vec![input_value])
            .map_err(|e| self.source_error(e, None))?
            .as_rooted();
        let unpacked = self
            .engine
            .call_function_by_name_with_args("unpack-buffer-result", vec![result.value().clone()])
            .map_err(|e| self.source_error(e, None))?;
        let values = Vec::<SteelVal>::from_steelval(&unpacked).map_err(script_error)?;
        let mut output = bridge::output(&values[3]).map_err(script_error)?;
        let state = self.states.get_mut(&input.buffer_id).unwrap();
        output.changed = state.value.value() != &values[0];
        state.value = values[0].as_rooted();
        let view = state.views.get_mut(&input.view_id).unwrap();
        view.value = values[1].as_rooted();
        view.memories
            .insert(input.mode.clone(), values[2].as_rooted());
        Ok(output)
    }

    pub fn is_native_buffer_id(&self, kind: BufferKindId) -> bool {
        matches!(
            self.registry.borrow().plugins.buffer_by_id(kind),
            Some(BufferBackend::Native(_))
        )
    }

    pub fn is_native_buffer(&self, kind: &str) -> bool {
        matches!(
            self.registry.borrow().plugins.buffer_backend(kind),
            Some(BufferBackend::Native(_))
        )
    }

    pub fn poll_operations(&mut self) -> Vec<PluginOutput> {
        let mut outputs = Vec::new();
        for backend in self.registry.borrow().plugins.buffer_backends() {
            if let BufferBackend::Native(backend) = backend {
                outputs.extend(backend.borrow_mut().poll_operations());
            }
        }
        outputs
    }

    pub fn remove_view(&mut self, view: ViewId) {
        for state in self.states.values_mut() {
            state.views.remove(&view);
        }
        for backend in self.registry.borrow().plugins.buffer_backends() {
            if let BufferBackend::Native(backend) = backend {
                backend.borrow_mut().remove_view(view);
            }
        }
    }

    pub fn remove_buffer_view(&mut self, buffer: BufferId, view: ViewId) {
        if let Some(state) = self.states.get_mut(&buffer) {
            state.views.remove(&view);
        }
        for backend in self.registry.borrow().plugins.buffer_backends() {
            if let BufferBackend::Native(backend) = backend {
                backend.borrow_mut().remove_buffer_view(buffer, view);
            }
        }
    }

    pub fn remove_buffer(&mut self, buffer: BufferId) {
        self.states.remove(&buffer);
        for backend in self.registry.borrow().plugins.buffer_backends() {
            if let BufferBackend::Native(backend) = backend {
                backend.borrow_mut().remove_buffer(buffer);
            }
        }
    }

    pub fn register_native_command(&mut self, id: &str, description: &str) {
        self.registry.borrow_mut().plugins.register_command(
            id,
            description,
            CommandImplementation::Native,
        );
    }

    pub fn register_native_action(&mut self, id: &str, description: &str) {
        self.registry.borrow_mut().plugins.register_action(
            id,
            description,
            CommandImplementation::Native,
        );
    }

    pub fn resolve_command_line(&self, name: &str) -> Result<String> {
        self.with_commands(|commands| {
            commands
                .resolve_command_line(name)
                .map(|command| command.id.clone())
                .ok_or_else(|| anyhow!("unknown command: {name}"))
        })
    }

    pub fn register_command_line(&mut self, id: &str, name: &str) {
        self.registry
            .borrow_mut()
            .plugins
            .register_command_line(id, name);
    }

    pub fn register_command_alias(&mut self, alias: &str, name: &str) {
        self.registry
            .borrow_mut()
            .plugins
            .register_command_alias(alias, name);
    }

    pub fn command(&mut self, name: &str, argument: &Value) -> Result<CommandOutput> {
        let (name, implementation) = {
            let registry = self.registry.borrow();
            let (name, implementation) = registry.plugins.lookup_command(name)?;
            (name.to_owned(), implementation.clone())
        };

        match implementation {
            CommandImplementation::Native => Ok(CommandOutput::Request(HostRequest::Action {
                name,
                argument: argument.clone(),
            })),
            CommandImplementation::Request(request) => Ok(CommandOutput::Request(request)),
            CommandImplementation::External(handler) => {
                let argument = Json(argument.clone())
                    .into_steelval()
                    .map_err(script_error)?;
                let value = self
                    .engine
                    .call_function_with_args(handler.function, vec![argument])
                    .map_err(|error| self.source_error(error, None))?;
                if let Ok(request) = bridge::Request::from_steelval(&value) {
                    Ok(CommandOutput::Request(request.0))
                } else {
                    let value = Json::from_steelval(&value).map_err(script_error)?.0;
                    if let Ok(request) = serde_json::from_value(value.clone()) {
                        Ok(CommandOutput::Request(request))
                    } else {
                        Ok(CommandOutput::Value(value))
                    }
                }
            }
            CommandImplementation::BufferEvent { kind, event } => {
                Ok(CommandOutput::Request(HostRequest::BufferEvent {
                    kind,
                    event,
                    argument: argument.clone(),
                }))
            }
        }
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

    pub fn event(&mut self, name: &str, argument: &Value) -> Result<Value> {
        let handler = self
            .registry
            .borrow()
            .plugins
            .events
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown event: {name}"))?;

        self.call(handler, argument)
            .with_context(|| format!("event {name}"))
    }

    pub fn has_event(&self, name: &str) -> bool {
        self.registry.borrow().plugins.events.contains_key(name)
    }

    pub fn editor_options(&self) -> EditorOptions {
        self.registry.borrow().plugins.editor_options
    }

    pub fn on_client_exit(&self) -> zess_plugin::OnClientExit {
        self.registry.borrow().plugins.on_client_exit
    }

    pub fn view_options(&self, kind: &str) -> Value {
        self.registry.borrow().plugins.view_options(kind)
    }

    pub fn validate_view_options(&self) -> Result<()> {
        self.registry.borrow().plugins.validate_view_options()
    }

    pub fn validate_statusline_layouts(&self) -> Result<()> {
        let registry = self.registry.borrow();
        for (kind, layout) in &registry.statusline_layouts {
            for region in [&layout.left, &layout.center, &layout.right]
                .into_iter()
                .flatten()
            {
                for component in &region.components {
                    if !registry.statusline_components.contains_key(component) {
                        anyhow::bail!("unknown statusline component {component} in layout {kind}");
                    }
                }
            }
        }
        Ok(())
    }

    pub fn validate_view_options_value(&self, options: &Value) -> Result<()> {
        self.registry
            .borrow()
            .plugins
            .validate_view_options_value(options)
    }

    pub fn format_statusline(
        &mut self,
        name: &str,
        context: &zess_plugin::StatusContext,
    ) -> Result<zess_plugin::StatusLine> {
        let (handler, layout) = {
            let registry = self.registry.borrow();
            let formatter = registry
                .plugins
                .statuslines
                .get(name)
                .ok_or_else(|| anyhow!("unknown statusline: {name}"))?;
            let layout = if registry.builtin_statuslines.contains(name) {
                registry.statusline_layouts.get(&context.kind).cloned()
            } else {
                None
            };
            (formatter.clone(), layout)
        };
        (|| -> Result<_> {
            let argument = bridge::status::context(context).map_err(script_error)?;
            let mut call = |handler: Handler| {
                self.engine
                    .call_function_with_args(handler.function, vec![argument.clone()])
                    .map_err(|error| self.source_error(error, None))
            };
            let mut line = if layout.as_ref().is_some_and(|layout| {
                layout.left.is_some() && layout.center.is_some() && layout.right.is_some()
            }) {
                zess_plugin::StatusLine {
                    left: Vec::new(),
                    center: Vec::new(),
                    right: Vec::new(),
                }
            } else {
                bridge::status::line(&call(handler)?)?
            };
            if let Some(layout) = layout {
                for (spans, region) in [
                    (&mut line.left, layout.left),
                    (&mut line.center, layout.center),
                    (&mut line.right, layout.right),
                ] {
                    if let Some(region) = region {
                        *spans = bridge::status::region(Some(&call(region.handler)?))?;
                    }
                }
            }
            Ok(line)
        })()
        .with_context(|| format!("statusline {name}"))
    }

    fn call(&mut self, handler: Handler, argument: &Value) -> Result<Value> {
        let argument = Json(argument.clone())
            .into_steelval()
            .map_err(script_error)?;
        let result = self
            .engine
            .call_function_with_args(handler.function, vec![argument]);

        let result = result.map_err(|error| self.source_error(error, None))?;

        Json::from_steelval(&result)
            .map(|value| value.0)
            .map_err(|error| self.source_error(error, None))
    }

    fn source_error(&self, error: SteelErr, fallback: Option<&Path>) -> anyhow::Error {
        self.sources.error(&self.engine, error, fallback)
    }

    pub fn commands(&self) -> Vec<String> {
        self.with_commands(|commands| commands.iter().map(|command| command.id.clone()).collect())
    }

    pub fn with_commands<R>(&self, use_commands: impl FnOnce(&CommandCatalog) -> R) -> R {
        use_commands(self.registry.borrow().plugins.catalog())
    }

    pub fn with_keymap<R>(&self, use_keymap: impl FnOnce(&Keymap) -> R) -> R {
        use_keymap(&self.registry.borrow().plugins.keymap)
    }

    pub fn buffer_kinds(&self) -> Vec<String> {
        let registry = self.registry.borrow();
        registry
            .plugins
            .buffer_kinds()
            .map(|kind| {
                registry
                    .plugins
                    .identities()
                    .buffer_kind_name(kind)
                    .to_string()
            })
            .collect()
    }

    pub fn has_buffer_kind(&self, kind: &str) -> bool {
        self.registry
            .borrow()
            .plugins
            .buffer_backend(kind)
            .is_some()
    }

    /// Heavy context fields requested by a buffer kind for the given event.
    pub fn context_fields(&self, kind: &str, event: &str) -> Vec<String> {
        self.registry.borrow().plugins.context_fields(kind, event)
    }

    pub fn identities(&self) -> IdentityRegistry {
        self.registry.borrow().plugins.identities().clone()
    }

    pub fn resolve_keys_ids(
        &self,
        scopes: &[KeyScopeId],
        mode: ModeId,
        keys: &[String],
    ) -> KeyResolution {
        self.with_keymap(|keymap| match keymap.resolve_ids(scopes, mode, keys) {
            KeyResolution::Command(command) => KeyResolution::Command(command.to_owned()),
            KeyResolution::Prefix => KeyResolution::Prefix,
            KeyResolution::Unbound => KeyResolution::Unbound,
        })
    }

    pub fn resolve_keys(&self, scopes: &[&str], mode: &str, keys: &[String]) -> KeyResolution {
        self.with_keymap(|keymap| match keymap.resolve(scopes, mode, keys) {
            KeyResolution::Command(command) => KeyResolution::Command(command.to_owned()),
            KeyResolution::Prefix => KeyResolution::Prefix,
            KeyResolution::Unbound => KeyResolution::Unbound,
        })
    }

    pub fn advance_key(
        &self,
        state: &mut KeyState,
        scopes: &[&str],
        mode: &str,
        key: &str,
    ) -> KeyResolution {
        self.with_keymap(|keymap| match keymap.advance(state, scopes, mode, key) {
            KeyResolution::Command(command) => KeyResolution::Command(command.to_owned()),
            KeyResolution::Prefix => KeyResolution::Prefix,
            KeyResolution::Unbound => KeyResolution::Unbound,
        })
    }

    pub fn validate_bindings(&self) -> Result<()> {
        self.registry.borrow().plugins.validate_bindings()
    }

    pub fn validate_bindings_for(&self, scopes: &[&str], mode: &str) -> Result<()> {
        self.registry
            .borrow()
            .plugins
            .validate_bindings_for(scopes, mode)
    }

    pub fn modes(&self) -> Vec<ModeRegistration> {
        self.registry.borrow().plugins.modes().to_owned()
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
