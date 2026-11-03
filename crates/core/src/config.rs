//! User config: single TOML file, hot-reloadable. Loaded from `$XDG_CONFIG_HOME/berth/config.toml`
//! (and equivalents per OS via `directories`). Defaults applied via `#[serde(default)]`.
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Font family/size for the grid renderer.
    pub font: Font,
    /// Sidebar options (width, position is left-only for now by design).
    pub sidebar: Sidebar,
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
