# Changelog

## [0.21.1](https://github.com/gabriel-rivero/berth/compare/v0.21.0...v0.21.1) (2026-11-04)


### Bug Fixes

* **logging:** fallback log dir + startup path announcement + panic hook ([54a701f](https://github.com/gabriel-rivero/berth/commit/54a701f90e4dbe41292ba9b08f2e345c03ffa6f6))

## [0.21.0](https://github.com/gabriel-rivero/berth/compare/v0.20.0...v0.21.0) (2026-11-04)


### Features

* **ui:** pane zoom (tmux-style toggle) ([cfa93aa](https://github.com/gabriel-rivero/berth/commit/cfa93aa84ccd8c658a7a15bdeb0600a856f36cb9))

## [0.20.0](https://github.com/gabriel-rivero/berth/compare/v0.19.0...v0.20.0) (2026-11-04)


### Features

* **ui:** drag tabs between workspaces via the sidebar ([77eb43a](https://github.com/gabriel-rivero/berth/commit/77eb43a5e70c9af9a0eef548a18762bfb3feaa14))

## [0.19.0](https://github.com/gabriel-rivero/berth/compare/v0.18.2...v0.19.0) (2026-11-04)


### Features

* app icon everywhere ([2ae52ed](https://github.com/gabriel-rivero/berth/commit/2ae52ed4800a6412ed71fb3c783c7f34bd6f5b7c))

## [0.18.2](https://github.com/gabriel-rivero/berth/compare/v0.18.1...v0.18.2) (2026-11-04)


### Bug Fixes

* **ui:** bracketed paste, copy-on-select, non-blocking PTY writes, louder quad budget ([6f7ee83](https://github.com/gabriel-rivero/berth/commit/6f7ee83a1aaf5f2732720c44240976d863e5dabd))

## [0.18.1](https://github.com/gabriel-rivero/berth/compare/v0.18.0...v0.18.1) (2026-11-04)


### Bug Fixes

* **ui:** scale all chrome by display scale factor + focused pane outline ([1fc892f](https://github.com/gabriel-rivero/berth/commit/1fc892faf9c7861c10533c14c2cc1e11fdb742d1))
* **windows:** windows-subsystem release build + file-based debug logging ([66f521c](https://github.com/gabriel-rivero/berth/commit/66f521cf11e8ccbebdead44837cbdc75381f0037))

## [0.18.0](https://github.com/gabriel-rivero/berth/compare/v0.17.1...v0.18.0) (2026-11-04)


### Features

* **terminal:** mouse reporting (SGR 1006 + X10 1000 + alternate scroll) ([c1a19e2](https://github.com/gabriel-rivero/berth/commit/c1a19e2f2582bd9ce478e43b68f404f455d9adc8))

## [0.17.1](https://github.com/gabriel-rivero/berth/compare/v0.17.0...v0.17.1) (2026-11-04)


### Bug Fixes

* **ui:** exit event loop on close, wheel-to-hovered-pane, DPR font swap, flaky roundtrip test ([a5187cb](https://github.com/gabriel-rivero/berth/commit/a5187cbea00d9e15582e002335a9840845d0e347))

## [0.17.0](https://github.com/gabriel-rivero/berth/compare/v0.16.0...v0.17.0) (2026-11-04)


### Features

* **plugins:** live plugin host (tier 1: layout snapshot + renames) ([2e0c618](https://github.com/gabriel-rivero/berth/commit/2e0c61801ee2c412912ad86f3c1f31241d897cb9))

## [0.16.0](https://github.com/gabriel-rivero/berth/compare/v0.15.0...v0.16.0) (2026-11-03)


### Features

* **ui:** quit-confirm guard when closing with live shells ([803aa3b](https://github.com/gabriel-rivero/berth/commit/803aa3b28ed521998e19973412c830fbed0569c2))

## [0.15.0](https://github.com/gabriel-rivero/berth/compare/v0.14.0...v0.15.0) (2026-11-03)


### Features

* **keybinds:** iTerm2-style cmd chords; fix pane colors + close buttons ([43d9198](https://github.com/gabriel-rivero/berth/commit/43d91986413ea3ce51862494be96f5780398302e))
* **keybinds:** user-configurable keybinds in config.toml ([358ba25](https://github.com/gabriel-rivero/berth/commit/358ba25840075b4c7d413a73dfa7abcb0640d71a))
* M1 single-pane terminal — PTY emulation, wgpu glyph renderer, input/selection/scrollback ([e9a5052](https://github.com/gabriel-rivero/berth/commit/e9a505229069c075505cd29bcba001e3e81dba86))
* **panes:** multi-pane splits wired to the layout tree ([513dfb3](https://github.com/gabriel-rivero/berth/commit/513dfb3520294e88535b7ead05ea19a004db6564))
* **ui:** alt+arrow pane focus navigation + fix intercepted chords leaking to PTY ([6102ea8](https://github.com/gabriel-rivero/berth/commit/6102ea86110cfe68bdc904bc7fb9e79b11477595))
* **ui:** ctrl+shift+q closes the active workspace ([4ec1bd7](https://github.com/gabriel-rivero/berth/commit/4ec1bd7e7a67fe59e4694b3d9b7f4d6535d9dacc))
* **ui:** drag splitters to resize panes ([444975c](https://github.com/gabriel-rivero/berth/commit/444975c48b6bc9c2908d8cd7072831b6c06bba1c))
* **ui:** emoji/symbol fallback font chain + user font dir ([8055407](https://github.com/gabriel-rivero/berth/commit/8055407cbcf614c777ea9882497a6607f615f561))
* **ui:** hot-reload config.toml (mtime poll, live font swap) ([9c86e60](https://github.com/gabriel-rivero/berth/commit/9c86e60e0f4f5875e77bc1a07e6c8c4daa4bf54b))
* **ui:** left sidebar with workspace switching ([a3c0e88](https://github.com/gabriel-rivero/berth/commit/a3c0e88e274a16b4ea8b22b451a24cdc0e047495))
* **ui:** OSC window titles ([bf89f37](https://github.com/gabriel-rivero/berth/commit/bf89f37d4563f9a19140ab8b2afba5315d5d433f))
* **ui:** rename workspaces from the sidebar ([65ad147](https://github.com/gabriel-rivero/berth/commit/65ad1474466fb8d7273e253b4ced21291176782f))
* **ui:** sidebar drag-resize + double-click to create tabs/workspaces ([bf09c12](https://github.com/gabriel-rivero/berth/commit/bf09c1228f30ac270d911713553d533e1623f541))
* **ui:** tab management — new tab, close pane, ctrl+tab cycling ([2aae6a5](https://github.com/gabriel-rivero/berth/commit/2aae6a5fedf1ad58352793cc7003e7929ef05eda))
* **ui:** tab/workspace close buttons, resize cursor affordance, macOS font fix ([08fa4f9](https://github.com/gabriel-rivero/berth/commit/08fa4f95d09fe8b540e92261a3caf0d5e752170d))
* **ui:** visible tab strip with click-to-focus ([072f2e2](https://github.com/gabriel-rivero/berth/commit/072f2e271386627690849060526cad0b6c1f83cb))


### Bug Fixes

* build against alacritty_terminal 0.26 / portable-pty 0.9 / wgpu 30; add release-please + release builds + dependabot ([bc174f0](https://github.com/gabriel-rivero/berth/commit/bc174f0bdda6322a0eaaffde017986d800c0f049))
* **ci:** aarch64 musl release via cargo-zigbuild (musl.cc is flaky from runners) ([93801a3](https://github.com/gabriel-rivero/berth/commit/93801a30e974a50f7313d280776c879ec9f4135d))
* **ci:** aarch64-musl cross toolchain + Intel mac runner image ([d3e90c4](https://github.com/gabriel-rivero/berth/commit/d3e90c4ae80aedf6a34cad435e5aba2272daeef6))
* **ci:** release-please for cargo workspace (simple type + generic version updater) ([96fdd4f](https://github.com/gabriel-rivero/berth/commit/96fdd4f2ac4047a5d1c25e2a0e0175bf011c8088))
* **pty:** configurable backspace byte — [terminal] backspace = "del" | "bs" ([033cee5](https://github.com/gabriel-rivero/berth/commit/033cee5fc8f323307994a05f9b8e12e1da25d60d))
* **render:** baseline-relative glyph placement, visible cursor on empty cells, Space key handling ([ae09d83](https://github.com/gabriel-rivero/berth/commit/ae09d83fd4ec6d8768085166ba640a0fc1741dac))
* **render:** respect fontdb face index + add BERTH_DEBUG=1 render diagnostics ([9b0e436](https://github.com/gabriel-rivero/berth/commit/9b0e436c28fc500738002f8d0b7f822c44afaba9))


### Documentation

* README reflects v0.8.0 state, keybind table, remaining roadmap ([0e5891a](https://github.com/gabriel-rivero/berth/commit/0e5891acea8682695aef0fa8f6780b5dd97d375f))

## [0.14.0](https://github.com/gabriel-rivero/berth/compare/v0.13.0...v0.14.0) (2026-11-03)


### Features

* **ui:** rename workspaces from the sidebar ([65ad147](https://github.com/gabriel-rivero/berth/commit/65ad1474466fb8d7273e253b4ced21291176782f))

## [0.13.0](https://github.com/gabriel-rivero/berth/compare/v0.12.0...v0.13.0) (2026-11-03)


### Features

* **keybinds:** iTerm2-style cmd chords; fix pane colors + close buttons ([43d9198](https://github.com/gabriel-rivero/berth/commit/43d91986413ea3ce51862494be96f5780398302e))

## [0.12.0](https://github.com/gabriel-rivero/berth/compare/v0.11.0...v0.12.0) (2026-11-03)


### Features

* **keybinds:** user-configurable keybinds in config.toml ([358ba25](https://github.com/gabriel-rivero/berth/commit/358ba25840075b4c7d413a73dfa7abcb0640d71a))
* **ui:** emoji/symbol fallback font chain + user font dir ([8055407](https://github.com/gabriel-rivero/berth/commit/8055407cbcf614c777ea9882497a6607f615f561))

## [0.11.0](https://github.com/gabriel-rivero/berth/compare/v0.10.0...v0.11.0) (2026-11-03)


### Features

* **ui:** hot-reload config.toml (mtime poll, live font swap) ([9c86e60](https://github.com/gabriel-rivero/berth/commit/9c86e60e0f4f5875e77bc1a07e6c8c4daa4bf54b))

## [0.10.0](https://github.com/gabriel-rivero/berth/compare/v0.9.0...v0.10.0) (2026-11-03)


### Features

* **ui:** tab/workspace close buttons, resize cursor affordance, macOS font fix ([08fa4f9](https://github.com/gabriel-rivero/berth/commit/08fa4f95d09fe8b540e92261a3caf0d5e752170d))

## [0.9.0](https://github.com/gabriel-rivero/berth/compare/v0.8.0...v0.9.0) (2026-11-03)


### Features

* **ui:** sidebar drag-resize + double-click to create tabs/workspaces ([bf09c12](https://github.com/gabriel-rivero/berth/commit/bf09c1228f30ac270d911713553d533e1623f541))


### Documentation

* README reflects v0.8.0 state, keybind table, remaining roadmap ([0e5891a](https://github.com/gabriel-rivero/berth/commit/0e5891acea8682695aef0fa8f6780b5dd97d375f))

## [0.8.0](https://github.com/gabriel-rivero/berth/compare/v0.7.0...v0.8.0) (2026-11-03)


### Features

* **ui:** ctrl+shift+q closes the active workspace ([4ec1bd7](https://github.com/gabriel-rivero/berth/commit/4ec1bd7e7a67fe59e4694b3d9b7f4d6535d9dacc))
* **ui:** drag splitters to resize panes ([444975c](https://github.com/gabriel-rivero/berth/commit/444975c48b6bc9c2908d8cd7072831b6c06bba1c))

## [0.7.0](https://github.com/gabriel-rivero/berth/compare/v0.6.0...v0.7.0) (2026-11-03)


### Features

* **ui:** OSC window titles ([bf89f37](https://github.com/gabriel-rivero/berth/commit/bf89f37d4563f9a19140ab8b2afba5315d5d433f))

## [0.6.0](https://github.com/gabriel-rivero/berth/compare/v0.5.0...v0.6.0) (2026-11-03)


### Features

* **ui:** left sidebar with workspace switching ([a3c0e88](https://github.com/gabriel-rivero/berth/commit/a3c0e88e274a16b4ea8b22b451a24cdc0e047495))

## [0.5.0](https://github.com/gabriel-rivero/berth/compare/v0.4.0...v0.5.0) (2026-11-03)


### Features

* **ui:** alt+arrow pane focus navigation + fix intercepted chords leaking to PTY ([6102ea8](https://github.com/gabriel-rivero/berth/commit/6102ea86110cfe68bdc904bc7fb9e79b11477595))

## [0.4.0](https://github.com/gabriel-rivero/berth/compare/v0.3.0...v0.4.0) (2026-11-03)


### Features

* **ui:** visible tab strip with click-to-focus ([072f2e2](https://github.com/gabriel-rivero/berth/commit/072f2e271386627690849060526cad0b6c1f83cb))

## [0.3.0](https://github.com/gabriel-rivero/berth/compare/v0.2.0...v0.3.0) (2026-11-03)


### Features

* **ui:** tab management — new tab, close pane, ctrl+tab cycling ([2aae6a5](https://github.com/gabriel-rivero/berth/commit/2aae6a5fedf1ad58352793cc7003e7929ef05eda))

## [0.2.0](https://github.com/gabriel-rivero/berth/compare/v0.1.2...v0.2.0) (2026-11-03)


### Features

* **panes:** multi-pane splits wired to the layout tree ([513dfb3](https://github.com/gabriel-rivero/berth/commit/513dfb3520294e88535b7ead05ea19a004db6564))


### Bug Fixes

* **ci:** aarch64 musl release via cargo-zigbuild (musl.cc is flaky from runners) ([93801a3](https://github.com/gabriel-rivero/berth/commit/93801a30e974a50f7313d280776c879ec9f4135d))

## [0.1.2](https://github.com/gabriel-rivero/berth/compare/v0.1.1...v0.1.2) (2026-11-03)


### Bug Fixes

* **render:** baseline-relative glyph placement, visible cursor on empty cells, Space key handling ([ae09d83](https://github.com/gabriel-rivero/berth/commit/ae09d83fd4ec6d8768085166ba640a0fc1741dac))

## [0.1.1](https://github.com/gabriel-rivero/berth/compare/v0.1.0...v0.1.1) (2026-11-03)


### Bug Fixes

* **ci:** aarch64-musl cross toolchain + Intel mac runner image ([d3e90c4](https://github.com/gabriel-rivero/berth/commit/d3e90c4ae80aedf6a34cad435e5aba2272daeef6))
* **render:** respect fontdb face index + add BERTH_DEBUG=1 render diagnostics ([9b0e436](https://github.com/gabriel-rivero/berth/commit/9b0e436c28fc500738002f8d0b7f822c44afaba9))

## 0.1.0 (2026-11-03)


### Features

* M1 single-pane terminal — PTY emulation, wgpu glyph renderer, input/selection/scrollback ([e9a5052](https://github.com/gabriel-rivero/berth/commit/e9a505229069c075505cd29bcba001e3e81dba86))


### Bug Fixes

* build against alacritty_terminal 0.26 / portable-pty 0.9 / wgpu 30; add release-please + release builds + dependabot ([bc174f0](https://github.com/gabriel-rivero/berth/commit/bc174f0bdda6322a0eaaffde017986d800c0f049))
* **ci:** release-please for cargo workspace (simple type + generic version updater) ([96fdd4f](https://github.com/gabriel-rivero/berth/commit/96fdd4f2ac4047a5d1c25e2a0e0175bf011c8088))
