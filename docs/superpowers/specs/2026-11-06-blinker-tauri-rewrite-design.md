# BlinkerUI: rewrite on Tauri 2 + Rust

Date: 2026-11-06. Status: draft for approval.

## Intent and why

BlinkerUI (the desktop manager for the `blinker` Blender hot-reload CLI) takes about 15 seconds
before its whole interface appears. The current app is PySide6 packaged with PyInstaller
(onedir, UPX-compressed Qt/Python DLLs, ~24 MB, ~1.9 MB exe + DLLs). Earlier work already
stripped Qt modules, ssl and QtNetwork, so the remaining cost is structural: Python
interpreter boot, Qt DLL loading, UPX decompression of those DLLs on every launch, and
Windows Defender scanning them.

The owner liked how Tauri + Rust feel (instant start, pleasant to use) and wants BlinkerUI
rewritten on that stack. The user-visible result must be: same app, same look, but it opens
immediately.

## Goals

1. Window visible and interactive fast: target under 1 s from double-click to a usable
   window on the owner's machine (my target, not the owner's number). Measure the current
   build first so the improvement is a real number.
2. Visual and behavioural parity with the Python UI: layout, colours, fonts, spacing,
   dialogs, tooltips, theme presets, every feature below. Source of truth is
   `blinker_ui.py` (`stylesheet()`, `THEME_*` dicts, `FolderRow`, dialogs).
3. Existing user data carries over untouched: `blinker_ui_config.json` keeps its schema and
   location (next to the exe), so current folders, ports, launchers and theme still load.
4. One new feature: an "Open folder" button on each addon row (opens the addon path in
   Explorer).
5. Existing installs can auto-update into the new build (see Updater).

## Non-goals

- No new features beyond "Open folder". No redesign, no cleanup of how the UI works.
- No Linux/macOS support. The current UI is Windows-only (WinHTTP, named mutex, `wt`
  launchers) and stays that way.
- `blinker.py` (CLI) and `bootstrap.py` (runs inside Blender) are not rewritten; they stay Python.
- No Node/npm build step and no JS framework (owner decision pending; my call is plain
  HTML/CSS/JS served as static files by Tauri).

## Anti-goals (a failure even if it technically works)

- Still slow to open, or slow only on first launch after install/reboot.
- Looks "like a web page": text selection on labels, browser context menu, scroll bounce,
  F5 reload, devtools in release, blue focus rings, flash of white before the theme loads.
- Colours or spacing that visibly differ from today; the existing look was iterated on.
- Losing or silently rewriting the user's existing config.
- An updater that strands existing users on the old version.

## Feature inventory to preserve (from `blinker_ui.py`)

- Main window: list of addon folders; each row shows name, status (running/stopped from
  port probe), port (editable), meta text, favourite toggle; selection and hover states.
  Per-row actions: launch, reload, restart, kill, clear, edit, remove, AI launchers
  (icons from `icons/*.svg|png`, alias list from settings), and the new Open folder.
  Output pane showing the selected folder's Blender stdout/stderr.
- Dialogs: scan a directory for addons (depth 3, `bl_info` / `blender_manifest.toml`
  detection, skip-dir and template filters, checklist); edit folder (path, port, repo,
  module, blend file); settings (terminal, AI aliases, Blender path with browse/auto-detect,
  fonts tab, colours tab with presets Frigus Nox / Light / Default / Custom, per-colour
  hex + picker + reset, export/import theme).
- Process management: spawn Blender with the bootstrap script and `BLINKER_*` env vars,
  stream output, detect exit code 75 and relaunch (restart), kill, clear console.
- Background: poll ports 9876-9895 and update statuses; TCP commands to a running
  Blender (`reload`, `restart [save|temp]`, `ping`).
- Singleton: second launch focuses the existing window.
- Updater: GitHub latest release check, download zip, verify SHA-256, spawn detached
  updater that swaps files and restarts.
- Windows details: AUMID/taskbar identity, window/taskbar icon (`logo.ico`/`logo.png`).

## Decisions made (my calls, not the owner's unless noted)

- Stack: Tauri 2, Rust backend, plain HTML/CSS/JS frontend, no bundler. `cargo tauri` is
  not installed; the builder installs it.
- Layout: new Tauri project in `app/` (`app/src-tauri`, `app/ui`). The Python UI stays in
  place until the Rust build reaches parity and the owner has used it, then gets removed in
  a separate change. `blinker.py`, `bootstrap.py`, `icons/`, logos stay at the repo root.
- Single instance: `tauri-plugin-single-instance` replaces the mutex + wake-file.
- HTTP: reqwest or the Windows-native stack in Rust; keep binary small and avoid shipping
  OpenSSL (use schannel/rustls as the builder judges best).
- Binary size: release profile tuned for size (opt-level, lto, strip, panic=abort). No UPX
  (it is part of what makes startup slow).
- Window created hidden and shown after first paint with the theme already applied, to
  avoid a white flash.

## Updater compatibility (important)

Release assets today are `BlinkerUI-vX.Y.Z.zip` (containing a `BlinkerUI/` folder with
`BlinkerUI.exe` plus support files) and `BlinkerUI-vX.Y.Z.zip.sha256`. Old installs
download that zip, check the hash, and run `UPDATER_PS1` to replace the install directory
and relaunch `BlinkerUI.exe`. The new build must keep the same exe name, the same zip
layout and asset names, and a layout the old PowerShell script can install, so existing
users upgrade by clicking Update. `release.yml` must be changed to build with Rust/Tauri
instead of PyInstaller, and version stamping must move from the `__version__` regex to
the Tauri/Cargo version.

## Answers from the code (verified)

- The old `UPDATER_PS1` renames the install dir to `.bak`, moves the staged zip folder in,
  then deletes `.bak`. Config (`APP_DIR/blinker_ui_config.json`) and any custom `icons/`
  live in that dir, so today's updates wipe them. This is an existing bug, not something
  the rewrite introduces. The old script cannot be changed for installs that already exist.
- On window close the Python app calls `terminate()` on each running Blender and waits
  500 ms. The new app must do the same. Python code is authoritative for behaviour, even
  where odd; the builder reads `blinker_ui.py` for details.
- Tags exist up to v0.1.2 (the file still says 0.1.0 because CI stamps it from the tag).

## Calls added after review (mine, not the owner's)

- Config moves to `%APPDATA%\BlinkerUI\blinker_ui_config.json` with a one-time migration
  from the exe-dir file (same schema), so future updates never touch it. Custom icons are
  looked up in `%APPDATA%\BlinkerUI\icons` first, then the exe dir, then bundled.
- Zip layout: `BlinkerUI/BlinkerUI.exe` plus `icons/`, `logo.*`. `bootstrap.py` is embedded
  in the exe and written out at launch. No `_internal` folder.
- First Tauri release is versioned above v0.1.2 (suggest 0.2.0).
- WebView2 Evergreen runtime is required (present on Windows 11). If missing, show a clear
  message with the download link. Startup is measured both cold (right after login/reboot)
  and warm, for old and new builds.
- Visual acceptance: side-by-side by eye on the same config at 100% and 150% DPI. The
  colour picker may use the browser's native one.

- Owner said "ok" without answering two questions, so these defaults stand: the first
  Tauri release is a GitHub pre-release (old installs won't see it until promoted), and a
  bridge Python release that saves config before the swap is an optional last task, needed
  only if other people have installs.

## Left to the builder

- Exact crate choices, module split, event/command API between Rust and the UI.
- How `bootstrap.py` reaches Blender (embedded in the exe and written out, or a bundled
  resource), as long as it works from the install dir and the updater path.
- How to reproduce Qt-specific styling (QSS arrows, scrollbars, splitter) in CSS.
- A parity checklist derived from the code, and how to verify it (screenshot comparison
  of the old and new window with the same config is encouraged).

## Success looks like

Owner double-clicks the new BlinkerUI.exe and sees the familiar window in well under a
second, with their existing folders and theme, can do everything they could before, and
has an Open folder button on each addon.
