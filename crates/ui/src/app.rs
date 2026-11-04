//! Window/eventloop: winit 0.30 ApplicationHandler + keyboard/clipboard/selection wiring.
//! M3: workspaces + sidebar — the layout tree (`berth-core::Layout`) decides geometry; live
//! `Pane`s are keyed by PaneId. Sidebar lists workspaces; tab strip + panes tile beside it.
use alacritty_terminal::term::TermMode;
use berth_core::config::{Backspace, Config};
use berth_core::keybinds::{Action, Chord, build_bindings};
use berth_core::layout::{Layout, PaneId, Rect, Tab, Workspace, WorkspaceId};
use berth_emulation::pane::{Pane, PaneEvent, TermPoint};
use berth_plugins::{LoadedPlugin, WorkspaceRename, plugin_engine};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    window::{Cursor, CursorIcon, Window, WindowId},
};

use crate::render::{Chrome, Renderer, sidebar_width, tab_index_at, workspace_index_at};

/// How many lines one wheel tick moves.
const WHEEL_LINES_PER_TICK: i32 = 3;

/// App-side diagnostics, same env gate + sink as render::dlog.
macro_rules! dlog_app {
    ($($arg:tt)*) => {
        if crate::render::debug_enabled() { crate::render::dlog_emit(&format!($($arg)*)); }
    };
}

/// Convert a pane-local viewport cell to a terminal-space selection point.
fn term_point(pane: &Pane, row: usize, col: usize) -> TermPoint {
    alacritty_terminal::term::viewport_to_point(
        pane.term().renderable_content().display_offset,
        alacritty_terminal::index::Point::new(row, alacritty_terminal::index::Column(col)),
    )
}

/// Encode one mouse event per the pane's active reporting modes.
///
/// SGR (1006): `CSI < {code+mods} ; {x} ; {y} M|m`. Legacy X10 (1000): bytes
/// `\x1b[M` + button/coords offset by 32 (1-based, capped at 223). Returns None
/// when the pane is not capturing mouse events.
fn mouse_event_bytes(
    mode: alacritty_terminal::term::TermMode,
    code: u8,
    row: usize,
    col: usize,
    pressed: bool,
) -> Option<Vec<u8>> {
    use alacritty_terminal::term::TermMode;
    if !mode
        .intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    {
        return None;
    }
    let (x, y) = (col + 1, row + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        // SGR carries modifier bits in the button field; the caller encodes
        // those into `code` before calling this.
        let action = if pressed { 'M' } else { 'm' };
        Some(format!("\x1b[{code};{x};{y}{action}").into_bytes())
    } else if x <= 223 && y <= 223 {
        // X10 release always reports "no button" (3); modifier bits are X10
        // extensions, so plain 3 keeps this compatible.
        let b = if pressed { 32 + code } else { 32 + 3 };
        Some(vec![0x1b, b'[', b'M', b, 32 + x as u8, 32 + y as u8])
    } else {
        None
    }
}

/// SGR mouse modifier bits (shift handled by callers as a selection bypass).
fn mouse_mod_bits(mods: ModifiersState) -> u8 {
    let mut bits = 0;
    if mods.shift_key() {
        bits |= 4;
    }
    if mods.alt_key() {
        bits |= 8;
    }
    if mods.control_key() {
        bits |= 16;
    }
    bits
}

pub struct Berth {
    /// live config (sidebar on/off + width; hot reload is a TODO).
    config: Config,
    /// mtime of config.toml at last seen change; poll trigger for hot reload.
    config_mtime: Option<std::time::SystemTime>,
    /// layout tree: geometry + focus; sidebar lists layout.workspaces.
    layout: Layout,
    window: Option<Window>,
    renderer: Option<Renderer>,
    /// Live panes keyed by PaneId; geometry for each lives in layout.tabs[].pane_rects.
    panes: HashMap<PaneId, Arc<Mutex<Pane>>>,
    proxy: EventLoopProxy<PaneEvent>,
    clipboard: Option<arboard::Clipboard>,
    modifiers: ModifiersState,
    /// Last cursor position in device px (for selection + wheel cell math).
    mouse: PhysicalPosition<f64>,
    /// True while left button held over a pane.
    dragging: bool,
    /// DFS index + axis of the split whose divider is being dragged (Some = dragging).
    divider_drag: Option<(usize, bool)>,
    /// True while dragging the sidebar's right edge to resize it.
    sidebar_drag: bool,
    /// Last press (time + zone) for double-click detection (tab bar / workspace bar).
    last_click: Option<(std::time::Instant, ClickZone)>,
    /// Set on first close attempt with live shells; second close (or esc-free
    /// timeout) actually exits.
    confirm_exit: Option<std::time::Instant>,
    /// Live sidebar rename (workspace + buffer), active while Some.
    rename_edit: Option<RenameEdit>,
    /// Pane currently receiving keys / selection events.
    focused_pane: PaneId,
    /// OSC title per pane (empty until the shell sets one; falls back to tab label).
    titles: HashMap<PaneId, String>,
    /// Compiled keybind table (chord -> action); rebuilt on config reload.
    bindings: HashMap<Chord, Action>,
    /// WASM plugins (tier-1 layout ABI), synced each tick.
    plugins: Vec<LoadedPlugin>,
    /// Set when the app decided to exit; about_to_wait stops the loop.
    exiting: bool,
}

/// Where the last mouse press landed — used for double-click-to-create.
#[derive(Debug, Clone, PartialEq)]
enum ClickZone {
    /// tab strip (slot or empty area right of the last tab)
    TabBar,
    /// sidebar, below the workspace entries (empty space)
    SidebarEmpty,
    /// sidebar workspace entry (index) — double-click starts rename
    SidebarEntry(usize),
}

/// In-progress sidebar rename (workspace id + current buffer text).
struct RenameEdit {
    ws: WorkspaceId,
    buf: String,
}

/// Double-click window (matches macOS/Windows defaults, generous on purpose).
const DOUBLE_CLICK_MS: u128 = 500;
/// px slack around the sidebar's right edge for the resize grab.
const SIDEBAR_EDGE_GRAB: f64 = 4.0;
/// Upper bound of wheel clicks forwarded per event (flood guard).
const MAX_WHEEL_CLICKS: u32 = 16;

impl Berth {
    /// Forward a mouse press/release to the hovered pane when it captured the
    /// mouse (DECSET 1000/1002/1003; SGR 1006 preferred, legacy X10 fallback).
    /// Returns true when the event was consumed by the pane. Shift+click always
    /// bypasses reporting so users can still select inside TUI apps.
    fn forward_mouse_button(&mut self, code: u8, pressed: bool, mods: ModifiersState) -> bool {
        if mods.shift_key() {
            return false;
        }
        let Some((id, row, col)) = self.pane_at_mouse() else {
            return false;
        };
        let Some(pane) = self.panes.get(&id) else {
            return false;
        };
        let bytes = {
            let guard = pane.lock().expect("pane poisoned");
            mouse_event_bytes(
                guard.term_mode(),
                code | mouse_mod_bits(mods),
                row,
                col,
                pressed,
            )
        };
        let Some(bytes) = bytes else {
            return false;
        };
        if pressed {
            self.focused_pane = id;
            if let Some(tab) = self.layout.active_tab_mut() {
                tab.focused = id;
            }
        }
        let _ = pane.lock().expect("pane poisoned").write(&bytes);
        dlog_app!(
            "mouse forward btn={code} pressed={pressed} pane={id:?} at [{row},{col}] bytes={:?}",
            String::from_utf8_lossy(&bytes)
        );
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    /// Forward the wheel to the hovered pane as button 64/65 clicks when the
    /// pane reports mouse events, or as arrow-key presses under alternate-scroll
    /// (xterm 1007 semantics). Returns true when consumed; otherwise the wheel
    /// scrolls the pane's own viewport.
    fn forward_mouse_wheel(&mut self, up: bool, clicks: u32, mods: ModifiersState) -> bool {
        if mods.shift_key() {
            return false;
        }
        let Some((id, row, col)) = self.pane_at_mouse() else {
            return false;
        };
        let Some(pane) = self.panes.get(&id) else {
            return false;
        };
        let clicks = clicks.min(MAX_WHEEL_CLICKS);
        let bytes = {
            let guard = pane.lock().expect("pane poisoned");
            let mode = guard.term_mode();
            let wheel_code = (if up { 64u8 } else { 65u8 }) | mouse_mod_bits(mods);
            if mode.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG) {
                Some(
                    (0..clicks)
                        .flat_map(move |_| {
                            mouse_event_bytes(mode, wheel_code, row, col, true)
                                .into_iter()
                                .flatten()
                                .chain(
                                    mouse_event_bytes(mode, wheel_code, row, col, false)
                                        .into_iter()
                                        .flatten(),
                                )
                                .collect::<Vec<u8>>()
                        })
                        .collect::<Vec<u8>>(),
                )
            } else if mode.contains(TermMode::ALTERNATE_SCROLL) {
                // xterm alternate-scroll: the wheel becomes arrow keys.
                let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
                Some(seq.repeat(clicks as usize))
            } else {
                None
            }
        };
        let Some(bytes) = bytes.filter(|b| !b.is_empty()) else {
            return false;
        };
        let _ = pane.lock().expect("pane poisoned").write(&bytes);
        dlog_app!(
            "wheel forward up={up} clicks={clicks} pane={id:?} bytes={}",
            bytes.len()
        );
        true
    }

    fn cell_metrics(&self) -> (f32, f32) {
        self.renderer
            .as_ref()
            .map(|r| (r.metrics().cell_w, r.metrics().cell_h))
            .unwrap_or((8.0, 16.0))
    }

    /// Window rect the layout tree tiles into: right of the sidebar, below the
    /// tab strip. Chrome heights scale with the window's scale factor.
    fn content_rect(&self) -> Rect {
        self.window
            .as_ref()
            .map(|w| w.inner_size())
            .map(|s| {
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0);
                let ch = Chrome::new(scale);
                let bar = ch.tab_bar_h.round() as u32;
                let sb = sidebar_width(
                    self.config.sidebar.show,
                    self.config.sidebar.width_px,
                    scale,
                )
                .round() as u32;
                Rect {
                    x: sb,
                    y: bar,
                    w: s.width.saturating_sub(sb),
                    h: s.height.saturating_sub(bar),
                }
            })
            .unwrap_or_default()
    }

    /// Propagate window size -> renderer surface + every pane grid.
    fn sync_size(&mut self) {
        if self.window.is_some() {
            self.sync_window_title();
        }
        let Some(window) = &self.window else { return };
        let Some(renderer) = &mut self.renderer else {
            return;
        };

        renderer.resize_surface(window);
        let (cell_w, cell_h) = (renderer.metrics().cell_w, renderer.metrics().cell_h);
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let area = self.content_rect();
        let rects = self.layout.active_tab().map(|t| t.pane_rects(area));
        if let Some(rects) = rects {
            for (id, rect) in rects {
                let Some(pane) = self.panes.get(&id) else {
                    continue;
                };
                let cols = (rect.w as f32 / cell_w).floor().max(1.0) as u16;
                let rows = (rect.h as f32 / cell_h).floor().max(1.0) as u16;
                let _ = pane.lock().expect("pane poisoned").resize(
                    cols,
                    rows,
                    cell_w as u16,
                    cell_h as u16,
                );
            }
        }
        window.request_redraw();
    }

    /// Pump every pane and redraw the whole layout.
    fn redraw(&mut self) {
        let area = self.content_rect();
        let Some(tab) = self.layout.active_tab() else {
            return;
        };
        let rect_map: HashMap<PaneId, Rect> = tab.pane_rects(area).into_iter().collect();
        let dividers = tab.dividers(area);
        let self_titles = &self.titles;
        let tabs: Vec<(&str, bool)> = self
            .layout
            .active_workspace()
            .map(|ws| {
                ws.tabs
                    .iter()
                    .map(|t| {
                        let label = self_titles
                            .get(&t.focused)
                            .map(String::as_str)
                            .unwrap_or(t.label.as_str());
                        (label, t.id == ws.focused_tab)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Rename editor: show buffer + caret for the edited workspace entry.
        let rename_ws = self.rename_edit.as_ref().map(|r| r.ws);
        let edit_label = self.rename_edit.as_ref().map(|r| format!("{}_", r.buf));
        let workspaces: Vec<(&str, bool)> = self
            .layout
            .workspaces
            .iter()
            .map(|ws| {
                let label = edit_label
                    .as_deref()
                    .filter(|_| rename_ws == Some(ws.id))
                    .unwrap_or(ws.label.as_str());
                (label, ws.id == self.layout.active)
            })
            .collect();
        let sb_w = sidebar_width(
            self.config.sidebar.show,
            self.config.sidebar.width_px,
            self.window
                .as_ref()
                .map(|w| w.scale_factor())
                .unwrap_or(1.0),
        );
        let focus = self
            .layout
            .active_tab()
            .map(|t| t.pane_rects(area))
            .and_then(|rects| rects.into_iter().find(|(id, _)| *id == self.focused_pane))
            .map(|(_, rect)| rect);

        let mut guards = Vec::new();
        for (id, rect) in rect_map {
            if let Some(pane) = self.panes.get(&id) {
                let mut guard = pane.lock().expect("pane poisoned");
                guard.pump();
                guards.push((guard, rect));
            }
        }
        let spans = guards
            .iter()
            .map(|(g, r)| (g.term(), *r))
            .collect::<Vec<_>>();

        let failed = match (self.renderer.as_mut(), self.window.as_ref()) {
            (Some(r), Some(w)) => {
                let confirm: Option<&str> = self
                    .confirm_exit
                    .as_ref()
                    .map(|_| "Live shells running — close again to quit (esc to cancel)");
                r.draw(
                    &spans,
                    &dividers,
                    &tabs,
                    &workspaces,
                    sb_w,
                    focus,
                    confirm,
                    w,
                )
                .is_err()
            }
            _ => false,
        };
        if failed {
            self.renderer = None; // surface lost hard; window close handles the rest
        }
    }

    /// Spawn a shell pane for `id` and register it in the pane map.
    fn spawn_pane(&mut self, id: PaneId, cwd: Option<&str>) -> bool {
        let proxy = self.proxy.clone();
        match Pane::spawn(
            id,
            cwd,
            Arc::new(move |ev| {
                let _ = proxy.send_event(ev);
            }),
        ) {
            Ok(pane) => {
                if self.panes.insert(id, Arc::new(Mutex::new(pane))).is_some() {
                    panic!("duplicate PaneId {id:?}");
                }
                true
            }
            Err(e) => {
                crate::render::dlog_emit(&format!("pane spawn failed: {e}"));
                false
            }
        }
    }

    /// Split the focused pane: `vertical` = stacked top/bottom, else side by side.
    fn split_focused(&mut self, vertical: bool) {
        let (cell_w, cell_h) = self.cell_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let new_id = self.layout.alloc_pane_id();
        let Some((target, cwd)) = self.layout.active_tab().map(|t| (t.focused, t.cwd.clone()))
        else {
            return;
        };
        if !self
            .layout
            .active_tab_mut()
            .map(|t| t.split_pane(target, new_id, vertical))
            .unwrap_or(false)
        {
            return;
        }
        if !self.spawn_pane(new_id, cwd.as_deref()) {
            // roll the tree back so geometry stays consistent
            let _ = self.layout.active_tab_mut().map(|t| t.remove_pane(new_id));
            return;
        }
        self.focused_pane = new_id;
        if let Some(tab) = self.layout.active_tab_mut() {
            tab.focused = new_id;
        }
        // Size the new pane immediately (sync_size would only land next redraw).
        if let Some(area) = Some(self.content_rect()) {
            let rect = self.layout.active_tab().and_then(|t| {
                t.pane_rects(area)
                    .into_iter()
                    .find(|(i, _)| *i == new_id)
                    .map(|(_, r)| r)
            });
            if let Some(rect) = rect {
                let area = rect;
                let cols = ((area.w as f32 / cell_w).floor().max(1.0) as u16).max(1);
                let rows = ((area.h as f32 / cell_h).floor().max(1.0) as u16).max(1);
                let _ = self
                    .panes
                    .get(&new_id)
                    .unwrap()
                    .lock()
                    .expect("pane poisoned")
                    .resize(cols, rows, cell_w as u16, cell_h as u16);
            }
        }
        self.sync_size();
    }

    /// Spawn a fresh workspace with one shell tab and focus it.
    fn new_workspace(&mut self) {
        let (cell_w, cell_h) = self.cell_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let ws_id = self.layout.alloc_workspace_id();
        let tab_id = self.layout.alloc_tab_id();
        let pane_id = self.layout.alloc_pane_id();
        let ws = Workspace {
            id: ws_id,
            label: format!("ws {}", ws_id.0 + 1),
            tabs: vec![Tab::new(tab_id, pane_id, None)],
            focused_tab: tab_id,
        };
        self.layout.workspaces.push(ws);
        self.layout.active = ws_id;
        if self.spawn_pane(pane_id, None) {
            self.focused_pane = pane_id;
            self.sync_size();
        } else {
            // keep the tree consistent when the shell could not start
            self.layout.workspaces.pop();
            self.layout.active = self
                .layout
                .workspaces
                .last()
                .map(|ws| ws.id)
                .unwrap_or(WorkspaceId(0));
            self.focused_pane = self
                .layout
                .active_tab()
                .map(|t| t.focused)
                .unwrap_or(pane_id);
        }
    }

    /// Switch workspaces with wrap-around; the new workspace's focused pane
    /// receives keys and its panes resize.
    fn cycle_workspace(&mut self, delta: isize) {
        if self.layout.cycle_workspace(delta).is_none() {
            return;
        }
        if let Some(tab) = self.layout.active_tab() {
            self.focused_pane = tab.focused;
        }
        self.sync_size();
    }

    /// Focus the workspace at slot `index` of the sidebar (click).
    fn select_workspace(&mut self, index: usize) {
        let Some(ws) = self.layout.workspaces.get(index) else {
            return;
        };
        let ws_id = ws.id;
        let pane_id = ws.focused_tab().map(|t| t.focused);
        self.layout.active = ws_id;
        if let Some(pane_id) = pane_id {
            self.focused_pane = pane_id;
        }
        self.sync_size();
    }

    /// Start editing a workspace's sidebar label in place.
    fn begin_rename(&mut self, index: usize) {
        let Some(ws) = self.layout.workspaces.get(index) else {
            return;
        };
        self.rename_edit = Some(RenameEdit {
            ws: ws.id,
            // Select-all semantics: type replaces the label; Enter on an
            // empty buffer keeps the old one.
            buf: String::new(),
        });
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Key interception while the rename editor is up. Consumes everything.
    fn rename_chord(&mut self, event: &KeyEvent) -> bool {
        if self.rename_edit.is_none() {
            return false;
        }
        // 0 = keep, 1 = commit, 2 = cancel, 3 = change buf.
        enum Rm {
            Keep,
            Commit,
            Cancel,
        }
        use winit::keyboard::{Key, NamedKey};
        let mut action = Rm::Keep;
        if let Some(edit) = &mut self.rename_edit {
            match &event.logical_key {
                Key::Named(NamedKey::Enter)
                | Key::Named(NamedKey::ArrowDown) // keys that would move focus
                => {
                    action = Rm::Commit;
                }
                Key::Named(NamedKey::Escape) => action = Rm::Cancel,
                Key::Named(NamedKey::Backspace) => {
                    edit.buf.pop();
                }
                Key::Named(NamedKey::Space) => edit.buf.push(' '),
                _ => {
                    if let Some(txt) = event
                        .text
                        .as_ref()
                        .and_then(|t| t.chars().next())
                        .filter(|c| !c.is_control())
                    {
                        edit.buf.push(txt);
                    }
                }
            }
        }
        match action {
            Rm::Keep => {}
            Rm::Commit => {
                let (ws, buf) = match self.rename_edit.take() {
                    Some(e) => (e.ws, e.buf),
                    None => return true,
                };
                let label = buf.trim();
                if !label.is_empty()
                    && let Some(ws) = self.layout.workspaces.iter_mut().find(|w| w.id == ws)
                {
                    dlog_app!("rename ws {:?} -> {label:?}", ws.id);
                    ws.label = label.to_string();
                }
            }
            Rm::Cancel => self.rename_edit = None,
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    /// True when two presses land in `zone` within the double-click window;
    /// always records the press (a matched pair resets the tracker).
    fn double_click(&mut self, zone: ClickZone) -> bool {
        let now = std::time::Instant::now();
        let dbl = self.last_click.as_ref().is_some_and(|(t, z)| {
            *z == zone && now.duration_since(*t).as_millis() <= DOUBLE_CLICK_MS
        });
        self.last_click = if dbl { None } else { Some((now, zone)) };
        dbl
    }

    /// Spawn a fresh shell tab in the active workspace and focus it.
    fn new_tab(&mut self) {
        let (cell_w, cell_h) = self.cell_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let tab_id = self.layout.alloc_tab_id();
        let pane_id = self.layout.alloc_pane_id();
        {
            let Some(ws) = self.layout.active_workspace_mut() else {
                return;
            };
            ws.tabs.push(Tab::new(tab_id, pane_id, None));
            ws.focused_tab = tab_id;
        }
        if self.spawn_pane(pane_id, None) {
            self.focused_pane = pane_id;
            self.sync_size();
        } else {
            // keep the tree consistent when the shell could not start
            if let Some(ws) = self.layout.active_workspace_mut() {
                ws.remove_tab(tab_id);
            }
        }
    }

    /// Switch tabs with wrap-around; each tab's panes resize when focused.
    fn cycle_tab(&mut self, delta: isize) {
        if self
            .layout
            .active_workspace_mut()
            .and_then(|ws| ws.cycle_tab(delta))
            .is_none()
        {
            return;
        }
        if let Some(tab) = self.layout.active_tab() {
            self.focused_pane = tab.focused;
        }
        self.sync_size();
    }

    /// Focus the tab at slot `index` of the active workspace (tab bar click).
    fn select_tab(&mut self, index: usize) {
        let Some(ws) = self.layout.active_workspace_mut() else {
            return;
        };
        let Some((tab_id, pane_id)) = ws.tabs.get(index).map(|t| (t.id, t.focused)) else {
            return;
        };
        ws.focused_tab = tab_id;
        self.focused_pane = pane_id;
        self.sync_size();
    }

    /// Move keyboard focus to `pane` (alt+arrow navigation and click focus).
    fn focus_pane(&mut self, pane: PaneId) {
        self.focused_pane = pane;
        if let Some(tab) = self.layout.active_tab_mut() {
            tab.focused = pane;
        }
        self.sync_window_title();
        self.window.as_ref().inspect(|w| w.request_redraw());
    }

    /// Mirror the focused pane's OSC title into the OS window title.
    fn sync_window_title(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        match self.titles.get(&self.focused_pane) {
            Some(title) => window.set_title(format!("berth — {title}").as_str()),
            None => window.set_title("berth"),
        }
    }

    /// Kill the focused pane's shell; the Closed event does tree cleanup.
    fn close_focused_pane(&mut self) {
        if let Some(pane) = self.panes.get(&self.focused_pane) {
            dlog_app!("close pane {:?}", self.focused_pane);
            pane.lock().expect("pane poisoned").kill();
        }
    }

    /// Kill every pane in the active workspace; the Closed events walk the tree
    /// through the normal cleanup path (tab -> workspace -> exit when all gone).
    fn close_workspace(&mut self) {
        let Some(ws) = self.layout.active_workspace() else {
            return;
        };
        let mut ids: Vec<PaneId> = Vec::new();
        for tab in &ws.tabs {
            ids.extend(tab.pane_ids_in_order());
        }
        dlog_app!("close workspace {:?} panes={ids:?}", ws.id);
        for id in ids {
            if let Some(pane) = self.panes.get(&id) {
                pane.lock().expect("pane poisoned").kill();
            }
        }
    }

    /// Mirror hover/drag state into the OS cursor icon (resize arrows on both
    /// sides of resize targets so the affordance exists without any chrome).
    fn update_cursor(&self) {
        let Some(window) = &self.window else {
            return;
        };
        let scale = window.scale_factor();
        let ch = Chrome::new(scale);
        let sb_w = sidebar_width(
            self.config.sidebar.show,
            self.config.sidebar.width_px,
            scale,
        )
        .round() as i32;
        let on_sidebar_edge = (self.mouse.x - f64::from(sb_w)).abs() <= SIDEBAR_EDGE_GRAB * scale;
        let on_divider = if self.mouse.y >= f64::from(ch.tab_bar_h) && !on_sidebar_edge {
            let x = self.mouse.x.max(0.0) as u32;
            let y = self.mouse.y.max(0.0) as u32;
            self.layout
                .active_tab()
                .and_then(|t| t.grab_index_at(x, y, self.content_rect()))
        } else {
            None
        };
        let icon = if self.sidebar_drag || on_sidebar_edge {
            CursorIcon::EwResize
        } else if let Some((_, vertical)) = self.divider_drag.or(on_divider) {
            if vertical {
                CursorIcon::NsResize
            } else {
                CursorIcon::EwResize
            }
        } else {
            CursorIcon::Default
        };
        window.set_cursor(Cursor::Icon(icon));
    }

    /// Kill every pane of the tab at slot `index` of the active workspace; the
    /// Closed event cascade removes the now-empty tab (event if it is not focused).
    fn close_tab(&mut self, index: usize) {
        let Some(tab) = self
            .layout
            .active_workspace()
            .and_then(|ws| ws.tabs.get(index))
        else {
            return;
        };
        let ids = tab.pane_ids_in_order();
        dlog_app!("close tab slot{index} {:?} panes={ids:?}", tab.id);
        for id in ids {
            if let Some(pane) = self.panes.get(&id) {
                pane.lock().expect("pane poisoned").kill();
            }
        }
    }

    /// Kill every pane of the workspace at sidebar slot `index`; the Closed
    /// cascade empties its tabs, then drops the workspace.
    fn close_workspace_at(&mut self, index: usize) {
        let Some(ws) = self.layout.workspaces.get(index) else {
            return;
        };
        let mut ids: Vec<PaneId> = Vec::new();
        for tab in &ws.tabs {
            ids.extend(tab.pane_ids_in_order());
        }
        dlog_app!("close workspace {} panes={ids:?}", ws.id.0);
        for id in ids {
            if let Some(pane) = self.panes.get(&id) {
                pane.lock().expect("pane poisoned").kill();
            }
        }
    }

    fn set_clipboard(&mut self, text: String) {
        if let Some(cb) = &mut self.clipboard {
            let _ = cb.set_text(text);
        }
    }

    fn get_clipboard(&mut self) -> Option<String> {
        self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
    }

    /// Locate the pane under `self.mouse`; returns (PaneId, row, col) pane-local.
    fn pane_at_mouse(&self) -> Option<(PaneId, usize, usize)> {
        let area = self.content_rect();
        let tab = self.layout.active_tab()?;
        let (cell_w, cell_h) = self.cell_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        for (id, rect) in tab.pane_rects(area) {
            let (rx, ry) = (rect.x as f64, rect.y as f64);
            if self.mouse.x < rx
                || self.mouse.y < ry
                || self.mouse.x >= rx + rect.w as f64
                || self.mouse.y >= ry + rect.h as f64
            {
                continue;
            }
            let local_x = (self.mouse.x - rx).max(0.0);
            let local_y = (self.mouse.y - ry).max(0.0);
            let col = ((local_x / cell_w as f64).floor() as usize)
                .min((rect.w as f32 / cell_w).floor().max(1.0) as usize - 1);
            let row = ((local_y / cell_h as f64).floor() as usize)
                .min((rect.h as f32 / cell_h).floor().max(1.0) as usize - 1);
            return Some((id, row, col));
        }
        None
    }

    /// Keys that act before the PTY. Returns true if consumed => encode_key skips it.
    /// Chord handling is table-driven (config [keybinds] -> berth_core::keybinds);
    /// alt+arrows pane focus stays hardcoded (it is not a user-facing action set).
    fn handle_chord(&mut self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let ctrl = mods.control_key();
        let shift = mods.shift_key();
        let alt = mods.alt_key();

        let named = event.logical_key.clone();
        dlog_app!(
            "chord: logical={named:?} ctrl={ctrl} shift={shift} alt={alt} super={}",
            mods.super_key()
        );

        // Quit-confirm banner: escape cancels the pending exit.
        if self.confirm_exit.is_some() {
            let name = chord_key_name(event);
            if name.as_deref() == Some("escape") {
                dlog_app!("quit confirm cancelled");
                self.confirm_exit = None;
                self.window.as_ref().inspect(|w| w.request_redraw());
            }
            return true;
        }

        // Sidebar rename editor consumes keys while it is up.
        if self.rename_chord(event) {
            return true;
        }

        // alt+arrows move pane focus within the tab (before table lookup).
        if alt {
            use berth_core::layout::FocusDir;
            let dir = match named {
                Key::Named(NamedKey::ArrowUp) => Some(FocusDir::Up),
                Key::Named(NamedKey::ArrowDown) => Some(FocusDir::Down),
                Key::Named(NamedKey::ArrowLeft) => Some(FocusDir::Left),
                Key::Named(NamedKey::ArrowRight) => Some(FocusDir::Right),
                _ => None,
            };
            if let Some(dir) = dir {
                if let Some(next) = self.layout.active_tab().and_then(|t| t.neighbor_pane(dir)) {
                    dlog_app!("alt-chord neighbor={next:?}");
                    self.focus_pane(next);
                }
                return true;
            }
        }

        // Table lookup: normalize this key event to a chord key name.
        if let Some(key_name) = chord_key_name(event) {
            let chord = Chord {
                ctrl,
                shift,
                alt,
                super_key: mods.super_key(),
                key: key_name.clone(),
            };
            if let Some(action) = self.bindings.get(&chord).copied() {
                dlog_app!("chord matched {:?}: {action:?}", chord);
                return self.run_action(action);
            }
            // Mac-style global modifier: unbound cmd combos never reach the
            // PTY (Terminal.app behaves the same for unhandled cmd chords).
            if mods.super_key() {
                dlog_app!("super chord unbound ({key_name}) -> consumed");
                return true;
            }
        }
        false
    }

    /// Execute a configured action; true = consumed.
    fn run_action(&mut self, action: Action) -> bool {
        match action {
            Action::SplitVertical => {
                self.split_focused(true);
                true
            }
            Action::SplitHorizontal => {
                self.split_focused(false);
                true
            }
            Action::NewTab => {
                self.new_tab();
                true
            }
            Action::ClosePane => {
                self.close_focused_pane();
                true
            }
            Action::CloseTab => {
                dlog_app!("action close-tab: click-only keybind ignored");
                false
            }
            Action::NextTab => {
                self.cycle_tab(1);
                true
            }
            Action::PrevTab => {
                self.cycle_tab(-1);
                true
            }
            Action::NewWorkspace => {
                self.new_workspace();
                true
            }
            Action::CloseWorkspace => {
                self.close_workspace();
                true
            }
            Action::NextWorkspace => {
                self.cycle_workspace(1);
                true
            }
            Action::PrevWorkspace => {
                self.cycle_workspace(-1);
                true
            }
            Action::Copy => {
                let text = self
                    .panes
                    .get(&self.focused_pane)
                    .and_then(|p| p.lock().expect("pane poisoned").selection_text());
                if let Some(text) = text {
                    self.set_clipboard(text);
                }
                true
            }
            Action::Paste => {
                if let (Some(text), Some(pane)) =
                    (self.get_clipboard(), self.panes.get(&self.focused_pane))
                {
                    // Bracketed paste (DECSET 2004): delimit so the app never
                    // executes pasted lines as if typed.
                    let payload = {
                        let mode = pane.lock().expect("pane poisoned").term_mode();
                        if mode.contains(TermMode::BRACKETED_PASTE) {
                            let mut p = Vec::with_capacity(text.len() + 8);
                            p.extend_from_slice(b"\x1b[200~");
                            p.extend_from_slice(text.as_bytes());
                            p.extend_from_slice(b"\x1b[201~");
                            p
                        } else {
                            text.as_bytes().to_vec()
                        }
                    };
                    let _ = pane.lock().expect("pane poisoned").write(&payload);
                }
                true
            }
            Action::ScrollPageUp | Action::ScrollPageDown => {
                let scroll = if action == Action::ScrollPageUp {
                    alacritty_terminal::grid::Scroll::PageUp
                } else {
                    alacritty_terminal::grid::Scroll::PageDown
                };
                if let Some(pane) = self.panes.get(&self.focused_pane) {
                    pane.lock().expect("pane poisoned").scroll_display(scroll);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                true
            }
        }
    }
}

/// Normalize a key event to the chord key-name space used in [keybinds]
/// (lowercase word for specials, single lowercase char for text keys).
fn chord_key_name(event: &KeyEvent) -> Option<String> {
    // Physical backslash: shift turns '"' on US layouts, but the chord is the key.
    if event.physical_key == PhysicalKey::Code(KeyCode::Backslash) {
        return Some("backslash".to_string());
    }
    match &event.logical_key {
        Key::Character(t) => {
            let l = t.to_lowercase();
            if l.chars().count() == 1 {
                Some(l)
            } else {
                None
            }
        }
        Key::Named(n) => Some(
            match n {
                NamedKey::Enter => "enter",
                NamedKey::Tab => "tab",
                NamedKey::Space => "space",
                NamedKey::Escape => "escape",
                NamedKey::Backspace => "backspace",
                NamedKey::ArrowUp => "up",
                NamedKey::ArrowDown => "down",
                NamedKey::ArrowLeft => "left",
                NamedKey::ArrowRight => "right",
                NamedKey::PageUp => "pageup",
                NamedKey::PageDown => "pagedown",
                NamedKey::Home => "home",
                NamedKey::End => "end",
                NamedKey::Insert => "insert",
                NamedKey::Delete => "delete",
                _ => return None,
            }
            .to_string(),
        ),
        _ => None,
    }
}

/// KeyDown -> PTY bytes (None = nothing to write).
fn encode_key(event: &KeyEvent, mods: ModifiersState, backspace: Backspace) -> Option<Vec<u8>> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let ctrl = mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    // CSI modifier param: 1 + shift*1 + alt*2 + ctrl*4.
    let csi_mod = 1 + shift as u8 + alt as u8 * 2 + ctrl as u8 * 4;
    let modified = shift || alt || ctrl;
    let csi = move |tail: &'static str| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{csi_mod}{tail}").into_bytes()
        } else {
            format!("\x1b[{tail}").into_bytes()
        }
    };

    let bytes: Option<Vec<u8>> = match &event.logical_key {
        Key::Named(named) => match named {
            NamedKey::Space => Some(vec![b' ']),
            NamedKey::Enter => Some(vec![b'\r']),
            NamedKey::Backspace => Some(match backspace {
                Backspace::Del => vec![0x7f],
                Backspace::Bs => vec![0x08],
            }),
            NamedKey::Tab => {
                if shift {
                    Some(b"\x1b[Z".to_vec())
                } else {
                    Some(vec![b'\t'])
                }
            }
            NamedKey::Escape => Some(vec![0x1b]),
            NamedKey::ArrowUp => Some(csi("A")),
            NamedKey::ArrowDown => Some(csi("B")),
            NamedKey::ArrowRight => Some(csi("C")),
            NamedKey::ArrowLeft => Some(csi("D")),
            NamedKey::Home => Some(csi("H")),
            NamedKey::End => Some(csi("F")),
            NamedKey::Insert => Some(csi("2~")),
            NamedKey::Delete => Some(csi("3~")),
            NamedKey::PageUp => Some(csi("5~")),
            NamedKey::PageDown => Some(csi("6~")),
            NamedKey::F1 => Some(vec![0x1b, b'O', b'P']),
            NamedKey::F2 => Some(vec![0x1b, b'O', b'Q']),
            NamedKey::F3 => Some(vec![0x1b, b'O', b'R']),
            NamedKey::F4 => Some(vec![0x1b, b'O', b'S']),
            NamedKey::F5 => Some(csi("15~")),
            NamedKey::F6 => Some(csi("17~")),
            NamedKey::F7 => Some(csi("18~")),
            NamedKey::F8 => Some(csi("19~")),
            NamedKey::F9 => Some(csi("20~")),
            NamedKey::F10 => Some(csi("21~")),
            NamedKey::F11 => Some(csi("23~")),
            NamedKey::F12 => Some(csi("24~")),
            _ => None,
        },
        Key::Character(text) => {
            let text = text.as_str();
            if ctrl {
                // Ctrl+letter / Ctrl+space / Ctrl+punct -> C0 code.
                let c = text.chars().next()?;
                match c.to_ascii_uppercase() {
                    'A'..='Z' => return Some(vec![(c.to_ascii_uppercase() as u8) & 0x1f]),
                    ' ' | '@' => Some(vec![0]),
                    '[' => Some(vec![0x1b]),
                    '\\' => Some(vec![0x1c]),
                    ']' => Some(vec![0x1d]),
                    '^' => Some(vec![0x1e]),
                    '_' => Some(vec![0x1f]),
                    _ => None,
                }
            } else {
                Some(text.as_bytes().to_vec())
            }
        }
        _ => None,
    };

    bytes.map(|mut b| {
        if alt {
            b.insert(0, 0x1b);
        }
        b
    })
}

/// App icon (window icon + taskbar) decoded from the bundled 128px asset.
fn load_window_icon() -> Option<winit::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../../../assets/berth-128.png");
    let rgba = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    winit::window::Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

impl ApplicationHandler<PaneEvent> for Berth {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("berth")
            .with_window_icon(load_window_icon())
            .with_min_inner_size(winit::dpi::LogicalSize::new(400.0, 200.0));
        let window = event_loop.create_window(attrs).expect("window creation");
        let font = &self.config.font;
        let renderer = Renderer::new(&window, &font.family, font.size_pt).expect("renderer init");

        self.window = Some(window);
        self.renderer = Some(renderer);

        let (first_id, cwd) = self
            .layout
            .active_tab()
            .map(|t| (t.focused, t.cwd.clone()))
            .unwrap_or((PaneId(0), None));
        self.spawn_pane(first_id, cwd.as_deref());
        self.focused_pane = first_id;
        self.sync_size();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: PaneEvent) {
        match event {
            PaneEvent::Output(id) => {
                if let Some(pane) = self.panes.get(&id) {
                    pane.lock().expect("pane poisoned").pump();
                }
                self.redraw();
            }
            PaneEvent::Title(id, title) => {
                dlog_app!("osc title pane={id:?} {title:?}");
                self.titles.insert(id, title);
                if self.focused_pane == id {
                    self.sync_window_title();
                }
                self.redraw();
            }
            PaneEvent::TitleReset(id) => {
                dlog_app!("osc title reset pane={id:?}");
                self.titles.remove(&id);
                if self.focused_pane == id {
                    self.sync_window_title();
                }
                self.redraw();
            }
            PaneEvent::Closed(id) => {
                let _ = self.panes.remove(&id);
                // The pane may belong to any tab of any workspace (close buttons
                // and close_workspace act on non-focused trees too), so walk all
                // of them: drop the pane from its tab, the tab when empty, the
                // workspace when empty; exit only when nothing remains.
                let mut empty_ws: Vec<WorkspaceId> = Vec::new();
                'outer: for ws in &mut self.layout.workspaces {
                    for tab in &mut ws.tabs {
                        // remove_pane() returns true when the tab's ROOT pane
                        // itself closed — the tree then holds a dangling leaf
                        // and the tab is definitively empty. Collapsed splits
                        // (non-root removal) need the pane-id recheck.
                        if tab.remove_pane(id) || tab.pane_ids_in_order().is_empty() {
                            dlog_app!("tab {:?} empty -> closing", tab.id);
                            let tab_id = tab.id;
                            ws.remove_tab(tab_id);
                            if ws.tabs.is_empty() {
                                dlog_app!("workspace {:?} empty -> closing", ws.id);
                                empty_ws.push(ws.id);
                            }
                            break 'outer;
                        }
                    }
                }
                for ws_id in empty_ws {
                    self.layout.remove_workspace(ws_id);
                }
                if self.layout.workspaces.is_empty() {
                    dlog_app!("all workspaces gone -> exit");
                    self.panes.clear();
                    self.layout = Layout::default();
                    event_loop.exit();
                    return;
                }
                if let Some(tab) = self.layout.active_tab()
                    && !tab.pane_ids_in_order().contains(&self.focused_pane)
                {
                    self.focused_pane = tab.focused;
                }
                self.sync_size();
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    self.sync_size();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Device-px cell size must follow the (possibly live) DPR change.
                if let Some(renderer) = &mut self.renderer {
                    let size_px = self.config.font.size_pt * 1.333_333 * scale_factor as f32;
                    // Font rebuild failure keeps the old strike (visible but
                    // stale-size chrome) instead of dropping the renderer.
                    if renderer.set_font(&self.config.font.family, size_px).is_ok() {
                        self.sync_size();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let consumed = event.state == ElementState::Pressed
                    && self.handle_chord(&event, self.modifiers);
                if !consumed
                    && let Some(bytes) =
                        encode_key(&event, self.modifiers, self.config.terminal.backspace)
                    && let Some(pane) = self.panes.get(&self.focused_pane)
                {
                    dlog_app!("kbd write: pane={:?} bytes={:?}", self.focused_pane, bytes);
                    let _ = pane.lock().expect("pane poisoned").write(&bytes);
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;
                if self.sidebar_drag {
                    // Drag x is device px; config stores logical px (scaled at draw).
                    let scale = self
                        .window
                        .as_ref()
                        .map(|w| w.scale_factor())
                        .unwrap_or(1.0);
                    let w = position.x.clamp(120.0 * scale, 720.0 * scale) / scale;
                    self.config.sidebar.width_px = w as f32;
                    dlog_app!("sidebar drag -> width {w}");
                    self.sync_size();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                } else if let Some((idx, _vertical)) = self.divider_drag {
                    let x = position.x.max(0.0) as u32;
                    let y = position.y.max(0.0) as u32;
                    let area = self.content_rect();
                    let mut resized = false;
                    if let Some(tab) = self.layout.active_tab_mut() {
                        resized = tab.resize_index(idx, x, y, area);
                    }
                    if resized {
                        self.sync_size();
                    }
                    dlog_app!("drag move ({x},{y}) resized={resized}");
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                } else if self.dragging
                    && let Some((id, row, col)) = self.pane_at_mouse()
                {
                    if let Some(pane) = self.panes.get(&id) {
                        let mut pane = pane.lock().expect("pane poisoned");
                        let pt = term_point(&pane, row, col);
                        pane.update_selection(pt);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                self.update_cursor();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    let scale = self
                        .window
                        .as_ref()
                        .map(|w| w.scale_factor())
                        .unwrap_or(1.0);
                    let ch = Chrome::new(scale);
                    let sb_w = sidebar_width(
                        self.config.sidebar.show,
                        self.config.sidebar.width_px,
                        scale,
                    )
                    .round() as i32;
                    let sb_w_f = f64::from(sb_w);
                    dlog_app!(
                        "mouse press at ({:?},{:?}) sb_w={sb_w} y_strip={}",
                        self.mouse.x,
                        self.mouse.y,
                        ch.tab_bar_h
                    );
                    if (self.mouse.x - sb_w_f).abs() <= SIDEBAR_EDGE_GRAB * scale {
                        // Sidebar edge grab wins over entry clicks: drag to resize.
                        dlog_app!("sidebar grab");
                        self.sidebar_drag = true;
                    } else if self.mouse.x < sb_w_f {
                        // Sidebar click: entries select or close (right edge of
                        // the entry is the close button); double-click below
                        // the entries creates a workspace.
                        let count = self.layout.workspaces.len();
                        if let Some(i) = workspace_index_at(self.mouse.y as f32, count, ch) {
                            if self.mouse.x >= sb_w_f - f64::from(ch.close_w) {
                                dlog_app!("sidebar close ws#{i}");
                                self.rename_edit = None;
                                self.close_workspace_at(i);
                            } else {
                                dlog_app!("sidebar click ws#{i}");
                                if self.double_click(ClickZone::SidebarEntry(i)) {
                                    dlog_app!("sidebar double-click -> rename ws#{i}");
                                    self.begin_rename(i);
                                } else {
                                    self.rename_edit = None;
                                    self.select_workspace(i);
                                }
                            }
                        } else if self.double_click(ClickZone::SidebarEmpty) {
                            dlog_app!("sidebar double-click -> new workspace");
                            self.new_workspace();
                        }
                    } else if self.mouse.y < f64::from(ch.tab_bar_h) {
                        // Tab strip: the right corner of a slot closes that tab;
                        // double-click on the focused tab (or empty area) opens
                        // a new tab; any other click selects.
                        let count = self
                            .layout
                            .active_workspace()
                            .map(|ws| ws.tabs.len())
                            .unwrap_or(0);
                        let focused_idx = self
                            .layout
                            .active_workspace()
                            .and_then(|ws| ws.tabs.iter().position(|t| t.id == ws.focused_tab));
                        let slot = tab_index_at(self.mouse.x as f32 - sb_w as f32, count, ch.tab_w);
                        let x_rel = self.mouse.x as f32 - sb_w as f32;
                        if let Some(i) = slot
                            && x_rel >= i as f32 * ch.tab_w + (ch.tab_w - ch.close_w)
                        {
                            dlog_app!("tab close slot{i}");
                            self.close_tab(i);
                        } else if self.double_click(ClickZone::TabBar)
                            && (slot.is_none() || focused_idx.is_none_or(|f| slot == Some(f)))
                        {
                            dlog_app!("tab bar double-click -> new tab");
                            self.new_tab();
                        } else if let Some(i) = slot {
                            self.select_tab(i);
                        }
                    } else {
                        // A TUI app that captured the mouse (1000/1002/1006)
                        // receives the press instead of pane selection.
                        if self.forward_mouse_button(0, true, self.modifiers) {
                            dlog_app!("press forwarded to pane");
                        } else {
                            // Divider grab wins over pane focus/selection.
                            let x = (self.mouse.x.max(0.0)) as u32;
                            let y = (self.mouse.y.max(0.0)) as u32;
                            let grab = self
                                .layout
                                .active_tab()
                                .and_then(|t| t.grab_index_at(x, y, self.content_rect()));
                            if let Some((idx, vertical)) = grab {
                                dlog_app!("divider grab #{idx} vertical={vertical} at ({x},{y})");
                                self.divider_drag = Some((idx, vertical));
                            } else {
                                self.dragging = true;
                                if let Some((id, row, col)) = self.pane_at_mouse() {
                                    self.focused_pane = id;
                                    if let Some(tab) = self.layout.active_tab_mut() {
                                        tab.focused = id;
                                    }
                                    if let Some(pane) = self.panes.get(&id) {
                                        let mut pane = pane.lock().expect("pane poisoned");
                                        let pt = term_point(&pane, row, col);
                                        pane.start_selection(pt);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                ElementState::Released => {
                    self.dragging = false;
                    self.divider_drag = None;
                    self.sidebar_drag = false;
                    // Release a captured mouse even when press+release straddle
                    // panes: apps track buttons, not clicks.
                    let _ = self.forward_mouse_button(0, false, self.modifiers);
                    // Copy-on-select: a finished drag with a live selection goes
                    // straight to the clipboard (iTerm2/Alacritty parity).
                    if self.config.terminal.copy_on_select {
                        let text = self
                            .panes
                            .get(&self.focused_pane)
                            .and_then(|pane| pane.lock().expect("pane poisoned").selection_text());
                        if let Some(text) = text {
                            self.set_clipboard(text);
                        }
                    }
                    self.update_cursor();
                }
            },
            WindowEvent::MouseInput { state, button, .. } => {
                // Middle/right clicks go to capturing TUI apps (2=right, 1=middle).
                // The Left arm above owns selection/focus semantics.
                let code = match button {
                    MouseButton::Middle => 1u8,
                    MouseButton::Right => 2u8,
                    _ => 0,
                };
                if code != 0 {
                    match state {
                        ElementState::Pressed => {
                            let _ = self.forward_mouse_button(code, true, self.modifiers);
                        }
                        ElementState::Released => {
                            let _ = self.forward_mouse_button(code, false, self.modifiers);
                        }
                    }
                    self.update_cursor();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (_, cell_h) = self.cell_metrics();
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * WHEEL_LINES_PER_TICK as f32) as i32,
                    MouseScrollDelta::PixelDelta(pos) => (pos.y / cell_h as f64).round() as i32,
                };
                if lines != 0 {
                    // Wheel follows the hovered pane, not the focused one
                    // (matches macOS Terminal / iTerm2 behavior).
                    let forwarded =
                        self.forward_mouse_wheel(lines > 0, lines.unsigned_abs(), self.modifiers);
                    if !forwarded
                        && let Some((id, _, _)) = self.pane_at_mouse()
                        && let Some(pane) = self.panes.get(&id)
                    {
                        pane.lock()
                            .expect("pane poisoned")
                            .scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CloseRequested => {
                // Confirm-quit guard: first close with live shells only shows the
                // banner; a second close (or last pane gone) really exits.
                if !self.panes.is_empty() && self.confirm_exit.is_none() {
                    dlog_app!("close requested -> confirm quit (shells alive)");
                    self.confirm_exit = Some(std::time::Instant::now());
                    self.window.as_ref().inspect(|w| w.request_redraw());
                } else {
                    // Real exit: stop the event loop (setting window to None alone
                    // leaves run_app polling forever — zombie process risk).
                    self.exiting = true;
                    self.window = None;
                    self.renderer = None;
                    self.panes.clear();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Exit stops everything else (also drains the poll timers).
        if self.exiting {
            event_loop.exit();
            return;
        }
        self.poll_config_reload();
        self.tick_plugins();
        // Auto-cancel a stale quit confirmation.
        if let Some(t) = self.confirm_exit
            && t.elapsed() >= CONFIRM_TIMEOUT
        {
            self.confirm_exit = None;
            self.window.as_ref().inspect(|w| w.request_redraw());
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(CONFIG_POLL_MS),
        ));
    }
}

/// Config polling interval for hot reload.
const CONFIG_POLL_MS: u64 = 750;
/// How long the quit-confirm banner stays up before auto-cancelling.
const CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl Berth {
    /// Re-read config.toml when its mtime changes; font changes rebuild the
    /// atlas + re-tile grids. Parse errors keep the previous config (and only
    /// retry on the *next* mtime change, so broken files don't spam).
    fn poll_config_reload(&mut self) {
        let Some(path) = Config::path() else { return };
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        if self.config_mtime == Some(mtime) {
            return;
        }
        self.config_mtime = Some(mtime);
        let Ok(cfg) = Config::load() else {
            crate::render::dlog_emit("config reload: parse failed, keeping previous config");
            return;
        };
        let font_changed = cfg.font.family != self.config.font.family
            || cfg.font.size_pt != self.config.font.size_pt;
        self.config = cfg;
        let (bindings, warns) = build_bindings(&self.config.keybinds.0);
        self.bindings = bindings;
        for (id, reason) in &warns {
            crate::render::dlog_emit(&format!("berth config: keybind '{id}': '{reason}'"));
        }
        dlog_app!(
            "config reload: sidebar={} width={} font=\"{}\" pt={}",
            self.config.sidebar.show,
            self.config.sidebar.width_px,
            self.config.font.family,
            self.config.font.size_pt
        );
        if font_changed && let (Some(renderer), Some(window)) = (&mut self.renderer, &self.window) {
            // Same scale chain as Renderer::new (pt -> CSS px -> device px).
            let size_px = self.config.font.size_pt * 1.333_333 * window.scale_factor() as f32;
            match renderer.set_font(&self.config.font.family, size_px) {
                Ok(()) => self.sync_size(),
                Err(e) => {
                    crate::render::dlog_emit(&format!("config reload: font swap failed: {e}"))
                }
            }
        }
    }

    /// Push the current layout snapshot into every plugin and apply the
    /// renames they queue up. Runs on each wait tick (~750 ms).
    fn tick_plugins(&mut self) {
        if self.plugins.is_empty() {
            return;
        }
        let active_ws = self.layout.active;
        let workspaces: Vec<_> = self
            .layout
            .workspaces
            .iter()
            .map(|ws| berth_plugins::berth::plugin::host::WorkspaceInfo {
                id: ws.id.0,
                label: ws.label.clone(),
                active: ws.id == active_ws,
            })
            .collect();
        let tabs: Vec<_> = self
            .layout
            .workspaces
            .iter()
            .flat_map(|ws| {
                ws.tabs
                    .iter()
                    .map(move |t| berth_plugins::berth::plugin::host::TabInfo {
                        id: t.id.0,
                        workspace_id: ws.id.0,
                        label: t.label.clone(),
                        active: ws.id == active_ws && t.id == ws.focused_tab,
                    })
            })
            .collect();

        let mut renames: Vec<WorkspaceRename> = Vec::new();
        for plugin in &mut self.plugins {
            renames.extend(plugin.sync(workspaces.clone(), tabs.clone()));
        }
        let mut changed = false;
        for r in renames {
            if self.rename_edit.as_ref().is_some_and(|e| e.ws.0 == r.id) {
                continue; // user is typing a label for this workspace: theirs wins
            }
            if let Some(ws) = self.layout.workspaces.iter_mut().find(|ws| ws.id.0 == r.id)
                && ws.label != r.label
            {
                dlog_app!("plugin rename ws {}: '{}'", r.id, r.label);
                ws.label = r.label;
                changed = true;
            }
        }
        if changed {
            self.window.as_ref().inspect(|w| w.request_redraw());
        }
    }
}

/// App config dir (berth/), shared with Config::path()'s origin. None when the
/// platform has no config home.
pub fn config_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("com", "berth", "berth").map(|d| d.config_dir().to_path_buf())
}

pub fn run() -> anyhow::Result<()> {
    let config = Config::load()?;
    let layout = Layout::with_defaults("main");
    let focused_pane = layout.active_tab().map(|t| t.focused).unwrap_or(PaneId(0));
    let (bindings, binds_warns) = build_bindings(&config.keybinds.0);
    for (id, reason) in &binds_warns {
        crate::render::dlog_emit(&format!("berth config: keybind '{id}': '{reason}'"));
    }

    let event_loop = EventLoop::<PaneEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    // Plugin host: load every *.wasm under the config dir's plugins/ folder.
    let mut plugins = Vec::new();
    if let Some(dir) = config_dir() {
        let engine = plugin_engine()?;
        plugins = berth_plugins::load_plugins(&engine, &dir.join("plugins"), &mut |msg| {
            crate::render::dlog_emit(&format!("berth plugins: {msg}"));
        });
        if !plugins.is_empty() {
            crate::render::dlog_emit(&format!("berth plugins: {} loaded", plugins.len()));
        }
    }

    let mut app = Berth {
        config,
        layout,
        window: None,
        renderer: None,
        panes: HashMap::new(),
        proxy,
        clipboard: arboard::Clipboard::new().ok(),
        modifiers: ModifiersState::default(),
        mouse: PhysicalPosition::new(0.0, 0.0),
        dragging: false,
        divider_drag: None,
        sidebar_drag: false,
        last_click: None,
        confirm_exit: None,
        rename_edit: None,
        focused_pane,
        titles: HashMap::new(),
        bindings,
        plugins,
        exiting: false,
        config_mtime: Config::path()
            .and_then(|p| std::fs::metadata(&p).ok())
            .and_then(|m| m.modified().ok()),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
