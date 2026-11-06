//! berth binary: loads config, boots the UI event loop; panes and plugin host slot in later.
// No console host window on release builds (debug builds keep stderr for `cargo run`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
fn main() -> anyhow::Result<()> {
    if std::env::args_os().any(|arg| arg == "--gpu-info") {
        print!("{}", berth_ui::gpu_report());
        return Ok(());
    }
    // fail fast + early on broken config; the UI owns everything after this.
    let config = berth_core::config::Config::load()?;
    let _ = config; // used by ui/app in M1 wiring
    berth_ui::run()
}
