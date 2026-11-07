//! Host-side roundtrip tests: load the built guest wasm with a manifest and
//! check the auto-namer demo plugin renames only default labels, publishes a
//! status segment, and is refused when the manifest does not grant a capability.
//!
//! Skips silently when the guest artifact has not been built yet (CI builds
//! it in a dedicated step; locally `cargo build -p berth-plugin-guest
//! --target wasm32-wasip2 --release`).
use std::path::{Path, PathBuf};

use berth_plugins::berth::plugin::host::{TabInfo, WorkspaceInfo};
use berth_plugins::{LoadedPlugin, plugin_engine};

fn guest_wasm() -> Option<PathBuf> {
    let wasm = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/guest/target/wasm32-wasip2/release/berth_plugin_guest.wasm"
    ));
    wasm.exists().then(|| wasm.to_path_buf())
}

/// Copy the guest to a scratch dir as `auto-namer.wasm` plus a manifest.
fn stage(wasm: &Path, manifest: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("berth-plugin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::copy(wasm, dir.join("auto-namer.wasm")).expect("copy wasm");
    std::fs::write(dir.join("auto-namer.toml"), manifest).expect("write manifest");
    dir
}

fn workspaces() -> Vec<WorkspaceInfo> {
    vec![
        WorkspaceInfo {
            id: 1,
            label: "ws 1".to_string(),
            active: true,
        },
        WorkspaceInfo {
            id: 2,
            label: "user-rename".to_string(),
            active: false,
        },
        WorkspaceInfo {
            id: 7,
            label: "ws 7".to_string(),
            active: false,
        },
    ]
}

fn tabs() -> Vec<TabInfo> {
    vec![TabInfo {
        id: 1,
        workspace_id: 1,
        label: "shell 1".to_string(),
        active: true,
    }]
}

#[test]
fn auto_namer_renames_default_labels_only() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("guest wasm artifact missing; skipping roundtrip test");
        return;
    };
    let dir = stage(
        &wasm,
        "name = \"auto-namer\"\ncapabilities = [\"rename\", \"status\"]\n",
        "full",
    );
    let engine = plugin_engine().expect("engine");
    let mut plugin = LoadedPlugin::load(&engine, &dir.join("auto-namer.wasm"))
        .expect("guest component should instantiate");
    assert!(plugin.capabilities().rename && plugin.capabilities().status);
    assert_eq!(plugin.name(), "auto-namer");

    let report = plugin.sync(workspaces(), tabs());
    assert!(report.error.is_none(), "guest trapped: {:?}", report.error);
    assert!(report.denials.is_empty(), "denials: {:?}", report.denials);
    assert_eq!(plugin.status(), "3 ws", "guest status segment");
    let mut names: Vec<(u64, String)> = report
        .renames
        .into_iter()
        .map(|r| (r.id, r.label))
        .collect();
    // Plugin-internal pending-rename ordering isn't stable; compare as a set.
    names.sort();
    assert_eq!(
        names,
        vec![(1, "ws-01".to_string()), (7, "ws-07".to_string())]
    );

    // Second sync: labels are now plugin-shaped, so no renames should fire.
    let workspaces = vec![
        WorkspaceInfo {
            id: 1,
            label: "ws-01".to_string(),
            active: true,
        },
        WorkspaceInfo {
            id: 7,
            label: "ws-07".to_string(),
            active: false,
        },
    ];
    let report = plugin.sync(workspaces, vec![]);
    assert!(
        report.renames.is_empty(),
        "idempotent: {:?}",
        report.renames
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_rename_capability_denies_renames_but_keeps_status() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("guest wasm artifact missing; skipping roundtrip test");
        return;
    };
    let dir = stage(&wasm, "capabilities = [\"status\"]\n", "status-only");
    let engine = plugin_engine().expect("engine");
    let mut plugin = LoadedPlugin::load(&engine, &dir.join("auto-namer.wasm"))
        .expect("guest component should instantiate");
    assert!(!plugin.capabilities().rename);
    assert!(plugin.capabilities().status);

    let report = plugin.sync(workspaces(), tabs());
    assert!(report.error.is_none(), "guest trapped: {:?}", report.error);
    assert!(
        report.renames.is_empty(),
        "rename must be ignored: {:?}",
        report.renames
    );
    assert_eq!(
        report.denials,
        vec!["needs 'rename' capability".to_string()]
    );
    assert_eq!(plugin.status(), "3 ws");

    let _ = std::fs::remove_dir_all(&dir);
}
