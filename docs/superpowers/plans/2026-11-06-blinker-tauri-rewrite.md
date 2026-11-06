# BlinkerUI Tauri Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the PySide6/PyInstaller BlinkerUI with a Tauri 2 + Rust app that looks and behaves the same, opens in well under a second, and adds an "Open folder" button on each addon row.

**Architecture:** A Rust crate (`app/src-tauri`) owns everything that touches the OS: config, theme presets, addon scanning, Blender process supervision, TCP probes, terminal launchers, updater. A static frontend (`app/ui`, plain HTML/CSS/JS, no bundler) renders the window and talks to Rust through Tauri `invoke` commands and events. Python UI (`blinker_ui.py`) stays untouched until parity; it is the behavioural source of truth.

**Tech Stack:** Rust 1.99, Tauri 2 (`tauri-plugin-single-instance`, `tauri-plugin-dialog`), serde/serde_json, sha2, a small HTTP client (builder's choice, no OpenSSL), WebView2 Evergreen, `cargo tauri` (install: `cargo install tauri-cli --version "^2" --locked`).

**Spec:** `docs/superpowers/specs/2026-11-06-blinker-tauri-rewrite-design.md`

## Global Constraints

- Windows only. Exe name `BlinkerUI.exe`; release zip `BlinkerUI-vX.Y.Z.zip` containing one folder `BlinkerUI/` (exe, `icons/`, `logo.*`); asset `BlinkerUI-vX.Y.Z.zip.sha256`; no `_internal`.
- Config file keeps the Python schema: `{folders:[{path,port,repo,module,blend,favourite}], terminal, ai_aliases, blender_path, theme}`. Location becomes `%APPDATA%\BlinkerUI\blinker_ui_config.json`, one-time copy from the exe-dir file if the new one is absent.
- Ports 9876-9895 probed every 2000 ms; port field range 1024-65535; default repo `blinker`.
- Window title `Blinker UI  v{version}`, initial size 1100x820, window created hidden and shown after first paint with the theme applied (no white flash).
- No UPX. Release profile: `opt-level="s"`, `lto=true`, `codegen-units=1`, `strip=true`, `panic="abort"`.
- No Node/npm build step, no JS framework. No devtools, F5, browser context menu, or text selection on labels in release.
- First Tauri release version is above v0.1.2 (use 0.2.0) and is published as a GitHub pre-release.
- Closing the window terminates each running Blender and waits 500 ms.
- `blinker.py` and `bootstrap.py` are not rewritten. `bootstrap.py` is embedded with `include_str!` and written to `%TEMP%\blinker_bootstrap.py` at launch.

## Review Focus

- Config is empty, hand-edited, has a UTF-8 BOM, or has unknown keys: app must start with defaults and must not drop unknown keys on save (Task 2).
- Addon paths with spaces, quotes or Cyrillic characters: launch, terminal `{path}` substitution and Open folder must work (Tasks 5, 7).
- Blender floods output or prints non-UTF-8 bytes: UI stays responsive, history capped at 20000 lines, bad bytes replaced (Task 5).
- Two folders with the same port, or a port edited while Blender runs: second launch is refused with a clear message; no orphaned process (Task 5).
- Window closed while Blender is exiting with code 75: no relaunch after close (Task 5).

---

### Task 1: Baseline measurement and Tauri scaffold

**Files:**
- Create: `tools/measure_startup.ps1`, `app/src-tauri/{Cargo.toml,build.rs,tauri.conf.json,capabilities/default.json,src/main.rs,src/lib.rs}`, `app/ui/index.html`
- Modify: `.gitignore` (add `app/src-tauri/target/`, `app/src-tauri/gen/`)

**Interfaces:**
- Produces: `blinker_lib::run()` in `src/lib.rs` (the app entry); `tools/measure_startup.ps1 -Exe <path> -Runs <n>` printing ms from process start to main window visible, one line per run plus the median.

- [ ] **Step 1: Write `tools/measure_startup.ps1`.** Start the exe, poll `MainWindowHandle -ne 0` and `IsWindowVisible` every 10 ms, print elapsed ms, stop the process. Run 3 times cold-ish and 3 warm.
- [ ] **Step 2: Measure the current build.** Run it against `dist\BlinkerUI\BlinkerUI.exe` (build with `build_exe.cmd` if missing). Record the median in `docs/superpowers/specs/startup-baseline.md`.
- [ ] **Step 3: Install and scaffold.** Run `cargo install tauri-cli --version "^2" --locked`; create the crate by hand (no `cargo tauri init` prompts), with `tauri.conf.json`: `productName` `BlinkerUI`, `frontendDist` `../ui`, window `visible:false`, `width:1100`, `height:820`, icons from `logo.ico`/`logo.png`, `bundle.active:false` (we ship the plain exe, not an installer). Add the release profile from Global Constraints. `ui/index.html` shows the text "blinker" and calls `window.__TAURI__.window.getCurrentWindow().show()` after first `requestAnimationFrame`.
- [ ] **Step 4: Build and run.** Run `cargo tauri build --no-bundle` in `app/`; expected: `app/src-tauri/target/release/BlinkerUI.exe` opens a window. Run `measure_startup.ps1` on it; the median should already be a small fraction of the baseline.
- [ ] **Step 5: Commit** `feat: tauri scaffold and startup measurement`.

### Task 2: Config and paths (`config.rs`, `paths.rs`)

**Files:**
- Create: `app/src-tauri/src/{config.rs,paths.rs}`; tests inline (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `struct Folder { path: String, port: u16, repo: String, module: String, blend: String, favourite: bool }` (serde defaults: port 9876, repo `"blinker"`, favourite false)
  - `struct Config { folders: Vec<Folder>, terminal: String, ai_aliases: String, blender_path: String, theme: serde_json::Map<String, Value>, #[serde(flatten)] extra: Map }`
  - `fn default_terminal() -> String` (`wt -d "{path}" {cmd}` if `wt` is on PATH, else `start "" /D "{path}" cmd /K "{cmd}"`); default `ai_aliases` is `"claude, codex"`
  - `fn load(path: &Path) -> Config` (never fails: missing/empty/invalid -> defaults); `fn save(path: &Path, cfg: &Config) -> io::Result<()>` (pretty JSON, indent 2, atomic via temp file + rename)
  - `fn config_path() -> PathBuf` = `%APPDATA%\BlinkerUI\blinker_ui_config.json`; `fn migrate_legacy_config(new: &Path, legacy: &Path) -> bool` copies once if `new` is absent and `legacy` exists
  - `fn sorted_folders(folders: Vec<Folder>) -> Vec<Folder>` (favourites first, stable)

- [ ] **Step 1: Write failing tests.** Names and assertions: `load_missing_returns_defaults`; `load_garbage_returns_defaults`; `load_strips_bom`; `roundtrip_preserves_unknown_keys` (an `{"future":1}` key survives load+save); `legacy_config_example_loads` (the exact JSON from `blinker_ui_config.json` in the repo yields one folder with port 9876 and a `CLAUDE` launcher key preserved in `extra`); `migrate_copies_once_and_never_overwrites`; `favourites_sort_first_stable`.
- [ ] **Step 2: Run** `cargo test -p blinker config` — expected FAIL (items undefined).
- [ ] **Step 3: Implement** the interfaces above. Note the legacy file has a `launchers` array not read by the Python code; it must survive in `extra`.
- [ ] **Step 4: Run tests** — expected PASS.
- [ ] **Step 5: Commit** `feat: config load/save/migrate`.

### Task 3: Theme presets (`theme.rs`)

**Files:**
- Create: `app/src-tauri/src/theme.rs`

**Interfaces:**
- Produces:
  - `const COLOR_KEYS: [(&str, &str); 18]` — the 18 `(key, label)` pairs from `COLOR_KEYS` in `blinker_ui.py:162-181`, same order
  - `fn preset(name: &str) -> Option<Map>` for `"Tokyo Night (default)"`, `"Frigus Nox"`, `"Light"` (values copied from `blinker_ui.py:84-151`, including `ui_font_family` `Segoe UI` size 10 and `mono_font_family` `Cascadia Mono` size 10)
  - `fn merge_with_default(user: &Map) -> Map`
  - `fn detect_preset(theme: &Map) -> String` (preset name or `"Custom"`, same rule as `SettingsDialog._detect_preset`, `blinker_ui.py:2049`)
  - `fn normalize_hex(s: &str) -> Option<String>` (same rules as `_normalize_hex`, `blinker_ui.py:2075`)

- [ ] **Step 1: Failing tests:** `default_has_all_18_color_keys`; `frigus_nox_window_bg_is_21252b`; `light_accent_is_0969da`; `normalize_hex_accepts_3_and_6_digits_with_or_without_hash` (cases read from the Python implementation); `detect_preset_returns_custom_for_modified_theme`; `merge_fills_missing_keys_from_default`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat: theme presets`.

### Task 4: Addon scanning and Blender discovery (`addons.rs`, `blender.rs::find_blender`)

**Files:**
- Create: `app/src-tauri/src/addons.rs`; `app/src-tauri/src/blender.rs` (discovery part only); dev-dependency `tempfile`

**Interfaces:**
- Produces: `enum AddonKind { Extension, Legacy }` (serialises `"extension"`/`"legacy"`); `fn addon_kind(dir: &Path) -> Option<AddonKind>`; `fn find_addons(root: &Path, max_depth: usize) -> Vec<(PathBuf, AddonKind)>` (default depth 3 at the call site); `fn find_blender(env_path: Option<&str>) -> Option<String>` (order: `BLENDER_PATH`, `PATH`, `Program Files\Blender Foundation\*\blender.exe` newest first, also `Program Files (x86)`).

- [ ] **Step 1: Failing tests with tempdir fixtures:** `manifest_dir_is_extension`; `init_with_bl_info_is_legacy`; `template_manifest_is_rejected` (contains `id = "ADDON_ID"`); `scan_skips_dot_and_skip_dirs` (`node_modules`, `tests`, `docs`, `.git`); `scan_does_not_recurse_into_found_addon`; `scan_respects_max_depth`; `scan_is_sorted_case_insensitively`. Skip list, template patterns and bl_info regex are copied from `blinker_ui.py:526-565`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** (no regex crate needed for the template strings; `bl_info` check is `^\s*bl_info\s*=\s*\{` multiline, use `regex`). **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat: addon scan and blender discovery`.

### Task 5: Blender supervisor (`blender.rs`)

**Files:**
- Modify: `app/src-tauri/src/blender.rs`; Create: `app/src-tauri/src/bootstrap.py` (copy of repo-root `bootstrap.py`, build step keeps them in sync, see Step 5)

**Interfaces:**
- Consumes: `Folder` (Task 2), `find_blender` (Task 4).
- Produces:
  - `struct LaunchSpec { blender: String, bootstrap: PathBuf, folder: Folder, blend: Option<String> }`
  - `trait OutputSink: Send + Sync { fn push(&self, port: u16, text: &str); fn exited(&self, port: u16, code: Option<i32>); }`
  - `struct Supervisor` with `fn new(sink: Arc<dyn OutputSink>) -> Self`, `fn launch(&self, spec: LaunchSpec) -> Result<(), String>`, `fn kill(&self, port: u16) -> bool` (true if a child was killed), `fn is_alive(&self, port: u16) -> bool`, `fn output(&self, port: u16) -> String`, `fn clear(&self, port: u16)`, `fn shutdown(&self)` (terminate all, wait 500 ms each, no restarts afterwards)
  - Env passed to Blender: `BLINKER_ADDON_PATH` (canonicalised, no `\\?\` prefix), `BLINKER_MODULE` (folder.module or dir name), `BLINKER_REPO`, `BLINKER_PORT`, `BLINKER_LEGACY` (`"1"` if no `blender_manifest.toml`, else `""`)
  - The log header lines written on launch match `_launch` (`blinker_ui.py:1377-1382`) exactly.
  - Exit code 75 with no stop requested: read `%TEMP%\blinker_restart_path` if present (use as the new blend, delete it), append `[restarting blender...]`, relaunch with the same env. Any other exit: delete `blinker_restart_path` and `blinker_restart.blend`, mark not alive.
  - Output: stdout and stderr merged, decoded as UTF-8 lossy, history capped at 20000 lines.

- [ ] **Step 1: Failing tests using `cmd.exe` as a stand-in Blender (`blender: "cmd.exe"`, bootstrap arg replaced by a test seam `args_override`):** `output_is_streamed_and_merged` (stdout+stderr both seen); `exit_75_relaunches_once_then_stops` (a script that exits 75 on the first run via a marker file); `exit_75_after_kill_does_not_relaunch`; `refuses_second_launch_on_same_port` (returns Err mentioning the port); `shutdown_prevents_restart`; `history_capped_at_20000_lines`; `invalid_utf8_is_replaced`; `path_with_spaces_and_cyrillic_is_passed_intact`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** with `std::process::Command` and two reader threads (or one merged pipe), `CREATE_NO_WINDOW` for the child's console only if Blender's own stdout still reaches the pipe (verify against the Python behaviour: `QProcess` merged channels). **Step 4: Run** — PASS.
- [ ] **Step 5: Embed bootstrap.** `build.rs` copies `../../bootstrap.py` into `OUT_DIR`; `include_str!` it; `fn write_bootstrap() -> io::Result<PathBuf>` writes it to `%TEMP%\blinker_bootstrap.py` (rewrite only if content differs). Test: `bootstrap_written_matches_embedded`.
- [ ] **Step 6: Commit** `feat: blender supervisor`.

### Task 6: TCP and status polling (`net.rs`)

**Files:**
- Create: `app/src-tauri/src/net.rs`

**Interfaces:**
- Produces: `fn tcp_send(port: u16, msg: &str, timeout: Duration) -> String` (returns the reply or `error: <reason>`); `fn is_running(port: u16) -> bool` (sends `ping`, true iff reply starts with `pong`, 1 s timeout); `fn probe(ports: &[u16]) -> BTreeMap<u16, bool>` (parallel, at most 32 workers); `fn start_poller(app: AppHandle, ports: Arc<Mutex<Vec<u16>>>)` emitting event `blinker://status` `{ "<port>": bool }` every 2000 ms, skipping a tick if the previous probe is still running.

- [ ] **Step 1: Failing tests with a local `TcpListener` fake bootstrap:** `ping_pong_is_running`; `silent_listener_is_not_running`; `closed_port_is_not_running`; `tcp_send_reload_returns_reply`; `tcp_send_failure_formats_error_prefix`; `probe_runs_in_parallel` (20 closed ports finish in < 3 s).
- [ ] **Step 2-4: Run FAIL, implement, run PASS. Step 5: Commit** `feat: tcp probes and status poller`.

### Task 7: Terminal launchers and Open folder (`terminal.rs`)

**Files:**
- Create: `app/src-tauri/src/terminal.rs`

**Interfaces:**
- Produces: `fn expand_template(template: &str, path: &str, cmd: &str) -> String` (replaces `{path}` and `{cmd}`); `fn open_ai(template: &str, path: &str, ai: &str) -> Result<(), TerminalError>` (spawn via `cmd /C` hidden, cwd = path, report failure only if the process exits non-zero within 2 s, like `_run_terminal`, `blinker_ui.py:1504`); `fn open_folder(path: &str) -> Result<(), String>` (errors `Path not found: <path>` if not a directory, else spawns `explorer.exe <path>`; Explorer returns exit code 1 even on success, so do not treat that as failure).

- [ ] **Step 1: Failing tests:** `template_replaces_path_and_cmd`; `template_with_cyrillic_path`; `open_folder_rejects_missing_dir`; `open_folder_rejects_file_path`; `open_ai_reports_nonzero_exit` (template `cmd /C exit 3`).
- [ ] **Step 2-4: FAIL, implement, PASS. Step 5: Commit** `feat: terminal launchers and open folder`.

### Task 8: Tauri commands and state wiring (`commands.rs`, `lib.rs`)

**Files:**
- Create: `app/src-tauri/src/commands.rs`; Modify: `app/src-tauri/src/lib.rs`, `capabilities/default.json`

**Interfaces:**
- Consumes: everything from Tasks 2-7.
- Produces (all `#[tauri::command]`, JSON in/out, errors as `String`):
  - `get_state() -> { config: Config, config_path: String, version: String, running: {port: bool}, default_theme: Map, presets: [name] , color_keys: [[key,label]] }`
  - `save_config(config: Config)`; `scan_addons(root: String) -> [{path, kind}]`
  - `launch(folder: Folder)`, `reload(port)`, `restart(port)`, `kill(port)`, `clear_output(port)`, `get_output(port) -> String` (each of reload/restart/kill returns the line appended to the log, e.g. `[reload] ok (7 modules)`; `kill` falls back to `tcp_send(port,"kill")` when no child is supervised)
  - `open_ai(path: String, ai: String)`, `open_folder(path: String)`, `find_blender() -> Option<String>`
  - `set_ports(ports: Vec<u16>)` (feeds the poller)
  - Events: `blinker://output` `{port, text}`, `blinker://exited` `{port, code}`, `blinker://status`.
- Window `CloseRequested` calls `Supervisor::shutdown`.

- [ ] **Step 1: Failing test** `get_state_with_legacy_config_returns_folder_and_defaults` against the pure function `build_state(cfg, paths)` the command wraps.
- [ ] **Step 2-4: FAIL, implement, PASS. Step 5: Manual check:** `cargo tauri dev`, in the webview console `await __TAURI__.core.invoke('get_state')` returns the migrated config. **Step 6: Commit** `feat: tauri command layer`.

### Task 9: Frontend shell: window, toolbar, rows, output pane, theming

**Files:**
- Create: `app/ui/{index.html,app.css,main.js,api.js,theme.js,rows.js,output.js,mock.js}`, copy `icons/` into `app/ui/icons/` at build time via `build.rs`/tauri `resources`

**Interfaces:**
- Produces: `api.js` exports `invoke`/`listen` wrappers; when `window.__TAURI__` is absent it loads `mock.js` (in-memory state, three fake folders, fake output) so the UI can be opened in a plain browser for screenshots. `theme.js` exports `applyTheme(theme)` which sets CSS custom properties named exactly like the theme keys (`--window_bg`, `--row_selected_bg`, ...) plus font family/size. `rows.js` exports `renderRows(state)`.

Layout, from `FolderRow` and `_build_ui` (`blinker_ui.py:854-1128`): toolbar (`+ Add Folder`, `Settings…`, `Check for updates`, spacer, `Config: <path>` meta label); vertical splitter, list on top (10 px padding, 8 px gap, scrollable) and output below (label `(no folder selected)` or the folder path, monospace pane, read-only, 20000-line cap), stretch 1:2, initial sizes 320/480, draggable. Row: 12/6 px padding, 8 px gap: status dot (`○`/`●`, running colour), path, meta text (`module=…   ·   repo=…   ·   blend=…`), stretch, `port` label, port number input (130 px, right aligned, 1024-65535, tooltip `TCP port the bootstrap server listens on`), then buttons: **Open folder** (new), `Edit`, `✕`, `★/☆` (tooltips `Pin to top`/`Unpin from top`), primary `Launch`/`Kill`, `Reload`, `Restart` (the last two disabled unless running), `Clear console`, divider `│`, one button per AI alias (icon 18 px in a 40 px button if `icons/<alias>.svg|png` exists, else text). Selected row has accent border and `row_selected_bg`; hover `row_hover_bg`. CSS reproduces `stylesheet()` (`blinker_ui.py:678-845`) value by value.

- [ ] **Step 1: Build the shell with `mock.js`; open `app/ui/index.html` through the Browser pane and screenshot** (default theme).
- [ ] **Step 2: Compare to the Python app** on the same three folders at 100% and 150% DPI (the owner supplies a screenshot of the Python window if the agent cannot capture it); fix spacing/colour differences until they are not visible by eye. Switch the theme to Frigus Nox and Light via the mock and compare again.
- [ ] **Step 3: Wire real events:** output appends to the selected row's pane (auto-scroll only if already at bottom), status events update dots and enable/disable Reload/Restart, `Open folder` calls `open_folder` and shows a message dialog on error, `Launch` etc. call the commands, selecting a row loads `get_output(port)`.
- [ ] **Step 4: Disable web-isms:** `user-select:none` on labels (output pane stays selectable), block `contextmenu` outside the output pane and inputs, block F5/Ctrl+R/F12/Ctrl+Shift+I in release builds, hide the window until theme applied then `show()`.
- [ ] **Step 5: Manual run** with `cargo tauri dev` and a real addon folder: Launch, Reload, Restart, Kill, Open folder, AI button.
- [ ] **Step 6: Commit** `feat: main window UI`.

### Task 10: Dialogs: scan, edit folder, add folder

**Files:**
- Create: `app/ui/dialogs/{dialog.js,scan.js,edit.js}`; Modify: `app/src-tauri/Cargo.toml` (add `tauri-plugin-dialog`), `capabilities/default.json`

**Interfaces:**
- Consumes: `scan_addons`, `save_config`, dialog plugin `open({directory:true})`.
- Behaviour copied from `_add_folder` (`blinker_ui.py:1241`), `ScanAddonsDialog` (1678) and `EditFolderDialog` (1758), including `_next_port` (1234: first free port from 9876), duplicate-path handling, `_change_port` (1278), `_toggle_fav` (1291), `_sort_folders` (1310), `_remove_folder` confirmation (1316).

- [ ] **Step 1: Implement** modal infrastructure (`dialog.js`: focus trap, Esc/Enter, themed) and the three dialogs. **Step 2: Manual check** against the Python dialogs: adding a folder that is an addon, a folder containing several addons (scan list with Select all/none, per-item toggle), editing every field, cancel does not mutate state, port clash is refused with a message.
- [ ] **Step 3: Screenshot compare** of each dialog, Python vs new. **Step 4: Commit** `feat: add/scan/edit dialogs`.

### Task 11: Settings dialog: terminal, fonts, colours, theme import/export

**Files:**
- Create: `app/ui/dialogs/{settings.js,colors.js}`

**Interfaces:**
- Consumes: `presets`, `color_keys`, `default_theme` from `get_state`; theme helpers `normalize_hex`/`detect_preset` implemented in JS identically to Task 3 (tests: `app/ui/theme.test.html` is not needed; instead expose them from Rust as `normalize_hex(s)`/`detect_preset(theme)` commands and call those, so there is one implementation).
- Behaviour copied from `SettingsDialog` (`blinker_ui.py:1835-2212`): tabs Terminal (terminal template, AI aliases, Blender path with Browse and Auto-detect), Fonts (UI and mono family/size), Colours (preset dropdown with `Custom`, per-key swatch + hex input + native colour picker + per-key reset, Reset all, Export theme, Import theme), Apply and OK, live preview while open, Cancel reverts.

- [ ] **Step 1: Add the two Rust commands** `normalize_hex`, `detect_preset` (thin wrappers, covered by Task 3 tests). **Step 2: Implement the dialog.** **Step 3: Manual check:** change a colour -> preset flips to Custom -> pick Frigus Nox -> all 18 swatches update; export to file, import it back, identical; invalid hex rejected. **Step 4: Screenshot compare. Step 5: Commit** `feat: settings dialog`.

### Task 12: Updater (`updater.rs`)

**Files:**
- Create: `app/src-tauri/src/updater.rs`

**Interfaces:**
- Produces: `fn parse_semver(s: &str) -> (u32,u32,u32)` (accepts leading `v`, ignores `-pre`/`+build`, as `_parse_semver`, `blinker_ui.py:288`); `fn find_release_assets(release: &Value) -> (Option<Asset>, Option<Asset>)` (zip, sha256 by name suffix `.zip` / `.zip.sha256`); `fn verify_sha256(file: &Path, expected_hex: &str) -> bool`; `async fn check() -> Result<UpdateInfo{version, notes, zip_url, sha_url}|None, String>` against `https://api.github.com/repos/Furash/blinker/releases/latest`; `async fn download_and_apply(info) -> Result<(), String>` (download, verify, write `UPDATER_PS1` to `%TEMP%\blinker_updater.ps1`, spawn it detached with `-WaitPid -Zip -Install -Exe`, then exit the app). `UPDATER_PS1` is the exact text from `blinker_ui.py:201-285`. Commands `check_for_updates` and `apply_update` expose these. UI: Check for updates shows the same dialogs/messages as `_check_for_updates` (`blinker_ui.py:1577-1665`).

- [ ] **Step 1: Failing tests:** `semver_orders_0_1_2_below_0_2_0`; `semver_ignores_prefix_and_suffix`; `finds_zip_and_sha_assets`; `missing_sha_asset_returns_none_for_sha`; `verify_sha256_rejects_tampered_file`; `verify_sha256_accepts_matching_lowercase_and_uppercase`.
- [ ] **Step 2-4: FAIL, implement, PASS.** **Step 5: Manual end-to-end** on a scratch install dir: build zip layout like CI, fake release served from a local HTTP server (API base overridable by env `BLINKER_UPDATE_API`), run the updater; app restarts from the new files and the `%APPDATA%` config is untouched. **Step 6: Commit** `feat: self-update`.

### Task 13: Single instance, Windows identity, close handling

**Files:**
- Modify: `app/src-tauri/src/lib.rs`

**Interfaces:**
- `tauri-plugin-single-instance` callback shows, unminimises and focuses the existing window (replaces mutex + wake file, `blinker_ui.py:2225-2279`); set AUMID as in `_set_windows_aumid` (`blinker_ui.py:2214`) so the taskbar icon matches.

- [ ] **Step 1: Implement.** **Step 2: Manual check:** launch twice, the second exits and the first comes to front, also when minimised; taskbar shows the logo; closing the window kills a running Blender. **Step 3: Commit** `feat: single instance and windows identity`.

### Task 14: Release workflow

**Files:**
- Modify: `.github/workflows/release.yml`, `build_exe.cmd` (rename note) / Create: `app/build.cmd`

**Interfaces:**
- Produces: tagged build `v*` -> `BlinkerUI-vX.Y.Z.zip` (folder `BlinkerUI/` with `BlinkerUI.exe`, `icons/`, `logo.png`, `logo.ico`) and `.zip.sha256`, published as `prerelease: true`.

- [ ] **Step 1:** Replace Python/PyInstaller steps with `dtolnay/rust-toolchain@stable`, `cargo install tauri-cli --version "^2" --locked` (cached), version stamping of `app/src-tauri/tauri.conf.json` and `Cargo.toml` from the tag (fail if pattern not found, like the current step), `cargo tauri build --no-bundle`, package, hash, `softprops/action-gh-release@v2` with `prerelease: true`.
- [ ] **Step 2: Validate** with `workflow_dispatch` on a throwaway tag `v0.2.0-rc1` on this branch; download the zip, check layout, hash and that it runs on a clean folder. Delete the test release afterwards (ask the owner first).
- [ ] **Step 3: Commit** `ci: build release with tauri`.

### Task 15: Parity pass, measurement, docs

**Files:**
- Modify: `README.md`; Create: `docs/superpowers/specs/startup-results.md`

- [ ] **Step 1: Walk the spec's feature inventory** item by item against the running app with the owner's real config; tick each in `startup-results.md` or list the gaps and fix them.
- [ ] **Step 2: Run `measure_startup.ps1`** on the old and new builds (cold after a reboot and warm); record numbers. Target: new under 1 s; if not, profile (WebView2 init vs. our code) before claiming done.
- [ ] **Step 3: Update README** (UI section: install from release zip, config location, build with `cargo tauri build`). **Step 4: Commit** `docs: tauri build and results`.
- [ ] **Step 5 (separate change, only after the owner has used the new build):** remove `blinker_ui.py`, `BlinkerUI.spec`, `build_exe.cmd`, `tools/upx`.

### Task 16 (optional): Bridge release for existing users

Only needed if other people have BlinkerUI installed (open question in the spec's "Decisions"). A Python-UI release (v0.1.3) whose updater first copies `blinker_ui_config.json` and `icons/` to `%APPDATA%\BlinkerUI\` before swapping the folder, tagged and published before the first Tauri release is promoted from pre-release.

- [ ] **Step 1:** Patch `UPDATER_PS1` and `_spawn_updater` in `blinker_ui.py` to copy config/icons before `Try-Rename`. **Step 2:** Test the swap on a scratch install. **Step 3:** Tag `v0.1.3`, then promote the Tauri pre-release once users have updated. **Step 4: Commit** `fix: preserve config across self-update`.
