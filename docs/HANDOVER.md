# Session handover — 2026-11-10

State for the next session. History lives in `docs/ROADMAP.md`; this file is
the short version: what landed, what to run, what is next.

## Branch state

`main` at the chrome-input commit (v0.27.0 tag cut, 0.28.0 release PR open),
pushed, all tests/clippy/fmt green. Since v0.26.0:

```
7a7ec0c feat(chrome): move-tab-to-workspace rows in the tab context menu
cb5e03d feat(chrome): right-click context menus for tabs, workspaces and panes
f316dd9 fix(sidebar): real single-line editor for workspace rename
8ba2296 feat(chrome): tab reordering, wheel-over-chrome, ctrl+digit tab slots
7dd0ebb feat(plugins): in-app plugin manager with enable/disable, error list and hot reload
bdfb0b3 chore(main): release 0.27.0 (#49)
2c64c3e docs: record touch support and roadmap focus
8878a1f feat(input): map touchscreen taps to left clicks
fc078b4 chore(main): release 0.26.2 (#48)
4c7995a fix(config): boot with defaults on parse errors; fix Windows IME test
2ee0cf9 docs: reconcile roadmap and qwen review with v0.26.1 status
735512a chore(main): release 0.26.1 (#47)
ac0b1a4 docs: add vision north star, refresh handover and README status
8c210b3 fix(input): make kitty keyboard protocol opt-in, Windows-safe IME gating
```

Touch taps are mapped to left clicks in `app.rs` (`WindowEvent::Touch` →
synthetic `CursorMoved`/`MouseInput` through `window_event`, first-contact id
tracking, 600 ms `TOUCH_MOUSE_GRACE` to drop Windows' promoted mouse events).
Windows/macOS remain unverified by hand — keep them compiling and CI-green.

The kitty commit is a **functional revert**: the protocol code and tests remain, but
`[terminal] kitty_keyboard` (default `false`) now gates `TermConfig`, so panes never
advertise CSI-u support unless the user asks. Same commit makes `ime_owns_text()`
never suppress `KeyboardInput` text on Windows (WM_CHAR delivers text even with an
IME associated; a stuck composition could otherwise swallow every keystroke).

## User-reported issues from the original brief

1. Close buttons on tabs/workspaces — fixed by one shared hit-test; press
   dispatch and hover now use the same `ChromeZone` (`tests/chrome_hit.rs`, DPI
   coverage). Not manually retested on macOS/Windows (no hardware here).
2. Wheel cycling history — fixed: `TermMode::default()` includes
   `ALTERNATE_SCROLL`; routing now requires `ALT_SCREEN` too.
3. Colors wrong — fixed: UI colors converted exactly to linear, surface format
   forced sRGB, verified end-to-end via PPM frame dumps.
4. Hardware acceleration — `berth --gpu-info` enumerates adapters; every run
   logs GPU/backend/type and warns on software adapters/non-sRGB surfaces.
5. alacritty_terminal base — confirmed and now used for grid, selection
   (including semantic/lines), scrollback, links, mode bits; no reimplementation.

## Where things live

| Area | File |
| --- | --- |
| config schema (`[font]`, `[theme]`, `[terminal]`, `[shell]`, `[keybinds]`) | `crates/core/src/config.rs` |
| actions + default chords | `crates/core/src/keybinds.rs` |
| pane (PTY, find, links, selection, clear, scroll) | `crates/emulation/src/pane.rs` |
| renderer, chrome geometry, colors, overlays | `crates/ui/src/render.rs` |
| event loop, input routing, state | `crates/ui/src/app.rs` |
| plugin host (wasmtime, fuel/memory caps) | `crates/plugins/src/lib.rs` |
| roadmap / status | `docs/ROADMAP.md`, `README.md` |

Chrome input is now mouse-complete: `Workspace::reorder_tab` (drag within the
strip), wheel-over-sidebar/strip cycling (`chrome_zone_at` routing), `cmd/ctrl+1…9`
tab slots (9 = last), right-click context menus for tab/workspace/pane built from
`MenuItem`/`MenuAction` and drawn by `render::draw_context_menu`, plus a real
single-line rename editor (`RenameEdit` with caret/anchor, word motion, paste,
IME). Tab menus also list flat `move to <workspace>` rows.

Settings live in `SettingsPanel`/`SettingId` (app.rs) drawn by
`render::draw_settings_panel`; `apply_live_config(font, layout, plugins)` is the
single "push in-memory config into renderer/bindings/panes" path, mirrored by
`poll_config_reload` for hand edits.

Useful new APIs: `chrome_zone_at`/`Chrome` (shared hit-test), `wheel_target`,
`ColorScheme::from_config`, `CursorPaint`/`cursor_rect`, `ScrollInfo`,
`SearchView`, `LinkSpan`/`url_at`/`is_safe_uri`, `GridMatch`/`find_matches`/
`reveal_line`, `SelectionKind`, `Pane::clear_scrollback`.

## Verification

```sh
cargo test --workspace
cargo clippy --workspace --all-targets   # must be warning-free
cargo fmt --all

# headless visual check
Xvfb :96 -screen 0 900x600x24 &
DISPLAY=:96 XDG_CONFIG_HOME=/tmp/opencode/berth-x \
  BERTH_DEBUG=1 BERTH_FRAME_DUMP=1 ./target/debug/berth
# frames: /tmp/opencode/berth-frame-N.ppm (P6; parse with data.split(b'\n',3))
# logs:   <XDG_CONFIG_HOME>/berth/berth.log
# typed input: xdotool windowfocus <id>; xdotool key ...  (absolute, not --window)
pkill -x berth; pkill -x Xvfb
```

Guest plugin artifact (needed by the roundtrip test):

```sh
cd crates/plugins/guest && cargo build --target wasm32-wasip2 --release
```

Without it the roundtrip test skips silently. `target/release/berth` build
takes ~9 min cold.

## Next up (roadmap order)

1. **Windows/macOS verification pass** — the single biggest risk to "near-equal on all
   platforms". On a ROG Ally (Windows 11 handheld, 0.26.2) the build launches and
   `dir` works, so ConPTY typing is confirmed; touchscreen taps did nothing, which
   `8878a1f` fixes by mapping them to left clicks (not yet retested on device).
   Still to check there: whether taps now hit the chrome, ConPTY resize, IME
   composition end, modifier chords. Same pass for macOS.
2. **Plugin-facing polish** — the manager panel shipped (`cmd/ctrl+shift+p`:
   list, enable/disable, one-line load status, `r` rescan, hot reload; toggles
   persist through `Config::save()`). Still open: richer segment metadata
   (badges/icons/click actions), change-event diffs, tier 2–4 APIs.
3. **Settings depth** — the `cmd+,` overlay (theme, font size/line height,
   sidebar, cursor, copy-on-select, scrollback, kitty, backspace, plugin-manager
   shortcut) applies live and saves through `Config::save()`. Still open:
   keybind editing, per-workspace settings (cwd/env/command/color/icon),
   theme/palette color pickers. Note `Config::save()` rewrites the file, so
   hand-written comments are lost on an in-app edit.
4. **Distribution** — winget manifest (needs the Windows zip SHA256 from a release
   with assets), overflow chevrons for tabs/sidebar, damage tracking + CI perf
   budget.
5. **Customization depth** — per-workspace config (cwd/env/command/color/icon),
   theme-aware plugin accents, configurable keybinds UI.

Lightweight budget is measured and gated: 49 MB binary, 365 ms to first frame,
~178 MiB idle RSS (Linux/ADL-N/Xvfb; real GPU is faster). Re-run
`scripts/lightweight.sh` after big renderer changes.

## Known gaps / gotchas

- macOS and Windows behavior is unverified on this machine; the close-button
  and ConPTY paths are reasoned/tested but not run there. Kitty keyboard
  support is opt-in (`[terminal] kitty_keyboard`, default `false`) precisely
  because ConPTY's CSI-u handling is unproven.
- Windows input triage: `set BERTH_DEBUG=1` then check
  `%APPDATA%\com\berth\berth\berth.log` for `kbd write:` lines — present means
  keys reached the PTY, absent means the UI dropped them.
- Rune (`anomalyco/rune`) was evaluated as a UI framework: rejected for the
  terminal surface (0.0.1 on a pinned dev rev, own event loop/renderer, still
  needs a custom cell-grid path). Same wgpu/winit majors, so a settings-window
  spike remains cheap later.
- Touch is tap-to-click only: `WindowEvent::Touch` re-dispatches synthetic
  cursor + left-button events through `window_event`, so every mouse path
  (chrome zones, double-tap, selection drag, tab drag, sidebar resize) works
  unchanged. First contact only; extra fingers ignored. Windows promotes the
  first touch point to mouse input, so promoted mouse events within 600 ms of
  a touch are dropped unless they are our own dispatch. Touch scroll / pinch
  are not bound yet.
- Clear-scrollback uses ED 3 through the parser (alacritty's `clear_screen` is
  private) — fine, but note it keeps the visible screen by design.
- Fullscreen under bare Xvfb cannot be visually distinguished (no WM); the
  action/log path was smoke-tested. F11 needed modifiers cleared via `xdotool
  keyup` in that environment.
- Search highlight tint was not visually confirmed (bar/text were); logic is
  covered by emulation tests.
- `crates/plugins/guest/target` was accidentally tracked once and is now
  ruled out by `.gitignore`; keep an eye on large diffs.
- Commit messages use conventional prefixes; release-please parses them.
