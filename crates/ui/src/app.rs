//! Window/eventloop: winit 0.30 ApplicationHandler + keyboard/clipboard/selection wiring.
//! M3: workspaces + sidebar — the layout tree (`berth-core::Layout`) decides geometry; live
//! `Pane`s are keyed by PaneId. Sidebar lists workspaces; tab strip + panes tile beside it.
use crate::render::{Chrome, ChromeZone, Renderer, sidebar_width, workspace_index_at};
use alacritty_terminal::term::TermMode;
use berth_core::config::{Backspace, Config};
use berth_core::keybinds::{Action, Chord, build_bindings};
use berth_core::layout::{Layout, PaneId, Rect, Session, Tab, TabId, Workspace, WorkspaceId};
use berth_emulation::pane::{LinkSpan, Pane, PaneEvent, SelectionKind, TermPoint};
use berth_plugins::{LoadedPlugin, WorkspaceRename, plugin_engine};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    window::{Cursor, CursorIcon, Window, WindowId},
};

/// How many lines one wheel tick moves.
const WHEEL_LINES_PER_TICK: i32 = 3;

/// App-side diagnostics, same env gate + sink as render::dlog.
macro_rules! dlog_app {
    ($($arg:tt)*) => {
        if crate::render::debug_enabled() { crate::render::dlog_emit(&format!($($arg)*)); }
    };
}

/// How one wheel tick should be delivered to a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WheelTarget {
    /// Pane captured the mouse: send SGR/X10 wheel press+release bytes.
    MouseReport,
    /// Alternate screen + DECSET 1007: send arrow keys (xterm semantics).
    Arrows,
    /// Normal screen: move the pane's own scrollback viewport.
    Viewport,
}

/// Decide wheel routing from the pane's mode bits.
///
/// `ALTERNATE_SCROLL` is on by default in `TermMode::default()`, so it must
/// never be consulted alone — doing so turned every wheel tick in a normal
/// shell into Up/Down (command history) instead of scrolling.
fn wheel_target(mode: TermMode) -> WheelTarget {
    if mode.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG) {
        WheelTarget::MouseReport
    } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        WheelTarget::Arrows
    } else {
        WheelTarget::Viewport
    }
}

/// Open a validated link with the platform handler. No shell is involved, so
/// terminal output can never turn into a command injection.
fn open_url(uri: &str) {
    use std::process::Command;
    let res = if cfg!(target_os = "macos") {
        Command::new("open").arg(uri).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(uri)
            .spawn()
    } else {
        Command::new("xdg-open").arg(uri).spawn()
    };
    match res {
        Ok(_) => dlog_app!("opened link: {uri}"),
        Err(e) => dlog_app!("link open failed for {uri}: {e}"),
    }
}

/// Add a (possibly fractional) line delta to the wheel accumulator and return
/// the whole lines to act on now. Sub-line trackpad deltas accumulate instead
/// of being rounded away; a direction flip discards the stale remainder.
fn accumulate_wheel(accum: &mut f32, delta_lines: f32) -> i32 {
    if delta_lines != 0.0 && accum.signum() != delta_lines.signum() {
        *accum = 0.0;
    }
    *accum += delta_lines;
    let lines = accum.trunc() as i32;
    *accum -= lines as f32;
    lines
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
    /// Pane owning the current drag selection (kept while the pointer leaves
    /// the pane so the selection can auto-scroll at the edges).
    drag_pane: Option<PaneId>,
    /// Auto-scroll direction/edge while a drag selection is outside its pane.
    drag_autoscroll: Option<DragScroll>,
    /// Next auto-scroll tick for the active drag selection.
    drag_scroll_next: std::time::Instant,
    /// DFS index + axis of the split whose divider is being dragged (Some = dragging).
    divider_drag: Option<(usize, bool)>,
    /// True while dragging the sidebar's right edge to resize it.
    sidebar_drag: bool,
    /// Some(tab) while a tab is being dragged (release over sidebar moves it).
    tab_drag: Option<TabId>,
    /// Press position where the current tab drag started (device px); a drag
    /// only engages after moving TAB_DRAG_THRESHOLD px away.
    tab_drag_origin: (f64, f64),
    /// Last press (time + zone) for double-click detection (tab bar / workspace bar).
    last_click: Option<(std::time::Instant, ClickZone)>,
    /// Set on first close attempt with live shells; second close (or esc-free
    /// timeout) actually exits.
    confirm_exit: Option<std::time::Instant>,
    /// Armed workspace-close confirm banner (id + armed time).
    confirm_ws: Option<(WorkspaceId, std::time::Instant)>,
    /// Live sidebar rename (workspace + buffer), active while Some.
    rename_edit: Option<RenameEdit>,
    /// Pane currently receiving keys / selection events.
    focused_pane: PaneId,
    /// Zoomed pane (tmux-style: occupies the full content rect until toggled).
    zoomed: Option<PaneId>,
    /// OSC title per pane (empty until the shell sets one; falls back to tab label).
    titles: HashMap<PaneId, String>,
    /// focused tab-strip slot or sidebar entry (for hover highlights).
    hovered: Option<crate::render::ChromeZone>,
    /// Whether the OS window is currently borderless-fullscreen.
    fullscreen: bool,
    /// Link under the pointer (pane + row/col span + URI), for Pointer cursor,
    /// underline highlight and modifier-click opening.
    hover_link: Option<(PaneId, LinkSpan)>,
    /// Active scrollback search overlay (None = closed).
    search: Option<SearchState>,
    /// True while the platform IME owns text input (Wayland text-input, XIM).
    /// In that case `Ime::Commit` supplies characters and `KeyboardInput` text
    /// is ignored to avoid double typing.
    ime_enabled: bool,
    /// In-progress IME composition shown at the terminal cursor until commit.
    preedit: String,
    /// Sub-line wheel remainder so trackpad pixel scrolling isn't rounded
    /// away on every event (fractional lines accumulate across events).
    wheel_accum: f32,
    /// Last pane press for double/triple-click selection streaks.
    pane_click: Option<(
        std::time::Instant,
        berth_core::layout::PaneId,
        usize,
        usize,
        u8,
    )>,
    /// Window focus (unfocused windows get a hollow cursor).
    window_focused: bool,
    /// Cursor blink phase (on/off) and next flip deadline.
    blink_on: bool,
    blink_next: std::time::Instant,
    /// OSC 7 cwd per pane (only shells with an integration hook report).
    cwds: HashMap<PaneId, String>,
    /// Compiled keybind table (chord -> action); rebuilt on config reload.
    bindings: HashMap<Chord, Action>,
    /// WASM plugins (tier-1 layout ABI), synced each tick.
    plugins: Vec<LoadedPlugin>,
    /// Status segments republished by plugins (`host.set-status`), tab strip.
    status_segments: Vec<String>,
    /// Transient bottom-strip message (non-zero shell exit, plugin errors) and
    /// when it should disappear.
    notice: Option<(String, std::time::Instant)>,
    /// Tabs that rang the bell while not focused; cleared when the tab is viewed.
    attention: HashSet<TabId>,
    /// `plugin:capability` keys already logged, so denied calls don't spam.
    plugin_denial_log: HashSet<String>,
    /// Set when the app decided to exit; about_to_wait stops the loop.
    exiting: bool,
    /// Where the layout session snapshot lives (None = no config dir).
    session_path: Option<PathBuf>,
    /// Last periodic session save (throttles Session::save to every few seconds).
    last_save: std::time::Instant,
    /// Panes to spawn on startup for a restored session (consumed by resumed()).
    restore_panes: Vec<(PaneId, Option<String>)>,
    /// Earliest time to retry building the renderer after a lost surface/device.
    renderer_retry_at: std::time::Instant,
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

/// Active scrollback search: modal query buffer plus the current hit list.
struct SearchState {
    query: String,
    matches: Vec<berth_emulation::pane::GridMatch>,
    /// Index into `matches` of the highlighted hit.
    current: usize,
}

/// Double-click window (matches macOS/Windows defaults, generous on purpose).
const DOUBLE_CLICK_MS: u128 = 500;
/// px slack around the sidebar's right edge for the resize grab.
const SIDEBAR_EDGE_GRAB: f64 = 4.0;
/// Tab drag must move this far (logical px) before it engages — a click plus
/// jitter must never relocate (or auto-remove) tabs/workspaces.
const TAB_DRAG_THRESHOLD: f64 = 6.0;
/// Upper bound of wheel clicks forwarded per event (flood guard).
const MAX_WHEEL_CLICKS: u32 = 16;
/// Auto-scroll cadence while a selection drag sits past a pane edge.
const DRAG_SCROLL_MS: std::time::Duration = std::time::Duration::from_millis(40);

/// Auto-scroll state while dragging a selection outside its pane.
#[derive(Debug, Clone, Copy)]
struct DragScroll {
    /// alacritty `Scroll::Delta` to apply each tick (negative = newer lines).
    delta: i32,
    /// Anchor the selection on the bottom row (true) or top row (false).
    bottom: bool,
    /// Column the selection keeps extending to (clamped to the pane width).
    col: usize,
}

/// Which way a selection drag beyond `top..bottom` should scroll.
/// Positive delta reveals older lines (pointer above), negative newer.
fn autoscroll_delta(mouse_y: f64, top: f64, bottom: f64) -> Option<i32> {
    if mouse_y < top {
        Some(1)
    } else if mouse_y >= bottom {
        Some(-1)
    } else {
        None
    }
}

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
            match wheel_target(mode) {
                WheelTarget::MouseReport => Some(
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
                ),
                WheelTarget::Arrows => {
                    // xterm alternate-scroll: the wheel becomes arrow keys, but
                    // only in the alternate screen (pagers/editors).
                    let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
                    Some(seq.repeat(clicks as usize))
                }
                WheelTarget::Viewport => None,
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
    /// Geometry of the visible panes: zoomed shows only the zoomed pane full-bleed.
    fn current_rects(&self, area: Rect) -> (Vec<(PaneId, Rect)>, Vec<Rect>) {
        let Some(tab) = self.layout.active_tab() else {
            return (Vec::new(), Vec::new());
        };
        if let Some(zid) = self.zoomed
            && tab.pane_ids_in_order().contains(&zid)
        {
            return (vec![(zid, area)], Vec::new());
        }
        (tab.pane_rects(area), tab.dividers(area))
    }

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
        let (rects, _) = self.current_rects(area);
        let rects = Some(rects);
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
    /// (Re)create the GPU renderer for an existing window. Used at startup and
    /// after a lost device/surface (driver reset, GPU hotplug, display change).
    fn build_renderer(&self, window: &Window) -> Option<Renderer> {
        let font = &self.config.font;
        let mut r = match Renderer::new(
            window,
            &font.family,
            font.size_pt,
            font.line_height,
            &self.config.theme,
        ) {
            Ok(r) => r,
            Err(e) => {
                crate::render::dlog_emit(&format!("renderer init failed: {e:#}"));
                return None;
            }
        };
        r.set_cursor_style(self.config.terminal.cursor_style);
        Some(r)
    }

    fn redraw(&mut self) {
        // Recover from a lost surface/device: rebuild lazily on the next frame
        // instead of leaving a dead window until restart.
        if self.renderer.is_none()
            && std::time::Instant::now() >= self.renderer_retry_at
            && let Some(w) = &self.window
        {
            self.renderer = self.build_renderer(w);
            if self.renderer.is_none() {
                self.renderer_retry_at =
                    std::time::Instant::now() + std::time::Duration::from_secs(1);
            }
        }
        let area = self.content_rect();
        let (rect_list, dividers) = self.current_rects(area);
        let rect_map: HashMap<PaneId, Rect> = rect_list.into_iter().collect();
        let self_titles = &self.titles;
        let tabs: Vec<(&str, bool, bool)> = self
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
                        (
                            label,
                            t.id == ws.focused_tab,
                            self.attention.contains(&t.id),
                        )
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
        let focus = rect_map.get(&self.focused_pane).copied();
        // Hovered link span for underline highlighting (pane rect + span).
        let hover_link = self
            .hover_link
            .as_ref()
            .and_then(|(id, span)| rect_map.get(id).map(|r| (*r, span.clone())));

        let mut guards = Vec::new();
        for (id, rect) in rect_map {
            if let Some(pane) = self.panes.get(&id) {
                let mut guard = pane.lock().expect("pane poisoned");
                guard.pump();
                guards.push((guard, rect));
            }
        }
        let mut scroll = None;
        if let Some(fr) = focus {
            for (g, r) in &guards {
                if *r == fr {
                    scroll = Some(crate::render::ScrollInfo {
                        offset: g.display_offset(),
                        history: g.history_size(),
                    });
                    break;
                }
            }
        }
        let spans = guards
            .iter()
            .map(|(g, r)| (g.term(), *r))
            .collect::<Vec<_>>();

        // Confirm banner (quit, or workspace-close cascade): owned text first so
        // the renderer match below doesn't borrow self twice.
        let confirm: Option<String> = if self.confirm_exit.is_some() {
            Some("Live shells running — close again to quit (esc to cancel)".to_string())
        } else if let Some((id, _)) = self.confirm_ws {
            let ws = self.layout.workspaces.iter().find(|w| w.id == id);
            let panes = ws
                .map(|w| {
                    w.tabs
                        .iter()
                        .map(|t| t.pane_ids_in_order().len())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            ws.map(|w| {
                format!(
                    "Close '{}' ({} live panes) — press ✕ again (esc to cancel)",
                    w.label, panes
                )
            })
        } else {
            None
        };

        let failed = match (self.renderer.as_mut(), self.window.as_ref()) {
            (Some(r), Some(w)) => r
                .draw(
                    &spans,
                    &dividers,
                    &tabs,
                    &workspaces,
                    sb_w,
                    focus,
                    scroll,
                    crate::render::CursorPaint {
                        show: true,
                        blink_on: self.blink_on || !self.config.terminal.cursor_blink,
                        window_focused: self.window_focused,
                    },
                    self.search.as_ref().map(|s| crate::render::SearchView {
                        query: &s.query,
                        matches: &s.matches,
                        current: s.current,
                    }),
                    hover_link,
                    self.zoomed.is_some(),
                    self.hovered,
                    &self.status_segments,
                    confirm.as_deref(),
                    self.notice.as_ref().map(|(msg, _)| msg.as_str()),
                    (!self.preedit.is_empty()
                        && self.search.is_none()
                        && self.rename_edit.is_none())
                    .then_some(self.preedit.as_str()),
                    w,
                )
                .is_err(),
            _ => false,
        };
        if failed {
            // The next redraw rebuilds the renderer (see top of redraw); keep
            // the window/content alive so a driver reset doesn't kill the app.
            crate::render::dlog_emit("draw failed: dropping renderer for rebuild");
            self.renderer = None;
            self.renderer_retry_at =
                std::time::Instant::now() + std::time::Duration::from_millis(500);
            // about_to_wait schedules the retry; no immediate redraw loop.
        }
    }

    /// Spawn a shell pane for `id` and register it in the pane map.
    fn spawn_pane(&mut self, id: PaneId, cwd: Option<&str>) -> bool {
        let proxy = self.proxy.clone();
        match Pane::spawn(
            id,
            cwd,
            &self.config.shell,
            self.config.terminal.scrollback,
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

    /// Best-known cwd for a new child shell: the focused pane's OSC 7 report
    /// (when the shell integrates), else the owning tab's cwd, else None.
    fn focused_cwd(&self) -> Option<String> {
        self.cwds
            .get(&self.focused_pane)
            .cloned()
            .or_else(|| self.layout.active_tab().and_then(|t| t.cwd.clone()))
    }

    /// Split the focused pane: `vertical` = stacked top/bottom, else side by side.
    fn split_focused(&mut self, vertical: bool) {
        let (cell_w, cell_h) = self.cell_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let new_id = self.layout.alloc_pane_id();
        let Some(target) = self.layout.active_tab().map(|t| t.focused) else {
            return;
        };
        let cwd = self.focused_cwd();
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
        let cwd = self.focused_cwd();
        let ws = Workspace {
            id: ws_id,
            label: format!("ws {}", ws_id.0 + 1),
            tabs: vec![Tab::new(tab_id, pane_id, cwd.clone())],
            focused_tab: tab_id,
        };
        self.layout.workspaces.push(ws);
        self.layout.active = ws_id;
        if self.spawn_pane(pane_id, cwd.as_deref()) {
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

    /// Open the scrollback search overlay with an empty query.
    fn open_search(&mut self) {
        self.search = Some(SearchState {
            query: String::new(),
            matches: Vec::new(),
            current: 0,
        });
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Re-run the query against the focused pane and reveal the first hit.
    fn refresh_search(&mut self) {
        let Some(query) = self.search.as_ref().map(|s| s.query.clone()) else {
            return;
        };
        let matches = if let Some(pane) = self.panes.get(&self.focused_pane) {
            let mut pane = pane.lock().expect("pane poisoned");
            let matches = pane.find_matches(&query);
            if let Some(first) = matches.first() {
                pane.reveal_line(first.line, 2);
            }
            matches
        } else {
            Vec::new()
        };
        if let Some(state) = self.search.as_mut() {
            state.matches = matches;
            state.current = 0;
        }
        self.wheel_accum = 0.0;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Cycle to the next/previous hit and scroll it into view.
    /// Route text produced by the IME to whatever owns input right now:
    /// the search query, the rename editor, or the focused shell.
    fn commit_text(&mut self, text: &str) {
        if let Some(state) = &mut self.search {
            state.query.push_str(text);
            self.refresh_search();
            return;
        }
        if let Some(edit) = &mut self.rename_edit {
            edit.buf.push_str(text);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        if let Some(pane) = self.panes.get(&self.focused_pane) {
            let mut pane = pane.lock().expect("pane poisoned");
            let _ = pane.write(text.as_bytes());
            if pane.scroll_to_bottom() {
                self.wheel_accum = 0.0;
            }
        }
    }

    fn search_next(&mut self, forward: bool) {
        let Some(state) = self.search.as_mut() else {
            return;
        };
        if state.matches.is_empty() {
            return;
        }
        let n = state.matches.len();
        state.current = if forward {
            (state.current + 1) % n
        } else {
            (state.current + n - 1) % n
        };
        let line = state.matches[state.current].line;
        if let Some(pane) = self.panes.get(&self.focused_pane) {
            pane.lock().expect("pane poisoned").reveal_line(line, 2);
        }
        self.wheel_accum = 0.0;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Key interception while the search overlay is up. Consumes everything.
    fn search_chord(&mut self, event: &KeyEvent) -> bool {
        if self.search.is_none() {
            return false;
        }
        use winit::keyboard::{Key, NamedKey};
        let mut changed = false;
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.search = None;
                self.window.as_ref().inspect(|w| w.request_redraw());
                return true;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(state) = &mut self.search {
                    state.query.pop();
                }
                changed = true;
            }
            Key::Named(NamedKey::Enter) => {
                let forward = !self.modifiers.shift_key();
                self.search_next(forward);
                return true;
            }
            _ => {
                let plain = !self.modifiers.control_key()
                    && !self.modifiers.alt_key()
                    && !self.modifiers.super_key();
                if plain
                    && let Some(text) = event.text.as_deref()
                    && !text.is_empty()
                {
                    if let Some(state) = &mut self.search {
                        state.query.push_str(text);
                    }
                    changed = true;
                }
            }
        }
        if changed {
            self.refresh_search();
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

    /// Selection granularity for a pane press: 1 click = freeform, 2 = word,
    /// 3 = line. A fourth click starts a new streak. Streaks require the same
    /// pane and cell inside the double-click window.
    fn selection_kind_for(
        &mut self,
        pane: berth_core::layout::PaneId,
        row: usize,
        col: usize,
    ) -> SelectionKind {
        let now = std::time::Instant::now();
        let count = match self.pane_click {
            Some((t, p, r, c, n))
                if p == pane
                    && r == row
                    && c == col
                    && now.duration_since(t).as_millis() <= DOUBLE_CLICK_MS =>
            {
                (n % 3) + 1
            }
            _ => 1,
        };
        self.pane_click = Some((now, pane, row, col, count));
        match count {
            2 => SelectionKind::Word,
            3 => SelectionKind::Line,
            _ => SelectionKind::Simple,
        }
    }

    /// Spawn a fresh shell tab in the active workspace and focus it.
    fn new_tab(&mut self) {
        let (cell_w, cell_h) = self.cell_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let tab_id = self.layout.alloc_tab_id();
        let pane_id = self.layout.alloc_pane_id();
        let cwd = self.focused_cwd();
        {
            let Some(ws) = self.layout.active_workspace_mut() else {
                return;
            };
            ws.tabs.push(Tab::new(tab_id, pane_id, cwd.clone()));
            ws.focused_tab = tab_id;
        }
        if self.spawn_pane(pane_id, cwd.as_deref()) {
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
    /// (index, tabs, only-workspace?) of the focused tab (close-tab guard).
    fn active_tab_counts(&self) -> Option<(usize, usize, bool)> {
        let ws = self.layout.active_workspace()?;
        let idx = ws.tabs.iter().position(|t| t.id == ws.focused_tab)?;
        Some((idx, ws.tabs.len(), self.layout.workspaces.len() == 1))
    }

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

    /// Which tab currently owns `pane` (any workspace; panes outlive focus).
    fn tab_of_pane(&self, pane: PaneId) -> Option<TabId> {
        self.layout
            .workspaces
            .iter()
            .flat_map(|ws| &ws.tabs)
            .find(|t| t.pane_ids_in_order().contains(&pane))
            .map(|t| t.id)
    }

    /// Mirror the focused pane's OSC title into the OS window title, prefixed
    /// with the workspace/tab context (qwen U-12) and a zoom marker.
    fn sync_window_title(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let ws_label = self
            .layout
            .active_workspace()
            .map(|w| w.label.as_str())
            .unwrap_or("");
        let tab_label = self.layout.active_tab().map(|t| t.label.as_str());
        let pane_title = self
            .titles
            .get(&self.focused_pane)
            .map(String::as_str)
            .filter(|t| !t.is_empty());
        let mut title = String::from("berth");
        if !ws_label.is_empty() {
            title.push_str(" — ");
            title.push_str(ws_label);
            if let Some(tab) = tab_label {
                title.push_str(" / ");
                title.push_str(tab);
            }
        }
        if let Some(pane) = pane_title
            && Some(pane) != tab_label
        {
            title.push_str(" · ");
            title.push_str(pane);
        }
        if self.zoomed.is_some() {
            title.push_str(" [zoom]");
        }
        window.set_title(&title);
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
    /// Link under the pointer, if the pointer is over a pane cell with an OSC 8
    /// hyperlink or a URL-looking span.
    fn link_at_mouse(&self) -> Option<(PaneId, LinkSpan)> {
        let (id, row, col) = self.pane_at_mouse()?;
        let pane = self.panes.get(&id)?;
        let guard = pane.lock().ok()?;
        guard.link_span_at(row, col).map(|span| (id, span))
    }

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
        let on_sidebar_edge = self.mouse.y >= f64::from(ch.tab_bar_h)
            && (self.mouse.x - f64::from(sb_w)).abs() <= SIDEBAR_EDGE_GRAB * scale;
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
        } else if self.hover_chrome() || self.hover_link.is_some() {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        };
        window.set_cursor(Cursor::Icon(icon));
    }

    /// Current chrome zone under the pointer (single source of truth shared by
    /// hover highlight, cursor icon and click dispatch).
    /// Blink cadence when enabled and the window is focused (config ms clamped).
    fn blink_interval(&self) -> Option<std::time::Duration> {
        let t = &self.config.terminal;
        (t.cursor_blink && self.window_focused)
            .then(|| std::time::Duration::from_millis(t.cursor_blink_ms.clamp(200, 2000)))
    }

    /// Chrome geometry for the current window. Tab slots shrink-to-fit so a
    /// crowded strip stays inside the window; draw and hit-testing share this.
    fn chrome(&self) -> Chrome {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor())
            .unwrap_or(1.0);
        let mut ch = Chrome::new(scale);
        let win_w = self
            .window
            .as_ref()
            .map(|w| w.inner_size().width as f32)
            .unwrap_or(0.0);
        let sb_w = sidebar_width(
            self.config.sidebar.show,
            self.config.sidebar.width_px,
            scale,
        );
        let tabs = self
            .layout
            .active_workspace()
            .map(|ws| ws.tabs.len())
            .unwrap_or(0);
        ch.tab_w = crate::render::tab_width((win_w - sb_w).max(1.0), tabs, ch.tab_w, ch.scale);
        ch
    }

    fn chrome_zone(&self) -> Option<ChromeZone> {
        self.window.as_ref()?;
        crate::render::chrome_zone_at(
            self.mouse.x,
            self.mouse.y,
            self.config.sidebar.show,
            self.config.sidebar.width_px,
            self.layout.workspaces.len(),
            self.layout
                .active_workspace()
                .map(|ws| ws.tabs.len())
                .unwrap_or(0),
            self.chrome(),
        )
    }

    /// True when the pointer is over interactive chrome (workspace rows, tab
    /// slots, close buttons) so the cursor suggests clickability.
    fn hover_chrome(&self) -> bool {
        self.chrome_zone().is_some_and(ChromeZone::is_clickable)
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
        // Closing the last tab of the last workspace kills the app. Arm the
        // same quit banner as the window ✕ so one stray click can't exit.
        if self.confirm_exit.is_none()
            && self.layout.workspaces.len() == 1
            && self
                .layout
                .active_workspace()
                .is_some_and(|ws| ws.tabs.len() == 1)
        {
            dlog_app!("close last tab -> confirm quit");
            self.confirm_exit = Some(std::time::Instant::now());
            self.window.as_ref().inspect(|w| w.request_redraw());
            return;
        }
        dlog_app!("close tab slot{index} {:?} panes={ids:?}", tab.id);
        for id in ids {
            if let Some(pane) = self.panes.get(&id) {
                pane.lock().expect("pane poisoned").kill();
            }
        }
    }

    /// ✕ / middle-click on a workspace entry. Multi-pane workspaces arm a
    /// confirm banner — a second press within CONFIRM_TIMEOUT commits; single
    /// pane entries close right away.
    fn confirm_workspace_close(&mut self, index: usize) {
        let Some(ws) = self.layout.workspaces.get(index) else {
            return;
        };
        let ws_id = ws.id;
        let pane_count = ws
            .tabs
            .iter()
            .map(|t| t.pane_ids_in_order().len())
            .sum::<usize>();
        if pane_count <= 1 {
            self.confirm_ws = None;
            self.close_workspace_at(index);
            return;
        }
        if let Some((id, _)) = self.confirm_ws
            && id == ws_id
        {
            dlog_app!("workspace close confirm -> commit ws#{index} panes={pane_count}");
            self.confirm_ws = None;
            self.close_workspace_at(index);
            return;
        }
        dlog_app!("workspace close confirm armed ws#{index} panes={pane_count}");
        self.rename_edit = None;
        self.confirm_ws = Some((ws_id, std::time::Instant::now()));
        self.window.as_ref().inspect(|w| w.request_redraw());
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

    /// Device-px rect of a pane in the active tab.
    fn pane_rect(&self, id: PaneId) -> Option<berth_core::layout::Rect> {
        let area = self.content_rect();
        let tab = self.layout.active_tab()?;
        tab.pane_rects(area)
            .into_iter()
            .find(|(pid, _)| *pid == id)
            .map(|(_, rect)| rect)
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

        // Workspace-close confirm banner: escape cancels the pending close.
        if self.confirm_ws.is_some() {
            let name = chord_key_name(event);
            if name.as_deref() == Some("escape") {
                dlog_app!("workspace close confirm cancelled");
                self.confirm_ws = None;
                self.window.as_ref().inspect(|w| w.request_redraw());
            }
            return true;
        }

        // Scrollback search overlay consumes keys while it is up.
        if self.search_chord(event) {
            return true;
        }

        // Sidebar rename editor consumes keys while it is up.
        if self.rename_chord(event) {
            return true;
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
            Action::Search => {
                self.open_search();
                true
            }
            Action::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                dlog_app!("fullscreen -> {}", self.fullscreen);
                if let Some(w) = &self.window {
                    w.set_fullscreen(if self.fullscreen {
                        Some(winit::window::Fullscreen::Borderless(None))
                    } else {
                        None
                    });
                }
                true
            }
            Action::ClearScrollback => {
                if let Some(pane) = self.panes.get(&self.focused_pane) {
                    pane.lock().expect("pane poisoned").clear_scrollback();
                }
                self.wheel_accum = 0.0;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                true
            }
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
                // Keyboard close of the focused tab; refuses to exit the app
                // (a single remaining tab is closed with the ✕ / quit instead).
                let counts = self.active_tab_counts();
                if let Some((idx, tabs, ws_last)) = counts
                    && (tabs > 1 || !ws_last)
                {
                    dlog_app!("action close-tab: idx={idx}");
                    self.close_tab(idx);
                } else {
                    dlog_app!("action close-tab: last tab -> ignored");
                }
                true
            }
            Action::ZoomPane => {
                self.zoomed = if self.zoomed == Some(self.focused_pane) {
                    dlog_app!("zoom-pane: unzoom");
                    None
                } else {
                    dlog_app!("zoom-pane: zoom {:?}", self.focused_pane);
                    Some(self.focused_pane)
                };
                self.sync_size();
                self.sync_window_title();
                true
            }
            Action::FocusPaneUp
            | Action::FocusPaneDown
            | Action::FocusPaneLeft
            | Action::FocusPaneRight
            | Action::ToggleSidebar => {
                if action == Action::ToggleSidebar {
                    self.config.sidebar.show = !self.config.sidebar.show;
                    dlog_app!(
                        "toggle-sidebar: show={} width={}",
                        self.config.sidebar.show,
                        self.config.sidebar.width_px
                    );
                    self.sync_size();
                    return true;
                }
                use berth_core::layout::FocusDir;
                let dir = match action {
                    Action::FocusPaneUp => FocusDir::Up,
                    Action::FocusPaneDown => FocusDir::Down,
                    Action::FocusPaneLeft => FocusDir::Left,
                    _ => FocusDir::Right,
                };
                if let Some(next) = self.layout.active_tab().and_then(|t| t.neighbor_pane(dir)) {
                    dlog_app!("focus-pane neighbor={next:?}");
                    self.focus_pane(next);
                }
                true
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
                NamedKey::F1 => "f1",
                NamedKey::F2 => "f2",
                NamedKey::F3 => "f3",
                NamedKey::F4 => "f4",
                NamedKey::F5 => "f5",
                NamedKey::F6 => "f6",
                NamedKey::F7 => "f7",
                NamedKey::F8 => "f8",
                NamedKey::F9 => "f9",
                NamedKey::F10 => "f10",
                NamedKey::F11 => "f11",
                NamedKey::F12 => "f12",
                _ => return None,
            }
            .to_string(),
        ),
        _ => None,
    }
}

/// KeyDown -> PTY bytes (None = nothing to write).
/// True when the platform IME, not the key event, is responsible for this
/// character. Keeps plain typing working on X11 (no IME) while avoiding double
/// input on Wayland/XIM, where both `Ime::Commit` and `KeyboardInput` fire.
fn ime_owns_text(ime_enabled: bool, key: &Key, mods: ModifiersState) -> bool {
    ime_enabled
        && matches!(key, Key::Character(_))
        && !(mods.control_key() || mods.alt_key() || mods.super_key())
}

/// Encode one cursor key. `DECCKM` (application cursor keys) switches the
/// unmodified form from `CSI A` to `SS3 A`; modified keys always stay CSI.
fn cursor_key(tail: &'static str, csi_mod: u8, modified: bool, app_cursor: bool) -> Vec<u8> {
    if modified {
        format!("\x1b[1;{csi_mod}{tail}").into_bytes()
    } else if app_cursor {
        format!("\x1bO{tail}").into_bytes()
    } else {
        format!("\x1b[{tail}").into_bytes()
    }
}

fn encode_key(
    event: &KeyEvent,
    mods: ModifiersState,
    backspace: Backspace,
    app_cursor: bool,
) -> Option<Vec<u8>> {
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
            NamedKey::ArrowUp => Some(cursor_key("A", csi_mod, modified, app_cursor)),
            NamedKey::ArrowDown => Some(cursor_key("B", csi_mod, modified, app_cursor)),
            NamedKey::ArrowRight => Some(cursor_key("C", csi_mod, modified, app_cursor)),
            NamedKey::ArrowLeft => Some(cursor_key("D", csi_mod, modified, app_cursor)),
            NamedKey::Home => Some(cursor_key("H", csi_mod, modified, app_cursor)),
            NamedKey::End => Some(cursor_key("F", csi_mod, modified, app_cursor)),
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
        // resumed() can fire again on platform lifecycle events; window is created once.
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("berth")
            .with_window_icon(load_window_icon())
            .with_min_inner_size(winit::dpi::LogicalSize::new(400.0, 200.0));
        let window = event_loop.create_window(attrs).expect("window creation");
        // Text input goes through the platform IME; `Ime::Commit` then feeds the
        // shell and `KeyboardInput` text is suppressed while IME is active.
        window.set_ime_allowed(true);
        let renderer = self.build_renderer(&window).expect("renderer init");

        self.window = Some(window);
        self.renderer = Some(renderer);

        let restore_panes = std::mem::take(&mut self.restore_panes);
        if restore_panes.is_empty() {
            // Fresh session: one pane in the active tab.
            let (first_id, cwd) = self
                .layout
                .active_tab()
                .map(|t| (t.focused, t.cwd.clone()))
                .unwrap_or((PaneId(0), None));
            self.spawn_pane(first_id, cwd.as_deref());
        } else {
            // Restored session: re-spawn a shell for every pane in the saved tree.
            let mut spawned = 0usize;
            for (id, cwd) in restore_panes {
                if self.spawn_pane(id, cwd.as_deref()) {
                    spawned += 1;
                }
            }
            crate::render::dlog_emit(&format!(
                "berths: restored layout, {} panes spawned",
                spawned
            ));
        }
        // Follow the saved focus (or fall back to a spawned pane if it's stale).
        self.focused_pane = self
            .layout
            .active_tab()
            .map(|t| t.focused)
            .filter(|p| self.panes.contains_key(p))
            .unwrap_or_else(|| self.panes.keys().copied().next().unwrap_or(PaneId(0)));
        self.sync_size();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: PaneEvent) {
        match event {
            PaneEvent::Log(msg) => {
                crate::render::dlog_emit(&format!("emulation: {msg}"));
            }
            PaneEvent::Output(id) => {
                if let Some(pane) = self.panes.get(&id) {
                    pane.lock().expect("pane poisoned").pump();
                }
                // Coalesce: request_redraw lets the event loop batch bursts of
                // output into one RedrawRequested per loop pass (a fast-emitting
                // PTY would otherwise present one frame per chunk — the UI
                // visibly stalls under `seq 1 3000`-style floods).
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            PaneEvent::Cwd(id, path) => {
                self.cwds.insert(id, path.clone());
                // Write back into the owning tab so saved sessions restore in place.
                let mut owner: Option<WorkspaceId> = None;
                for ws in &mut self.layout.workspaces {
                    for tab in &mut ws.tabs {
                        if tab.pane_ids_in_order().contains(&id) {
                            tab.cwd = Some(path.clone());
                            owner = Some(ws.id);
                        }
                    }
                }
                dlog_app!("osc cwd pane={:?} ws={owner:?} path={path:?}", id);
            }
            PaneEvent::PtyWrite(id, bytes) => {
                dlog_app!("pty write reply {:?}: {} b", id, bytes.len());
                if let Some(pane) = self.panes.get(&id) {
                    let _ = pane.lock().expect("pane poisoned").write(&bytes);
                }
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
            PaneEvent::Bell(id) => {
                // A bell in the visible+focused pane is redundant; everything
                // else gets a dot on its tab until that tab is looked at.
                let visible = self.window_focused && self.focused_pane == id;
                if !visible && let Some(tab) = self.tab_of_pane(id) {
                    dlog_app!("bell pane={id:?} -> tab {:?} attention", tab);
                    if self.attention.insert(tab) {
                        self.redraw();
                    }
                }
            }
            PaneEvent::Closed(id) => {
                // Reap the child first: reader EOF fires for both a clean `exit`
                // and a crash, and a non-zero code is worth surfacing before the
                // pane/tab disappears.
                let exit_code = self
                    .panes
                    .get(&id)
                    .and_then(|p| p.lock().expect("pane poisoned").exit_code());
                let _ = self.panes.remove(&id);
                if let Some(code) = exit_code {
                    dlog_app!("pane {:?} exited with code {code}", id);
                    self.notice = Some((
                        format!("shell exited (code {code})"),
                        std::time::Instant::now(),
                    ));
                }
                // Prune the OSC title so a future PaneId reuse can't inherit it.
                self.titles.remove(&id);
                if self.zoomed == Some(id) {
                    self.zoomed = None;
                }
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
                    self.exiting = true;
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
                    if renderer
                        .set_font(
                            &self.config.font.family,
                            size_px,
                            self.config.font.line_height,
                        )
                        .is_ok()
                    {
                        self.sync_size();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    // Typing resets the blink phase (cursor is solid while active).
                    self.blink_on = true;
                    self.blink_next =
                        std::time::Instant::now() + self.blink_interval().unwrap_or(BLINK_DEFAULT);
                }
                let consumed = event.state == ElementState::Pressed
                    && self.handle_chord(&event, self.modifiers);
                // DECCKM: the focused pane decides arrow-key encoding.
                let app_cursor = self
                    .panes
                    .get(&self.focused_pane)
                    .map(|p| {
                        p.lock()
                            .expect("pane poisoned")
                            .term_mode()
                            .contains(TermMode::APP_CURSOR)
                    })
                    .unwrap_or(false);
                let ime_owns_text =
                    ime_owns_text(self.ime_enabled, &event.logical_key, self.modifiers);
                if !consumed
                    && !ime_owns_text
                    && let Some(bytes) = encode_key(
                        &event,
                        self.modifiers,
                        self.config.terminal.backspace,
                        app_cursor,
                    )
                    && let Some(pane) = self.panes.get(&self.focused_pane)
                {
                    dlog_app!("kbd write: pane={:?} bytes={:?}", self.focused_pane, bytes);
                    let mut pane = pane.lock().expect("pane poisoned");
                    let _ = pane.write(&bytes);
                    // Typing while scrolled back returns to the live tail (xterm
                    // behavior); the wheel remainder is stale afterwards.
                    if pane.scroll_to_bottom() {
                        self.wheel_accum = 0.0;
                    }
                }
            }
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                self.blink_on = true;
                self.blink_next =
                    std::time::Instant::now() + self.blink_interval().unwrap_or(BLINK_DEFAULT);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => self.ime_enabled = true,
                Ime::Disabled => {
                    self.ime_enabled = false;
                    self.preedit.clear();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                Ime::Preedit(text, _) => {
                    if self.preedit != text {
                        self.preedit = text;
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
                Ime::Commit(text) => {
                    if !text.is_empty() {
                        self.commit_text(&text);
                    }
                    self.preedit.clear();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            },
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;

                let next_hover = self.chrome_zone();
                if next_hover != self.hovered {
                    self.hovered = next_hover;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }

                // Link hover is only meaningful when no chrome/drag owns the mouse.
                let next_link = if self.sidebar_drag
                    || self.tab_drag.is_some()
                    || self.divider_drag.is_some()
                {
                    None
                } else {
                    self.link_at_mouse()
                };
                if next_link != self.hover_link {
                    self.hover_link = next_link;
                    self.update_cursor();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }

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
                } else if let Some(tab_id) = self.tab_drag {
                    // Dragging a tab over the sidebar moves it to the hovered
                    // workspace — but only after crossing the drag threshold,
                    // so a plain click (plus jitter) stays a click.
                    let moved = (self.mouse.x - self.tab_drag_origin.0).abs()
                        > TAB_DRAG_THRESHOLD
                            * self
                                .window
                                .as_ref()
                                .map(|w| w.scale_factor())
                                .unwrap_or(1.0)
                        || (self.mouse.y - self.tab_drag_origin.1).abs()
                            > TAB_DRAG_THRESHOLD
                                * self
                                    .window
                                    .as_ref()
                                    .map(|w| w.scale_factor())
                                    .unwrap_or(1.0);
                    if !moved {
                        return;
                    }
                    dlog_app!(
                        "tab-drag move at ({},{}) sh={}",
                        self.mouse.x,
                        self.mouse.y,
                        self.config.sidebar.show
                    );
                    let scale = self
                        .window
                        .as_ref()
                        .map(|w| w.scale_factor())
                        .unwrap_or(1.0);
                    let ch = crate::render::Chrome::new(scale);
                    let sb_w = crate::render::sidebar_width(
                        self.config.sidebar.show,
                        self.config.sidebar.width_px,
                        scale,
                    );
                    let over_sidebar = self.config.sidebar.show
                        && self.layout.workspaces.len() > 1
                        && self.mouse.x < sb_w as f64;
                    if over_sidebar {
                        let ws_count = self.layout.workspaces.len();
                        let idx = workspace_index_at(self.mouse.y as f32, ws_count, ch);
                        if let Some(ws) = idx
                            .and_then(|i| self.layout.workspaces.get(i))
                            .map(|w| w.id)
                        {
                            let holder = self
                                .layout
                                .workspaces
                                .iter()
                                .find(|w| w.tabs.iter().any(|t| t.id == tab_id))
                                .map(|w| w.id);
                            if holder != Some(ws) {
                                dlog_app!("tab drag -> ws {:?}", ws);
                                let empty = self.layout.move_tab_to(tab_id, ws);
                                if let Some(empty) = empty {
                                    self.layout.remove_workspace(empty);
                                }
                            }
                            if let Some(id) = self
                                .layout
                                .active_workspace()
                                .and_then(|w| w.focused_tab())
                                .map(|t| t.focused)
                            {
                                self.focused_pane = id;
                            }
                            self.sync_size();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                        return;
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
                    && let Some(id) = self.drag_pane
                {
                    if let (Some(pane), Some(rect)) = (self.panes.get(&id), self.pane_rect(id)) {
                        let (cw, ch) = self.cell_metrics();
                        let (rx, ry) = (rect.x as f64, rect.y as f64);
                        let rows = ((rect.h as f32 / ch).floor() as usize).max(1);
                        let cols = ((rect.w as f32 / cw).floor() as usize).max(1);
                        // Pointer outside the pane still extends to the nearest
                        // edge cell; the timer in about_to_wait scrolls the grid.
                        let lx = (self.mouse.x - rx).max(0.0).min(rect.w as f64 - 1.0);
                        let ly = (self.mouse.y - ry).max(0.0).min(rect.h as f64 - 1.0);
                        let col = ((lx / cw as f64) as usize).min(cols - 1);
                        let row = ((ly / ch as f64) as usize).min(rows - 1);
                        {
                            let mut p = pane.lock().expect("pane poisoned");
                            let pt = term_point(&p, row, col);
                            p.update_selection(pt);
                        }
                        let dir = autoscroll_delta(self.mouse.y, ry, ry + rect.h as f64);
                        self.drag_autoscroll = dir.map(|delta| DragScroll {
                            delta,
                            bottom: delta < 0,
                            col,
                        });
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
            } => {
                match state {
                    ElementState::Pressed => {
                        let ch = self.chrome();
                        dlog_app!(
                            "mouse press at ({:?},{:?}) y_strip={}",
                            self.mouse.x,
                            self.mouse.y,
                            ch.tab_bar_h
                        );
                        // One shared zone map drives hover, cursor and press
                        // precedence (close-✕ > row > plus/empty > edge > pane), so
                        // the branches can never drift apart again.
                        let zone = crate::render::chrome_zone_at(
                            self.mouse.x,
                            self.mouse.y,
                            self.config.sidebar.show,
                            self.config.sidebar.width_px,
                            self.layout.workspaces.len(),
                            self.layout
                                .active_workspace()
                                .map(|ws| ws.tabs.len())
                                .unwrap_or(0),
                            ch,
                        );
                        dlog_app!("press zone={zone:?}");
                        match zone {
                            Some(ChromeZone::SidebarClose(i)) => {
                                dlog_app!("sidebar close ws#{i}");
                                self.rename_edit = None;
                                self.confirm_workspace_close(i);
                            }
                            Some(ChromeZone::SidebarEntry(i)) => {
                                dlog_app!("sidebar click ws#{i}");
                                if self.double_click(ClickZone::SidebarEntry(i)) {
                                    dlog_app!("sidebar double-click -> rename ws#{i}");
                                    self.begin_rename(i);
                                } else {
                                    self.rename_edit = None;
                                    self.select_workspace(i);
                                }
                            }
                            Some(ChromeZone::SidebarPlus) => {
                                dlog_app!("sidebar plus -> new workspace");
                                self.new_workspace();
                            }
                            Some(ChromeZone::SidebarEmpty) => {
                                if self.double_click(ClickZone::SidebarEmpty) {
                                    dlog_app!("sidebar double-click -> new workspace");
                                    self.new_workspace();
                                }
                            }
                            Some(ChromeZone::TabClose(i)) => {
                                dlog_app!("tab close slot{i}");
                                self.close_tab(i);
                            }
                            Some(ChromeZone::TabPlus) => {
                                dlog_app!("tab bar plus -> new tab");
                                self.new_tab();
                            }
                            Some(ChromeZone::Tab(i)) => {
                                let focused_idx = self.layout.active_workspace().and_then(|ws| {
                                    ws.tabs.iter().position(|t| t.id == ws.focused_tab)
                                });
                                if focused_idx == Some(i) && self.double_click(ClickZone::TabBar) {
                                    dlog_app!("tab bar double-click -> new tab");
                                    self.new_tab();
                                } else {
                                    self.select_tab(i);
                                    if let Some(tab) =
                                        self.layout.active_workspace().and_then(|ws| ws.tabs.get(i))
                                    {
                                        dlog_app!("tab drag begin slot{i} tab {:?}", tab.id);
                                        self.tab_drag = Some(tab.id);
                                        self.tab_drag_origin = (self.mouse.x, self.mouse.y);
                                    }
                                }
                            }
                            Some(ChromeZone::TabStripEmpty) => {
                                if self.double_click(ClickZone::TabBar) {
                                    dlog_app!("tab bar double-click -> new tab");
                                    self.new_tab();
                                }
                            }
                            Some(ChromeZone::SidebarEdge) => {
                                // Only reachable below the tab strip; never steals ✕.
                                dlog_app!("sidebar grab");
                                self.sidebar_drag = true;
                            }
                            None => {
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
                                        dlog_app!(
                                            "divider grab #{idx} vertical={vertical} at ({x},{y})"
                                        );
                                        self.divider_drag = Some((idx, vertical));
                                    } else {
                                        self.dragging = true;
                                        if let Some((id, row, col)) = self.pane_at_mouse() {
                                            self.focused_pane = id;
                                            if let Some(tab) = self.layout.active_tab_mut() {
                                                tab.focused = id;
                                            }
                                            // Modifier+click opens a hovered link instead of
                                            // starting a selection (cmd on mac, ctrl elsewhere).
                                            let open_click = if cfg!(target_os = "macos") {
                                                self.modifiers.super_key()
                                            } else {
                                                self.modifiers.control_key()
                                            };
                                            if open_click
                                                && let Some((lid, span)) = self.hover_link.clone()
                                                && lid == id
                                            {
                                                open_url(&span.uri);
                                                return;
                                            }
                                            let kind = self.selection_kind_for(id, row, col);
                                            self.drag_pane = Some(id);
                                            self.drag_autoscroll = None;
                                            self.drag_scroll_next = std::time::Instant::now();
                                            if let Some(pane) = self.panes.get(&id) {
                                                let mut pane = pane.lock().expect("pane poisoned");
                                                let pt = term_point(&pane, row, col);
                                                pane.start_selection_kind(pt, kind);
                                            }
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
                        self.drag_pane = None;
                        self.drag_autoscroll = None;
                        self.divider_drag = None;
                        self.sidebar_drag = false;
                        self.tab_drag = None;
                        // Release a captured mouse even when press+release straddle
                        // panes: apps track buttons, not clicks.
                        let _ = self.forward_mouse_button(0, false, self.modifiers);
                        // Copy-on-select: a finished drag with a live selection goes
                        // straight to the clipboard (iTerm2/Alacritty parity).
                        if self.config.terminal.copy_on_select {
                            let text = self.panes.get(&self.focused_pane).and_then(|pane| {
                                pane.lock().expect("pane poisoned").selection_text()
                            });
                            if let Some(text) = text {
                                self.set_clipboard(text);
                            }
                        }
                        self.update_cursor();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // Middle-click closes the chrome item under the pointer (iTerm2
                // style); other middle/right clicks go to capturing TUI apps
                // (2=right, 1=middle). The Left arm owns selection semantics.
                if button == MouseButton::Middle && state == ElementState::Pressed {
                    let ch = self.chrome();
                    let zone = crate::render::chrome_zone_at(
                        self.mouse.x,
                        self.mouse.y,
                        self.config.sidebar.show,
                        self.config.sidebar.width_px,
                        self.layout.workspaces.len(),
                        self.layout
                            .active_workspace()
                            .map(|ws| ws.tabs.len())
                            .unwrap_or(0),
                        ch,
                    );
                    if let Some(i) = zone.and_then(ChromeZone::workspace_index)
                        && self.layout.workspaces.len() > 1
                    {
                        dlog_app!("middle click -> close workspace #{i}");
                        self.confirm_workspace_close(i);
                        return;
                    }
                    if let Some(i) = zone.and_then(ChromeZone::tab_index) {
                        dlog_app!("middle click -> close tab slot{i}");
                        self.close_tab(i);
                        return;
                    }
                    if matches!(zone, Some(ChromeZone::TabPlus | ChromeZone::TabStripEmpty)) {
                        // Chrome, not a pane: never forward.
                        return;
                    }
                    // Not over a closable chrome item: fall through to TUI.
                }
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
                let delta_lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * WHEEL_LINES_PER_TICK as f32,
                    MouseScrollDelta::PixelDelta(pos) => (pos.y / cell_h.max(1.0) as f64) as f32,
                };
                // Trackpads emit many sub-line pixel deltas; accumulate instead of
                // rounding each event away (the old code dropped smooth scrolling).
                let lines = accumulate_wheel(&mut self.wheel_accum, delta_lines);
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
            // Flush the final layout snapshot before the loop stops.
            if let Some(path) = &self.session_path
                && let Err(e) = berth_core::layout::Session::save(&self.layout, path)
            {
                crate::render::dlog_emit(&format!("berths: final save failed: {e:#}"));
            }
            event_loop.exit();
            return;
        }
        self.poll_config_reload();
        self.tick_plugins();
        // Viewing a tab (focused window) consumes its bell attention dot.
        if self.window_focused
            && let Some(tab) = self.layout.active_tab().map(|t| t.id)
        {
            self.attention.remove(&tab);
        }
        // Periodic layout snapshot (shells are not persisted; only the tree).
        if let Some(path) = self.session_path.as_ref()
            && self.last_save.elapsed() >= std::time::Duration::from_millis(SESSION_SAVE_MS)
        {
            let path = path.clone();
            self.last_save = std::time::Instant::now();
            if let Err(e) = berth_core::layout::Session::save(&self.layout, &path) {
                crate::render::dlog_emit(&format!("berths: save failed: {e:#}"));
            } else {
                dlog_app!("berths: saved layout snapshot");
            }
        }
        // Auto-cancel a stale quit confirmation.
        if let Some(t) = self.confirm_exit
            && t.elapsed() >= CONFIRM_TIMEOUT
        {
            self.confirm_exit = None;
            self.window.as_ref().inspect(|w| w.request_redraw());
        }
        // Auto-cancel a stale workspace-close confirmation.
        if let Some((_, t)) = self.confirm_ws
            && t.elapsed() >= CONFIRM_TIMEOUT
        {
            self.confirm_ws = None;
            self.window.as_ref().inspect(|w| w.request_redraw());
        }
        // Auto-dismiss a transient notice (e.g. shell exited non-zero).
        if let Some((_, t)) = &self.notice
            && t.elapsed() >= NOTICE_TIMEOUT
        {
            self.notice = None;
            self.window.as_ref().inspect(|w| w.request_redraw());
        }
        // Selection drag past the pane edge: scroll the grid on a timer and keep
        // extending the selection at the edge row (qwen U-13).
        if self.dragging
            && let Some(edge) = self.drag_autoscroll
            && std::time::Instant::now() >= self.drag_scroll_next
            && let Some(id) = self.drag_pane
            && let Some(pane) = self.panes.get(&id)
            && let Some(rect) = self.pane_rect(id)
        {
            let (cw, ch) = self.cell_metrics();
            let rows = ((rect.h as f32 / ch).floor() as usize).max(1);
            let cols = ((rect.w as f32 / cw).floor() as usize).max(1);
            let mut p = pane.lock().expect("pane poisoned");
            p.scroll_display(alacritty_terminal::grid::Scroll::Delta(edge.delta));
            let row = if edge.bottom { rows - 1 } else { 0 };
            let pt = term_point(&p, row, edge.col.min(cols - 1));
            p.update_selection(pt);
            drop(p);
            self.drag_scroll_next = std::time::Instant::now() + DRAG_SCROLL_MS;
            self.window.as_ref().inspect(|w| w.request_redraw());
        }
        // Cursor blink: only while the window is focused and config allows it.
        let mut wait_until =
            std::time::Instant::now() + std::time::Duration::from_millis(CONFIG_POLL_MS);
        let blink_interval = self.blink_interval();
        if let Some(interval) = blink_interval {
            let now = std::time::Instant::now();
            if now >= self.blink_next {
                self.blink_on = !self.blink_on;
                self.blink_next = now + interval;
                self.window.as_ref().inspect(|w| w.request_redraw());
            }
            wait_until = wait_until.min(self.blink_next);
        }
        // Wake up when the transient notice expires so it clears on its own.
        if let Some((_, t)) = &self.notice {
            wait_until = wait_until.min(*t + NOTICE_TIMEOUT);
        }
        // Renderer rebuild retry after a lost surface/device (backoff, driven by
        // the wait timer so a broken GPU doesn't spin the event loop).
        if self.renderer.is_none() && self.window.is_some() {
            if std::time::Instant::now() >= self.renderer_retry_at {
                self.window.as_ref().inspect(|w| w.request_redraw());
            } else {
                wait_until = wait_until.min(self.renderer_retry_at);
            }
        }
        if self.dragging && self.drag_autoscroll.is_some() {
            wait_until = wait_until.min(self.drag_scroll_next);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
    }
}

/// Config polling interval for hot reload.
const CONFIG_POLL_MS: u64 = 750;
/// Interval between session layout snapshots (berths).
const SESSION_SAVE_MS: u64 = 4000;
/// How long the quit-confirm banner stays up before auto-cancelling.
const CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a transient notice (non-zero shell exit) stays on screen.
const NOTICE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
/// Default cursor blink interval until config is consulted (530 ms ≈ xterm).
const BLINK_DEFAULT: std::time::Duration = std::time::Duration::from_millis(530);

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
            || cfg.font.size_pt != self.config.font.size_pt
            || cfg.font.line_height != self.config.font.line_height;
        self.config = cfg;
        if let Some(renderer) = &mut self.renderer {
            renderer.set_theme(&self.config.theme);
            renderer.set_cursor_style(self.config.terminal.cursor_style);
        }
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
            match renderer.set_font(
                &self.config.font.family,
                size_px,
                self.config.font.line_height,
            ) {
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
            if !self.status_segments.is_empty() {
                self.status_segments.clear();
                self.window.as_ref().inspect(|w| w.request_redraw());
            }
            return;
        }
        let accent =
            self.config
                .theme
                .accent
                .clone()
                .unwrap_or_else(|| match self.config.theme.preset {
                    berth_core::config::ThemePreset::Light => "#0969da".to_string(),
                    berth_core::config::ThemePreset::Dark => "#4f8cff".to_string(),
                });
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
        let mut segments: Vec<String> = Vec::new();
        for plugin in &mut self.plugins {
            plugin.set_accent(&accent);
            let report = plugin.sync(workspaces.clone(), tabs.clone());
            if let Some(err) = report.error {
                // A trap means the guest was stopped (fuel/memory/panic). It is
                // skipped from now on instead of retried every layout change.
                crate::render::dlog_emit(&format!(
                    "plugin '{}' trapped; disabled for this session: {err}",
                    plugin.name()
                ));
            }
            for denial in report.denials {
                let key = format!("{}:{denial}", plugin.name());
                if self.plugin_denial_log.insert(key) {
                    crate::render::dlog_emit(&format!(
                        "plugin '{}' ignored: {}",
                        plugin.name(),
                        denial
                    ));
                }
            }
            let status = plugin.status();
            if !status.is_empty() {
                segments.push(status.to_string());
            }
            renames.extend(report.renames);
        }
        if segments != self.status_segments {
            self.status_segments = segments;
            self.window.as_ref().inspect(|w| w.request_redraw());
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
    // First log line always announces where the log lives (Windows triage),
    // and a panic hook turns invisible GUI panics into log lines.
    crate::render::dlog_emit(&format!("log: config_dir={:?}", crate::app::config_dir()));
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            crate::render::dlog_emit(&format!("panic: {info}"));
            default_hook(info);
        }));
    }
    let config = Config::load()?;
    // Session restore: reload the last saved layout tree (panes re-spawn fresh shells;
    // invalid/absent sessions fall back to the default layout).
    let session_file = config_dir().map(|d| d.join("berths.json"));
    let (layout, restored) = session_file
        .as_deref()
        .and_then(|p| Session::load(p).ok().map(|s| s.layout))
        .filter(|l| !l.workspaces.is_empty())
        .map(|l| (l, true))
        .unwrap_or_else(|| (Layout::with_defaults("main"), false));
    let focused_pane = layout
        .active_tab()
        .or_else(|| layout.workspaces.first().and_then(|w| w.tabs.first()))
        .map(|t| t.focused)
        .unwrap_or(PaneId(0));
    let (bindings, binds_warns) = build_bindings(&config.keybinds.0);
    for (id, reason) in &binds_warns {
        crate::render::dlog_emit(&format!("berth config: keybind '{id}': '{reason}'"));
    }

    let event_loop = EventLoop::<PaneEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    // Plugin host: load every enabled *.wasm under the config dir's plugins/ folder.
    let mut plugins = Vec::new();
    let plugin_disabled = config.plugins.disabled();
    if let Some(dir) = config_dir() {
        let engine = plugin_engine()?;
        plugins = berth_plugins::load_plugins(
            &engine,
            &dir.join("plugins"),
            &plugin_disabled,
            &mut |msg| {
                crate::render::dlog_emit(&format!("berth plugins: {msg}"));
            },
        );
        if !plugins.is_empty() {
            crate::render::dlog_emit(&format!("berth plugins: {} loaded", plugins.len()));
        }
    }

    // Layout pane list for a restored session (built before `layout` moves into Berth).
    let restore_panes = if restored {
        let mut list = Vec::new();
        for ws in &layout.workspaces {
            for tab in &ws.tabs {
                for id in tab.pane_ids_in_order() {
                    list.push((id, tab.cwd.clone()));
                }
            }
        }
        list
    } else {
        Vec::new()
    };

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
        drag_pane: None,
        drag_autoscroll: None,
        drag_scroll_next: std::time::Instant::now(),
        divider_drag: None,
        sidebar_drag: false,
        tab_drag: None,
        tab_drag_origin: (0.0, 0.0),
        last_click: None,
        confirm_exit: None,
        confirm_ws: None,
        rename_edit: None,
        focused_pane,
        zoomed: None,
        titles: HashMap::new(),
        hovered: None,
        fullscreen: false,
        hover_link: None,
        search: None,
        ime_enabled: false,
        preedit: String::new(),
        wheel_accum: 0.0,
        pane_click: None,
        renderer_retry_at: std::time::Instant::now(),
        window_focused: true,
        blink_on: true,
        blink_next: std::time::Instant::now() + BLINK_DEFAULT,
        cwds: HashMap::new(),
        bindings,
        plugins,
        status_segments: Vec::new(),
        notice: None,
        attention: HashSet::new(),
        plugin_denial_log: HashSet::new(),
        exiting: false,
        session_path: session_file,
        last_save: std::time::Instant::now(),
        restore_panes,
        config_mtime: Config::path()
            .and_then(|p| std::fs::metadata(&p).ok())
            .and_then(|m| m.modified().ok()),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_in_normal_shell_scrolls_viewport_not_history() {
        // ALTERNATE_SCROLL is part of TermMode::default(); without the ALT_SCREEN
        // guard every wheel tick became Up/Down in a plain prompt.
        let mode = TermMode::default();
        assert!(mode.contains(TermMode::ALTERNATE_SCROLL));
        assert!(!mode.contains(TermMode::ALT_SCREEN));
        assert_eq!(wheel_target(mode), WheelTarget::Viewport);
    }

    #[test]
    fn wheel_uses_arrows_only_in_alternate_screen() {
        let mode = TermMode::default() | TermMode::ALT_SCREEN;
        assert_eq!(wheel_target(mode), WheelTarget::Arrows);
    }

    #[test]
    fn wheel_mouse_reporting_wins_over_alternate_scroll() {
        let mode = TermMode::default() | TermMode::ALT_SCREEN | TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(wheel_target(mode), WheelTarget::MouseReport);
        let drag = TermMode::default() | TermMode::MOUSE_DRAG;
        assert_eq!(wheel_target(drag), WheelTarget::MouseReport);
    }

    #[test]
    fn sub_line_wheel_deltas_accumulate_instead_of_rounding_away() {
        // A trackpad sends 0.25-line pixel deltas; four of them must move one line
        // instead of being rounded to zero and lost (the old behavior).
        let mut acc = 0.0;
        assert_eq!(accumulate_wheel(&mut acc, 0.25), 0);
        assert_eq!(accumulate_wheel(&mut acc, 0.25), 0);
        assert_eq!(accumulate_wheel(&mut acc, 0.25), 0);
        assert_eq!(accumulate_wheel(&mut acc, 0.25), 1);
        assert!(acc.abs() < 1e-6);
    }

    #[test]
    fn wheel_direction_flip_drops_stale_remainder() {
        let mut acc = 0.0;
        assert_eq!(accumulate_wheel(&mut acc, 0.9), 0);
        assert_eq!(accumulate_wheel(&mut acc, -0.5), 0);
        assert!((acc + 0.5).abs() < 1e-6);
        assert_eq!(accumulate_wheel(&mut acc, -0.5), -1);
    }

    #[test]
    fn decckm_switches_arrows_to_ss3_but_modified_keys_stay_csi() {
        assert_eq!(cursor_key("A", 1, false, false), b"\x1b[A");
        assert_eq!(cursor_key("A", 1, false, true), b"\x1bOA");
        assert_eq!(cursor_key("D", 1, false, true), b"\x1bOD");
        // Shift/alt/ctrl keep the CSI modifier form even in application mode.
        assert_eq!(cursor_key("A", 2, true, true), b"\x1b[1;2A");
    }

    #[test]
    fn ime_owns_plain_text_only_while_enabled() {
        let text = Key::Character("a".into());
        assert!(!ime_owns_text(false, &text, ModifiersState::empty()));
        assert!(ime_owns_text(true, &text, ModifiersState::empty()));
        // Ctrl/alt/super shortcuts stay on the key-event path.
        assert!(!ime_owns_text(true, &text, ModifiersState::CONTROL));
        // Named keys (arrows, enter) never come from the IME.
        let enter = Key::Named(NamedKey::Enter);
        assert!(!ime_owns_text(true, &enter, ModifiersState::empty()));
    }

    #[test]
    fn drag_beyond_pane_edges_scrolls_toward_the_pointer() {
        // Above the pane reveals older lines (+), below reveals newer (-).
        assert_eq!(autoscroll_delta(10.0, 40.0, 400.0), Some(1));
        assert_eq!(autoscroll_delta(450.0, 40.0, 400.0), Some(-1));
        assert_eq!(autoscroll_delta(200.0, 40.0, 400.0), None);
        // Edges are half-open: exactly top is inside, exactly bottom is outside.
        assert_eq!(autoscroll_delta(40.0, 40.0, 400.0), None);
        assert_eq!(autoscroll_delta(400.0, 40.0, 400.0), Some(-1));
    }
}
