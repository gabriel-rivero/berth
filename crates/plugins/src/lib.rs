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

/// Fuel granted to one `on-layout-change` call. Instrumented guests trap after
/// this much work, so `loop {}` (or accidental O(n²)) can no longer freeze the
/// UI; the guest is then logged and skipped.
const FUEL_PER_SYNC: u64 = 50_000_000;

/// Guest linear-memory ceiling (a runaway guest cannot balloon the host RSS).
const GUEST_MEMORY_LIMIT: usize = 64 << 20;

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
    /// Status segment the guest published this run (shown in the tab strip).
    status: String,
    /// Theme accent exposed to guests as `#rrggbb`.
    accent: String,
    /// WASI p2 state; guests are plain std shims that import clocks/io/cli.
    table: ResourceTable,
    wasi: WasiCtx,
    /// Caps guest memory growth (fuel limits CPU, this limits RSS).
    limits: wasmtime::StoreLimits,
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

    fn set_status(&mut self, text: String) {
        // Bound the payload: guests should publish short segments, and this
        // text is laid out in the tab strip every frame.
        self.status = text.chars().take(80).collect();
    }

    fn accent(&mut self) -> String {
        self.accent.clone()
    }
}

/// One loaded plugin instance (component bound to its own sandbox store).
pub struct LoadedPlugin {
    name: String,
    store: Store<HostState>,
    exports: Plugin,
    /// Set after a guest trap (fuel/memory/panic); a faulted plugin is skipped
    /// instead of being retried on every layout change.
    faulted: bool,
}

/// What one `on-layout-change` run produced.
#[derive(Default)]
pub struct SyncReport {
    pub renames: Vec<WorkspaceRename>,
    /// Trap message when the guest was stopped; the plugin is faulted then.
    pub error: Option<String>,
}

impl LoadedPlugin {
    /// Compile + instantiate `path` (a `.wasm` component file).
    pub fn load(engine: &Engine, path: &Path) -> anyhow::Result<Self> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let component = Component::from_file(engine, path)?;
        let mut store = Store::new(
            engine,
            HostState {
                workspaces: Vec::new(),
                tabs: Vec::new(),
                pending_renames: HashMap::new(),
                status: String::new(),
                accent: String::new(),
                table: ResourceTable::new(),
                wasi: wasmtime_wasi::WasiCtxBuilder::new().build(),
                limits: wasmtime::StoreLimitsBuilder::new()
                    .memory_size(GUEST_MEMORY_LIMIT)
                    .build(),
            },
        );
        store.limiter(|state| &mut state.limits);
        // Instantiation itself runs wasm (start functions, data segments), so the
        // store needs fuel before `Plugin::instantiate` or it traps at 0.
        store.set_fuel(FUEL_PER_SYNC)?;
        let mut linker = Linker::<HostState>::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)?;
        let exports = Plugin::instantiate(&mut store, &component, &linker)?;
        Ok(Self {
            name,
            store,
            exports,
            faulted: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    /// Status segment published during the last `sync` (empty if none).
    pub fn status(&self) -> &str {
        &self.store.data().status
    }

    /// Theme accent handed to the guest's `accent()` import.
    pub fn set_accent(&mut self, accent: &str) {
        self.store.data_mut().accent = accent.to_string();
    }

    /// Push a fresh layout snapshot and run the guest's `on-layout-change`.
    /// Returns the renames the plugin queued and any trap (which faults it).
    pub fn sync(
        &mut self,
        workspaces: Vec<berth::plugin::host::WorkspaceInfo>,
        tabs: Vec<berth::plugin::host::TabInfo>,
    ) -> SyncReport {
        if self.faulted {
            return SyncReport::default();
        }
        {
            let state = self.store.data_mut();
            state.workspaces = workspaces;
            state.tabs = tabs;
            state.pending_renames.clear();
            // A segment is only shown while the guest keeps republishing it.
            state.status.clear();
        }
        self.store.set_fuel(FUEL_PER_SYNC).ok();
        let result = self.exports.call_on_layout_change(&mut self.store);
        let renames = {
            let state = self.store.data();
            state
                .pending_renames
                .iter()
                .map(|(&id, label)| WorkspaceRename {
                    id,
                    label: label.clone(),
                })
                .collect()
        };
        let error = match result {
            Ok(()) => None,
            Err(e) => {
                self.faulted = true;
                Some(format!("{e:#}"))
            }
        };
        SyncReport { renames, error }
    }
}

/// A workspace rename a plugin queued during `on-layout-change`.
#[derive(Debug, Clone)]
pub struct WorkspaceRename {
    pub id: u64,
    pub label: String,
}

/// Discover + instantiate every enabled `*.wasm` plugin in `dir`. Broken
/// plugins are reported and skipped, never fatal. `disabled` holds file stems
/// switched off in `[plugins]` config.
pub fn load_plugins(
    engine: &Engine,
    dir: &Path,
    disabled: &[&str],
    log: &mut dyn FnMut(String),
) -> Vec<LoadedPlugin> {
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
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if disabled.contains(&stem) {
            log(format!("plugin {stem} disabled by config, skipping"));
            continue;
        }
        match LoadedPlugin::load(engine, &path) {
            Ok(plugin) => out.push(plugin),
            Err(e) => log(format!("plugin {} failed to load: {e}", path.display())),
        }
    }
    out
}

/// One engine per app; component-model on (guests compile to components),
/// fuel metering on so a runaway guest traps instead of freezing the UI.
pub fn plugin_engine() -> anyhow::Result<Engine> {
    let mut cfg = wasmtime::Config::new();
    cfg.wasm_component_model(true);
    cfg.consume_fuel(true);
    Ok(Engine::new(&cfg)?)
}
