//! berth demo guest plugin: auto-namer for workspaces (wasm32-wasip2).
//!
//! Renames default-named workspaces ("ws N") to zero-padded "ws-01" style
//! labels; user renames are respected (anything not matching the default
//! pattern is left alone).
//!
//! Build: cargo build --target wasm32-wasip2 --release
//! -> target/wasm32-wasip2/release/berth_plugin_guest.wasm; drop into the
//! berth config `plugins/` directory.
wit_bindgen::generate!({
    path: "wit/berth-plugin.wit",
    world: "plugin",
});

use crate::berth::plugin::host::{get_workspaces, set_workspace_label};

struct AutoNamer;

/// True while `label` is exactly berth's auto-generated "ws {n}" form.
fn is_default(label: &str) -> bool {
    let Some(rest) = label.strip_prefix("ws ") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

impl Guest for AutoNamer {
    // world-exported functions have no self receiver (WIT flat functions)
    fn on_layout_change() {
        for ws in get_workspaces() {
            if is_default(&ws.label) {
                let n: u32 = ws
                    .label
                    .strip_prefix("ws ")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(ws.id as u32);
                set_workspace_label(ws.id, &format!("ws-{n:02}"));
            }
        }
    }
}

export!(AutoNamer);
