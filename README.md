# berth

A lightweight, cross-platform (Linux / macOS / Windows) terminal emulator, Ghostty-like in feel, with
a **left-side tab sidebar** and **project workspaces** — the UX that mainstream terminals keep rejecting:
Ghostty closed the side-tabs request and ships only horizontal tabs; cmux has the right shape but is
macOS-first and closed.

## Meaning

- **Workspaces**: named project containers in the sidebar; each holds its own tabs, grouped and
  re-orderable. Switching workspace keeps shell session state.
- **Tabs-to-the-left**: the sidebar is first-class, not a decoration — drag tabs between workspaces.
- **Identical everywhere**: same rendering, keybinds and config on all three platforms.
- **Extensible**: sandboxed WASM plugins (`wasmtime`), no orphaned native-code plugin APIs.

## Build

```sh
cargo build --release
```

## Status

Events and full tracking: **[docs/ROADMAP.md](docs/ROADMAP.md)** (includes the
Nov 2026 external review's findings and their status). Every config key,
default and per-OS path: **[docs/CONFIG.md](docs/CONFIG.md)**.

v0.24.0 — M1–M3 shipped (details below), M4 plugin host in progress.
Post-0.24.0 fixes: correct sRGB→linear color pipeline, mouse wheel scrolls the
viewport instead of walking shell history (trackpad pixel deltas accumulate
instead of rounding away), typing while scrolled back returns to the live tail
with an overlay scrollbar/`↑N` badge, real hover states, one shared chrome
hit-test for every press (close buttons stable across DPI/OS), rounded pill
tab/sidebar chrome with shrink-to-fit tabs, real bold/italic
faces, theme presets, and per-launch GPU diagnostics in `berth.log`.

### Shipped

- **M1** — single pane, PTY on all 3 OSes, wgpu glyph atlas renderer, key input
  (CSI encoded), selection + copy/paste, scrollback, resize/PTY sync
- **M2** — splits (`ctrl+shift+Enter` vertical, `ctrl+shift+\` horizontal),
  draggable splitters, tabs (`ctrl+shift+n`, `ctrl+tab` / `ctrl+shift+tab`
  cycle), visible tab strip with click-to-focus, close pane (`ctrl+shift+w`),
  middle-click closes tabs/workspaces,
  pane focus with `alt+arrows`, OSC window/tab titles
- **M3** — left sidebar with workspaces (`ctrl+shift+o` new,
  `ctrl+shift+q` close, `ctrl+alt+←/→` cycle, click entries), auto
  `ws N` labels, per-workspace tabs, hot-reload of `config.toml`
  (mtime polling, every change incl. font swaps applies live),
  `cmd+u` sidebar toggle, `+` buttons on tab strip and sidebar,
  confirm banner before closing a multi-pane workspace ✕ cascade,
  session persistence: workspaces/tabs/splits are snapshotted every ~4s
  and on exit (`berths.json`) and restored — with fresh shells — on relaunch,
  OSC 7 cwd tracking: new tabs/splits/workspaces start in the focused pane's
  directory (works with shell-integration-aware shells; see below)

### Shell integration (optional)

Emit OSC 7 to give berth the real cwd for new panes and saved sessions — add
to your rc file:

```sh
# zsh
autoload -Uz add-zsh-hook
report_cwd() { printf '\e]7;file://%s%s\a' "$(hostname)" "$PWD" }
add-zsh-hook precmd report_cwd
```

(bash: append the same `printf` to a `PS1` via `PROMPT_COMMAND`.)

### Keybinds

Defaults follow iTerm2 (`cmd`/super chords work on all platforms); legacy
`ctrl` combos ship as aliases. Everything is user-overridable.

Text input goes through the platform IME: CJK/dead-key composition is shown at
the cursor until commit, and DECCKM application-cursor mode (`ESC O A…`) is
honored for arrows/Home/End. Ctrl/alt/super shortcuts still go through the
key-event path, never the IME.

| action | keys |
| --- | --- |
| split vertically / horizontally | `cmd+d` (`ctrl+shift+Enter`) / `cmd+shift+d` (`ctrl+shift+\`) |
| resize split | drag divider (2px, 4px hit slack) |
| new tab | `cmd+t` (`ctrl+shift+n`) |
| new / close workspace | `ctrl+shift+o` / `ctrl+shift+q` |
| cycle workspaces | `ctrl+alt+←` / `ctrl+alt+→` |
| cycle tabs | `cmd+]` / `cmd+[` (`ctrl+tab`, +shift for prev) |
| close focused pane | `cmd+w` (`ctrl+shift+w`) |
| zoom focused pane (tmux-style) | `cmd+shift+Enter` |
| close any tab / workspace | `✕` button in the tab slot / sidebar entry |
| resize sidebar | drag its right edge (hover shows ⟷ cursor) |
| pane focus | `alt+arrows` (`focus-pane-*` action, rebindable) |
| drag tab to another workspace | drag a tab slot onto a sidebar entry |
| select text | drag = freeform · double-click = word · triple-click = line (copy-on-select; dragging past the pane edge auto-scrolls) |
| copy / paste selection | `cmd+c` / `cmd+v` (`ctrl+shift+c` / `ctrl+v`) |
| scroll | mouse wheel, `shift+PgUp` / `shift+PgDn`, `cmd+PgUp` / `cmd+PgDn` |
| search scrollback | `cmd+f` (`ctrl+shift+f`); type to filter, `enter`/`shift+enter` cycles hits, `esc` closes |
| open link | `cmd+click` (`ctrl+click` on Linux/Windows) over OSC 8 or `http(s)://`/`file://`/`mailto:` text |
| fullscreen | `F11` or `alt+Enter` (borderless; same chord toggles back) |
| clear scrollback | `cmd+k` (`ctrl+shift+k`); keeps the visible screen |

Every chord above is reassignable in `config.toml`; a user entry replaces all
defaults for that action:

```toml
[keybinds]
"new-tab"   = ["ctrl+shift+t", "cmd+t"]   # extra chord, removes default aliases as wanted
"copy"      = ["ctrl+shift+c"]            # drop the cmd+c default if it collides
```

Action ids: `split-vertical`, `split-horizontal`, `new-tab`, `close-pane`,
`close-tab`, `next-tab`, `prev-tab`, `new-workspace`, `close-workspace`,
`next-workspace`, `prev-workspace`, `zoom-pane`, `focus-pane-up`, `focus-pane-down`,
`toggle-sidebar`,
`focus-pane-left`, `focus-pane-right`, `copy`, `paste`,
`scroll-page-up`, `scroll-page-down`, `search`, `fullscreen`, `clear-scrollback`. Chord syntax is modifier tokens (`ctrl`/`control`,
`shift`, `alt`/`option`, `super`/`cmd`/`win`) joined with `+` plus the key
(`enter`, `tab`, `backslash`, `[`, `]`, `left/right/up/down`,
`pageup/pagedown`, `home`, `end`, `insert`, `delete`, `escape`, `space`,
`f1..f12`, or any digit/letter). Unknown actions or unparseable chords are
reported on stderr at load. Changes hot-reload like the rest of the config.

### Fonts & themes

Typefaces and colors live in the same `config.toml` and hot-reload on save:

```toml
[font]
family = "JetBrains Mono"   # any installed family; falls back to a per-OS mono
size_pt = 12.5
line_height = 1.15          # multiplier on the face's ascent+descent (1.0 = tight)

[theme]
preset = "dark"             # "dark" (default) or "light"
background = "#121317"      # optional overrides
foreground = "#e6e6e6"
cursor = "#f0e6c8"
accent = "#7aa2ff"           # handed to plugins (host.accent)

[theme.palette]             # ANSI 0-15, any subset
red = "#ff5f5f"
bright_blue = "#7aa2ff"
```

Real bold, italic, and bold-italic faces are loaded from the chosen family;
when a family ships no italic, berth synthesizes a skew so italics still read
differently. Drop extra `.ttf`/`.otf` files into `~/.config/berth/fonts/`
to make them selectable. Unknown `[theme]` keys and malformed colors are
reported in `berth.log` rather than silently ignored. The light preset themes
both the terminal and the window chrome (tab strip, sidebar, dividers).

Cursor shape and blink are separate knobs:

```toml
[terminal]
cursor_style = "block"   # "block" | "beam" | "underline"
cursor_blink = true      # blink only while the window is focused
cursor_blink_ms = 530    # full on/off cycle, clamped to 200..2000
```

Typing resets the blink to solid; when the window loses focus the block cursor
becomes a hollow outline. Multi-pane layouts draw a cursor in the focused pane
only.

### Shell & startup

New panes run your login shell by default. Pin a different program, its
arguments, environment, and starting directory:

```toml
[shell]
program = "/usr/bin/fish"        # omit for the platform login shell
args = ["-l"]                     # passed only when program is set
working_dir = "~/code"            # leading ~/ expands; defaults to the tab's cwd

[shell.env]                       # overrides inherited variables
EDITOR = "hx"
RUST_BACKTRACE = "1"

[terminal]
scrollback = 10000                # lines kept per pane (0 disables, capped at 1M)
```

`working_dir` beats the tab's remembered cwd (OSC 7). Scrollback size applies
to panes created after the config reload — existing panes keep their grid.

A shell that exits non-zero shows a transient `shell exited (code N)` strip at
the bottom of the window (clean exits close the pane silently). A terminal bell
in a background tab lights a dot on that tab until you switch to it.

### Remaining

- sidebar: rename is done (**double-click a workspace entry** to rename it
  inline; Enter commits, Esc cancels); still missing drag tabs between
  workspaces, drag tabs between panes
- polish: window-title-based quit guard, scrolling/selection interplay;
  wide glyphs render through the fallback chain with two-cell decorations
- advanced TUIs: kitty keyboard protocol probe/responses (TERM=xterm-256color
  + COLORTERM=truecolor already cover the common palette cases)
- platform: keep ConPTY path honest on Windows (CI compiles but manual QA pending)
- **M4** — plugin host (`crates/plugins`, slice 1 shipped, growing): fuel +
  memory limits, guest-trap surfacing/auto-disable, per-plugin config flags;
  capability gating, status-bar segments and pane-output taps next; keybind
  presets via plugins

### Plugins (slice 1)

Drop `.wasm` components into `~/.config/berth/plugins/` (Windows/macOS:
the `berth/config dir` per OS). Plugins use the WIT world `berth:plugin`
(`crates/plugins/wit/berth-plugin.wit`): they receive a workspace/tab
snapshot (`get-workspaces`/`get-tabs` via the exported `on-layout-change`
callback), queue workspace renames with `set-workspace-label`, publish a short
tab-strip segment with `set-status`, and read the active theme color with
`accent` (`#rrggbb`). Status segments are cleared before every callback, so
guests republish them each run.
Broken plugins log and are skipped. Guests are sandboxed by fuel metering
(50 M units per callback) and a 64 MiB linear-memory cap: a `loop {}`, O(n²)
bug or memory runaway traps the guest, logs `plugin '<name>' trapped; disabled
for this session`, and the plugin is skipped until the next launch — the UI
never freezes. Demo guest: `crates/plugins/guest`
(auto-namer: `ws 2` → `ws-02`, plus a `N ws` status segment), compiled with
`cargo build -p berth-plugin-guest --release --target wasm32-wasip2`.

### Branding

App icon ships everywhere: embedded in the window (winit),
`berth.icns` in the macOS bundle (`CFBundleIconFile`), `berth.ico` linked
into the Windows exe (`winresource`). Source art in `assets/`.

### Debugging GPU / colors

Every launch appends one diagnostic line (GPU name, backend, device type,
surface format, sRGB flag) to `berth.log` in the config dir — on
Windows/macOS that is the only reliable sink for GUI builds:

```
tail -n 20 ~/.config/berth/berth.log
```

To list every adapter wgpu can see (no window, works in CI), run:

```
berth --gpu-info
```

`BERTH_DEBUG=1` adds per-event lines (input routing, wheel, font raster
probes) to the same file and stderr, plus a bottom-right HUD with the smoothed
CPU draw time and quad count. If the log says `type=Cpu`, wgpu fell
back to software rendering (lavapipe/SwiftShader) — install the GPU driver or
force a backend with `WGPU_BACKEND=vulkan|metal|dx12`. A `srgb=false` line
means the compositor offered no sRGB surface; colors are converted but may
look slightly off.

### Lightweight budget

`berth --version` prints the crate version; `scripts/lightweight.sh` reports
binary size, time to first rendered frame, and idle RSS (Linux, needs Xvfb):

```
cargo build --release -p berth
scripts/lightweight.sh
```

Reference numbers (Linux, Intel ADL-N laptop, debug Vulkan/lavapipe under
Xvfb — real GPU startup is faster): **49 MB binary, 365 ms to first frame,
~178 MiB idle RSS**. The RSS figure includes wgpu+Vulkan and the lazily
loaded plugin host; CI builds the release binary and fails past **60 MiB**
(`.github/workflows/ci.yml`). Startup/RSS stay manual: GitHub runners have no
GPU, so frame-time numbers there are meaningless.

### macOS notes

The `Berth.app` bundle is ad-hoc signed, not notarized: on first launch use
right-click → Open, or `xattr -cr Berth.app` to clear Gatekeeper. Font
selection prefers Menlo / SF Mono / Monaco over the generic `monospace`
(that resolves to Courier New on macOS, whose metrics clip glyphs).

## Crates

| crate | role |
| --- | --- |
| `crates/core` | workspace/tab/pane/split tree, layout+focus math, config |
| `crates/emulation` | alacritty_terminal VT parsing + portable-pty |
| `crates/ui` | winit event loop, wgpu renderer (glyph atlas, tab strip, sidebar) |
| `crates/plugins` | wasmtime host, capability-gated ABI |
| `crates/app` | binary wiring it together |

## Releases

Tags `vX.Y.Z` are cut by release-please from conventional commits; the release
workflow attaches: linux x64/aarch64 binaries (`tar.gz`, musl static, the
aarch64 via cargo-zigbuild) plus `deb`/`rpm` packages for both arches (binary
at `/usr/bin/berth`, desktop entry + hicolor icons, no runtime dependencies),
macOS aarch64/x64 (`tar.gz` + signed `.app` bundles), Windows x64 zip.

## License

Dual-licensed `MIT OR Apache-2.0`.
