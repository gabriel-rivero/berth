//! User-configurable keybinds: `action = ["chord", ...]` TOML table.
//!
//! Chord syntax: modifier tokens joined with `+`, key last, e.g.
//! "ctrl+shift+enter", "alt+left", "cmd+t". Modifiers: ctrl, shift, alt,
//! super/cmd/win. Keys are matched by physical key name; letters/digits use
//! their lowercase character.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Action an application handles (mouse/keyboard routing targets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    SplitVertical,
    SplitHorizontal,
    NewTab,
    ClosePane,
    CloseTab,
    NextTab,
    PrevTab,
    NewWorkspace,
    CloseWorkspace,
    NextWorkspace,
    PrevWorkspace,
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    ZoomPane,
    FocusPaneUp,
    FocusPaneDown,
    FocusPaneLeft,
    FocusPaneRight,
    ToggleSidebar,
    Search,
    Fullscreen,
    ClearScrollback,
    PluginManager,
}

impl Action {
    /// Stable TOML identifier ("split-vertical" etc.).
    pub fn as_str(self) -> &'static str {
        match self {
            Action::SplitVertical => "split-vertical",
            Action::SplitHorizontal => "split-horizontal",
            Action::NewTab => "new-tab",
            Action::ClosePane => "close-pane",
            Action::CloseTab => "close-tab",
            Action::NextTab => "next-tab",
            Action::PrevTab => "prev-tab",
            Action::NewWorkspace => "new-workspace",
            Action::CloseWorkspace => "close-workspace",
            Action::NextWorkspace => "next-workspace",
            Action::PrevWorkspace => "prev-workspace",
            Action::Copy => "copy",
            Action::Paste => "paste",
            Action::ScrollPageUp => "scroll-page-up",
            Action::ScrollPageDown => "scroll-page-down",
            Action::ZoomPane => "zoom-pane",
            Action::FocusPaneUp => "focus-pane-up",
            Action::FocusPaneDown => "focus-pane-down",
            Action::FocusPaneLeft => "focus-pane-left",
            Action::FocusPaneRight => "focus-pane-right",
            Action::ToggleSidebar => "toggle-sidebar",
            Action::Search => "search",
            Action::Fullscreen => "fullscreen",
            Action::ClearScrollback => "clear-scrollback",
            Action::PluginManager => "plugin-manager",
        }
    }

    pub fn from_action_str(s: &str) -> Option<Action> {
        Some(match s {
            "split-vertical" => Action::SplitVertical,
            "split-horizontal" => Action::SplitHorizontal,
            "new-tab" => Action::NewTab,
            "close-pane" => Action::ClosePane,
            "close-tab" => Action::CloseTab,
            "next-tab" => Action::NextTab,
            "prev-tab" => Action::PrevTab,
            "new-workspace" => Action::NewWorkspace,
            "close-workspace" => Action::CloseWorkspace,
            "next-workspace" => Action::NextWorkspace,
            "prev-workspace" => Action::PrevWorkspace,
            "copy" => Action::Copy,
            "paste" => Action::Paste,
            "scroll-page-up" => Action::ScrollPageUp,
            "scroll-page-down" => Action::ScrollPageDown,
            "zoom-pane" => Action::ZoomPane,
            "focus-pane-up" => Action::FocusPaneUp,
            "focus-pane-down" => Action::FocusPaneDown,
            "focus-pane-left" => Action::FocusPaneLeft,
            "focus-pane-right" => Action::FocusPaneRight,
            "toggle-sidebar" => Action::ToggleSidebar,
            "search" => Action::Search,
            "fullscreen" => Action::Fullscreen,
            "clear-scrollback" => Action::ClearScrollback,
            "plugin-manager" => Action::PluginManager,
            _ => return None,
        })
    }

    /// Defaults follow iTerm2's chord style (cmd on all platforms; ctrl
    /// variants kept as aliases so Linux/Windows muscle memory still works).
    pub fn defaults() -> &'static [(Action, &'static [&'static str])] {
        &[
            (Action::Copy, &["cmd+c", "ctrl+shift+c"]),
            (Action::Paste, &["cmd+v", "ctrl+v"]),
            (Action::ScrollPageUp, &["shift+pageup", "cmd+pageup"]),
            (Action::ScrollPageDown, &["shift+pagedown", "cmd+pagedown"]),
            (Action::SplitVertical, &["cmd+d", "ctrl+shift+enter"]),
            (
                Action::SplitHorizontal,
                &["cmd+shift+d", "ctrl+shift+backslash"],
            ),
            (Action::NewTab, &["cmd+t", "ctrl+shift+n"]),
            (Action::ClosePane, &["cmd+w", "ctrl+shift+w"]),
            (Action::NextTab, &["cmd+]", "ctrl+tab"]),
            (Action::PrevTab, &["cmd+[", "ctrl+shift+tab"]),
            (Action::NewWorkspace, &["ctrl+shift+o"]),
            (Action::CloseWorkspace, &["ctrl+shift+q"]),
            (Action::NextWorkspace, &["ctrl+alt+right"]),
            (Action::PrevWorkspace, &["ctrl+alt+left"]),
            // iTerm2's zoom-pane chord; Linux/Windows ctrl alias stays free for users.
            (Action::ZoomPane, &["cmd+shift+enter"]),
            // Pane focus (alt+arrows, was hardcoded before the bindings table
            // existed and made ctrl+alt+arrows unreachable); rebindable now.
            (Action::FocusPaneUp, &["alt+up"]),
            (Action::FocusPaneDown, &["alt+down"]),
            (Action::FocusPaneLeft, &["alt+left"]),
            (Action::FocusPaneRight, &["alt+right"]),
            // iTerm2's "show/hide sidebar" chord.
            (Action::ToggleSidebar, &["cmd+u", "ctrl+shift+u"]),
            // Scrollback search (incremental; Enter/Shift+Enter cycle hits).
            (Action::Search, &["cmd+f", "ctrl+shift+f"]),
            // Fullscreen toggle (F11 is the cross-platform convention; alt+enter
            // matches xterm/kitty on Linux and Windows).
            (Action::Fullscreen, &["f11", "alt+enter"]),
            // ED 3: drop saved lines, keep the visible screen (xterm/iTerm).
            (Action::ClearScrollback, &["cmd+k", "ctrl+shift+k"]),
            // Plugin manager overlay (list, enable/disable, reload).
            (Action::PluginManager, &["cmd+shift+p", "ctrl+shift+p"]),
        ]
    }
}

/// One parsed chord: modifier set + normalized key name (lowercase).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
    pub key: String,
}

/// Parse a chord string like "ctrl+shift+enter" into a Chord.
/// Unknown modifiers/keys, or bare keys with no modifiers, return None
/// (bare chords would shadow typing) — except function keys, which are safe
/// to bind bare (F11 fullscreen, F1 help, ...).
pub fn parse_chord(s: &str) -> Option<Chord> {
    let mut chord = Chord {
        ctrl: false,
        shift: false,
        alt: false,
        super_key: false,
        key: String::new(),
    };
    let mut parts = s.split('+');
    // Last part must be the key; everything before must be modifiers.
    let key = parts.next_back()?.trim();
    chord.key = key.to_ascii_lowercase();
    if !is_bindable_key(&chord.key) {
        return None;
    }
    for m in parts {
        match m.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => chord.ctrl = true,
            "shift" => chord.shift = true,
            "alt" | "option" => chord.alt = true,
            "super" | "cmd" | "win" => chord.super_key = true,
            _ => return None,
        }
    }
    if !chord.ctrl && !chord.shift && !chord.alt && !chord.super_key && !is_function_key(&chord.key)
    {
        return None;
    }
    Some(chord)
}

/// `f1`..`f12` (case-insensitive input is lowercased before this check).
fn is_function_key(k: &str) -> bool {
    k.len() >= 2 && k.len() <= 3 && k.starts_with('f') && k[1..].chars().all(|c| c.is_ascii_digit())
}

/// Keys we allow in chords. Anything else (weird Dead keys etc.) is rejected
/// at parse time so typos fail loudly on config load.
fn is_bindable_key(k: &str) -> bool {
    matches!(
        k,
        "enter"
            | "tab"
            | "escape"
            | "backspace"
            | "space"
            | "backslash"
            | "bracketleft"
            | "bracketright"
            | "pageup"
            | "pagedown"
            | "home"
            | "end"
            | "insert"
            | "delete"
            | "left"
            | "right"
            | "up"
            | "down"
    ) || k.len() == 1
        && k.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '[' || c == ']')
        || is_function_key(k)
}

/// Compile a config table (action id -> chords) into bindings, falling back to
/// defaults for actions the file doesn't mention. Unknown action ids are kept
/// out silently but reported via the returned list of (id, reason) pairs.
pub fn build_bindings(
    table: &HashMap<String, Vec<String>>,
) -> (HashMap<Chord, Action>, Vec<(String, String)>) {
    let mut bindings = HashMap::new();
    let mut warnings = Vec::new();

    // Seed defaults first so user entries override them.
    for (action, chords) in Action::defaults() {
        for cs in *chords {
            if let Some(chord) = parse_chord(cs) {
                bindings.insert(chord, *action);
            }
        }
    }

    for (id, chords) in table {
        match Action::from_action_str(id) {
            Some(action) => {
                // A user entry replaces all defaults for this action.
                let stale: Vec<Chord> = bindings
                    .iter()
                    .filter(|(_, a)| **a == action)
                    .map(|(c, _)| c.clone())
                    .collect();
                for c in stale {
                    bindings.remove(&c);
                }
                for cs in chords {
                    match parse_chord(cs) {
                        Some(chord) => {
                            bindings.insert(chord, action);
                        }
                        None => warnings.push((id.clone(), format!("unparseable chord '{cs}'"))),
                    }
                }
            }
            None => warnings.push((id.clone(), "unknown action".to_string())),
        }
    }
    (bindings, warnings)
}

/// Raw `[keybinds]` table as it appears in config.toml.
pub type KeybindTable = HashMap<String, Vec<String>>;

/// Deserialize helper for `#[serde(with)]`-free TOML: plain map of lists.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Keybinds(pub KeybindTable);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chords() {
        let c = parse_chord("ctrl+shift+enter").unwrap();
        assert!(c.ctrl && c.shift && !c.alt);
        assert_eq!(c.key, "enter");
        let c = parse_chord("alt+left").unwrap();
        assert_eq!(c.key, "left");
        assert!(c.alt && !c.ctrl);
        let cmd_t = parse_chord("cmd+t").unwrap();
        assert!(cmd_t.super_key && !cmd_t.ctrl && !cmd_t.shift);
        let bracket = parse_chord("cmd+]").unwrap();
        assert_eq!(bracket.key, "]");
        assert_eq!(parse_chord("n"), None); // bare key never binds
        assert_eq!(parse_chord("ctrl+bogus+enter"), None);
        assert_eq!(parse_chord("ctrl+enter").unwrap().key, "enter");
        assert_eq!(parse_chord("ctrl+5").unwrap().key, "5");
    }

    #[test]
    fn defaults_build() {
        let (bindings, warn) = build_bindings(&HashMap::new());
        assert!(warn.is_empty());
        let chord = parse_chord("ctrl+shift+Enter").unwrap();
        assert_eq!(bindings.get(&chord), Some(&Action::SplitVertical));
        // iTerm2-style primary chords are seeded.
        let chord = parse_chord("cmd+t").unwrap();
        assert_eq!(bindings.get(&chord), Some(&Action::NewTab));
        let chord = parse_chord("cmd+w").unwrap();
        assert_eq!(bindings.get(&chord), Some(&Action::ClosePane));
        // case-insensitive key
        let chord = parse_chord("CTRL+SHIFT+N").unwrap();
        assert_eq!(bindings.get(&chord), Some(&Action::NewTab));
        let chord = parse_chord("f11").unwrap();
        assert_eq!(bindings.get(&chord), Some(&Action::Fullscreen));
        let chord = parse_chord("cmd+k").unwrap();
        assert_eq!(bindings.get(&chord), Some(&Action::ClearScrollback));
    }

    #[test]
    fn action_ids_roundtrip() {
        for (action, _) in Action::defaults() {
            assert_eq!(
                Action::from_action_str(action.as_str()),
                Some(*action),
                "{} must parse back to itself",
                action.as_str()
            );
        }
    }

    #[test]
    fn user_overrides_default() {
        let mut table = HashMap::new();
        table.insert("new-tab".to_string(), vec!["ctrl+shift+t".to_string()]);
        let (bindings, warn) = build_bindings(&table);
        assert!(warn.is_empty());
        let chord = parse_chord("ctrl+shift+t").unwrap();
        assert_eq!(bindings.get(&chord), Some(&Action::NewTab));
        // old default for that key: nothing bound new-tab on ctrl+shift+n anymore
        let chord = parse_chord("ctrl+shift+n").unwrap();
        assert_ne!(bindings.get(&chord), Some(&Action::NewTab));
    }

    #[test]
    fn unknown_action_warns() {
        let mut table = HashMap::new();
        table.insert("frobnicate".to_string(), vec!["ctrl+shift+x".to_string()]);
        let (_, warn) = build_bindings(&table);
        assert_eq!(warn.len(), 1);
        assert!(warn[0].1.contains("unknown action"));
    }
}
