use std::path::PathBuf;

use steel::{SteelVal, rvals::Result, steel_vm::engine::Engine};

/// Creates script engines and compiles programs, including imported modules.
pub trait ScriptCompiler: Send + Sync {
    fn create_engine(&self) -> Engine;

    /// Compiles a program using `path` for relative imports and source locations.
    fn compile_and_run(
        &self,
        engine: &mut Engine,
        source: String,
        path: Option<PathBuf>,
    ) -> Result<Vec<SteelVal>>;
}

/// Uses Steel's native module paths, filesystem reads, and compiler caches.
/// Imports use the host filesystem independently of the runtime's `FileSystem`.
pub struct NativeSteelCompiler;

impl ScriptCompiler for NativeSteelCompiler {
    fn create_engine(&self) -> Engine {
        Engine::new_sandboxed()
    }

    fn compile_and_run(
        &self,
        engine: &mut Engine,
        source: String,
        path: Option<PathBuf>,
    ) -> Result<Vec<SteelVal>> {
        if let Some(path) = path {
            engine.compile_and_run_raw_program_with_path(source, path)
        } else {
            engine.compile_and_run_raw_program(source)
        }
    }
}
