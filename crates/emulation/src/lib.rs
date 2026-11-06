//! berth-emulation: one Pane = alacritty Term grid + a live PTY. Re-exports Pane, PaneEvent.
pub mod pane;

pub use pane::{GridMatch, LinkSpan, Pane, PaneEvent, SelectionKind, is_safe_uri, url_at};
