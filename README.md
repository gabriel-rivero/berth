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

Pre-MVP. Roadmap:

1. **M1** — one window/pane, PTY on all 3 OSes, glyph rendering, input, scrollback
2. **M2** — tabs + splits + hot-reloadable TOML config
3. **M3** — sidebar: workspaces, tab groups, drag tabs to workspaces
4. **M4** — plugin host + minimal API + example plugins

## Crates

| crate | role |
| --- | --- |
| `crates/core` | workspace/tab/pane tree, session model, config |
| `crates/emulation` | alacritty_terminal VT parsing + portable-pty |
| `crates/ui` | winit event loop, wgpu renderer |
| `crates/plugins` | wasmtime host, capability-gated ABI |
| `crates/app` | binary wiring it together |

## License

Dual-licensed `MIT OR Apache-2.0`.
