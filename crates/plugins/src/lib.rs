//! berth plugin host: wasmtime engine + capability-gated host calls into the app.
//! Guests are `wasm32-wasip1` modules; per-plugin Store = per-plugin sandbox.
//! Design reference: Zellij's plugin API (wasmtime, host-owned permissions).

pub struct PluginHost {
    engine: wasmtime::Engine,
    // TODO(M4): registry of loaded modules + permission grants per plugin
    // (e.g. allowed: spawn tab in workspace, read layout snapshot, subscribe pane output).
}

impl PluginHost {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            engine: wasmtime::Engine::default(),
        })
    }

    /// Compile a guest module without instantiating it (validation happens here).
    pub fn load(&self, path: &str) -> anyhow::Result<()> {
        let bytes = std::fs::read(path)?;
        wasmtime::Module::new(&self.engine, bytes)?;
        // TODO(M4): instantiate into a fresh Store with a Linker exposing only granted
        // capabilities; module handles are resolved from the plugin dir + config.
        Ok(())
    }
}
