<!-- docs/ROADMAP.md — merged from the GLM-5.3 product/code review (2026-11-03, made against main @ 0.16.0) plus the shipped-work status. Full original report: /tmp/opencode/berth-review.md (uncommitted). -->
<!-- 2026-11-04: qwen review @ v0.21.4 (38cbed0) folded in — runtime-verified under Xvfb/xdotool (scales 1+2) + user-reported UX list cross-checked. Full report: docs/UI-REVIEW-qwen.md (BUGS-n / U-n refs below point into it). -->

# berth — Roadmap

Consolidated from the external product/code review + shipped work. Status markers:
**[done]** · **[open]** · **[deferred]**. Items tagged *(qwen)* come from the Nov-2026
qwen review; items tagged *(user-reported)* from the user UX list (statuses in
`docs/UI-REVIEW-qwen.md` §4).

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
| S1 | Renderer loss unrecoverable (black window) | **[open]** — recreate Renderer on draw failure |
| S1 | Atlas exhaustion → cache wipe + torn frames | **[open]** — LRU eviction or texture pages (CJK screens) |
| S1 | **Default `ctrl+alt+←/→` workspace cycling is dead** *(qwen BUGS-1, runtime-verified)* — hardcoded alt+arrow pane-focus intercept (`app.rs:899-916`) runs before the bindings table and swallows the chord silently; alt+arrow is also unbindable for users | **[open]** — table-first lookup; promote pane focus to `focus-pane-*` actions |
| S1 | **Sidebar edge-grab branch steals the workspace close-✕** *(qwen BUGS-2 = user-reported "close button not working on tabs/workspaces", CONFIRMED at scales 1+2)* — grab band `|x−sb_w|≤4·scale` checked first with **no y-guard** (`app.rs:1455`) overlaps the right ~29% of the ✕ zone (`app.rs:1465`): clean click = no-op, jitter = surprise resize. Same band eats pane clicks and leftmost tab-strip clicks (8px full-height dead band). Tab-✕ geometry itself verified correct — perception side: no hover (below) + drag-arm-on-press + last-tab-✕ exits app | **[done 0.21.6]** — press restructured: ✕-check first inside sidebar branch, edge band only below the tab strip (+ same guard in cursor icons); tab-drag motion threshold added; full `chrome_zone_at()` refactor deferred with hover wave |
| S2 | DECCKM application cursor keys ignored | **[open]** (`term.mode().application_cursor` → `ESC O A…`) |
| S2 | Ctrl+digit (2..8) sends nothing | **[open]** — NUL/ESC/FS/GS/RS/US/DEL table |
| S2 | Bold/italic/underline/strikethrough not rendered | **[open]** — second faces + underline quads; synth-bold fallback |
| S2 | New tab/split/workspace loses cwd (`Tab.cwd` dead) | **[open]** — forward OSC 7 from TermListener → per-pane cwd → spawn cwd; also `basename(cwd)` tab labels |
| S2 | Close pane/tab/workspace kills live jobs without confirm | **[open]** — extend window-close banner to live-shell closes; single-pane escape hatch. *(qwen U-4: also covers the ✕ cascade — closing the last tab of the last workspace exits the app instantly, `app.rs:1275-1281`, with no banner)* |
| S2 | No IME handling (`WindowEvent::Ime`) | **[open]** — CJK input blocker |
| S2 | Tab/sidebar overflow past ~6/25 entries unreachable | **[open]** — shrink-to-fit + ellipsis + chevrons/scroll |
| S2 | Palette math: sRGB constants through linear pipeline (washed) | **[open]** — convert constants once or mark targets consistently |
| S2 | PTY write errors ignored → typing into dead pane; no exit-status surface | **[open]** — "process exited (N)" pane state; also Bell → attention + OSC 52 clipboard |
| S2 | No scroll-to-bottom on keypress | **[open]** |
| S2 | **Redraw storm: full redraw per PTY Output event** *(qwen BUGS-3, verified: `seq 1 3000` → 689 presents)* — `user_event` calls `redraw()` directly (`app.rs:1216-1221`); vsync-blocked presents stall input under output floods | **[open]** — dirty flag + `request_redraw()`; draw once per loop pass (damage tracking later) |
| S2 | **No tab-drag threshold: clicks teleport tabs + delete the emptied origin workspace** *(qwen BUGS-4)* — press arms `tab_drag` (`app.rs:1508-1515`); first CursorMoved over sidebar moves the tab and drops the origin ws (`app.rs:1381-1384`); 2px jitter = data loss, no undo | **[done 0.21.6]** — `TAB_DRAG_THRESHOLD` 6 logical px; origin recorded on press; drag arm early-returns before threshold (xvfb-verified: jitter no-ops, real drag moves) |
| S2 | **No hover states / pointer cursor on any chrome** *(user-reported "no ui hover animations on clickable buttons" — CONFIRMED; qwen BUGS-6)* — hover never reaches the renderer (`draw` gets only `active` bools); `update_cursor` emits resize icons only (`app.rs:758-792`) | **[open]** — shared `chrome_zone_at()` → hover highlight + `CursorIcon::Pointer`; animations optional |
| S2 | **No middle-click close of tabs/workspaces** *(user-reported — CONFIRMED, probe: total no-op; qwen BUGS-5)* — non-left buttons only forward to capturing panes (`app.rs:1575-1594`) | **[open]** — Middle-Pressed → chrome hit-test → `close_tab`/`close_workspace_at` |
| S2 | **`close-tab` keybind is a trap** *(qwen BUGS-7; upgrades the old S3 "close-tab no-ops")* — action returns false (`app.rs:960-963`); binding it silently steals the chord's old action AND the fall-through **types the letter into the shell** | **[done 0.21.6]** — closes active tab (kills its panes); refuses on the only tab of the only workspace (`last tab -> ignored`); user keybind verified via `close-tab = [ctrl+alt+t]` |
| S2 | **Windows: broken config = silent launch failure** *(qwen BUGS-8)* — `Config::load()?` in `main.rs:6` errors to a nonexistent stderr (GUI subsystem); dlog sink starts only inside `run()` | **[open]** — init dlog in main; msgbox/log before exit, or boot with defaults + banner |
| S2 | **No word/line selection: double/triple-click inert in panes** *(qwen BUGS-9)* — `double_click` tracks chrome zones only; pane always `SelectionType::Simple` | **[open]** — alacritty `Selection` Semantic/Linear + grid word-boundary walk |
| S3 | rename blur discards buffer; F1–F4 modifiers; kitty keyboard protocol; 750ms wakeup + font rescan on UI thread; `--version`; config docs; `FontCtx` unsafe-transmute hygiene; guest crate in host builds | **[open]** (kitty keyboard protocol: user-requested) |
| S3 | **Stale OSC titles** *(qwen BUGS-10)* — `titles` never pruned on Closed + PaneId reuse mislabels fresh tabs with dead panes' titles (`app.rs:1244-1288`, `layout.rs:563-573`) | **[open]** — `titles.remove(&id)` in the Closed handler |
| S3 | **Second `resumed()` panics** *(qwen BUGS-11)* — duplicate `spawn_pane(PaneId(0))` hits `panic!("duplicate PaneId")` (`app.rs:440-442`) | **[open]** — guard on `window.is_some()` |
| S3 | **Rename editor quirks** *(qwen BUGS-12)* — ArrowDown commits (`app.rs:598-602`); no caret motion/paste/IME; swallows all chords while open | **[open]** |

### User-reported UX list (Nov 2026) — cross-check status (qwen review §4)

| Quote | Verdict | Tracked as |
|-------|---------|-----------|
| "no ui hover animations on clickable buttons" | **CONFIRMED** (no hover plumbing; no Pointer cursor) | S2 hover row above (BUGS-6) |
| "close button not working on tabs/workspaces" | **CONFIRMED (workspace ✕)**: edge-grab branch beats close-✕ hit-test, no y-guard — right ~29% of the ✕ starts a resize (verified scales 1+2). **Tab ✕: PLAUSIBLE** — geometry verified correct; perception = no hover + drag-arm-on-press + last-tab-✕ exits app | S1 edge-grab row (BUGS-2) + BUGS-4/BUGS-6 + confirm row — **fixed 0.21.6** (edge branch reorder; hover still owed) |
| "no button to hide/show the workspaces" | **CONFIRMED** (config `[sidebar].show` only, no action/button) | Identity section (U-1) |
| "no config panel" | **CONFIRMED** (no settings UI; `Config` has no `save()` — nothing writes back today) | Distribution section (U-5) |
| "no plugin enable/disable interface" | **CONFIRMED** (load-all at startup; no manifest/toggles/rescan) | Plugin platform item 8 (U-6) |
| "no middle-click close of tabs/workspaces" | **CONFIRMED** (probe: strip middle-click = total no-op) | S2 middle-click row (BUGS-5) |

## Identity: workspaces as real project containers (review §4b)

- **[done]** Tab drag between workspaces (sidebar as drop target; empty origin ws dies).
- **[open]** Drag panes between tabs.
- **[open]** **Sidebar show/hide affordance** *(user-reported; qwen U-1)*: `toggle-sidebar` keybind action + chevron button in the sidebar header — today only `config.toml [sidebar].show` + hot-reload.
- **[open]** **Tab reordering** *(qwen U-8)*: drag only moves tabs *between* workspaces; strip slot order is immutable (README "re-orderable" oversells).
- **[open]** **Discoverable chrome buttons** *(qwen U-9/U-7/U-10)*: tab-strip trailing `+`, sidebar-header `+ ws` (header double-click secretly creates workspaces today — make it visible), right-click context menus (tab: close/rename/close-others/move-to-ws; entry: rename/close; pane: copy/paste/split/zoom), wheel-over-strip cycles tabs / wheel-over-sidebar cycles workspaces.
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
2. **[open]** Guest error logging/surfacing (`let _ = call_on_layout_change` swallows traps silently — `plugins/lib.rs:96`) + disable affordance.
3. **[open]** `[[plugin]]` capability manifest (`layout.read`, `layout.rename`, …) before tier 2 adds power; unknown capability = load error.
4. **[open]** Change-event diff payload (or guests O(n²)-diff snapshots); rename conflict policy (first-writer-wins?); rename validation (empty/oversized/dup).
5. **[open]** Lifecycle/versioning: host-version advertisement, WIT semver policy, single-source WIT (guest copy will drift).
6. **[open]** Tier 2 shape: `set-tab-label`, `badge(target, text, color)` (pairs with core activity badges), `register-command` → quick switcher, notifications. Tier 3 observability: rate-limited PTY output taps, OSC event stream. Tier 4 control: spawn/move/`send-text` (the `berth` CLI/IPC = cmux `send` equivalent).
7. **[open]** Document the supported std surface for wasm32-wasip2 guests (rustc shim imports WASI at link time) or move tier-2 guests to `wasm32-unknown-unknown` + bindgen-only.
8. **[open]** **Plugin manager UX** *(user-reported "no plugin enable/disable interface" — CONFIRMED; qwen U-6)*: `load_plugins` loads every `*.wasm` at startup with no manifest, no per-plugin toggles, no in-app error visibility, and **no rescan on config hot-reload** (`app.rs:1795-1805`, `plugins/lib.rs:118-135`). Staging: `[[plugin]] name/enabled` config flags (cheap, pairs with item 3's manifest) → list view with load status/errors → hot (re)scan.

## Table stakes still missing (review §4a, priority order)

Mouse reporting **[done]** · bracketed paste/copy-on-select **[done]** ·
**[open]**: bold/italic faces; OSC 7 cwd; scrollback search (`ctrl+shift+f`
overlay, incremental highlight via a second Selection); **word/line selection
(double/triple-click — qwen BUGS-9)**; clickable links (OSC 8 +
regex, `cmd+click`); cursor shapes/blink + `WindowEvent::Focused`; fullscreen +
clear-to-scrollback binds; `[shell]`/`command`+`args`+`env` config (can't start
fish/login shells today), `working-dir`, `scrollback` size, theme/`[palette]`;
IME; scroll-to-bottom; DECCKM + kitty keyboard probe; bell→attention;
exit-status surfacing; wide-glyph advance audit; selection drag auto-scroll
past window edges (qwen U-13); zoomed-pane indicator + window title carrying
tab/workspace context (qwen U-12).

## Distribution / branding (review §4d)

- **[done]** App icon everywhere (window icon, .ico, .icns + Info.plist, ad-hoc codesign).
- **[open]** README screenshot/GIF (sidebar+workspaces demo); Linux `.desktop` + AppStream (AppImage/deb later); winget manifest; `sha256sums.txt`; cargo-binstall metadata; config reference doc (per-OS paths — the #1 new-user question); CHANGELOG 0.15.0 dedupe; `--version`.
- **[open]** **In-app config access** *(user-reported "no config panel"; qwen U-5)*: `Config` has no `save()` — nothing writes back today. Staging: `open-config` action ($EDITOR / reveal path + copy) → settings panel (font/theme/keybinds; needs a config writer + schema). The "(later) settings UI" from the competitive notes, formalized.
- **[open]** **README drift** *(qwen U-14)*: status line says **v0.11.0** while releases ship v0.21.4; the keybinds table documents `ctrl+alt+←/→` (dead — S1 BUGS-1) and "Remaining" duplicates this roadmap. Point README status at ROADMAP.md; fix after BUGS-1 lands.
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
3. **Wire plugins with a safety model, prove with 2 guests** — host + auto-namer **[done]**; limits/capabilities/errors & badge-plugin guest **[open]**.

## qwen review's "next 3 work items" (Nov 2026, in order)

1. **Chrome-input correctness PR** *(S, ~200 lines)* **[done 0.21.6]** — BUGS-1: alt+arrows now `focus-pane-{up,down,left,right}` actions via the table (hardcoded intercept deleted; `ctrl+alt+←/→` workspace cycling verified reachable); BUGS-2: press precedence sidebar(✕ first) → tab strip → edge band (only below strip) → pane, same guard in cursor icons; BUGS-4: 6px tab-drag threshold; BUGS-7: `close-tab` implemented with last-tab guard. Xvfb-verified end-to-end. §7 CI hit-test smokes remain [open] (folded into the hover/affordance PR).
2. **Perceived-perf + affordance PR** *(S/M)* — BUGS-3 (coalesce redraws: dirty flag + `request_redraw`) + BUGS-6 (hover highlight + Pointer cursor via the shared zone fn) + BUGS-5 (middle-click close). Three of the six user reports answered; makes the perf story match the Ghostty competitive note.
3. **Chrome affordances PR** *(S-M)* — U-1 (`toggle-sidebar` action + header chevron) + U-9 (`+` buttons) + U-4 (confirm banner for ✕ cascades incl. last-tab-exits-app) + BUGS-10/11 nits. Then start plugin-platform item 8 staging (`[[plugin]] enabled` flags) alongside safety items 1-2.
