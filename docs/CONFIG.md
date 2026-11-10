# Configuration reference

`berth` reads a single TOML file and hot-reloads it while running (font, theme,
keybinds, plugin toggles and shell settings for *new* panes; existing shells
keep their process and scrollback). Everything is optional.

## Where the file lives

| OS | Path |
| --- | --- |
| Linux / BSD | `$XDG_CONFIG_HOME/berth/config.toml` (default `~/.config/berth/config.toml`) |
| macOS | `~/Library/Application Support/com.berth.berth/config.toml` |
| Windows | `%APPDATA%\com\berth\berth\config.toml` |

The exact directory is printed on the first line of the log, which is the
authoritative answer if your platform disagrees:

```sh
head -1 ~/.config/berth/berth.log
# log: config_dir=/home/you/.config/berth
```

Everything else the app owns sits next to it:

| File | Purpose |
| --- | --- |
| `config.toml` | this file |
| `berths.json` | layout/session snapshot, restored on launch |
| `berth.log` | append-only diagnostics (GPU, themes, plugins, errors) |
| `plugins/*.wasm` + `plugins/<stem>.toml` | WASM plugins and their capability manifests |

A missing `config.toml` is fine (defaults are used). A malformed file, an
unknown key, or an unknown enum value is a **hard error** (`parse <path>: …`
in the log) — typos never silently fall back to defaults.

## Minimal file

```toml
[font]
family = "JetBrains Mono"
size_pt = 12.5

[theme]
preset = "dark"
```

## `[font]`

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `family` | string | `"monospace"` | First existing family from the system font chain; `~/.local/share/fonts` and per-OS font dirs are scanned. |
| `size_pt` | float | `12.0` | Points at 96 dpi; scaled by the window DPI factor. |
| `line_height` | float | `1.0` | Multiplier over ascent+descent, clamped `0.8..3.0`. |
| `ligatures` | bool | `true` | Shape ASCII runs through the font's `calt`/`liga` features; multi-cell glyphs are drawn once at their cluster start. No effect on fonts without those features. |

The primary family is used for regular, **real bold/italic/bold-italic** faces
when the family ships them; otherwise a synthetic embolden/12° skew is used and
the log says `face: no real bold …; using synthetic embolden`.

### Fallbacks (icons, emoji, CJK)

Missing glyphs fall through a fallback chain per cell, so the primary family
does not have to be patched:

1. faces in the config dir's `fonts/` folder (drop a Nerd Font there and it is
   consulted first),
2. common Nerd Font / Powerline families (`MesloLGS NF`, `JetBrainsMono Nerd
   Font`, `Hack Nerd Font`, `FiraCode Nerd Font`, `Symbols Nerd Font`, …),
3. symbol/CJK/emoji faces (`Noto Sans Symbols 2`, `Noto Sans CJK SC`, `Apple
   Color Emoji`, `Segoe UI Emoji`, …), then `DejaVu Sans` and `Unifont`,
4. on Linux, `fc-list` resolves any still-missing codepoint to a covering
   system face at draw time (one lookup per codepoint, cached).

Private-use icons (p10k, oh-my-posh, starship) need a patched face somewhere in
that chain; install a Nerd Font or vendor one in `fonts/`. Placeholder fonts
(`*sample*`, `LastResort`, `NotDef`) are skipped — they claim coverage but paint
codepoint boxes.

## `[terminal]`

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `backspace` | `"del"` or `"bs"` | `"del"` | Byte sent for the Backspace key. |
| `copy_on_select` | bool | `true` | Copy the selection as soon as the mouse is released. |
| `cursor_style` | `"block"`, `"beam"`, `"underline"` | `"block"` | Focused-pane cursor shape. |
| `cursor_blink` | bool | `true` | Blink the focused cursor; typing shows it solid. |
| `cursor_blink_ms` | integer | `530` | Half-period, clamped `200..=2000`. |
| `scrollback` | integer | `10000` | History lines per pane, clamped `0..=1000000`; applies to panes created after a reload. |
| `kitty_keyboard` | bool | `false` | Advertise the kitty keyboard protocol (CSI-u encoding). Off by default: PTY layers that don't decode `CSI u` (ConPTY) break input in apps that opt in. Applies to panes created after a reload. |

## `[shell]`

Controls what a new pane runs. Without `program` the platform login shell is
used (e.g. `$SHELL`, or PowerShell on Windows).

```toml
[shell]
program = "/usr/bin/fish"      # empty/absent → default login shell
args = ["-l"]
working_dir = "~/projects"     # leading ~ expands to $HOME

[shell.env]
EDITOR = "hx"
RUST_BACKTRACE = "1"
```

`env` entries override the defaults `berth` always sets (`TERM=xterm-256color`,
`COLORTERM=truecolor`, `TERM_PROGRAM=berth`). `working_dir` is used for new
panes unless the focused pane's OSC 7 report or the tab's recorded cwd wins.

## `[theme]`

```toml
[theme]
preset = "dark"          # "dark" | "light"
background = "#101014"
foreground = "#e6e6e6"
cursor = "#7aa2ff"
accent = "#7aa2ff"       # handed to plugins via host.accent()

[theme.palette]
red = "#d75f5f"
bright_cyan = "#5fd7d7"
# …all 16 ANSI slots: black red green yellow blue magenta cyan white
#   and bright_black bright_red … bright_white
```

Colors accept `#rgb` and `#rrggbb`. The preset also themes the chrome (tab
strip, sidebar, dividers, banner, focus ring); a custom `background` is
propagated to the pane gutters. Invalid colors are logged and ignored
individually rather than failing the whole file; unknown theme keys are
rejected at parse time.

`accent` is what guests read through `host.accent()`; it defaults per preset
(`#4f8cff` dark, `#0969da` light).

## In-app settings (`cmd+,` / `ctrl+,`)

The settings overlay edits the knobs below without leaving the terminal:
theme preset, font size, line height, sidebar width/visibility, cursor
style/blink, copy-on-select, scrollback, kitty keyboard, backspace, and a
shortcut row that opens the plugin manager. Every change is applied
immediately (font/theme swap, re-tile) and written back to this file through
`Config::save()`, which rewrites the whole document — **hand-written comments
are not preserved** by an in-app edit. `scrollback`, `kitty keyboard` and
`backspace` only affect panes spawned afterwards.

## `[sidebar]`

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `show` | bool | `true` | `cmd+u` / `ctrl+shift+u` toggles at runtime. |
| `width_px` | float | `220.0` | Logical pixels; drag the sidebar edge to resize (120–720). |

## `[plugins]`

Keyed by the `.wasm` **file stem**; absent means enabled. The plugin manager
(`cmd+shift+p` / `ctrl+shift+p`) flips these flags and writes the file back,
preserving every other section.

```toml
[plugins]
"auto-namer" = false
```

Per-plugin permissions live in the manifest next to the component, not here:

```toml
# ~/.config/berth/plugins/auto-namer.toml
name = "auto-namer"
capabilities = ["rename", "status"]
```

Without a manifest (or with empty capabilities) a plugin loads read-only:
`set-workspace-label` and `set-status` are refused and the first refusal is
logged as `plugin '<name>' ignored: needs '<cap>' capability`.

## `[keybinds]`

Each action maps to one chord string or a list of them. A chord is
modifier tokens joined with `+` and one key: `ctrl`, `shift`, `alt`/`option`,
`cmd`/`super`/`win`, then `a`…`z`, `0`…`9`, `f1`…`f12`, `enter`, `tab`,
`space`, `escape`, `backspace`, `pageup`, `pagedown`, `home`, `end`, `left`,
`right`, `up`, `down`, or a punctuation name (`backslash`, `slash`, …).
Bare keys are only allowed for function keys (`f11 = ["fullscreen"]`).

```toml
[keybinds]
new-tab = ["cmd+t", "ctrl+shift+n"]
paste = ["cmd+v"]
search = ["cmd+f"]
```

An action bound to an empty list is disabled. Unknown action ids and
unparseable chords are logged as warnings (the rest of the config still
applies). Action ids and their defaults:

| Action | Default chords |
| --- | --- |
| `copy` | `cmd+c`, `ctrl+shift+c` |
| `paste` | `cmd+v`, `ctrl+v` |
| `scroll-page-up` | `shift+pageup`, `cmd+pageup` |
| `scroll-page-down` | `shift+pagedown`, `cmd+pagedown` |
| `split-vertical` | `cmd+d`, `ctrl+shift+enter` |
| `split-horizontal` | `cmd+shift+d`, `ctrl+shift+backslash` |
| `new-tab` | `cmd+t`, `ctrl+shift+n` |
| `close-pane` | `cmd+w`, `ctrl+shift+w` |
| `next-tab` | `cmd+]`, `ctrl+tab` |
| `prev-tab` | `cmd+[`, `ctrl+shift+tab` |
| `new-workspace` | `ctrl+shift+o` |
| `close-workspace` | `ctrl+shift+q` |
| `next-workspace` | `ctrl+alt+right` |
| `prev-workspace` | `ctrl+alt+left` |
| `zoom-pane` | `cmd+shift+enter` |
| `focus-pane-up` / `-down` / `-left` / `-right` | `alt+up` / `alt+down` / `alt+left` / `alt+right` |
| `toggle-sidebar` | `cmd+u`, `ctrl+shift+u` |
| `search` | `cmd+f`, `ctrl+shift+f` |
| `fullscreen` | `f11`, `alt+enter` |
| `clear-scrollback` | `cmd+k`, `ctrl+shift+k` |
| `plugin-manager` | `cmd+shift+p`, `ctrl+shift+p` |
| `settings` | `cmd+,`, `ctrl+,` |

## Runtime diagnostics

| Command / env | Effect |
| --- | --- |
| `berth --version` | Print version and exit. |
| `berth --gpu-info` | Enumerate adapters, label the selected one hardware vs software, exit. |
| `BERTH_DEBUG=1` | Frame-time/quad HUD bottom-right plus verbose logging. |
| `BERTH_DUMP=1` | Dump grid rows into `berth.log` (debugging rendering). |
| `BERTH_FRAME_DUMP=1` | Write `berth-frame-N.ppm` per frame into the system temp dir. |
| `WGPU_BACKEND=vulkan\|metal\|dx12\|gl` | Force a backend when driver selection misbehaves. |

Colors are authored in sRGB and converted to linear before the GPU sees them;
the surface is explicitly requested as an sRGB format. If `berth.log` reports
`srgb=false` or an adapter `type=Cpu`, rendering is software or wrong-space and
the log entry is the first thing to check.
