//! berth binary: loads config, boots the UI event loop; panes and plugin host slot in later.
fn main() -> anyhow::Result<()> {
    // fail fast + early on broken config; the UI owns everything after this.
    let config = berth_core::config::Config::load()?;
    let _ = config; // used by ui/app in M1 wiring
    berth_ui::run()
}
