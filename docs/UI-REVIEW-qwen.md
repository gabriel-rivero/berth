# berth — UI/product review (qwen, 2026-11-04)

Scope: v0.21.4 @ `38cbed0`. Method: full read of `crates/{core,emulation,ui,plugins,app}` +
**runtime verification under Xvfb with xdotool-injected input** (`BERTH_DEBUG=1` dlog as
ground truth; scripts + raw logs preserved in §7). Companion to `docs/ROADMAP.md`
(the GLM review baseline); new items below are marked **[new]**, user-reported items
carry the verbatim quote.

Verdict: architecture is sound (clean crate seams, shared hit-test helpers, per-pane
reader/writer threads, per-store plugin sandbox). The defects cluster in **one place**:
the mouse-press dispatch in `app.rs` is an if/else chain ordered by accident rather than
by zone precedence — that single structure produces the user-reported close-button bug,
a full-height dead band beside the sidebar, and accidental tab teleporting. The
keybind path has one analogous ordering bug that kills a documented default feature.

---

## §1 Runtime-verified defects (probed under Xvfb, scale 1 and 2)

### BUGS-1 · S1 · **[new]** Default workspace cycling `ctrl+alt+←/→` is dead
- **Evidence**: probe W2/W3 — `chord: logical=Named(ArrowLeft) ctrl=true alt=true` arrives;
  no `chord matched`, no workspace switch, key silently swallowed.
- **Root cause**: `app.rs:899-916` — hardcoded `alt+arrows → neighbor_pane` intercept runs
  **before** the bindings-table lookup (`app.rs:918-937`) and returns `true` even when no
  neighbor exists. Defaults for `next/prev-workspace` are `ctrl+alt+right/left`
  (`keybinds.rs:95-96`) — both contain alt+arrow ⇒ unreachable. README documents them.
  Side effect: alt+arrow chords are unbindable by users, period.
- **Fix**: table lookup first, hardcoded alt+arrow as fallback; better: promote pane focus
  to real actions (`focus-pane-up/down/left/right`) in `keybinds.rs` and delete the hardcode.
- **Effort**: S.

### BUGS-2 · S1 · **user-reported** "close button not working on tabs/workspaces"
- **Status**: **CONFIRMED for workspace ✕** (root cause below, verified at scale 1 and 2).
  **Tab ✕ hit-test verified correct** (boundary-exact closes at x=627/638 scale 1, x=706
  scale 2) — the tab-side complaint is perception: zero hover/cursor feedback (BUGS-6) +
  a 14-logical-px zone where a 2px miss selects-and-drag-arms the tab (BUGS-4), + closing
  the last tab exits the app with no confirm (§3 U-11).
- **Root cause (workspace ✕)**: press-dispatch branch order in `app.rs:1455` — the sidebar
  **edge-grab** (`|x − sb_w| ≤ 4·scale`, `SIDEBAR_EDGE_GRAB` `app.rs:155`) is checked
  **first and has no y-guard**, while the ✕ zone is `[sb_w − 14·scale, sb_w)`
  (`app.rs:1465`, `UI_CLOSE_W` `render.rs:116`). The right 4 logical px (~29%) of every
  workspace ✕ — including the right half of the drawn glyph (centered at `sb_w − 7·scale`,
  `render.rs:1013-1019`) — starts a **resize drag** instead of closing. A clean click
  there is a silent no-op; any 1px jitter visibly resizes the sidebar.
  - Probe: `(218,76)` → `sidebar grab` (+`sidebar drag -> width 400` on motion) at scale 1;
    `(436,136)` → `sidebar grab` at scale 2. `(210,76)`/`(420,136)` → `sidebar close ws#1` ✓.
- **Same branch, two more steals (verified)**: pane-content clicks within 4 logical px
  right of the sidebar (`(222,300)` → `sidebar grab`, no selection/caret) and leftmost
  tab-strip clicks (`(222,14)` → `sidebar grab`). Net: an **8-px-wide, full-height dead
  band** along the sidebar edge.
- **Fix**: zone-precedence dispatch — compute one `ChromeZone` (see BUGS-6 fix) and match:
  close-✕ > chrome rows > edge-grab; y-guard the grab (skip tab-strip row); shrink to ±2-3px;
  require motion threshold before committing a resize.
- **Effort**: S (M together with the shared-zone refactor).

### BUGS-3 · S2 · **[new]** Redraw storm — no coalescing of Output events
- **Evidence**: probe R0/R1 — `seq 1 3000` produced **689 full redraws** (~170/s) in the
  burst window; each rebuilds every pane's grid walk + vertex buffer + submit + present.
- **Root cause**: `app.rs:1216-1221` — `PaneEvent::Output` calls `self.redraw()` directly
  (per ≤8 KB reader chunk) instead of setting a dirty flag + `request_redraw()`. Under
  vsync, `get_current_texture` blocks the UI thread mid-event-stream ⇒ input latency while
  output floods (builds, `git log`, `cat`). This is the concrete mechanism behind the
  roadmap's "must match performance perception" competitive note.
- **Fix**: Output → `pump()` + dirty + `request_redraw()`; draw once per loop pass in
  `RedrawRequested`. (Damage tracking remains a later optimization.)
- **Effort**: S.

### BUGS-4 · S2 · **[new]** No tab-drag threshold — clicks can teleport tabs and delete workspaces
- **Root cause**: `app.rs:1508-1515` sets `tab_drag` on **press**; the first `CursorMoved`
  over the sidebar (`app.rs:1363-1400`) calls `move_tab_to` **and `remove_workspace` on the
  emptied origin** (`app.rs:1381-1384`). With ≥2 workspaces, a plain click on tab slot 0
  plus a few px of leftward jitter moves the tab to whatever workspace is under the cursor
  and silently destroys the origin workspace. No undo.
- **Fix**: record press point; arm `tab_drag` only after >4-6px motion (also gives free
  click-vs-drag disambiguation for BUGS-2's perception problem).
- **Effort**: S.

### BUGS-5 · S2 · **user-reported** "no middle-click close of tabs/workspaces" — CONFIRMED
- **Evidence**: probe TESTD — middle-click on tab slot: zero log lines, zero effect.
- **Root cause**: the non-left `MouseInput` catch-all (`app.rs:1575-1594`) only forwards
  Middle/Right to mouse-capturing panes (`forward_mouse_button` → `pane_at_mouse()` → None
  over chrome). No chrome handling at all.
- **Fix**: in the Middle-Pressed arm, run the shared chrome hit-test → `close_tab(i)` /
  `close_workspace_at(i)` (same confirm policy as ✕).
- **Effort**: S.

---

## §2 Code-confirmed defects (not separately probed)

### BUGS-6 · S2 · **user-reported** "no ui hover animations on clickable buttons" — CONFIRMED
- Hover state is **not plumbed to the renderer at all**: `Renderer::draw` takes
  `tabs: &[(&str, bool)]` / `workspaces: &[(&str, bool)]` where bool = *active* only
  (`render.rs:1111-1121`; `draw_tab_bar` `render.rs:926-968` and `draw_sidebar`
  `render.rs:971-1021` have two states each). `update_cursor` (`app.rs:758-792`) only ever
  emits `EwResize`/`NsResize`/`Default` — never `Pointer` over tabs, ✕'s, or sidebar entries.
  No press states either.
- **Fix**: one shared `chrome_zone_at(x,y) -> ChromeZone` used by press dispatch, hover
  tracking, and cursor selection (structurally prevents BUGS-2-class drift); pass hovered
  slot into `draw`; brighten bg + ✕ color on hover; `CursorIcon::Pointer`. Animations
  optional — hover highlight + pointer cursor is the actual requirement.
- **Effort**: M.

### BUGS-7 · S2 · **[new]** `close-tab` keybind is a trap: binds do nothing *and* type text
- `run_action(Action::CloseTab)` returns `false` (`app.rs:960-963`). A user binding e.g.
  `"close-tab" = ["cmd+w"]` overwrites the `cmd+w → close-pane` map entry
  (`build_bindings` HashMap insert, `keybinds.rs:206-213`), then the `false` fall-through
  reaches `encode_key` — **the chord types `w` into the shell**. Silent feature removal +
  input corruption.
- **Fix**: implement `CloseTab` = close the tab containing `focused_pane` (find slot index,
  `close_tab(i)` — 10 lines). Roadmap had this as S3 "close-tab no-ops"; upgraded: it is a
  correctness trap, not polish.
- **Effort**: S.

### BUGS-8 · S2 · **[new]** Windows: broken config = silent launch failure
- `main.rs:6` — `Config::load()?` fails fast; the error prints to stderr, but release
  builds are `windows_subsystem = "windows"` (`main.rs:3`) ⇒ no console, and the dlog file
  sink is only initialized inside `berth_ui::run()` (`render.rs:37-71`) which is never
  reached. Double-click → nothing happens, no log anywhere. (Hot-reload correctly keeps the
  previous config on parse error — the asymmetry is startup-only.)
- **Fix**: init the dlog sink in `main` before `Config::load`; on error also show a
  message box (Windows) / eprintln+exit code; or boot with defaults + on-screen banner.
- **Effort**: S.

### BUGS-9 · S2 · **[new]** No word/line selection: double-click and triple-click do nothing in panes
- `double_click()` only tracks `TabBar`/`SidebarEmpty`/`SidebarEntry` zones
  (`app.rs:136-153`); pane presses always `start_selection` with `SelectionType::Simple`
  (`app.rs:1543`, `pane.rs:271-280`). Every reference terminal does semantic double-click
  word select + triple-click line select; alacritty_terminal's `Selection` already supports
  `Semantic`/`Linear`.
- **Effort**: M (needs grid word-boundary walk for Semantic points).

### BUGS-10 · S3 · **[new]** Stale OSC titles: `titles` map never pruned + PaneId reuse mislabels tabs
- `PaneEvent::Closed` removes from `self.panes` but not `self.titles` (`app.rs:1244-1288`).
  `alloc_pane_id` reuses ids of removed maxima (`layout.rs:563-573`) ⇒ close pane 5
  (titled `vim`), open a new tab → it gets PaneId 5 and instantly shows the dead pane's
  title (`redraw()` label lookup `app.rs:357-360`). Also a small unbounded leak.
- **Fix**: `self.titles.remove(&id);` in the Closed handler. Effort: S.

### BUGS-11 · S3 · **[new]** Second `resumed()` = guaranteed panic
- `resumed()` (`app.rs:1189-1209`) creates a window + spawns `PaneId(0)` unconditionally;
  a second resume (possible on macOS reactivation paths) hits `panic!("duplicate PaneId")`
  (`app.rs:440-442`). Guard: `if self.window.is_some() { return; }`. Effort: S.

### BUGS-12 · S3 · **[new]** Rename editor quirks
- `ArrowDown` **commits** the rename (`app.rs:598-602`) — surprising; no caret motion
  (Left/Right dead), no paste, no IME, blur discards silently (roadmap S3 ✓), and the
  editor swallows *all* chords while open (including `cmd+w`). Effort: S-M.

### BUGS-13 · S3 confirmations of existing roadmap items (code located)
- Renderer loss → permanent black window: `app.rs:424-426` (`self.renderer = None`, never
  recreated) — roadmap S1 ✓.
- Atlas exhaustion → full cache wipe + torn frame: `render.rs:1094-1099` — roadmap S1 ✓.
- Confirm-banner keyboard blackout: `app.rs:883-892` swallows every key except Esc for 5s —
  roadmap [deferred] ✓ (note: mouse still fully live during blackout — inconsistent).
- DECCKM ignored: `encode_key` arrows always CSI (`app.rs:1126-1129`) — roadmap S2 ✓.
- Ctrl+digit dead: ctrl path handles only A-Z/@/[/\/]/^/_ (`app.rs:1152-1164`) — roadmap S2 ✓.
- Bold/italic/underline never rendered: single `Weight::NORMAL/Style::Normal` face
  (`render.rs:352-357`); `draw_term` checks only `WIDE_CHAR_SPACER` (`render.rs:1415`) — roadmap S2 ✓.
- No IME: no `WindowEvent::Ime` arm (falls to `_ => {}` `app.rs:1636`) — roadmap S2 ✓.
- Plugin host: no fuel/epoch (`plugins/lib.rs:138-142`), sync on UI thread every 750ms
  (`app.rs:1713-1764`) ⇒ one `loop{}` guest freezes the UI forever; guest traps swallowed
  (`plugins/lib.rs:96`) — roadmap plugin items 1-2 ✓.
- `Tab.cwd` dead / no OSC 7: `Tab::new(.., None)` everywhere (`app.rs:666`), TermListener
  handles only Title/ResetTitle/PtyWrite (`pane.rs:50-67`) — roadmap S2 ✓.
- Nit: `QUAD_LIMIT` comment says "32 MiB", value is 24 MiB (`render.rs:543-544`).

---

## §3 UX / feature findings (ranked)

| # | Sev | Item | Status / notes | Effort |
|---|-----|------|----------------|--------|
| U-1 | S2 | **user-reported** "no button to hide/show the workspaces" | **CONFIRMED**: only `config.toml [sidebar].show` + hot-reload (`config.rs:38-53`); no `Action` variant, no button. Fix: `toggle-sidebar` action + a «/» chevron in the sidebar header strip (currently dead chrome whose double-click secretly creates workspaces, `app.rs:1479`). | S-M |
| U-2 | S2 | **user-reported** middle-click close | = BUGS-5. | S |
| U-3 | S2 | **user-reported** hover states | = BUGS-6. | M |
| U-4 | S2 | Confirm guard on ✕ cascades: closing the last tab of the last workspace **exits the app instantly** (cascade `app.rs:1275-1281`) and workspace ✕ kills every live job in it — the CloseRequested banner (`app.rs:1620-1634`) covers neither. Fold into roadmap's confirm item. | **[new]** detail | M |
| U-5 | S3 | **user-reported** "no config panel" | **CONFIRMED**: no settings UI, and `Config` has no `save()` — nothing can write back today. Staging: (1) `open-config` action (open `config.toml` in $EDITOR / reveal path) — cheap; (2) real panel (font/theme/keybinds) — L. | S / L |
| U-6 | S3 | **user-reported** "no plugin enable/disable interface" | **CONFIRMED**: `load_plugins` loads every `*.wasm` at startup (`plugins/lib.rs:118-135`, `app.rs:1795-1805`); no manifest, no toggles, no error visibility in-app, **no rescan on hot-reload**. Cheap first step: `[[plugin]] name/enabled` config section; full manager UI pairs with roadmap plugin items 2-3. | M |
| U-7 | S3 | Right-click context menus (tab: close/rename/close-others/move-to-ws; entry: rename/close/new-tab; pane: copy/paste/split/zoom) — none exist anywhere; right-click only forwards to capturing TUIs (`app.rs:1575-1594`). | **[new]** | M |
| U-8 | S3 | Tab **reordering** within a workspace — drag only moves between workspaces; strip slot order is immutable. README's "re-orderable" phrasing oversells. | **[new]** | M |
| U-9 | S3 | Discoverable "+" affordances: tab-strip trailing `+`, sidebar-header `+ ws` (double-click-empty-space is undocumented in-app). | **[new]** | S |
| U-10 | S3 | Wheel over tab strip cycles tabs; over sidebar cycles workspaces (currently inert). | **[new]** | S |
| U-11 | S3 | Dim/hollow cursor in unfocused panes — `draw_term` has no focused flag; every pane renders the identical `CURSOR_BG` block (`render.rs:1402-1414`). Roadmap S1-cluster straggler ✓. | confirmed | S |
| U-12 | S3 | No "ZOOMED" indicator when pane zoom is active; window title could carry `title — tab — workspace`. | **[new]** | S |
| U-13 | S3 | Selection drag past window edge doesn't auto-scroll. | **[new]** | M |
| U-14 | S3 | README drift: status line says **v0.11.0** (repo ships v0.21.4); keybinds table documents the broken `ctrl+alt+←/→` (BUGS-1); "Remaining" duplicates the roadmap. Docs-only fix; roadmap already owns tracking. | **[new]** | S |
| U-15 | S3 | Startup config typo = hard fail (see BUGS-8); consider boot-with-defaults + banner, matching hot-reload's keep-previous behavior. | **[new]** | S |

---

## §4 User-reported list — cross-reference verdicts

| Quote | Verdict | Where it lands |
|-------|---------|----------------|
| "no ui hover animations on clickable buttons" | **CONFIRMED** (no hover plumbing; no Pointer cursor) | BUGS-6 / U-3 — `render.rs:1111-1121`, `app.rs:758-792` |
| "close button not working on tabs/workspaces" | **CONFIRMED (workspace ✕)**: edge-grab branch (`app.rs:1455`) checked before close-✕ (`app.rs:1465`), no y-guard → right ~29% of ✕ zone starts resize; verified scales 1+2. **Tab ✕: PLAUSIBLE** — geometry verified correct; failure perception = no hover (BUGS-6) + drag-arm-on-press (BUGS-4) + last-tab-✕ exits app (U-4). Bonus confirmed: 8px dead band steals pane + tab-strip clicks. | BUGS-2 (+BUGS-4/6, U-4) |
| "no button to hide/show the workspaces" | **CONFIRMED** (config-only) | U-1 |
| "no config panel" | **CONFIRMED** (no UI, no `Config::save()`) | U-5 |
| "no plugin enable/disable interface" | **CONFIRMED** (load-all, no toggles/manifest/rescan) | U-6 |
| "no middle-click close of tabs/workspaces" | **CONFIRMED** (probe: strip middle-click = total no-op) | BUGS-5 / U-2 |

---

## §5 Architecture notes (keep / praise)

- Shared hit-test helpers (`tab_index_at`, `workspace_index_at`, `sidebar_width`,
  `Chrome`) already exist — BUGS-2 is an *ordering* defect, not duplication. The
  `ChromeZone` refactor completes the pattern.
- Per-pane reader + writer threads; UI never blocks on PTY I/O (`pane.rs:156-222`).
- `Closed`-event cascade walks all workspaces (`app.rs:1249-1274`) — ✕ on non-focused
  trees works; id-reuse analysis shows no spawn race (tree keeps dying panes until Closed).
- Plugin sandbox per store; snapshot+queued-rename; guest respects user renames
  (`guest/src/lib.rs is_default`); auto-namer pattern is right.
- Config hot-reload keeps previous config on parse error; keybind replace-defaults design;
  release matrix + dlog/BERTH_FRAME_DUMP harness (this review used it as the primary tool —
  it works headless, cross-checkable, and should be promoted into CI as hit-test smokes).

## §6 Recommended next 3 work items (in order)

1. **Chrome-input correctness PR** (S): BUGS-1 (table-first chord lookup) + BUGS-2
   (zone-precedence dispatch, ✕ before grab, y-guarded grab) + BUGS-4 (drag threshold) +
   BUGS-7 (implement close-tab). ~200 lines, kills both S1s and the worst S2 traps. Ship
   the Xvfb+xdotool probes (§7) as a regression script.
2. **Perceived-perf + affordance PR** (S/M): BUGS-3 (coalesce redraws via dirty flag +
   `request_redraw`) + BUGS-6/U-3 (shared `chrome_zone_at` → hover highlight + Pointer
   cursor) + BUGS-5/U-2 (middle-click close). Directly answers 3 of the 6 user reports.
3. **Chrome affordances PR** (S-M): U-1 (`toggle-sidebar` action + header chevron) +
   U-9 (+ buttons) + U-4 (confirm for ✕ cascades incl. last-tab exit) + BUGS-10/11 nits.
   Then start U-6 step 1 (`[[plugin]] enabled` config flags) alongside roadmap plugin
   safety items 1-2.

## §7 Probe appendix (repro)

Harness: `Xvfb :99 800x600`, `XDG_CONFIG_HOME` isolated per probe, `BERTH_DEBUG=1`
(+`WINIT_X11_SCALE_FACTOR=2` for HiDPI), `xdotool windowfocus` / absolute `mousemove` +
`mousedown/mouseup` / `key --window`. (Note: `xdotool click --window` silently failed to
deliver — use absolute coords.) Scripts: `/tmp/opencode/closetest{2..5}.sh`,
`probe6.sh`; logs `/tmp/opencode/berth-cfg{2..6}/berth/berth.log`.

Key raw lines:
```
press (218,76)  -> "sidebar grab" + "sidebar drag -> width 400"   (✕ right part, scale 1)
press (436,136) -> "sidebar grab"                                  (✕ right part, scale 2)
press (210,76)  -> "sidebar close ws#1" -> "workspace WorkspaceId(1) empty -> closing"
press (493,14)  -> "tab close slot1" -> "tab TabId(1) empty -> closing"
press (627,14)/(638,14) -> "tab close slot2"        (close-zone boundaries exact)
press (222,300) -> "sidebar grab"                    (PANE click 2px from sidebar edge!)
press (222,14)  -> "sidebar grab"                    (tab-strip slot0 label click)
middle (250,14) -> (nothing)
key ctrl+alt+Left -> "chord: ... alt=true" then (nothing)   (workspace cycle dead)
seq 1 3000 -> 689 "draw done" lines in burst window         (redraw storm)
```
