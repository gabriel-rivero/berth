# Changelog

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
