<!-- docs/ROADMAP.md — merged from the GLM-5.3 product/code review (2026-11-03, made against main @ 0.16.0) plus the shipped-work status. Full original report: /tmp/opencode/berth-review.md (uncommitted). -->

# berth — Roadmap

Consolidated from the external product/code review + shipped work. Status markers:
**[done]** · **[open]** · **[deferred]**.

Review acks (do-not-break list): clean crate boundaries (core has no UI deps);
alacritty_terminal + portable-pty is the right emulation core; the keybind
replace-defaults table design; hot-reload keeping previous config on parse
error; snapshot+queued-rename plugin pattern; the release matrix.

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
| S1 | Renderer loss unrecoverable (black window) | **[open]** — recreate Renderer on draw failure |
| S1 | Atlas exhaustion → cache wipe + torn frames | **[open]** — LRU eviction or texture pages (CJK screens) |
| S2 | DECCKM application cursor keys ignored | **[open]** (`term.mode().application_cursor` → `ESC O A…`) |
| S2 | Ctrl+digit (2..8) sends nothing | **[open]** — NUL/ESC/FS/GS/RS/US/DEL table |
| S2 | Bold/italic/underline/strikethrough not rendered | **[open]** — second faces + underline quads; synth-bold fallback |
| S2 | New tab/split/workspace loses cwd (`Tab.cwd` dead) | **[open]** — forward OSC 7 from TermListener → per-pane cwd → spawn cwd; also `basename(cwd)` tab labels |
| S2 | Close pane/tab/workspace kills live jobs without confirm | **[open]** — extend window-close banner to live-shell closes; single-pane escape hatch |
| S2 | No IME handling (`WindowEvent::Ime`) | **[open]** — CJK input blocker |
| S2 | Tab/sidebar overflow past ~6/25 entries unreachable | **[open]** — shrink-to-fit + ellipsis + chevrons/scroll |
| S2 | Palette math: sRGB constants through linear pipeline (washed) | **[open]** — convert constants once or mark targets consistently |
| S2 | PTY write errors ignored → typing into dead pane; no exit-status surface | **[open]** — "process exited (N)" pane state; also Bell → attention + OSC 52 clipboard |
| S2 | No scroll-to-bottom on keypress | **[open]** |
| S3 | rename blur discards buffer; close-tab no-ops; F1–F4 modifiers; kitty keyboard protocol; 750ms wakeup + font rescan on UI thread; `--version`; config docs; `FontCtx` unsafe-transmute hygiene; guest crate in host builds | **[open]** (kitty keyboard protocol: user-requested) |

## Identity: workspaces as real project containers (review §4b)

- **[done]** Tab drag between workspaces (sidebar as drop target; empty origin ws dies).
- **[open]** Drag panes between tabs.
- **[open]** Per-workspace `[workspaces.<id>]` config: cwd, env, command, color/icon; auto-name from git basename core-side.
- **[open]** Session persistence ("berths"): serde the `Layout` tree + labels + cwd + per-pane command; "Reopen last session" on launch. #1 structural advantage over cmux (crash-loss complaints) and the tmux-detach crowd; cheap because core has no UI deps.
- **[open]** Quick switcher (`cmd+p`) over workspaces/tabs.
- **[open]** Activity badges: output-since-last-look + child-exited dots on sidebar entries/tabs (event stream already carries PaneId) — *the* cmux wedge, done open-source.
- **[open]** Keyboard split-resize (`alt+shift+arrows` → ratio ±5%).
- **[deferred]** Workspace templates (`berth new --template …`); pop-out tab → new window (needs multi-window).

## Plugin platform (review §5 — next on tier 1)

Current tier 1 (shipped): `get-workspaces`/`get-tabs` snapshot + `set-workspace-label` + `on-layout-change`, host wired, auto-namer guest shipped.

Safety/pipeline, in risk order:
1. **[open]** Fuel/epoch limits + off-thread execution (a guest `loop{}` hangs the UI today); marshal results via proxy (same shape as reader threads).
2. **[open]** Guest error logging/surfacing (`let _ = call_on_layout_change` swallows traps silently) + disable affordance.
3. **[open]** `[[plugin]]` capability manifest (`layout.read`, `layout.rename`, …) before tier 2 adds power; unknown capability = load error.
4. **[open]** Change-event diff payload (or guests O(n²)-diff snapshots); rename conflict policy (first-writer-wins?); rename validation (empty/oversized/dup).
5. **[open]** Lifecycle/versioning: host-version advertisement, WIT semver policy, single-source WIT (guest copy will drift).
6. **[open]** Tier 2 shape: `set-tab-label`, `badge(target, text, color)` (pairs with core activity badges), `register-command` → quick switcher, notifications. Tier 3 observability: rate-limited PTY output taps, OSC event stream. Tier 4 control: spawn/move/`send-text` (the `berth` CLI/IPC = cmux `send` equivalent).
7. **[open]** Document the supported std surface for wasm32-wasip2 guests (rustc shim imports WASI at link time) or move tier-2 guests to `wasm32-unknown-unknown` + bindgen-only.

## Table stakes still missing (review §4a, priority order)

Mouse reporting **[done]** · bracketed paste/copy-on-select **[done]** ·
**[open]**: bold/italic faces; OSC 7 cwd; scrollback search (`ctrl+shift+f`
overlay, incremental highlight via a second Selection); clickable links (OSC 8 +
regex, `cmd+click`); cursor shapes/blink + `WindowEvent::Focused`; fullscreen +
clear-to-scrollback binds; `[shell]`/`command`+`args`+`env` config (can't start
fish/login shells today), `working-dir`, `scrollback` size, theme/`[palette]`;
IME; scroll-to-bottom; DECCKM + kitty keyboard probe; bell→attention;
exit-status surfacing; wide-glyph advance audit.

## Distribution / branding (review §4d)

- **[done]** App icon everywhere (window icon, .ico, .icns + Info.plist, ad-hoc codesign).
- **[open]** README screenshot/GIF (sidebar+workspaces demo); Linux `.desktop` + AppStream (AppImage/deb later); winget manifest; `sha256sums.txt`; cargo-binstall metadata; config reference doc (per-OS paths — the #1 new-user question); CHANGELOG 0.15.0 dedupe; `--version`.
- **[deferred]** macOS notarization ($99/yr) — right-click→Open documented meanwhile.
- **[open]** CI has no app-layer tests: property tests for the split-tree (`layout.rs` grab/resize/neighbor math — the most intricate pure logic, zero tests today), `encode_key` golden-bytes tests, config parse tests, vttest/vte-conformance vs `Pane`; promote `BERTH_FRAME_DUMP` + lavapipe into CI for renderer snapshot diffs.
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
3. **Wire plugins with a safety model, prove with 2 guests** — host + auto-namer **[done]**; limits/capabilities/errors & badge-plugin guest **[open]**.
