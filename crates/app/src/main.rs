//! berth binary: loads config, boots the UI event loop; panes and plugin host slot in later.
// No console host window on release builds (debug builds keep stderr for `cargo run`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
fn main() -> anyhow::Result<()> {
    if std::env::args_os().any(|arg| arg == "--version" || arg == "-V") {
        println!("berth {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::args_os().any(|arg| arg == "--gpu-info") {
        print!("{}", berth_ui::gpu_report());
        return Ok(());
    }
    // Config loading lives in berth_ui::run(): it logs failures and falls back
    // to defaults, which matters on Windows where release builds have no console.
    berth_ui::run()
}
