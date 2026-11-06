# Developer notes (Tauri app)

## Environment variables

| Variable | Effect |
|---|---|
| `BLINKER_CONFIG_DIR` | The app reads and writes `<dir>\blinker_ui_config.json` (and looks for custom icons in `<dir>\icons`) instead of `%APPDATA%\BlinkerUI`. The legacy exe-dir config is not migrated. Use a temp dir for tests and manual runs so the real config is never touched. |
| `BLINKER_READY_LOG` | When the window is shown, the UI appends `ready_ms=<ms>` (`performance.now()` at show, from navigation start) to this file. `tools/measure_startup.ps1` sets it and prints the value per run. |
| `BLENDER_PATH` | Extra candidate for Blender auto-discovery (as in the Python UI). |

## UI without the backend

`app/ui` runs in a plain browser through `mock.js` (three fake folders). Serve the repo
root over HTTP (ES modules do not load from `file://`) and open `/app/ui/index.html`;
options `?theme=tokyo|frigus|light&scene=state|plain|empty`. `/app/ui/selftest.html`
runs the checks for the pure helpers and prints PASS/FAIL.

## Reference screenshots

`python -I tools/grab_reference.py <out_dir>` renders the PySide6 window (never shown on
screen, temp config) for visual comparison.
