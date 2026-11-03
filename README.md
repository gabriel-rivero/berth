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

v0.8.0 — M1 done, M2 done except polish, M3 mostly done, M4 not started.

### Shipped

- **M1** — single pane, PTY on all 3 OSes, wgpu glyph atlas renderer, key input
  (CSI encoded), selection + copy/paste, scrollback, resize/PTY sync
- **M2** — splits (`ctrl+shift+Enter` vertical, `ctrl+shift+\` horizontal),
  draggable splitters, tabs (`ctrl+shift+n`, `ctrl+tab` / `ctrl+shift+tab`
  cycle), visible tab strip with click-to-focus, close pane (`ctrl+shift+w`),
  pane focus with `alt+arrows`, OSC window/tab titles
- **M3** — left sidebar with workspaces (`ctrl+shift+o` new,
  `ctrl+shift+q` close, `ctrl+alt+←/→` cycle, click entries), auto
  `ws N` labels, per-workspace tabs

### Keybinds

| action | keys |
| --- | --- |
| split vertically / horizontally | `ctrl+shift+Enter` / `ctrl+shift+\` |
| resize split | drag divider (2px, 4px hit slack) |
| new / close workspace | `ctrl+shift+o` / `ctrl+shift+q` |
| cycle workspaces | `ctrl+alt+←` / `ctrl+alt+→` |
| new tab / cycle tabs | `ctrl+shift+n` / `ctrl+tab` (+shift for prev) |
| close focused pane | `ctrl+shift+w` |
| pane focus | `alt+arrows` |
| copy / paste selection | `ctrl+shift+c` / `ctrl+v` |
| scroll | mouse wheel, `shift+PgUp` / `shift+PgDn` |

### Remaining

- config: hot-reload of `config.toml`; user keybind config (currently compiled in)
- sidebar: workspace/tab rename, drag tabs between workspaces, drag tabs between panes
- polish: no window-title-based quit guard, no close-confirmation, scrolling
  selection interplay; emoji/wide-glyph fallback fonts
- platform: keep ConPTY path honest on Windows (CI compiles but manual QA pending)
- **M4** — plugin host (`crates/plugins`), capability-gated WASM ABI,
  example plugins

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
