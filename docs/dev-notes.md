# Developer notes (Tauri app)

## Environment variables

| Variable | Effect |
|---|---|
| `BLINKER_CONFIG_DIR` | The app reads and writes `<dir>\blinker_ui_config.json` (and looks for custom icons in `<dir>\icons`) instead of `%APPDATA%\BlinkerUI`. The legacy exe-dir config is not migrated. Use a temp dir for tests and manual runs so the real config is never touched. |
| `BLINKER_READY_LOG` | When the window is shown, the UI appends `ready_ms=<ms> timeline=<json>` to this file: `performance.now()` at show (from navigation start), then the startup steps as `[name, ms since process creation]` pairs in time order (`run` = Rust `run()`, `nav_start` = webview navigation start, `setup` = Tauri setup hook (window and WebView2 created), `page_started`, `js_start`, `dom_content_loaded`, `page_finished`, `get_state`, `icons_fonts`, `rendered`, `show_raf` or `show_timeout`, `shown`). `tools/measure_startup.ps1` sets it and prints the line per run. |
| `BLINKER_SIMULATE_NO_WEBVIEW2` | **Debug builds only.** Treat the WebView2 runtime as missing: the app shows the native "needs the WebView2 Runtime" box (No = exit code 1; Yes opens the download page) instead of starting. |
| `BLINKER_TEST_FORCE_NO_WINDOW` | `cargo test` only: the windowed stand-in Blender reports that it cannot create a window, so the two windowed Supervisor tests take their SKIP path (as on a runner without a desktop). |
| `BLENDER_PATH` | Extra candidate for Blender auto-discovery (as in the Python UI). |
| `BLINKER_UPDATE_API` | **Debug builds only** (ignored by release builds, `#[cfg(debug_assertions)]`). Replaces the release URL `https://api.github.com/repos/Furash/blinker/releases/latest` used by Check for updates. While it is set, release and asset URLs may also be `http://127.0.0.1[:port]/...` (a local test server); otherwise only `https://` URLs are fetched. Without it a debug build answers "Running from source" like the unfrozen Python app. |

## UI without the backend

`app/ui` runs in a plain browser through `mock.js` (three fake folders). Serve the repo
root over HTTP (ES modules do not load from `file://`) and open `/app/ui/index.html`;
options `?theme=tokyo|frigus|light&scene=state|plain|empty`. `/app/ui/selftest.html`
runs the checks for the pure helpers and prints PASS/FAIL.

## Reference screenshots

`python -I tools/grab_reference.py <out_dir>` renders the PySide6 window (never shown on
screen, temp config) for visual comparison.

## Self-update test (Check for updates)

`BlinkerUI.exe --update-selftest [--check-only] [--proceed-unverified]` (debug builds
only) runs the Check for updates flow without a window and prints one JSON line per
step (`check:`, `apply:`, `continue:`/`cancel:`): it answers Yes to "Update available"
(unless `--check-only`) and, with `--proceed-unverified`, Yes to "Could not verify
SHA256 ... Proceed anyway?" (else No, which deletes the zip). When the updater starts the
process exits, and the PowerShell script swaps the exe's folder and relaunches it.

End-to-end recipe (never on the real install or config): copy a debug v0.1.0 build to
`<scratch>\inst\BlinkerUI\BlinkerUI.exe`; build a release zip in the CI layout
(`Compress-Archive -Path <dir>\BlinkerUI` with a v0.2.0 build inside, plus
`<zip>.sha256` = lower-case hex, no newline); serve a fake `releases/latest` JSON
(`tag_name`, `body`, `assets[].name` / `browser_download_url`) and the assets with
`python -I -m http.server <port> --bind 127.0.0.1`; then run the installed exe with
`BLINKER_UPDATE_API=http://127.0.0.1:<port>/<json>`, `BLINKER_CONFIG_DIR=<scratch>\cfg`
and `TEMP`/`TMP=<scratch>\tmp` (the script, the zip, the stage folder and
`blinker_update.log` go to `%TEMP%`). Setting `BLINKER_READY_LOG` as well shows that the
relaunched app opened its window.
