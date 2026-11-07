//! wgpu renderer: glyph atlas + grid pipeline for pane terminal content.
//!
//! Per frame: cell backgrounds -> selection tint -> cursor -> glyph quads, all
//! in one dynamic vertex buffer. Glyphs are rasterized with swash into a single
//! RGBA8 atlas (alpha glyphs stored as white-with-coverage, color glyphs raw).
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use berth_core::layout::Rect;
use berth_emulation::pane::{GridMatch, LinkSpan, TermListener};
use bytemuck::{Pod, Zeroable};
use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use std::collections::HashMap;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::{FontRef, zeno};
use wgpu::{
    CurrentSurfaceTexture, LoadOp, Operations, RenderPassColorAttachment, RenderPassDescriptor,
};
use winit::window::Window;

/// One-line diagnostics, enabled with BERTH_DEBUG=1 (cross-platform font/render triage).
/// Output goes to stderr AND `config_dir/berth.log` — the file is the only sink that
/// survives GUI launches on Windows (windows-subsystem builds have no console).
pub fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BERTH_DEBUG").ok().as_deref() == Some("1"))
}

static DLOG_FILE: std::sync::OnceLock<std::sync::Mutex<Option<std::fs::File>>> =
    std::sync::OnceLock::new();

/// Append one diagnostic line to `config_dir/berth.log` and (best-effort) stderr.
/// stderr writes never panic: GUI-subsystem processes may have no valid stderr.
pub fn dlog_emit(line: &str) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let stamp = START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f32();
    let full = format!("[+{stamp:.1}s] {line}");
    {
        let m = DLOG_FILE.get_or_init(|| std::sync::Mutex::new(None));
        let mut slot = match m.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if slot.is_none() {
            // Primary: real config dir; fallback: OS temp dir (a broken/odd
            // AppData must never leave us log-less on Windows).
            *slot = crate::app::config_dir()
                .or_else(|| Some(std::env::temp_dir().join("berth")))
                .and_then(|dir| {
                    std::fs::create_dir_all(&dir).ok()?;
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join("berth.log"))
                        .ok()
                });
        }
        if let Some(f) = slot.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{full}");
        }
    }
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{full}");
}

macro_rules! dlog {
    ($($arg:tt)*) => {
        if debug_enabled() { dlog_emit(&format!($($arg)*)); }
    };
}

/// Enumerate every wgpu adapter this build can see, without a window.
/// Backs `berth --gpu-info` so users can verify hardware acceleration and
/// report which backend/driver was actually available.
pub fn gpu_report() -> String {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    if adapters.is_empty() {
        return "gpu: no wgpu adapters found (no GPU driver / unsupported backend)\n".to_string();
    }
    let mut out = String::new();
    for adapter in adapters {
        let info = adapter.get_info();
        let accel = if info.device_type == wgpu::DeviceType::Cpu {
            "SOFTWARE"
        } else {
            "hardware"
        };
        out.push_str(&format!(
            "gpu: {accel} name={:?} backend={:?} type={:?}\n     driver={:?} driver_info={:?} vendor=0x{:x} device=0x{:x}\n",
            info.name, info.backend, info.device_type, info.driver, info.driver_info, info.vendor, info.device
        ));
    }
    out
}

/// BERTH_FRAME_DUMP=1: read back every presented frame to
/// /tmp/opencode/berth-frame-N.ppm (rootless render-truth check).
fn frame_dump_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BERTH_FRAME_DUMP").ok().as_deref() == Some("1"))
}

fn frame_dump_index() -> u32 {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

fn srgb_to_linear(c: [f32; 4]) -> [f32; 4] {
    let f = |x: f32| {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

const PANEL_BG: [f32; 4] = [0.00394, 0.00394, 0.00490, 1.0];
const CURSOR_BG: [f32; 4] = [0.69207, 0.69207, 0.63828, 1.0];
const DIVIDER_COLOR: [f32; 4] = [0.05088, 0.05088, 0.06372, 1.0];
/// Focused-pane outline (iTerm-style indicator that keyboard input follows).
const FOCUS_OUTLINE: [f32; 4] = [0.10048, 0.26327, 1.0, 1.0];
/// Outline thickness ~1.5 device px, independent of chrome scale.
const FOCUS_OUTLINE_PX: f32 = 1.5;
const FG_DEFAULT: [f32; 4] = [0.68658, 0.68658, 0.68658, 1.0];
const DIM_FACTOR: f32 = 0.62;
const ATLAS_W: u32 = 2048;
const ATLAS_H: u32 = 2048;

/// Top tab strip geometry (device px). TODO(M3): scale with window scale_factor.
pub const TAB_BAR_H: f32 = 28.0;
pub const TAB_WIDTH: f32 = 140.0;

/// Width of the ✕ close-hit zone in the right corner of tab slots and
/// sidebar entries (shared by renderer and input).
pub const UI_CLOSE_W: f32 = 14.0;

/// Width of the `+` (new tab / new workspace) buttons.
pub const UI_PLUS_W: f32 = 26.0;

/// Sidebar geometry: full-height column on the left; entries start below a
/// header strip that aligns with the tab bar.
pub const SIDEBAR_ENTRY_H: f32 = 32.0;
const SIDEBAR_PAD: f32 = 12.0;

const BAR_BG: [f32; 4] = [0.00232, 0.00232, 0.00310, 1.0];
const TAB_BG: [f32; 4] = [0.00854, 0.00854, 0.01002, 1.0];
const TAB_ACTIVE_BG: [f32; 4] = [0.01739, 0.01739, 0.02198, 1.0];
const TAB_TEXT: [f32; 4] = [0.38006, 0.38006, 0.42003, 1.0];
const TAB_TEXT_ACTIVE: [f32; 4] = [0.89001, 0.89001, 0.89001, 1.0];

const SIDEBAR_BG: [f32; 4] = [0.00310, 0.00310, 0.00394, 1.0];
const SIDEBAR_ENTRY_BG: [f32; 4] = [0.00598, 0.00598, 0.00719, 1.0];
const SIDEBAR_ENTRY_ACTIVE_BG: [f32; 4] = [0.01341, 0.01341, 0.01739, 1.0];
const SIDEBAR_TEXT: [f32; 4] = [0.31855, 0.31855, 0.34239, 1.0];
const SIDEBAR_TEXT_ACTIVE: [f32; 4] = [0.89001, 0.89001, 0.89001, 1.0];

const TAB_HOVER_BG: [f32; 4] = [0.012, 0.012, 0.015, 1.0];
const SIDEBAR_ENTRY_HOVER_BG: [f32; 4] = [0.009, 0.009, 0.01, 1.0];
const UI_TEXT_HOVER: [f32; 4] = [0.65, 0.65, 0.70, 1.0];

/// Quit-confirm banner (bottom strip shown when closing with live shells).
const CONFIRM_BG: [f32; 4] = [0.14732, 0.07324, 0.00719, 1.0];
const CONFIRM_TEXT: [f32; 4] = [0.93311, 0.86890, 0.71057, 1.0];
pub const CONFIRM_H: f32 = 34.0;

/// Interactive zones in the chrome (shared by input dispatch and hover renderer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeZone {
    /// A tab slot index.
    Tab(usize),
    /// The ✕ close button on a tab slot.
    TabClose(usize),
    /// The `+` new-tab button in the strip.
    TabPlus,
    /// A workspace entry in the sidebar.
    SidebarEntry(usize),
    /// The ✕ close button on a workspace entry.
    SidebarClose(usize),
    /// The `+` new-workspace button in the sidebar.
    SidebarPlus,
    /// Empty sidebar space below the `+` row (double-click creates a workspace).
    SidebarEmpty,
    /// Empty tab-strip space right of the `+` button (double-click creates a tab).
    TabStripEmpty,
    /// The sidebar resize-drag edge band.
    SidebarEdge,
}

impl ChromeZone {
    /// True for zones that should show the pointer cursor (not the resize edge).
    pub fn is_clickable(self) -> bool {
        !matches!(self, ChromeZone::SidebarEdge)
    }

    /// The tab slot index this zone belongs to, if any.
    pub fn tab_index(self) -> Option<usize> {
        match self {
            ChromeZone::Tab(i) | ChromeZone::TabClose(i) => Some(i),
            _ => None,
        }
    }

    /// The workspace row index this zone belongs to, if any.
    pub fn workspace_index(self) -> Option<usize> {
        match self {
            ChromeZone::SidebarEntry(i) | ChromeZone::SidebarClose(i) => Some(i),
            _ => None,
        }
    }
}

/// Identify which chrome item sits under physical px (x,y).
pub fn chrome_zone_at(
    x: f64,
    y: f64,
    sb_show: bool,
    sb_w_logical: f32,
    ws_count: usize,
    tab_count: usize,
    ch: Chrome,
) -> Option<ChromeZone> {
    let sb_w = sidebar_width(sb_show, sb_w_logical, ch.scale as f64) as f64;
    let (x, y) = (x, y);

    // 1. Sidebar precedence (if shown)
    if sb_show && x < sb_w {
        // Close buttons win over entry selection.
        if let Some(i) = workspace_index_at(y as f32, ws_count, ch) {
            if x >= sb_w - ch.close_w as f64 {
                return Some(ChromeZone::SidebarClose(i));
            }
            return Some(ChromeZone::SidebarEntry(i));
        }
        // Sidebar header/empty space double-click or plus button.
        let plus_y = f64::from(ch.sidebar_top + ws_count as f32 * ch.sidebar_entry_h);
        if y >= plus_y && y < plus_y + ch.sidebar_entry_h as f64 {
            return Some(ChromeZone::SidebarPlus);
        }
        return Some(ChromeZone::SidebarEmpty);
    }

    // 2. Tab strip precedence
    if y < ch.tab_bar_h as f64 {
        let x_rel = (x - sb_w) as f32;
        let slot = tab_index_at(x_rel, tab_count, ch.tab_w);
        if let Some(i) = slot {
            if x_rel >= i as f32 * ch.tab_w + (ch.tab_w - ch.close_w) {
                return Some(ChromeZone::TabClose(i));
            }
            return Some(ChromeZone::Tab(i));
        }
        if x_rel >= tab_count as f32 * ch.tab_w && x_rel < tab_count as f32 * ch.tab_w + ch.plus_w {
            return Some(ChromeZone::TabPlus);
        }
        return Some(ChromeZone::TabStripEmpty);
    }

    // 3. Edge grab (only below the tab strip, ±4 logical px).
    if sb_show && (x - sb_w).abs() <= 4.0 * ch.scale as f64 {
        return Some(ChromeZone::SidebarEdge);
    }

    None
}

/// Minimum tab slot width (logical px) before tabs overflow the strip.
pub const TAB_MIN_WIDTH: f32 = 96.0;

/// Effective tab slot width: shrink from [`TAB_WIDTH`] down to
/// [`TAB_MIN_WIDTH`] so a crowded strip still fits inside the window.
pub fn tab_width(bar_w: f32, count: usize, max_w: f32, scale: f32) -> f32 {
    if count == 0 {
        return max_w;
    }
    (bar_w.max(1.0) / count as f32).clamp(TAB_MIN_WIDTH * scale, max_w)
}

/// How the cursor should be painted for one pane this frame.
#[derive(Debug, Clone, Copy)]
pub struct CursorPaint {
    /// Pane owns the cursor: only the focused pane shows one.
    pub show: bool,
    /// False during the blink-off phase (cursor hidden, text normal).
    pub blink_on: bool,
    /// Window focus: an unfocused window gets a hollow block, not a solid one.
    pub window_focused: bool,
}

/// Bounds of the cursor shape inside its cell (device px).
pub fn cursor_rect(
    style: berth_core::config::CursorStyle,
    px: f32,
    py: f32,
    cw: f32,
    ch: f32,
) -> (f32, f32, f32, f32) {
    use berth_core::config::CursorStyle;
    match style {
        CursorStyle::Block => (px, py, px + cw, py + ch),
        CursorStyle::Beam => {
            let w = (cw * 0.12).max(2.0);
            (px, py, px + w, py + ch)
        }
        CursorStyle::Underline => {
            let h = (ch * 0.1).max(2.0);
            (px, py + ch - h, px + cw, py + ch)
        }
    }
}

/// Viewport position of a scrolled-back pane, for the overlay scrollbar/badge.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollInfo {
    /// Lines scrolled back from the live bottom.
    pub offset: usize,
    /// Lines of history above the viewport.
    pub history: usize,
}

/// Active scrollback search: query, all hits, and the selected hit index.
#[derive(Debug, Clone, Copy)]
pub struct SearchView<'a> {
    pub query: &'a str,
    pub matches: &'a [GridMatch],
    pub current: usize,
}

/// Text shown in the search overlay: query plus "current/total", or 0/0 when
/// nothing matches (so an empty result is distinguishable from a pending query).
fn search_label(query: &str, current: usize, total: usize) -> String {
    if total == 0 {
        format!("find: {query}   0/0")
    } else {
        format!("find: {query}   {}/{}", current + 1, total)
    }
}

/// Which fixed-width tab slot sits under device-px `x` (None past the last tab).
/// `x` is sidebar-relative (the app subtracts the sidebar width first).
/// Chrome geometry in device px, scaled by the window's scale factor
/// (logical px × scale) so Retina/HiDPI screens get normal-size chrome.
#[derive(Debug, Clone, Copy)]
pub struct Chrome {
    pub tab_bar_h: f32,
    pub tab_w: f32,
    pub close_w: f32,
    /// Width of the `+` buttons (tab strip + sidebar).
    pub plus_w: f32,
    pub sidebar_entry_h: f32,
    pub sidebar_top: f32,
    pub sidebar_pad: f32,
    pub confirm_h: f32,
    /// Logical-px scale the geometry was built with.
    pub scale: f32,
}

impl Chrome {
    pub fn new(scale: f64) -> Self {
        let s = scale as f32;
        Self {
            tab_bar_h: TAB_BAR_H * s,
            tab_w: TAB_WIDTH * s,
            close_w: UI_CLOSE_W * s,
            plus_w: UI_PLUS_W * s,
            sidebar_entry_h: SIDEBAR_ENTRY_H * s,
            sidebar_top: TAB_BAR_H * s,
            sidebar_pad: SIDEBAR_PAD * s,
            confirm_h: CONFIRM_H * s,
            scale: s,
        }
    }
}

/// Which tab slot sits under device-px `x` (origin is the sidebar's right edge).
pub fn tab_index_at(x: f32, count: usize, tab_w: f32) -> Option<usize> {
    if count == 0 || x < 0.0 || tab_w <= 0.0 {
        return None;
    }
    let i = (x / tab_w).floor() as usize;
    (i < count).then_some(i)
}

/// Which sidebar entry sits under device-px `y` (entries start below the header).
pub fn workspace_index_at(y: f32, count: usize, ch: Chrome) -> Option<usize> {
    if count == 0 || y < ch.sidebar_top {
        return None;
    }
    let i = ((y - ch.sidebar_top) / ch.sidebar_entry_h).floor() as usize;
    (i < count).then_some(i)
}

/// Sidebar width in device px (`width_px` is logical; device = logical * scale).
pub fn sidebar_width(show: bool, width_px: f32, scale: f64) -> f32 {
    if show {
        let min = SIDEBAR_ENTRY_H * scale as f32;
        (width_px * scale as f32).max(min)
    } else {
        0.0
    }
}

/// xterm-style 16 color palette (0-7 dim, 8-15 bright).
const PALETTE: [[f32; 4]; 16] = [
    [0.0, 0.0, 0.0, 1.0],
    [0.604, 0.0, 0.0, 1.0], // 0.8 srgb -> 0.604 linear
    [0.0, 0.604, 0.0, 1.0],
    [0.604, 0.604, 0.0, 1.0],
    [0.0, 0.0, 0.604, 1.0],
    [0.604, 0.0, 0.604, 1.0],
    [0.0, 0.604, 0.604, 1.0],
    [0.78346, 0.78346, 0.78346, 1.0], // 0.898 srgb -> 0.78346 linear
    [0.214, 0.214, 0.214, 1.0],       // 0.5 srgb -> 0.214 linear
    [1.0, 0.0, 0.0, 1.0],
    [0.0, 1.0, 0.0, 1.0],
    [1.0, 1.0, 0.0, 1.0],
    [0.10654, 0.10654, 1.0, 1.0], // 0.36 srgb -> 0.10654 linear
    [1.0, 0.0, 1.0, 1.0],
    [0.0, 1.0, 1.0, 1.0],
    [1.0, 1.0, 1.0, 1.0],
];

/// Active terminal colors (preset resolved + config overrides applied).
#[derive(Debug, Clone, Copy)]
pub struct ColorScheme {
    pub bg: [f32; 4],
    pub fg: [f32; 4],
    pub cursor: [f32; 4],
    pub palette: [[f32; 4]; 16],
    pub chrome: ChromeColors,
}

/// Window chrome colors (tab strip, sidebar, dividers, banners). Kept separate
/// from ANSI colors so a theme can tune UI and terminal independently.
#[derive(Debug, Clone, Copy)]
pub struct ChromeColors {
    pub panel_bg: [f32; 4],
    pub bar_bg: [f32; 4],
    pub tab_bg: [f32; 4],
    pub tab_active_bg: [f32; 4],
    pub tab_text: [f32; 4],
    pub tab_text_active: [f32; 4],
    pub sidebar_bg: [f32; 4],
    pub sidebar_entry_bg: [f32; 4],
    pub sidebar_entry_active_bg: [f32; 4],
    pub sidebar_text: [f32; 4],
    pub sidebar_text_active: [f32; 4],
    pub tab_hover_bg: [f32; 4],
    pub sidebar_entry_hover_bg: [f32; 4],
    pub text_hover: [f32; 4],
    pub divider: [f32; 4],
    pub focus_outline: [f32; 4],
    pub confirm_bg: [f32; 4],
    pub confirm_text: [f32; 4],
}

impl ChromeColors {
    fn dark() -> Self {
        Self {
            panel_bg: PANEL_BG,
            bar_bg: BAR_BG,
            tab_bg: TAB_BG,
            tab_active_bg: TAB_ACTIVE_BG,
            tab_text: TAB_TEXT,
            tab_text_active: TAB_TEXT_ACTIVE,
            sidebar_bg: SIDEBAR_BG,
            sidebar_entry_bg: SIDEBAR_ENTRY_BG,
            sidebar_entry_active_bg: SIDEBAR_ENTRY_ACTIVE_BG,
            sidebar_text: SIDEBAR_TEXT,
            sidebar_text_active: SIDEBAR_TEXT_ACTIVE,
            tab_hover_bg: TAB_HOVER_BG,
            sidebar_entry_hover_bg: SIDEBAR_ENTRY_HOVER_BG,
            text_hover: UI_TEXT_HOVER,
            divider: DIVIDER_COLOR,
            focus_outline: FOCUS_OUTLINE,
            confirm_bg: CONFIRM_BG,
            confirm_text: CONFIRM_TEXT,
        }
    }

    fn light() -> Self {
        let h = |s: &str| hex_to_linear(s).expect("built-in theme hex is valid");
        Self {
            panel_bg: h("#f7f7f7"),
            bar_bg: h("#ececef"),
            tab_bg: h("#e8e8ec"),
            tab_active_bg: h("#f7f7f7"),
            tab_text: h("#6b6b73"),
            tab_text_active: h("#1f2328"),
            sidebar_bg: h("#f2f2f4"),
            sidebar_entry_bg: h("#f7f7f7"),
            sidebar_entry_active_bg: h("#e3e3e8"),
            sidebar_text: h("#5c5c66"),
            sidebar_text_active: h("#1f2328"),
            tab_hover_bg: h("#e0e0e6"),
            sidebar_entry_hover_bg: h("#e9e9ee"),
            text_hover: h("#2b2b31"),
            divider: h("#d4d4da"),
            focus_outline: h("#0969da"),
            confirm_bg: h("#fff4d6"),
            confirm_text: h("#6b5200"),
        }
    }
}

impl ColorScheme {
    /// Default dark scheme (matches the original hardcoded 16-color palette).
    pub fn dark() -> Self {
        Self {
            bg: PANEL_BG,
            fg: FG_DEFAULT,
            cursor: CURSOR_BG,
            palette: PALETTE,
            chrome: ChromeColors::dark(),
        }
    }

    /// Light scheme with GitHub-flavored contrast.
    pub fn light() -> Self {
        const HEX: [&str; 16] = [
            "#24292f", "#cf222e", "#116329", "#4d2d00", "#0969da", "#8250df", "#1b7c83", "#6e7781",
            "#57606a", "#a40e26", "#1a7f37", "#633c01", "#218bff", "#a475f9", "#3192aa", "#8c959f",
        ];
        let mut palette = [[0.0; 4]; 16];
        for (dst, hex) in palette.iter_mut().zip(HEX) {
            *dst = hex_to_linear(hex).expect("built-in theme hex is valid");
        }
        Self {
            bg: hex_to_linear("#f7f7f7").unwrap(),
            fg: hex_to_linear("#24292f").unwrap(),
            cursor: hex_to_linear("#24292f").unwrap(),
            palette,
            chrome: ChromeColors::light(),
        }
    }

    /// Resolve `[theme]` from config; invalid color strings are logged and ignored.
    pub fn from_config(cfg: &berth_core::config::Theme) -> Self {
        use berth_core::config::ThemePreset;
        let mut scheme = match cfg.preset {
            ThemePreset::Dark => Self::dark(),
            ThemePreset::Light => Self::light(),
        };
        fn apply(dst: &mut [f32; 4], value: &Option<String>, name: &str) {
            if let Some(v) = value {
                match hex_to_linear(v) {
                    Some(c) => *dst = c,
                    None => dlog!("theme: invalid color {name}={v:?} (want #rgb or #rrggbb)"),
                }
            }
        }
        apply(&mut scheme.bg, &cfg.background, "background");
        // Keep the pane gutter in sync with a custom background.
        scheme.chrome.panel_bg = scheme.bg;
        apply(&mut scheme.fg, &cfg.foreground, "foreground");
        apply(&mut scheme.cursor, &cfg.cursor, "cursor");
        for (i, value) in cfg.palette.as_slots().iter().enumerate() {
            apply(&mut scheme.palette[i], value, &format!("palette slot {i}"));
        }
        scheme
    }

    /// Map a term Color to linear RGBA32; handles dim and 256-color variants.
    fn color_of(&self, c: Color) -> [f32; 4] {
        match c {
            Color::Named(NamedColor::Foreground) => self.fg,
            Color::Named(NamedColor::Background) => self.bg,
            Color::Named(n) => match n {
                NamedColor::Black => self.palette[0],
                NamedColor::Red => self.palette[1],
                NamedColor::Green => self.palette[2],
                NamedColor::Yellow => self.palette[3],
                NamedColor::Blue => self.palette[4],
                NamedColor::Magenta => self.palette[5],
                NamedColor::Cyan => self.palette[6],
                NamedColor::White => self.palette[7],
                NamedColor::BrightBlack => self.palette[8],
                NamedColor::BrightRed => self.palette[9],
                NamedColor::BrightGreen => self.palette[10],
                NamedColor::BrightYellow => self.palette[11],
                NamedColor::BrightBlue => self.palette[12],
                NamedColor::BrightMagenta => self.palette[13],
                NamedColor::BrightCyan => self.palette[14],
                NamedColor::BrightWhite => self.palette[15],
                // Dim variants (SGR 2).
                NamedColor::DimBlack => dim(self.palette[0]),
                NamedColor::DimRed => dim(self.palette[1]),
                NamedColor::DimGreen => dim(self.palette[2]),
                NamedColor::DimYellow => dim(self.palette[3]),
                NamedColor::DimBlue => dim(self.palette[4]),
                NamedColor::DimMagenta => dim(self.palette[5]),
                NamedColor::DimCyan => dim(self.palette[6]),
                NamedColor::DimWhite => dim(self.palette[7]),
                NamedColor::DimForeground => dim(self.fg),
                _ => self.palette[7],
            },
            Color::Spec(Rgb { r, g, b }) => {
                srgb_to_linear([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
            }
            Color::Indexed(i) => self.indexed(i),
        }
    }

    /// xterm-256 palette into the 16-color range.
    fn indexed(&self, i: u8) -> [f32; 4] {
        let (r, g, b) = if i < 16 {
            return self.palette[i as usize];
        } else if i < 232 {
            let i = i as u32 - 16;
            let lvl = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
            (lvl(i / 36) as u8, lvl((i % 36) / 6) as u8, lvl(i % 6) as u8)
        } else {
            let v = ((i as u32 - 232) * 10 + 8) as u8;
            (v, v, v)
        };
        srgb_to_linear([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
    }

    fn blend(&self, c: [f32; 4]) -> [f32; 4] {
        [
            c[0] * 0.5 + self.fg[0] * 0.5,
            c[1] * 0.5 + self.fg[1] * 0.5,
            c[2] * 0.5 + self.fg[2] * 0.5,
            1.0,
        ]
    }
}

/// Parse `#rgb` / `#rrggbb` into linear RGBA.
pub fn hex_to_linear(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    let expand = |c: char| -> Option<u8> { c.to_digit(16).map(|d| (d * 17) as u8) };
    let (r, g, b) = match h.len() {
        3 => {
            let mut it = h.chars();
            (
                expand(it.next()?)?,
                expand(it.next()?)?,
                expand(it.next()?)?,
            )
        }
        6 => {
            let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
            (p(0)?, p(2)?, p(4)?)
        }
        _ => return None,
    };
    Some(srgb_to_linear([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        1.0,
    ]))
}

/// Device-pixel cell metrics (whole px for a stable grid).
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub cell_w: f32,
    pub cell_h: f32,
    /// Baseline distance from cell top (swash scale ascent, y-down).
    pub ascent: f32,
}

/// A rasterized glyph parked in the atlas.
#[derive(Debug, Clone, Copy)]
struct GlyphRef {
    /// (x, y, w, h) in atlas pixels.
    uv: [f32; 4],
    /// Ink offset from the pen/baseline (zeno Placement, y-down).
    left: i32,
    top: i32,
    /// false = alpha coverage (tint by fg), true = color glyph (emoji...).
    color: bool,
}

/// Shelf packer for the glyph atlas.
struct Atlas {
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
}

impl Atlas {
    fn new() -> Self {
        Self {
            cursor_x: 1,
            cursor_y: 1,
            row_h: 1,
        }
    }

    fn reset(&mut self) {
        self.cursor_x = 1;
        self.cursor_y = 1;
        self.row_h = 1;
    }

    fn pack(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        if self.cursor_y + h + 1 >= ATLAS_H {
            return None;
        }
        if self.cursor_x + w + 1 > ATLAS_W {
            self.cursor_y += self.row_h + 1;
            self.cursor_x = 1;
            self.row_h = 1;
            if self.cursor_y + h + 1 >= ATLAS_H {
                return None;
            }
        }
        let pos = [self.cursor_x, self.cursor_y];
        self.cursor_x += w + 1;
        self.row_h = self.row_h.max(h);
        Some(pos)
    }
}

/// Pending texture upload from a rasterized glyph.
struct Upload {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// RGBA bytes, w*h*4, rows top-down.
    bytes: Vec<u8>,
}

/// Owned fallback face (emoji/symbol/CJK) + its 'static FontRef.
struct FallbackFont {
    _data: Vec<u8>,
    font: FontRef<'static>,
    /// fontdb family name (diagnostics only).
    name: String,
}

/// A real bold/italic face resolved from the primary family, with owned bytes.
struct Face {
    /// Keeps the transmuted 'static `font` alive.
    _data: Vec<u8>,
    font: FontRef<'static>,
}

/// Cache key for rasterized glyphs: the faux-bold/italic variants are separate
/// bitmaps, so they need separate atlas entries.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    c: char,
    bold: bool,
    italic: bool,
}

/// Per-font glyph cache + raster contexts.
pub struct FontCtx {
    /// Font data kept alive for the transmuted 'static FontRef.
    _data: Vec<u8>,
    font: FontRef<'static>,
    /// Real bold face from the same family (None = synthesize with embolden).
    bold: Option<Face>,
    /// Real italic/oblique face (None = synthesize with a skew transform).
    italic: Option<Face>,
    /// Real bold-italic face (falls back to bold/italic/regular + synthesis).
    bold_italic: Option<Face>,
    /// Faces consulted when the primary lacks a glyph (first hit wins).
    fallbacks: Vec<FallbackFont>,
    size_px: f32,
    /// Line-height multiplier used when computing `cell_h`.
    line_height: f32,
    /// The family this context was built for (hot-reload comparison).
    family: String,
    ctx: ScaleContext,
    /// (char, faux-bold, faux-italic) -> raster (None = unavailable in the chain).
    glyphs: HashMap<GlyphKey, Option<GlyphRef>>,
    pub metrics: Metrics,
}

impl FontCtx {
    /// Bundled fallback when no system font matches (OFL; keeps M1 usable on minimal installs).
    const FALLBACK_FONT: &'static [u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");

    /// Load `family` (fallback path: system monospace -> bundled font) at `size_px` device px.
    pub fn new(family: &str, size_px: f32, line_height: f32) -> anyhow::Result<Self> {
        let mut db = Database::new();
        db.load_system_fonts();
        // User-vendored faces: drop TTFs in the config dir's fonts/ to make them
        // selectable (custom families) or auto-available as fallbacks.
        if let Some(cfg) = crate::app::config_dir() {
            let dir = cfg.join("fonts");
            if dir.is_dir() {
                db.load_fonts_dir(&dir);
            }
        }
        // fontdb resolves generic "monospace" to Courier New on macOS (bad
        // hhea metrics => clipped glyphs), so prefer known-good per-OS faces
        // before the generic family name.
        let generic = matches!(
            family.trim().to_ascii_lowercase().as_str(),
            "" | "monospace"
        );
        let preferred: &[&str] = if cfg!(target_os = "macos") {
            &["Menlo", "SF Mono", "Monaco"]
        } else if cfg!(target_os = "windows") {
            &["Cascadia Mono", "Consolas"]
        } else {
            &["DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono"]
        };
        let mut fams = Vec::with_capacity(preferred.len() + 2);
        if !generic {
            fams.push(Family::Name(family));
        }
        fams.extend(preferred.iter().copied().map(Family::Name));
        fams.push(Family::Monospace);
        let query = Query {
            families: &fams,
            weight: Weight::NORMAL,
            stretch: Stretch::Normal,
            style: Style::Normal,
        };
        let id = db.query(&query);
        let (data, face_index) = id
            .and_then(|id| db.with_face_data(id, |d, idx| (d.to_vec(), idx)))
            .unwrap_or_else(|| (Self::FALLBACK_FONT.to_vec(), 0));
        let face_index = face_index as usize;
        let font_ref = FontRef::from_index(&data, face_index)
            .ok_or_else(|| anyhow::anyhow!("invalid sfnt font"))?;
        // SAFETY: `data` is owned by this struct and outlives the FontRef.
        let font: FontRef<'static> = unsafe { std::mem::transmute(font_ref) };
        let source_desc = id
            .and_then(|id| db.face(id))
            .map(|fi| {
                format!(
                    "system '{}' (index {face_index}, ps {:?})",
                    fi.families
                        .first()
                        .map(|f| &f.0)
                        .unwrap_or(&"?".to_string()),
                    fi.post_script_name
                )
            })
            .unwrap_or_else(|| "bundled JetBrainsMono-Regular".to_string());
        dlog!("font: family={family:?} -> {source_desc}");

        // Real weight/style faces: query the same family list so bold/italic
        // pick up the family's actual faces. If the family has no such face
        // (or fontdb hands back the regular one), keep None and synthesize.
        let load_face = |weight: Weight, style: Style, label: &str| -> Option<Face> {
            let fid = db.query(&Query {
                families: &fams,
                weight,
                stretch: Stretch::Normal,
                style,
            })?;
            if Some(fid) == id {
                return None;
            }
            let (fdata, fidx) = db.with_face_data(fid, |d, idx| (d.to_vec(), idx))?;
            let fr = FontRef::from_index(&fdata, fidx as usize)?;
            // SAFETY: `fdata` moves into the returned Face and outlives its FontRef.
            let fr: FontRef<'static> = unsafe { std::mem::transmute(fr) };
            let desc = db
                .face(fid)
                .map(|fi| {
                    format!(
                        "'{}' (index {fidx}, ps {:?})",
                        fi.families
                            .first()
                            .map(|f| &f.0)
                            .unwrap_or(&"?".to_string()),
                        fi.post_script_name
                    )
                })
                .unwrap_or_else(|| format!("face {fidx}"));
            dlog!("face: {label} -> {desc}");
            Some(Face {
                _data: fdata,
                font: fr,
            })
        };
        let bold = load_face(Weight::BOLD, Style::Normal, "bold");
        let italic = load_face(Weight::NORMAL, Style::Italic, "italic");
        let bold_italic = load_face(Weight::BOLD, Style::Italic, "bold-italic");
        if bold.is_none() {
            dlog!("face: no real bold in family; using synthetic embolden");
        }
        if italic.is_none() {
            dlog!("face: no real italic in family; using synthetic skew");
        }

        // Emoji/symbol/CJK fallbacks: consulted only for glyphs the primary lacks.
        const FALLBACK_FAMILIES: &[&str] = &[
            "Apple Color Emoji",
            "Segoe UI Emoji",
            "Noto Color Emoji",
            "Noto Emoji",
            "Noto Sans CJK SC",
            "Noto Sans Symbols 2",
            "Symbola",
            "DejaVu Sans",
        ];
        let mut seen: std::collections::HashSet<Option<fontdb::ID>> =
            std::collections::HashSet::new();
        seen.insert(id);
        let mut fallbacks = Vec::new();
        for name in FALLBACK_FAMILIES {
            if fallbacks.len() >= 4 {
                break;
            }
            let Some(fid) = db.query(&Query {
                families: &[Family::Name(name)],
                weight: Weight::NORMAL,
                stretch: Stretch::Normal,
                style: Style::Normal,
            }) else {
                continue;
            };
            if !seen.insert(Some(fid)) {
                continue;
            }
            let Some((fdata, fidx)) = db.with_face_data(fid, |d, idx| (d.to_vec(), idx)) else {
                continue;
            };
            let Some(fr) = FontRef::from_index(&fdata, fidx as usize) else {
                continue;
            };
            // SAFETY: `fdata` moves into FallbackFont below and outlives its FontRef.
            let fr: FontRef<'static> = unsafe { std::mem::transmute(fr) };
            dlog!("fallback: {name} (index {fidx})");
            fallbacks.push(FallbackFont {
                _data: fdata,
                font: fr,
                name: (*name).to_string(),
            });
        }

        Ok(Self {
            _data: data,
            font,
            bold,
            italic,
            bold_italic,
            fallbacks,
            size_px,
            line_height,
            family: family.to_string(),
            ctx: swash::scale::ScaleContext::new(),
            glyphs: HashMap::new(),
            metrics: Self::compute_metrics(&font, size_px, line_height),
        })
    }

    /// True when this context is already built for `family` and line height
    /// (size compared separately).
    pub fn matches(&self, family: &str, line_height: f32) -> bool {
        self.family == family && (self.line_height - line_height).abs() < f32::EPSILON
    }

    fn compute_metrics(font: &FontRef<'_>, size_px: f32, line_height: f32) -> Metrics {
        let m = font.metrics(&[]).scale(size_px);
        // swash: ascent = baseline->top (positive), descent = baseline->bottom
        // (negative; some faces report it positive, so normalize).
        let ascent = m.ascent;
        let descent = m.descent.abs();
        let cell_h = ((ascent + descent) * line_height.clamp(0.8, 3.0))
            .clamp(6.0, 256.0)
            .round();
        let gid = font.charmap().map('M');
        let cell_w = font
            .glyph_metrics(&[])
            .scale(size_px)
            .advance_width(gid)
            .clamp(cell_h * 0.25, cell_h * 2.0)
            .round();
        Metrics {
            cell_w,
            cell_h,
            ascent,
        }
    }

    /// Probe a handful of representative chars; part of BERTH_DEBUG triage output.
    pub fn raster_probe(&mut self, chars: &[char]) {
        if !debug_enabled() {
            return;
        }
        let mut ok = 0usize;
        for &c in chars {
            let gid = self.font.charmap().map(c);
            if gid == 0 {
                dlog!("probe '{c}' (U+{:04X}): not in font", c as u32);
                continue;
            }
            let mut scaler = self.ctx.builder(self.font).size(self.size_px).build();
            let sources = [
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ];
            let mut render = Render::new(&sources);
            let render = render.format(zeno::Format::Alpha);
            match render.render(&mut scaler, gid) {
                Some(image) => {
                    ok += 1;
                    dlog!(
                        "probe '{c}' (U+{:04X}): gid={gid} {}x{} content={:?}",
                        c as u32,
                        image.placement.width,
                        image.placement.height,
                        image.content
                    );
                }
                None => dlog!("probe '{c}' (U+{:04X}): gid={gid} RENDER FAILED", c as u32),
            }
        }
        dlog!("probe done: {ok}/{} rasterized", chars.len());
    }

    /// Update rasters after DPI/size change; invalidates the glyph cache.
    pub fn set_size(&mut self, size_px: f32) {
        if (size_px - self.size_px).abs() > 0.01 {
            self.size_px = size_px;
            self.metrics = Self::compute_metrics(&self.font, size_px, self.line_height);
            self.glyphs.clear();
            self.ctx = swash::scale::ScaleContext::new();
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Uniforms {
    /// Screen size in device px (y-down -> NDC flip in the shader).
    screen: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
    /// Normalized atlas coords; (0,0) for plain color quads.
    uv: [f32; 2],
    /// 0 = plain color, 1 = alpha coverage, 2 = color glyph.
    tex: u32,
    /// Device-px bounds (x0, y0, x1, y1) for the rounded-corner SDF.
    rect: [f32; 4],
    /// Corner radius in device px; 0 = square corners.
    radius: f32,
}

/// Per-frame quad accumulator; `dropped` counts budget refusals (logged once).
#[derive(Default)]
struct QuadSink {
    verts: Vec<Vertex>,
    uploads: Vec<Upload>,
    /// Quads refused this frame due to the budget (total, not per cell).
    dropped: usize,
}

/// Vertex budget: 32 MiB buffer / headroom / vertex size; 1 in 4 verts reserved
/// for chrome (tab bar, sidebar, outlines) so a full grid never starves chrome.
const QUAD_LIMIT: usize = (24 << 20) / std::mem::size_of::<Vertex>();

/// 4 thin quads tracing the inside of `r` (focused-pane outline).
fn draw_focus_outline(sink: &mut QuadSink, r: Rect, color: [f32; 4]) {
    let t = FOCUS_OUTLINE_PX;
    let (x0, y0) = (r.x as f32, r.y as f32);
    let (x1, y1) = (r.x as f32 + r.w as f32, r.y as f32 + r.h as f32);
    let (x1i, y1i) = (x1 - t, y1 - t);
    if x1 <= x0 + 2.0 * t || y1 <= y0 + 2.0 * t {
        return; // pane too small for an outline
    }
    sink.push(x0, y0, x1, y0 + t, color, None); // top
    sink.push(x0, y1i, x1, y1, color, None); // bottom
    sink.push(x0, y0 + t, x0 + t, y1i, color, None); // left
    sink.push(x1i, y0 + t, x1, y1i, color, None); // right
}

impl QuadSink {
    fn push(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        glyph: Option<GlyphRef>,
    ) {
        self.push_radius(x0, y0, x1, y1, color, glyph, 0.0);
    }

    /// Rounded-corner variant used by chrome (tabs, sidebar pills, badges).
    fn push_round(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 4], radius: f32) {
        self.push_radius(x0, y0, x1, y1, color, None, radius);
    }

    #[allow(clippy::too_many_arguments)]
    fn push_radius(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        glyph: Option<GlyphRef>,
        radius: f32,
    ) {
        if self.verts.len() + 6 > QUAD_LIMIT {
            if self.dropped == 0 {
                // Loud, once per frame: silent cell drops were a release bug.
                dlog!(
                    "quad budget exhausted at {} verts (grid too dense); dropping rest",
                    self.verts.len()
                );
            }
            self.dropped += 1;
            return;
        }
        let [r, g, b, a] = color;
        let (u0, v0, u1, v1, tex) = match glyph {
            Some(gl) => (
                gl.uv[0] / ATLAS_W as f32,
                gl.uv[1] / ATLAS_H as f32,
                (gl.uv[0] + gl.uv[2]) / ATLAS_W as f32,
                (gl.uv[1] + gl.uv[3]) / ATLAS_H as f32,
                if gl.color { 2u32 } else { 1u32 },
            ),
            None => (0.0, 0.0, 0.0, 0.0, 0u32),
        };
        let (bl, br, tl, tr) = (
            Vertex {
                pos: [x0, y1],
                color: [r, g, b, a],
                uv: [u0, v1],
                tex,
                rect: [x0, y0, x1, y1],
                radius,
            },
            Vertex {
                pos: [x1, y1],
                color: [r, g, b, a],
                uv: [u1, v1],
                tex,
                rect: [x0, y0, x1, y1],
                radius,
            },
            Vertex {
                pos: [x0, y0],
                color: [r, g, b, a],
                uv: [u0, v0],
                tex,
                rect: [x0, y0, x1, y1],
                radius,
            },
            Vertex {
                pos: [x1, y0],
                color: [r, g, b, a],
                uv: [u1, v0],
                tex,
                rect: [x0, y0, x1, y1],
                radius,
            },
        );
        // Triangle-list quad: (bl, br, tl) + (br, tr, tl).
        self.verts.extend([bl, br, tl, br, tr, tl]);
    }
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    atlas: Atlas,
    font: FontCtx,
    /// Active terminal colors (theme preset + overrides).
    scheme: ColorScheme,
    /// Cursor shape (block/beam/underline), from `[terminal] cursor_style`.
    cursor_style: berth_core::config::CursorStyle,
    quad_buf: wgpu::Buffer,
    /// Draw a small frame-time HUD in the corner (`BERTH_DEBUG=1`).
    hud: bool,
    /// Smoothed CPU frame time in ms (HUD only; not GPU time).
    ema_ms: f32,
}

impl Renderer {
    /// Create surface + pipeline; call once per window.
    pub fn new(
        window: &Window,
        family: &str,
        size_pt: f32,
        line_height: f32,
        theme: &berth_core::config::Theme,
    ) -> anyhow::Result<Self> {
        let scale = window.scale_factor();
        let size_px = size_pt * 1.333_333 * scale as f32;
        let mut font = FontCtx::new(family, size_px, line_height)?;
        let scheme = ColorScheme::from_config(theme);
        dlog!("theme: preset={:?} bg={:?}", theme.preset, scheme.bg);
        let phys = window.inner_size();
        dlog!(
            "renderer: scale={scale} size_px={size_px} metrics={:?} surface={}x{}",
            font.metrics,
            phys.width,
            phys.height
        );
        font.raster_probe(&['M', 'p', '%', '×']);

        // Env-overridable (WGPU_BACKEND etc. via with_env), display handled per-surface.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        // SAFETY: the caller keeps `window` alive (app holds it in a field) while
        // the surface lives inside the Renderer returned here. Unlike from_window(),
        // from_display_and_window() actually carries the display handle (wgpu 30
        // from_window() deliberately sets it to None).
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_display_and_window(
                window, window,
            )?)?
        };
        let adapter =
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })) {
                Ok(adapter) => adapter,
                Err(err) => {
                    dlog_emit(&format!(
                        "gpu: no high-performance adapter ({err}); retrying software fallback"
                    ));
                    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        force_fallback_adapter: true,
                        compatible_surface: Some(&surface),
                        ..Default::default()
                    }))?
                }
            };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        // Surface backend validation errors loudly: glyphs/atlas issues show up here.
        device.on_uncaptured_error(std::sync::Arc::new(|err| {
            eprintln!("wgpu uncaptured error: {err}");
        }));

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow::anyhow!("no matching surface configuration"))?;
        // The renderer's colors are linear (see `srgb_to_linear`), so the target
        // MUST be an sRGB format; otherwise every quad is written raw and the UI
        // looks washed out. get_default_config usually picks one, but some
        // backends/Windows compositions return a UNORM format first.
        let caps = surface.get_capabilities(&adapter);
        if !config.format.is_srgb()
            && let Some(fmt) = caps.formats.iter().copied().find(|f| f.is_srgb())
        {
            config.format = fmt;
        }
        let info = adapter.get_info();
        dlog_emit(&format!(
            "gpu: name={:?} backend={:?} type={:?} driver={:?} {}/{}",
            info.name, info.backend, info.device_type, info.driver, info.driver_info, info.vendor
        ));
        dlog_emit(&format!(
            "surface: format={:?} srgb={} present={:?} size={}x{}",
            config.format,
            config.format.is_srgb(),
            config.present_mode,
            config.width,
            config.height
        ));
        if info.device_type == wgpu::DeviceType::Cpu {
            eprintln!(
                "berth: adapter {:?} is software rendering ({:?}); install a GPU driver or set WGPU_BACKEND",
                info.name, info.backend
            );
        }
        if !config.format.is_srgb() {
            eprintln!(
                "berth: no sRGB surface format available ({:?}); colors may be washed out",
                config.format
            );
        }
        // COPY_SRC enables BERTH_FRAME_DUMP readbacks.
        config.usage |= wgpu::TextureUsages::COPY_SRC;

        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("berth-atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_W,
                height: ATLAS_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("berth-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("berth-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("berth-bind-group"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("berth-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let quad_capacity = 24 << 20;
        let quad_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("berth-quads"),
            size: quad_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("berth-pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("berth-pipeline-layout"),
                    bind_group_layouts: &[Some(&bind_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 24,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32,
                            offset: 32,
                            shader_location: 3,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 36,
                            shader_location: 4,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 52,
                            shader_location: 5,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            uniforms,
            atlas_tex,
            atlas: Atlas::new(),
            font,
            scheme,
            cursor_style: berth_core::config::CursorStyle::default(),
            quad_buf,
            hud: std::env::var_os("BERTH_DEBUG").is_some(),
            ema_ms: 0.0,
        })
    }

    /// Font metrics in device px.
    pub fn metrics(&self) -> Metrics {
        self.font.metrics
    }

    /// Hot font swap for config reload: rebuild the font context (fresh glyph
    /// cache) and rewind the atlas packer. Stale pixels get overwritten as the
    /// new glyphs upload at the same shelf positions.
    pub fn set_font(&mut self, family: &str, size_px: f32, line_height: f32) -> anyhow::Result<()> {
        if (size_px - self.font.size_px).abs() <= f32::EPSILON
            && self.font.matches(family, line_height)
        {
            return Ok(());
        }
        dlog!("set_font: family={family:?} size_px={size_px} line_height={line_height}");
        self.font = FontCtx::new(family, size_px, line_height)?;
        self.font.raster_probe(&['M', 'p', '%']);
        self.atlas.reset();
        Ok(())
    }

    /// Apply a theme from config (no font/atlas rebuild needed).
    pub fn set_theme(&mut self, theme: &berth_core::config::Theme) {
        self.scheme = ColorScheme::from_config(theme);
    }

    /// Cursor shape from `[terminal] cursor_style` (hot-reloadable).
    pub fn set_cursor_style(&mut self, style: berth_core::config::CursorStyle) {
        self.cursor_style = style;
    }

    pub fn resize_surface(&mut self, window: &Window) {
        let size = window.inner_size();
        if size.width > 0 && size.height > 0 {
            dlog!("resize_surface: {}x{}", size.width, size.height);
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Rasterize a close glyph ('×') centered in the given box.
    fn draw_close(
        &mut self,
        cx: f32,
        cy: f32,
        _metrics: Metrics,
        color: [f32; 4],
        sink: &mut QuadSink,
    ) {
        let Some(g) = self.rasterize('×', false, false, sink) else {
            dlog!("draw_close: '×' glyph unavailable");
            return;
        };
        // Center on the ink box, not on cell/ascent metrics: ink centers
        // differently per font (macOS font chain misaligned the other way).
        let gx = cx - g.uv[2] * 0.5;
        let gy = cy - g.uv[3] * 0.5;
        sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], color, Some(g));
    }

    /// Small "zoom" pill in the top-left of a zoomed pane so the full-bleed
    /// layout is distinguishable from a single-pane tab.
    fn draw_zoom_badge(&mut self, rect: Rect, ch: Chrome, metrics: Metrics, sink: &mut QuadSink) {
        let colors = self.scheme.chrome;
        let s = ch.scale;
        let label = "zoom";
        let text_w = label.chars().count() as f32 * metrics.cell_w;
        let bw = text_w + 12.0 * s;
        let bh = metrics.cell_h + 6.0 * s;
        let bx = rect.x as f32 + 8.0 * s;
        let by = rect.y as f32 + 8.0 * s;
        let mut bg = colors.focus_outline;
        bg[3] = 0.9;
        sink.push_round(bx, by, bx + bw, by + bh, bg, bh * 0.5);
        let baseline = by + 3.0 * s + metrics.ascent;
        let mut pen = bx + 6.0 * s;
        for c in label.chars() {
            let Some(g) = self.rasterize(c, false, false, sink) else {
                continue;
            };
            let gx = pen + g.left as f32;
            let gy = baseline - g.top as f32;
            sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], colors.panel_bg, Some(g));
            pen += metrics.cell_w;
        }
    }

    /// Overlay scrollbar + "scrolled back" badge for the focused pane. Only
    /// visible while the viewport is off the live bottom, so the grid stays
    /// clean during normal typing.
    fn draw_scroll_overlay(
        &mut self,
        rect: Rect,
        info: ScrollInfo,
        ch: Chrome,
        metrics: Metrics,
        sink: &mut QuadSink,
    ) {
        if info.offset == 0 || info.history == 0 || rect.w < 40 || rect.h < 40 {
            return;
        }
        let colors = self.scheme.chrome;
        let s = ch.scale;
        let (rx, ry) = (rect.x as f32, rect.y as f32);
        let (rw, rh) = (rect.w as f32, rect.h as f32);
        let pad = 8.0 * s;
        let track_w = 3.0 * s;
        let track_x = rx + rw - pad - track_w;
        let (track_y0, track_y1) = (ry + pad, ry + rh - pad);
        let track_h = (track_y1 - track_y0).max(1.0);
        let viewport = (rh / metrics.cell_h).max(1.0);
        let total = info.history as f32 + viewport;
        let thumb_h = (track_h * (viewport / total)).clamp(24.0 * s, track_h);
        let progress = (info.offset as f32 / info.history as f32).clamp(0.0, 1.0);
        let thumb_y0 = track_y1 - thumb_h - progress * (track_h - thumb_h);
        let mut track = colors.divider;
        track[3] = 0.55;
        sink.push(track_x, track_y0, track_x + track_w, track_y1, track, None);
        let mut thumb = colors.sidebar_text_active;
        thumb[3] = 0.8;
        sink.push_round(
            track_x,
            thumb_y0,
            track_x + track_w,
            thumb_y0 + thumb_h,
            thumb,
            track_w * 0.5,
        );

        // Pill badge centered at the top of the pane: "↑ 42".
        let label = format!("↑{}", info.offset);
        let text_w = label.chars().count() as f32 * metrics.cell_w;
        let bw = text_w + 12.0 * s;
        let bh = metrics.cell_h + 6.0 * s;
        let bx = rx + (rw - bw) * 0.5;
        let by = ry + pad;
        let mut bg = colors.sidebar_entry_active_bg;
        bg[3] = 0.94;
        sink.push_round(bx, by, bx + bw, by + bh, bg, bh * 0.5);
        let baseline = by + 3.0 * s + metrics.ascent;
        let mut pen = bx + 6.0 * s;
        for c in label.chars() {
            let Some(g) = self.rasterize(c, false, false, sink) else {
                continue;
            };
            let gx = pen + g.left as f32;
            let gy = baseline - g.top as f32;
            sink.push(
                gx,
                gy,
                gx + g.uv[2],
                gy + g.uv[3],
                colors.sidebar_text_active,
                Some(g),
            );
            pen += metrics.cell_w;
        }
    }

    /// Small top-right HUD (visible with `BERTH_DEBUG=1`): smoothed CPU draw
    /// time and quad count. GPU time is not reported (wgpu timestamps are off by
    /// default); this is a "is the CPU the bottleneck" indicator only.
    fn draw_hud(
        &mut self,
        label: &str,
        screen_w: f32,
        screen_h: f32,
        ch: Chrome,
        confirm_h: f32,
        sink: &mut QuadSink,
    ) {
        let metrics = self.font.metrics;
        let s = ch.scale;
        let text_w = label.chars().count() as f32 * metrics.cell_w;
        let bh = metrics.cell_h + 8.0 * s;
        let bw = text_w + 16.0 * s;
        let bx = (screen_w - bw - 8.0 * s).max(0.0);
        let by = (screen_h - bh - 8.0 * s - confirm_h).max(0.0);
        let mut bg = self.scheme.chrome.tab_active_bg;
        bg[3] = 0.92;
        sink.push_round(bx, by, bx + bw, by + bh, bg, bh * 0.5);
        let baseline = by + 4.0 * s + metrics.ascent;
        let mut pen = bx + 8.0 * s;
        let mut color = self.scheme.chrome.tab_text_active;
        color[3] = 0.85;
        for c in label.chars() {
            let Some(g) = self.rasterize(c, false, false, sink) else {
                continue;
            };
            let gx = pen + g.left as f32;
            let gy = baseline - g.top as f32;
            sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], color, Some(g));
            pen += metrics.cell_w;
        }
    }

    /// Floating top-center input showing the active search query and hit count.
    fn draw_search(
        &mut self,
        view: SearchView<'_>,
        sb_w: f32,
        screen_w: f32,
        ch: Chrome,
        metrics: Metrics,
        sink: &mut QuadSink,
    ) {
        let s = ch.scale;
        let colors = self.scheme.chrome;
        let bh = metrics.cell_h + 10.0 * s;
        let bw = (340.0 * s).min((screen_w - sb_w - 16.0 * s).max(80.0 * s));
        let bx = sb_w + ((screen_w - sb_w) - bw) * 0.5;
        let by = ch.tab_bar_h + 8.0 * s;
        let mut bg = colors.sidebar_entry_active_bg;
        bg[3] = 0.97;
        sink.push_round(bx, by, bx + bw, by + bh, bg, 6.0 * s);
        let total = view.matches.len();
        let label = search_label(view.query, view.current, total);
        let max_chars = ((bw - 16.0 * s) / metrics.cell_w).floor().max(1.0) as usize;
        let text: String = label.chars().take(max_chars).collect();
        let color = if total == 0 {
            colors.tab_text
        } else {
            colors.tab_text_active
        };
        let baseline = by + (bh - metrics.cell_h) * 0.5 + metrics.ascent;
        let mut pen = bx + 8.0 * s;
        for c in text.chars() {
            let Some(g) = self.rasterize(c, false, false, sink) else {
                continue;
            };
            let gx = pen + g.left as f32;
            let gy = baseline - g.top as f32;
            sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], color, Some(g));
            pen += metrics.cell_w;
        }
    }

    /// Centered bottom overlay strip (quit confirmation). Drawn last so it sits
    /// on top of panes/tabs/sidebar.
    fn draw_confirm(
        &mut self,
        text: &str,
        screen_w: f32,
        screen_h: f32,
        ch: Chrome,
        sink: &mut QuadSink,
    ) {
        let metrics = self.font.metrics;
        let colors = self.scheme.chrome;
        let y0 = (screen_h - ch.confirm_h).max(0.0);
        sink.push(0.0, y0, screen_w, screen_h, colors.confirm_bg, None);
        let text_w = text.chars().count() as f32 * metrics.cell_w;
        let mut pen = ((screen_w - text_w) * 0.5).max(8.0);
        let baseline = y0 + (ch.confirm_h - metrics.cell_h) * 0.5 + metrics.ascent;
        for c in text.chars() {
            let Some(g) = self.rasterize(c, false, false, sink) else {
                continue;
            };
            let gx = pen + g.left as f32;
            let gy = baseline - g.top as f32;
            sink.push(
                gx,
                gy,
                gx + g.uv[2],
                gy + g.uv[3],
                colors.confirm_text,
                Some(g),
            );
            pen += metrics.cell_w;
        }
    }

    /// Draw the top tab strip (starting right of the sidebar): one fixed-width
    /// slot per tab, active slot brighter, plus a `+` button to the right.
    #[allow(clippy::too_many_arguments)]
    fn draw_tab_bar(
        &mut self,
        tabs: &[(&str, bool, bool)],
        metrics: Metrics,
        ch: Chrome,
        sb_w: f32,
        hovered: Option<ChromeZone>,
        status: &[String],
        sink: &mut QuadSink,
    ) {
        let bar_w = self.config.width as f32 - sb_w;
        if bar_w <= 0.0 {
            return;
        }
        let colors = self.scheme.chrome;
        let s = ch.scale;
        let tab_w = tab_width(bar_w, tabs.len(), ch.tab_w, s);
        sink.push(sb_w, 0.0, sb_w + bar_w, ch.tab_bar_h, colors.bar_bg, None);
        // Vertical centering: text baseline sits half a cell below the strip middle.
        let baseline = (ch.tab_bar_h - metrics.cell_h) * 0.5 + metrics.ascent;
        for (i, (title, active, attention)) in tabs.iter().enumerate() {
            let x0 = sb_w + i as f32 * tab_w;
            let is_hovered = matches!(hovered, Some(ChromeZone::Tab(idx) | ChromeZone::TabClose(idx)) if idx == i);
            let mut bg = if *active {
                colors.tab_active_bg
            } else {
                colors.tab_bg
            };
            if is_hovered && !*active {
                bg = colors.tab_hover_bg;
            }
            // Pill-shaped slots: small inset + radius keeps the strip airy.
            let (pad_x, pad_y) = (3.0 * s, 4.0 * s);
            sink.push_round(
                x0 + pad_x,
                pad_y,
                x0 + tab_w - pad_x,
                ch.tab_bar_h - pad_y,
                bg,
                5.0 * s,
            );
            let max_chars =
                (((tab_w - ch.close_w - 20.0 * s) / metrics.cell_w).floor() as usize).max(1);
            let text: String = title.chars().take(max_chars).collect();
            let mut pen = x0 + 10.0 * s;
            for c in text.chars() {
                let Some(g) = self.rasterize(c, false, false, sink) else {
                    continue;
                };
                let gx = pen + g.left as f32;
                let gy = baseline - g.top as f32;
                let mut color = if *active {
                    colors.tab_text_active
                } else {
                    colors.tab_text
                };
                if is_hovered && !*active {
                    color = colors.text_hover;
                }
                sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], color, Some(g));
                pen += metrics.cell_w;
            }
            // Reveal ✕ on the active or hovered tab only (no layout shift).
            let close_hovered = matches!(hovered, Some(ChromeZone::TabClose(idx)) if idx == i);
            if *active || is_hovered {
                let close_color = if close_hovered {
                    colors.tab_text_active
                } else {
                    colors.tab_text
                };
                self.draw_close(
                    x0 + tab_w - pad_x - ch.close_w * 0.5,
                    ch.tab_bar_h * 0.5,
                    metrics,
                    close_color,
                    sink,
                );
            }
            // Bell attention dot sits left of the (possibly hidden) ✕ so it
            // never shifts the label or the close target.
            if *attention {
                let r = 3.0 * s;
                let gap = if *active || is_hovered {
                    ch.close_w + 8.0 * s
                } else {
                    8.0 * s
                };
                let cx = x0 + tab_w - pad_x - gap - r;
                let cy = ch.tab_bar_h * 0.5;
                sink.push_round(cx - r, cy - r, cx + r, cy + r, colors.focus_outline, r);
            }
        }
        // `+` new-tab button when the strip has room for it.
        let plus_x = sb_w + tabs.len() as f32 * tab_w;
        if plus_x + ch.plus_w <= sb_w + bar_w {
            let is_plus_hovered = matches!(hovered, Some(ChromeZone::TabPlus));
            let plus_color = if is_plus_hovered {
                colors.tab_text_active
            } else {
                colors.tab_text
            };
            self.draw_char_c(
                '+',
                plus_x + ch.plus_w * 0.5,
                ch.tab_bar_h * 0.5,
                metrics,
                plus_color,
                sink,
            );
        }

        // Plugin status segments: right-aligned in the strip, clipped from the
        // left so long guest text never covers the tabs or the `+` button.
        if status.is_empty() {
            return;
        }
        let label = status.join("  ·  ");
        let right = sb_w + bar_w - 8.0 * ch.scale;
        let left_limit = (plus_x + ch.plus_w + 8.0 * ch.scale).min(right);
        let max_chars = ((right - left_limit) / metrics.cell_w).floor() as usize;
        if max_chars == 0 {
            return;
        }
        let chars: Vec<char> = label.chars().collect();
        let start = chars.len().saturating_sub(max_chars);
        let text: String = chars[start..].iter().collect();
        let text_w = text.chars().count() as f32 * metrics.cell_w;
        let mut pen = right - text_w;
        let baseline = (ch.tab_bar_h - metrics.cell_h) * 0.5 + metrics.ascent;
        for c in text.chars() {
            let Some(g) = self.rasterize(c, false, false, sink) else {
                continue;
            };
            let gx = pen + g.left as f32;
            let gy = baseline - g.top as f32;
            sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], colors.tab_text, Some(g));
            pen += metrics.cell_w;
        }
    }

    /// Draw the left sidebar: workspace entries, active highlighted.
    fn draw_sidebar(
        &mut self,
        workspaces: &[(&str, bool)],
        sb_w: f32,
        ch: Chrome,
        metrics: Metrics,
        hovered: Option<ChromeZone>,
        sink: &mut QuadSink,
    ) {
        if sb_w <= 0.0 {
            return;
        }
        let full_h = self.config.height as f32;
        let colors = self.scheme.chrome;
        sink.push(0.0, 0.0, sb_w, full_h, colors.sidebar_bg, None);
        for (i, (label, active)) in workspaces.iter().enumerate() {
            let y0 = ch.sidebar_top + i as f32 * ch.sidebar_entry_h;
            let is_hovered = matches!(hovered, Some(ChromeZone::SidebarEntry(idx) | ChromeZone::SidebarClose(idx)) if idx == i);
            let mut bg = if *active {
                colors.sidebar_entry_active_bg
            } else {
                colors.sidebar_entry_bg
            };
            if is_hovered && !*active {
                bg = colors.sidebar_entry_hover_bg;
            }
            let s = ch.scale;
            sink.push_round(
                6.0 * s,
                y0 + 2.0 * s,
                sb_w - 6.0 * s,
                y0 + ch.sidebar_entry_h - 2.0 * s,
                bg,
                6.0 * s,
            );
            let baseline = (ch.sidebar_entry_h - metrics.cell_h) * 0.5 + metrics.ascent;
            let baseline = y0 + baseline;
            let max_chars = (((sb_w - 2.0 * ch.sidebar_pad - ch.close_w) / metrics.cell_w).floor()
                as usize)
                .max(1);
            let text: String = label.chars().take(max_chars).collect();
            let mut pen = ch.sidebar_pad;
            for c in text.chars() {
                let Some(g) = self.rasterize(c, false, false, sink) else {
                    continue;
                };
                let gx = pen + g.left as f32;
                let gy = baseline - g.top as f32;
                let mut color = if *active {
                    colors.sidebar_text_active
                } else {
                    colors.sidebar_text
                };
                if is_hovered && !*active {
                    color = colors.text_hover;
                }
                sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], color, Some(g));
                pen += metrics.cell_w;
            }
            // ✕ only on the active or hovered row.
            let close_hovered = matches!(hovered, Some(ChromeZone::SidebarClose(idx)) if idx == i);
            if *active || is_hovered {
                let close_color = if close_hovered {
                    colors.sidebar_text_active
                } else {
                    colors.sidebar_text
                };
                self.draw_close(
                    sb_w - 6.0 * s - ch.close_w * 0.5,
                    y0 + ch.sidebar_entry_h * 0.5,
                    metrics,
                    close_color,
                    sink,
                );
            }
        }
        // `+` new-workspace button: one free row band below the last entry.
        let plus_y = ch.sidebar_top + workspaces.len() as f32 * ch.sidebar_entry_h;
        if plus_y + ch.sidebar_entry_h <= full_h {
            let is_plus_hovered = matches!(hovered, Some(ChromeZone::SidebarPlus));
            let plus_color = if is_plus_hovered {
                colors.sidebar_text_active
            } else {
                colors.sidebar_text
            };
            self.draw_char_c(
                '+',
                ch.sidebar_pad + ch.close_w * 0.5,
                plus_y + ch.sidebar_entry_h * 0.5,
                metrics,
                plus_color,
                sink,
            );
        }
    }

    /// Draw `c` ink-box-centered at (cx, cy) — the button-glyph path ('+').
    fn draw_char_c(
        &mut self,
        c: char,
        cx: f32,
        cy: f32,
        _metrics: Metrics,
        color: [f32; 4],
        sink: &mut QuadSink,
    ) {
        let Some(g) = self.rasterize(c, false, false, sink) else {
            return;
        };
        let gx = cx - g.uv[2] * 0.5;
        let gy = cy - g.uv[3] * 0.5;
        sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], color, Some(g));
    }

    /// Rasterize `c` (cached) from the primary font or fallbacks; queues the atlas upload on first use.
    /// `bold`/`italic` use swash's faux transforms (real faces are a later pass).
    fn rasterize(
        &mut self,
        c: char,
        bold: bool,
        italic: bool,
        sink: &mut QuadSink,
    ) -> Option<GlyphRef> {
        let key = GlyphKey { c, bold, italic };
        if let Some(cached) = self.font.glyphs.get(&key).copied() {
            return cached;
        }
        let fontc = &mut self.font;
        let sources = [
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ];
        // Prefer the family's real faces; synthesize only the styles it lacks.
        let face = match (bold, italic) {
            (false, false) => fontc.font,
            (true, false) => fontc.bold.as_ref().map(|f| f.font).unwrap_or(fontc.font),
            (false, true) => fontc.italic.as_ref().map(|f| f.font).unwrap_or(fontc.font),
            (true, true) => fontc
                .bold_italic
                .as_ref()
                .or(fontc.bold.as_ref())
                .or(fontc.italic.as_ref())
                .map(|f| f.font)
                .unwrap_or(fontc.font),
        };
        let faux_bold = bold && fontc.bold.is_none();
        let faux_italic = italic && fontc.italic.is_none();
        // FontRefs are Copy; snapshot the whole chain (primary first).
        // Chain snapshot taken via self.font.fallbacks to keep fontc's mut
        // field borrow disjoint (fontc = self.font, fallbacks = sibling field).
        let fonts: Vec<(FontRef<'static>, Option<String>)> = {
            let mut chain = vec![(face, None)];
            chain.extend(
                fontc
                    .fallbacks
                    .iter()
                    .map(|f| (f.font, Some(f.name.clone()))),
            );
            chain
        };
        let mut found = None;
        for (font, fname) in fonts {
            let gid = font.charmap().map(c);
            if gid == 0 {
                continue;
            }
            let mut scaler = fontc.ctx.builder(font).size(fontc.size_px).build();
            let mut render = Render::new(&sources);
            render.format(zeno::Format::Alpha);
            if faux_bold {
                // Faux bold: swash expands the scaled outline; ~1px at 16px.
                render.embolden((fontc.size_px * 0.055).clamp(0.5, 2.5));
            }
            if faux_italic {
                // Faux italic: shear the outline ~12° when no italic face exists.
                render.transform(Some(zeno::Transform::skew(
                    zeno::Angle::from_degrees(12.0),
                    zeno::Angle::ZERO,
                )));
            }
            let Some(image) = render.render(&mut scaler, gid) else {
                continue;
            };
            let is_color = matches!(image.content, Content::Color);
            let bytes = if is_color {
                image.data.clone()
            } else {
                image
                    .data
                    .iter()
                    .flat_map(|&a| [255u8, 255, 255, a])
                    .collect::<Vec<u8>>()
            };
            let w = image.placement.width;
            let h = image.placement.height;
            if w == 0 || h == 0 {
                continue;
            }
            match self.atlas.pack(w, h) {
                Some([px, py]) => {
                    let g = GlyphRef {
                        uv: [px as f32, py as f32, w as f32, h as f32],
                        left: image.placement.left,
                        top: image.placement.top,
                        color: is_color,
                    };
                    sink.uploads.push(Upload {
                        x: px,
                        y: py,
                        w,
                        h,
                        bytes,
                    });
                    found = Some(g);
                    if let Some(name) = fname {
                        dlog!("glyph '{}' (U+{:04X}) via fallback {name}", c, c as u32);
                    }
                }
                None => {
                    // Atlas exhausted: reset packer; glyph cache cleared by reset().
                    self.atlas.reset();
                    fontc.glyphs.clear();
                }
            }
            if found.is_some() {
                break;
            }
        }
        fontc.glyphs.insert(key, found);
        found
    }

    /// Full frame: cell quads per pane + chrome. Drawn on RedrawRequested.
    #[allow(clippy::too_many_arguments)] // draw args are the frame's scene inputs
    pub fn draw(
        &mut self,
        panes: &[(&Term<TermListener>, Rect)],
        dividers: &[Rect],
        tabs: &[(&str, bool, bool)],
        workspaces: &[(&str, bool)],
        sb_w: f32,
        focus: Option<Rect>,
        scroll: Option<ScrollInfo>,
        cursor: CursorPaint,
        search: Option<SearchView<'_>>,
        link: Option<(Rect, LinkSpan)>,
        zoomed: bool,
        hovered: Option<ChromeZone>,
        status: &[String],
        confirm: Option<&str>,
        notice: Option<&str>,
        preedit: Option<&str>,
        window: &Window,
    ) -> anyhow::Result<()> {
        let frame_start = std::time::Instant::now();
        let phys = window.inner_size();
        let screen_w = phys.width as f32;
        let screen_h = phys.height as f32;
        self.queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&Uniforms {
                screen: [screen_w, screen_h],
                _pad: [0.0; 2],
            }),
        );

        let metrics = self.font.metrics;
        let pane_count = panes.len();
        dlog!(
            "draw: screen={screen_w}x{screen_h} cell={0:.1}x{1:.1} panes={pane_count}",
            metrics.cell_w,
            metrics.cell_h,
        );
        let quad_trace = std::env::var("BERTH_TRACE").ok().as_deref() == Some("1");

        let ch = Chrome::new(window.scale_factor());
        let ch = Chrome {
            tab_w: tab_width(screen_w - sb_w, tabs.len(), ch.tab_w, ch.scale),
            ..ch
        };
        let mut sink = QuadSink::default();
        self.draw_sidebar(workspaces, sb_w, ch, metrics, hovered, &mut sink);
        self.draw_tab_bar(tabs, metrics, ch, sb_w, hovered, status, &mut sink);
        let mut uploads_pending = 0usize;
        let mut quads_bg = 0usize;
        let mut quads_glyph = 0usize;
        for (term, rect) in panes {
            let paint = CursorPaint {
                show: focus == Some(*rect),
                ..cursor
            };
            let pane_search = if focus == Some(*rect) { search } else { None };
            let pane_link = link
                .as_ref()
                .filter(|(r, _)| *r == *rect)
                .map(|(_, l)| l.clone());
            let (n_g, n_b, n_u) = self.draw_term(
                term,
                *rect,
                metrics,
                quad_trace,
                paint,
                pane_search,
                pane_link,
                if focus == Some(*rect) { preedit } else { None },
                &mut sink,
            );
            quads_glyph += n_g;
            quads_bg += n_b;
            uploads_pending += n_u;
        }
        let _ = uploads_pending; // uploads are queued inside push paths below
        for d in dividers {
            sink.push(
                d.x as f32,
                d.y as f32,
                (d.x + d.w) as f32,
                (d.y + d.h) as f32,
                self.scheme.chrome.divider,
                None,
            );
        }
        if let Some(r) = focus {
            draw_focus_outline(&mut sink, r, self.scheme.chrome.focus_outline);
            if let Some(info) = scroll {
                self.draw_scroll_overlay(r, info, ch, metrics, &mut sink);
            }
            if zoomed {
                self.draw_zoom_badge(r, ch, metrics, &mut sink);
            }
        }
        if let Some(view) = search {
            self.draw_search(view, sb_w, screen_w, ch, metrics, &mut sink);
        }
        if self.hud {
            let label = format!("{:.1} ms · {} quads", self.ema_ms, sink.verts.len() / 6);
            let confirm_h = if confirm.is_some() || notice.is_some() {
                ch.confirm_h
            } else {
                0.0
            };
            self.draw_hud(&label, screen_w, screen_h, ch, confirm_h, &mut sink);
        }
        // Confirm banner last: panes skip default-bg quads, so an earlier banner
        // showed through the grid but lost to any glyph/selection painted over it.
        // Transient notices (e.g. a shell exiting non-zero) use the same strip but
        // yield to an active confirmation.
        if let Some(msg) = confirm {
            self.draw_confirm(msg, screen_w, screen_h, ch, &mut sink);
        } else if let Some(msg) = notice {
            self.draw_confirm(msg, screen_w, screen_h, ch, &mut sink);
        }

        dlog!(
            "draw done: bg_quads={} glyph_quads={} uploads={} total_verts={}",
            quads_bg,
            quads_glyph,
            sink.uploads.len(),
            sink.verts.len()
        );

        // Upload newly rastered glyphs.
        for u in &sink.uploads {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: u.x,
                        y: u.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &u.bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(u.w * 4),
                    rows_per_image: Some(u.h),
                },
                wgpu::Extent3d {
                    width: u.w,
                    height: u.h,
                    depth_or_array_layers: 1,
                },
            );
        }

        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(t) => t,
            CurrentSurfaceTexture::Suboptimal(t) => t,
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return Ok(()),
            CurrentSurfaceTexture::Outdated => {
                // Window resize/scale change raced the acquire: reconfigure and
                // let the next redraw present normally.
                dlog!("surface outdated: reconfiguring");
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            _ => anyhow::bail!("surface texture lost/invalid"),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("berth-frame"),
            });

        if !sink.verts.is_empty() {
            self.queue
                .write_buffer(&self.quad_buf, 0, bytemuck::cast_slice(&sink.verts));
        }

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("berth-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: self.scheme.bg[0] as f64,
                            g: self.scheme.bg[1] as f64,
                            b: self.scheme.bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.quad_buf.slice(..));
            let n = sink.verts.len() as u32;
            if n > 0 {
                pass.draw(0..n, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        let ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        self.ema_ms = if self.ema_ms <= 0.0 {
            ms
        } else {
            self.ema_ms * 0.9 + ms * 0.1
        };

        if frame_dump_enabled() {
            let (w, h) = (self.config.width, self.config.height);
            let padded = (w * 4).div_ceil(256) * 256;
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("berth-frame-dump"),
                size: padded as u64 * h as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("berth-frame-dump-enc"),
                });
            enc.copy_texture_to_buffer(
                frame.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(Some(enc.finish()));

            // flush copy, map, read back (debug only)
            let slice = buf.slice(..);
            let (txm, rxm) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |_| {
                let _ = txm.send(());
            });
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(2)),
            });
            let _ = rxm.recv_timeout(std::time::Duration::from_secs(2));
            let data = slice
                .get_mapped_range()
                .map_err(|e| anyhow::anyhow!("frame dump map failed: {e}"))?;
            let fx = format!("/tmp/opencode/berth-frame-{}.ppm", frame_dump_index());
            if let Ok(mut f) = std::fs::File::create(&fx) {
                use std::io::Write;
                let _ = write!(f, "P6\n{w} {h}\n255\n");
                for y in 0..h as usize {
                    let row = &data[y * padded as usize..][..(w * 4) as usize];
                    for px in row.as_chunks::<4>().0 {
                        // surface format (bgra) -> ppm rgb, expected srgb bytes
                        let _ = f.write_all(&[px[2], px[1], px[0]]);
                    }
                }
            }
            drop(data);
            buf.unmap();
            dlog!("frame dumped: {fx}");
        }
        self.queue.present(frame);
        Ok(())
    }

    /// Emit one visible pane grid into `sink` (translated by `rect`).
    /// Returns (glyph_quads, bg_quads, unused-upload placer).
    #[allow(clippy::too_many_arguments)]
    fn draw_term(
        &mut self,
        term: &Term<TermListener>,
        rect: Rect,
        metrics: Metrics,
        quad_trace: bool,
        cursor: CursorPaint,
        search: Option<SearchView<'_>>,
        link: Option<LinkSpan>,
        preedit: Option<&str>,
        sink: &mut QuadSink,
    ) -> (usize, usize, usize) {
        let content = term.renderable_content();
        dlog!(
            "term grid: {}x{} display_offset={}",
            term.grid().columns(),
            term.grid().screen_lines(),
            content.display_offset
        );
        let mut dump_rows: Vec<String> = Vec::new();
        let mut dump_cur: Option<(usize, String)> = None; // (row, chars)
        let cursor_vp = point_to_viewport(content.display_offset, content.cursor.point);
        let mut quads_bg = 0usize;
        let mut quads_glyph = 0usize;

        let cols = (rect.w as f32 / metrics.cell_w).floor().max(1.0) as usize;
        let rows = (rect.h as f32 / metrics.cell_h).floor().max(1.0) as usize;

        // Visible search hits per absolute line: (start, end, is_current).
        let mut search_rows: HashMap<i32, Vec<(usize, usize, bool)>> = HashMap::new();
        if let Some(view) = search {
            for (i, m) in view.matches.iter().enumerate() {
                let row = m.line + content.display_offset as i32;
                if row >= 0 && row < rows as i32 {
                    search_rows.entry(m.line).or_default().push((
                        m.start,
                        m.end,
                        i == view.current,
                    ));
                }
            }
        }

        // Row-major iteration over the visible viewport cells.
        let scheme = self.scheme;
        let mut cells = content.display_iter;
        for idx in 0..(rows * cols) {
            let Some(indexed) = cells.next() else { break };
            let cell = indexed.cell;
            let row = idx / cols;
            let col = idx % cols;
            let px = rect.x as f32 + col as f32 * metrics.cell_w;
            let py = rect.y as f32 + row as f32 * metrics.cell_h;

            // bg (respect selection tint + SGR reverse video).
            let mut fg = scheme.color_of(cell.fg);
            if cell.flags.contains(Flags::DIM) {
                fg = dim(fg);
            }
            let mut bg = scheme.color_of(cell.bg);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut is_default_bg = bg == scheme.bg;
            let selected = content
                .selection
                .as_ref()
                .map(|sel| {
                    let pt =
                        viewport_to_point(content.display_offset, Point::new(row, Column(col)));
                    sel.contains(pt)
                })
                .unwrap_or(false);
            if selected {
                bg = scheme.blend(bg);
                is_default_bg = false;
            }
            if !is_default_bg {
                sink.push(px, py, px + metrics.cell_w, py + metrics.cell_h, bg, None);
                quads_bg += 1;
            }
            let line = indexed.point.line.0;
            if let Some(ranges) = search_rows.get(&line)
                && let Some(&(_, _, is_current)) =
                    ranges.iter().find(|(s, e, _)| col >= *s && col < *e)
            {
                let mut tint = if is_current {
                    scheme.chrome.focus_outline
                } else {
                    [0.82, 0.68, 0.12, 1.0]
                };
                tint[3] = if is_current { 0.45 } else { 0.26 };
                sink.push(px, py, px + metrics.cell_w, py + metrics.cell_h, tint, None);
                quads_bg += 1;
            }
            if std::env::var("BERTH_DUMP").ok().as_deref() == Some("1") {
                match &mut dump_cur {
                    Some((r, chars)) if *r == row => {}
                    _ => {
                        if let Some((r, chars)) = dump_cur.take() {
                            dump_rows.push(format!("[{r:02}] |{chars}|"));
                        }
                        dump_cur = Some((row, String::new()));
                    }
                }
                if let Some((_, chars)) = &mut dump_cur {
                    chars.push(cell.c);
                }
            }

            let c = cell.c;
            // cursor: block drawn under the glyph (shown here in inverse). Must be
            // handled before the empty-cell skip so the block shows on blank rows.
            if selected {
                fg = scheme.blend(fg);
            }
            let on_cursor = cursor.show
                && cursor.blink_on
                && cursor_vp
                    .map(|vp| vp.line == row && vp.column.0 == col)
                    .unwrap_or(false);
            let solid_block = on_cursor
                && cursor.window_focused
                && self.cursor_style == berth_core::config::CursorStyle::Block;
            if on_cursor {
                let (x0, y0, x1, y1) =
                    cursor_rect(self.cursor_style, px, py, metrics.cell_w, metrics.cell_h);
                if solid_block {
                    sink.push(x0, y0, x1, y1, scheme.cursor, None);
                } else {
                    // Hollow block when the window is unfocused; bars otherwise.
                    let mut color = scheme.cursor;
                    color[3] = 0.7;
                    if self.cursor_style == berth_core::config::CursorStyle::Block {
                        let t = (metrics.cell_w * 0.1).max(1.0);
                        sink.push(x0, y0, x1, y0 + t, color, None);
                        sink.push(x0, y1 - t, x1, y1, color, None);
                        sink.push(x0, y0 + t, x0 + t, y1 - t, color, None);
                        sink.push(x1 - t, y0 + t, x1, y1 - t, color, None);
                    } else {
                        sink.push(x0, y0, x1, y1, color, None);
                    }
                }
            }
            if c == ' '
                || c == '\0'
                || cell.flags.contains(Flags::HIDDEN)
                || cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            {
                continue;
            }
            fg = if solid_block { scheme.bg } else { fg };

            let bold = cell.flags.intersects(Flags::BOLD | Flags::DIM_BOLD);
            let italic = cell.flags.intersects(Flags::ITALIC | Flags::BOLD_ITALIC);
            if let Some(g) = self.rasterize(c, bold, italic, sink) {
                // swash origin = (pen_x, baseline_y), y-up: the image's top edge
                // sits `top` px above the baseline, `left` px right of the pen.
                let gx = px + g.left as f32;
                let gy = py + metrics.ascent - g.top as f32;
                sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], fg, Some(g));
                quads_glyph += 1;
                if quad_trace && row < 4 {
                    dlog!(
                        "quad [{row},{col}] '{c}' gx={gx:.2} gy={gy:.2} gx2={:.2} gy2={:.2} tex={}",
                        gx + g.uv[2],
                        gy + g.uv[3],
                        if g.color { 2u32 } else { 1u32 }
                    );
                }
            } else if debug_enabled() {
                dlog!("char '{}' (U+{:04X}) has no glyph quad", c, c as u32);
            }

            // SGR 4/21/24/53: underline and strikeout rects in the text color.
            let thickness = (metrics.cell_h * 0.055).max(1.0);
            let baseline = py + metrics.ascent;
            let link_hovered = link
                .as_ref()
                .is_some_and(|l| l.row == row && col >= l.start && col < l.end);
            if cell.flags.intersects(Flags::ALL_UNDERLINES)
                || cell.hyperlink().is_some()
                || link_hovered
            {
                let uy = (baseline + thickness).min(py + metrics.cell_h - thickness);
                sink.push(px, uy, px + metrics.cell_w, uy + thickness, fg, None);
                if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                    let uy2 = uy + thickness * 1.8;
                    sink.push(px, uy2, px + metrics.cell_w, uy2 + thickness, fg, None);
                }
                quads_bg += 1;
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                let sy = py + metrics.cell_h * 0.55;
                sink.push(px, sy, px + metrics.cell_w, sy + thickness, fg, None);
                quads_bg += 1;
            }
        }

        // IME composition: draw the pending text at the cursor cell with an
        // accent underline so the candidate/preedit is visible before commit.
        if let Some(text) = preedit
            && !text.is_empty()
            && let Some(vp) = cursor_vp
        {
            let px = rect.x as f32 + vp.column.0 as f32 * metrics.cell_w;
            let py = rect.y as f32 + vp.line as f32 * metrics.cell_h;
            let tw = (text.chars().count() as f32 * metrics.cell_w).max(metrics.cell_w);
            sink.push(
                px,
                py,
                px + tw,
                py + metrics.cell_h,
                scheme.chrome.sidebar_entry_active_bg,
                None,
            );
            let baseline = py + metrics.ascent;
            for (i, c) in text.chars().enumerate() {
                let Some(g) = self.rasterize(c, false, false, sink) else {
                    continue;
                };
                let gx = px + i as f32 * metrics.cell_w + g.left as f32;
                let gy = baseline - g.top as f32;
                sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], scheme.fg, Some(g));
            }
            let t = (metrics.cell_h * 0.06).max(1.5);
            sink.push(
                px,
                py + metrics.cell_h - t,
                px + tw,
                py + metrics.cell_h,
                scheme.chrome.focus_outline,
                None,
            );
        }

        if !dump_rows.is_empty() {
            if let Some((r, chars)) = dump_cur.take() {
                dump_rows.push(format!("[{r:02}] |{chars}|"));
            }
            for line in &dump_rows {
                dlog!("{line}");
            }
        }
        (quads_glyph, quads_bg, sink.uploads.len())
    }
}

fn dim(c: [f32; 4]) -> [f32; 4] {
    [
        c[0] * DIM_FACTOR,
        c[1] * DIM_FACTOR,
        c[2] * DIM_FACTOR,
        c[3],
    ]
}

/// Shader source exposed for offline WGSL validation tests.
pub fn shader_source() -> &'static str {
    SHADER
}

const SHADER: &str = r#"struct Uniforms {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VertexInput {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tex: u32,
    @location(4) rect: vec4<f32>,
    @location(5) radius: f32,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) @interpolate(flat) tex: u32,
    @location(2) uv: vec2<f32>,
    @location(3) @interpolate(flat) rect: vec4<f32>,
    @location(4) @interpolate(flat) radius: f32,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    // CPU coords are device px, y-down; flip into NDC.
    let x = 2.0 * in.pos.x / u.screen.x - 1.0;
    let y = 1.0 - 2.0 * in.pos.y / u.screen.y;
    let clip = vec4<f32>(x, y, 0.0, 1.0);
    // naga: struct construction is parenthesized (positional, declaration order).
    return VertexOutput(clip, in.color, in.tex, in.uv, in.rect, in.radius);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (in.tex == 1u) {
        let cov = textureSample(atlas, samp, in.uv).a;
        return vec4<f32>(in.color.rgb, in.color.a * cov);
    } else if (in.tex == 2u) {
        let t = textureSample(atlas, samp, in.uv);
        return vec4<f32>(t.rgb, t.a * in.color.a);
    } else {
        // Rounded-rect SDF: distance from the quadrant-inset box, 1px AA.
        var alpha = in.color.a;
        if (in.radius > 0.0) {
            let half = (in.rect.zw - in.rect.xy) * 0.5;
            let center = in.rect.xy + half;
            let r = min(in.radius, min(half.x, half.y));
            let q = abs(in.clip.xy - center) - half + vec2<f32>(r, r);
            let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
            alpha = alpha * clamp(0.5 - d, 0.0, 1.0);
        }
        return vec4<f32>(in.color.rgb, alpha);
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn search_label_shows_hit_position_or_zero() {
        assert_eq!(search_label("needle", 0, 0), "find: needle   0/0");
        assert_eq!(search_label("needle", 2, 7), "find: needle   3/7");
    }

    #[test]
    fn cursor_shapes_are_inset_bars_for_beam_and_underline() {
        use berth_core::config::CursorStyle;
        assert_eq!(
            cursor_rect(CursorStyle::Block, 10.0, 20.0, 10.0, 20.0),
            (10.0, 20.0, 20.0, 40.0)
        );
        let (x0, _, x1, _) = cursor_rect(CursorStyle::Beam, 10.0, 20.0, 10.0, 20.0);
        assert_eq!((x0, x1), (10.0, 12.0), "beam is max(12% width, 2px)");
        let (_, y0, _, y1) = cursor_rect(CursorStyle::Underline, 10.0, 20.0, 10.0, 20.0);
        assert_eq!((y0, y1), (38.0, 40.0), "underline hugs the cell bottom");
    }

    #[test]
    fn srgb_linear_endpoints_and_midpoint() {
        assert_eq!(srgb_to_linear([0.0, 0.0, 0.0, 1.0]), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(srgb_to_linear([1.0, 1.0, 1.0, 1.0]), [1.0, 1.0, 1.0, 1.0]);
        let mid = srgb_to_linear([0.5, 0.5, 0.5, 1.0]);
        assert!(
            close(mid[0], 0.2140),
            "0.5 sRGB -> 0.2140 linear, got {}",
            mid[0]
        );
        // 0.847 sRGB is the default-ish foreground; linear ~0.687.
        let fg = srgb_to_linear([0.847, 0.847, 0.847, 1.0]);
        assert!(close(fg[0], 0.6866), "got {}", fg[0]);
        assert_eq!(mid[3], 1.0);
    }

    #[test]
    fn top_level_ui_constants_match_their_srgb_design_values() {
        // Each chrome constant must be the *linear* form of the sRGB value the
        // palette was designed against; a bare sRGB literal here washes out.
        let cases: [(&str, [f32; 4], [f32; 4]); 6] = [
            ("PANEL_BG", PANEL_BG, [0.05, 0.05, 0.06, 1.0]),
            ("BAR_BG", BAR_BG, [0.03, 0.03, 0.04, 1.0]),
            ("SIDEBAR_BG", SIDEBAR_BG, [0.04, 0.04, 0.05, 1.0]),
            ("FG_DEFAULT", FG_DEFAULT, [0.847, 0.847, 0.847, 1.0]),
            ("TAB_TEXT", TAB_TEXT, [0.65, 0.65, 0.68, 1.0]),
            ("CONFIRM_BG", CONFIRM_BG, [0.42, 0.30, 0.08, 1.0]),
        ];
        for (name, linear, srgb) in cases {
            let want = srgb_to_linear(srgb);
            for ch in 0..3 {
                assert!(
                    (linear[ch] - want[ch]).abs() < 5e-4,
                    "{name}[{ch}]: constant {} vs linear({}) = {}",
                    linear[ch],
                    srgb[ch],
                    want[ch]
                );
            }
        }
    }

    #[test]
    fn palette_is_linear_not_srgb() {
        // 0.8 sRGB -> 0.604 linear; the pre-fix constants leaked sRGB values
        // straight into the linear pipeline and washed every color out.
        let dark = ColorScheme::dark();
        assert!(
            close(dark.palette[1][0], 0.604),
            "red={}",
            dark.palette[1][0]
        );
        assert!(
            close(dark.palette[7][0], 0.78346),
            "white={}",
            dark.palette[7][0]
        );
        assert_eq!(dark.palette[9], [1.0, 0.0, 0.0, 1.0]);
        // Indexed slots 0-15 must not be gamma-converted twice.
        assert_eq!(dark.indexed(1), dark.palette[1]);
        assert_eq!(dark.indexed(9), dark.palette[9]);
    }

    #[test]
    fn truecolor_spec_is_converted_once() {
        let dark = ColorScheme::dark();
        let white = dark.color_of(Color::Spec(Rgb {
            r: 255,
            g: 255,
            b: 255,
        }));
        assert_eq!(white, [1.0, 1.0, 1.0, 1.0]);
        let grey = dark.color_of(Color::Spec(Rgb {
            r: 128,
            g: 128,
            b: 128,
        }));
        // 128/255 = 0.502 sRGB -> ~0.2158 linear.
        assert!(close(grey[0], 0.2158), "got {}", grey[0]);
    }

    #[test]
    fn theme_overrides_apply_to_palette_and_ui() {
        use berth_core::config::{Palette, Theme, ThemePreset};
        let cfg = Theme {
            preset: ThemePreset::Dark,
            background: Some("#112233".into()),
            foreground: Some("#eeeeee".into()),
            cursor: None,
            accent: None,
            palette: Palette {
                red: Some("#ff0000".into()),
                ..Default::default()
            },
        };
        let scheme = ColorScheme::from_config(&cfg);
        assert_eq!(scheme.bg, hex_to_linear("#112233").unwrap());
        assert_eq!(scheme.fg, hex_to_linear("#eeeeee").unwrap());
        assert_eq!(scheme.palette[1], [1.0, 0.0, 0.0, 1.0]);
        // Non-overridden slots keep the preset.
        assert_eq!(scheme.palette[2], ColorScheme::dark().palette[2]);
    }

    #[test]
    fn light_preset_is_light_and_invalid_hex_is_ignored() {
        use berth_core::config::{Theme, ThemePreset};
        let mut cfg = Theme {
            preset: ThemePreset::Light,
            ..Default::default()
        };
        cfg.background = Some("not-a-color".into());
        let scheme = ColorScheme::from_config(&cfg);
        // Invalid override ignored -> preset background stays light grey.
        assert_eq!(scheme.bg, hex_to_linear("#f7f7f7").unwrap());
        assert!(scheme.fg[0] < 0.1, "light theme needs dark text");
        assert!(
            scheme.chrome.tab_text_active[0] < 0.05,
            "light chrome needs dark active-tab text"
        );
    }

    #[test]
    fn hex_parser_accepts_short_and_long_forms() {
        assert_eq!(hex_to_linear("#fff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(hex_to_linear("abc"), None);
        assert_eq!(hex_to_linear("#12345"), None);
        assert_eq!(hex_to_linear("#xyzxyz"), None);
    }
}
