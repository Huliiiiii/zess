mod identity;
pub use identity::{BufferKindId, IdentityRegistry, KeyScopeId, ModeId};

mod command;
pub use command::{CommandCatalog, CommandImplementation, CommandInfo, CommandLineEntry};

mod keymap;
pub use keymap::{KeyResolution, KeyState, Keymap, ResolvedBinding};

mod registry;
pub use registry::{BufferBackend, ModeRegistration, Registry};

mod buffer;
pub use buffer::{
    ItemNavigation, ItemSelection, OperationCompletion, OperationResult, OutlineSymbol,
    PluginContext, PluginInput, PluginOutput, QueryWatch, SelectionChange, TextContext,
};

mod host;
pub use host::{
    FileSnapshot, HostRequest, Navigation, PreviewArea, PreviewContent, PreviewLayout, PreviewPane,
    SnapshotId, SnapshotPresentation, validate_register_write,
};

mod options;
pub use options::{DEFAULT_PREVIEW_DEBOUNCE, EditorOptions, OnClientExit};

mod status;
pub use status::{
    DEFAULT_STATUS_FORMATS, StatusContent, StatusContext, StatusLine, StatusPosition,
    default_status_text, validate_status_formats,
};

mod count;
pub use count::CommandCount;

pub type BufferId = u64;
pub type ViewId = u64;

use std::{cell::RefCell, rc::Rc};

use anyhow::Result;

/// A buffer backend owns its model and view-local plugin state. The host owns
/// selection, layout and rendering, for both native and scripted backends.
pub trait BufferPlugin {
    fn update(&mut self, input: &PluginInput) -> Result<PluginOutput>;

    fn poll_operations(&mut self) -> Vec<PluginOutput> {
        Vec::new()
    }

    fn remove_view(&mut self, _view: ViewId) {}

    fn remove_buffer_view(&mut self, _buffer: BufferId, _view: ViewId) {}

    fn remove_buffer(&mut self, _buffer: BufferId) {}
}

pub type BufferPluginHandle = Rc<RefCell<dyn BufferPlugin>>;

pub trait PluginRegistrar {
    fn buffer(&mut self, kind: &str, backend: BufferPluginHandle);
    fn view_defaults(&mut self, kind: &str, options: serde_json::Map<String, serde_json::Value>);
    fn scope(&mut self, id: &str, label: &str);
    fn command(&mut self, name: &str, description: &str, request: HostRequest);
    fn buffer_command(&mut self, name: &str, description: &str, kind: &str, event: &str);
    fn binding(&mut self, scope: &str, mode: &str, keys: &[&str], command: &str);
    fn mode(&mut self, kind: &str, name: &str);
}
