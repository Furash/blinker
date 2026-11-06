# Startup baseline

Measured with `tools/measure_startup.ps1` (ms from process start to a visible main window,
polled every 10 ms) on 2026-11-06, Windows 11, nothing else running.

## Current build (PySide6/PyInstaller, `dist\BlinkerUI\BlinkerUI.exe`)

- Runs 1-3: 2045, 1779, 1891 ms (median 1891)
- Runs 4-6: 1940, 2002, 1810 ms (median 1940)
- Baseline median: about 1.9 s

Caveat: this is "main window visible", which for the Python app precedes the full UI being
populated; the owner reports about 15 s until the interface is complete. The numbers above
therefore understate the old build's time-to-usable.

## Tauri scaffold (empty page, `BlinkerUI.exe` release, 3.2 MB)

- Runs 1-3: 185, 66, 65 ms (median 66)
- Runs 4-6: 157, 46, 48 ms (median 48)
