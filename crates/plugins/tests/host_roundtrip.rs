//! Host-side roundtrip test: loads the built guest wasm and asserts the
//! auto-namer demo plugin renames default workspaces only.
//!
//! Skips silently when the guest artifact has not been built yet (CI builds
//! it in a dedicated step; locally `cargo build -p berth-plugin-guest
//! --target wasm32-wasip2 --release`).
use berth_plugins::berth::plugin::host::{TabInfo, WorkspaceInfo};
use berth_plugins::{LoadedPlugin, plugin_engine};

#[test]
fn auto_namer_renames_default_labels_only() {
    let wasm = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/guest/target/wasm32-wasip2/release/berth_plugin_guest.wasm"
    );
    if !std::path::Path::new(wasm).exists() {
        eprintln!("guest wasm artifact missing; skipping roundtrip test");
        return;
    }
    let engine = plugin_engine().expect("engine");
    let mut plugin = LoadedPlugin::load(&engine, std::path::Path::new(wasm))
        .expect("guest component should instantiate");

    let workspaces = vec![
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
    ];
    let tabs = vec![TabInfo {
        id: 1,
        workspace_id: 1,
        label: "shell 1".to_string(),
        active: true,
    }];

    let renames = plugin.sync(workspaces, tabs);
    let names: Vec<(u64, String)> = renames.into_iter().map(|r| (r.id, r.label)).collect();
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
    let renames = plugin.sync(workspaces, vec![]);
    assert!(renames.is_empty(), "idempotent: {renames:?}");
}
