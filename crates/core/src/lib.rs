//! berth-core: layout tree (workspaces/tabs/panes), session model, config. Re-exports Config, Layout.
pub mod config;
pub mod keybinds;
pub mod layout;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),
}
