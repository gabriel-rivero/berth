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

v0.11.0 — M1 done, M2 done except polish, M3 mostly done, M4 not started.

### Shipped

- **M1** — single pane, PTY on all 3 OSes, wgpu glyph atlas renderer, key input
  (CSI encoded), selection + copy/paste, scrollback, resize/PTY sync
- **M2** — splits (`ctrl+shift+Enter` vertical, `ctrl+shift+\` horizontal),
  draggable splitters, tabs (`ctrl+shift+n`, `ctrl+tab` / `ctrl+shift+tab`
  cycle), visible tab strip with click-to-focus, close pane (`ctrl+shift+w`),
  pane focus with `alt+arrows`, OSC window/tab titles
- **M3** — left sidebar with workspaces (`ctrl+shift+o` new,
  `ctrl+shift+q` close, `ctrl+alt+←/→` cycle, click entries), auto
  `ws N` labels, per-workspace tabs, hot-reload of `config.toml`
  (mtime polling, every change incl. font swaps applies live)

### Keybinds

Defaults follow iTerm2 (`cmd`/super chords work on all platforms); legacy
`ctrl` combos ship as aliases. Everything is user-overridable.

| action | keys |
| --- | --- |
| split vertically / horizontally | `cmd+d` (`ctrl+shift+Enter`) / `cmd+shift+d` (`ctrl+shift+\`) |
| resize split | drag divider (2px, 4px hit slack) |
| new tab | `cmd+t` (`ctrl+shift+n`) |
| new / close workspace | `ctrl+shift+o` / `ctrl+shift+q` |
| cycle workspaces | `ctrl+alt+←` / `ctrl+alt+→` |
| cycle tabs | `cmd+]` / `cmd+[` (`ctrl+tab`, +shift for prev) |
| close focused pane | `cmd+w` (`ctrl+shift+w`) |
| close any tab / workspace | `✕` button in the tab slot / sidebar entry |
| resize sidebar | drag its right edge (hover shows ⟷ cursor) |
| pane focus | `alt+arrows` |
| copy / paste selection | `cmd+c` / `cmd+v` (`ctrl+shift+c` / `ctrl+v`) |
| scroll | mouse wheel, `shift+PgUp` / `shift+PgDn`, `cmd+PgUp` / `cmd+PgDn` |

Every chord above is reassignable in `config.toml`; a user entry replaces all
defaults for that action:

```toml
[keybinds]
"new-tab"   = ["ctrl+shift+t", "cmd+t"]   # extra chord, removes default aliases as wanted
"copy"      = ["ctrl+shift+c"]            # drop the cmd+c default if it collides
```

Action ids: `split-vertical`, `split-horizontal`, `new-tab`, `close-pane`,
`close-tab`, `next-tab`, `prev-tab`, `new-workspace`, `close-workspace`,
`next-workspace`, `prev-workspace`, `copy`, `paste`, `scroll-page-up`,
`scroll-page-down`. Chord syntax is modifier tokens (`ctrl`/`control`,
`shift`, `alt`/`option`, `super`/`cmd`/`win`) joined with `+` plus the key
(`enter`, `tab`, `backslash`, `[`, `]`, `left/right/up/down`,
`pageup/pagedown`, `home`, `end`, `insert`, `delete`, `escape`, `space`,
`f1..f12`, or any digit/letter). Unknown actions or unparseable chords are
reported on stderr at load. Changes hot-reload like the rest of the config.

### Remaining

- sidebar: workspace/tab rename, drag tabs between workspaces, drag tabs between panes
- polish: no window-title-based quit guard, no close-confirmation, scrolling
  selection interplay; wide-glyph spacing beyond the fallback chain
- advanced TUIs: kitty keyboard protocol probe/responses (TERM=xterm-256color
  + COLORTERM=truecolor already cover the common palette cases)
- platform: keep ConPTY path honest on Windows (CI compiles but manual QA pending)
- **M4** — plugin host (`crates/plugins`), capability-gated WASM ABI,
  example plugins

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
workflow attaches 5 archives: linux x64/aarch64 (musl static, the aarch64 via
cargo-zigbuild), macOS aarch64/x64, Windows x64 zip.

## License

Dual-licensed `MIT OR Apache-2.0`.
