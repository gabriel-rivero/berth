//! berth-emulation: one Pane = alacritty Term grid + a live PTY. Re-exports Pane, PaneEvent.
pub mod pane;

pub use pane::{Pane, PaneEvent};
