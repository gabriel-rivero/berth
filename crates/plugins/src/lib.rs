//! berth plugin host: wasmtime component runtime for `berth:plugin` guests.
//!
//! Tier-1 ABI (layout commands + events): a plugin sees a read-only workspace
//! tab snapshot and may queue workspace renames, applied by the app after the
//! guest callback returns. Every plugin runs in its own `Store` sandbox.
//!
//! Canonical WIT: `wit/berth-plugin.wit` here; the guest crate keeps a synced
//! copy at `guest/wit/`.
use std::collections::HashMap;
use std::path::Path;
use wasmtime::Engine;
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "wit",
    world: "plugin",
});

/// Host-side state exposed to one plugin instance.
pub struct HostState {
    /// Read-only snapshot filled by the app right before each sync call.
    workspaces: Vec<berth::plugin::host::WorkspaceInfo>,
    tabs: Vec<berth::plugin::host::TabInfo>,
    /// Renames queued by the guest, drained after the callback returns.
    pending_renames: HashMap<u64, String>,
    /// WASI p2 state; guests are plain std shims that import clocks/io/cli.
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl berth::plugin::host::Host for HostState {
    fn get_workspaces(&mut self) -> Vec<berth::plugin::host::WorkspaceInfo> {
        self.workspaces.clone()
    }

    fn get_tabs(&mut self) -> Vec<berth::plugin::host::TabInfo> {
        self.tabs.clone()
    }

    fn set_workspace_label(&mut self, id: u64, label: String) {
        self.pending_renames.insert(id, label);
    }
}

/// One loaded plugin instance (component bound to its own sandbox store).
pub struct LoadedPlugin {
    store: Store<HostState>,
    exports: Plugin,
}

impl LoadedPlugin {
    /// Compile + instantiate `path` (a `.wasm` component file).
    pub fn load(engine: &Engine, path: &Path) -> anyhow::Result<Self> {
        let component = Component::from_file(engine, path)?;
        let mut store = Store::new(
            engine,
            HostState {
                workspaces: Vec::new(),
                tabs: Vec::new(),
                pending_renames: HashMap::new(),
                table: ResourceTable::new(),
                wasi: wasmtime_wasi::WasiCtxBuilder::new().build(),
            },
        );
        let mut linker = Linker::<HostState>::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)?;
        let exports = Plugin::instantiate(&mut store, &component, &linker)?;
        Ok(Self { store, exports })
    }

    /// Push a fresh layout snapshot and run the guest's `on-layout-change`.
    /// Returns the renames the plugin queued.
    pub fn sync(
        &mut self,
        workspaces: Vec<berth::plugin::host::WorkspaceInfo>,
        tabs: Vec<berth::plugin::host::TabInfo>,
    ) -> Vec<WorkspaceRename> {
        {
            let state = self.store.data_mut();
            state.workspaces = workspaces;
            state.tabs = tabs;
            state.pending_renames.clear();
        }
        let _ = self.exports.call_on_layout_change(&mut self.store);
        let state = self.store.data();
        state
            .pending_renames
            .iter()
            .map(|(&id, label)| WorkspaceRename {
                id,
                label: label.clone(),
            })
            .collect()
    }
}

/// A workspace rename a plugin queued during `on-layout-change`.
#[derive(Debug, Clone)]
pub struct WorkspaceRename {
    pub id: u64,
    pub label: String,
}

/// Discover + instantiate every `*.wasm` plugin in `dir`. Broken plugins are
/// reported and skipped, never fatal.
pub fn load_plugins(engine: &Engine, dir: &Path, log: &mut dyn FnMut(String)) -> Vec<LoadedPlugin> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wasm"))
        .collect();
    paths.sort();
    for path in paths {
        match LoadedPlugin::load(engine, &path) {
            Ok(plugin) => out.push(plugin),
            Err(e) => log(format!("plugin {} failed to load: {e}", path.display())),
        }
    }
    out
}

/// One engine per app; component-model on (guests compile to components).
pub fn plugin_engine() -> anyhow::Result<Engine> {
    let mut cfg = wasmtime::Config::new();
    cfg.wasm_component_model(true);
    Ok(Engine::new(&cfg)?)
}
