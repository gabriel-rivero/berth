//! Window/eventloop: winit 0.30 ApplicationHandler + keyboard/clipboard/selection wiring.
//! M3: workspaces + sidebar — the layout tree (`berth-core::Layout`) decides geometry; live
//! `Pane`s are keyed by PaneId. Sidebar lists workspaces; tab strip + panes tile beside it.
use berth_core::config::Config;
use berth_core::layout::{Layout, PaneId, Rect, Tab, Workspace, WorkspaceId};
use berth_emulation::pane::{Pane, PaneEvent, TermPoint};
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

use crate::render::{
    Renderer, TAB_BAR_H, TAB_WIDTH, UI_CLOSE_W, sidebar_width, tab_index_at, workspace_index_at,
};

/// How many lines one wheel tick moves.
const WHEEL_LINES_PER_TICK: i32 = 3;

/// App-side diagnostics, same env gate as render::dlog.
macro_rules! dlog_app {
    ($($arg:tt)*) => {
        if crate::render::debug_enabled() { eprintln!($($arg)*); }
    };
}

/// Convert a pane-local viewport cell to a terminal-space selection point.
fn term_point(pane: &Pane, row: usize, col: usize) -> TermPoint {
    alacritty_terminal::term::viewport_to_point(
        pane.term().renderable_content().display_offset,
        alacritty_terminal::index::Point::new(row, alacritty_terminal::index::Column(col)),
    )
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
    /// Pane currently receiving keys / selection events.
    focused_pane: PaneId,
    /// OSC title per pane (empty until the shell sets one; falls back to tab label).
    titles: HashMap<PaneId, String>,
}

/// Where the last mouse press landed — used for double-click-to-create.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ClickZone {
    /// tab strip (slot or empty area right of the last tab)
    TabBar,
    /// sidebar, below the workspace entries (empty space)
    SidebarEmpty,
}

/// Double-click window (matches macOS/Windows defaults, generous on purpose).
const DOUBLE_CLICK_MS: u128 = 500;
/// px slack around the sidebar's right edge for the resize grab.
const SIDEBAR_EDGE_GRAB: f64 = 4.0;

impl Berth {
    fn cell_metrics(&self) -> (f32, f32) {
        self.renderer
            .as_ref()
            .map(|r| (r.metrics().cell_w, r.metrics().cell_h))
            .unwrap_or((8.0, 16.0))
    }

    /// Window rect the layout tree tiles into: right of the sidebar, below the
    /// tab strip.
    fn content_rect(&self) -> Rect {
        self.window
            .as_ref()
            .map(|w| w.inner_size())
            .map(|s| {
                let bar = TAB_BAR_H as u32;
                let sb =
                    sidebar_width(self.config.sidebar.show, self.config.sidebar.width_px) as u32;
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
        let workspaces: Vec<(&str, bool)> = self
            .layout
            .workspaces
            .iter()
            .map(|ws| (ws.label.as_str(), ws.id == self.layout.active))
            .collect();
        let sb_w = sidebar_width(self.config.sidebar.show, self.config.sidebar.width_px);

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
            (Some(r), Some(w)) => r
                .draw(&spans, &dividers, &tabs, &workspaces, sb_w, w)
                .is_err(),
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
                eprintln!("pane spawn failed: {e}");
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

    /// True when two presses land in `zone` within the double-click window;
    /// always records the press (a matched pair resets the tracker).
    fn double_click(&mut self, zone: ClickZone) -> bool {
        let now = std::time::Instant::now();
        let dbl = self.last_click.is_some_and(|(t, z)| {
            z == zone && now.duration_since(t).as_millis() <= DOUBLE_CLICK_MS
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
        let sb_w = sidebar_width(self.config.sidebar.show, self.config.sidebar.width_px);
        let on_sidebar_edge = (self.mouse.x - f64::from(sb_w)).abs() <= SIDEBAR_EDGE_GRAB;
        let on_divider = if self.mouse.y >= f64::from(TAB_BAR_H) && !on_sidebar_edge {
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
    fn handle_chord(&mut self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let ctrl = mods.control_key();
        let shift = mods.shift_key();

        // Splits run prior to any pane access; fall through otherwise.
        let named = event.logical_key.clone();
        dlog_app!(
            "chord: logical={named:?} ctrl={ctrl} shift={shift} alt={} super={}",
            mods.alt_key(),
            mods.super_key()
        );
        if ctrl && shift && named == Key::Named(NamedKey::Enter) {
            self.split_focused(true);
            return true;
        }
        // Layout-independent: Shift+Backslash types '"' on US layouts, so bind by code.
        if ctrl && shift && event.physical_key == PhysicalKey::Code(KeyCode::Backslash) {
            self.split_focused(false);
            return true;
        }
        // Workspace cycling: ctrl+alt+arrow next/prev in sidebar order.
        if ctrl && mods.alt_key() {
            let delta = match named {
                Key::Named(NamedKey::ArrowLeft) => Some(-1),
                Key::Named(NamedKey::ArrowRight) => Some(1),
                _ => None,
            };
            if let Some(delta) = delta {
                dlog_app!("ws-chord delta={delta}");
                self.cycle_workspace(delta);
                return true;
            }
        }
        // Tab cycling: ctrl+tab next, ctrl+shift+tab prev (Firefox/konsole style).
        if event.logical_key == Key::Named(NamedKey::Tab) {
            if ctrl && shift {
                self.cycle_tab(-1);
            } else if ctrl {
                self.cycle_tab(1);
            }
            return true;
        }
        // ctrl+shift+o: new workspace (one shell tab), focus it.
        if ctrl && shift && event.physical_key == PhysicalKey::Code(KeyCode::KeyO) {
            dlog_app!("chord: new workspace");
            self.new_workspace();
            return true;
        }
        // ctrl+shift+w close focused pane (shell dies -> Closed
        // event -> tree cleanup; closing the last tab's root exits).
        if ctrl && shift && event.physical_key == PhysicalKey::Code(KeyCode::KeyN) {
            self.new_tab();
            return true;
        }
        // alt+arrows move pane focus within the tab (up/down/left/right).
        if mods.alt_key() {
            use berth_core::layout::FocusDir;
            let dir = match named {
                Key::Named(NamedKey::ArrowUp) => Some(FocusDir::Up),
                Key::Named(NamedKey::ArrowDown) => Some(FocusDir::Down),
                Key::Named(NamedKey::ArrowLeft) => Some(FocusDir::Left),
                Key::Named(NamedKey::ArrowRight) => Some(FocusDir::Right),
                _ => None,
            };
            if let Some(dir) = dir {
                dlog_app!("alt-chord dir={dir:?}");
                if let Some(next) = self.layout.active_tab().and_then(|t| t.neighbor_pane(dir)) {
                    dlog_app!("alt-chord neighbor={next:?}");
                    self.focus_pane(next);
                }
                return true;
            }
        }
        if ctrl && shift && event.physical_key == PhysicalKey::Code(KeyCode::KeyW) {
            self.close_focused_pane();
            return true;
        }
        // ctrl+shift+q: close the whole active workspace (all panes killed;
        // when the last workspace empties the app exits — the Closed handler
        // owns the cascade).
        if ctrl && shift && event.physical_key == PhysicalKey::Code(KeyCode::KeyQ) {
            dlog_app!("chord: close workspace");
            self.close_workspace();
            return true;
        }

        let Some(pane) = self.panes.get(&self.focused_pane).cloned() else {
            return false;
        };

        let chr = match &event.logical_key {
            Key::Character(t) => t.as_str().to_ascii_lowercase(),
            _ => return false,
        };

        // Ctrl+Shift+C / Ctrl+Shift+V terminal-style copy & paste.
        if ctrl && shift && chr == "c" {
            let text = pane.lock().expect("pane poisoned").selection_text();
            if let Some(text) = text {
                self.set_clipboard(text);
            }
            return true;
        }
        if ctrl && chr == "v" {
            if let Some(text) = self.get_clipboard() {
                let _ = pane.lock().expect("pane poisoned").write(text.as_bytes());
            }
            return true;
        }

        // Shift+PgUp/PgDn scroll (encoded PgUp/PgDn stays for the shell).
        let scroll = if shift && event.logical_key == Key::Named(NamedKey::PageUp) {
            alacritty_terminal::grid::Scroll::PageUp
        } else if shift && event.logical_key == Key::Named(NamedKey::PageDown) {
            alacritty_terminal::grid::Scroll::PageDown
        } else {
            return false;
        };
        pane.lock().expect("pane poisoned").scroll_display(scroll);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }
}

/// KeyDown -> PTY bytes (None = nothing to write).
fn encode_key(event: &KeyEvent, mods: ModifiersState) -> Option<Vec<u8>> {
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
            NamedKey::Backspace => Some(vec![0x7f]),
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

impl ApplicationHandler<PaneEvent> for Berth {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("berth")
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
            WindowEvent::KeyboardInput { event, .. } => {
                let consumed = event.state == ElementState::Pressed
                    && self.handle_chord(&event, self.modifiers);
                if !consumed
                    && let Some(bytes) = encode_key(&event, self.modifiers)
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
                    // Logical px by design; device px == logical * scale is ignored for now
                    // so the value matches what content_rect()/render use for sb_w.
                    let w = position.x.clamp(120.0, 720.0) as f32;
                    self.config.sidebar.width_px = w;
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
                    let sb_w =
                        sidebar_width(self.config.sidebar.show, self.config.sidebar.width_px);
                    let sb_w_f = f64::from(sb_w);
                    dlog_app!(
                        "mouse press at ({:?},{:?}) sb_w={sb_w} y_strip={}",
                        self.mouse.x,
                        self.mouse.y,
                        TAB_BAR_H
                    );
                    if (self.mouse.x - sb_w_f).abs() <= SIDEBAR_EDGE_GRAB {
                        // Sidebar edge grab wins over entry clicks: drag to resize.
                        dlog_app!("sidebar grab");
                        self.sidebar_drag = true;
                    } else if self.mouse.x < sb_w_f {
                        // Sidebar click: entries select or close (right edge of
                        // the entry is the close button); double-click below
                        // the entries creates a workspace.
                        let count = self.layout.workspaces.len();
                        if let Some(i) = workspace_index_at(self.mouse.y as f32, count) {
                            if self.mouse.x >= sb_w_f - f64::from(UI_CLOSE_W) {
                                dlog_app!("sidebar close ws#{i}");
                                self.close_workspace_at(i);
                            } else {
                                dlog_app!("sidebar click ws#{i}");
                                self.select_workspace(i);
                            }
                        } else if self.double_click(ClickZone::SidebarEmpty) {
                            dlog_app!("sidebar double-click -> new workspace");
                            self.new_workspace();
                        }
                    } else if self.mouse.y < f64::from(TAB_BAR_H) {
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
                        let slot = tab_index_at(self.mouse.x as f32 - sb_w, count);
                        let x_rel = self.mouse.x as f32 - sb_w;
                        if let Some(i) = slot
                            && x_rel >= i as f32 * TAB_WIDTH + (TAB_WIDTH - UI_CLOSE_W)
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                ElementState::Released => {
                    self.dragging = false;
                    self.divider_drag = None;
                    self.sidebar_drag = false;
                    self.update_cursor();
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let (_, cell_h) = self.cell_metrics();
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * WHEEL_LINES_PER_TICK as f32) as i32,
                    MouseScrollDelta::PixelDelta(pos) => (pos.y / cell_h as f64).round() as i32,
                };
                if lines != 0
                    && let Some(pane) = self.panes.get(&self.focused_pane)
                {
                    pane.lock()
                        .expect("pane poisoned")
                        .scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CloseRequested => {
                // TODO(M2): confirm panes with live shells before exit.
                self.window = None;
                self.renderer = None;
                self.panes.clear();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_config_reload();
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(CONFIG_POLL_MS),
        ));
    }
}

/// Config polling interval for hot reload.
const CONFIG_POLL_MS: u64 = 750;

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
            eprintln!("config reload: parse failed, keeping previous config");
            return;
        };
        let font_changed = cfg.font.family != self.config.font.family
            || cfg.font.size_pt != self.config.font.size_pt;
        self.config = cfg;
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
                Err(e) => eprintln!("config reload: font swap failed: {e}"),
            }
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    let config = Config::load()?;
    let layout = Layout::with_defaults("main");
    let focused_pane = layout.active_tab().map(|t| t.focused).unwrap_or(PaneId(0));

    let event_loop = EventLoop::<PaneEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
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
        focused_pane,
        titles: HashMap::new(),
        config_mtime: Config::path()
            .and_then(|p| std::fs::metadata(&p).ok())
            .and_then(|m| m.modified().ok()),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
