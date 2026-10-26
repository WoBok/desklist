# Desk List

A minimal todo list that resides on the Windows desktop. It sits below every other window, ignores mouse input by default, and becomes interactive only while `Ctrl` is held.

English · [简体中文](README.zh-CN.md)

<img src="assets/screenshot.png" width="440" alt="Desk List">

## Overview

Desk List renders a single persistent card of tasks directly on the desktop surface. It occupies no taskbar slot, never opens on demand, and requires no interaction beyond a glance.

Because pointer input passes through by default, it obstructs neither desktop icons nor other applications. Holding `Ctrl` reveals the interface: category tabs, the add action, and the completed and archived drawers.

## Features

- **Desktop layer** — positioned at the bottom of the Z-order; mouse input passes through; excluded from the taskbar and `Alt+Tab`
- **Interactive only while `Ctrl` is held** — tabs, add, completed and archived drawers appear
- **`Ctrl+Alt+D`** — toggles pinned mode: the card becomes clickable and stays above other windows; press again to return it to the desktop
- **`Ctrl+Alt+N`** — creates an item from any application
- **Tray menu** — show, pin, unpin, quit
- **Categories and archiving** — checking an item moves it to that category's completed list; archiving clears it
- **Adjustable width** — drag the right edge, 320–520 px
- **Interface language** — follows the system language (Chinese or English) and can be changed in Settings; the tray menu follows suit
- **Local storage** — a single JSON file; no account, no synchronization, no telemetry

## Download

Windows 10 / 11 (x64). Available from the [latest release](https://github.com/WoBok/desklist/releases/latest):

- `DeskList-Setup-x.y.z.exe` — installer, per-user, no administrator rights required
- `DeskList-x.y.z-Portable.exe` — single executable, no installation required

## Build from source

Requires Rust 1.77+, Node 18+, and the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/).

```bash
npm install
npx tauri build            # → src-tauri/target/release/desklist.exe
```

The installer is generated from `installer/setup.nsi`.

## Notes

- Windows is the primary target. macOS code paths exist but have not been verified.
- GPU compositing is disabled in `tauri.conf.json` (`--disable-gpu`). On the development machine this was required for a transparent window to render at all; remove the flags where hardware acceleration works correctly.
- Idle resource usage: approximately 31 MB of memory and under 0.1% CPU.

## License

MIT — see [LICENSE](LICENSE).
