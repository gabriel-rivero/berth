# Session handover — 2026-11-10

State for the next session. History lives in `docs/ROADMAP.md`; this file is
the short version: what landed, what to run, what is next.

## Branch state

`main` at `8c210b3` (v0.26.1), pushed, all tests/clippy/fmt green. Since v0.25.0:

```
8c210b3 fix(input): make kitty keyboard protocol opt-in, Windows-safe IME gating
cb405a5 ci(release): fix macOS arm runner and Windows archive version
306f08e chore(main): release 0.26.0
844ce96 plugins: enforce per-plugin capability manifests
cb22717 ui: kitty keyboard protocol (CSI-u encoding from pushed flags)
1f5e980 chore(main): release 0.25.0
```

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
   platforms". Run the 0.26.1 Windows build with `set BERTH_DEBUG=1`, confirm typing
   (look for `kbd write:` lines in `%APPDATA%\com\berth\berth\berth.log`), then check
   ConPTY resize, IME composition end, and modifier chords. Same for macOS.
2. **Plugin-facing polish (item 8 remainder)** — status segments shipped via
   host `set-status`/`accent` imports; still open: plugin manager panel (list,
   enable/disable, last error, reload), richer segment
   metadata (badges/icons/click actions).
3. **Distribution** — winget manifest (needs the Windows zip SHA256 from a release
   with assets), in-app config access (`Config` still has no `save()`), overflow
   chevrons for tabs/sidebar, damage tracking + CI perf budget.
4. **Customization depth** — per-workspace config (cwd/env/command/color/icon),
   theme-aware plugin accents, settings panel backed by a config writer.

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
