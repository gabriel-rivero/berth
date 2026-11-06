//! User config: single TOML file, hot-reloadable. Loaded from `$XDG_CONFIG_HOME/berth/config.toml`
//! (and equivalents per OS via `directories`). Defaults applied via `#[serde(default)]`.
use crate::keybinds::Keybinds;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Font family/size for the grid renderer.
    pub font: Font,
    /// Sidebar options (width, position is left-only for now by design).
    pub sidebar: Sidebar,
    /// Raw `action -> chords` table (compiled into bindings by the ui layer).
    pub keybinds: Keybinds,
    /// Per-plugin enabled flags keyed by plugin file stem ("auto-namer" = false).
    pub plugins: Plugins,
    /// PTY/keyboard compatibility knobs.
    pub terminal: Terminal,
    /// Terminal color preset + overrides.
    pub theme: Theme,
}

/// Built-in color presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreset {
    #[default]
    Dark,
    Light,
}

/// `[theme]` table: preset plus optional `#rrggbb` overrides. Unknown keys are
/// rejected so a typo (`backgroud`) fails loudly instead of silently ignoring.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub preset: ThemePreset,
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub cursor: Option<String>,
    /// ANSI 0-15 overrides, e.g. `[theme.palette] red = "#ff5555"`.
    pub palette: Palette,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Palette {
    pub black: Option<String>,
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub blue: Option<String>,
    pub magenta: Option<String>,
    pub cyan: Option<String>,
    pub white: Option<String>,
    pub bright_black: Option<String>,
    pub bright_red: Option<String>,
    pub bright_green: Option<String>,
    pub bright_yellow: Option<String>,
    pub bright_blue: Option<String>,
    pub bright_magenta: Option<String>,
    pub bright_cyan: Option<String>,
    pub bright_white: Option<String>,
}

impl Palette {
    /// ANSI slot order (0-15) for applying overrides.
    pub fn as_slots(&self) -> [&Option<String>; 16] {
        [
            &self.black,
            &self.red,
            &self.green,
            &self.yellow,
            &self.blue,
            &self.magenta,
            &self.cyan,
            &self.white,
            &self.bright_black,
            &self.bright_red,
            &self.bright_green,
            &self.bright_yellow,
            &self.bright_blue,
            &self.bright_magenta,
            &self.bright_cyan,
            &self.bright_white,
        ]
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// fontconfig pattern (Linux), system font name (macOS/Windows)
    pub family: String,
    pub size_pt: f32,
    /// Line-height multiplier applied to the face's ascent+descent (1.0 = tight).
    pub line_height: f32,
}

impl Default for Font {
    fn default() -> Self {
        // NOTE: pick per-OS default at render time if this is empty.
        Self {
            family: "monospace".to_string(),
            size_pt: 12.0,
            line_height: 1.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sidebar {
    /// sidebar width in logical pixels
    pub width_px: f32,
    pub show: bool,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            width_px: 220.0,
            show: true,
        }
    }
}

/// What the Backspace key sends to the PTY.
///
/// `del` (0x7f) is the modern default — xterm-256color's `kbs` and every
/// current shell/readline expect it. Set `bs` (0x08) for remote hosts whose
/// `stty erase` is `^H` (older/quirky terminfo over ssh), where DEL otherwise
/// does nothing and the caret appears to move forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backspace {
    /// 0x7f (DEL).
    #[default]
    Del,
    /// 0x08 (^H).
    Bs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Terminal {
    pub backspace: Backspace,
    /// Selection goes to the clipboard when the drag ends (iTerm2/Alacritty parity).
    pub copy_on_select: bool,
    /// Cursor shape: `block`, `beam`, or `underline`.
    pub cursor_style: CursorStyle,
    /// Blink the cursor while the window is focused.
    pub cursor_blink: bool,
    /// Full blink cycle length in ms (clamped to 200..=2000 on use).
    pub cursor_blink_ms: u64,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            backspace: Backspace::Del,
            copy_on_select: true,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            cursor_blink_ms: 530,
        }
    }
}

/// Per-plugin enable flags, keyed by plugin file stem:
///
/// ```toml
/// [plugins]
/// "auto-namer" = false
/// ```
///
/// Absent entries count as enabled.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct Plugins(pub HashMap<String, bool>);

impl Plugins {
    /// Stems marked disabled.
    pub fn disabled(&self) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(_, on)| !**on)
            .map(|(k, _)| k.as_str())
            .collect()
    }
}

impl Config {
    /// Resolve the config file path for the current platform.
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "berth", "berth")
            .map(|d| d.config_dir().join("config.toml"))
    }

    /// Load config from disk; missing file -> defaults (not an error, file is created on first save).
    pub fn load() -> Result<Self, anyhow::Error> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => anyhow::bail!("read {}: {e}", path.display()),
        };
        // TODO(core): watch file for hot reload (ui layer re-emits Layout snapshots on change).
        toml::from_str(&raw).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_theme_preset_overrides_and_line_height() {
        let cfg: Config = toml::from_str(
            r##"
            [font]
            family = "JetBrains Mono"
            size_pt = 13.5
            line_height = 1.2

            [theme]
            preset = "light"
            background = "#101010"
            cursor = "#ff0000"

            [theme.palette]
            red = "#aa0000"
            bright_cyan = "#00ffff"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.font.family, "JetBrains Mono");
        assert_eq!(cfg.font.line_height, 1.2);
        assert_eq!(cfg.theme.preset, ThemePreset::Light);
        assert_eq!(cfg.theme.background.as_deref(), Some("#101010"));
        assert_eq!(cfg.theme.palette.red.as_deref(), Some("#aa0000"));
        assert_eq!(cfg.theme.palette.bright_cyan.as_deref(), Some("#00ffff"));
        assert!(cfg.theme.palette.green.is_none());
    }

    #[test]
    fn theme_defaults_are_dark_and_unset() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.theme.preset, ThemePreset::Dark);
        assert!(cfg.theme.background.is_none());
        assert_eq!(cfg.font.line_height, 1.0);
    }

    #[test]
    fn unknown_theme_key_is_rejected() {
        let err = toml::from_str::<Config>("[theme]\nbackgroud = \"#fff\"\n");
        assert!(err.is_err(), "typo should fail loudly: {err:?}");
    }

    #[test]
    fn parses_cursor_style_and_blink() {
        let cfg: Config = toml::from_str(
            "[terminal]\ncursor_style = \"beam\"\ncursor_blink = false\ncursor_blink_ms = 900\n",
        )
        .unwrap();
        assert_eq!(cfg.terminal.cursor_style, CursorStyle::Beam);
        assert!(!cfg.terminal.cursor_blink);
        assert_eq!(cfg.terminal.cursor_blink_ms, 900);

        let default: Config = toml::from_str("").unwrap();
        assert_eq!(default.terminal.cursor_style, CursorStyle::Block);
        assert!(default.terminal.cursor_blink);
    }

    #[test]
    fn unknown_cursor_style_is_rejected() {
        let err = toml::from_str::<Config>("[terminal]\ncursor_style = \"box\"\n");
        assert!(err.is_err(), "unknown cursor shape should fail loudly");
    }
}
