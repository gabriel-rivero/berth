# Session handover — 2026-11-06

State for the next session. History lives in `docs/ROADMAP.md`; this file is
the short version: what landed, what to run, what is next.

## Branch state

`main` at `83112cd`, pushed to origin. 16 commits since the v0.24.0 release
(`bc620e8`), all tests/clippy green at the time of writing:

```
83112cd lightweight budget probe, --version, CI 60 MiB size gate
6b0970a session handover doc
0be64f8 fullscreen, clear-scrollback, zoom badge + rich window title
fa9421c clickable links (OSC 8 + URL detection, hover underline, cmd/ctrl+click)
abd72d4 double-click word / triple-click line selection
387ee78 scrollback search (incremental query, hit cycling, live highlight)
dea96e7 [shell] program/args/env/working_dir + [terminal] scrollback
0e18b42 stop tracking crates/plugins/guest/target
aadbd3d plugin fuel (50M) + memory cap (64 MiB), trap surfacing, auto-disable
7e42e5c surface-loss recovery, BERTH_DEBUG frame HUD, confirm banner drawn last
33fd501 cursor shapes/blink, focused-pane-only cursor, hollow unfocused block
1d12eaf chrome polish: SDF rounded pills, shrink-to-fit tabs (140→96), hover-only ✕
35f4ed5 scrolling: sub-line wheel accumulation, tail snap, overlay scrollbar + ↑N badge
8fd8967 real bold/italic faces, [font] line_height, [theme] dark/light + palette overrides
44326f9 faux bold/italic, underline/strikeout, SGR reverse/dim
3b36c12 chrome hit-test unification, wheel routing fix (ALTERNATE_SCROLL trap), sRGB pipeline
```

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
`reveal_line`, `SelectionKind`, `ScrollInfo`, `Pane::clear_scrollback`.

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

1. **Plugin-facing polish (item 8 remainder)** — WIT export for status-bar
   segments/badges (currently only `on-layout-change` + host imports), plugin
   manager panel (list, enable/disable, last error, reload), capability
   manifest, theme-aware accent colors passed to guests.
2. **Table stakes still open** — IME; DECCKM + kitty keyboard probe;
   bell→attention; exit-status surfacing; wide-glyph advance audit; selection
   drag auto-scroll past window edges (qwen U-13).
3. **Distribution** — README screenshot/GIF, `.desktop` + AppStream, winget,
   `sha256sums.txt`, cargo-binstall metadata, config reference doc, CHANGELOG
   dedupe, in-app config access (`Config` still has no `save()`).

Lightweight budget is measured and gated: 49 MB binary, 365 ms to first frame,
~178 MiB idle RSS (Linux/ADL-N/Xvfb; real GPU is faster). Re-run
`scripts/lightweight.sh` after big renderer changes.

## Known gaps / gotchas

- macOS and Windows behavior is unverified on this machine; the close-button
  and ConPTY paths are reasoned/tested but not run there.
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
