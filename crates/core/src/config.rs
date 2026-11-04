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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// fontconfig pattern (Linux), system font name (macOS/Windows)
    pub family: String,
    pub size_pt: f32,
}

impl Default for Font {
    fn default() -> Self {
        // NOTE: pick per-OS default at render time if this is empty.
        Self {
            family: "monospace".to_string(),
            size_pt: 12.0,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Terminal {
    pub backspace: Backspace,
    /// Selection goes to the clipboard when the drag ends (iTerm2/Alacritty parity).
    pub copy_on_select: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            backspace: Backspace::Del,
            copy_on_select: true,
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
