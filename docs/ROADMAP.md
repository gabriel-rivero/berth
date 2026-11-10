<!-- docs/ROADMAP.md — merged from the GLM-5.3 product/code review (2026-11-03, made against main @ 0.16.0) plus the shipped-work status. Full original report: /tmp/opencode/berth-review.md (uncommitted). -->
<!-- 2026-11-04: qwen review @ v0.21.4 (38cbed0) folded in — runtime-verified under Xvfb/xdotool (scales 1+2) + user-reported UX list cross-checked. Full report: docs/UI-REVIEW-qwen.md (BUGS-n / U-n refs below point into it). -->
<!-- 2026-11-06: post-0.24.0 Gemini-fix audit (wheel / colors / hover / close buttons / GPU) — findings and the new "visual & terminal quality" wave folded in. -->

# berth — Roadmap

Consolidated from the external product/code review + shipped work. Status markers:
**[done]** · **[open]** · **[deferred]**. Items tagged *(qwen)* come from the Nov-2026
qwen review; items tagged *(user-reported)* from the user UX list (statuses in
`docs/UI-REVIEW-qwen.md` §4).

## Vision (north star, Nov 2026)

**A Rust cmux clone with hardware acceleration that behaves near-equally across all
major desktop OSes, with a modern look & feel, plugin support, and heavy
customizability.**

The five pillars and where they stand:

| Pillar | Meaning | Status / where tracked |
| --- | --- | --- |
| **cmux shape** | Workspaces as project containers, tabs in a left sidebar, splits, session persistence, a `berth` CLI/IPC for scripting | Layout/workspaces/persistence **[done]**; quick switcher, activity badges, CLI/IPC **[open]** (Identity, Plugin tier 4) |
| **Hardware accelerated** | GPU-drawn cells + chrome, no webview; honest adapter reporting and a usable software fallback | **[done 2026-11]** — wgpu renderer, `--gpu-info`, sRGB pipeline, lost-surface recovery; damage tracking + CI perf budget **[open]** (visual wave 7) |
| **Near-equal on Linux/macOS/Windows** | Same rendering, keybinds, config and input semantics everywhere | Linux verified (Xvfb + real); macOS/Windows reasoned + CI-built but **not manually verified** (no hardware) — IME, ConPTY resize and modifier mapping remain the risk areas |
| **Modern look & feel** | Rounded chrome, real font faces, themes, hover/press states, smooth scrolling, no visual glitches | **[done 2026-11]** — theme presets/overrides, real bold/italic, pill chrome, cursor styles, frame recovery; subpixel glyphs, ligatures, animations **[open]** (visual wave) |
| **Plugins & customization** | Sandboxed WASM plugins; every visual/behavioral knob in TOML, hot-reloaded; no rebuild-to-configure | Plugin host + fuel/memory caps + capability manifests + status segments + **plugin manager UI (enable/disable, error list, rescan)** **[done 2026-11]**; change-event diffs, tier 2–4 APIs, settings panel **[open]**. Config covers font/theme/terminal/shell/keybinds and hot-reloads; `Config::save()` shipped for plugin toggles |

Release cadence: conventional commits → release-please bumps `Cargo.toml`, `CHANGELOG.md`
and tags `v*`; the release workflow builds all five targets and attaches archives, deb/rpm
and checksums. Patch releases should be cheap; every wave updates this file in the same PR.

**Acceptance rule (single source of truth):** every future feature wave must update this
roadmap in the same PR — new items, status flips, and stragglers land here before release.

Review acks (do-not-break list): clean crate boundaries (core has no UI deps);
alacritty_terminal + portable-pty is the right emulation core; the keybind
replace-defaults table design; hot-reload keeping previous config on parse
error; snapshot+queued-rename plugin pattern; the release matrix.
(qwen adds: per-pane reader/writer threads; shared `Chrome`/hit-test helpers — the
close-button defect below is branch *ordering*, not duplication; the dlog +
BERTH_FRAME_DUMP + Xvfb harness — promote it to CI hit-test smokes.)

## Milestone view

Today the binary is **a real terminal** (v0.21+) except where marked below.
Three pillars for "1.0":

1. **Trust** — remaining S1/S2 correctness cluster (below).
2. **Identity** — workspaces as real, persistent project containers.
3. **Platform** — plugin host that is safe, observable, and alive by default.

## S1/S2 correctness cluster (review §2, remaining)

| Sev | Item | Status |
|-----|------|--------|
| S1 | Chrome geometry in device px (HiDPI) + `ScaleFactorChanged` handled | **[done]** (Chrome struct, `winit` scale wire-up) |
| S1 | QUAD budget silently drops cells on dense panes | **[done]** (24 MiB + one-shot dlog; still drops, never thrashes) |
| S1 | Bracketed paste; blocking PTY writes | **[done]** (DECSET 2004 wrap; dedicated writer thread per pane) |
| S1 | Quit path never calls `event_loop.exit()` | **[done]** (`exiting` + explicit close path) |
| S1 | Mouse reporting (X10/SGR/alternate-scroll) | **[done]** (incl. button 0/1/2 + mod bits) |
| S1 | Wheel scrolls focused pane → hovered pane | **[done]** |
| S1 | Focused-pane indicator | **[done]** (accent outline; cursor still identical — dim unfocused cursors **[open]**) |
| S1 | 5s keyboard blackout during quit-confirm | **[deferred]** (only scroll/escape flow suffers; revisit when confirm path gets per-key routing) |
| S1 | Renderer loss unrecoverable (black window) | **[done 2026-11]** — `Outdated` reconfigures; lost device/surface rebuilds the renderer with backoff (see visual wave item 7) |
| S1 | Atlas exhaustion → cache wipe + torn frames | **[open]** — LRU eviction or texture pages (CJK screens) |
| S1 | **Default `ctrl+alt+←/→` workspace cycling is dead** *(qwen BUGS-1, runtime-verified)* — hardcoded alt+arrow pane-focus intercept (`app.rs:899-916`) runs before the bindings table and swallows the chord silently; alt+arrow is also unbindable for users | **[open]** — table-first lookup; promote pane focus to `focus-pane-*` actions |
| S1 | Wheel scroll only (never history arrows) in normal shell | **[done 2026-11]** — root cause: `TermMode::default()` includes `ALTERNATE_SCROLL`, so the old `ALTERNATE_SCROLL`-only check arrowed through history on every tick. `wheel_target()` now requires `ALT_SCREEN|ALTERNATE_SCROLL` (mouse-reporting wins, else viewport scroll) + unit tests |
| S1 | **Sidebar edge-grab branch steals the workspace close-✕** *(qwen BUGS-2 = user-reported "close button not working on tabs/workspaces", CONFIRMED at scales 1+2)* — grab band `|x−sb_w|≤4·scale` checked first with **no y-guard** (`app.rs:1455`) overlaps the right ~29% of the ✕ zone (`app.rs:1465`): clean click = no-op, jitter = surprise resize. Same band eats pane clicks and leftmost tab-strip clicks (8px full-height dead band). Tab-✕ geometry itself verified correct — perception side: no hover (below) + drag-arm-on-press + last-tab-✕ exits app | **[done 0.21.6]** — press restructured: ✕-check first inside sidebar branch, edge band only below the tab strip (+ same guard in cursor icons); tab-drag motion threshold added. **[done 2026-11]** — all press/middle-click/cursor paths now dispatch on the single shared `chrome_zone_at()` (sidebar → tab strip → edge band → pane), removing the last draw/hit-test drift; `tests/chrome_hit.rs` locks geometry at DPI 1x + 2x incl. close-✕ vs edge-grab precedence |
| S1 | **Colors washed out** *(user-reported "colors look wrong")* — UI constants authored as sRGB but fed to a linear pipeline | **[done 2026-11]** — `srgb_to_linear()` applied to constants and truecolor/256-color specs exactly once; sRGB surface format explicitly selected (warn + fallback otherwise); unit tests pin endpoints/midpoint and "palette is linear, not sRGB" |
| S1 | **Hardware acceleration unverified** *(user-reported)* — adapter silently chosen, no diagnostics, no fallback | **[done 2026-11]** — GPU name/backend/device-type logged every launch to `berth.log`; `berth --gpu-info` enumerates adapters without a window; warns on `DeviceType::Cpu` (software) and non-sRGB surfaces; adapter request retries with the software fallback before failing |
| S2 | DECCKM application cursor keys ignored | **[done 2026-11]** — `cursor_key()` emits `ESC O A…` for unmodified arrows/Home/End when `TermMode::APP_CURSOR` is set; modified keys keep the CSI modifier form. Kitty keyboard protocol **[done 2026-11, opt-in]** |
| S2 | Ctrl+digit (2..8) sends nothing | **[open]** — NUL/ESC/FS/GS/RS/US/DEL table |
| S2 | Bold/italic/underline/strikethrough not rendered | **[done 2026-11, real + faux fallback]** — real bold/italic/bold-italic faces resolved from the family; swash `embolden`/skew only as synthetic fallback; underline/double-underline/strikeout quads, SGR reverse (INVERSE) and DIM honored; ligatures remain [open] |
| S2 | New tab/split/workspace loses cwd (`Tab.cwd` dead) | **[done 0.24.0]** — OSC 7 extracted from raw PTY bytes in the reader thread (cross-chunk, BEL + ESC\, percent-decode; unit-test covering split carry) → per-pane cwd pool → `Tab.cwd` write-back → new tab/split/workspace inherit the focused pane's cwd. Leftover: `basename(cwd)` tab labels + shell-integration docs/plugins. |
| S2 | Close pane/tab/workspace kills live jobs without confirm | **[open]** — extend window-close banner to live-shell closes; single-pane escape hatch. *(qwen U-4: also covers the ✕ cascade — closing the last tab of the last workspace exits the app instantly, `app.rs:1275-1281`, with no banner)* **[done 2026-11]** — last-tab/last-workspace ✕ now arms the quit banner instead of killing the shell; multi-pane ✕ already confirms, single-pane workspace close still immediate |
| S2 | No IME handling (`WindowEvent::Ime`) | **[done 2026-11]** — `set_ime_allowed(true)`; `Ime::Commit` routes to search query / rename editor / focused shell; `Ime::Preedit` renders at the cursor with an accent underline; while IME is enabled, plain `KeyboardInput` text is suppressed to avoid double input on Wayland/XIM |
| S2 | Tab/sidebar overflow past ~6/25 entries unreachable | **[open]** — shrink-to-fit + ellipsis + chevrons/scroll |
| S2 | Palette math: sRGB constants through linear pipeline (washed) | **[done 2026-11]** — see the S1 colors row: constants + `Color::Spec`/indexed 16-255 converted once via `srgb_to_linear()`; host palette returned pre-linearized; sRGB surface format enforced |
| S2 | PTY write errors ignored → typing into dead pane; no exit-status surface | **[partial]** — non-zero shell exits raise a transient `shell exited (code N)` notice and background-tab bells light an attention dot (2026-11); dead-pane input guard + OSC 52 clipboard still open |
| S2 | No scroll-to-bottom on keypress | **[open]** |
| S2 | **Redraw storm: full redraw per PTY Output event** *(qwen BUGS-3, verified: `seq 1 3000` → 689 presents)* — `user_event` calls `redraw()` directly (`app.rs:1216-1221`); vsync-blocked presents stall input under output floods | **[done 0.21.8]** — `PaneEvent::Output` now pumps + `request_redraw()` only (title/closed redraws kept synchronous; damage tracking later). Verified: `seq 1 3000` → 37 draw frames (was 689) |
| S2 | **No tab-drag threshold: clicks teleport tabs + delete the emptied origin workspace** *(qwen BUGS-4)* — press arms `tab_drag` (`app.rs:1508-1515`); first CursorMoved over sidebar moves the tab and drops the origin ws (`app.rs:1381-1384`); 2px jitter = data loss, no undo | **[done 0.21.6]** — `TAB_DRAG_THRESHOLD` 6 logical px; origin recorded on press; drag arm early-returns before threshold (xvfb-verified: jitter no-ops, real drag moves) |
| S2 | **No hover states / pointer cursor on any chrome** *(user-reported "no ui hover animations on clickable buttons" — CONFIRMED; qwen BUGS-6)* — hover never reaches the renderer (`draw` gets only `active` bools); `update_cursor` emits resize icons only (`app.rs:758-792`) | **[done]** — `CursorIcon::Pointer` + visual hover highlight on sidebar rows/✕/`+` and tab slots/✕/`+`; hover and press share one `chrome_zone_at()` (empty strip/sidebar regions are clickable targets for double-click new tab/workspace) |
| S2 | **No middle-click close of tabs/workspaces** *(user-reported — CONFIRMED, probe: total no-op; qwen BUGS-5)* — non-left buttons only forward to capturing panes (`app.rs:1575-1594`) | **[done 0.21.8]** — Middle-Pressed over tab slot → `close_tab`, over workspace row → `close_workspace_at` (never the only workspace), else forwards to TUI; xvfb-verified end-to-end |
| S2 | **`close-tab` keybind is a trap** *(qwen BUGS-7; upgrades the old S3 "close-tab no-ops")* — action returns false (`app.rs:960-963`); binding it silently steals the chord's old action AND the fall-through **types the letter into the shell** | **[done 0.21.6]** — closes active tab (kills its panes); refuses on the only tab of the only workspace (`last tab -> ignored`); user keybind verified via `close-tab = [ctrl+alt+t]` |
| S2 | **Windows: broken config = silent launch failure** *(qwen BUGS-8)* — `Config::load()?` in `main.rs` errored to a nonexistent stderr (GUI subsystem) before the dlog sink started | **[done 2026-11]** — config loading moved entirely into `berth_ui::run()`: a parse error is logged unconditionally to `berth.log`, the app boots with `Config::default()`, and a 30 s on-screen notice names the error. `main.rs` no longer loads config. |
| S2 | **Word/line selection: double/triple-click** *(qwen BUGS-9)* — pane presses now carry a click streak (same pane+cell, 500 ms) into `SelectionKind::{Simple,Word,Line}` → alacritty `Simple`/`Semantic`/`Lines`; drag still extends the same mode | **[done 2026-11]** |
| S3 | rename blur discards buffer; F1–F4 modifiers; 750ms wakeup + font rescan on UI thread; config docs; `FontCtx` unsafe-transmute hygiene; guest crate in host builds | **[open]** (kitty keyboard protocol **[done 2026-11]**) |
| S3 | **Stale OSC titles** *(qwen BUGS-10)* — `titles` never pruned on Closed + PaneId reuse mislabels fresh tabs with dead panes' titles (`app.rs:1244-1288`, `layout.rs:563-573`) | **[done]** — `titles.remove(&id)` in the Closed handler |
| S3 | **Second `resumed()` panics** *(qwen BUGS-11)* — duplicate `spawn_pane(PaneId(0))` hits `panic!("duplicate PaneId")` (`app.rs:440-442`) | **[done]** — guard on `window.is_some()` |
| S3 | **Rename editor quirks** *(qwen BUGS-12)* — ArrowDown commits; no caret motion/paste/IME; swallows all chords while open | **[done 2026-11]** real single-line editor: ←/→ (+shift select, alt = word jump), Home/End, ⌘/ctrl+A, paste, delete-forward, IME text, Esc cancels, Enter or click-outside commits |

### User-reported UX list (Nov 2026) — cross-check status (qwen review §4)

| Quote | Verdict | Tracked as |
|-------|---------|-----------|
| "no ui hover animations on clickable buttons" | **CONFIRMED** (no hover plumbing; no Pointer cursor) | S2 hover row above (BUGS-6) |
| "close button not working on tabs/workspaces" | **CONFIRMED (workspace ✕)**: edge-grab branch beats close-✕ hit-test, no y-guard — right ~29% of the ✕ starts a resize (verified scales 1+2). **Tab ✕: PLAUSIBLE** — geometry verified correct; perception = no hover + drag-arm-on-press + last-tab-✕ exits app | S1 edge-grab row (BUGS-2) + BUGS-4/BUGS-6 + confirm row — **done** (edge reorder 0.21.6; shared zone + hover + DPI tests + last-tab confirm 2026-11) |
| "no button to hide/show the workspaces" | **CONFIRMED** (config `[sidebar].show` only, no action/button) | Identity section (U-1) |
| "no config panel" | **CONFIRMED** (no settings UI; `Config` has no `save()` — nothing writes back today) | Distribution section (U-5) |
| "no plugin enable/disable interface" | **CONFIRMED** (load-all at startup; no manifest/toggles/rescan) | Plugin platform item 8 (U-6) |
| "no middle-click close of tabs/workspaces" | **CONFIRMED** (probe: strip middle-click = total no-op) | S2 middle-click row (BUGS-5) |

## Identity: workspaces as real project containers (review §4b)

- **[done]** Tab drag between workspaces (sidebar as drop target; empty origin ws dies).
- **[open]** Drag panes between tabs.
- **[open]** **Sidebar show/hide affordance** *(user-reported; qwen U-1)*: `toggle-sidebar` keybind action + chevron button in the sidebar header — today only `config.toml [sidebar].show` + hot-reload. **[done 0.21.9]** toggle-sidebar action (defaults cmd+u / ctrl+shift+u) + plus buttons; chevron header still [open].
- **[done 2026-11]** **Tab reordering** *(qwen U-8)*: dragging a tab within the strip reorders it live (`Workspace::reorder_tab`, identity/tree/focus preserved); dragging onto a sidebar entry still moves it between workspaces.
- **[partly done 2026-11]** **Discoverable chrome buttons** *(qwen U-9/U-7/U-10)*: tab-strip trailing `+` and sidebar `+` shipped; wheel-over-strip now cycles tabs and wheel-over-sidebar cycles workspaces (U-10 done), as do `ctrl+1…9` (9 = last tab). Still open: visible `+ ws` header affordance, right-click context menus (tab: close/rename/close-others/move-to-ws; entry: rename/close; pane: copy/paste/split/zoom).
- **[open]** Per-workspace `[workspaces.<id>]` config: cwd, env, command, color/icon; auto-name from git basename core-side.
- **[done 0.23.0]** Session persistence ("berths"): layout tree (workspaces/tabs/splits + labels) snapshotted to `config_dir/berths.json` every ~4s and on exit; relaunched restores the tree and re-spawns a fresh shell per pane. New tab/split/workspace cwd now inherited from the focused pane's OSC 7 (see S2 row).
- **[open]** Quick switcher (`cmd+p`) over workspaces/tabs.
- **[open]** Activity badges: output-since-last-look + child-exited dots on sidebar entries/tabs (event stream already carries PaneId) — *the* cmux wedge, done open-source.
- **[open]** Keyboard split-resize (`alt+shift+arrows` → ratio ±5%).
- **[deferred]** Workspace templates (`berth new --template …`); pop-out tab → new window (needs multi-window).

## Plugin platform (review §5 — next on tier 1)

Current tier 1 (shipped): `get-workspaces`/`get-tabs` snapshot + `set-workspace-label` + `on-layout-change`, host wired, auto-namer guest shipped.

Safety/pipeline, in risk order:
1. **[done 2026-11]** Fuel + memory limits: 50 M fuel per `on-layout-change`
   and a 64 MiB guest memory cap; a trapping guest is logged and skipped for the
   session (no UI freeze). Guest calls still run on the UI thread, but metering
   bounds them; off-thread marshaling only matters for slow *legit* guests.
2. **[done 2026-11]** Guest error surfacing: `call_on_layout_change` results are
   checked, logged with the plugin name, and faulted plugins are not retried
   (`LoadedPlugin::is_faulted`). Remaining: in-app error list/disable toggle.
3. **[done 2026-11]** Capability manifest: `<stem>.toml` beside the component
   (`name`, `capabilities = ["rename", "status"]`). Unknown capabilities or keys
   are load errors; a missing/empty manifest grants nothing (read-only plugin).
   Enforced per callback: denied renames/status are dropped, first denial per
   plugin is logged as `plugin '<name>' ignored: needs '<cap>' capability`.
4. **[open]** Change-event diff payload (or guests O(n²)-diff snapshots); rename conflict policy (first-writer-wins?); rename validation (empty/oversized/dup).
5. **[open]** Lifecycle/versioning: host-version advertisement, WIT semver policy, single-source WIT (guest copy will drift).
6. **[open]** Tier 2 shape: `set-tab-label`, `badge(target, text, color)` (pairs with core activity badges), `register-command` → quick switcher, notifications. Tier 3 observability: rate-limited PTY output taps, OSC event stream. Tier 4 control: spawn/move/`send-text` (the `berth` CLI/IPC = cmux `send` equivalent).
7. **[open]** Document the supported std surface for wasm32-wasip2 guests (rustc shim imports WASI at link time) or move tier-2 guests to `wasm32-unknown-unknown` + bindgen-only.
8. **[done 2026-11]** **Plugin manager UX** *(user-reported "no plugin enable/disable interface" — CONFIRMED; qwen U-6)*: `cmd+shift+p` / `ctrl+shift+p` opens a centered panel listing every `plugins/*.wasm` with `[x]`/`[ ]` enable state and a one-line status (`disabled`, `manifest error: …`, `loaded · rename, status`, `faulted (skipped)`, `failed: …`, `not loaded`). `space`/`enter` flips the flag, persists `[plugins]` via atomic `Config::save()`, and hot-reloads the host; `r` rescans; `esc`/the same chord closes. Config hot-reload also diffs `[plugins]` and rescans. Staging history: `[[plugin]] name/enabled` flags **[done 0.21.9]** → list view → hot (re)scan.

## Visual & terminal-quality wave (Nov 2026) — "good looking lightweight terminal with plugins"

Goal: the binary should *look* like a modern terminal out of the box (not just behave
like one) while staying small and GPU-cheap. Ordered by perceived impact per effort.

1. **[done 2026-11]** Color correctness + sRGB pipeline (see S1 colors row) and
   **GPU honesty**: adapter/backend logged every run; software fallback + non-sRGB
   surface warned; no silent lavapipe.
2. **[done 2026-11, faux→real faces + font config; subpixel/ligatures open]**
   **Font quality** *(the single biggest "looks cheap" factor)*: real bold/italic/
   bold-italic faces are resolved from the same family (fontdb weight/style query;
   synthetic embolden/skew only when the family lacks them); underline/double
   underline/strikethrough quads; per-font fallback chain with diagnostics;
   `[font] family/size_pt/line_height` config, hot-reloaded. Still open: fractional
   (subpixel-positioned) glyph placement + gamma-correct blending, cell padding,
   `.woff2`/variable-font tuning, ligatures (off by default).
3. **[done 2026-11, auto-switch open]** **Theme system**: `[theme] preset = "dark"|"light"`
   plus `background`/`foreground`/`cursor` and 16 `[theme.palette]` `#rrggbb` overrides;
   separate chrome palette (tab strip/sidebar/divider/banner) from the ANSI palette;
   hot-reload applies live; unknown keys and malformed colors fail loudly in the log.
   Still open: OS light/dark auto-switch, contrast-checked user colors, per-theme
   chrome accent override knobs, theme-aware plugin accents.
4. **[done 2026-11, overflow chevrons open]** **Chrome polish**: pill-shaped
   tab slots and sidebar rows (rounded-corner SDF in the quad shader), shared
   spacing, tabs shrink from 140→96 logical px to fit the strip, `✕` revealed
   only on the active/hovered tab or workspace (hit zones unchanged, so no
   layout shift), hover states and the shared zone map. Still open: overflow
   chevrons/scrollable strip when even the minimum width doesn't fit, and a
   color-blind-safe focused-pane affordance audit.
5. **[done 2026-11, animation/drag open]** **Scrolling feel**: sub-line trackpad
   pixel deltas accumulate across events (no more dropped smooth scrolling);
   typing while scrolled back snaps to the live tail; while off the bottom the
   focused pane shows an overlay scrollbar + `↑N` badge. Still open: animated
   pixel/line scrolling, scrollbar drag, wheel-over-strip/sidebar tab cycling
   (see Identity). Wheel routing in panes is **[done 2026-11]**.
6. **[done 2026-11]** **Cursor & focus**: `[terminal] cursor_style =
   "block"|"beam"|"underline"`, `cursor_blink`, `cursor_blink_ms` (hot-reload,
   clamped); blink only while the window is focused and reset to solid on
   input; unfocused window shows a hollow block; only the focused pane draws a
   cursor (multi-pane bug fixed).
7. **[done 2026-11, damage tracking/CI budget open]** **Frame quality under
   load**: lost-surface/device recovery (renderer rebuilt with a 500 ms backoff;
   `Outdated` reconfigures instead of freezing), and a `BERTH_DEBUG=1` HUD with
   smoothed CPU draw time + quad count. Still open: damage tracking (only
   repaint dirty cells), atlas paging/LRU (exhaustion currently clears the glyph
   cache mid-frame), GPU-timestamp timing, and a CI perf budget
   (e.g. `seq 1 3000` < N frames, idle CPU ≈ 0).
8. **[partial 2026-11]** **Plugin-facing polish**: guest safety is done (fuel +
   memory caps, trap logging/auto-disable); `set-status` lets guests publish a
   short right-aligned tab-strip segment (clipped, refreshed each callback) and
   `accent` hands them the theme's `#rrggbb` accent (new `[theme] accent`, preset
   fallback), so plugin UI can match berth. Still open: plugin manager panel
   (enable/disable, error list, reload), richer segment
   metadata (badges/icons, click actions).
9. **[done 2026-11, self-hosted GPU budget open]** **Lightweight budget**: `--version`,
   `scripts/lightweight.sh` (binary size, first-frame latency, idle RSS under
   Xvfb) and a CI release-size gate (60 MiB) exist; keep the wgpu/winit-only
   stack (no webview/Electron) and lazy plugin host. Reference numbers (Linux,
   Intel ADL-N, Xvfb/lavapipe): 49 MB binary, 365 ms to first frame, ~178 MiB
   idle RSS. Remaining: startup/RSS budget on a self-hosted GPU runner and
   per-release regression tracking.

## Table stakes still missing (review §4a, priority order)

Mouse reporting **[done]** · bracketed paste/copy-on-select **[done]** ·
`[shell]` program/args/env + `working_dir` + `[terminal] scrollback` **[done 2026-11]** ·
scrollback search (`ctrl+shift+f`/`cmd+f`, incremental case-insensitive
highlight + hit cycling) **[done 2026-11]** ·
word/line selection (double/triple-click → semantic/lines selection,
qwen BUGS-9) **[done 2026-11]** ·
clickable links (OSC 8 + scheme-scan, safe-scheme allowlist, hover underline +
`cmd`/`ctrl`-click opening through the OS handler) **[done 2026-11]** ·
 fullscreen (`f11`/`alt+enter`, borderless toggle) and clear-scrollback
 (ED 3, keeps the visible screen) **[done 2026-11]** ·
 zoomed-pane badge + window title carrying workspace/tab/pane context and a
 `[zoom]` marker **[done 2026-11]** · non-zero shell exit notice **[done 2026-11]** ·
 drag-selection auto-scroll past pane edges (40 ms timer, edge-anchored
 extension — qwen U-13) **[done 2026-11]** ·
kitty keyboard protocol (query/push/pop flags accepted; CSI-u encoding with
modifier/event params, disambiguated Esc/Enter/Tab/Backspace) **[done 2026-11,
now opt-in]** — gated behind `[terminal] kitty_keyboard` (default `false`) after
a Windows typing regression; alternate-key and associated-text parameters still
to come ·
wide-glyph audit (glyph advance from swash; underline/strikeout decorations
now span both cells) **[done 2026-11]** ·
touch input: a tap is a left click at the contact point (chrome hit-test,
double-tap, pane focus/selection all reuse the mouse path; OS-promoted mouse
events within 600 ms of a touch are ignored so a tap never double-fires)
**[done 2026-11]** — touch scroll/pinch not bound yet.

All terminal table stakes from the Nov 2026 review are shipped; remaining work
is plugin polish and distribution (below).

## Distribution / branding (review §4d)

- **[done]** App icon everywhere (window icon, .ico, .icns + Info.plist, ad-hoc codesign).
- **[done 2026-11]** README screenshot (`docs/assets/berth.png`, captured headlessly under Xvfb; GIF later). Config reference **[done 2026-11]** (`docs/CONFIG.md`: all keys/defaults/per-OS paths/log location). Linux `.desktop` + AppStream metadata, `sha256sums.txt` on releases, and `--version` **[done 2026-11]** (validated in CI; AppImage/deb later). CHANGELOG dedupe **[done 2026-11]** (verified no duplicate headings; 0.15.0 survives only in compare links). **[open]** winget manifest (blocked: `v0.25.0` release currently has no uploaded assets, so no real Windows zip SHA256 exists yet — generate it in the release workflow when assets land); cargo-binstall metadata (intentionally skipped while `crates/app` has `publish = false`; add when the crate is published).
- **[open]** **In-app config access** *(user-reported "no config panel"; qwen U-5)*: `Config` has no `save()` — nothing writes back today. Staging: `open-config` action ($EDITOR / reveal path + copy) → settings panel (font/theme/keybinds; needs a config writer + schema). The "(later) settings UI" from the competitive notes, formalized.
- **[done 2026-11]** **README drift** *(qwen U-14)*: status line now tracks the release (v0.26.1), the keybinds table was corrected when `ctrl+alt+←/→` became reachable (BUGS-1), and "Remaining" points at this roadmap. Keep both in sync at release time.
- **[deferred]** macOS notarization ($99/yr) — right-click→Open documented meanwhile.
- **[open]** CI has no app-layer tests: property tests for the split-tree (`layout.rs` grab/resize/neighbor math — the most intricate pure logic, zero tests today), `encode_key` golden-bytes tests, config parse tests, vttest/vte-conformance vs `Pane`; promote `BERTH_FRAME_DUMP` + lavapipe into CI for renderer snapshot diffs; **Xvfb+xdotool chrome hit-test smokes** (the qwen probe scripts — `docs/UI-REVIEW-qwen.md` §7 — caught BUGS-1/2/3/5 headlessly; wire them as a CI gate so branch-order regressions can't recur).
- **[open]** Windows honesty: ConPTY resize semantics unverified on real hardware; the blank-pane triage (log-based) underway at time of merge.

## Competitive notes (why people switch *to* berth)

- **cmux**: stability + cross-OS + license + activity/CLI wedge; `berth` CLI/IPC with `new-tab`/`send` verbs.
- **Ghostty**: sidebar/workspaces story; must match performance perception (damage tracking, idle wakeups, no torn frames).
- **WezTerm**: TOML-vs-Lua config + sandboxed-WASM security story; must reach zoom/search/URL-level polish first.
- **tmux**: persistence + CLI; don't rebuild tmux, make "berth as window manager for shells" strictly nicer.
- **Windows Terminal**: workspaces + cross-OS config; (later) settings UI.

## Review's "3 things next" — tracked status

1. **"Trust it" S1 release** — core cluster **[done]**; stragglers tracked above.
2. **Finish workspace identity** — drag **[done]**, persistence/config/switcher/badges **[open]**.
3. **Wire plugins with a safety model, prove with 2 guests** — host + auto-namer **[done]**; safety limits + error surfacing **[done 2026-11]**; capability manifest **[done 2026-11]**, status segments & second (badge) guest **[open]**.

## qwen review's "next 3 work items" (Nov 2026, in order)

1. **Chrome-input correctness PR** *(S, ~200 lines)* **[done 0.21.6]** — BUGS-1: alt+arrows now `focus-pane-{up,down,left,right}` actions via the table (hardcoded intercept deleted; `ctrl+alt+←/→` workspace cycling verified reachable); BUGS-2: press precedence sidebar(✕ first) → tab strip → edge band (only below strip) → pane, same guard in cursor icons; BUGS-4: 6px tab-drag threshold; BUGS-7: `close-tab` implemented with last-tab guard. Xvfb-verified end-to-end. §7 CI hit-test smokes remain [open] (folded into the hover/affordance PR).
2. **Perceived-perf + affordance PR** *(S/M)* — BUGS-3 (coalesce redraws: dirty flag + `request_redraw`) + BUGS-6 (hover highlight + Pointer cursor via the shared zone fn) + BUGS-5 (middle-click close). Three of the six user reports answered; makes the perf story match the Ghostty competitive note.
3. **Chrome affordances PR** *(S-M)* — U-1 (`toggle-sidebar` action + header chevron) + U-9 (`+` buttons) + U-4 (confirm banner for ✕ cascades incl. last-tab-exits-app) **[done 0.21.9]** (ws close-confirm banner + titles pruning; last-tab guard shipped 0.21.6) + BUGS-10/11 nits. Then start plugin-platform item 8 staging (`[[plugin]] enabled` flags) alongside safety items 1-2.
