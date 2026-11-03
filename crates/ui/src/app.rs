//! Window/eventloop: winit 0.30 ApplicationHandler + keyboard/clipboard/selection wiring (M1).
//! MVP layout: [sidebar | active tab pane] — sidebar exists only in config; M1 pane fills the window.
use berth_core::{config::Config, layout::Layout};
use berth_emulation::pane::{Pane, PaneEvent};
use std::sync::{Arc, Mutex};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

use crate::render::Renderer;

/// How many lines one wheel tick moves.
const WHEEL_LINES_PER_TICK: i32 = 3;

/// Convert a viewport cell to a terminal-space selection point.
fn term_point(pane: &Pane, row: usize, col: usize) -> berth_emulation::pane::TermPoint {
    alacritty_terminal::term::viewport_to_point(
        pane.term().renderable_content().display_offset,
        alacritty_terminal::index::Point::new(row, alacritty_terminal::index::Column(col)),
    )
}

pub struct Berth {
    /// live config (hot-reloaded in M2; drives sidebar size/font).
    #[allow(dead_code)]
    config: Config,
    /// layout tree the renderer consumes; sidebar rendering lands outside M1 core path.
    #[allow(dead_code)]
    layout: Layout,
    window: Option<Window>,
    renderer: Option<Renderer>,
    /// The single M1 pane. Mutex is held briefly by UI callbacks only.
    pane: Option<Arc<Mutex<Pane>>>,
    proxy: EventLoopProxy<PaneEvent>,
    clipboard: Option<arboard::Clipboard>,
    modifiers: ModifiersState,
    /// Last cursor position in device px (for selection + wheel cell math).
    mouse: PhysicalPosition<f64>,
    /// True while left button held over the pane.
    dragging: bool,
}

impl Berth {
    fn cell_metrics(&self) -> (f32, f32) {
        self.renderer
            .as_ref()
            .map(|r| (r.metrics().cell_w, r.metrics().cell_h))
            .unwrap_or((8.0, 16.0))
    }

    /// Propagate window size -> renderer surface + pane grid.
    fn sync_size(&mut self) {
        let Some(window) = &self.window else { return };
        let Some(renderer) = &mut self.renderer else {
            return;
        };

        renderer.resize_surface(window);
        let phys = window.inner_size();
        let (cell_w, cell_h) = (renderer.metrics().cell_w, renderer.metrics().cell_h);
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let cols = (phys.width as f32 / cell_w).floor().max(1.0) as u16;
        let rows = (phys.height as f32 / cell_h).floor().max(1.0) as u16;
        if let Some(pane) = &self.pane {
            let _ = pane.lock().expect("pane poisoned").resize(
                cols,
                rows,
                cell_w as u16,
                cell_h as u16,
            );
        }
        window.request_redraw();
    }

    fn redraw_pane(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let Some(window) = &self.window else { return };
        let Some(pane) = &self.pane else { return };
        let mut guard = pane.lock().expect("pane poisoned");
        guard.pump();
        if renderer.draw(guard.term(), window).is_err() {
            self.renderer = None; // surface lost hard; window close handles the rest
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

    /// Cell (row, col) under `self.mouse`, clamped to the visible grid.
    fn cell_at_mouse(&self) -> Option<(usize, usize)> {
        let window = self.window.as_ref()?;
        let phys = window.inner_size();
        let (cell_w, cell_h) = self.cell_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        let col = ((self.mouse.x / cell_w as f64).floor().max(0.0) as usize)
            .min(((phys.width as f32 / cell_w).floor().max(1.0) as usize).saturating_sub(1));
        let row = ((self.mouse.y / cell_h as f64).floor().max(0.0) as usize)
            .min(((phys.height as f32 / cell_h).floor().max(1.0) as usize).saturating_sub(1));
        Some((row, col))
    }

    /// Keys that act before the PTY (clipboard, scroll). None if not an app chord.
    fn handle_chord(&mut self, event: &KeyEvent, mods: ModifiersState) {
        let Some(pane) = self.pane.clone() else {
            return;
        };

        let ctrl = mods.control_key();
        let shift = mods.shift_key();
        let chr = match &event.logical_key {
            Key::Character(t) => t.as_str().to_ascii_lowercase(),
            _ => return,
        };

        // Ctrl+Shift+C / Ctrl+Shift+V terminal-style copy & paste.
        if ctrl && shift && chr == "c" {
            let text = pane.lock().expect("pane poisoned").selection_text();
            if let Some(text) = text {
                self.set_clipboard(text);
            }
            return;
        }
        if ctrl && chr == "v" {
            if let Some(text) = self.get_clipboard() {
                let _ = pane.lock().expect("pane poisoned").write(text.as_bytes());
            }
            return;
        }

        // Shift+PgUp/PgDn scroll (encoded PgUp/PgDn stays for the shell).
        let scroll = if shift && event.logical_key == Key::Named(NamedKey::PageUp) {
            alacritty_terminal::grid::Scroll::PageUp
        } else if shift && event.logical_key == Key::Named(NamedKey::PageDown) {
            alacritty_terminal::grid::Scroll::PageDown
        } else {
            return;
        };
        pane.lock().expect("pane poisoned").scroll_display(scroll);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
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

        let proxy = self.proxy.clone();
        let pane = Arc::new(Mutex::new(
            Pane::spawn(
                None,
                Arc::new(move |ev| {
                    let _ = proxy.send_event(ev);
                }),
            )
            .expect("pane spawn"),
        ));
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.pane = Some(pane);
        self.sync_size();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: PaneEvent) {
        match event {
            PaneEvent::Output => {
                if let (Some(renderer), Some(window), Some(pane)) =
                    (self.renderer.as_mut(), &self.window, &self.pane)
                {
                    let mut guard = pane.lock().expect("pane poisoned");
                    guard.pump();
                    if renderer.draw(guard.term(), window).is_err() {
                        self.renderer = None;
                    }
                }
            }
            PaneEvent::Closed => {
                self.pane = None;
                event_loop.exit();
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
                self.handle_chord(&event, self.modifiers);
                if let Some(bytes) = encode_key(&event, self.modifiers)
                    && let Some(pane) = &self.pane
                {
                    let _ = pane.lock().expect("pane poisoned").write(&bytes);
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;
                if self.dragging {
                    if let Some(pane) = &self.pane
                        && let Some((row, col)) = self.cell_at_mouse()
                    {
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
                    self.dragging = true;
                    if let Some(pane) = &self.pane
                        && let Some((row, col)) = self.cell_at_mouse()
                    {
                        let mut pane = pane.lock().expect("pane poisoned");
                        let pt = term_point(&pane, row, col);
                        pane.start_selection(pt);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                ElementState::Released => self.dragging = false,
            },
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(pane) = &self.pane {
                    let (_, cell_h) = self.cell_metrics();
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => {
                            (y * WHEEL_LINES_PER_TICK as f32) as i32
                        }
                        MouseScrollDelta::PixelDelta(pos) => (pos.y / cell_h as f64).round() as i32,
                    };
                    if lines != 0 {
                        pane.lock()
                            .expect("pane poisoned")
                            .scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => self.redraw_pane(),
            WindowEvent::CloseRequested => {
                // TODO(M2): confirm panes with live shells before exit.
                self.window = None;
                self.renderer = None;
                self.pane = None;
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {}
}

pub fn run() -> anyhow::Result<()> {
    let config = Config::load()?;
    let layout = Layout::with_defaults("main");

    let event_loop = EventLoop::<PaneEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = Berth {
        config,
        layout,
        window: None,
        renderer: None,
        pane: None,
        proxy,
        clipboard: arboard::Clipboard::new().ok(),
        modifiers: ModifiersState::default(),
        mouse: PhysicalPosition::new(0.0, 0.0),
        dragging: false,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
