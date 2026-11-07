//! Tauri command layer: JSON in/out, shared state, events.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::addons::{self, AddonKind};
use crate::blender::{self, LaunchSpec, OutputSink, Supervisor};
use crate::config::{self, Config, Folder};
use crate::{import, net, terminal, theme, updater};

/// Shared state managed by Tauri.
pub struct AppState {
    pub config_path: PathBuf,
    pub config: Mutex<Config>,
    pub supervisor: Arc<Supervisor>,
    pub ports: Arc<Mutex<Vec<u16>>>,
    /// Last applicable TCP probe per port (poller and `probe_now`). Its lock also
    /// serialises `publish_status`, so status events leave in the order they were built.
    pub last_probe: Mutex<ProbeState>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forwards Blender output and exit notifications to the webview.
pub struct TauriSink {
    pub app: AppHandle,
}

impl OutputSink for TauriSink {
    fn push(&self, port: u16, text: &str, seq: u64) {
        let _ = self.app.emit("blinker://output", serde_json::json!({ "port": port, "text": text, "seq": seq }));
    }
    fn exited(&self, port: u16, code: Option<i32>) {
        let _ = self.app.emit("blinker://exited", serde_json::json!({ "port": port, "code": code }));
        // Python `_on_proc_finished`: running = False now, until a later poll says otherwise.
        // The supervisor already marked the port not alive.
        publish_status_with(&self.app, |ps| ps.exited(port));
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// `blinker://status` payload: for every listed port, running = a Blender answers the
/// last probe OR this app supervises a live Blender on it (Python `_apply_statuses`:
/// `statuses.get(port) or proc_alive`). The backend owns alive-ness, so the UI never
/// has to infer it from the order of `launch` replies and `exited` events.
pub fn status_payload(
    ports: &[u16],
    last_probe: &BTreeMap<u16, bool>,
    alive: impl Fn(u16) -> bool,
) -> BTreeMap<String, bool> {
    ports
        .iter()
        .map(|&p| (p.to_string(), last_probe.get(&p).copied().unwrap_or(false) || alive(p)))
        .collect()
}

/// Probe results with per-port epochs, so a probe can never report a state older than
/// a launch or exit that happened while it ran, and overlapping probes cannot store an
/// older result over a newer one.
#[derive(Debug, Default)]
pub struct ProbeState {
    probe: BTreeMap<u16, bool>,
    /// Bumped on every launch and exit of a supervised Blender on the port.
    epoch: BTreeMap<u16, u64>,
    /// Start number of the probe whose result is stored for the port.
    applied: BTreeMap<u16, u64>,
    started: u64,
}

/// Taken when a probe starts; `ProbeState::apply` uses it to drop stale results.
#[derive(Debug, Clone)]
pub struct ProbeTicket {
    seq: u64,
    epochs: BTreeMap<u16, u64>,
}

impl ProbeState {
    pub fn begin(&mut self, ports: &[u16]) -> ProbeTicket {
        self.started += 1;
        let epochs = ports.iter().map(|&p| (p, self.epoch.get(&p).copied().unwrap_or(0))).collect();
        ProbeTicket { seq: self.started, epochs }
    }

    /// Store results whose port saw no launch/exit since `t` began and no newer probe.
    pub fn apply(&mut self, t: &ProbeTicket, results: &BTreeMap<u16, bool>) {
        for (&p, &v) in results {
            let same_epoch = t.epochs.get(&p) == Some(&self.epoch.get(&p).copied().unwrap_or(0));
            let newer = self.applied.get(&p).map_or(true, |&a| t.seq > a);
            if same_epoch && newer {
                self.probe.insert(p, v);
                self.applied.insert(p, t.seq);
            }
        }
    }

    /// A supervised Blender exited: not running until a probe begun later says so.
    pub fn exited(&mut self, port: u16) {
        *self.epoch.entry(port).or_insert(0) += 1;
        self.probe.insert(port, false);
    }

    /// A supervised Blender started (alive covers it); earlier probes are stale.
    pub fn launched(&mut self, port: u16) {
        *self.epoch.entry(port).or_insert(0) += 1;
    }

    #[cfg(test)]
    pub fn get(&self, port: u16) -> bool {
        self.probe.get(&port).copied().unwrap_or(false)
    }
}

/// Update the probe state under its lock, then emit the full status (still under it).
pub fn publish_status_with(app: &AppHandle, update: impl FnOnce(&mut ProbeState)) {
    let Some(st) = app.try_state::<AppState>() else { return };
    let mut ps = lock(&st.last_probe);
    update(&mut ps);
    let ports = lock(&st.ports).clone();
    let payload = status_payload(&ports, &ps.probe, |p| st.supervisor.is_alive(p));
    let _ = app.emit("blinker://status", payload);
}

/// Start a probe (see `ProbeState::begin`).
pub fn probe_begin(app: &AppHandle, ports: &[u16]) -> Option<ProbeTicket> {
    app.try_state::<AppState>().map(|st| lock(&st.last_probe).begin(ports))
}

/// Apply a finished probe and publish.
pub fn probe_finish(app: &AppHandle, t: Option<ProbeTicket>, results: BTreeMap<u16, bool>) {
    if let Some(t) = t {
        publish_status_with(app, |ps| ps.apply(&t, &results));
    }
}

// ---------------------------------------------------------------------------
// get_state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AppStateDto {
    pub config: Config,
    pub config_path: String,
    pub version: String,
    pub running: BTreeMap<String, bool>,
    pub default_theme: Map<String, Value>,
    pub presets: Vec<String>,
    /// The preset themes, in `presets` order (the Settings preset combo).
    pub preset_themes: Vec<Map<String, Value>>,
    pub color_keys: Vec<[String; 2]>,
    /// DEFAULT_TERMINAL / DEFAULT_AI_ALIASES (Settings: Restore Defaults, hint text).
    pub default_terminal: String,
    pub default_ai_aliases: String,
}

/// Pure core of `get_state` (MainWindow.__init__ 1042-1065): favourites first,
/// theme merged over the default preset. Defaults for terminal/ai_aliases are
/// already applied by `config::load`.
pub fn build_state(cfg: &Config, config_path: &Path, running: &BTreeMap<u16, bool>) -> AppStateDto {
    let mut config = cfg.clone();
    config.folders = config::sorted_folders(config.folders);
    config.theme = theme::merge_with_default(&config.theme);
    AppStateDto {
        config,
        config_path: config_path.to_string_lossy().into_owned(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        running: running.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        default_theme: theme::preset(theme::PRESET_NAMES[0]).unwrap_or_default(),
        presets: theme::PRESET_NAMES.iter().map(|s| s.to_string()).collect(),
        preset_themes: theme::PRESET_NAMES.iter().filter_map(|n| theme::preset(n)).collect(),
        color_keys: theme::COLOR_KEYS.iter().map(|(k, l)| [k.to_string(), l.to_string()]).collect(),
        default_terminal: config::default_terminal(),
        default_ai_aliases: config::DEFAULT_AI_ALIASES.to_string(),
    }
}

// ---------------------------------------------------------------------------
// launch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaunchError {
    /// "path_not_found" | "not_addon" | "blender_not_found" | "spawn_failed"
    pub kind: &'static str,
    pub message: String,
}

fn launch_err(kind: &'static str, message: String) -> LaunchError {
    LaunchError { kind, message }
}

/// The first two `_launch` checks (blinker_ui.py:1337-1346), message texts verbatim.
pub fn validate_folder(path: &str) -> Result<(), LaunchError> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return Err(launch_err("path_not_found", format!("Path not found:\n{path}")));
    }
    if !dir.join("blender_manifest.toml").exists() && !dir.join("__init__.py").exists() {
        return Err(launch_err(
            "not_addon",
            format!("Not a Blender addon (no __init__.py or blender_manifest.toml):\n{path}"),
        ));
    }
    Ok(())
}

/// `_resolve_blender` extended with the explicit (user-picked) path: explicit file,
/// then the configured file, then auto-discovery.
pub fn resolve_blender(
    explicit: Option<&str>,
    configured: &str,
    discover: impl FnOnce() -> Option<String>,
) -> Option<String> {
    for cand in [explicit.unwrap_or(""), configured] {
        if !cand.is_empty() && Path::new(cand).is_file() {
            return Some(cand.to_string());
        }
    }
    discover()
}

// ---------------------------------------------------------------------------
// alias icons
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `find_alias_icon` (blinker_ui.py:54-71): first `<alias>.svg` / `.png` found in
/// `dirs` (svg before png within a dir), returned as a `data:` URI.
pub fn find_alias_icon(dirs: &[PathBuf], alias: &str) -> Option<String> {
    let name = alias.trim().to_lowercase();
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    for d in dirs {
        for (ext, mime) in [("svg", "image/svg+xml"), ("png", "image/png")] {
            let p = d.join(format!("{name}.{ext}"));
            if p.is_file() {
                if let Ok(bytes) = std::fs::read(&p) {
                    return Some(format!("data:{mime};base64,{}", base64(&bytes)));
                }
            }
        }
    }
    None
}

fn icon_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // %APPDATA%\BlinkerUI\icons (or BLINKER_CONFIG_DIR\icons): next to the config.
    if let Some(cfg_dir) = crate::paths::config_path().parent() {
        dirs.push(cfg_dir.join("icons"));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("icons"));
    }
    if let Ok(res) = app.path().resource_dir() {
        dirs.push(res.join("icons"));
    }
    dirs
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Returns at once: `running` is empty. Probing here blocked startup for ~1 s (a
/// connect to a closed localhost port waits for the full probe timeout on Windows);
/// the UI calls `probe_now` once its listeners are in place, as Python polls right
/// after the window is built (`QTimer.singleShot(0, self._poll_status)`).
#[tauri::command]
pub async fn get_state(state: State<'_, AppState>) -> Result<AppStateDto, String> {
    let cfg = lock(&state.config).clone();
    Ok(build_state(&cfg, &state.config_path, &BTreeMap::new()))
}

/// Emit the current status without probing (the UI asks after a `launch` reply, so it
/// sees a status built after the launch finished).
#[tauri::command]
pub async fn status_now(app: AppHandle) -> Result<(), String> {
    publish_status_with(&app, |_| {});
    Ok(())
}

/// Probe the given ports now and emit the full status (`publish_status_with`).
#[tauri::command]
pub async fn probe_now(app: AppHandle, ports: Vec<u16>) -> Result<(), String> {
    let ticket = probe_begin(&app, &ports);
    let running = tauri::async_runtime::spawn_blocking(move || net::probe(&ports))
        .await
        .map_err(|e| e.to_string())?;
    probe_finish(&app, ticket, running);
    Ok(())
}

/// Entries the UI never sees (unparsable folders, wrongly typed values) are
/// `serde(skip)` fields: carry them over from the managed config.
pub fn merge_for_save(incoming: Config, current: &Config) -> Config {
    let mut cfg = incoming;
    cfg.unparsed_folders = current.unparsed_folders.clone();
    cfg.invalid = current.invalid.clone();
    cfg
}

/// The single write path for the shared config. The caller holds the config mutex
/// (`slot` is the guarded value), so no two writers can interleave on the `.tmp` file.
fn commit_config(path: &Path, slot: &mut Config, new: Config) -> std::io::Result<()> {
    let r = config::save(path, &new);
    if r.is_ok() {
        *slot = new;
    }
    r
}

#[tauri::command]
pub async fn save_config(state: State<'_, AppState>, config: Config) -> Result<(), String> {
    let mut slot = lock(&state.config);
    let cfg = merge_for_save(config, &slot);
    commit_config(&state.config_path, &mut slot, cfg).map_err(|e| format!("Could not save config: {e}"))
}

#[derive(Debug, Serialize)]
pub struct AddonEntry {
    pub path: String,
    pub kind: AddonKind,
}

#[tauri::command]
pub async fn scan_addons(root: String) -> Result<Vec<AddonEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        addons::find_addons(Path::new(&root), 3)
            .into_iter()
            .map(|(p, kind)| AddonEntry { path: p.to_string_lossy().into_owned(), kind })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn addon_kind_of(path: String) -> Result<Option<AddonKind>, String> {
    Ok(addons::addon_kind(Path::new(&path)))
}

/// Python's "bootstrap.py not found at:\n{path}", plus why the embedded copy could not
/// be written there.
fn bootstrap_error(path: &std::path::Path, e: &std::io::Error) -> String {
    format!("bootstrap.py not found at:\n{}\n\n{e}", path.display())
}

/// Returns `Some(path)` when this call newly persisted a Blender executable to the
/// config (the user-picked `blender` argument). The UI MUST store it in its local
/// config copy, otherwise its next `save_config` overwrites it with the stale value.
#[tauri::command]
pub async fn launch(
    app: AppHandle,
    state: State<'_, AppState>,
    folder: Folder,
    blender: Option<String>,
) -> Result<Option<String>, LaunchError> {
    validate_folder(&folder.path)?;
    let bootstrap = blender::write_bootstrap()
        .map_err(|e| launch_err("spawn_failed", bootstrap_error(&blender::bootstrap_path(), &e)))?;
    let configured = lock(&state.config).blender_path.clone();
    let exe = resolve_blender(blender.as_deref(), &configured, || {
        blender::find_blender(std::env::var("BLENDER_PATH").ok().as_deref())
    })
    .ok_or_else(|| {
        launch_err(
            "blender_not_found",
            "Could not locate Blender automatically.\n\nPick the Blender executable now?".into(),
        )
    })?;
    let port = folder.port;
    let spec = LaunchSpec {
        blender: exe.clone(),
        bootstrap,
        blend: Some(folder.blend.clone()).filter(|b| !b.is_empty()),
        folder,
        args_override: None,
    };
    let sup = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || sup.launch(spec))
        .await
        .map_err(|e| launch_err("spawn_failed", e.to_string()))?
        .map_err(|m| launch_err("spawn_failed", m))?;
    // Kill/Reload/Restart flip on now, as Python sets running right after start().
    publish_status_with(&app, |ps| ps.launched(port));

    // `_prompt_blender_path` persists a picked executable once it is accepted.
    if blender.as_deref() == Some(exe.as_str()) {
        let mut slot = lock(&state.config);
        if slot.blender_path != exe {
            let mut new = slot.clone();
            new.blender_path = exe.clone();
            if let Err(e) = commit_config(&state.config_path, &mut slot, new.clone()) {
                // Keep it for this session and make the failure visible in the console.
                *slot = new;
                drop(slot);
                state.supervisor.append(
                    port,
                    &format!("[config] could not save blender path: {e}
"),
                );
            }
            return Ok(Some(exe));
        }
    }
    Ok(None)
}

async fn tcp_line(state: &State<'_, AppState>, port: u16, cmd: &'static str) -> Result<String, String> {
    let resp = tauri::async_runtime::spawn_blocking(move || net::tcp_send(port, cmd, Duration::from_secs(3)))
        .await
        .map_err(|e| e.to_string())?;
    let line = format!("[{cmd}] {resp}\n");
    state.supervisor.append(port, &line);
    Ok(line)
}

#[tauri::command]
pub async fn reload(state: State<'_, AppState>, port: u16) -> Result<String, String> {
    tcp_line(&state, port, "reload").await
}

#[tauri::command]
pub async fn restart(state: State<'_, AppState>, port: u16) -> Result<String, String> {
    tcp_line(&state, port, "restart").await
}

#[tauri::command]
pub async fn kill(state: State<'_, AppState>, port: u16) -> Result<String, String> {
    if state.supervisor.kill(port) {
        // Supervisor::kill already logged and pushed "[kill] process killed".
        return Ok("[kill] process killed\n".to_string());
    }
    tcp_line(&state, port, "kill").await
}

/// Graceful stop used when a folder is removed (blinker_ui.py:1316-1334).
#[tauri::command]
pub async fn terminate_process(state: State<'_, AppState>, port: u16) -> Result<bool, String> {
    let sup = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || sup.terminate(port))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
/// Returns the console sequence at the clear; output events at or below it are older.
pub async fn clear_output(state: State<'_, AppState>, port: u16) -> Result<u64, String> {
    Ok(state.supervisor.clear(port))
}

#[derive(Debug, Serialize)]
pub struct OutputSnapshot {
    pub text: String,
    /// Console sequence number of `text`; `blinker://output` events with a larger
    /// `seq` are not in it.
    pub seq: u64,
}

#[tauri::command]
pub async fn get_output(state: State<'_, AppState>, port: u16) -> Result<OutputSnapshot, String> {
    let (text, seq) = state.supervisor.output_with_seq(port);
    Ok(OutputSnapshot { text, seq })
}

#[tauri::command]
pub async fn open_ai(app: AppHandle, state: State<'_, AppState>, path: String, ai: String) -> Result<(), String> {
    let template = lock(&state.config).terminal.clone();
    if template.trim().is_empty() {
        return Err("Terminal alias not set (Settings → Terminal).".into());
    }
    std::thread::spawn(move || {
        if let Err(e) = terminal::open_ai(&template, &path, &ai) {
            let _ = app.emit(
                "blinker://terminal-error",
                serde_json::json!({ "ai": e.ai.clone(), "message": e.to_string() }),
            );
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn open_folder(path: String) -> Result<(), String> {
    terminal::open_folder(&path)
}

#[tauri::command]
pub async fn find_blender() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        blender::find_blender(std::env::var("BLENDER_PATH").ok().as_deref())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_ports(state: State<'_, AppState>, ports: Vec<u16>) -> Result<(), String> {
    *lock(&state.ports) = ports;
    Ok(())
}

#[tauri::command]
pub async fn alias_icon(app: AppHandle, alias: String) -> Result<Option<String>, String> {
    let dirs = icon_dirs(&app);
    tauri::async_runtime::spawn_blocking(move || find_alias_icon(&dirs, &alias))
        .await
        .map_err(|e| e.to_string())
}

// ----- Settings dialog: one implementation of the theme helpers (theme.rs) -----

/// `SettingsDialog._normalize_hex`.
#[tauri::command]
pub fn normalize_hex(s: String) -> Option<String> {
    theme::normalize_hex(&s)
}

/// `SettingsDialog._detect_preset`: preset name or "Custom".
#[tauri::command]
pub fn detect_preset(theme: Map<String, Value>) -> String {
    crate::theme::detect_preset(&theme)
}

/// `_export_theme` after the save dialog: write the draft theme as JSON. The error
/// text is what the "Export failed" box shows.
#[tauri::command]
pub async fn export_theme(path: String, theme: Map<String, Value>) -> Result<(), String> {
    crate::theme::write_theme_file(Path::new(&path), &theme)
}

/// `_import_theme` after the open dialog: accepted values, unknown and invalid keys.
#[tauri::command]
pub async fn import_theme(path: String) -> Result<theme::ImportedTheme, String> {
    theme::read_theme_file(Path::new(&path))
}

// ----- Import Config (import.rs) -----

/// What the confirm dialog shows about a candidate `blinker_ui_config.json`.
#[tauri::command]
pub async fn inspect_config_file(path: String) -> Result<import::ConfigFileSummary, String> {
    import::read_config_file(Path::new(&path)).map(|p| p.summary)
}

const FILE_CHANGED: &str = "The file changed since you chose it; select it again.";

/// Import step 1, no locks: read the file again (the earlier inspect is not trusted),
/// refuse when its content is not the one the user confirmed (`fingerprint` from
/// `inspect_config_file`), and build the config to store: the state the UI would get at
/// startup for this file (`build_state`: favourites first, theme merged over the default,
/// defaults applied). The imported file's own skipped data (unknown keys, wrongly typed
/// values, unparsable folders) stays in it, so the next `save_config` (which carries the
/// slot's skipped data over, `merge_for_save`) keeps the imported ones and drops the old.
/// Touches nothing on disk.
pub fn prepare_import(config_path: &Path, src: &Path, fingerprint: &str) -> Result<Config, String> {
    let parsed = import::read_config_file(src)?;
    if parsed.summary.fingerprint != fingerprint {
        return Err(FILE_CHANGED.into());
    }
    Ok(build_state(&parsed.config, config_path, &BTreeMap::new()).config)
}

/// Import step 2, the only part under the config mutex (`slot` is the guarded value):
/// refuse while a supervised Blender is alive (`alive` must be read inside the same
/// critical section, right before this call), copy the previous on-disk config to
/// `<config>.bak`, then write through `commit_config`, the single writer. A launch that
/// starts after the alive check but before the commit is tolerated (the UI refuses to
/// import while one is pending).
pub fn commit_import(config_path: &Path, slot: &mut Config, new: Config, alive: &[u16]) -> Result<Config, String> {
    if !alive.is_empty() {
        return Err("Stop the running Blender instances first, then import again.".into());
    }
    config::backup(config_path).map_err(|e| format!("Could not back up the current config: {e}"))?;
    commit_config(config_path, slot, new.clone()).map_err(|e| format!("Could not save config: {e}"))?;
    Ok(new)
}

/// Both steps for callers that already hold the lock (tests, the debug self-test).
#[cfg(any(test, debug_assertions))]
pub fn apply_import(
    config_path: &Path,
    slot: &mut Config,
    src: &Path,
    fingerprint: &str,
    alive: &[u16],
) -> Result<Config, String> {
    let new = prepare_import(config_path, src, fingerprint)?;
    commit_import(config_path, slot, new, alive)
}

/// Import Config, step 2 of the UI flow: replace the setup with the file's, provided it
/// still has the content `inspect_config_file` fingerprinted. The returned config has the
/// shape of `get_state().config`; the UI swaps it in. Reading and parsing happen before
/// the config mutex is taken; lock order is config -> supervisor.
#[tauri::command]
pub async fn apply_config_file(state: State<'_, AppState>, path: String, fingerprint: String) -> Result<Config, String> {
    let new = prepare_import(&state.config_path, Path::new(&path), &fingerprint)?;
    let mut slot = lock(&state.config);
    let alive = state.supervisor.alive_ports();
    commit_import(&state.config_path, &mut slot, new, &alive)
}

/// `QFontDatabase.families()` for the font combo boxes.
#[tauri::command]
pub async fn list_fonts() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(crate::fonts::families).await.map_err(|e| e.to_string())
}

// ----- Check for updates (updater.rs; `_check_for_updates`) -----

/// The check up to the "Update available" question: which message box to show.
#[tauri::command]
pub async fn check_for_updates() -> Result<updater::UpdateCheck, String> {
    tauri::async_runtime::spawn_blocking(updater::check).await.map_err(|e| e.to_string())
}

/// After Yes: download the zip into `%TEMP%`, verify its SHA-256, start the updater
/// script and quit (`QApplication.quit()`; running Blenders are closed as on window
/// close). It installs the offer the backend kept from `check_for_updates`; the webview
/// passes no URLs or names.
#[tauri::command]
pub async fn apply_update(app: AppHandle) -> Result<updater::ApplyResult, String> {
    let r = tauri::async_runtime::spawn_blocking(updater::apply).await.map_err(|e| e.to_string())?;
    quit_if_started(&app, &r);
    Ok(r)
}

/// "Could not verify SHA256 ... Proceed anyway?" -> Yes.
#[tauri::command]
pub async fn continue_update(app: AppHandle, zip_path: String) -> Result<updater::ApplyResult, String> {
    let r = tauri::async_runtime::spawn_blocking(move || updater::continue_pending(&zip_path))
        .await
        .map_err(|e| e.to_string())?;
    quit_if_started(&app, &r);
    Ok(r)
}

/// "Proceed anyway?" -> No: delete the downloaded zip.
#[tauri::command]
pub async fn cancel_update(zip_path: String) {
    updater::cancel_pending(&zip_path);
}

fn quit_if_started(app: &AppHandle, r: &updater::ApplyResult) {
    if matches!(r, updater::ApplyResult::Started) {
        app.state::<AppState>().supervisor.shutdown();
        app.exit(0);
    }
}

/// Debug build? The UI keeps reload/devtools keys only in debug builds.
#[tauri::command]
pub fn is_dev() -> bool {
    cfg!(debug_assertions)
}

/// The UI calls this once the main window is shown: after the themed first paint, or
/// after showing the "Startup failed" message when startup failed. From then on a second
/// launch may show and raise the window (`bring_to_front`, which also treats an already
/// visible window as ready).
///
/// `ms` is `performance.now()` at show (`window.__blinker_ready_ms`); `marks` are the
/// UI's startup steps as epoch ms. When `BLINKER_READY_LOG` names a file, one line
/// `ready_ms=<ms> timeline=<json>` is appended to it (startup measurement,
/// tools/measure_startup.ps1): the backend's and the UI's steps in ms since the process
/// was created. Otherwise only the ready flag is set.
#[tauri::command]
pub fn report_ready(ms: f64, marks: Option<serde_json::Map<String, serde_json::Value>>) {
    crate::WINDOW_READY.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(p) = std::env::var_os("BLINKER_READY_LOG").filter(|p| !p.is_empty()) {
        use std::io::Write;
        let line = format!("ready_ms={ms:.1} timeline={}", startup_timeline(marks.unwrap_or_default()));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Backend marks + UI marks (epoch ms) as ms since process creation, in time order.
fn startup_timeline(ui: serde_json::Map<String, serde_json::Value>) -> String {
    let base = crate::timeline::process_start_ms().unwrap_or(0.0);
    let mut all: Vec<(String, f64)> = crate::timeline::marks().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    all.extend(ui.into_iter().filter_map(|(k, v)| v.as_f64().map(|v| (k, v))));
    all.sort_by(|a, b| a.1.total_cmp(&b.1));
    let rel: Vec<(String, f64)> = all.into_iter().map(|(k, v)| (k, ((v - base) * 10.0).round() / 10.0)).collect();
    serde_json::to_string(&rel).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bootstrap_error_shows_path_and_cause() {
        let e = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access is denied.");
        let m = bootstrap_error(std::path::Path::new(r"C:\T\blinker_bootstrap.py"), &e);
        assert_eq!(m, "bootstrap.py not found at:\nC:\\T\\blinker_bootstrap.py\n\nAccess is denied.");
    }

    #[test]
    fn startup_timeline_merges_ui_marks_in_time_order_relative_to_process_start() {
        let now = crate::timeline::now_ms();
        let ui = serde_json::json!({ "shown": now + 50.0, "js_start": now - 10.0, "bad": "x" });
        let s = startup_timeline(ui.as_object().unwrap().clone());
        let v: Vec<(String, f64)> = serde_json::from_str(&s).unwrap();
        let names: Vec<&str> = v.iter().map(|(k, _)| k.as_str()).collect();
        let (js, shown) = (names.iter().position(|n| *n == "js_start").unwrap(), names.iter().position(|n| *n == "shown").unwrap());
        assert!(js < shown, "{s}");
        assert!(!names.contains(&"bad"));
        assert!(v.windows(2).all(|w| w[0].1 <= w[1].1), "{s}");
        // relative to this (test) process's creation: positive, below an hour
        assert!(v[shown].1 > 0.0 && v[shown].1 < 3_600_000.0, "{s}");
    }

    /// Content of the repo's real sample `blinker_ui_config.json`.
    const LEGACY_SAMPLE: &str = r#"{
  "folders": [
    { "path": "D:/git/Blender_CCP_Tools", "port": 9876, "repo": "blinker", "module": "", "blend": "" }
  ],
  "launchers": [
    { "name": "CLAUDE", "command": "wt -d {path} -NoExit -Command claude" }
  ]
}"#;

    #[test]
    fn get_state_with_legacy_config_returns_folder_and_defaults() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("blinker_ui_config.json");
        fs::write(&p, LEGACY_SAMPLE).unwrap();
        let cfg = config::load(&p);
        let s = build_state(&cfg, &p, &BTreeMap::from([(9876, true)]));
        assert_eq!(s.config.folders.len(), 1);
        assert_eq!(s.config.folders[0].path, "D:/git/Blender_CCP_Tools");
        assert_eq!(s.config.folders[0].port, 9876);
        assert_eq!(s.config.ai_aliases, "claude, codex");
        assert_eq!(s.config.terminal, config::default_terminal());
        assert_eq!(s.config.blender_path, "");
        assert_eq!(s.config.theme, theme::merge_with_default(&Map::new()));
        assert_eq!(s.default_theme, theme::preset("Tokyo Night (default)").unwrap());
        assert_eq!(s.presets, vec!["Tokyo Night (default)", "Frigus Nox", "Light"]);
        assert_eq!(s.color_keys.len(), 18);
        assert_eq!(s.preset_themes.len(), 3);
        assert_eq!(s.preset_themes[1], theme::preset("Frigus Nox").unwrap());
        assert_eq!(s.default_terminal, config::default_terminal());
        assert_eq!(s.default_ai_aliases, "claude, codex");
        assert_eq!(s.running.get("9876"), Some(&true));
        assert_eq!(s.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(s.config_path, p.to_string_lossy());
        // unknown keys survive the round trip to the UI
        let j = serde_json::to_value(&s).unwrap();
        assert!(j["config"]["launchers"].is_array());
    }

    #[test]
    fn build_state_sorts_favourites_first_and_merges_user_theme() {
        let mut cfg = Config::default();
        cfg.folders = vec![
            Folder { path: "a".into(), ..Folder::default() },
            Folder { path: "b".into(), favourite: true, ..Folder::default() },
        ];
        let key = theme::COLOR_KEYS[0].0;
        cfg.theme.insert(key.into(), Value::String("#000000".into()));
        let s = build_state(&cfg, Path::new("c.json"), &BTreeMap::new());
        assert_eq!(s.config.folders[0].path, "b");
        assert_eq!(s.config.theme.get(key), Some(&Value::String("#000000".into())));
        assert_eq!(s.config.theme.len(), s.default_theme.len());
    }

    #[test]
    fn probe_begun_before_exit_cannot_resurrect_running() {
        let mut ps = ProbeState::default();
        let t0 = ps.begin(&[1]);
        ps.apply(&t0, &BTreeMap::from([(1, true)]));
        assert!(ps.get(1));
        let before = ps.begin(&[1]); // started while Blender still answered
        ps.exited(1);
        assert!(!ps.get(1), "exit clears running at once");
        ps.apply(&before, &BTreeMap::from([(1, true)]));
        assert!(!ps.get(1), "stale probe ignored");
        let after = ps.begin(&[1]); // e.g. an external Blender answers later
        ps.apply(&after, &BTreeMap::from([(1, true)]));
        assert!(ps.get(1), "probe begun after the exit applies");
    }

    #[test]
    fn overlapping_probes_keep_the_newer_result_and_launch_bumps_epoch() {
        let mut ps = ProbeState::default();
        let older = ps.begin(&[1, 2]);
        let newer = ps.begin(&[1, 2]);
        ps.apply(&newer, &BTreeMap::from([(1, false), (2, true)]));
        ps.apply(&older, &BTreeMap::from([(1, true), (2, false)]));
        assert!(!ps.get(1) && ps.get(2), "older overlapping probe must not win");
        let pre_launch = ps.begin(&[2]);
        ps.launched(2);
        ps.apply(&pre_launch, &BTreeMap::from([(2, false)]));
        assert!(ps.get(2), "probe begun before the launch is stale");
        // ports outside the ticket are ignored
        let t = ps.begin(&[1]);
        ps.apply(&t, &BTreeMap::from([(3, true)]));
        assert!(!ps.get(3));
    }

    #[test]
    fn status_payload_is_probe_or_alive_and_missing_probe_is_false() {
        let probe = BTreeMap::from([(1u16, true), (2, false), (9, true)]);
        let s = status_payload(&[1, 2, 3, 4], &probe, |p| p == 2 || p == 4);
        let want: BTreeMap<String, bool> =
            [("1", true), ("2", true), ("3", false), ("4", true)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
        assert_eq!(s, want); // port 9 is not listed any more: left out
    }

    #[test]
    fn validate_path_not_found() {
        let e = validate_folder("Z:/definitely/not/here").unwrap_err();
        assert_eq!(e.kind, "path_not_found");
        assert_eq!(e.message, "Path not found:\nZ:/definitely/not/here");
    }

    #[test]
    fn validate_not_an_addon() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().to_string_lossy().into_owned();
        let e = validate_folder(&p).unwrap_err();
        assert_eq!(e.kind, "not_addon");
        assert_eq!(
            e.message,
            format!("Not a Blender addon (no __init__.py or blender_manifest.toml):\n{p}")
        );
    }

    #[test]
    fn validate_checks_path_before_addon_and_accepts_either_marker() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("__init__.py"), "").unwrap();
        assert!(validate_folder(&t.path().to_string_lossy()).is_ok());
        let u = tempfile::tempdir().unwrap();
        fs::write(u.path().join("blender_manifest.toml"), "").unwrap();
        assert!(validate_folder(&u.path().to_string_lossy()).is_ok());
        // a file is not a directory
        let f = t.path().join("__init__.py");
        assert_eq!(validate_folder(&f.to_string_lossy()).unwrap_err().kind, "path_not_found");
    }

    fn touch(dir: &Path, name: &str) -> String {
        let p = dir.join(name);
        fs::write(&p, "x").unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn resolve_blender_order_explicit_then_config_then_discovery() {
        let t = tempfile::tempdir().unwrap();
        let a = touch(t.path(), "a.exe");
        let b = touch(t.path(), "b.exe");
        let find = || Some("found".to_string());
        assert_eq!(resolve_blender(Some(&a), &b, find), Some(a.clone()));
        assert_eq!(resolve_blender(None, &b, find), Some(b.clone()));
        assert_eq!(resolve_blender(Some("missing.exe"), &b, find), Some(b.clone()));
        assert_eq!(resolve_blender(None, "missing.exe", find), Some("found".into()));
        assert_eq!(resolve_blender(None, "", || None), None);
    }

    #[test]
    fn resolve_blender_does_not_discover_when_config_valid() {
        let t = tempfile::tempdir().unwrap();
        let b = touch(t.path(), "b.exe");
        assert_eq!(resolve_blender(None, &b, || panic!("must not discover")), Some(b));
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn alias_icon_prefers_svg_and_sets_mime() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("claude.png"), b"png").unwrap();
        fs::write(t.path().join("claude.svg"), b"<svg/>").unwrap();
        let dirs = [t.path().to_path_buf()];
        assert_eq!(
            find_alias_icon(&dirs, "claude").unwrap(),
            format!("data:image/svg+xml;base64,{}", base64(b"<svg/>"))
        );
        fs::remove_file(t.path().join("claude.svg")).unwrap();
        assert_eq!(
            find_alias_icon(&dirs, "claude").unwrap(),
            format!("data:image/png;base64,{}", base64(b"png"))
        );
    }

    #[test]
    fn alias_icon_earlier_dir_wins_even_with_png() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("x.png"), b"A").unwrap();
        fs::write(b.path().join("x.svg"), b"B").unwrap();
        let dirs = [a.path().to_path_buf(), b.path().to_path_buf()];
        assert!(find_alias_icon(&dirs, "x").unwrap().starts_with("data:image/png;base64,"));
    }

    #[test]
    fn alias_icon_unknown_empty_and_missing_dirs_give_none() {
        let t = tempfile::tempdir().unwrap();
        let dirs = [t.path().join("no-such-dir"), t.path().to_path_buf()];
        assert_eq!(find_alias_icon(&dirs, "nope"), None);
        assert_eq!(find_alias_icon(&dirs, "   "), None);
        assert_eq!(find_alias_icon(&dirs, "../claude"), None);
    }

    #[test]
    fn alias_icon_trims_and_lowercases() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("codex.svg"), b"<svg/>").unwrap();
        let dirs = [t.path().to_path_buf()];
        assert!(find_alias_icon(&dirs, "  CoDeX ").is_some());
    }

    #[test]
    fn merge_for_save_preserves_unparsed_invalid_and_unknown_through_ui_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("in.json");
        fs::write(
            &src,
            r#"{"folders":[{"path":"a","port":9876},{"path":"bad","port":"x"}],
                "terminal":5,"mystery":{"k":1}}"#,
        )
        .unwrap();
        let loaded = config::load(&src);
        assert_eq!(loaded.unparsed_folders.len(), 1);
        // as get_state hands it to the UI and the UI returns it
        let dto = build_state(&loaded, &src, &BTreeMap::new());
        let json = serde_json::to_string(&dto.config).unwrap();
        let incoming: Config = serde_json::from_str(&json).unwrap();
        assert!(incoming.unparsed_folders.is_empty() && incoming.invalid.is_empty());
        let merged = merge_for_save(incoming, &loaded);
        let out = t.path().join("out.json");
        config::save(&out, &merged).unwrap();
        let back = config::load(&out);
        assert_eq!(back.unparsed_folders.len(), 1);
        assert_eq!(back.unparsed_folders[0]["path"], "bad");
        assert_eq!(back.invalid.get("terminal"), Some(&serde_json::json!(5)));
        assert_eq!(back.extra.get("mystery"), Some(&serde_json::json!({"k":1})));
        assert_eq!(back.folders.len(), 1);
    }

    #[test]
    fn merge_for_save_edited_folders_replace_but_unparsed_kept_once() {
        let mut current = Config::default();
        current.folders = vec![Folder { path: "old".into(), ..Folder::default() }];
        current.unparsed_folders = vec![serde_json::json!({"path": 1})];
        let mut incoming = Config::default();
        incoming.folders = vec![
            Folder { path: "n1".into(), ..Folder::default() },
            Folder { path: "n2".into(), ..Folder::default() },
        ];
        let m = merge_for_save(incoming, &current);
        assert_eq!(m.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["n1", "n2"]);
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("c.json");
        config::save(&p, &m).unwrap();
        let back = config::load(&p);
        assert_eq!(back.folders.len(), 2);
        assert_eq!(back.unparsed_folders.len(), 1);
    }

    #[test]
    fn theme_commands_wrap_theme_rs() {
        assert_eq!(normalize_hex(" #ABC".into()).as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex("nope".into()), None);
        assert_eq!(detect_preset(theme::preset("Light").unwrap()), "Light");
        let mut t = theme::preset("Light").unwrap();
        t.insert("accent_fg".into(), Value::from("#000000"));
        assert_eq!(detect_preset(t), "Custom");
        // the command's JSON shape (what the dialog reads)
        let r = theme::parse_theme_import(r#"{"x": 1, "ui_font_size": "y"}"#).unwrap();
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({"theme": {}, "unknown": ["x"], "invalid": ["ui_font_size"]})
        );
    }

    #[test]
    fn export_and_import_commands_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("t.json").to_string_lossy().into_owned();
        let theme = theme::preset("Frigus Nox").unwrap();
        tauri::async_runtime::block_on(export_theme(p.clone(), theme.clone())).unwrap();
        let back = tauri::async_runtime::block_on(import_theme(p)).unwrap();
        assert_eq!(back.theme, theme);
        let bad = t.path().to_string_lossy().into_owned(); // a directory
        assert!(tauri::async_runtime::block_on(export_theme(bad.clone(), theme)).is_err());
        assert!(tauri::async_runtime::block_on(import_theme(bad)).is_err());
    }

    #[test]
    fn commit_config_replaces_slot_only_on_success() {
        let t = tempfile::tempdir().unwrap();
        let mut slot = Config::default();
        let mut new = Config::default();
        new.blender_path = "b".into();
        commit_config(&t.path().join("c.json"), &mut slot, new.clone()).unwrap();
        assert_eq!(slot.blender_path, "b");
        // a directory in place of the file makes the rename fail
        let bad = t.path().join("dir");
        fs::create_dir(&bad).unwrap();
        let mut other = Config::default();
        other.blender_path = "z".into();
        assert!(commit_config(&bad, &mut slot, other).is_err());
        assert_eq!(slot.blender_path, "b");
    }

    // ----- Import Config -----

    fn write(p: &Path, s: &str) -> PathBuf {
        fs::write(p, s).unwrap();
        p.to_path_buf()
    }

    /// What the UI does with a config from `apply_config_file` / `get_state`: it comes
    /// back through JSON (skipped fields are `serde(skip)`) and `save_config` merges it.
    fn ui_save(path: &Path, slot: &mut Config, ui: &Config) {
        let incoming: Config = serde_json::from_str(&serde_json::to_string(ui).unwrap()).unwrap();
        let merged = merge_for_save(incoming, slot);
        commit_config(path, slot, merged).unwrap();
    }

    fn json_of(p: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }

    /// `apply_import` with the fingerprint `inspect_config_file` would give right now.
    fn apply(s: &Setup, slot: &mut Config, alive: &[u16]) -> Result<Config, String> {
        let fp = import::read_config_file(&s.src).map(|p| p.summary.fingerprint).unwrap_or_default();
        apply_import(&s.cfg, slot, &s.src, &fp, alive)
    }

    struct Setup {
        _t: tempfile::TempDir,
        cfg: PathBuf,
        src: PathBuf,
    }

    fn setup(current: Option<&str>, imported: &str) -> (Setup, Config) {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("blinker_ui_config.json");
        let src = write(&t.path().join("other.json"), imported);
        if let Some(c) = current {
            write(&cfg, c);
        }
        let slot = config::load(&cfg);
        (Setup { _t: t, cfg, src }, slot)
    }

    #[test]
    fn apply_import_replaces_everything_and_returns_the_get_state_shape() {
        let (s, mut slot) = setup(
            Some(r#"{"folders":[{"path":"old","port":9000}],"terminal":"old term","blender_path":"B","launchers":[1]}"#),
            crate::import::tests::OLD_PYTHON_CONFIG,
        );
        let got = apply(&s, &mut slot, &[]).unwrap();
        // same as get_state would hand the UI for the imported file
        let expect = build_state(&config::load(&s.src), &s.cfg, &BTreeMap::new()).config;
        assert_eq!(got, expect);
        assert_eq!(got.folders.len(), 7);
        assert_eq!(got.folders[0].path, r"D:\git\Blender_CCP_Tools");
        assert_eq!(got.theme, theme::merge_with_default(&config::load(&s.src).theme));
        assert_eq!(got.blender_path, "", "the old blender_path is gone");
        assert!(got.extra.is_empty(), "the old unknown keys are gone");
        assert_eq!(slot, got);
        // disk == imported content (modulo defaults: theme merged over the preset)
        let disk = config::load(&s.cfg);
        assert_eq!(disk, got);
        let ports: Vec<u16> = disk.folders.iter().map(|f| f.port).collect();
        assert_eq!(ports, [9901, 9900, 9876, 9910, 9907, 9910, 9877], "duplicate ports kept");
        assert!(!s.cfg.with_file_name("blinker_ui_config.json.tmp").exists());
    }

    #[test]
    fn apply_import_sorts_favourites_first_and_defaults_missing_fields() {
        let (s, mut slot) = setup(None, r#"{"folders":[{"path":"a"},{"path":"b","favourite":true}]}"#);
        let got = apply(&s, &mut slot, &[]).unwrap();
        assert_eq!(got.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(got.terminal, config::default_terminal());
        assert_eq!(got.ai_aliases, "claude, codex");
        assert_eq!(got.theme, theme::merge_with_default(&Map::new()));
        assert!(s.cfg.is_file(), "created although there was no config");
    }

    #[test]
    fn apply_import_keeps_the_imported_skipped_data_through_the_next_ui_save() {
        let (s, mut slot) = setup(
            Some(r#"{"folders":[{"path":"old"},{"path":"oldbad","port":"x"}],"terminal":5,"oldkey":1}"#),
            r#"{"folders":[{"path":"n1"},{"path":"badport","port":99999},"junk"],"ai_aliases":9,"newkey":{"k":2}}"#,
        );
        assert_eq!(slot.unparsed_folders.len(), 1);
        let got = apply(&s, &mut slot, &[]).unwrap();
        // the UI edits something and saves
        let mut ui = got.clone();
        ui.blender_path = "C:/blender.exe".into();
        ui_save(&s.cfg, &mut slot, &ui);
        let v = json_of(&s.cfg);
        let folders = v["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 3, "n1 + the two unparsable imported entries");
        assert!(folders.iter().any(|f| f["port"] == 99999));
        assert!(folders.iter().any(|f| f == "junk"));
        assert!(!folders.iter().any(|f| f["path"] == "oldbad"), "old unparsable entry not resurrected");
        assert_eq!(v["ai_aliases"], 9, "imported wrongly typed value kept");
        assert_eq!(v["newkey"], serde_json::json!({"k": 2}));
        assert!(v.get("oldkey").is_none(), "old unknown key dropped");
        assert_ne!(v["terminal"], 5, "old wrongly typed value not resurrected");
        assert_eq!(v["blender_path"], "C:/blender.exe");
    }

    #[test]
    fn apply_import_previous_skipped_fields_do_not_leak_when_the_file_has_none() {
        let (s, mut slot) = setup(
            Some(r#"{"folders":[{"path":"oldbad","port":"x"}],"folders_extra":1,"theme":[],"oldkey":true}"#),
            r#"{"folders":[{"path":"n"}]}"#,
        );
        let got = apply(&s, &mut slot, &[]).unwrap();
        assert!(slot.unparsed_folders.is_empty() && slot.invalid.is_empty());
        ui_save(&s.cfg, &mut slot, &got);
        let v = json_of(&s.cfg);
        assert_eq!(v["folders"].as_array().unwrap().len(), 1);
        assert!(v.get("oldkey").is_none() && v.get("folders_extra").is_none());
        assert!(v["theme"].is_object(), "the old wrongly typed theme is not written back");
    }

    #[test]
    fn apply_import_writes_and_replaces_a_single_bak_of_the_previous_config() {
        let first = r#"{"folders":[{"path":"first"}]}"#;
        let (s, mut slot) = setup(Some(first), r#"{"folders":[{"path":"second"}]}"#);
        let bak = config::backup_path(&s.cfg);
        assert!(bak.to_string_lossy().ends_with("blinker_ui_config.json.bak"));
        apply(&s, &mut slot, &[]).unwrap();
        assert_eq!(fs::read_to_string(&bak).unwrap(), first, "byte-exact copy of the previous file");
        let second_on_disk = fs::read_to_string(&s.cfg).unwrap();
        write(&s.src, r#"{"folders":[{"path":"third"}]}"#);
        apply(&s, &mut slot, &[]).unwrap();
        assert_eq!(fs::read_to_string(&bak).unwrap(), second_on_disk, "overwritten, not accumulated");
        let names: Vec<_> = fs::read_dir(s.cfg.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 3, "config, .bak and the source file only: {names:?}");
    }

    #[test]
    fn apply_import_without_a_previous_file_writes_no_bak() {
        let (s, mut slot) = setup(None, r#"{"folders":[]}"#);
        apply(&s, &mut slot, &[]).unwrap();
        assert!(!config::backup_path(&s.cfg).exists());
    }

    #[test]
    fn apply_import_refuses_while_a_supervised_blender_is_alive() {
        struct Quiet;
        impl OutputSink for Quiet {
            fn push(&self, _: u16, _: &str, _: u64) {}
            fn exited(&self, _: u16, _: Option<i32>) {}
        }
        let t = tempfile::tempdir().unwrap();
        let tmp = t.path().join("temp");
        fs::create_dir_all(&tmp).unwrap();
        let addon = t.path().join("addon");
        fs::create_dir_all(&addon).unwrap();
        fs::write(addon.join("__init__.py"), b"").unwrap();
        let boot = t.path().join("bootstrap.py");
        fs::write(&boot, b"#").unwrap();
        let sup = Supervisor::with_temp_dir(Arc::new(Quiet), tmp);
        sup.launch(LaunchSpec {
            blender: "cmd.exe".into(),
            bootstrap: boot,
            folder: Folder { path: addon.to_string_lossy().into_owned(), port: 9931, ..Folder::default() },
            blend: None,
            args_override: Some(vec!["/d".into(), "/c".into(), "for /L %i in (1,1,2000000000) do @rem".into()]),
        })
        .unwrap();
        assert_eq!(sup.alive_ports(), vec![9931]);

        let (s, mut slot) = setup(Some(r#"{"folders":[{"path":"keep"}]}"#), r#"{"folders":[{"path":"new"}]}"#);
        let before = fs::read_to_string(&s.cfg).unwrap();
        let before_slot = slot.clone();
        let e = apply(&s, &mut slot, &sup.alive_ports()).unwrap_err();
        assert_eq!(e, "Stop the running Blender instances first, then import again.");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), before);
        assert_eq!(slot, before_slot);
        assert!(!config::backup_path(&s.cfg).exists());

        assert!(sup.kill(9931));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !sup.alive_ports().is_empty() {
            assert!(std::time::Instant::now() < deadline, "stand-in did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        apply(&s, &mut slot, &sup.alive_ports()).unwrap();
        assert_eq!(slot.folders[0].path, "new");
    }

    #[test]
    fn apply_import_rejects_bad_files_before_touching_anything() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        for (bad, msg) in [
            ("{}", "This file does not look like a Blinker UI config (no folders or settings found)"),
            ("[1]", "Config file must be a JSON object"),
            ("", "Expecting value: line 1 column 1 (char 0)"),
        ] {
            let (s, mut slot) = setup(Some(cur), bad);
            let e = apply(&s, &mut slot, &[]).unwrap_err();
            assert_eq!(e, msg);
            assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur);
            assert_eq!(slot.folders[0].path, "keep");
            assert!(!config::backup_path(&s.cfg).exists());
        }
        let (s, mut slot) = setup(Some(cur), "{}");
        let e = apply_import(&s.cfg, &mut slot, &s.cfg.with_file_name("missing.json"), "x", &[]).unwrap_err();
        assert!(e.starts_with("[Errno 2] No such file or directory"), "{e}");
    }

    #[test]
    fn apply_import_commit_failure_leaves_the_old_config_in_place() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        let (s, mut slot) = setup(Some(cur), r#"{"folders":[{"path":"new"}]}"#);
        // a directory where the temp file must go makes the write fail
        fs::create_dir(s.cfg.with_file_name("blinker_ui_config.json.tmp")).unwrap();
        let e = apply(&s, &mut slot, &[]).unwrap_err();
        assert!(e.starts_with("Could not save config: "), "{e}");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur, "config file untouched");
        assert_eq!(slot.folders[0].path, "keep", "managed config untouched");
    }

    #[test]
    fn apply_import_backup_failure_aborts_the_import() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        let (s, mut slot) = setup(Some(cur), r#"{"folders":[{"path":"new"}]}"#);
        fs::create_dir(config::backup_path(&s.cfg)).unwrap(); // a directory in the way
        let e = apply(&s, &mut slot, &[]).unwrap_err();
        assert!(e.starts_with("Could not back up the current config: "), "{e}");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur);
        assert_eq!(slot.folders[0].path, "keep");
    }

    #[test]
    fn apply_import_refuses_a_file_that_changed_since_inspect() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        let (s, mut slot) = setup(Some(cur), r#"{"folders":[{"path":"confirmed"}]}"#);
        let fp = import::read_config_file(&s.src).unwrap().summary.fingerprint;
        // someone swaps the file after the user confirmed it
        write(&s.src, r#"{"folders":[{"path":"swapped"}]}"#);
        let e = apply_import(&s.cfg, &mut slot, &s.src, &fp, &[]).unwrap_err();
        assert_eq!(e, "The file changed since you chose it; select it again.");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur);
        assert_eq!(slot.folders[0].path, "keep");
        assert!(!config::backup_path(&s.cfg).exists(), "no .bak for a refused import");
        // an empty or wrong fingerprint is a mismatch too; the right one imports
        for bad in ["", "abc"] {
            assert_eq!(apply_import(&s.cfg, &mut slot, &s.src, bad, &[]).unwrap_err(), e);
        }
        let fp2 = import::read_config_file(&s.src).unwrap().summary.fingerprint;
        apply_import(&s.cfg, &mut slot, &s.src, &fp2, &[]).unwrap();
        assert_eq!(slot.folders[0].path, "swapped");
    }

    #[test]
    fn prepare_import_builds_the_new_config_without_touching_disk() {
        let (s, slot) = setup(
            Some(r#"{"folders":[{"path":"keep"}]}"#),
            r#"{"folders":[{"path":"a"},{"path":"b","favourite":true}]}"#,
        );
        let before = fs::read_to_string(&s.cfg).unwrap();
        let fp = import::read_config_file(&s.src).unwrap().summary.fingerprint;
        let new = prepare_import(&s.cfg, &s.src, &fp).unwrap();
        assert_eq!(new.folders[0].path, "b");
        assert_eq!(new.theme, theme::merge_with_default(&Map::new()));
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), before);
        assert!(!config::backup_path(&s.cfg).exists());
        assert_eq!(slot.folders[0].path, "keep");
        // the locked step alone is tiny: refusal, then backup + commit
        let mut slot = slot;
        assert!(commit_import(&s.cfg, &mut slot, new.clone(), &[9876]).is_err());
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), before);
        commit_import(&s.cfg, &mut slot, new, &[]).unwrap();
        assert_eq!(slot.folders[0].path, "b");
        assert_eq!(fs::read_to_string(config::backup_path(&s.cfg)).unwrap(), before);
    }

    #[test]
    fn inspect_command_returns_the_summary() {
        let (s, _) = setup(None, crate::import::tests::OLD_PYTHON_CONFIG);
        let sum = tauri::async_runtime::block_on(inspect_config_file(s.src.to_string_lossy().into_owned())).unwrap();
        assert_eq!(sum.folders, 7);
        assert!(tauri::async_runtime::block_on(inspect_config_file("Z:/nope.json".into())).is_err());
    }
}
