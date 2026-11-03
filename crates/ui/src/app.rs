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
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    window::{Window, WindowId},
};

use crate::render::{Renderer, TAB_BAR_H, sidebar_width, tab_index_at, workspace_index_at};

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
    /// Pane currently receiving keys / selection events.
    focused_pane: PaneId,
}

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
        let tabs: Vec<(&str, bool)> = self
            .layout
            .active_workspace()
            .map(|ws| {
                ws.tabs
                    .iter()
                    .map(|t| (t.label.as_str(), t.id == ws.focused_tab))
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
        self.window.as_ref().inspect(|w| w.request_redraw());
    }

    /// Kill the focused pane's shell; the Closed event does tree cleanup.
    fn close_focused_pane(&mut self) {
        if let Some(pane) = self.panes.get(&self.focused_pane) {
            dlog_app!("close pane {:?}", self.focused_pane);
            pane.lock().expect("pane poisoned").kill();
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
            PaneEvent::Closed(id) => {
                let _ = self.panes.remove(&id);
                // Empty tab -> drop the tab; empty workspace -> exit (M2 has a single
                // workspace; when the sidebar lands this becomes "leave workspace").
                let mut empty_tab = None;
                if let Some(tab) = self.layout.active_tab_mut()
                    && tab.remove_pane(id)
                {
                    empty_tab = Some(tab.id);
                }
                if let Some(tab_id) = empty_tab {
                    let mut exit = false;
                    if let Some(ws) = self.layout.active_workspace_mut() {
                        ws.remove_tab(tab_id);
                        if ws.tabs.is_empty() {
                            let ws_id = ws.id;
                            dlog_app!("workspace {ws_id:?} empty -> closing");
                            self.layout.remove_workspace(ws_id);
                            if self.layout.workspaces.is_empty() {
                                exit = true;
                            }
                        }
                    }
                    if exit {
                        self.panes.clear();
                        self.layout = Layout::default();
                        event_loop.exit();
                        return;
                    }
                    if let Some(tab) = self.layout.active_tab() {
                        self.focused_pane = tab.focused;
                    }
                    self.sync_size();
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
                if self.dragging
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
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    let sb_w =
                        sidebar_width(self.config.sidebar.show, self.config.sidebar.width_px);
                    dlog_app!(
                        "mouse press at ({:?},{:?}) sb_w={sb_w} y_strip={}",
                        self.mouse.x,
                        self.mouse.y,
                        TAB_BAR_H
                    );
                    if self.mouse.x < f64::from(sb_w) {
                        // Sidebar click: pick the workspace entry under the cursor.
                        let count = self.layout.workspaces.len();
                        if let Some(i) = workspace_index_at(self.mouse.y as f32, count) {
                            dlog_app!("sidebar click ws#{i}");
                            self.select_workspace(i);
                        }
                    } else if self.mouse.y < f64::from(TAB_BAR_H) {
                        // Tab strip wins over pane selection: y above the strip picks a tab.
                        let count = self
                            .layout
                            .active_workspace()
                            .map(|ws| ws.tabs.len())
                            .unwrap_or(0);
                        if let Some(i) = tab_index_at(self.mouse.x as f32 - sb_w, count) {
                            self.select_tab(i);
                        }
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                ElementState::Released => self.dragging = false,
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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {}
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
        focused_pane,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
