# Desk List

**A todo list that lives on your wallpaper.** It sits on the desktop layer, under every window, and ignores the mouse completely — until you hold `Ctrl`.

English · [简体中文](README.zh-CN.md)

<img src="assets/screenshot.png" width="440" alt="Desk List">

## What it is

A small, always-there card of tasks pinned to the desktop. It takes no taskbar slot, never pops up, and never asks to be opened. The list is simply there, next to your icons.

Clicks pass straight through it, so it blocks nothing. Hold `Ctrl` and it wakes up: category tabs, "new item", and the done / archive drawers fade in.

## Features

- **Part of the wallpaper** — bottom of the Z-order, click-through, absent from the taskbar and `Alt+Tab`
- **Hold `Ctrl` to interact** — tabs, add, done and archive drawers appear
- **`Ctrl+Alt+D`** — toggle a pinned card: clickable, always on top, one keystroke away from the desktop again
- **`Ctrl+Alt+N`** — add an item from any app
- **Tray menu** — show, pin / unpin, quit
- **Categories and archive** — checking an item moves it to that category's *Done*; archiving clears them
- **Resizable** — drag the right edge, 320–520 px
- **Local JSON file** — no account, no sync, no telemetry

## Download

Windows 10 / 11 (x64). Grab the [latest release](https://github.com/WoBok/desklist/releases/latest):

- `DeskList-Setup-x.y.z.exe` — installer, per-user, no admin rights
- `DeskList-x.y.z-Portable.exe` — single file, run it from anywhere

## Build from source

Rust 1.77+, Node 18+, and the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/).

```bash
npm install
npx tauri build            # → src-tauri/target/release/desklist.exe
```

The installer is scripted in `installer/setup.nsi`.

## Notes

- Windows first. macOS code paths exist but are untested.
- GPU compositing is disabled in `tauri.conf.json` (`--disable-gpu`). It was the only way to get a transparent window to paint on the development machine — drop those flags if your GPU handles it.
- Idle cost: about 31 MB RAM, well under 0.1% CPU.
