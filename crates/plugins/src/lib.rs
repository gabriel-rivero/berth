//! berth plugin host: wasmtime component runtime for `berth:plugin` guests.
//!
//! Tier-1 ABI (layout commands + events): a plugin sees a read-only workspace
//! tab snapshot and may queue workspace renames, applied by the app after the
//! guest callback returns. Every plugin runs in its own `Store` sandbox.
//!
//! Canonical WIT: `wit/berth-plugin.wit` here; the guest crate keeps a synced
//! copy at `guest/wit/`.
use anyhow::Context;
use serde::Deserialize;
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

/// What a plugin declared it may do, read from its `<stem>.toml` manifest.
///
/// The host enforces these on every callback: without `rename` a guest cannot
/// change workspace labels, without `status` it cannot publish a tab-strip
/// segment. A plugin with no manifest gets no capabilities (and is logged).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub rename: bool,
    pub status: bool,
}

impl Capabilities {
    pub fn is_empty(self) -> bool {
        !self.rename && !self.status
    }

    /// Human-readable list for the plugin manager ("rename, status" / "none").
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        if self.rename {
            parts.push("rename");
        }
        if self.status {
            parts.push("status");
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Parsed plugin manifest (`<stem>.toml` beside the `.wasm`).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Manifest {
    /// Display name override; defaults to the wasm file stem.
    name: Option<String>,
    /// Declared capabilities: `"rename"`, `"status"`.
    capabilities: Vec<String>,
}

impl Manifest {
    fn capabilities(&self) -> anyhow::Result<Capabilities> {
        let mut caps = Capabilities::default();
        for cap in &self.capabilities {
            match cap.as_str() {
                "rename" => caps.rename = true,
                "status" => caps.status = true,
                other => anyhow::bail!("unknown capability {other:?} (expected rename|status)"),
            }
        }
        Ok(caps)
    }
}

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
    /// Capabilities granted by the plugin manifest.
    caps: Capabilities,
    /// Calls a guest made without the matching capability (drained per sync).
    denials: Vec<String>,
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
        if !self.caps.rename {
            self.deny("rename");
            return;
        }
        self.pending_renames.insert(id, label);
    }

    fn set_status(&mut self, text: String) {
        if !self.caps.status {
            self.deny("status");
            return;
        }
        // Bound the payload: guests should publish short segments, and this
        // text is laid out in the tab strip every frame.
        self.status = text.chars().take(80).collect();
    }

    fn accent(&mut self) -> String {
        self.accent.clone()
    }
}

impl HostState {
    /// Record a capability denial once per callback so the app can surface it
    /// without a chatty guest producing hundreds of log lines.
    fn deny(&mut self, cap: &str) {
        let msg = format!("needs '{cap}' capability");
        if !self.denials.contains(&msg) {
            self.denials.push(msg);
        }
    }
}

/// One loaded plugin instance (component bound to its own sandbox store).
pub struct LoadedPlugin {
    /// File stem (`auto-namer` for `auto-namer.wasm`); stable config key.
    stem: String,
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
    /// Calls the guest made without the declared capability, deduplicated.
    pub denials: Vec<String>,
}

impl LoadedPlugin {
    /// Compile + instantiate `path` (a `.wasm` component file).
    pub fn load(engine: &Engine, path: &Path) -> anyhow::Result<Self> {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed");
        // Capabilities are declared next to the component (`foo.toml` for
        // `foo.wasm`). No manifest = no capabilities, not full trust.
        let manifest_path = path.with_extension("toml");
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => toml::from_str::<Manifest>(&text)
                .with_context(|| format!("parsing {}", manifest_path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Manifest::default(),
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", manifest_path.display()));
            }
        };
        let caps = manifest
            .capabilities()
            .with_context(|| format!("validating {}", manifest_path.display()))?;
        let name = manifest.name.unwrap_or_else(|| stem.to_string());
        let component = Component::from_file(engine, path)?;
        let mut store = Store::new(
            engine,
            HostState {
                workspaces: Vec::new(),
                tabs: Vec::new(),
                pending_renames: HashMap::new(),
                status: String::new(),
                accent: String::new(),
                caps,
                denials: Vec::new(),
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
            stem: stem.to_string(),
            name,
            store,
            exports,
            faulted: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Config key / file stem (`[plugins] "<stem>" = false`).
    pub fn stem(&self) -> &str {
        &self.stem
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    /// Status segment published during the last `sync` (empty if none).
    pub fn status(&self) -> &str {
        &self.store.data().status
    }

    /// Capabilities the manifest granted (empty when it is missing).
    pub fn capabilities(&self) -> Capabilities {
        self.store.data().caps
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
        let denials = std::mem::take(&mut self.store.data_mut().denials);
        SyncReport {
            renames,
            error,
            denials,
        }
    }
}

/// A workspace rename a plugin queued during `on-layout-change`.
#[derive(Debug, Clone)]
pub struct WorkspaceRename {
    pub id: u64,
    pub label: String,
}

/// One `*.wasm` file found on disk, read without compiling it (cheap enough for
/// the plugin manager to rescan on every open).
#[derive(Debug, Clone)]
pub struct PluginSummary {
    pub stem: String,
    /// Manifest display name, else the file stem.
    pub name: String,
    pub capabilities: Capabilities,
    /// Manifest problems; the plugin would fail to load until fixed.
    pub manifest_error: Option<String>,
}

/// Discover plugin components in `dir` and read their manifests. Sorted by
/// stem; unreadable directories yield an empty list.
pub fn scan_plugins(dir: &Path) -> Vec<PluginSummary> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wasm"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unnamed")
                .to_string();
            let manifest_path = path.with_extension("toml");
            let (name, capabilities, manifest_error) = match std::fs::read_to_string(&manifest_path)
            {
                Ok(text) => match toml::from_str::<Manifest>(&text)
                    .map_err(|e| e.to_string())
                    .and_then(|m| {
                        m.capabilities()
                            .map(|c| (m.name, c))
                            .map_err(|e| e.to_string())
                    }) {
                    Ok((name, caps)) => (name.unwrap_or_else(|| stem.clone()), caps, None),
                    Err(e) => (stem.clone(), Capabilities::default(), Some(e)),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    (stem.clone(), Capabilities::default(), None)
                }
                Err(e) => (stem.clone(), Capabilities::default(), Some(e.to_string())),
            };
            PluginSummary {
                stem,
                name,
                capabilities,
                manifest_error,
            }
        })
        .collect()
}

/// What one discovery/load pass produced.
#[derive(Default)]
pub struct LoadReport {
    pub plugins: Vec<LoadedPlugin>,
    /// `(stem, message)` for components that failed to compile/instantiate.
    pub errors: Vec<(String, String)>,
}

/// Discover + instantiate every enabled `*.wasm` plugin in `dir`, keeping the
/// load errors for the plugin manager. Broken plugins are reported and skipped,
/// never fatal. `disabled` holds file stems switched off in `[plugins]` config.
pub fn load_plugins_report(
    engine: &Engine,
    dir: &Path,
    disabled: &[&str],
    log: &mut dyn FnMut(String),
) -> LoadReport {
    let mut report = LoadReport::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return report;
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
            .unwrap_or_default()
            .to_string();
        if disabled.contains(&stem.as_str()) {
            log(format!("plugin {stem} disabled by config, skipping"));
            continue;
        }
        match LoadedPlugin::load(engine, &path) {
            Ok(plugin) => {
                if plugin.capabilities().is_empty() {
                    log(format!(
                        "plugin {stem} declared no capabilities ({stem}.toml missing or empty); renames/status will be ignored"
                    ));
                }
                report.plugins.push(plugin)
            }
            Err(e) => {
                log(format!("plugin {} failed to load: {e}", path.display()));
                report.errors.push((stem, e.to_string()));
            }
        }
    }
    report
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
    load_plugins_report(engine, dir, disabled, log).plugins
}

/// One engine per app; component-model on (guests compile to components),
/// fuel metering on so a runaway guest traps instead of freezing the UI.
pub fn plugin_engine() -> anyhow::Result<Engine> {
    let mut cfg = wasmtime::Config::new();
    cfg.wasm_component_model(true);
    cfg.consume_fuel(true);
    Ok(Engine::new(&cfg)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_grants_only_declared_capabilities() {
        let manifest: Manifest =
            toml::from_str("name = \"auto-namer\"\ncapabilities = [\"rename\"]\n").unwrap();
        assert_eq!(manifest.name.as_deref(), Some("auto-namer"));
        let caps = manifest.capabilities().unwrap();
        assert!(caps.rename);
        assert!(!caps.status);
        assert!(!caps.is_empty());
    }

    #[test]
    fn manifest_without_entries_grants_nothing() {
        let caps = Manifest::default().capabilities().unwrap();
        assert!(caps.is_empty());
    }

    #[test]
    fn manifest_rejects_unknown_capability_and_unknown_keys() {
        let manifest: Manifest = toml::from_str("capabilities = [\"network\"]\n").unwrap();
        let err = manifest.capabilities().unwrap_err().to_string();
        assert!(err.contains("unknown capability"), "{err}");
        assert!(toml::from_str::<Manifest>("sandbox = true\n").is_err());
    }

    #[test]
    fn scan_reads_manifests_and_surfaces_broken_ones() {
        let dir = std::env::temp_dir().join(format!("berth-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auto-namer.wasm"), b"not wasm").unwrap();
        std::fs::write(
            dir.join("auto-namer.toml"),
            "name = \"Auto Namer\"\ncapabilities = [\"rename\", \"status\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("broken.wasm"), b"not wasm").unwrap();
        std::fs::write(dir.join("broken.toml"), "capabilities = [\"network\"]\n").unwrap();

        let found = scan_plugins(&dir);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].stem, "auto-namer");
        assert_eq!(found[0].name, "Auto Namer");
        assert_eq!(found[0].capabilities.label(), "rename, status");
        assert!(found[0].manifest_error.is_none());
        assert_eq!(found[1].stem, "broken");
        assert!(found[1].manifest_error.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
